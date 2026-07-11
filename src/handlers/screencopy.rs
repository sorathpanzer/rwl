//! `zwlr_screencopy_v1` implementation.
//!
//! Allows screenshot tools such as `grim` to capture compositor output.
//! Follows the same `Dispatch2`/`GlobalDispatch2` pattern used by
//! `gamma_control.rs`; registered via `delegate_dispatch2!(Rwl)`.
//!
//! Protocol flow:
//! 1. Client calls `capture_output` → compositor creates a frame object and
//!    immediately sends `buffer(Argb8888, w, h, stride)` + `buffer_done` (v3+).
//! 2. Client allocates an SHM buffer and calls `copy(buffer)`.
//! 3. Compositor stores a `PendingFrame` and schedules a render.
//! 4. After the next render (EGL context still bound), compositor calls
//!    `submit_screencopy_frames()` which copies the framebuffer into the SHM
//!    buffer and sends `flags` + `ready` to the client.

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::ExportMem as _;
use smithay::backend::renderer::gles::{GlesRenderer, GlesTarget};
use smithay::output::Output;
use smithay::reexports::wayland_protocols_wlr::screencopy::v1::server::{
    zwlr_screencopy_frame_v1::{self, ZwlrScreencopyFrameV1},
    zwlr_screencopy_manager_v1::{self, ZwlrScreencopyManagerV1},
};
use smithay::reexports::wayland_server::backend::ClientId;
use smithay::reexports::wayland_server::protocol::{wl_buffer::WlBuffer, wl_shm};
use smithay::reexports::wayland_server::{Client, DataInit, DisplayHandle, New, Resource as _};
use smithay::utils::{Buffer as BufferCoords, Rectangle};
use smithay::wayland::shm::with_buffer_contents_mut;
use smithay::wayland::{Dispatch2, GlobalDispatch2};

use crate::state::Rwl;

// ---------------------------------------------------------------------------
// Manager global
// ---------------------------------------------------------------------------

/// User data for the `zwlr_screencopy_manager_v1` global and bound resource.
pub struct ScreencopyManagerData;

impl GlobalDispatch2<ZwlrScreencopyManagerV1, Rwl> for ScreencopyManagerData {
    fn bind(
        &self,
        _state: &mut Rwl,
        _handle: &DisplayHandle,
        _client: &Client,
        resource: New<ZwlrScreencopyManagerV1>,
        data_init: &mut DataInit<'_, Rwl>,
    ) {
        data_init.init(resource, Self);
    }
}

impl Dispatch2<ZwlrScreencopyManagerV1, Rwl> for ScreencopyManagerData {
    #[allow(clippy::cast_sign_loss)]
    fn request(
        &self,
        _state: &mut Rwl,
        _client: &Client,
        _resource: &ZwlrScreencopyManagerV1,
        request: zwlr_screencopy_manager_v1::Request,
        _dhandle: &DisplayHandle,
        data_init: &mut DataInit<'_, Rwl>,
    ) {
        // Unpack the four fields common to both capture requests.
        let (wl_output, overlay_cursor, frame_id, region_opt) = match request {
            zwlr_screencopy_manager_v1::Request::CaptureOutput {
                overlay_cursor,
                output,
                frame,
            } => (output, overlay_cursor != 0, frame, None),

            zwlr_screencopy_manager_v1::Request::CaptureOutputRegion {
                overlay_cursor,
                output,
                frame,
                x,
                y,
                width,
                height,
            } => {
                let r: Rectangle<i32, BufferCoords> =
                    Rectangle::new((x, y).into(), (width, height).into());
                (output, overlay_cursor != 0, frame, Some(r))
            }

            _ => return,
        };

        // Resolve wl_output → smithay Output.
        let Some(output) = Output::from_resource(&wl_output) else {
            let frame = data_init.init(frame_id, ScreencopyFrameData { capture: None, used: std::sync::atomic::AtomicBool::new(false) });
            frame.failed();
            return;
        };

        let Some(mode) = output.current_mode() else {
            let frame = data_init.init(frame_id, ScreencopyFrameData { capture: None, used: std::sync::atomic::AtomicBool::new(false) });
            frame.failed();
            return;
        };

        let full: Rectangle<i32, BufferCoords> =
            Rectangle::new((0, 0).into(), (mode.size.w, mode.size.h).into());
        let region: Rectangle<i32, BufferCoords> =
            region_opt.map_or(full, |r| r.intersection(full).unwrap_or(full));

        let width = region.size.w.max(0) as u32;
        let height = region.size.h.max(0) as u32;
        let stride = width * 4; // Argb8888 → 4 bytes per pixel

        let frame = data_init.init(
            frame_id,
            ScreencopyFrameData {
                capture: Some(CaptureInfo {
                    output,
                    region,
                    overlay_cursor,
                }),
                used: std::sync::atomic::AtomicBool::new(false),
            },
        );

        // Tell the client what kind of SHM buffer to allocate.
        frame.buffer(wl_shm::Format::Argb8888, width, height, stride);
        // v3+ clients expect buffer_done to know when all formats are listed.
        if frame.version() >= 3 {
            frame.buffer_done();
        }
    }
}

// ---------------------------------------------------------------------------
// Frame object
// ---------------------------------------------------------------------------

struct CaptureInfo {
    output: Output,
    region: Rectangle<i32, BufferCoords>,
    #[allow(dead_code)]
    overlay_cursor: bool,
}

/// User data for each `zwlr_screencopy_frame_v1` object.
pub struct ScreencopyFrameData {
    /// `None` when the output was invalid at capture time.
    capture: Option<CaptureInfo>,
    /// A frame object accepts exactly one `copy` / `copy_with_damage`.  Set on
    /// the first copy so a second one raises the `already_used` protocol error
    /// instead of queueing a duplicate `PendingFrame`.
    used: std::sync::atomic::AtomicBool,
}

impl Dispatch2<ZwlrScreencopyFrameV1, Rwl> for ScreencopyFrameData {
    fn request(
        &self,
        state: &mut Rwl,
        _client: &Client,
        resource: &ZwlrScreencopyFrameV1,
        request: zwlr_screencopy_frame_v1::Request,
        _dhandle: &DisplayHandle,
        _data_init: &mut DataInit<'_, Rwl>,
    ) {
        // Both Copy and CopyWithDamage are treated identically — we always do
        // a full region capture.
        let (zwlr_screencopy_frame_v1::Request::Copy { buffer }
            | zwlr_screencopy_frame_v1::Request::CopyWithDamage { buffer }) = request
        else {
            return;
        };

        // A frame object may only be copied once (protocol requirement).  A
        // second copy would queue a duplicate PendingFrame for the same
        // resource; raise the defined error instead.
        if self.used.swap(true, std::sync::atomic::Ordering::Relaxed) {
            resource.post_error(
                zwlr_screencopy_frame_v1::Error::AlreadyUsed,
                "frame already used",
            );
            return;
        }

        let Some(ref info) = self.capture else {
            resource.failed();
            return;
        };

        // Refuse framebuffer readback while the session is locked. Otherwise any
        // client could capture the lock screen — including the password prompt
        // and keystroke feedback of the screen locker.
        if state.locked {
            resource.failed();
            return;
        }

        state.pending_screencopy_frames.push(PendingFrame {
            frame: resource.clone(),
            buffer,
            output: info.output.clone(),
            region: info.region,
        });

        state.schedule_render();
    }

    fn destroyed(&self, state: &mut Rwl, _client: ClientId, resource: &ZwlrScreencopyFrameV1) {
        // Remove any pending frame for this object so we don't try to write to
        // a destroyed resource after the next render.
        state
            .pending_screencopy_frames
            .retain(|pf| &pf.frame != resource);
    }
}

// ---------------------------------------------------------------------------
// Pending frame (held in Rwl until the next render)
// ---------------------------------------------------------------------------

/// A screencopy frame that is waiting for the next render pass to be served.
pub struct PendingFrame {
    /// The protocol object used to send `ready`/`failed` events.
    pub frame: ZwlrScreencopyFrameV1,
    /// The SHM buffer the client provided.
    pub buffer: WlBuffer,
    /// Which output the client wants captured.
    pub output: Output,
    /// Region within the output buffer to capture, in physical pixels.
    pub region: Rectangle<i32, BufferCoords>,
}

// ---------------------------------------------------------------------------
// Render-time pixel copy
// ---------------------------------------------------------------------------

/// Copy the current OpenGL framebuffer into every pending screencopy frame
/// that targets `output`.
///
/// Must be called **after** rendering to the output but **before** swapping
/// buffers (i.e. before `eglSwapBuffers` / `queue_frame`), while the EGL
/// context is still current and `target` refers to the just-rendered surface.
#[allow(
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
)]
pub fn submit_screencopy_frames(
    renderer: &mut GlesRenderer,
    target: &GlesTarget<'_>,
    output: &Output,
    pending: &mut Vec<PendingFrame>,
    time: smithay::utils::Time<smithay::utils::Monotonic>,
) {
    let mut i = 0;
    while i < pending.len() {
        if &pending[i].output != output {
            i += 1;
            continue;
        }
        // swap_remove replaces this slot with the last element; don't advance i.
        let pf = pending.swap_remove(i);
        submit_one(renderer, target, pf, time);
    }
}

/// Fail all pending screencopy frames targeting `output` (used by backends
/// that don't support framebuffer readback, e.g. udev/DRM for now).
pub fn fail_screencopy_frames(output: &Output, pending: &mut Vec<PendingFrame>) {
    let mut i = 0;
    while i < pending.len() {
        if &pending[i].output == output {
            let pf = pending.swap_remove(i);
            if pf.frame.is_alive() {
                pf.frame.failed();
            }
        } else {
            i += 1;
        }
    }
}

#[allow(
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    // Smithay's with_buffer_contents_mut provides a raw *mut u8 pointer;
    // we convert it to a safe slice internally.
    unsafe_code,
)]
fn submit_one(
    renderer: &mut GlesRenderer,
    target: &GlesTarget<'_>,
    pf: PendingFrame,
    time: smithay::utils::Time<smithay::utils::Monotonic>,
) {
    let PendingFrame { frame, buffer, region, .. } = pf;

    if !frame.is_alive() {
        return;
    }

    // Read the rendered pixels from the current framebuffer.
    tracing::debug!("screencopy: copy_framebuffer region={region:?}");
    let mapping = match renderer.copy_framebuffer(target, region, Fourcc::Argb8888) {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!("screencopy: copy_framebuffer failed: {e}");
            frame.failed();
            return;
        }
    };

    // Map the GPU buffer to CPU memory and make an owned copy so we can
    // release the renderer borrow before writing to the SHM pool.
    let pixels: Vec<u8> = match renderer.map_texture(&mapping) {
        Ok(p) => p.to_vec(),
        Err(e) => {
            tracing::warn!("screencopy: map_texture: {e}");
            frame.failed();
            return;
        }
    };

    // Write into the client's SHM buffer, handling potential padding strides.
    // Smithay's with_buffer_contents_mut provides a raw *mut u8 + length;
    // we convert to a safe slice inside the closure.
    let write_result = with_buffer_contents_mut(&buffer, |ptr, len, spec| {
        // SAFETY: ptr is a valid, non-null pointer into an mmap'd SHM pool
        // with `len` accessible bytes, as guaranteed by Smithay's SHM state.
        let data: &mut [u8] = unsafe { std::slice::from_raw_parts_mut(ptr, len) };

        let offset = spec.offset.max(0) as usize;
        // Source: tightly packed Argb8888 rows from copy_framebuffer.
        let src_stride = (region.size.w as usize) * 4;
        // Destination: the client's SHM buffer may have extra stride padding.
        let dst_stride = spec.stride as usize;
        let rows = (region.size.h as usize).min(spec.height as usize);

        for row in 0..rows {
            let src_start = row * src_stride;
            let dst_start = offset + row * dst_stride;
            // Copy only as many bytes as both source and destination allow.
            let copy_len = src_stride
                .min(pixels.len().saturating_sub(src_start))
                .min(data.len().saturating_sub(dst_start));
            if copy_len > 0 {
                data[dst_start..dst_start + copy_len]
                    .copy_from_slice(&pixels[src_start..src_start + copy_len]);
            }
        }
    });

    match write_result {
        Ok(()) => {
            // Send the ready event with a monotonic timestamp.
            let dur = std::time::Duration::from(time);
            let secs = dur.as_secs();
            let tv_sec_hi = (secs >> 32) as u32;
            let tv_sec_lo = (secs & 0xFFFF_FFFF) as u32;
            frame.ready(tv_sec_hi, tv_sec_lo, dur.subsec_nanos());
        }
        Err(e) => {
            tracing::warn!("screencopy: SHM buffer write failed: {e:?}");
            frame.failed();
        }
    }
}
