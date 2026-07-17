//! Central compositor state — the `Rwl` struct that is threaded through the
//! calloop event loop and implements all Smithay protocol handlers.

use std::os::fd::AsFd;
use std::sync::Arc;

use smithay::desktop::{layer_map_for_output, PopupManager, Space, Window, WindowSurfaceType};
use smithay::input::keyboard::{KeyboardHandle, ModifiersState, XkbConfig};
use smithay::input::pointer::{CursorIcon, CursorImageStatus, PointerHandle};
use smithay::input::{Seat, SeatState};
use smithay::output::Output;
use smithay::reexports::calloop::generic::Generic;
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::reexports::calloop::{Interest, LoopHandle, LoopSignal, Mode, PostAction, RegistrationToken};
use smithay::reexports::wayland_server::backend::ClientData;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{Display, DisplayHandle};
use smithay::utils::{Clock, Logical, Monotonic, Point, Rectangle, SERIAL_COUNTER};
use smithay::wayland::compositor::{with_states, CompositorClientState, CompositorState};
use smithay::wayland::seat::WaylandFocus;
use smithay::wayland::dmabuf::{DmabufGlobal, DmabufState};
use smithay::wayland::drm_syncobj::{DrmSyncobjHandler, DrmSyncobjState};
use smithay::wayland::fractional_scale::FractionalScaleManagerState;
use smithay::wayland::output::OutputManagerState;
use smithay::wayland::presentation::PresentationState;
use smithay::wayland::selection::data_device::DataDeviceState;
use smithay::wayland::selection::primary_selection::PrimarySelectionState;
use smithay::wayland::selection::wlr_data_control::DataControlState;
use smithay::wayland::shell::wlr_layer::{ExclusiveZone, Layer as WlrLayer, WlrLayerShellState};
use smithay::wayland::shell::xdg::decoration::XdgDecorationState;
use smithay::wayland::shell::xdg::dialog::{ToplevelDialogHint, XdgDialogHandler, XdgDialogState};
use smithay::wayland::shell::xdg::{
    SurfaceCachedState, ToplevelSurface, XdgShellState, XdgToplevelSurfaceData,
};
use smithay::wayland::idle_inhibit::IdleInhibitManagerState;
use smithay::wayland::idle_notify::IdleNotifierState;
use smithay::wayland::session_lock::{LockSurface, SessionLockManagerState};
use smithay::wayland::shm::ShmState;
use smithay::wayland::socket::ListeningSocketSource;
use smithay::wayland::viewporter::ViewporterState;
use smithay::wayland::cursor_shape::CursorShapeManagerState;
use smithay::wayland::xdg_activation::XdgActivationState;

use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::reexports::wayland_server::backend::GlobalId;

#[cfg(feature = "xwayland")]
use smithay::wayland::xwayland_shell::{XWaylandShellHandler, XWaylandShellState};
#[cfg(feature = "xwayland")]
use smithay::xwayland::{X11Wm, XWayland, XWaylandEvent};

use crate::config::Action;
use crate::error::{RwlError, Result};
use crate::monitor::Monitor;
use crate::window::{
    window_is_floating, window_is_fullscreen, window_tags,
    window_visible_on, with_state, with_state_mut,
};

// ---------------------------------------------------------------------------
// Cursor mode
// ---------------------------------------------------------------------------

/// What the pointer is currently doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorMode {
    /// Normal pointer activity.
    Normal,
    /// Button held down.
    Pressed,
    /// Interactively moving a window.
    Move,
    /// Interactively resizing a window.
    Resize,
}

// ---------------------------------------------------------------------------
// Per-client Wayland state
// ---------------------------------------------------------------------------

/// Compositor state stored per Wayland client connection.
#[derive(Default)]
pub struct ClientState {
    /// Per-client compositor state (required by smithay).
    pub compositor_state: CompositorClientState,
}

impl ClientData for ClientState {
    fn initialized(&self, _client_id: smithay::reexports::wayland_server::backend::ClientId) {}
    fn disconnected(
        &self,
        _client_id: smithay::reexports::wayland_server::backend::ClientId,
        _reason: smithay::reexports::wayland_server::backend::DisconnectReason,
    ) {
    }
}

// ---------------------------------------------------------------------------
// Protocol-global holders
// ---------------------------------------------------------------------------

/// Protocol states held purely to keep their Wayland globals alive for the
/// compositor's lifetime.  None of these fields are read after `Rwl::new()`
/// returns; grouping them here means one `#[allow(dead_code)]` covers all.
#[allow(dead_code)]
struct ProtocolGlobals {
    output_manager_state:           OutputManagerState,
    xdg_decoration_state:           XdgDecorationState,
    xdg_dialog_state:               XdgDialogState,
    presentation_state:             PresentationState,
    fractional_scale_manager_state: FractionalScaleManagerState,
    viewporter_state:               ViewporterState,
    idle_inhibit_state:             IdleInhibitManagerState,
    /// `None` on the winit backend (no DRM device); set in `backend_device_added`.
    syncobj_state:                  Option<DrmSyncobjState>,
    gamma_manager_global:           GlobalId,
    screencopy_manager_global:      GlobalId,
    cursor_shape_manager:           CursorShapeManagerState,
}

// ---------------------------------------------------------------------------
// Main compositor state
// ---------------------------------------------------------------------------

/// The top-level compositor state, passed through the calloop event loop.
#[allow(clippy::struct_excessive_bools)]
pub struct Rwl {
    // ---- event-loop plumbing ----
    /// Handle to the calloop event loop for inserting new sources.
    pub loop_handle: LoopHandle<'static, Self>,
    /// Signal to stop the event loop.
    pub loop_signal: LoopSignal,
    /// Monotonic clock used for timestamps.
    pub clock: Clock<Monotonic>,

    // ---- Wayland display ----
    /// The Wayland display (stored for `dispatch_clients` calls).
    pub display: Option<Display<Self>>,
    /// Wayland display handle (used to create protocol objects).
    pub display_handle: DisplayHandle,

    // ---- protocol states ----
    pub compositor_state: CompositorState,
    pub shm_state: ShmState,
    /// Wayland globals that are kept alive by being held in `Rwl` but are
    /// never read after construction.  See [`ProtocolGlobals`].
    protocol_globals: ProtocolGlobals,
    pub xdg_shell_state: XdgShellState,
    pub layer_shell_state: WlrLayerShellState,
    pub seat_state: SeatState<Self>,
    pub data_device_state: DataDeviceState,
    pub primary_selection_state: PrimarySelectionState,
    /// `zwlr_data_control_manager_v1` — lets clipboard managers (cliphist,
    /// wl-clip-persist, wl-paste --watch, …) read and set the clipboard and
    /// primary selection WITHOUT a surface or keyboard focus.  Without this,
    /// such tools fall back to creating a surface and grabbing focus to set the
    /// selection, which steals focus from the active client and clears its
    /// text-selection highlight.  dwl exposes this via wlroots.
    pub data_control_state: DataControlState,
    pub xdg_activation_state: XdgActivationState,
    pub dmabuf_state: DmabufState,
    /// `ext_idle_notify_v1` — notifies clients (swayidle, hypridle, bars) when
    /// the seat has been idle for their requested timeout.
    pub idle_notifier: IdleNotifierState<Self>,
    /// Surfaces that currently hold a live idle-inhibitor object.
    /// When this set is non-empty the [`idle_notifier`] is inhibited.
    pub inhibited_surfaces: std::collections::HashSet<WlSurface>,

    // ---- desktop / window management ----
    pub space: Space<Window>,
    pub popup_manager: PopupManager,

    // ---- input ----
    pub seat: Seat<Self>,
    pub keyboard: Option<KeyboardHandle<Self>>,
    pub pointer: Option<PointerHandle<Self>>,
    pub cursor_status: CursorImageStatus,
    /// Surface used as the drag icon during an active `DnD` operation.
    /// `None` when no `DnD` is in progress or the client provided no icon.
    pub dnd_icon: Option<WlSurface>,
    pub cursor_mode: CursorMode,
    /// Calloop token for the pending cursor-hide timer.
    /// `None` when no timer is active (cursor already hidden or no activity yet).
    pub cursor_hide_timer: Option<RegistrationToken>,
    /// Calloop token for the compositor keybinding key-repeat timer.
    /// Fires the same action repeatedly while a bound key is held.
    pub key_repeat_timer: Option<RegistrationToken>,
    /// The action being repeated by `key_repeat_timer`.
    pub key_repeat_action: Option<Action>,
    /// Whether the cursor is currently hidden due to inactivity.
    /// Kept separate from `cursor_status` so the client's surface cursor is
    /// preserved through hide/show cycles (avoids the client needing to
    /// re-send `wl_pointer.set_cursor` every time the cursor un-hides).
    pub cursor_hidden: bool,
    /// Last surface-based cursor set by any client (never overwritten by
    /// Named/Hidden resets).  Used as a software-cursor fallback when the
    /// pointer is over empty space and `cursor_status` is Named or Hidden
    /// (e.g. after a video player hid the cursor and the user switched tags).
    pub last_cursor_surface: Option<WlSurface>,

    // ---- monitors ----
    pub monitors: Vec<Monitor>,
    pub sel_mon: usize,
    /// History of active tag masks per monitor, indexed by monitor index.
    pub tag_history: Vec<std::collections::VecDeque<u32>>,
    /// Cached bounding box (`min_x`, `min_y`, `max_x`, `max_y`) of all monitor
    /// geometries. Updated in `output_added`/`output_removed` so `clamp_pointer()`
    /// never iterates monitors on the hot pointer-motion path.
    pub monitor_bounds: (i32, i32, i32, i32),

    // ---- window lists ----
    pub windows: Vec<Window>,
    pub focus_stack: Vec<Window>,
    /// O(1) lookup from a toplevel `WlSurface` to its Window. Kept in sync with
    /// `windows` (insert in `new_toplevel`, remove in `toplevel_destroyed`).
    pub window_map: std::collections::HashMap<WlSurface, Window>,

    // ---- move / resize grab ----
    pub grabbed_window: Option<Window>,
    pub grab_start: Option<Point<f64, Logical>>,
    pub grab_geom: Option<Rectangle<i32, Logical>>,
    /// Monitor `mfact` at the start of a tiled-window resize grab (layouts whose
    /// resize adjusts the master/stack split).  `None` when no grab is active or
    /// the grab is on a floating window.
    pub grab_mfact: Option<f64>,
    /// The grabbed window's `cfact` at the start of a tiled-window resize grab.
    /// `None` when no grab is active or the grab is on a floating window.
    pub grab_cfact: Option<f64>,
    /// Index of the grabbed window's column at the start of a col-layout resize.
    #[cfg(feature = "col")]
    pub grab_col_idx: Option<usize>,
    /// Snapshot of `monitor.col_facts` at the start of a col-layout resize.
    #[cfg(feature = "col")]
    pub grab_col_facts: Option<Vec<f64>>,
    /// Whether the grabbed window was already floating before the grab started.
    /// Used by `end_grab` to restore tiled windows to tiled after a move drag.
    pub grab_was_floating: bool,

    // ---- session lock ----
    pub session_lock_state: SessionLockManagerState,
    /// Active lock surfaces, one per output, while the session is locked.
    pub lock_surfaces: Vec<(LockSurface, Output)>,
    /// `true` while an `ext-session-lock-v1` session lock is active.
    pub locked: bool,
    /// Active native (built-in) screen locker, or `None`. Mutually exclusive
    /// with `ext-session-lock-v1` clients; both set `locked`.
    #[cfg(feature = "lock")]
    pub native_lock: Option<crate::features::lock::LockState>,

    // ---- overview / exposé ----
    /// Active workspace-overview (Exposé) session, or `None` when inactive.
    #[cfg(feature = "overview")]
    pub overview: Option<crate::features::overview::OverviewState>,
    /// In-progress tap-modkey gesture: `(started_at, peak_modifier_bits)`.
    /// `None` unless modifier keys are being held with no other key pressed yet;
    /// the bits accumulate every modifier held so combos like `M..S` can match.
    #[cfg(feature = "mod-tap")]
    pub mod_tap: Option<(std::time::Instant, u32)>,

    // ---- picture-in-picture ----
    /// Active PiP session (a window mirrored into a corner), or `None`.
    #[cfg(feature = "pip")]
    pub pip: Option<crate::features::pip::PipState>,

    // ---- compositor-level flags ----
    pub passthrough: bool,
    pub running: bool,
    /// Set to `true` after the first startup cursor warp so it only fires once.
    #[cfg(feature = "warp")]
    pub initial_warp_done: bool,

    // ---- IPC ----
    pub socket_name: String,
    /// Path to the WM IPC socket (`rwl.sock`); exported to spawned children as
    /// `RWL_SOCK` via [`Rwl::configure_child`]. `None` until the IPC server binds.
    #[cfg(feature = "ipc")]
    #[allow(clippy::struct_field_names)] // mirrors the RWL_SOCK env var name
    pub rwl_sock: Option<std::path::PathBuf>,
    /// Write end of the startup-command pipe; status output is sent here when set.
    pub ipc_out: Option<std::io::PipeWriter>,
    /// Persistent subscriber streams: clients that sent `subscribe [output]` and stay
    /// connected to receive pushed status updates on every `print_status()` call.
    /// Each entry is `(stream, output_filter)`.  Dead streams are pruned on write error.
    #[cfg(feature = "ipc")]
    pub ipc_subscribers: Vec<(std::os::unix::net::UnixStream, Option<String>)>,
    /// Persistent `watch` subscribers: clients streaming structured JSON events
    /// (`rwl msg watch`). Each entry is `(stream, event_filter)`; a present filter
    /// limits delivery to the listed event kinds. Dead streams are pruned on
    /// write error. See [`crate::features::ipc::event`].
    #[cfg(feature = "ipc")]
    pub event_subscribers: Vec<(std::os::unix::net::UnixStream, Option<Vec<String>>)>,
    /// Set by the embedded bar thread when any bar has an active title notification
    /// (set via `rwl msg -title`). Read by the render loop to reveal the Top layer
    /// above fullscreen windows while the notification is on-screen.
    #[cfg(feature = "bar")]
    pub bar_has_notification: Arc<std::sync::atomic::AtomicBool>,

    // ---- xcursor (TTY named cursor) ----
    /// Cursor image cache keyed by [`CursorIcon`].  Populated lazily on first use;
    /// `Option::None` means the icon was tried but not found in any theme.
    pub cursor_cache: std::collections::HashMap<CursorIcon, Option<crate::render::NamedCursorBuffer>>,

    // ---- backend ----
    pub backend: Option<crate::backend::BackendData>,
    /// `zwp_linux_dmabuf_v1` global — keeps the global alive and lets clients
    /// discover GPU-supported buffer formats for zero-copy import.
    pub dmabuf_global: Option<DmabufGlobal>,

    // ---- gamma control ----
    /// Tracks which client has exclusive gamma control per output.
    pub gamma_controls: crate::handlers::gamma_control::GammaControlMap,
    /// Last gamma ramp applied per output (red+green+blue packed, u16 LE).
    /// Persisted across suspend/resume so we can re-apply after the hardware
    /// gamma LUT is wiped by the kernel on session resume / VT switch.
    pub gamma_ramps: std::collections::HashMap<Output, Vec<u16>>,

    // ---- screencopy ----
    /// Screencopy frames waiting for the next render pass.
    pub pending_screencopy_frames: Vec<crate::handlers::screencopy::PendingFrame>,

    // ---- arrange() scratch buffers ----
    // Reused across calls to avoid repeated heap allocation on every layout pass.
    arrange_hidden:   Vec<usize>,
    arrange_tiled:    Vec<usize>,
    arrange_floating: Vec<usize>,
    arrange_fs:       Vec<usize>,
    #[cfg(feature = "scratchpad")]
    arrange_scratch:  Vec<usize>,

    /// Timestamp of when the render gate first became active for the current
    /// layout transition.  Reset to `None` when the gate clears.  Used to
    /// enforce a maximum gate duration so rapid key-repeat window opening
    /// cannot starve the render loop indefinitely.
    pub layout_gate_since: Option<std::time::Instant>,
    /// Pending tag-transition set by `apply_rules` when a window rule causes an
    /// automatic tag switch.  Consumed by `arrange_all()` to fire slide-in
    /// animations for the newly visible windows after the layout pass.
    /// `(old_tag, new_tag, output_w_logical)`
    #[cfg(feature = "tag-transition")]
    pub pending_tag_transition: Option<(u32, u32, f64)>,
    /// `true` while any tag-transition slide animation (in or out) is active.
    /// The moment this flips from `true` to `false`, a fresh configure is sent
    /// Tracks whether a tag-transition animation was running on the previous
    /// frame.  When it flips true→false (the animation just ended) the render
    /// loop re-runs `arrange_all()`, which re-maps and re-configures windows
    /// brought onto the tag by `switch_to_tag` now that they are buffer-mapped.
    /// Without this, a window arranged during its first (buffer-less) commit can
    /// stay showing a blank/grey initial frame until a manual re-arrange.
    #[cfg(feature = "tag-transition")]
    pub tag_transition_was_active: bool,
    /// Counts down for a short window after a tag-transition animation completes.
    /// While non-zero the render loop keeps scheduling frames and, partway
    /// through, re-runs `arrange_all()` a second time to catch clients (e.g.
    /// video players) that commit their first real buffer only after the
    /// transition has finished.
    #[cfg(feature = "tag-transition")]
    pub post_transition_wakeup: u32,

    // ---- XWayland ----
    /// `xwayland_shell_v1` global — lets `XWayland` associate its X11 windows with
    /// their backing `wl_surface`s.  `None` when `RWL_NO_XWAYLAND` disables `XWayland`.
    #[cfg(feature = "xwayland")]
    pub xwayland_shell_state: Option<XWaylandShellState>,
    /// The X11 window manager, set once `XWayland` reports Ready. `None` until then
    /// (and on the winit backend if `XWayland` fails to start).
    #[cfg(feature = "xwayland")]
    pub xwm: Option<X11Wm>,
    /// The X11 `DISPLAY` number `XWayland` is serving on, exported to child procs.
    #[cfg(feature = "xwayland")]
    pub xdisplay: Option<u32>,
    /// `XWayland` override-redirect surfaces (menus, tooltips, drag icons).  Kept
    /// separate from `windows` so they render (they are mapped in `space`) but
    /// are never tiled, focused, or given borders.
    #[cfg(feature = "xwayland")]
    pub x11_or_windows: Vec<Window>,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

struct XkbParams {
    rules:        String,
    model:        String,
    layout:       String,
    variant:      String,
    options:      Option<String>,
    repeat_delay: i32,
    repeat_rate:  i32,
}

/// Read XKB and repeat settings from the current config into owned strings.
/// Shared by `init_input()` and `reapply_keyboard_config()` to avoid
/// duplicating the field-extraction block.
fn xkb_params_from_cfg() -> XkbParams {
    let cfg = crate::config::get();
    XkbParams {
        rules:        cfg.xkb_rules.rules.as_deref().unwrap_or("").to_owned(),
        model:        cfg.xkb_rules.model.as_deref().unwrap_or("").to_owned(),
        layout:       cfg.xkb_rules.layout.as_deref().unwrap_or("").to_owned(),
        variant:      cfg.xkb_rules.variant.as_deref().unwrap_or("").to_owned(),
        options:      cfg.xkb_rules.options.clone(),
        repeat_delay: cfg.repeat_delay,
        repeat_rate:  cfg.repeat_rate,
    }
}

// ---------------------------------------------------------------------------
// Construction
// ---------------------------------------------------------------------------

impl Rwl {
    /// Create the compositor state and insert the Wayland display source into
    /// the event loop.
    ///
    /// # Errors
    /// Returns an error if the Wayland socket cannot be created.
    #[allow(clippy::too_many_lines)]
    pub fn new(
        display: Display<Self>,
        loop_handle: LoopHandle<'static, Self>,
        loop_signal: LoopSignal,
    ) -> Result<Self> {
        let display_handle = display.handle();
        let clock = Clock::<Monotonic>::new();

        // Duplicate the display poll FD so calloop can wake up when clients
        // send requests.  The Display itself is stored in state so
        // dispatch_clients can be called safely (no unsafe needed).
        let poll_fd = display.as_fd().try_clone_to_owned()?;
        loop_handle
            .insert_source(
                Generic::new(poll_fd, Interest::READ, Mode::Level),
                |_, _fd, state| {
                    if let Some(mut d) = state.display.take() {
                        d.dispatch_clients(state).ok();
                        state.display = Some(d);
                    }
                    Ok(PostAction::Continue)
                },
            )
            .map_err(|e| RwlError::EventLoop(e.to_string()))?;

        // Set up the listening socket for Wayland client connections.
        let socket_source = ListeningSocketSource::new_auto()
            .map_err(|e| RwlError::Socket(e.to_string()))?;
        let socket_name = socket_source.socket_name().to_string_lossy().into_owned();
        tracing::info!("Listening on Wayland socket: {}", socket_name);

        loop_handle
            .insert_source(socket_source, |client_stream, (), state| {
                if let Err(e) = state.display_handle.insert_client(
                    client_stream,
                    Arc::new(ClientState::default()),
                ) {
                    tracing::warn!("Error adding wayland client: {}", e);
                }
            })
            .map_err(|e| RwlError::EventLoop(e.to_string()))?;

        // Protocol states
        let compositor_state = CompositorState::new::<Self>(&display_handle);
        let shm_state = ShmState::new::<Self>(&display_handle, vec![]);
        let xdg_shell_state = XdgShellState::new::<Self>(&display_handle);
        let layer_shell_state = WlrLayerShellState::new::<Self>(&display_handle);
        let mut seat_state = SeatState::new();
        let data_device_state = DataDeviceState::new::<Self>(&display_handle);
        let primary_selection_state = PrimarySelectionState::new::<Self>(&display_handle);
        // Enable data-control with primary-selection support so clipboard
        // managers use the focus-less path instead of grabbing keyboard focus.
        let data_control_state = DataControlState::new::<Self, _>(
            &display_handle,
            Some(&primary_selection_state),
            |_| true,
        );
        let xdg_activation_state = XdgActivationState::new::<Self>(&display_handle);
        let dmabuf_state = DmabufState::new();
        let idle_notifier = IdleNotifierState::<Self>::new(&display_handle, loop_handle.clone());
        let session_lock_state = SessionLockManagerState::new::<Self, _>(&display_handle, |_| true);
        let protocol_globals = ProtocolGlobals {
            output_manager_state:           OutputManagerState::new_with_xdg_output::<Self>(&display_handle),
            xdg_decoration_state:           XdgDecorationState::new::<Self>(&display_handle),
            xdg_dialog_state:               XdgDialogState::new::<Self>(&display_handle),
            presentation_state:             PresentationState::new::<Self>(&display_handle, clock.id() as u32),
            fractional_scale_manager_state: FractionalScaleManagerState::new::<Self>(&display_handle),
            viewporter_state:               ViewporterState::new::<Self>(&display_handle),
            idle_inhibit_state:             IdleInhibitManagerState::new::<Self>(&display_handle),
            syncobj_state:                  None,
            gamma_manager_global:           display_handle.create_global::<Self, _, _>(
                1,
                crate::handlers::gamma_control::GammaManagerData,
            ),
            screencopy_manager_global:      display_handle.create_global::<Self, _, _>(
                3,
                crate::handlers::screencopy::ScreencopyManagerData,
            ),
            cursor_shape_manager:           CursorShapeManagerState::new::<Self>(&display_handle),
        };

        // Gate the xwayland_shell global on the same env kill-switch as the spawn,
        // so `RWL_NO_XWAYLAND=1` fully disables all XWayland runtime code.
        #[cfg(feature = "xwayland")]
        let xwayland_shell_state = if std::env::var_os("RWL_NO_XWAYLAND").is_none() {
            Some(XWaylandShellState::new::<Self>(&display_handle))
        } else {
            None
        };

        // Seat
        let seat = seat_state.new_wl_seat(&display_handle, "seat0");

        Ok(Self {
            loop_handle,
            loop_signal,
            clock,
            display: Some(display),
            display_handle,
            compositor_state,
            shm_state,
            protocol_globals,
            xdg_shell_state,
            layer_shell_state,
            seat_state,
            data_device_state,
            primary_selection_state,
            data_control_state,
            xdg_activation_state,
            dmabuf_state,
            idle_notifier,
            session_lock_state,
            lock_surfaces: Vec::new(),
            locked: false,
            #[cfg(feature = "lock")]
            native_lock: None,
            inhibited_surfaces: std::collections::HashSet::new(),
            space: Space::default(),
            popup_manager: PopupManager::default(),
            seat,
            keyboard: None,
            pointer: None,
            cursor_status: CursorImageStatus::default_named(),
            dnd_icon: None,
            cursor_mode: CursorMode::Normal,
            cursor_hide_timer: None,
            key_repeat_timer: None,
            key_repeat_action: None,
            cursor_hidden: false,
            last_cursor_surface: None,
            monitors: Vec::new(),
            sel_mon: 0,
            tag_history: Vec::new(),
            monitor_bounds: (0, 0, 0, 0),
            windows: Vec::new(),
            focus_stack: Vec::new(),
            window_map: std::collections::HashMap::new(),
            grabbed_window: None,
            grab_start: None,
            grab_geom: None,
            grab_mfact: None,
            grab_cfact: None,
            #[cfg(feature = "col")]
            grab_col_idx: None,
            #[cfg(feature = "col")]
            grab_col_facts: None,
            grab_was_floating: false,
            #[cfg(feature = "overview")]
            overview: None,
            #[cfg(feature = "mod-tap")]
            mod_tap: None,
            #[cfg(feature = "pip")]
            pip: None,
            passthrough: false,
            running: true,
            #[cfg(feature = "warp")]
            initial_warp_done: false,
            socket_name,
            #[cfg(feature = "ipc")]
            rwl_sock: None,
            ipc_out: None,
            #[cfg(feature = "ipc")]
            ipc_subscribers: Vec::new(),
            #[cfg(feature = "ipc")]
            event_subscribers: Vec::new(),
            #[cfg(feature = "bar")]
            bar_has_notification: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            cursor_cache: {
                let cfg = crate::config::get();
                let size = if cfg.cursor_size > 0 { cfg.cursor_size } else {
                    std::env::var("XCURSOR_SIZE").ok().and_then(|s| s.parse().ok()).unwrap_or(24)
                };
                let default_cursor = crate::render::load_cursor_icon(
                    cfg.cursor_theme.as_deref(), size, CursorIcon::Default,
                ).or_else(|| Some(crate::render::load_default_cursor(cfg.cursor_theme.as_deref(), size)));
                drop(cfg);
                let mut cache = std::collections::HashMap::new();
                cache.insert(CursorIcon::Default, default_cursor);
                cache
            },
            backend: None,
            dmabuf_global: None,
            gamma_controls: std::collections::HashMap::new(),
            gamma_ramps: std::collections::HashMap::new(),
            pending_screencopy_frames: Vec::new(),
            arrange_hidden:   Vec::new(),
            arrange_tiled:    Vec::new(),
            arrange_floating: Vec::new(),
            arrange_fs:       Vec::new(),
            #[cfg(feature = "scratchpad")]
            arrange_scratch:  Vec::new(),
            layout_gate_since: None,
            #[cfg(feature = "tag-transition")]
            pending_tag_transition: None,
            #[cfg(feature = "tag-transition")]
            tag_transition_was_active: false,
            #[cfg(feature = "tag-transition")]
            post_transition_wakeup: 0,
            #[cfg(feature = "xwayland")]
            xwayland_shell_state,
            #[cfg(feature = "xwayland")]
            xwm: None,
            #[cfg(feature = "xwayland")]
            xdisplay: None,
            #[cfg(feature = "xwayland")]
            x11_or_windows: Vec::new(),
        })
    }

    /// Spawn `XWayland` and hook its readiness event into the calloop loop.
    ///
    /// On `Ready` we start an [`X11Wm`] bound to the privileged X11 socket and
    /// export `DISPLAY` so any process launched afterwards (startup commands,
    /// terminals, Steam) can reach `XWayland`.  Called once from `main` after the
    /// backend and globals are up.
    #[cfg(feature = "xwayland")]
    pub fn start_xwayland(&self) {
        let (xwayland, client) = match XWayland::spawn(
            &self.display_handle,
            None,
            std::iter::empty::<(String, String)>(),
            true,
            std::process::Stdio::null(),
            std::process::Stdio::null(),
            |_| {},
        ) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("failed to spawn XWayland: {e}");
                return;
            }
        };

        let dh = self.display_handle.clone();
        let res = self.loop_handle.insert_source(xwayland, move |event, (), state| match event {
            XWaylandEvent::Ready { x11_socket, display_number } => {
                match X11Wm::start_wm(state.loop_handle.clone(), &dh, x11_socket, client.clone()) {
                    Ok(wm) => {
                        state.xwm = Some(wm);
                        // Stored so `configure_child` can export DISPLAY to every
                        // process rwl spawns (startup cmds, terminals, Steam, games)
                        // without mutating the compositor's own environment.
                        state.xdisplay = Some(display_number);
                        tracing::info!("XWayland ready on DISPLAY=:{display_number}");
                    }
                    Err(e) => tracing::error!("failed to start X11 window manager: {e}"),
                }
            }
            XWaylandEvent::Error => {
                tracing::warn!("XWayland crashed on startup");
            }
        });
        if let Err(e) = res {
            tracing::error!("failed to insert XWayland source into event loop: {e}");
        }
    }

    /// Whether the `wp_linux_drm_syncobj_manager_v1` global has been created.
    pub(crate) const fn has_syncobj_state(&self) -> bool {
        self.protocol_globals.syncobj_state.is_some()
    }

    /// Store the `wp_linux_drm_syncobj_manager_v1` state after the DRM device is opened.
    pub(crate) fn set_syncobj_state(&mut self, state: DrmSyncobjState) {
        self.protocol_globals.syncobj_state = Some(state);
    }

    /// Re-apply XKB config and repeat settings from the current config to the live keyboard.
    /// Called after a config reload so changes take effect without restarting.
    pub fn reapply_keyboard_config(&mut self) {
        let Some(kb) = self.keyboard.clone() else { return };
        // Save physically-held modifiers before set_xkb_config() wipes XKB state,
        // so keybinds keep working when Mod is still held during a config reload.
        let held = kb.modifier_state();
        let p = xkb_params_from_cfg();
        let xkb_config = XkbConfig {
            rules:   &p.rules,
            model:   &p.model,
            layout:  &p.layout,
            variant: &p.variant,
            options: p.options,
        };
        if let Err(e) = kb.set_xkb_config(self, xkb_config) {
            tracing::error!("Failed to apply new XKB config: {}", e);
        }
        kb.change_repeat_info(p.repeat_rate, p.repeat_delay);
        // set_xkb_config() creates a fresh XKB state — re-lock NumLock/CapsLock
        // from config, and restore any physical modifier keys that were held.
        let (numlock, capslock) = {
            let cfg = crate::config::get();
            (cfg.numlock, cfg.capslock)
        };
        let mods = ModifiersState {
            num_lock:         numlock,
            caps_lock:        capslock,
            logo:             held.logo,
            ctrl:             held.ctrl,
            shift:            held.shift,
            alt:              held.alt,
            iso_level3_shift: held.iso_level3_shift,
            iso_level5_shift: held.iso_level5_shift,
            ..Default::default()
        };
        kb.set_modifier_state(mods);
        kb.advertise_modifier_state(self);
        self.update_keyboard_leds(kb.led_state());
    }

    /// Initialise keyboard and pointer devices on the seat.
    pub fn init_input(&mut self) {
        let p = xkb_params_from_cfg();
        let xkb_config = XkbConfig {
            rules:   &p.rules,
            model:   &p.model,
            layout:  &p.layout,
            variant: &p.variant,
            options: p.options,
        };

        match self.seat.add_keyboard(xkb_config, p.repeat_delay, p.repeat_rate) {
            Ok(kb) => {
                let (numlock, capslock) = {
                    let cfg = crate::config::get();
                    (cfg.numlock, cfg.capslock)
                };
                let mods = ModifiersState { num_lock: numlock, caps_lock: capslock, ..Default::default() };
                kb.set_modifier_state(mods);
                // Notify any already-focused client and prime the LED state so
                // configure_libinput_device() can read it when keyboard devices
                // are enumerated during the first event-loop poll.
                kb.advertise_modifier_state(self);
                self.keyboard = Some(kb);
            }
            Err(e) => tracing::error!("Failed to add keyboard: {}", e),
        }

        self.pointer = Some(self.seat.add_pointer());

        // Winit backend: output is added before init_input(), so monitors are
        // already present here — do the initial centre warp immediately.
        #[cfg(feature = "warp")]
        if !self.initial_warp_done && !self.monitors.is_empty() {
            self.initial_warp_done = true;
            crate::features::warp::warp_cursor_to_focused(self);
        }
    }
}

// ---------------------------------------------------------------------------
// Window management helpers
// ---------------------------------------------------------------------------

/// Accumulated output of applying all matching window rules.
struct RuleAccum {
    tags:          u32,
    is_floating:   bool,
    scratch_key:   char,
    monitor:       i32,
    switch_to_tag: Option<u32>,
}

impl Rwl {
    /// Return the currently focused window.
    #[must_use]
    pub fn focused_window(&self) -> Option<&Window> {
        let tags = self.sel_monitor().map_or(0, Monitor::tags);
        let sel = self.sel_mon;
        // Also check mon_idx so that when two monitors show the same tagset,
        // actions (kill, float-toggle, etc.) target this monitor's own windows.
        // Single borrow: combines the window_visible_on (tags check) and mon_idx check.
        self.focus_stack
            .iter()
            .find(|w| with_state(w, |s| s.tags & tags != 0 && s.mon_idx == sel).unwrap_or(false))
    }

    /// In tile layout, returns the master window (first tiled, non-floating,
    /// non-fullscreen window on the selected monitor's current tagset).
    /// Returns `None` for other layouts or when there are no tiled windows.
    #[cfg(feature = "tile")]
    pub fn tile_master_window(&self) -> Option<&Window> {
        use crate::config::LayoutKind;
        let mon = self.sel_monitor()?;
        if mon.layout_kind() != LayoutKind::Tile {
            return None;
        }
        let tags = mon.tags();
        let sel = self.sel_mon;
        self.windows.iter().find(|w| {
            with_state(w, |s| {
                s.mon_idx == sel
                    && s.tags & tags != 0
                    && s.rules_applied
                    && !s.is_floating
                    && !s.is_fullscreen
            })
            .unwrap_or(false)
        })
    }

    /// When the tile layout feature is disabled there is no master concept.
    #[cfg(not(feature = "tile"))]
    #[must_use]
    pub fn tile_master_window(&self) -> Option<&Window> {
        None
    }

    #[must_use]
    pub fn sel_monitor(&self) -> Option<&Monitor> {
        self.monitors.get(self.sel_mon)
    }

    pub fn sel_monitor_mut(&mut self) -> Option<&mut Monitor> {
        self.monitors.get_mut(self.sel_mon)
    }

    #[must_use]
    pub fn monitor_for_output(&self, output: &Output) -> Option<usize> {
        self.monitors.iter().position(|m| &m.output == output)
    }

    #[must_use]
    #[allow(clippy::cast_possible_truncation)]
    pub fn monitor_at(&self, x: f64, y: f64) -> Option<usize> {
        let pt = Point::<i32, Logical>::from((x as i32, y as i32));
        self.monitors.iter().position(|m| m.m.contains(pt))
    }

    #[must_use]
    pub fn window_for_surface(&self, surface: &WlSurface) -> Option<&Window> {
        self.window_map.get(surface)
    }

    /// Recompute and cache the union bounding box of all monitor geometries.
    /// Must be called after any monitor is added, removed, or repositioned.
    pub fn update_monitor_bounds(&mut self) {
        self.monitor_bounds = self.monitors.iter()
            .map(|m| (m.m.loc.x, m.m.loc.y, m.m.loc.x + m.m.size.w, m.m.loc.y + m.m.size.h))
            .reduce(|(ax1, ay1, ax2, ay2), (x1, y1, x2, y2)| {
                (ax1.min(x1), ay1.min(y1), ax2.max(x2), ay2.max(y2))
            })
            .unwrap_or((0, 0, 0, 0));
    }

    /// Return the topmost `(WlSurface, surface_origin_in_space)` pair at `loc`.
    ///
    /// Used to re-establish pointer focus after subsurfaces are
    /// destroyed/recreated (e.g. `QtWebEngine` during page navigation).
    #[must_use]
    pub fn surface_under(&self, loc: Point<f64, Logical>) -> Option<(WlSurface, Point<f64, Logical>)> {
        self.space
            .element_under(loc)
            .and_then(|(window, render_loc)| {
                let point_local = loc - render_loc.to_f64();
                window
                    .surface_under(point_local, WindowSurfaceType::ALL)
                    .map(|(surface, surface_local_loc)| {
                        let origin = render_loc.to_f64() + surface_local_loc.to_f64();
                        (surface, origin)
                    })
            })
    }

    /// Find the topmost layer-shell surface on `layer` at global logical `loc`.
    ///
    /// Returns `(WlSurface, surface_origin_in_global_space)` or `None`.
    /// `geo` from the layer map is output-local; we add `mon.m.loc` to get
    /// global coordinates.
    fn layer_surf_at(
        &self,
        loc: Point<f64, Logical>,
        layer: WlrLayer,
    ) -> Option<(WlSurface, Point<f64, Logical>)> {
        self.monitors.iter().find_map(|mon| {
            let output_origin = mon.m.loc;
            // Collect (surface, global_loc) while holding the map guard, then
            // drop the guard before calling surface_under (which may re-enter
            // the map).  Mirrors the pattern in render::layer_elements.
            let layer_data: Vec<_> = {
                let map = layer_map_for_output(&mon.output);
                // Skip allocation when this layer has no surfaces (common for
                // Overlay, Bottom, and Background on typical setups).
                map.layers_on(layer).next()?;
                // layers_on yields oldest→newest; .rev() = topmost first.
                map.layers_on(layer)
                    .rev()
                    .filter_map(|surf| {
                        map.layer_geometry(surf).map(|geo| {
                            let global_loc: Point<i32, Logical> = (
                                output_origin.x + geo.loc.x,
                                output_origin.y + geo.loc.y,
                            ).into();
                            (surf.clone(), global_loc)
                        })
                    })
                    .collect()
            }; // map guard dropped here

            layer_data.into_iter().find_map(|(surf, global_loc)| {
                let surface_local = loc - global_loc.to_f64();
                surf.surface_under(surface_local, WindowSurfaceType::ALL)
                    .map(|(wl_surf, offset)| (wl_surf, global_loc.to_f64() + offset.to_f64()))
            })
        })
    }

    /// Return the topmost `(WlSurface, surface_origin_in_space)` pair under
    /// `loc`, respecting the compositor's front-to-back z-order:
    ///
    /// ```text
    /// Overlay layers → Top layers → XDG windows → Bottom layers → Background layers
    /// ```
    ///
    /// Use this for all pointer focus decisions so that bars, overlays, and
    /// wallpapers receive input correctly.
    #[must_use]
    pub fn pointer_focus_under(
        &self,
        loc: Point<f64, Logical>,
    ) -> Option<(WlSurface, Point<f64, Logical>)> {
        // When the session is locked, only lock surfaces receive pointer input.
        if self.locked {
            let pt = loc.to_i32_floor();
            return self.lock_surfaces.iter()
                .filter(|(surf, _)| surf.alive())
                .find_map(|(surf, out)| {
                    let out_loc = out.current_location();
                    #[allow(clippy::cast_possible_truncation)]
                    let out_size = out.current_mode()
                        .map(|m| {
                            let scale = out.current_scale().fractional_scale();
                            let w = (f64::from(m.size.w) / scale).round() as i32;
                            let h = (f64::from(m.size.h) / scale).round() as i32;
                            smithay::utils::Size::from((w, h))
                        })
                        .unwrap_or_default();
                    smithay::utils::Rectangle::new(out_loc, out_size)
                        .contains(pt)
                        .then(|| (surf.wl_surface().clone(), out_loc.to_f64()))
                });
        }

        // z-order: Overlay/Top → XDG windows → Bottom/Background
        [WlrLayer::Overlay, WlrLayer::Top].into_iter()
            .find_map(|layer| self.layer_surf_at(loc, layer))
            .or_else(|| self.surface_under(loc))
            .or_else(|| {
                [WlrLayer::Bottom, WlrLayer::Background].into_iter()
                    .find_map(|layer| self.layer_surf_at(loc, layer))
            })
    }

    #[must_use]
    #[allow(clippy::cast_sign_loss)]
    pub fn target_monitor_for_window(&self, rule_monitor: i32) -> usize {
        if rule_monitor >= 0 {
            return (rule_monitor as usize).min(self.monitors.len().saturating_sub(1));
        }
        self.sel_mon
    }

    /// Mirror C dwl `client_is_float_type()`: fixed-size windows (min==max on
    /// either axis, both non-zero) are intrinsically floating.  Checks both
    /// current and pending state so apps that send constraints before their
    /// first commit (e.g. ripdrag/dragon) are also caught.
    pub(crate) fn window_is_float_type(window: &Window) -> bool {
        // XWayland: popups/menus and fixed-size (min==max) windows float.
        #[cfg(feature = "xwayland")]
        if let Some(x) = window.x11_surface() {
            use smithay::xwayland::xwm::WmWindowType;
            if x.is_popup() {
                return true;
            }
            // Float typed auxiliary windows — dialogs, utility panels, splash
            // screens, toolbars, menus, tooltips.  Steam's login/splash popups are
            // typed this way; without this they get tiled (wrong size + position)
            // and some clients fight the resize, which manifests as a flickering
            // title.  Only truly document-like `Normal`/`Desktop`/`Dock` tile.
            if let Some(t) = x.window_type()
                && matches!(
                    t,
                    WmWindowType::Dialog
                        | WmWindowType::Utility
                        | WmWindowType::Splash
                        | WmWindowType::Toolbar
                        | WmWindowType::Menu
                        | WmWindowType::DropdownMenu
                        | WmWindowType::PopupMenu
                        | WmWindowType::Tooltip
                        | WmWindowType::Notification
                        | WmWindowType::Combo
                )
            {
                return true;
            }
            if let (Some(min), Some(max)) = (x.min_size(), x.max_size()) {
                return min.w != 0 && min.h != 0 && (min.w == max.w || min.h == max.h);
            }
            return false;
        }
        window.toplevel().is_some_and(|t| {
            with_states(t.wl_surface(), |states| {
                let mut guard = states.cached_state.get::<SurfaceCachedState>();
                let (cmin, cmax) = { let s = guard.current(); (s.min_size, s.max_size) };
                let (pmin, pmax) = { let s = guard.pending(); (s.min_size, s.max_size) };
                drop(guard);
                let fixed = |min: smithay::utils::Size<i32, smithay::utils::Logical>,
                              max: smithay::utils::Size<i32, smithay::utils::Logical>| {
                    min.w != 0 && min.h != 0 && (min.w == max.w || min.h == max.h)
                };
                fixed(cmin, cmax) || fixed(pmin, pmax)
            })
        })
    }

    /// Apply window rules to a newly mapped window.
    #[allow(clippy::too_many_lines)]
    pub fn apply_rules(&mut self, window: &Window) {
        // Mirror C dwl mapnotify: if the toplevel declares a parent (dialog /
        // transient window) skip config rules entirely — always floating,
        // always centred on the parent's monitor, always on the parent's tags.
        let parent_wl = window.toplevel().and_then(smithay::wayland::shell::xdg::ToplevelSurface::parent);
        if let Some(ref parent_surf) = parent_wl {
            let parent_data = self
                .window_map.get(parent_surf)
                .and_then(|pw| with_state(pw, |s| (s.mon_idx, s.tags)));
            let (parent_mon, parent_tags) = parent_data.unwrap_or_else(|| {
                let tags = self.monitors.get(self.sel_mon).map_or(1, super::monitor::Monitor::tags);
                (self.sel_mon, tags)
            });
            with_state_mut(window, |s| {
                s.tags = parent_tags;
                s.is_floating = true;
                s.needs_centering = true;
                s.mon_idx = parent_mon;
            });
            return;
        }

        let (appid, title, dialog_hint): (String, String, ToplevelDialogHint) = window
            .toplevel()
            .and_then(|t| {
                with_states(t.wl_surface(), |states| {
                    states
                        .data_map
                        .get::<XdgToplevelSurfaceData>()
                        .and_then(|data| {
                            data.lock().ok().map(|guard| (
                                guard.app_id.clone().unwrap_or_default(),
                                guard.title.clone().unwrap_or_default(),
                                guard.dialog_hint,
                            ))
                        })
                })
            })
            .or_else(|| {
                // XWayland windows have no xdg toplevel: match rules against the
                // X11 WM_CLASS (app id) and WM_NAME (title) instead.
                #[cfg(feature = "xwayland")]
                {
                    // Transient (parented) X11 windows are dialogs; float them.
                    window.x11_surface().map(|x| {
                        let dialog = if x.is_transient_for().is_some() {
                            ToplevelDialogHint::Dialog
                        } else {
                            ToplevelDialogHint::Unknown
                        };
                        (x.class(), x.title(), dialog)
                    })
                }
                #[cfg(not(feature = "xwayland"))]
                None
            })
            .unwrap_or_default();

        // xdg_dialog_v1: client declared itself as a dialog or modal dialog.
        // Mirrors C dwl XWayland DIALOG/modal detection for pure-Wayland apps.
        let is_dialog = matches!(dialog_hint, ToplevelDialogHint::Dialog | ToplevelDialogHint::Modal);

        let cfg = crate::config::get();
        let valid_tags = crate::config::tag_mask();
        // Fixed-size windows are intrinsically floating (mirrors C dwl client_is_float_type).
        let RuleAccum {
            tags: new_tags,
            is_floating,
            scratch_key,
            monitor: rule_monitor,
            switch_to_tag: switch_to_tag_mask,
        } = cfg.rules.iter()
            .filter(|r| {
                r.id.as_deref().is_none_or(|id| appid.contains(id))
                    && r.title.as_deref().is_none_or(|t| title.contains(t))
            })
            .fold(
                RuleAccum {
                    tags:         0,
                    // OR the current flag so a float decision already made at map
                    // time (XWayland fixed-size/typed windows whose hints have since
                    // changed — see handlers::xwayland::map_x11_window) is preserved.
                    is_floating:  Self::window_is_float_type(window) || is_dialog
                        || with_state(window, |s| s.is_floating).unwrap_or(false),
                    scratch_key:  '\0',
                    monitor:      -1,
                    switch_to_tag: None,
                },
                |acc, r| RuleAccum {
                    tags:         acc.tags | r.tags,
                    is_floating:  acc.is_floating | r.is_floating,
                    scratch_key:  if r.scratch_key == '\0' { acc.scratch_key } else { r.scratch_key },
                    monitor:      if r.monitor >= 0 { r.monitor } else { acc.monitor },
                    switch_to_tag: if r.switch_to_tag && r.tags != 0 { Some(r.tags & valid_tags) } else { acc.switch_to_tag },
                },
            );
        drop(cfg);

        let mon_idx = self.target_monitor_for_window(rule_monitor);

        let effective_tags = if new_tags != 0 {
            new_tags & valid_tags
        } else if let Some(m) = self.monitors.get(mon_idx) {
            m.tags()
        } else {
            1
        };

        // A window that is about to be swallowed replaces its terminal in place,
        // so it must not trigger the rule's switch_to_tag view jump — the view
        // must stay on the terminal's tag, not follow the (overridden) rule tag.
        #[cfg(feature = "swallow")]
        let will_swallow =
            crate::features::swallow::find_target(self, window, &appid, &title).is_some();
        #[cfg(not(feature = "swallow"))]
        let will_swallow = false;

        if let Some(tag_mask) = switch_to_tag_mask
            && !will_swallow
        {
            self.update_tag_history(mon_idx, tag_mask);
            if let Some(m) = self.monitors.get_mut(mon_idx) {
                let prev = m.tags();
                let effective = tag_mask & valid_tags;
                if prev != effective {
                    // Only record switch_to_tag when we actually jump to a different tag.
                    // If already on the target tag, leave switch_to_tag = None so close
                    // falls through to the normal history-based fallback.
                    #[cfg(feature = "tag-transition")]
                    let output_w = f64::from(m.w.size.w);
                    #[cfg(feature = "tag-transition")]
                    crate::features::tag_transition::mark_slide_outs(
                        &self.windows, prev, effective, output_w,
                    );
                    m.view(tag_mask);
                    with_state_mut(window, |s| s.switch_to_tag = Some(prev));
                    #[cfg(feature = "tag-transition")]
                    { self.pending_tag_transition = Some((prev, effective, output_w)); }
                }
            }
        }

        // XWayland windows place themselves (honoured in configure_request, as dwl
        // does) — so don't run rwl's auto-centring for them; it would override the
        // client's own position.  Wayland floating windows still get centred.
        #[cfg(feature = "xwayland")]
        let center = is_floating && window.x11_surface().is_none();
        #[cfg(not(feature = "xwayland"))]
        let center = is_floating;

        with_state_mut(window, |s| {
            s.tags = effective_tags;
            s.is_floating = is_floating;
            s.needs_centering = center;
            s.scratch_key = scratch_key;
            s.mon_idx = mon_idx;
            // Seed the IPC cache so format_status() can avoid re-locking
            // Wayland surface state on every print.
            if s.last_appid.is_none() {
                s.last_appid = Some(appid);
            }
            if s.last_title.is_none() {
                s.last_title = Some(title);
            }
        });
    }

    /// Bring `window` to the top of the focus stack and give it keyboard focus.
    pub fn focus_window(&mut self, window: Option<Window>) {
        let surface = window
            .as_ref()
            .and_then(|w| w.wl_surface().map(std::borrow::Cow::into_owned));

        let prev_focused = self.focus_stack.first().cloned();

        if let Some(w) = window {
            crate::window::set_urgent(&w, false);
            // Common case: w is already in the stack. Rotating the prefix
            // [0..=pos] right by 1 brings w to the front in O(pos) moves,
            // avoiding the two O(n) passes of retain() + insert(0).
            if let Some(pos) = self.focus_stack.iter().position(|x| x == &w) {
                self.focus_stack[0..=pos].rotate_right(1);
            } else {
                self.focus_stack.insert(0, w);
            }
        }

        // When the session is locked the lock surface owns keyboard focus; don't
        // let any window mapping or focus change steal it.
        if self.locked {
            return;
        }

        if let Some(kb) = self.keyboard.clone() {
            let serial = SERIAL_COUNTER.next_serial();
            kb.set_focus(self, surface, serial);
        }
    
        self.update_borders();
    
        if self.focus_stack.first() != prev_focused.as_ref() {
            crate::ipc::print_status(self);

            // Fire on_focus for the newly focused window (deduplicated inside).
            #[cfg(feature = "hooks")]
            if let Some(w) = self.focus_stack.first().cloned() {
                crate::features::hooks::focus(self, &w);
            }

            #[cfg(feature = "ipc")]
            {
                let w = self.focus_stack.first().cloned();
                crate::features::ipc::event::focus(self, w.as_ref());
            }

            let new_mon = self.focus_stack.first().and_then(|w| with_state(w, |s| s.mon_idx));
            let prev_mon = prev_focused.as_ref().and_then(|w| with_state(w, |s| s.mon_idx));

            if let Some(mon_idx) = new_mon {
                // Mirror C dwl: selmon always tracks the focused client's monitor so
                // that keyboard-driven actions (View, SetLayout, …) target the right
                // output without requiring the pointer to move there first.
                self.sel_mon = mon_idx;
                self.arrange(mon_idx);
                // If the previously focused window was on a different monitor, arrange that
                // monitor too so its windows receive a cleared XDG Activated state.
                if let Some(prev_idx) = prev_mon
                    && prev_idx != mon_idx
                {
                    self.arrange(prev_idx);
                }
                // arrange() does not call schedule_render(); ensure a frame fires
                // so the updated border colours become visible even if no client
                // commits a new buffer (e.g. keyboard focus change with no resize).
                self.schedule_render();
            } else {
                self.arrange_all();
            }
        }
    }

    /// Update border colours on all mapped windows.
    pub fn update_borders(&self) {
        let focused = self.focused_window().cloned();
        let cfg = crate::config::get();
        let focus_color = cfg.focus_color;
        let border_color = cfg.border_color;
        drop(cfg);
        for w in &self.windows {
            let color = if Some(w) == focused.as_ref() { focus_color } else { border_color };
            with_state_mut(w, |s| {
                s.border_color = color;
                s.border_counter.increment();
            });
        }
    }

    /// Arrange all windows on `mon_idx` according to the active layout.
    #[allow(clippy::too_many_lines)]
    pub fn arrange(&mut self, mon_idx: usize) {
        use crate::features::layout;

        let (tags, default_loc, fs_geom) = {
            let Some(mon) = self.monitors.get(mon_idx) else {
                return;
            };
            (mon.tags(), mon.w.loc, mon.m)
            // mon borrow ends here
        };

        // Count-based per-tag layout: on a single-tag view, pick the layout for
        // the current tiled-window count.  Runs here (before the layout is read)
        // so it reacts to every open/close, not just tag switches.
        #[cfg(feature = "pertag-layouts")]
        if tags.is_power_of_two() {
            let count = self.count_tiled(tags);
            if let Some(mon) = self.monitors.get_mut(mon_idx) {
                crate::features::pertag_layouts::apply_dynamic(mon, tags, count);
            }
        }

        // Union of every monitor's visible tagset.  A window is only unmapped
        // from the space when no monitor shows its tag; otherwise it stays
        // mapped (potentially at another monitor's position).
        let all_visible: u32 = self.monitors.iter().map(super::monitor::Monitor::tags).fold(0, |a, b| a | b);

        // Single pass: partition all windows into their display categories for
        // this monitor.  Avoids re-scanning self.windows four separate times.
        // Take pre-allocated scratch Vecs (always returned empty with capacity retained).
        let mut hidden       = std::mem::take(&mut self.arrange_hidden);
        let mut tiled        = std::mem::take(&mut self.arrange_tiled);
        let mut floating     = std::mem::take(&mut self.arrange_floating);
        let mut fs_windows   = std::mem::take(&mut self.arrange_fs);
        #[cfg(feature = "scratchpad")]
        let mut scratch_windows = std::mem::take(&mut self.arrange_scratch);
        for (idx, w) in self.windows.iter().enumerate() {
            // A terminal hidden behind the child that swallowed it: keep it out of
            // the layout and unmapped from the space until the child closes.
            #[cfg(feature = "swallow")]
            if with_state(w, |s| s.is_swallowed).unwrap_or(false) {
                hidden.push(idx);
                continue;
            }
            let Some((w_tags, is_fs, is_float, rules_done, scratch_key)) =
                with_state(w, |s| (s.tags, s.is_fullscreen, s.is_floating, s.rules_applied, s.scratch_key))
            else {
                continue;
            };
            if w_tags & all_visible == 0 {
                hidden.push(idx);
            } else if w_tags & tags != 0 {
                if is_fs {
                    fs_windows.push(idx);
                } else if is_float {
                    #[cfg(feature = "scratchpad")]
                    if scratch_key != '\0' {
                        scratch_windows.push(idx);
                    }
                    let _ = scratch_key; // suppress unused warning when scratchpad is off
                    floating.push(idx);
                } else if rules_done {
                    // Exclude windows whose rules haven't been applied yet: their
                    // final tag / floating / monitor assignments are unknown, so
                    // counting them would produce wrong tile geometries for the
                    // other windows.  They get a proper configure once commit()
                    // fires and apply_rules() runs.
                    tiled.push(idx);
                }
            }
        }

        // Before unmapping, save the space-position of any floating window so
        // it can be restored when its tag becomes active again.
        for &idx in &hidden {
            let w = &self.windows[idx];
            // Keep slide-out windows in the space during their animation;
            // finalize_slide_outs() unmaps them once the animation finishes.
            #[cfg(feature = "tag-transition")]
            if with_state(w, |s| s.slide_out && s.slide_start.is_some()).unwrap_or(false) {
                continue;
            }
            // Save the position for any individually-floating window so it
            // reappears where it was when its tag returns.  Use the per-window
            // signal rather than the monitor's current layout: with
            // pertag-layouts the monitor may already show a different tag.
            if window_is_floating(w)
                && let Some(geom) = self.space.element_geometry(w)
            {
                with_state_mut(w, |s| s.saved_loc = Some(geom.loc));
            }
            self.space.unmap_elem(w);
            // Strip Activated (and tiled) states so clients that derive their
            // "focused" status from xdg_toplevel state (e.g. mpv) see the
            // focus change when their tag is hidden.
            if let Some(tl) = w.toplevel() {
                tl.with_pending_state(|state| {
                    state.states.unset(xdg_toplevel::State::Activated);
                    state.states.unset(xdg_toplevel::State::TiledLeft);
                    state.states.unset(xdg_toplevel::State::TiledRight);
                    state.states.unset(xdg_toplevel::State::TiledTop);
                    state.states.unset(xdg_toplevel::State::TiledBottom);
                });
                // Use send_configure() instead of send_pending_configure() so
                // initial_configure_sent is guaranteed to be set even when all
                // the unset() calls produced no actual state delta (e.g. a
                // freshly created scratchpad whose states were never set).
                // send_pending_configure() is a no-op in that case, leaving
                // initial_configure_sent=false and causing the commit() safety
                // net to call arrange_all() on every subsequent client commit.
                tl.send_configure();
            }
        }

        // Track which window has keyboard focus so we can set the Activated
        // state correctly on every configure we send.
        //
        // Use the monitor-local focused window (first visible on this monitor's
        // current tagset) rather than focus_stack.first() (which is the globally
        // last-focused window and may belong to a completely different tag).
        //
        // Without this, switching FROM a tag with windows TO Firefox's tag would
        // have focus_stack.first() pointing to a window on the OTHER tag.  The
        // first arrange() after the switch would send Firefox Tiled* but no
        // Activated.  The follow-up arrange() from focus_window() would then
        // call send_pending_configure() with {Tiled* + Activated}, but that
        // matches what Firefox last acked (its pre-switch state), making it a
        // no-op.  Firefox never receives the correct configure and stays in its
        // floating CSD layout (tabs shifted left).
        let focused = self.focus_stack.iter()
            .find(|w| with_state(w, |s| s.tags & tags != 0 && s.mon_idx == mon_idx).unwrap_or(false))
            .cloned();

        // Keep scroll_col in sync with the focused window so the scrollable
        // layouts (monocle and scroll) can centre it.  Must happen before
        // layout::arrange borrows the monitor.  Only relevant when a scrollable
        // layout is compiled in.
        #[cfg(any(feature = "monocle", feature = "scroll"))]
        {
            use crate::config::LayoutKind;
            let layout_kind = self.monitors.get(mon_idx).map(super::monitor::Monitor::layout_kind);
            #[cfg(feature = "monocle")]
            let is_scrollable = matches!(layout_kind, Some(LayoutKind::Monocle));
            #[cfg(not(feature = "monocle"))]
            let is_scrollable = false;
            #[cfg(feature = "scroll")]
            let is_scrollable = is_scrollable || matches!(layout_kind, Some(LayoutKind::Scroll));
            if is_scrollable {
                let n = tiled.len();
                let focus_idx = tiled
                    .iter()
                    .position(|&idx| focused.as_ref() == Some(&self.windows[idx]))
                    .unwrap_or_else(|| {
                        self.monitors[mon_idx].scroll_col.min(n.saturating_sub(1))
                    });
                self.monitors[mon_idx].scroll_col = focus_idx;
            }
        }

        // Per-window size factors in tiling order; layouts that stack windows
        // in a shared column/row weight each window's span by its factor.
        let cfacts: Vec<f64> = tiled
            .iter()
            .map(|&idx| with_state(&self.windows[idx], |s| s.cfact).unwrap_or(1.0))
            .collect();
        let geoms = layout::arrange(&self.monitors[mon_idx], &cfacts);

        #[cfg(feature = "monocle")]
        let is_monocle_layout =
            self.monitors[mon_idx].layout_kind() == crate::config::LayoutKind::Monocle;
        #[cfg(not(feature = "monocle"))]
        let is_monocle_layout = false;

        for (&idx, geom) in tiled.iter().zip(geoms.iter()) {
            let w = &self.windows[idx];
            let is_focused = focused.as_ref() == Some(w);
            // A None element_geometry means the window was unmapped (its tag was hidden).
            // For such windows send_configure() must be used unconditionally: if Firefox
            // hasn't yet acked the "hide" configure when we switch back, Smithay's
            // with_pending_state initialises server_pending from `current` (last *acked*
            // state), which still contains Tiled+Activated. The resulting pending state
            // then matches the last acked state, so send_pending_configure() is a no-op
            // and Firefox never receives the re-tiled configure.
            let old_space_geom = self.space.element_geometry(w);
            let was_unmapped = old_space_geom.is_none();

            if was_unmapped {
                // New or re-appearing: must use map_element to add to the space.
                self.space.map_element(w.clone(), geom.loc, false);
            } else if old_space_geom.is_none_or(|g| g.loc != geom.loc) {
                // Position changed (regardless of size): relocate_element keeps the
                // space location in sync so borders and focus color appear at the
                // correct position immediately, even before the client commits a
                // resized buffer.  relocate_element avoids remove+reinsert and the
                // associated output_enter damage marking that map_element causes.
                self.space.relocate_element(w, geom.loc);
            } else if is_focused && is_monocle_layout {
                // Monocle: all windows share the same position, so relocate_element
                // is a no-op and the focused window would stay buried.  Re-insert it
                // via map_element to bring it to the top of the Space's render order
                // (equal z_index; last-mapped wins).
                self.space.map_element(w.clone(), geom.loc, false);
            }
            // Size-only change: space.element_geometry() reads w.geometry().size live,
            // so when the client commits the new buffer commit() merely bumps
            // border_counter to trigger a redraw at the correct new geometry.

            let (geom_changed, focus_changed) = with_state(w, |s| {
                (s.pending_geom != Some(*geom), s.last_focused != is_focused)
            }).unwrap_or((true, true));
            with_state_mut(w, |s| {
                s.pending_geom = Some(*geom);
                s.last_focused = is_focused;
                if geom_changed || focus_changed || was_unmapped {
                    s.border_counter.increment();
                }
            });

            if let Some(tl) = w.toplevel() {
                tl.with_pending_state(|state| {
                    state.size = Some(geom.size);
                    state.states.set(xdg_toplevel::State::TiledLeft);
                    state.states.set(xdg_toplevel::State::TiledRight);
                    state.states.set(xdg_toplevel::State::TiledTop);
                    state.states.set(xdg_toplevel::State::TiledBottom);
                    state.states.unset(xdg_toplevel::State::Fullscreen);

                    // This ensures state changes accurately when switching focus side-by-side
                    if is_focused {
                        state.states.set(xdg_toplevel::State::Activated);
                    } else {
                        state.states.unset(xdg_toplevel::State::Activated);
                    }
                });
                if was_unmapped {
                    tl.send_configure();
                } else {
                    tl.send_pending_configure();
                }
            }
            // XWayland tiled window: position + size it absolutely and mark focus.
            // Each X11 setter writes to the socket + flushes unconditionally, so
            // only call them on an actual transition (mirrors the send_pending_configure
            // no-op the Wayland path above relies on).
            #[cfg(feature = "xwayland")]
            if let Some(x11) = w.x11_surface() {
                if x11.is_fullscreen() {
                    let _ = x11.set_fullscreen(false);
                }
                if geom_changed || was_unmapped {
                    let _ = x11.configure(*geom);
                }
                if focus_changed || was_unmapped {
                    let _ = x11.set_activated(is_focused);
                }
            }
        }

        // Floating visible windows keep their last position; re-map them if
        // they were unmapped (i.e. their tag was previously hidden).
        for &idx in &floating {
            let w = &self.windows[idx];
            // Always re-map floating windows after tiled ones so they sit on
            // top in smithay's Space render order (equal z_index, last-mapped
            // wins).  Preserve the current position when already mapped.
            // Scratchpad windows skip map_element here; the scratch loop below
            // re-maps them last so they render above fullscreen windows.
            let geom = self.space.element_geometry(w);
            let was_unmapped = geom.is_none();
            let loc = geom
                .map(|g| g.loc)
                .or_else(|| with_state(w, |s| s.saved_loc).flatten())
                .unwrap_or(default_loc);
            #[cfg(feature = "scratchpad")]
            let skip_map = scratch_windows.contains(&idx);
            #[cfg(not(feature = "scratchpad"))]
            let skip_map = false;
            if !skip_map {
                self.space.map_element(w.clone(), loc, false);
            }
            with_state_mut(w, |s| {
                s.pending_geom = None;
                s.border_counter.increment();
            });
            // Mirror C dwl commitnotify: send a configure to every visible
            // floating window so the client gets the right XDG states (no
            // Tiled*, correct Activated) and size=None (client picks its own).
            // This is essential for scratchpads that may have received a tiled
            // configure before rules were applied.
            //
            let is_focused = focused.as_ref() == Some(w);
            if let Some(tl) = w.toplevel() {
                tl.with_pending_state(|state| {
                    // Every window in this loop is individually floating, so let
                    // the client pick its own size (no tiled size constraint).
                    state.size = None;
                    state.states.unset(xdg_toplevel::State::TiledLeft);
                    state.states.unset(xdg_toplevel::State::TiledRight);
                    state.states.unset(xdg_toplevel::State::TiledTop);
                    state.states.unset(xdg_toplevel::State::TiledBottom);
                    state.states.unset(xdg_toplevel::State::Fullscreen);
                    if is_focused {
                        state.states.set(xdg_toplevel::State::Activated);
                    } else {
                        state.states.unset(xdg_toplevel::State::Activated);
                    }
                });
                if was_unmapped {
                    tl.send_configure();
                } else {
                    tl.send_pending_configure();
                }
            }
            // XWayland floating window: keep its own size, just place it at loc.
            // Guard each X11 write (socket + flush) so a re-arrange that didn't
            // move or refocus this window emits no X traffic.
            #[cfg(feature = "xwayland")]
            if let Some(x11) = w.x11_surface() {
                let focus_changed = with_state(w, |s| s.last_focused != is_focused).unwrap_or(true);
                if focus_changed {
                    with_state_mut(w, |s| s.last_focused = is_focused);
                }
                if x11.is_fullscreen() {
                    let _ = x11.set_fullscreen(false);
                }
                if was_unmapped || x11.geometry().loc != loc {
                    let _ = x11.configure(Rectangle::new(loc, x11.geometry().size));
                }
                if focus_changed || was_unmapped {
                    let _ = x11.set_activated(is_focused);
                }
            }
        }

        for &idx in &fs_windows {
            let w = &self.windows[idx];
            let is_focused = focused.as_ref() == Some(w);
            let was_unmapped = self.space.element_geometry(w).is_none();
            self.space.map_element(w.clone(), fs_geom.loc, false);
            with_state_mut(w, |s| {
                s.pending_geom = Some(fs_geom);
                s.border_counter.increment();
            });
            if let Some(tl) = w.toplevel() {
                tl.with_pending_state(|state| {
                    state.size = Some(fs_geom.size);
                    state.states.set(xdg_toplevel::State::Fullscreen);
                    // Fullscreen windows have no tiled constraints
                    state.states.unset(xdg_toplevel::State::TiledLeft);
                    state.states.unset(xdg_toplevel::State::TiledRight);
                    state.states.unset(xdg_toplevel::State::TiledTop);
                    state.states.unset(xdg_toplevel::State::TiledBottom);
                    if is_focused {
                        state.states.set(xdg_toplevel::State::Activated);
                    } else {
                        state.states.unset(xdg_toplevel::State::Activated);
                    }
                });
                if was_unmapped {
                    tl.send_configure();
                } else {
                    tl.send_pending_configure();
                }
            }
            // XWayland fullscreen window: size it to the whole output.  Guard each
            // X11 write so re-arranging a still-fullscreen window emits no X traffic.
            #[cfg(feature = "xwayland")]
            if let Some(x11) = w.x11_surface() {
                let focus_changed = with_state(w, |s| s.last_focused != is_focused).unwrap_or(true);
                if focus_changed {
                    with_state_mut(w, |s| s.last_focused = is_focused);
                }
                if !x11.is_fullscreen() {
                    let _ = x11.set_fullscreen(true);
                }
                if was_unmapped || x11.geometry() != fs_geom {
                    let _ = x11.configure(fs_geom);
                }
                if focus_changed || was_unmapped {
                    let _ = x11.set_activated(is_focused);
                }
            }
        }

        // Re-map visible scratchpad windows after fullscreen windows so they
        // appear on top (smithay Space: equal z_index, last-mapped wins).
        // The floating loop above skipped map_element for these windows, so
        // this is the sole placement call; use the same loc resolution logic
        // (element_geometry → saved_loc → default_loc).
        #[cfg(feature = "scratchpad")]
        for &idx in &scratch_windows {
            let w = &self.windows[idx];
            let loc = self.space.element_geometry(w)
                .map(|g| g.loc)
                .or_else(|| with_state(w, |s| s.saved_loc).flatten())
                .unwrap_or(default_loc);
            self.space.map_element(w.clone(), loc, false);
        }

        // Return scratch Vecs: clear Arc refs, preserve allocated capacity.
        hidden.clear();
        tiled.clear();
        floating.clear();
        fs_windows.clear();
        #[cfg(feature = "scratchpad")]
        scratch_windows.clear();
        self.arrange_hidden   = hidden;
        self.arrange_tiled    = tiled;
        self.arrange_floating = floating;
        self.arrange_fs       = fs_windows;
        #[cfg(feature = "scratchpad")]
        { self.arrange_scratch = scratch_windows; }
    }

    /// Returns true if a newly-mapped tiled window on `output` has not yet
    /// committed its first buffer.  Used by the render gate to hold the previous
    /// frame while the new window's content is being drawn, preventing a blank
    /// wallpaper flash in the master/slave area.
    ///
    /// Only new (`buffer_mapped=false`) windows trigger the long gate.
    pub fn any_tiled_pending_resize(&self, output: &smithay::output::Output) -> bool {
        let tags = self
            .monitors
            .iter()
            .find(|m| &m.output == output)
            .map_or(0, Monitor::tags);

        self.windows.iter().any(|w| {
            with_state(w, |s| {
                if s.is_floating || s.is_fullscreen { return false; }
                if s.tags & tags == 0 { return false; }
                if s.buffer_mapped { return false; }
                let Some(pending) = s.pending_geom else { return false };
                w.geometry().size != pending.size
            })
            .unwrap_or(false)
        })
    }

    /// True when any already-buffered tiled window hasn't yet committed a
    /// buffer at its new configured size.  Covers the close-window case where
    /// all neighbours need to grow/shrink — they have content but the wrong size.
    pub fn any_existing_tiled_pending_resize(&self, output: &smithay::output::Output) -> bool {
        let tags = self
            .monitors
            .iter()
            .find(|m| &m.output == output)
            .map_or(0, Monitor::tags);

        self.windows.iter().any(|w| {
            with_state(w, |s| {
                if s.is_floating || s.is_fullscreen { return false; }
                if s.tags & tags == 0 { return false; }
                if !s.buffer_mapped { return false; }
                let Some(pending) = s.pending_geom else { return false };
                w.geometry().size != pending.size
            })
            .unwrap_or(false)
        })
    }

    pub fn arrange_all(&mut self) {
        self.update_work_areas();
        #[cfg(feature = "tag-transition")]
        let pending_transition = self.pending_tag_transition.take();
        for i in 0..self.monitors.len() {
            self.arrange(i);
        }
        #[cfg(feature = "tag-transition")]
        if let Some((old, new, ow)) = pending_transition {
            crate::features::tag_transition::start_tag_transition(&self.windows, old, new, ow);
        }
        self.schedule_render();
        crate::ipc::print_status(self);
    }

    /// Unmap slide-out windows whose animation has finished (`slide_start` cleared
    /// by `advance_tag_transitions`).  Called each render frame after advancing
    /// transitions.  No-op when the `tag-transition` feature is disabled.
    #[cfg(feature = "tag-transition")]
    pub fn finalize_slide_outs(&mut self) {
        // Union of every monitor's visible tagset.  A slide-out window must, by
        // definition, be leaving the visible tags — so if a window flagged
        // slide_out is still on a visible tag, the flag is stale/wrong (e.g. a
        // freshly-opened window on the destination tag that got mis-marked).
        // Unmapping it would hide a window that must stay on screen, so clear
        // the flag and skip the unmap instead.
        let all_visible: u32 = self
            .monitors
            .iter()
            .map(super::monitor::Monitor::tags)
            .fold(0, |a, b| a | b);

        let to_finalize: Vec<smithay::desktop::Window> = self.windows.iter()
            .filter(|w| with_state(w, |s| s.slide_out && s.slide_start.is_none()).unwrap_or(false))
            .cloned()
            .collect();
        for w in to_finalize {
            // Safety invariant: never unmap a window that is on a visible tag.
            let visible = with_state(&w, |s| s.tags & all_visible != 0).unwrap_or(false);
            if visible {
                with_state_mut(&w, |s| {
                    s.slide_out = false;
                    s.slide_offset_x = 0.0;
                });
                continue;
            }
            // Save the position for any individually-floating window so it
            // reappears where it was when its tag returns.  Must NOT be gated on
            // the monitor's current layout: with pertag-layouts the monitor has
            // already switched to the destination tag's layout by the time this
            // runs.  Use the per-window `is_floating` signal instead.
            if window_is_floating(&w)
                && let Some(geom) = self.space.element_geometry(&w)
            {
                with_state_mut(&w, |s| s.saved_loc = Some(geom.loc));
            }
            self.space.unmap_elem(&w);
            with_state_mut(&w, |s| {
                s.slide_out = false;
                s.slide_offset_x = 0.0;
            });
            if let Some(tl) = w.toplevel() {
                tl.with_pending_state(|state| {
                    state.states.unset(xdg_toplevel::State::Activated);
                    state.states.unset(xdg_toplevel::State::TiledLeft);
                    state.states.unset(xdg_toplevel::State::TiledRight);
                    state.states.unset(xdg_toplevel::State::TiledTop);
                    state.states.unset(xdg_toplevel::State::TiledBottom);
                });
                tl.send_configure();
            }
        }
    }

    /// Mirror C dwl's `arrangelayers` end-of-function logic: after any layer
    /// surface change, give keyboard focus to the topmost keyboard-interactive
    /// Overlay/Top layer that has a committed buffer.  If none is found,
    /// restore keyboard focus to the top window on the focus stack instead.
    /// No-ops when the session is locked — the lock surface owns keyboard focus.
    pub fn update_layer_focus(&mut self) {
        if self.locked {
            return;
        }
        // Collect the surface to focus before borrowing self mutably.
        let layer_surface = self.monitors.iter().find_map(|mon| {
            let map = layer_map_for_output(&mon.output);
            // Overlay first, then Top — matches layers_above_shell[] in C dwl.
            for layer in [WlrLayer::Overlay, WlrLayer::Top] {
                if let Some(l) = map
                    .layers_on(layer)
                    .rev() // topmost first
                    .find(|l| {
                        l.can_receive_keyboard_focus()
                            && l.geometry().size.w > 0 // has a committed buffer
                    })
                {
                    return Some(l.wl_surface().clone());
                }
            }
            None
        });

        let kb = self.keyboard.clone();
        if let Some(surface) = layer_surface {
            if let Some(kb) = kb {
                kb.set_focus(self, Some(surface), SERIAL_COUNTER.next_serial());
            }
        } else {
            // No interactive layer — restore focus to the topmost window that is
            // visible on the current tagset.  focus_stack.first() is the globally
            // most-recently-focused window and may be on a hidden tag; focused_window()
            // filters by sel_monitor's active tagset and is always the right choice here.
            let top = self
                .focused_window()
                .and_then(|w| w.wl_surface().map(std::borrow::Cow::into_owned));
            if let Some(kb) = kb {
                kb.set_focus(self, top, SERIAL_COUNTER.next_serial());
            }
        }
    }

    /// Recompute each monitor's usable window area (`mon.w`) from the
    /// layer-shell exclusive zones currently in effect on that output.
    ///
    /// Must be called before any layout pass so that bars/panels that request
    /// an exclusive zone actually shrink the tiling area.
    pub fn update_work_areas(&mut self) {
        for mon in &mut self.monitors {
            // The inner block ensures the LayerMap guard is dropped (releasing
            // the borrow on mon.output) before we assign to mon.w.
            let (zone, has_exclusive_layers) = {
                let mut map = layer_map_for_output(&mon.output);
                map.arrange();
                let z = map.non_exclusive_zone();
                // Only guard against transient states when there are layers that
                // actually claim an exclusive zone (bars, panels).  Non-exclusive
                // layers such as wallpapers have ExclusiveZone::Neutral and must
                // never block the work-area from expanding when a bar destroys
                // itself (hide_bar calls ls.destroy(), removing it from the map).
                let has_excl = map.layers().any(|l| {
                    matches!(l.cached_state().exclusive_zone, ExclusiveZone::Exclusive(_))
                });
                drop(map);
                (z, has_excl)
            };
            // non_exclusive_zone() is output-local (origin = output top-left).
            // Translate to global logical space by adding the output's position.
            let new_w = if zone.size.w > 0 && zone.size.h > 0 {
                smithay::utils::Rectangle::new(
                    (mon.m.loc.x + zone.loc.x, mon.m.loc.y + zone.loc.y).into(),
                    zone.size,
                )
            } else {
                mon.m
            };
            // Guard against a transient smithay state where the bar's cached
            // LayerSurfaceCachedState briefly reverts to default between commits,
            // causing map.arrange() to return a full-output zone while the bar is
            // still mapped.  Only skip when there are layers with an actual
            // exclusive zone; wallpapers (Neutral zone) must not block the update.
            if has_exclusive_layers && new_w == mon.m && mon.w != mon.m {
                continue;
            }
            mon.w = new_w;
        }
    }

    // -----------------------------------------------------------------------
    // Cursor hiding
    // -----------------------------------------------------------------------

    /// Called on every pointer event.  Shows the cursor if it was hidden and
    /// resets the inactivity timer that will hide it again after
    /// `CURSOR_TIMEOUT_SECS` seconds.
    /// Report user activity to the idle-notify subsystem.
    ///
    /// Called from every input path so idle timeouts reset on any user action.
    pub fn notify_idle_activity(&mut self) {
        self.idle_notifier.notify_activity(&self.seat);
    }

    pub fn handle_cursor_activity(&mut self) {
        self.notify_idle_activity();
        if self.cursor_hidden {
            self.cursor_hidden = false;
            // Cursor just became visible — render immediately so the host
            // cursor appears without waiting for the next event-driven frame.
            self.schedule_render();
        }

        // Cancel any pending timer and start a fresh one.
        if let Some(token) = self.cursor_hide_timer.take() {
            self.loop_handle.remove(token);
        }

        self.cursor_hide_timer = self
            .loop_handle
            .insert_source(
                Timer::from_duration(std::time::Duration::from_secs(
                    crate::config::get().cursor_timeout_secs,
                )),
                |_, (), state| {
                    state.cursor_hidden = true;
                    state.cursor_hide_timer = None;
                    state.schedule_render();
                    TimeoutAction::Drop
                },
            )
            .ok();
    }

    #[must_use]
    pub const fn dir_to_monitor(&self, from: usize, dir: i32) -> Option<usize> {
        if self.monitors.len() < 2 {
            return None;
        }
        let next = if dir > 0 {
            (from + 1) % self.monitors.len()
        } else {
            (from + self.monitors.len() - 1) % self.monitors.len()
        };
        Some(next)
    }

    #[must_use]
    pub fn count_tiled(&self, tags: u32) -> usize {
        self.windows
            .iter()
            .filter(|w| {
                window_visible_on(w, tags)
                    && !window_is_floating(w)
                    && !window_is_fullscreen(w)
                    && with_state(w, |s| s.rules_applied).unwrap_or(false)
            })
            .count()
    }

    #[must_use]
    pub fn next_occ_tag(&self, dir: i32) -> Option<u32> {
        let mon = self.sel_monitor()?;
        let occupied: u32 = self.windows.iter().map(window_tags).fold(0, |a, b| a | b);
        mon.next_occupied_tag(occupied, dir)
    }

    /// Records the current tag mask of a monitor to history before a view change.
    pub(crate) fn update_tag_history(&mut self, mon_idx: usize, target_tags: u32) {
        if let Some(m) = self.monitors.get(mon_idx) {
            let old_tags = m.tags();
            if old_tags != target_tags {
                let history = &mut self.tag_history[mon_idx];
                
                // Keep the chronological stack unique so we don't loop infinitely
                history.retain(|&t| t != old_tags);
                history.push_back(old_tags);
                if history.len() > 16 {
                    history.pop_front();
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // Title / app-id polling
    // -----------------------------------------------------------------------

    /// Scan every toplevel for title or app-id changes that arrived via
    /// `set_title` / `set_app_id` without a subsequent `wl_surface.commit`
    /// (Firefox's tab-switch pattern).  Called from the post-dispatch hook in
    /// main.rs so it runs on every batch of Wayland events at zero idle cost.
    pub fn check_toplevel_titles(&mut self) {
        struct PendingUpdate {
            window:        Window,
            new_title:     Option<String>,
            update_title:  bool,
            new_appid:     Option<String>,
            update_appid:  bool,
        }

        // Phase 1: compute which windows have stale cached metadata (read-only).
        let updates: Vec<_> = self.windows.iter()
            .filter_map(|w| {
                let (title_now, appid_now) = w.toplevel().and_then(|t| {
                    with_states(t.wl_surface(), |states| {
                        states
                            .data_map
                            .get::<XdgToplevelSurfaceData>()
                            .and_then(|d| d.lock().ok().map(|g| (g.title.clone(), g.app_id.clone())))
                    })
                }).or_else(|| {
                    // XWayland: pick up live WM_NAME / WM_CLASS changes too.
                    #[cfg(feature = "xwayland")]
                    { w.x11_surface().map(|x| (Some(x.title()), Some(x.class()))) }
                    #[cfg(not(feature = "xwayland"))]
                    { None }
                }).unwrap_or_default();
                let (title_changed, appid_changed) = with_state(w, |s| (
                    s.last_title.as_deref() != title_now.as_deref(),
                    s.last_appid.as_deref() != appid_now.as_deref(),
                )).unwrap_or((false, false));
                (title_changed || appid_changed).then(|| PendingUpdate {
                    window:       w.clone(),
                    new_title:    title_now,
                    update_title: title_changed,
                    new_appid:    appid_now,
                    update_appid: appid_changed,
                })
            })
            .collect();

        // Phase 2: apply updates then notify IPC.
        if updates.is_empty() {
            return;
        }
        for u in updates {
            with_state_mut(&u.window, |s| {
                if u.update_title { s.last_title.clone_from(&u.new_title); }
                if u.update_appid { s.last_appid.clone_from(&u.new_appid); }
            });
            #[cfg(feature = "hooks")]
            if u.update_title
                && let Some(title) = u.new_title.as_deref()
            {
                crate::features::hooks::title_change(self, &u.window, title);
            }
            #[cfg(feature = "ipc")]
            if u.update_title {
                crate::features::ipc::event::title(self, &u.window);
            }
        }
        crate::ipc::print_status(self);
    }
}

// ---------------------------------------------------------------------------
// Protocol dispatch — one macro handles all protocols via Dispatch2
// ---------------------------------------------------------------------------

impl DrmSyncobjHandler for Rwl {
    fn drm_syncobj_state(&mut self) -> Option<&mut DrmSyncobjState> {
        self.protocol_globals.syncobj_state.as_mut()
    }
}

impl XdgDialogHandler for Rwl {
    fn dialog_hint_changed(&mut self, surface: ToplevelSurface, hint: ToplevelDialogHint) {
        // A client declared (or changed) its dialog hint after the first commit.
        // If it became a dialog/modal and isn't floating yet, mark it floating and
        // re-tile (matches C dwl's client_is_float_type → isfloating path).
        if matches!(hint, ToplevelDialogHint::Dialog | ToplevelDialogHint::Modal) {
            let window = self.window_map.get(surface.wl_surface()).cloned();
            if let Some(w) = window {
                let already_floating = with_state(&w, |s| s.is_floating).unwrap_or(false);
                if !already_floating {
                    with_state_mut(&w, |s| {
                        s.is_floating = true;
                        s.needs_centering = true;
                    });
                    self.arrange_all();
                }
            }
        }
    }
}

#[cfg(feature = "xwayland")]
impl XWaylandShellHandler for Rwl {
    fn xwayland_shell_state(&mut self) -> &mut XWaylandShellState {
        // Only reached when a client has bound the xwayland_shell global, which
        // only exists when we created it (XWayland enabled), so this is Some.
        self.xwayland_shell_state
            .as_mut()
            .unwrap_or_else(|| unreachable!("xwayland_shell_state accessed while XWayland disabled"))
    }

    /// `XWayland` has bound a `wl_surface` to an X11 window.  If that window is
    /// already mapped, key it into `window_map` now so the commit path (rules,
    /// borders, focus — all `window_for_surface`-based) finds it.  When the
    /// association arrives *before* the map instead, `map_x11_window` inserts it
    /// (the surface is available by then), so both orderings are covered without
    /// scanning every surface commit.
    fn surface_associated(
        &mut self,
        _xwm: smithay::xwayland::xwm::XwmId,
        wl_surface: WlSurface,
        surface: smithay::xwayland::X11Surface,
    ) {
        if let Some(window) = self.window_for_x11(&surface) {
            self.window_map.insert(wl_surface, window);
        }
    }
}

smithay::delegate_dispatch2!(Rwl);
