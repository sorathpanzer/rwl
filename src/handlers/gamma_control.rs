//! `zwlr_gamma_control_v1` protocol handler.
//!
//! Implements the wlr gamma-control protocol so that tools such as
//! gammastep/wlsunset can adjust per-output colour temperature.
//!
//! Only the udev/DRM backend supports actual gamma adjustment; the winit
//! backend sends `failed` immediately because it has no DRM access.

use std::collections::HashMap;
use std::io::Read as _;

use smithay::output::Output;
use smithay::reexports::wayland_protocols_wlr::gamma_control::v1::server::{
    zwlr_gamma_control_manager_v1::{self, ZwlrGammaControlManagerV1},
    zwlr_gamma_control_v1::{self, ZwlrGammaControlV1},
};
use smithay::reexports::wayland_server::backend::ClientId;
use smithay::reexports::wayland_server::{Client, DataInit, DisplayHandle, New};
use smithay::wayland::{Dispatch2, GlobalDispatch2};

use crate::state::Rwl;

// ---------------------------------------------------------------------------
// Manager global
// ---------------------------------------------------------------------------

/// User data for both the `zwlr_gamma_control_manager_v1` global and the
/// bound manager resource.
pub struct GammaManagerData;

impl GlobalDispatch2<ZwlrGammaControlManagerV1, Rwl> for GammaManagerData {
    fn bind(
        &self,
        _state: &mut Rwl,
        _handle: &DisplayHandle,
        _client: &Client,
        resource: New<ZwlrGammaControlManagerV1>,
        data_init: &mut DataInit<'_, Rwl>,
    ) {
        data_init.init(resource, Self);
    }
}

impl Dispatch2<ZwlrGammaControlManagerV1, Rwl> for GammaManagerData {
    fn request(
        &self,
        state: &mut Rwl,
        _client: &Client,
        _resource: &ZwlrGammaControlManagerV1,
        request: zwlr_gamma_control_manager_v1::Request,
        _dhandle: &DisplayHandle,
        data_init: &mut DataInit<'_, Rwl>,
    ) {
        let zwlr_gamma_control_manager_v1::Request::GetGammaControl { id, output } = request else {
            return;
        };

        let Some(output) = Output::from_resource(&output) else {
            // Unknown output — immediately fail.
            let ctrl = data_init.init(id, GammaControlData { output: None });
            ctrl.failed();
            return;
        };

        // Only one exclusive client per output.
        if state.gamma_controls.contains_key(&output) {
            let ctrl = data_init.init(id, GammaControlData { output: None });
            ctrl.failed();
            return;
        }

        let gamma_size = gamma_size_for_output(state, &output);

        let ctrl = data_init.init(id, GammaControlData { output: Some(output.clone()) });

        match gamma_size {
            Some(size) => {
                ctrl.gamma_size(size);
                state.gamma_controls.insert(output, ctrl);
            }
            None => {
                // Winit or output not yet backed by DRM.
                ctrl.failed();
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Per-output control object
// ---------------------------------------------------------------------------

/// User data for a `zwlr_gamma_control_v1` object.
pub struct GammaControlData {
    output: Option<Output>,
}

impl Dispatch2<ZwlrGammaControlV1, Rwl> for GammaControlData {
    fn request(
        &self,
        state: &mut Rwl,
        _client: &Client,
        resource: &ZwlrGammaControlV1,
        request: zwlr_gamma_control_v1::Request,
        _dhandle: &DisplayHandle,
        _data_init: &mut DataInit<'_, Rwl>,
    ) {
        let zwlr_gamma_control_v1::Request::SetGamma { fd } = request else {
            return;
        };

        let Some(ref output) = self.output else {
            return;
        };

        if apply_gamma(state, output, fd).is_err() {
            resource.failed();
        }
    }

    fn destroyed(&self, state: &mut Rwl, _client: ClientId, _resource: &ZwlrGammaControlV1) {
        if let Some(ref output) = self.output {
            state.gamma_controls.remove(output);
            state.gamma_ramps.remove(output);
            restore_gamma(state, output);
        }
    }
}

// ---------------------------------------------------------------------------
// DRM helpers
// ---------------------------------------------------------------------------

/// Return the gamma ramp size for `output`, or `None` if the udev backend
/// has no DRM surface for it (e.g. winit backend or output not yet scanned).
// Without the winit feature BackendData has only one variant, making the
// let-else pattern irrefutable; the else branch is still correct dead code.
#[cfg_attr(not(feature = "winit"), allow(irrefutable_let_patterns))]
fn gamma_size_for_output(state: &Rwl, output: &Output) -> Option<u32> {
    use smithay::reexports::drm::control::Device as _;

    let crate::backend::BackendData::Udev(ref udev) =
        *state.backend.as_ref()? else { return None; };

    udev.devices.values().find_map(|device| {
        device.surfaces.iter()
            .find(|(_, surface)| &surface.output == output)
            .and_then(|(&crtc, _)| device.drm.get_crtc(crtc).ok().map(|i| i.gamma_length()))
    })
}

/// Apply the gamma ramp contained in `fd` to the DRM CRTC for `output`.
///
/// The fd must contain exactly `3 * gamma_size` little-endian `u16` values:
/// the red, green, and blue ramps packed one after the other.
#[cfg_attr(not(feature = "winit"), allow(irrefutable_let_patterns))]
fn apply_gamma(
    state: &mut Rwl,
    output: &Output,
    fd: std::os::fd::OwnedFd,
) -> Result<(), ()> {
    use smithay::reexports::drm::control::Device as _;

    // Determine the CRTC's expected gamma ramp size up front so we can bound the
    // read: the client supplies exactly `3 * gamma_size` u16 values (R, G, B).
    let gamma_size = gamma_size_for_output(state, output).ok_or(())? as usize;
    if gamma_size == 0 {
        return Err(());
    }
    let expected_bytes = gamma_size
        .checked_mul(3)
        .and_then(|v| v.checked_mul(2))
        .ok_or(())?;

    // Read from the client fd (a memfd / pipe), but never more than the ramp we
    // expect plus one byte — a sentinel that lets us detect an over-long payload
    // without letting a malicious client exhaust memory with a huge fd.
    let mut file = std::fs::File::from(fd);
    let mut data = Vec::with_capacity(expected_bytes);
    std::io::Read::by_ref(&mut file)
        .take(expected_bytes as u64 + 1)
        .read_to_end(&mut data)
        .map_err(|_| ())?;

    // The payload must be exactly the expected size — no more, no less.
    if data.len() != expected_bytes {
        return Err(());
    }
    let values: Vec<u16> = data
        .chunks_exact(2)
        .map(|b| u16::from_ne_bytes([b[0], b[1]]))
        .collect();

    let ramp_size = values.len() / 3;

    // Scope the mutable backend borrow so it is dropped before we can access
    // state.gamma_ramps below.  Also scope the slice borrows of `values` so
    // `values` can be moved into the map after set_gamma returns.
    let result: Result<(), ()> = {
        let crate::backend::BackendData::Udev(ref mut udev) =
            *state.backend.as_mut().ok_or(())? else { return Err(()); };

        udev.devices.values_mut()
            .find_map(|device| {
                let crtc = device.surfaces.iter()
                    .find(|(_, surface)| &surface.output == output)
                    .map(|(&crtc, _)| crtc)?;
                Some(device.drm.set_gamma(crtc, &values[..ramp_size], &values[ramp_size..2 * ramp_size], &values[2 * ramp_size..]).map_err(|_| ()))
            })
            .unwrap_or(Err(()))
    }; // backend borrow and slice borrows released here

    // Persist the ramp so we can re-apply it after suspend/resume resets
    // the hardware gamma LUT.
    if result.is_ok() {
        state.gamma_ramps.insert(output.clone(), values);
    }
    result
}

/// Restore a linear (identity) gamma ramp on the CRTC for `output`.
///
/// Called when a gamma control object is destroyed so the display returns
/// to its un-adjusted state.
fn restore_gamma(state: &mut Rwl, output: &Output) {
    use smithay::reexports::drm::control::Device as _;

    let Some(crate::backend::BackendData::Udev(ref mut udev)) = state.backend else {
        return;
    };

    for device in udev.devices.values_mut() {
        for (crtc, surface) in &device.surfaces {
            if &surface.output == output {
                let Ok(info) = device.drm.get_crtc(*crtc) else { return };
                let size = info.gamma_length() as usize;
                if size == 0 { return; }
                let ramp: Vec<u16> = (0..size)
                    .map(|i| {
                        if size == 1 {
                            0xffffu16
                        } else {
                            #[allow(clippy::cast_possible_truncation)]
                            { ((i * 0xffff) / (size - 1)) as u16 }
                        }
                    })
                    .collect();
                let _ = device.drm.set_gamma(*crtc, &ramp, &ramp, &ramp);
                return;
            }
        }
    }
}

/// Re-apply all stored gamma ramps after a suspend/resume cycle wipes the
/// hardware LUT.  Called from `open_pending_devices()` after DRM re-activation.
pub fn reapply_gamma_ramps(state: &mut Rwl) {
    use smithay::reexports::drm::control::Device as _;

    let Some(crate::backend::BackendData::Udev(ref mut udev)) = state.backend else {
        return;
    };

    for device in udev.devices.values_mut() {
        for (crtc, surface) in &device.surfaces {
            if let Some(values) = state.gamma_ramps.get(&surface.output) {
                let n = values.len() / 3;
                if n == 0 { continue; }
                let red   = &values[..n];
                let green = &values[n..2 * n];
                let blue  = &values[2 * n..];
                let _ = device.drm.set_gamma(*crtc, red, green, blue);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// State helper — exposes the gamma-controls map type for state.rs
// ---------------------------------------------------------------------------

/// Type alias for the per-output gamma-control tracking map.
pub type GammaControlMap = HashMap<Output, ZwlrGammaControlV1>;
