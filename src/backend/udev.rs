//! udev/DRM/KMS backend.
//!
//! Responsible for:
//! - Opening a session via libseat
//! - Discovering GPU devices via udev
//! - Creating DRM outputs and a [`GlesRenderer`] per GPU
//! - Driving the render loop (frame callbacks, damage tracking)
//! - Handling libinput events and forwarding them to the compositor

use smithay::reexports::wayland_server::Resource;
use std::collections::HashMap;
use std::path::PathBuf;

use smithay::input::pointer::CursorImageStatus;

use smithay::backend::allocator::dmabuf::{AsDmabuf, Dmabuf};
use smithay::backend::allocator::gbm::{GbmAllocator, GbmBufferFlags, GbmDevice};
use smithay::backend::allocator::Fourcc;
use smithay::backend::drm::compositor::{DrmCompositor, FrameFlags, PrimaryPlaneElement};
use smithay::backend::renderer::Bind;
use smithay::backend::drm::exporter::gbm::{GbmFramebufferExporter, NodeFilter};
use smithay::backend::drm::{DrmDevice, DrmDeviceFd, DrmEvent, DrmEventTime, DrmNode, NodeType};
use smithay::wayland::dmabuf::DmabufFeedbackBuilder;
use smithay::wayland::drm_syncobj::{DrmSyncobjState, supports_syncobj_eventfd};
use smithay::desktop::utils::{
    OutputPresentationFeedback, surface_presentation_feedback_flags_from_states,
    surface_primary_scanout_output, update_surface_primary_scanout_output,
};
use smithay::backend::renderer::element::default_primary_scanout_output_compare;
use smithay::wayland::presentation::Refresh;
use smithay::reexports::wayland_protocols::wp::presentation_time::server::wp_presentation_feedback;
use smithay::backend::egl::{EGLContext, EGLDisplay};
use smithay::backend::input::{InputBackend, InputEvent};
use smithay::backend::libinput::{LibinputInputBackend, LibinputSessionInterface};
use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::ImportDma;
use smithay::backend::session::{Event as SessionEvent, Session};
use smithay::backend::udev::{UdevBackend, UdevEvent};
use smithay::output::{Mode as WlMode, Output, PhysicalProperties, Subpixel};
use smithay::reexports::calloop::LoopHandle;
use smithay::reexports::drm::control::{connector, crtc, Device as ControlDevice, ModeTypeFlags};
use smithay::reexports::input as libinput;
use smithay::reexports::rustix::fs::OFlags;
use smithay::reexports::wayland_server::backend::GlobalId;
use smithay::reexports::wayland_server::DisplayHandle;
use smithay::utils::{Buffer as BufferCoords, DeviceFd, Size};
use smithay::wayland::seat::WaylandFocus;

use crate::error::{RwlError, Result};
use crate::state::Rwl;

// ---------------------------------------------------------------------------
// Type aliases to reduce verbosity
// ---------------------------------------------------------------------------

type GbmDrmCompositor = DrmCompositor<
    GbmAllocator<DrmDeviceFd>,
    GbmFramebufferExporter<DrmDeviceFd>,
    Option<OutputPresentationFeedback>,
    DrmDeviceFd,
>;

// ---------------------------------------------------------------------------
// Per-device data
// ---------------------------------------------------------------------------

/// State associated with one GPU device.
pub struct DeviceData {
    // `surfaces` must be declared first so it is dropped before `drm`, `gbm`,
    // and `renderer`.  Each `GbmDrmCompositor` inside the map calls into the
    // DRM/GBM/EGL layer during Drop (cleanup modeset commit); those resources
    // must still be valid at that point.  Rust drops struct fields in
    // declaration order, so declaring `surfaces` first ensures the compositors
    // are torn down while the underlying device is still live.
    pub surfaces: HashMap<crtc::Handle, OutputSurface>,
    pub drm: DrmDevice,
    pub gbm: GbmDevice<DrmDeviceFd>,
    pub renderer: GlesRenderer,
    /// GPU-side state for the rounded-corner feature.
    #[cfg(feature = "rounded-corners")]
    pub rounded: crate::features::rounded_corners::RoundedCornerState,
    #[allow(dead_code)]
    pub path: PathBuf,
}

/// State associated with one CRTC / output.
pub struct OutputSurface {
    pub compositor: GbmDrmCompositor,
    pub output: Output,
    #[allow(dead_code)]
    pub damage_tracker: OutputDamageTracker,
    /// Keeps the `wl_output` Wayland global alive for the lifetime of this output.
    /// Dropping this removes the global and clients lose the output.
    #[allow(dead_code)]
    pub output_global: GlobalId,
    /// True when a DRM page flip has been submitted and we are waiting for
    /// the hardware `VBlank` completion event.  While this is set we must not
    /// call `render_frame` / `queue_frame` again — doing so before the `VBlank`
    /// callback would corrupt GBM buffer state and cause the driver to
    /// silently drop subsequent flips.
    pub flip_pending: bool,
    /// The connector handle, retained so VRR can be toggled per-output at runtime.
    pub connector: connector::Handle,
    /// The configured VRR policy for this output (from its monitor rule).
    pub vrr_mode: crate::config::VrrMode,
}

// ---------------------------------------------------------------------------
// Connector scan helper
// ---------------------------------------------------------------------------

/// Collected info for one connected DRM connector, gathered before mutable
/// borrows are needed so we can release the immutable `&DeviceData` borrow
/// and then call `self.output_added()`.
struct ConnInfo {
    crtc: crtc::Handle,
    drm_mode: smithay::reexports::drm::control::Mode,
    conn: connector::Handle,
    name: String,
    mm_w: u32,
    mm_h: u32,
}

// ---------------------------------------------------------------------------
// Top-level backend data
// ---------------------------------------------------------------------------

/// All data owned by the udev backend, stored alongside the compositor state.
pub struct UdevData {
    /// Per-GPU device map, keyed by device number (`libc::dev_t`).
    /// Declared before `session` so DRM surfaces (and their Drop cleanup
    /// commits) run while we still hold DRM master, before the session closes.
    pub devices: HashMap<libc::dev_t, DeviceData>,
    /// Devices discovered via udev but not yet opened (session was inactive).
    pub pending_devices: Vec<(libc::dev_t, std::path::PathBuf)>,
    pub session: smithay::backend::session::libseat::LibSeatSession,
    /// Libinput context — must be explicitly suspended/resumed alongside the
    /// session so its input-device fds are closed/re-opened by libseat on
    /// VT-switch and suspend/resume cycles.  Without this, libinput holds
    /// stale revoked fds after wake and never generates events again.
    pub libinput_ctx: libinput::Libinput,
    /// All currently connected keyboard devices, kept so LED state
    /// (`CapsLock`, `NumLock`) can be pushed down to the physical hardware.
    pub kbd_devices: Vec<libinput::Device>,
    /// All currently connected input devices, kept so libinput settings can be
    /// re-applied on config reload without waiting for a hotplug event.
    pub input_devices: Vec<libinput::Device>,
}

// ---------------------------------------------------------------------------
// Backend initialisation
// ---------------------------------------------------------------------------

/// Initialise the udev/DRM/libinput backend, register sources with the event
/// loop, and return [`UdevData`].
///
/// Existing DRM devices are stored in [`UdevData::pending_devices`] and will
/// be opened once `SessionEvent::ActivateSession` fires (as soon as the event
/// loop starts dispatching the libseat source).  This ensures DRM master has
/// actually been granted before we attempt EGL initialisation.
pub fn init(
    loop_handle: &LoopHandle<'static, Rwl>,
    _display_handle: &DisplayHandle,
) -> Result<UdevData> {
    use smithay::backend::session::libseat::LibSeatSession;

    // --- Session ---
    let (session, notifier) = LibSeatSession::new()
        .map_err(|e| RwlError::Session(e.to_string()))?;
    let seat_name = session.seat();
    tracing::info!("Opened libseat session on {}", seat_name);

    // Smithay's LibSeatSession::new() dispatches libseat once internally and
    // consumes the initial Enable event via try_recv() to set the active flag,
    // but does NOT re-inject it into the channel.  As a result our notifier
    // handler never fires SessionEvent::ActivateSession on the first run.
    // Detect this case now so we can schedule open_pending_devices() below.
    let session_already_active = session.is_active();

    // --- udev backend ---
    let udev_backend = UdevBackend::new(&seat_name)
        .map_err(|e| RwlError::Backend(e.to_string()))?;

    // Snapshot existing DRM devices now — once inserted as a source the backend
    // only delivers *changes*, not the initial list.  Store them as pending so
    // they are opened after ActivateSession, not before the event loop starts.
    let pending_devices: Vec<(libc::dev_t, std::path::PathBuf)> = udev_backend
        .device_list()
        .map(|(id, path)| (id, path.to_owned()))
        .collect();
    tracing::info!("Found {} DRM device(s), deferring until session active", pending_devices.len());

    // --- libinput ---
    let mut libinput_context =
        libinput::Libinput::new_with_udev(LibinputSessionInterface::from(session.clone()));
    libinput_context
        .udev_assign_seat(&seat_name)
        .map_err(|()| RwlError::Backend("libinput udev_assign_seat failed".into()))?;

    let libinput_backend = LibinputInputBackend::new(libinput_context.clone());

    // Register libinput event source
    loop_handle
        .insert_source(libinput_backend, |mut event, (), state| {
            // Configure newly added libinput devices before forwarding the event.
            if let smithay::backend::input::InputEvent::DeviceAdded { ref mut device } = event {
                state.configure_libinput_device(device);
            }
            state.process_input_event(event);
        })
        .map_err(|e| RwlError::Backend(e.to_string()))?;

    // Register session notification source.
    loop_handle
        .insert_source(notifier, move |event, (), state| {
            match event {
                SessionEvent::PauseSession => {
                    tracing::info!("Session paused — suspending libinput and DRM devices");
                    // Suspend libinput before pausing DRM so its device fds are
                    // closed via libseat before logind revokes them.  Without this
                    // call, libinput holds stale revoked fds and generates no events
                    // after the system wakes from suspend.
                    if let Some(backend) = state.backend_data_opt() {
                        backend.libinput_ctx.suspend();
                    }
                    state.pause_drm_devices();
                }
                SessionEvent::ActivateSession => {
                    tracing::info!("Session activated — resuming DRM devices and libinput");
                    state.open_pending_devices();
                    // Resume libinput after the session is active so it can
                    // re-open its input-device fds via libseat and start
                    // delivering keyboard/pointer events again.
                    if let Some(backend) = state.backend_data_opt()
                        && backend.libinput_ctx.resume() == Err(())
                    {
                        tracing::error!("libinput_ctx.resume() failed");
                    }
                }
            }
        })
        .map_err(|e| RwlError::Backend(e.to_string()))?;

    // If the session was already active when new() was called (seatd/logind
    // granted it synchronously), the Enable event was consumed internally by
    // smithay and our ActivateSession handler above will never fire.  Schedule
    // an idle callback so pending devices are still opened on the first loop tick.
    if session_already_active {
        tracing::info!("Session was already active at init — scheduling initial device open");
        loop_handle.insert_idle(Rwl::open_pending_devices);
    }

    let data = UdevData {
        session,
        devices: HashMap::new(),
        pending_devices,
        libinput_ctx: libinput_context,
        kbd_devices: Vec::new(),
        input_devices: Vec::new(),
    };

    // Register udev event source (device hotplug)
    loop_handle
        .insert_source(udev_backend, move |event, _, state| {
            match event {
                UdevEvent::Added { device_id, path } => {
                    state.enqueue_device(device_id, path);
                }
                UdevEvent::Changed { device_id } => {
                    state.backend_device_changed(device_id);
                }
                UdevEvent::Removed { device_id } => {
                    state.backend_device_removed(device_id);
                }
            }
        })
        .map_err(|e| RwlError::Backend(e.to_string()))?;

    Ok(data)
}

// ---------------------------------------------------------------------------
// Input event dispatch
// ---------------------------------------------------------------------------

impl Rwl {
    pub fn process_input_event<B: InputBackend>(&mut self, event: InputEvent<B>) {
        use smithay::backend::input::InputEvent as E;
        match event {
            E::Keyboard { event } => self.process_key_event::<B>(&event),
            E::PointerMotion { event } => self.process_pointer_motion::<B>(&event),
            E::PointerMotionAbsolute { event } => self.process_pointer_motion_abs::<B>(&event),
            E::PointerButton { event } => self.process_pointer_button::<B>(&event),
            E::PointerAxis { event } => self.process_pointer_axis::<B>(&event),
            E::DeviceAdded { device } => self.configure_input_device(&device),
            _ => {}
        }
    }

    #[allow(clippy::unused_self)]
    pub fn configure_input_device<D: smithay::backend::input::Device>(&self, device: &D) {
        tracing::debug!("New input device: {}", device.name());
    }

    /// Apply libinput-specific configuration to a newly discovered device.
    /// Called from the libinput source handler on `DeviceAdded` events.
    pub fn configure_libinput_device(&mut self, device: &mut smithay::reexports::input::Device) {
        tracing::debug!("Configuring libinput device: {}", device.name());

        apply_libinput_config(device, &crate::config::get());

        if let Some(backend) = self.backend_data_opt() {
            backend.input_devices.push(device.clone());
        }

        if device.has_capability(libinput::DeviceCapability::Keyboard) {
            if let Some(backend) = self.backend_data_opt() {
                backend.kbd_devices.push(device.clone());
            }
            // Apply the initial LED state (numlock/capslock) to the physical
            // keyboard immediately.  Call led_update on the original device
            // reference directly — the clone in kbd_devices shares the same
            // underlying libinput_device pointer, but going through the
            // original avoids any last_mut() ambiguity.
            if let Some(kb) = self.keyboard.clone() {
                let leds = libinput_leds_from_smithay(kb.led_state());
                tracing::debug!(
                    "Applying initial LED state {:?} to keyboard {}",
                    leds, device.name()
                );
                device.led_update(leds);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Device hotplug
// ---------------------------------------------------------------------------

impl Rwl {
    /// Queue a device to be opened (either immediately if session is active, or
    /// deferred until `SessionEvent::ActivateSession` fires).
    pub fn enqueue_device(&mut self, device_id: libc::dev_t, path: std::path::PathBuf) {
        if let Err(e) = self.backend_device_added(device_id, &path) {
            tracing::warn!(
                "Device {:?} not ready yet (session inactive?): {e} — deferring",
                path
            );
            if let Some(backend) = self.backend_data_opt() {
                backend.pending_devices.push((device_id, path));
            }
        }
    }

    /// Called when the session becomes active (VT switch return or resume from suspend).
    ///
    /// Both cases fire `SessionEvent::ActivateSession`.  We re-acquire DRM master via
    /// `activate()`, then call `reset_state()` + `reset_buffers()` on each `DrmCompositor`
    /// so it forces a full modeset on the next render.  Without `reset_state`, the compositor
    /// thinks its last frame is still valid, `render_frame` returns `is_empty=true`, and the
    /// screen stays black indefinitely after resume.
    pub fn open_pending_devices(&mut self) {
        let device_ids: Vec<libc::dev_t> = self
            .backend_data_opt()
            .map(|b| b.devices.keys().copied().collect())
            .unwrap_or_default();

        for device_id in device_ids {
            let needs_activate = self
                .backend_data_opt()
                .and_then(|b| b.devices.get(&device_id))
                .is_some_and(|d| !d.drm.is_active());

            if !needs_activate {
                continue;
            }

            // Re-acquire DRM master.  The kernel DRM driver restores GPU hardware
            // state during resume before logind fires ResumeDevice, so DRM ioctls
            // are safe by the time ActivateSession reaches us.
            let activate_ok = self
                .backend_data_opt()
                .and_then(|b| b.devices.get_mut(&device_id))
                .is_some_and(|dev| match dev.drm.activate(false) {
                    Ok(()) => true,
                    Err(e) => {
                        tracing::error!("Failed to re-activate DRM device: {e}");
                        false
                    }
                });

            if !activate_ok {
                continue;
            }

            // After a suspend/resume (or VT switch) the CRTC was re-programmed by the
            // kernel; tell each DrmCompositor to force a full modeset on the next
            // render_frame call, and discard stale GBM swapchain slots.
            if let Some(backend) = self.backend_data_opt()
                && let Some(dev) = backend.devices.get_mut(&device_id)
            {
                for surf in dev.surfaces.values_mut() {
                    if let Err(e) = surf.compositor.reset_state() {
                        tracing::warn!("DrmCompositor::reset_state: {e}");
                    }
                    surf.compositor.reset_buffers();
                    // The kernel discards in-flight page flips on session pause;
                    // clear the flag so the render loop is not permanently stalled.
                    surf.flip_pending = false;
                }
            }

            // Rescan connectors — picks up any hotplug events during suspend.
            let _ = self.scan_connectors(device_id);

            // Kick the render loop for pre-existing outputs.  scan_connectors skips
            // CRTCs that already have an OutputSurface, so on_vblank is not called
            // for them automatically — do it manually.
            let existing_crtcs: Vec<crtc::Handle> = self
                .backend_data_opt()
                .and_then(|b| b.devices.get(&device_id))
                .map(|d| d.surfaces.keys().copied().collect())
                .unwrap_or_default();
            for crtc_handle in existing_crtcs {
                self.on_vblank(device_id, crtc_handle);
            }
        }

        // Re-apply gamma ramps wiped by the modeset during suspend.
        crate::handlers::gamma_control::reapply_gamma_ramps(self);

        // Open newly discovered (first-time) devices.
        let pending = self
            .backend_data_opt()
            .map(|b| std::mem::take(&mut b.pending_devices))
            .unwrap_or_default();

        for (device_id, path) in pending {
            if let Err(e) = self.backend_device_added(device_id, &path) {
                tracing::error!("Failed to open deferred device {:?}: {e}", path);
            }
        }
    }

    /// Pause all open DRM devices when the session is deactivated (VT switch away).
    pub fn pause_drm_devices(&mut self) {
        let Some(backend) = self.backend_data_opt() else { return };
        backend.devices.values_mut().for_each(|dev| dev.drm.pause());
    }

    #[allow(clippy::too_many_lines, clippy::redundant_closure_for_method_calls, unsafe_code)]
    pub fn backend_device_added(
        &mut self,
        device_id: libc::dev_t,
        path: &std::path::Path,
    ) -> Result<()> {
        tracing::info!("DRM device added: {:?}", path);

        // Guard: only open DRM devices when the session is active (i.e. we have
        // DRM master).  Attempting EGL initialisation without master can cause
        // the process to hang inside the GPU driver.
        {
            let active = self
                .backend_data_opt()
                .is_some_and(|b| b.session.is_active());
            if !active {
                return Err(RwlError::Backend(
                    "session not yet active — deferring device".into(),
                ));
            }
        }

        let fd = self
            .backend_data_opt()
            .ok_or_else(|| RwlError::Backend("no backend data".into()))?
            .session
            .open(path, OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOCTTY | OFlags::NONBLOCK)
            .map_err(|e| RwlError::Device(e.to_string()))?;

        let drm_device_fd = DrmDeviceFd::new(DeviceFd::from(fd));

        let (drm, drm_notifier) = DrmDevice::new(drm_device_fd.clone(), false)
            .map_err(|e| RwlError::Drm(e.to_string()))?;

        let gbm = GbmDevice::new(drm_device_fd)
            .map_err(|e| RwlError::Drm(e.to_string()))?;

        // EGLDisplay::new, EGLContext::new, GlesRenderer::new are unsafe fn in
        // smithay because they call into C EGL/GL libraries. This is the only
        // unavoidable unsafe block in the codebase; the rest of the compositor
        // is fully safe Rust.
        #[cfg_attr(not(feature = "rounded-corners"), allow(unused_mut))]
        let (egl, mut renderer) = unsafe {
            let egl = EGLDisplay::new(gbm.clone())
                .map_err(|e| RwlError::Renderer(e.to_string()))?;
            let context = EGLContext::new(&egl)
                .map_err(|e| RwlError::Renderer(e.to_string()))?;
            let renderer = GlesRenderer::new(context)
                .map_err(|e| RwlError::Renderer(e.to_string()))?;
            (egl, renderer)
        };
        // egl is only needed during renderer creation; keep it alive via drop order.
        drop(egl);

        if let Some(backend) = self.backend_data_opt() {
            #[cfg(feature = "rounded-corners")]
            let rounded = crate::features::rounded_corners::RoundedCornerState::init(&mut renderer);
            backend.devices.insert(
                device_id,
                DeviceData {
                    drm,
                    gbm,
                    renderer,
                    #[cfg(feature = "rounded-corners")]
                    rounded,
                    surfaces: HashMap::new(),
                    path: path.to_owned(),
                },
            );
        }

        // Advertise zwp_linux_dmabuf_v1 using the first GPU's supported formats.
        // The global must be created exactly once; subsequent devices are ignored.
        //
        // Use create_global_with_default_feedback (protocol v4+) rather than
        // create_global (v3).  The v4 feedback mechanism sends a `main_device`
        // event containing the render-node dev_t.  Without it Mesa EGL cannot
        // identify which DRM device to open and falls back to llvmpipe (software
        // rendering), making video playback extremely slow.
        if self.dmabuf_global.is_none() {
            let formats: Option<Vec<_>> = self
                .backend_data_opt()
                .and_then(|b| b.devices.get_mut(&device_id))
                .map(|dev| dev.renderer.dmabuf_formats().into_iter().collect());

            if let Some(formats) = formats
                && !formats.is_empty()
            {
                    // Prefer the render node (renderD128) so EGL clients open the
                    // right device.  Fall back to the primary node dev_t if the
                    // render node cannot be resolved.
                    let render_dev = DrmNode::from_dev_id(device_id)
                        .ok()
                        .and_then(|n| n.node_with_type(NodeType::Render))
                        .and_then(|r| r.ok())
                        .map_or(device_id, |n| n.dev_id());

                    match DmabufFeedbackBuilder::new(render_dev, formats).build() {
                        Ok(feedback) => {
                            let global = self
                                .dmabuf_state
                                .create_global_with_default_feedback::<Self>(
                                    &self.display_handle,
                                    &feedback,
                                );
                            self.dmabuf_global = Some(global);
                            tracing::info!("DMA-BUF global (v4 + feedback) created");
                        }
                        Err(e) => tracing::error!("Failed to build DmabufFeedback: {e}"),
                    }
            }
        }

        // Create the wp_linux_drm_syncobj_manager_v1 global for explicit GPU sync.
        // Like the dmabuf global, only create it once for the first device.
        if !self.has_syncobj_state() {
            let dev_fd = self
                .backend_data_opt()
                .and_then(|b| b.devices.get(&device_id))
                .map(|dev| dev.drm.device_fd().clone());

            if let Some(fd) = dev_fd
                && supports_syncobj_eventfd(&fd)
            {
                self.set_syncobj_state(DrmSyncobjState::new::<Self>(&self.display_handle, fd));
                tracing::info!("DRM syncobj explicit sync global created");
            }
        }

        self.loop_handle
            .insert_source(drm_notifier, move |event, metadata, state| {
                match event {
                    DrmEvent::VBlank(crtc) => {
                        tracing::debug!("DRM VBlank crtc {:?}", crtc);
                        // Compute fallback clock before the mutable borrow of state.backend.
                        let fallback_clock = state.clock.now();
                        // Notify smithay that the submitted frame is now on screen,
                        // releasing the old GBM buffer and clearing internal flip state.
                        // Retrieve the wp_presentation feedback stored with that frame
                        // and mark it as presented with accurate VBlank timing.
                        if let Some(backend) = state.backend_data_opt()
                            && let Some(dev) = backend.devices.get_mut(&device_id)
                            && let Some(surf) = dev.surfaces.get_mut(&crtc)
                        {
                                    surf.flip_pending = false;
                                    match surf.compositor.frame_submitted() {
                                        Ok(user_data) => {
                                            if let Some(mut feedback) = user_data.flatten() {
                                                let seq = metadata
                                                    .as_ref()
                                                    .map_or(0, |m| u64::from(m.sequence));
                                                let (time, flags) = metadata
                                                    .as_ref()
                                                    .and_then(|m| match m.time {
                                                        DrmEventTime::Monotonic(d)
                                                            if !d.is_zero() =>
                                                        {
                                                            Some((
                                                                d.into(),
                                                                wp_presentation_feedback::Kind::Vsync
                                                                    | wp_presentation_feedback::Kind::HwClock
                                                                    | wp_presentation_feedback::Kind::HwCompletion,
                                                            ))
                                                        }
                                                        _ => None,
                                                    })
                                                    .unwrap_or((
                                                        fallback_clock,
                                                        wp_presentation_feedback::Kind::Vsync,
                                                    ));
                                                let refresh = surf
                                                    .output
                                                    .current_mode()
                                                    .map_or(Refresh::Unknown, |m| {
                                                        Refresh::fixed(
                                                            std::time::Duration::from_secs_f64(
                                                                1_000f64 / f64::from(m.refresh),
                                                            ),
                                                        )
                                                    });
                                                feedback.presented(time, refresh, seq, flags);
                                            }
                                        }
                                        Err(e) => tracing::error!("frame_submitted error: {e}"),
                                    }
                        }
                        state.on_vblank(device_id, crtc);
                    }
                    DrmEvent::Error(e) => tracing::error!("DRM error on device {device_id}: {e}"),
                }
            })
            .map_err(|e| RwlError::Backend(e.to_string()))?;

        self.scan_connectors(device_id)?;

        Ok(())
    }

    pub fn backend_device_changed(&mut self, device_id: libc::dev_t) {
        tracing::info!("DRM device changed, rescanning connectors");
        let _ = self.scan_connectors(device_id);
    }

    pub fn backend_device_removed(&mut self, device_id: libc::dev_t) {
        tracing::info!("DRM device removed");
        if let Some(backend) = self.backend_data_opt()
            && let Some(dev) = backend.devices.remove(&device_id)
        {
            for (_, surface) in dev.surfaces {
                self.output_removed(&surface.output);
            }
        }
    }

    fn scan_connectors(&mut self, device_id: libc::dev_t) -> Result<()> {
        let conn_infos = self.collect_conn_infos(device_id)?;
        for info in conn_infos {
            self.setup_connector_output(device_id, &info);
        }
        Ok(())
    }

    fn collect_conn_infos(&mut self, device_id: libc::dev_t) -> Result<Vec<ConnInfo>> {
        let Some(backend) = self.backend_data_opt() else { return Ok(vec![]) };
        let Some(dev) = backend.devices.get(&device_id) else { return Ok(vec![]) };

        let resources = dev
            .drm
            .resource_handles()
            .map_err(|e| RwlError::Drm(e.to_string()))?;

        let mut result = Vec::new();
        // Track CRTCs claimed by earlier connectors in this same scan so
        // two connectors that share compatible CRTCs don't both pick the
        // same one (dev.surfaces won't reflect the second claim yet).
        let mut claimed: std::collections::HashSet<crtc::Handle> =
            std::collections::HashSet::new();

        for &conn_handle in resources.connectors() {
            let Ok(connector) = dev.drm.get_connector(conn_handle, true) else {
                continue;
            };
            if connector.state()
                != smithay::reexports::drm::control::connector::State::Connected
            {
                continue;
            }

            let Some(drm_mode) = connector
                .modes()
                .iter()
                .find(|m| m.mode_type().contains(ModeTypeFlags::PREFERRED))
                .or_else(|| connector.modes().first())
                .copied()
            else {
                continue;
            };

            // Find the first compatible CRTC that is neither already driving an
            // existing surface nor claimed by an earlier connector in this scan.
            let crtc = connector
                .encoders()
                .iter()
                .filter_map(|&enc| dev.drm.get_encoder(enc).ok())
                .flat_map(|enc| resources.filter_crtcs(enc.possible_crtcs()))
                .find(|c| !dev.surfaces.contains_key(c) && !claimed.contains(c));
            let Some(crtc) = crtc else { continue };

            claimed.insert(crtc);

            let name = format!(
                "{}-{}",
                connector.interface().as_str(),
                connector.interface_id()
            );
            let (mm_w, mm_h) = connector.size().unwrap_or((0, 0));

            result.push(ConnInfo {
                crtc,
                drm_mode,
                conn: conn_handle,
                name,
                mm_w,
                mm_h,
            });
        }

        Ok(result)
    }

    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_possible_wrap,
        clippy::cast_sign_loss,
    )]
    fn setup_connector_output(&mut self, device_id: libc::dev_t, info: &ConnInfo) {
        let wl_mode = WlMode {
            size: (
                i32::from(info.drm_mode.size().0),
                i32::from(info.drm_mode.size().1),
            )
                .into(),
            refresh: info.drm_mode.vrefresh() as i32 * 1000,
        };

        let output = Output::new(
            info.name.clone(),
            PhysicalProperties {
                size: (info.mm_w as i32, info.mm_h as i32).into(),
                subpixel: Subpixel::Unknown,
                make: "Generic".into(),
                model: info.name.clone(),
                serial_number: String::new(),
            },
        );
        output.set_preferred(wl_mode);
        output.change_current_state(Some(wl_mode), None, None, None);

        let compositor_result = self.build_drm_compositor(device_id, info, &output);

        match compositor_result {
            Ok((mut compositor, damage_tracker)) => {
                // Apply the output's VRR policy up-front.  `On` enables adaptive
                // sync immediately when the hardware supports it; `OnDemand` is
                // left off here and toggled from the render loop when a fullscreen
                // window appears.  Requires atomic modesetting — the legacy DRM
                // backend reports `NotSupported`, so this is a safe no-op there.
                let vrr_mode = crate::monitor::matching_rule_for_output(&output).vrr;
                if vrr_mode == crate::config::VrrMode::On {
                    if matches!(
                        compositor.vrr_supported(info.conn),
                        Ok(smithay::backend::drm::VrrSupport::Supported)
                    ) {
                        if let Err(e) = compositor.use_vrr(true) {
                            tracing::warn!("{}: enabling VRR failed: {e}", info.name);
                        } else {
                            tracing::info!("{}: VRR enabled", info.name);
                        }
                    } else {
                        tracing::info!("{}: VRR requested but not supported", info.name);
                    }
                }
                // Publish a wl_output global so Wayland clients (bars, apps,
                // layer-shell surfaces) can see this monitor.  This MUST be done
                // before output_added() advertises it to the space, otherwise
                // clients connecting during startup race with the global not yet
                // existing.  The GlobalId is stored in OutputSurface so the
                // global stays alive until the output is removed.
                let output_global = output.create_global::<Self>(&self.display_handle);
                if let Some(backend) = self.backend_data_opt()
                    && let Some(dev) = backend.devices.get_mut(&device_id)
                {
                    dev.surfaces.insert(
                        info.crtc,
                        OutputSurface {
                            compositor,
                            output: output.clone(),
                            damage_tracker,
                            output_global,
                            flip_pending: false,
                            connector: info.conn,
                            vrr_mode,
                        },
                    );
                }
                self.output_added(&output);
                tracing::info!("Output created: {}", info.name);
                // Kickstart the render loop — without this first render+queue,
                // no DrmEvent::VBlank ever fires and the screen stays dark.
                self.on_vblank(device_id, info.crtc);
            }
            Err(e) => {
                tracing::error!(
                    "Failed to create DRM compositor for {}: {}",
                    info.name,
                    e
                );
            }
        }
    }

    fn build_drm_compositor(
        &mut self,
        device_id: libc::dev_t,
        info: &ConnInfo,
        output: &Output,
    ) -> std::result::Result<(GbmDrmCompositor, OutputDamageTracker), Box<dyn std::error::Error>>
    {
        let Some(backend) = self.backend_data_opt() else {
            return Err("no backend".into());
        };
        let Some(dev) = backend.devices.get_mut(&device_id) else {
            return Err("device not found".into());
        };

        let drm_surface = dev
            .drm
            .create_surface(info.crtc, info.drm_mode, &[info.conn])
            .map_err(|e| {
                tracing::error!("Failed to create DRM surface for {}: {}", info.name, e);
                e
            })?;

        let gbm = dev.gbm.clone();
        let allocator =
            GbmAllocator::new(gbm.clone(), GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT);
        let exporter = GbmFramebufferExporter::new(gbm.clone(), NodeFilter::All);

        let renderer_formats = dev
            .renderer
            .dmabuf_formats()
            .into_iter()
            .collect::<std::collections::HashSet<_>>();

        let cursor_size: Size<u32, BufferCoords> = Size::from((64u32, 64u32));

        let compositor: GbmDrmCompositor = DrmCompositor::new(
            output,
            drm_surface,
            None,
            allocator,
            exporter,
            [Fourcc::Argb8888, Fourcc::Xrgb8888],
            renderer_formats,
            cursor_size,
            Some(gbm),
        )?;

        let dt = OutputDamageTracker::from_output(output);
        Ok((compositor, dt))
    }

    #[allow(clippy::too_many_lines)]
    fn on_vblank(&mut self, device_id: libc::dev_t, crtc: crtc::Handle) {
        // Reset a dead client cursor surface to the named default so the xcursor
        // fallback path takes over.  Without this the cursor disappears when the
        // client that set it disconnects and never recovers in TTY mode (the winit
        // path has the same check in render_frame).
        if let CursorImageStatus::Surface(ref surf) = self.cursor_status
            && !surf.is_alive()
        {
            self.cursor_status = CursorImageStatus::default_named();
        }

        // 1. Grab output info (immutable borrow, dropped before mutable use).
        //    Guard first: if the DRM device is paused (VT switch / suspend) do nothing.
        //    schedule_render idle callbacks can be queued before PauseSession fires and
        //    run after pause_drm_devices() returns — attempting EGL/DRM operations on a
        //    paused device causes the ioctl to block in the kernel, freezing the machine.
        let output = {
            let Some(backend) = self.backend_data_opt() else { return };
            let Some(dev) = backend.devices.get(&device_id) else { return };
            if !dev.drm.is_active() {
                tracing::debug!("on_vblank: device not active (suspended/paused), skipping");
                return;
            }
            let Some(surf) = dev.surfaces.get(&crtc) else { return };
            surf.output.clone()
        };

        let time = self.clock.now();
        let scale = output.current_scale().fractional_scale(); // f64

        // Refresh the space so element-to-output associations are current.
        // Without this, space_render_elements queries elements_for_output()
        // which returns nothing (empty current_outputs), making every frame
        // render only the background regardless of mapped windows.
        self.space.refresh();

        // Render gate: hold the previous framebuffer while any tiled window on
        // this output is mid-resize (committed size ≠ pending_geom size).
        // Without this, the brief gap between the configure and the client's
        // ack commit shows blank wallpaper in the master/slave area.
        // schedule_render() is called from commit() when the client delivers the
        // new buffer, which clears the gate and triggers a clean render.
        //
        // Frame callbacks MUST still be sent even when skipping render: clients
        // such as foot request wl_surface.frame() before committing their resize
        // buffer and will deadlock if the callback never arrives.
        //
        // Timeout: if the gate has been active for more than ~100 ms (e.g. rapid
        // key-repeat keeps opening new windows faster than they can resize),
        // fall through and render the best available frame rather than starving
        // the display indefinitely.
        // New windows (no buffer yet) get a 100 ms budget — they need to render
        // from scratch.  Existing windows resizing after a close only need to
        // swap their already-rendered buffer; 32 ms (~2 frames) is ample.
        let new_pending = self.any_tiled_pending_resize(&output);
        let resize_pending = !new_pending && self.any_existing_tiled_pending_resize(&output);
        let gate_timeout_ms: u128 = if new_pending { 100 } else { 32 };
        if new_pending || resize_pending {
            let now = std::time::Instant::now();
            let since = self.layout_gate_since.get_or_insert(now);
            let elapsed = now.saturating_duration_since(*since).as_millis();
            if elapsed < gate_timeout_ms {
                self.space.elements().for_each(|w| {
                    if let Some(wl_surf) = w.wl_surface() {
                        smithay::desktop::utils::send_frames_surface_tree(
                            &wl_surf, &output, time, None, |_, _| Some(output.clone()),
                        );
                        smithay::desktop::PopupManager::popups_for_surface(&wl_surf)
                            .for_each(|(popup, _)| {
                                smithay::desktop::utils::send_frames_surface_tree(
                                    popup.wl_surface(), &output, time, None,
                                    |_, _| Some(output.clone()),
                                );
                            });
                    }
                });
                let layer_map = smithay::desktop::layer_map_for_output(&output);
                layer_map.layers().for_each(|layer| {
                    smithay::desktop::utils::send_frames_surface_tree(
                        layer.wl_surface(), &output, time, None, |_, _| Some(output.clone()),
                    );
                });
                drop(layer_map);
                return;
            }
            // Timeout expired: render now and reset so the next transition gets
            // a fresh gate window.
        }
        self.layout_gate_since = None;

        // 2. Build render elements via the Space API, then prepend border
        //    decoration elements.  Both are wrapped in RwlRenderElement so
        //    render_frame receives a single homogeneous slice.
        //
        //    space_render_elements handles XDG geometry offsets (render_location =
        //    element_location - geometry.loc), layer-shell z-ordering
        //    (Top/Overlay above windows, Background/Bottom below), and output
        //    geometry clipping automatically.
        //    Split borrow: self.backend (renderer) vs self.space are different fields.
        let pointer_loc = self
            .pointer
            .as_ref()
            .map(smithay::input::pointer::PointerHandle::current_location)
            .unwrap_or_default();

        // On TTY there is no host-cursor fallback: compute the software-cursor
        // fallback surface before borrowing self.backend mutably.
        let cursor_fallback = if self.space.element_under(pointer_loc).is_none() {
            self.last_cursor_surface.as_ref().filter(|s| s.is_alive()).cloned()
        } else {
            None
        };

        // Resolve the cursor image for the current Named status before the
        // backend mutable borrow (split borrow: cursor_cache ≠ backend).
        let named_cursor: Option<crate::render::NamedCursorBuffer> =
            if let CursorImageStatus::Named(icon) = self.cursor_status {
                self.cursor_cache.entry(icon).or_insert_with(|| {
                    let cfg = crate::config::get();
                    let size = if cfg.cursor_size > 0 { cfg.cursor_size } else {
                        std::env::var("XCURSOR_SIZE").ok().and_then(|s| s.parse().ok()).unwrap_or(24)
                    };
                    let buf = crate::render::load_cursor_icon(cfg.cursor_theme.as_deref(), size, icon);
                    drop(cfg);
                    buf
                });
                self.cursor_cache
                    .get(&icon)
                    .and_then(|b| b.as_ref())
                    .or_else(|| self.cursor_cache.get(&smithay::input::pointer::CursorIcon::Default).and_then(|b| b.as_ref()))
                    .cloned()
            } else {
                None
            };

        // Advance per-window fade animations; must run before elements are built
        // so render_elements_from_surface_tree sees the updated alpha this frame.
        #[cfg(feature = "fade")]
        let any_fading = crate::features::fade::advance_fades(self);
        // Advance tag-transition slide animations before building elements.
        #[cfg(feature = "tag-transition")]
        let any_transitioning = crate::features::tag_transition::advance_tag_transitions(self);
        // Advance the overview open/close zoom (may finalize a selection).
        #[cfg(feature = "overview")]
        let any_overview = crate::features::overview::advance(self);
        // Unmap slide-out windows whose animation just finished.
        #[cfg(feature = "tag-transition")]
        self.finalize_slide_outs();
        // When the last animation frame completes (any_transitioning flips
        // true→false), start the post-transition wakeup: for ~1 s (60 frames)
        // emit a transparent sentinel per window so is_empty stays false and
        // DRM flips — and thus frame callbacks — keep flowing.  Chromium-based
        // clients (Firefox, Signal, WhatsApp) stall after the tight animation
        // loop ends because their pending frame() callbacks never move to
        // current state (they don't commit without a prior callback), creating
        // a deadlock.  Continuous DRM flips break it: the compositor sends
        // frame callbacks unconditionally on each on_vblank, so eventually
        // the client's callback is in current state and gets delivered.
        #[cfg(feature = "tag-transition")]
        if self.tag_transition_was_active && !any_transitioning {
            // Transition just finished.  Re-run the full arrange — exactly what
            // the user's manual `mod+<tag>` / layout-change workaround does, and
            // the ONLY thing that reliably clears the grey.  A window brought
            // onto this tag by switch_to_tag was arranged once during its first
            // commit, but at that point it had no buffer and could not act on
            // the configure; the client then draws a blank initial frame and
            // waits.  Re-arranging here (which re-maps it via map_element and
            // recomputes work areas) is what mod+<tag> does to make it repaint.
            // A forced send_configure alone does NOT fix it, so the cure is the
            // space re-map inside arrange(), not the configure.
            self.arrange_all();
            let top = self.focused_window().cloned();
            self.focus_window(top);
            self.post_transition_wakeup = 30;
        }
        #[cfg(feature = "tag-transition")]
        { self.tag_transition_was_active = any_transitioning; }

        // Post-transition follow-up: some clients commit their first real buffer
        // only after the transition ends.  Re-arrange once more partway through
        // the countdown to catch them, and keep the render loop alive so the
        // repaint is presented.
        #[cfg(feature = "tag-transition")]
        if self.post_transition_wakeup > 0 {
            self.post_transition_wakeup -= 1;
            if self.post_transition_wakeup == 15 {
                self.arrange_all();
                let top = self.focused_window().cloned();
                self.focus_window(top);
            }
            self.schedule_render();
        }

        // Compute context values before mutably borrowing self.backend.
        let has_fullscreen = self.space.elements().any(crate::window::window_is_fullscreen);
        // Per-output fullscreen state for OnDemand VRR — the global `has_fullscreen`
        // above would (wrongly) drive VRR on a second, non-gaming monitor.
        let output_has_fullscreen = self.monitor_for_output(&output).is_some_and(|idx| {
            self.space.elements().any(|w| {
                crate::window::window_is_fullscreen(w)
                    && crate::window::with_state(w, |s| s.mon_idx) == Some(idx)
            })
        });
        let top_has_kbd_focus = {
            let map = smithay::desktop::layer_map_for_output(&output);
            map.layers_on(smithay::wayland::shell::wlr_layer::Layer::Top)
                .any(smithay::desktop::LayerSurface::can_receive_keyboard_focus)
        };
        #[cfg(feature = "bar")]
        let bar_has_notification = self.bar_has_notification.load(std::sync::atomic::Ordering::Relaxed);
        #[cfg(not(feature = "bar"))]
        let bar_has_notification = false;
        let visible_count = self
            .monitors
            .iter()
            .find(|m| m.output == output)
            .map_or(0, |mon| {
                let tags = mon.tags();
                self.windows
                    .iter()
                    .filter(|w| {
                        crate::window::window_visible_on(w, tags)
                            && crate::window::with_state(w, |s| s.buffer_mapped).unwrap_or(false)
                    })
                    .count()
            });
        // Clone the focused window so we can pass it into the renderer block
        // without holding a borrow on self while self.backend is mutably borrowed.
        let focused = self.focused_window().cloned();

        // Tag-mask of the monitor driving this output, read before the mutable
        // `self.backend` borrow, so the per-tag wallpaper can be selected.
        #[cfg(feature = "wallpaper")]
        let wallpaper_tags = self
            .monitors
            .iter()
            .find(|m| m.output == output)
            .map_or(0, crate::monitor::Monitor::tags);

        // When the overview is open on this output its thumbnail grid replaces the
        // normal window + border elements.
        #[cfg(feature = "overview")]
        let overview_on = self
            .overview
            .as_ref()
            .is_some_and(|o| o.on_output(&output));

        let elements: Vec<crate::render::RwlRenderElement> = {
            let (window_elems, border_elems, cursor_elems, dnd_elems, xcursor_elems, overlay_elems, top_elems, bottom_elems, bg_elems, wallpaper_elems, lock_elems, pip_elems) = {
                let Some(crate::backend::BackendData::Udev(ref mut udev)) = self.backend else {
                    return;
                };
                let Some(dev) = udev.devices.get_mut(&device_id) else { return };
                let renderer = &mut dev.renderer;
                // Overview replaces the window + border element lists with its
                // thumbnail grid; when inactive both are built normally.
                #[cfg(feature = "overview")]
                let (window_elems, border_elems): (Vec<crate::render::RwlRenderElement>, Vec<crate::render::RwlRenderElement>) =
                    if overview_on {
                        #[cfg(feature = "rounded-corners")]
                        let round = crate::render::thumb_round(&dev.rounded, scale, true);
                        #[cfg(not(feature = "rounded-corners"))]
                        let round = None;
                        let ov = self.overview.as_ref().map_or_else(Vec::new, |ov| {
                            crate::features::overview::overview_elements(renderer, ov, &self.space, &output, scale, round)
                        });
                        (ov, Vec::new())
                    } else {
                        let win = {
                            #[cfg(feature = "rounded-corners")]
                            { crate::render::build_window_elements_rounded(renderer, &self.space, &output, scale, Some(&dev.rounded), true) }
                            #[cfg(not(feature = "rounded-corners"))]
                            { crate::render::build_window_elements_plain(renderer, &self.space, &output, scale) }
                        };
                        let brd = {
                            #[cfg(feature = "rounded-corners")]
                            { crate::render::build_border_elements_rounded(renderer, &self.space, focused.as_ref(), &output, scale, Some(&dev.rounded), true, visible_count, has_fullscreen) }
                            #[cfg(not(feature = "rounded-corners"))]
                            { crate::render::build_border_elements_plain(&self.space, focused.as_ref(), scale, visible_count, has_fullscreen) }
                        };
                        (win, brd)
                    };
                #[cfg(not(feature = "overview"))]
                let window_elems: Vec<crate::render::RwlRenderElement> = {
                    #[cfg(feature = "rounded-corners")]
                    { crate::render::build_window_elements_rounded(renderer, &self.space, &output, scale, Some(&dev.rounded), true) }
                    #[cfg(not(feature = "rounded-corners"))]
                    { crate::render::build_window_elements_plain(renderer, &self.space, &output, scale) }
                };
                #[cfg(not(feature = "overview"))]
                let border_elems: Vec<crate::render::RwlRenderElement> = {
                    #[cfg(feature = "rounded-corners")]
                    { crate::render::build_border_elements_rounded(renderer, &self.space, focused.as_ref(), &output, scale, Some(&dev.rounded), true, visible_count, has_fullscreen) }
                    #[cfg(not(feature = "rounded-corners"))]
                    { crate::render::build_border_elements_plain(&self.space, focused.as_ref(), scale, visible_count, has_fullscreen) }
                };
                let cursor_elems = crate::render::cursor_elements(
                    renderer,
                    &self.cursor_status,
                    self.cursor_hidden,
                    pointer_loc,
                    scale,
                    cursor_fallback.as_ref(),
                );
                let dnd_elems = self.dnd_icon.as_ref().map_or_else(Vec::new, |icon| {
                    crate::render::dnd_icon_elements(renderer, icon, pointer_loc, scale)
                });
                // Render the xcursor arrow only when cursor_elements produced nothing
                // (Named status with no alive fallback surface).  If cursor_elements
                // already rendered a surface (the fallback from last_cursor_surface),
                // skip xcursor to avoid a double arrow on empty tags.
                let xcursor_elems =
                    if !self.cursor_hidden
                        && matches!(self.cursor_status, CursorImageStatus::Named(_))
                        && cursor_elems.is_empty()
                    {
                        crate::render::named_cursor_elements(
                            renderer,
                            named_cursor.as_ref(),
                            pointer_loc,
                            scale,
                        )
                    } else {
                        Vec::new()
                    };
                // Layer-shell elements — generated here so they share the renderer borrow.
                // Front-to-back z-order: Overlay → Top → (windows) → Bottom → Background.
                let overlay_elems = crate::render::layer_elements(
                    renderer, &output, smithay::wayland::shell::wlr_layer::Layer::Overlay, scale,
                );
                let top_elems = crate::render::layer_elements(
                    renderer, &output, smithay::wayland::shell::wlr_layer::Layer::Top, scale,
                );
                let bottom_elems = crate::render::layer_elements(
                    renderer, &output, smithay::wayland::shell::wlr_layer::Layer::Bottom, scale,
                );
                let bg_elems = crate::render::layer_elements(
                    renderer, &output, smithay::wayland::shell::wlr_layer::Layer::Background, scale,
                );
                // Native wallpaper — backmost, below the Background layer shell.
                #[cfg(feature = "wallpaper")]
                let wallpaper_elems: Vec<crate::render::RwlRenderElement> =
                    crate::features::wallpaper::elements(renderer, &output, scale, wallpaper_tags)
                        .into_iter()
                        .map(crate::render::RwlRenderElement::from)
                        .collect();
                #[cfg(not(feature = "wallpaper"))]
                let wallpaper_elems: Vec<crate::render::RwlRenderElement> = Vec::new();
                let lock_elems = crate::render::lock_surface_elements(
                    renderer, &self.lock_surfaces, &output, scale,
                );
                // Picture-in-Picture: an always-on-top live thumbnail, above the
                // layers but below the cursor and lock surface. Hidden while the
                // session is locked or the overview is open.
                #[cfg(feature = "pip")]
                let pip_elems: Vec<crate::render::RwlRenderElement> = {
                    #[cfg(feature = "overview")]
                    let overview_shown = self.overview.as_ref().is_some_and(|o| o.on_output(&output));
                    #[cfg(not(feature = "overview"))]
                    let overview_shown = false;
                    if self.locked || overview_shown {
                        Vec::new()
                    } else {
                        #[cfg(feature = "rounded-corners")]
                        let round = crate::render::thumb_round(&dev.rounded, scale, true);
                        #[cfg(not(feature = "rounded-corners"))]
                        let round = None;
                        self.pip.as_ref().map_or_else(Vec::new, |pip| {
                            crate::features::pip::pip_elements(renderer, pip, &self.space, &output, scale, round)
                        })
                    }
                };
                #[cfg(not(feature = "pip"))]
                let pip_elems: Vec<crate::render::RwlRenderElement> = Vec::new();
                (window_elems, border_elems, cursor_elems, dnd_elems, xcursor_elems, overlay_elems, top_elems, bottom_elems, bg_elems, wallpaper_elems, lock_elems, pip_elems)
            };

            // Session locked: composite ONLY the cursor, lock surfaces, and an
            // opaque black backdrop. Client windows and layer-shell surfaces
            // (bars, notification daemons) are never drawn while locked, and the
            // backdrop guarantees outputs without a live lock surface reveal
            // nothing behind them.
            if self.locked {
                // The native locker hides the pointer entirely; external
                // `ext-session-lock` clients keep it (some draw controls).
                #[cfg(feature = "lock")]
                let hide_cursor = self.native_lock.is_some();
                #[cfg(not(feature = "lock"))]
                let hide_cursor = false;

                let mut combined = Vec::with_capacity(
                    cursor_elems.len() + xcursor_elems.len() + lock_elems.len() + 1,
                );
                if !hide_cursor {
                    combined.extend(cursor_elems.into_iter().map(crate::render::RwlRenderElement::from));
                    combined.extend(xcursor_elems.into_iter().map(crate::render::RwlRenderElement::from));
                }
                combined.extend(lock_elems.into_iter().map(crate::render::RwlRenderElement::from));
                // Native locker: draw the password-progress bar above the
                // backdrop. (External `ext-session-lock` clients draw their own
                // surface via `lock_elems`, so `native_lock` is `None` then.)
                #[cfg(feature = "lock")]
                if let Some(lock) = self.native_lock.as_ref() {
                    combined.extend(
                        crate::features::lock::bar_elements(lock, &output)
                            .into_iter()
                            .map(crate::render::RwlRenderElement::Border),
                    );
                }
                if let Some(backdrop) = crate::render::lock_backdrop_element(&output) {
                    combined.push(crate::render::RwlRenderElement::Border(backdrop));
                }
                combined
            } else {

            // Elements are front-to-back (index 0 = topmost layer).
            // Normal:     Cursor → DnD → XCursor → Lock → Overlay → Top
            //               → borders → windows (incl. popups) → Bottom → Background
            // Fullscreen: Top layer omitted so bars don't overlay the window,
            //             UNLESS Overlay has surfaces, a Top surface requests
            //             keyboard focus (bar prompt), or the bar has an active
            //             title notification (set via `rwl msg -title`).
            let show_top = !has_fullscreen || !overlay_elems.is_empty() || top_has_kbd_focus || bar_has_notification;
            let mut combined = Vec::with_capacity(
                cursor_elems.len()
                    + dnd_elems.len()
                    + xcursor_elems.len()
                    + lock_elems.len()
                    + pip_elems.len()
                    + overlay_elems.len()
                    + if show_top { top_elems.len() } else { 0 }
                    + border_elems.len()
                    + window_elems.len()
                    + bottom_elems.len()
                    + bg_elems.len()
                    + wallpaper_elems.len(),
            );
            combined.extend(cursor_elems.into_iter().map(crate::render::RwlRenderElement::from));
            combined.extend(dnd_elems.into_iter().map(crate::render::RwlRenderElement::from));
            combined.extend(xcursor_elems.into_iter().map(crate::render::RwlRenderElement::from));
            combined.extend(lock_elems.into_iter().map(crate::render::RwlRenderElement::from));
            // PiP sits above the layer shells (bars) but below the cursor/lock.
            combined.extend(pip_elems);
            combined.extend(overlay_elems.into_iter().map(crate::render::RwlRenderElement::from));
            if show_top {
                combined.extend(top_elems.into_iter().map(crate::render::RwlRenderElement::from));
            }
            combined.extend(border_elems);
            combined.extend(window_elems);
            combined.extend(bottom_elems.into_iter().map(crate::render::RwlRenderElement::from));
            combined.extend(bg_elems.into_iter().map(crate::render::RwlRenderElement::from));
            combined.extend(wallpaper_elems);
            combined
            }
        };

        tracing::trace!("on_vblank: {} render elements", elements.len());

        // 3. Composite the frame.
        // Split into sub-steps so we can drop the renderer borrow before building
        // the presentation feedback (which needs self.space and the layer map).
        //
        // We also extract a clone of the primary-plane DMA-BUF here so that
        // screencopy frames can be served by re-binding the buffer after the
        // render_result borrow is released.
        let has_pending_screencopy = !self.pending_screencopy_frames.is_empty();
        let (is_empty, render_states, screencopy_dmabuf) = {
            let Some(backend) = self.backend_data_opt() else { return };
            let Some(dev) = backend.devices.get_mut(&device_id) else { return };
            let (surfaces, renderer) = (&mut dev.surfaces, &mut dev.renderer);
            let Some(surface) = surfaces.get_mut(&crtc) else { return };

            // OnDemand VRR: follow the presence of a fullscreen window, flipping
            // adaptive sync on/off only on transitions to avoid per-frame churn.
            if surface.vrr_mode == crate::config::VrrMode::OnDemand
                && surface.compositor.vrr_enabled() != output_has_fullscreen
                && (!output_has_fullscreen
                    || matches!(
                        surface.compositor.vrr_supported(surface.connector),
                        Ok(smithay::backend::drm::VrrSupport::Supported)
                    ))
                && let Err(e) = surface.compositor.use_vrr(output_has_fullscreen)
            {
                tracing::warn!("on-demand VRR toggle failed: {e}");
            }

            let fullscreen_bg = crate::config::get().fullscreen_bg;
            let render_result = match surface.compositor.render_frame(
                renderer,
                &elements,
                fullscreen_bg,
                FrameFlags::DEFAULT,
            ) {
                Ok(r) => r,
                Err(e) => {
                    tracing::error!("render_frame error: {e}");
                    return;
                }
            };

            // Export the rendered primary-plane buffer as a DMA-BUF so we can
            // re-bind it for screencopy readback outside this borrow scope.
            let screencopy_dmabuf: Option<Dmabuf> = if has_pending_screencopy {
                if let PrimaryPlaneElement::Swapchain(ref elem) = render_result.primary_element {
                    match elem.buffer().export() {
                        Ok(d) => Some(d),
                        Err(e) => {
                            tracing::warn!("screencopy: export primary dmabuf: {e:?}");
                            None
                        }
                    }
                } else {
                    tracing::debug!("screencopy: primary plane is direct scanout, cannot readback");
                    None
                }
            } else {
                None
            };

            (render_result.is_empty, render_result.states, screencopy_dmabuf)
        };

        // 3b. Serve pending screencopy frames by re-binding the just-rendered
        // DMA-BUF and copying its pixels into the client's SHM buffer.
        if let Some(mut dmabuf) = screencopy_dmabuf {
            if let Some(crate::backend::BackendData::Udev(ref mut backend)) = self.backend
                && let Some(dev) = backend.devices.get_mut(&device_id)
            {
                    match dev.renderer.bind(&mut dmabuf) {
                        Ok(target) => {
                            tracing::debug!("screencopy: bound dmabuf, submitting frame(s)");
                            crate::handlers::screencopy::submit_screencopy_frames(
                                &mut dev.renderer,
                                &target,
                                &output,
                                &mut self.pending_screencopy_frames,
                                time,
                            );
                        }
                        Err(e) => {
                            tracing::warn!("screencopy: bind dmabuf failed: {e}");
                            crate::handlers::screencopy::fail_screencopy_frames(
                                &output,
                                &mut self.pending_screencopy_frames,
                            );
                        }
                    }
            }
        } else if has_pending_screencopy {
            // No dmabuf available (e.g. direct scan-out element) — fail cleanly.
            tracing::debug!("screencopy: no dmabuf, failing all pending frames");
            crate::handlers::screencopy::fail_screencopy_frames(
                &output,
                &mut self.pending_screencopy_frames,
            );
        }

        // 3b-post. Update the primary-scanout-output per surface so that
        // take_presentation_feedback_surface_tree can collect wp_presentation_time
        // callbacks.  Without this every surface returns None from
        // surface_primary_scanout_output() and feedback is never sent — causing
        // Chromium-based clients (Firefox, Electron) to stall waiting for
        // wp_presentation_feedback.presented() and stay grey.
        self.space.elements().for_each(|window| {
            window.with_surfaces(|surface, states| {
                update_surface_primary_scanout_output(
                    surface,
                    &output,
                    states,
                    None,
                    &render_states,
                    default_primary_scanout_output_compare,
                );
            });
        });
        {
            let layer_map = smithay::desktop::layer_map_for_output(&output);
            layer_map.layers().for_each(|layer| {
                layer.with_surfaces(|surface, states| {
                    update_surface_primary_scanout_output(
                        surface,
                        &output,
                        states,
                        None,
                        &render_states,
                        default_primary_scanout_output_compare,
                    );
                });
            });
        }

        // 3c. Build wp_presentation feedback from the render element states.
        //     Only populated when there is actual damage to submit.
        let feedback = if is_empty {
            None
        } else {
            let mut fb = OutputPresentationFeedback::new(&output);
            for w in self.space.elements() {
                w.take_presentation_feedback(
                    &mut fb,
                    surface_primary_scanout_output,
                    |surf, _| {
                        surface_presentation_feedback_flags_from_states(surf, None, &render_states)
                    },
                );
            }
            let layer_map = smithay::desktop::layer_map_for_output(&output);
            layer_map.layers().for_each(|layer| {
                layer.take_presentation_feedback(
                    &mut fb,
                    surface_primary_scanout_output,
                    |surf, _| {
                        surface_presentation_feedback_flags_from_states(surf, None, &render_states)
                    },
                );
            });
            drop(layer_map);
            Some(fb)
        };

        // 3c. Queue the frame, attaching the feedback so VBlank can call presented().
        if !is_empty {
            let Some(backend) = self.backend_data_opt() else { return };
            let Some(dev) = backend.devices.get_mut(&device_id) else { return };
            let Some(surface) = dev.surfaces.get_mut(&crtc) else { return };

            match surface.compositor.queue_frame(feedback) {
                Ok(()) => {
                    surface.flip_pending = true;
                }
                Err(e) => {
                    tracing::error!("queue_frame error: {e}");
                    return;
                }
            }
        }

        // 4. Send frame-done callbacks so clients know when to produce the next frame.
        self.space.elements().for_each(|w| {
            if let Some(wl_surf) = w.wl_surface() {
                smithay::desktop::utils::send_frames_surface_tree(
                    &wl_surf,
                    &output,
                    time,
                    None,
                    |_, _| Some(output.clone()),
                );
                // Also send frame callbacks for all xdg_popup surfaces attached to this window.
                smithay::desktop::PopupManager::popups_for_surface(&wl_surf)
                    .for_each(|(popup, _)| {
                        smithay::desktop::utils::send_frames_surface_tree(
                            popup.wl_surface(),
                            &output,
                            time,
                            None,
                            |_, _| Some(output.clone()),
                        );
                    });
            }
        });

        // Layer-shell surfaces also need frame callbacks.
        let layer_map = smithay::desktop::layer_map_for_output(&output);
        layer_map.layers().for_each(|layer| {
            smithay::desktop::utils::send_frames_surface_tree(
                layer.wl_surface(),
                &output,
                time,
                None,
                |_, _| Some(output.clone()),
            );
        });
        drop(layer_map);

        // Lock surfaces need frame callbacks so screen-locker animations (e.g.
        // swaylock's typing indicator) can advance and so the locker's event
        // loop does not block waiting indefinitely for a callback that never comes.
        self.lock_surfaces
            .iter()
            .filter(|(_, o)| o == &output)
            .filter(|(s, _)| s.alive())
            .for_each(|(s, _)| {
                smithay::desktop::utils::send_frames_surface_tree(
                    s.wl_surface(),
                    &output,
                    time,
                    None,
                    |_, _| Some(output.clone()),
                );
            });

        // Keep a PiP window on a hidden tag alive by delivering it a frame
        // callback even though it is unmapped from the space.
        #[cfg(feature = "pip")]
        crate::features::pip::send_frame(self, &output, time);

        self.popup_manager.cleanup();

        // Keep the render loop alive for the duration of any fade or slide animation.
        #[cfg(feature = "fade")]
        if any_fading {
            self.schedule_render();
        }
        #[cfg(feature = "tag-transition")]
        if any_transitioning {
            self.schedule_render();
        }
        #[cfg(feature = "overview")]
        if any_overview {
            self.schedule_render();
        }
    }

    /// Schedule a re-render for all active outputs.
    /// Call this whenever surface content changes (e.g. from the commit handler) so
    /// the render loop restarts even when there was no pending `VBlank`.
    pub fn schedule_render(&self) {
        // Winit backend: request a redraw via the host compositor.
        // request_redraw() is coalesced by winit (many calls → one Redraw event)
        // and is rate-limited by the host compositor's vsync frame callbacks.
        // Using insert_idle here would bypass vsync and create a tight loop at
        // 100% CPU: commit → render → send_frames → commit → render → …
        #[cfg(feature = "winit")]
        if let Some(crate::backend::BackendData::Winit(ref data)) = self.backend {
            data.gfx.window().request_redraw();
            return;
        }

        let pairs: Vec<(libc::dev_t, crtc::Handle)> =
            match &self.backend {
                Some(crate::backend::BackendData::Udev(d)) => d
                    .devices
                    .iter()
                    .flat_map(|(dev_id, dev)| dev.surfaces.keys().map(|c| (*dev_id, *c)))
                    .collect(),
                _ => return,
            };
        for (device_id, crtc) in pairs {
            let _ = self.loop_handle.insert_idle(move |state| {
                // Skip if the device is paused (suspend / VT switch) — on_vblank
                // would attempt EGL/DRM ioctls that can block in the kernel when
                // the GPU is powered down, freezing the machine.
                let active = state
                    .backend_data_opt()
                    .and_then(|b| b.devices.get(&device_id))
                    .is_some_and(|d| d.drm.is_active());
                if !active {
                    return;
                }
                // Skip if a hardware page flip is already in flight — rendering
                // while flip_pending would submit a second queue_frame before the
                // first VBlank fires, causing the DRM compositor to stall.
                let flip_pending = state
                    .backend_data_opt()
                    .and_then(|b| b.devices.get(&device_id))
                    .and_then(|d| d.surfaces.get(&crtc))
                    .is_none_or(|s| s.flip_pending);
                if !flip_pending {
                    state.on_vblank(device_id, crtc);
                }
            });
        }
    }

    fn backend_data_opt(&mut self) -> Option<&mut UdevData> {
        if let Some(crate::backend::BackendData::Udev(ref mut d)) = self.backend {
            Some(&mut **d)
        } else {
            None
        }
    }

    /// Called by `SeatHandler::led_state_changed` to push the new LED state
    /// (`CapsLock`, `NumLock`, `ScrollLock`) to every connected physical keyboard.
    pub fn update_keyboard_leds(&mut self, led_state: smithay::input::keyboard::LedState) {
        let leds = libinput_leds_from_smithay(led_state);
        if let Some(backend) = self.backend_data_opt() {
            backend.kbd_devices.iter_mut().for_each(|kbd| kbd.led_update(leds));
        }
    }
}

/// Convert a Smithay `LedState` to a libinput `Led` bitmask.
/// Apply the current config to a single libinput device.
/// Free function so it can be called both on `DeviceAdded` and on config reload
/// without requiring `&mut Rwl` (avoids borrow conflicts with stored device lists).
fn apply_libinput_config(
    device: &mut smithay::reexports::input::Device,
    cfg: &crate::config::Config,
) {
    use smithay::reexports::input::DragLockState;
    if device.config_tap_finger_count() > 0 {
        let _ = device.config_tap_set_enabled(cfg.tap_to_click);
        let _ = device.config_tap_set_drag_enabled(cfg.tap_and_drag);
        let drag_lock = if cfg.drag_lock {
            DragLockState::EnabledTimeout
        } else {
            DragLockState::Disabled
        };
        let _ = device.config_tap_set_drag_lock_enabled(drag_lock);
        let _ = device.config_tap_set_button_map(cfg.tap_button_map);
    }
    if device.config_scroll_has_natural_scroll() {
        let _ = device.config_scroll_set_natural_scroll_enabled(cfg.natural_scrolling);
    }
    let _ = device.config_scroll_set_method(cfg.scroll_method);
    if device.config_click_methods().contains(&cfg.click_method) {
        let _ = device.config_click_set_method(cfg.click_method);
    }
    if device.config_left_handed_is_available() {
        let _ = device.config_left_handed_set(cfg.left_handed);
    }
    if device.config_middle_emulation_is_available() {
        let _ = device.config_middle_emulation_set_enabled(cfg.middle_button_emulation);
    }
    if device.config_dwt_is_available() {
        let _ = device.config_dwt_set_enabled(cfg.disable_while_typing);
    }
    if device.config_accel_is_available() {
        let _ = device.config_accel_set_profile(cfg.accel_profile);
        let _ = device.config_accel_set_speed(cfg.accel_speed);
    }
}

impl Rwl {
    /// Re-apply libinput config to all currently connected input devices.
    /// Called after a config reload so settings take effect without hotplugging.
    pub fn reapply_input_device_config(&mut self) {
        let Some(backend) = self.backend_data_opt() else { return };
        let mut devices = backend.input_devices.clone();
        let cfg = crate::config::get();
        for d in &mut devices { apply_libinput_config(d, &cfg); }
        if let Some(backend) = self.backend_data_opt() {
            backend.input_devices = devices;
        }
    }

    /// Re-apply each output's VRR policy after a config reload, so changing a
    /// monitor rule's `vrr` field takes effect without reconnecting the display.
    /// `OnDemand` outputs are left to the render loop; only `On`/`Off` are forced.
    pub fn reapply_vrr(&mut self) {
        let Some(backend) = self.backend_data_opt() else { return };
        for dev in backend.devices.values_mut() {
            for surface in dev.surfaces.values_mut() {
                let mode = crate::monitor::matching_rule_for_output(&surface.output).vrr;
                surface.vrr_mode = mode;
                if mode == crate::config::VrrMode::OnDemand {
                    continue;
                }
                let want = mode == crate::config::VrrMode::On;
                if surface.compositor.vrr_enabled() != want
                    && (!want
                        || matches!(
                            surface.compositor.vrr_supported(surface.connector),
                            Ok(smithay::backend::drm::VrrSupport::Supported)
                        ))
                    && let Err(e) = surface.compositor.use_vrr(want)
                {
                    tracing::warn!("reapply VRR failed: {e}");
                }
            }
        }
    }
}

fn libinput_leds_from_smithay(s: smithay::input::keyboard::LedState) -> libinput::Led {
    let mut leds = libinput::Led::empty();
    if s.num    == Some(true) { leds |= libinput::Led::NUMLOCK; }
    if s.caps   == Some(true) { leds |= libinput::Led::CAPSLOCK; }
    if s.scroll == Some(true) { leds |= libinput::Led::SCROLLLOCK; }
    leds
}
