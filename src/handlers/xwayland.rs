//! `XWayland` window-manager handler.
//!
//! Bridges X11 windows (Steam, Proton games, legacy X11 apps) into rwl's native
//! window model.  `smithay::desktop::Window` wraps an [`X11Surface`] the same way
//! it wraps an xdg toplevel, so once an X11 window is inserted into `self.windows`
//! / `self.space` / `self.window_map` it flows through the existing layout, border,
//! tag, and focus machinery unchanged.  The only X11-specific work is answering
//! configure/map requests and pushing geometry back out via [`X11Surface::configure`]
//! (done from `arrange`, see `state.rs`).

use smithay::desktop::Window;
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::utils::{Logical, Point, Rectangle};
use smithay::xwayland::xwm::{Reorder, ResizeEdge, XwmId};
use smithay::xwayland::{X11Surface, X11Wm, XwmHandler};

use crate::state::Rwl;
use smithay::wayland::seat::WaylandFocus;

use crate::window::{with_state, with_state_mut};

impl Rwl {
    /// Locate the [`Window`] wrapping a given X11 surface, if it is mapped —
    /// searching both the managed list and the override-redirect list.
    pub(crate) fn window_for_x11(&self, surface: &X11Surface) -> Option<Window> {
        self.windows
            .iter()
            .chain(self.x11_or_windows.iter())
            .find(|w| matches!(w.x11_surface(), Some(x) if x == surface))
            .cloned()
    }

    /// Locate the override-redirect [`Window`] whose surface tree is rooted at
    /// `wl`.  Cheap: the list is empty unless a menu/tooltip is currently open.
    pub(crate) fn x11_or_window_for_surface(
        &self,
        wl: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
    ) -> Option<Window> {
        if self.x11_or_windows.is_empty() {
            return None;
        }
        self.x11_or_windows
            .iter()
            .find(|w| w.wl_surface().as_deref() == Some(wl))
            .cloned()
    }

    /// Map a managed (non-override-redirect) X11 window, mirroring `new_toplevel`.
    ///
    /// Rules are left un-applied (`rules_applied = false`) so the shared commit
    /// path in `handlers::compositor` applies them — and fires the open hooks —
    /// exactly as it does for Wayland windows, once the surface commits a buffer.
    /// Centre a managed floating X11 window on its monitor's work area, then push
    /// the position out to the client.  Uses the fixed-size hint (`min==max`) as the
    /// size when available — splash/startup windows often map tiny and set their
    /// real size via hints a moment later, so the raw geometry can't be trusted yet.
    fn center_x11(&mut self, window: &Window) {
        let Some(x11) = window.x11_surface() else { return };
        let size = match (x11.min_size(), x11.max_size()) {
            (Some(min), Some(max)) if min == max => min,
            _ => x11.geometry().size,
        };
        if size.w <= 0 || size.h <= 0 {
            return;
        }
        let mon_idx = with_state(window, |s| s.mon_idx).unwrap_or(self.sel_mon);
        let Some((origin, work)) = self.monitors.get(mon_idx).map(|m| (m.m.loc, m.w.size)) else {
            return;
        };
        let loc = Point::from((
            origin.x + (work.w - size.w) / 2,
            origin.y + (work.h - size.h) / 2,
        ));
        with_state_mut(window, |s| s.saved_loc = Some(loc));
        self.space.map_element(window.clone(), loc, false);
        let _ = x11.configure(Rectangle::new(loc, size));
        self.schedule_render();
    }

    fn map_x11_window(&mut self, surface: &X11Surface) {
        let window = Window::new_x11_window(surface.clone());

        let sel_mon = self.sel_mon;
        let (base_loc, initial_tags) = self
            .sel_monitor()
            .map_or_else(|| (Point::from((0, 0)), 1), |mon| (mon.w.loc, mon.tags()));

        let float = Self::window_is_float_type(&window);

        with_state_mut(&window, |s| {
            s.tags = initial_tags;
            s.mon_idx = sel_mon;
            if float {
                s.is_floating = true;
            }
        });

        self.space.map_element(window.clone(), base_loc, true);
        self.windows.insert(0, window.clone());
        if let Some(wl) = surface.wl_surface() {
            self.window_map.insert(wl, window.clone());
        }
        // Centre a floating window on its monitor's work area (like mango's default
        // setclient_coordinate_center).  Uses the fixed-size hint when present, so
        // it's correct even before the client's first buffer.
        if float {
            self.center_x11(&window);
        }

        // Focus the freshly-mapped window and re-tile.  focus_window handles the
        // focus stack, keyboard focus, sel_mon tracking, borders and arrange.
        self.focus_window(Some(window));
        self.arrange_all();
        crate::ipc::print_status(self);
    }

    /// Map an override-redirect X11 window (menus, tooltips, drag icons, Steam popups).
    ///
    /// Unmanaged: placed in `space` at the exact coordinates the client asked for
    /// and tracked only in `x11_or_windows` — never in `windows` / `window_map` /
    /// `focus_stack`, so tiling, focus, borders, and the managed commit path all
    /// skip it.  It still renders and receives frame callbacks via the space.
    fn map_x11_override_redirect(&mut self, surface: &X11Surface) {
        let window = Window::new_x11_window(surface.clone());
        let geo = surface.geometry();
        with_state_mut(&window, |s| {
            s.is_override_redirect = true;
            s.rules_applied = true; // no rules for unmanaged popups
        });
        self.space.map_element(window.clone(), geo.loc, true);
        self.x11_or_windows.push(window);
        self.schedule_render();
    }

    /// Remove an X11 window from all tracking structures.  Managed windows also
    /// re-focus and re-tile; override-redirect windows just leave the space.
    fn unmap_x11_window(&mut self, surface: &X11Surface) {
        let Some(window) = self.window_for_x11(surface) else {
            return;
        };
        self.space.unmap_elem(&window);

        if with_state(&window, |s| s.is_override_redirect).unwrap_or(false) {
            self.x11_or_windows.retain(|w| w != &window);
            self.schedule_render();
            return;
        }

        if let Some(wl) = surface.wl_surface() {
            self.window_map.remove(&wl);
        }
        self.windows.retain(|w| w != &window);
        self.focus_stack.retain(|w| w != &window);

        // A managed window just closed.  Dismiss any override-redirect popups
        // (menus, tooltips): clients like Steam orphan their open menu when their
        // window is closed out from under them — never sending an UnmapNotify — so
        // it would otherwise linger on screen forever.  Menus are transient and
        // re-created on demand, so clearing them here is safe.
        self.dismiss_x11_popups();

        let top = self.focused_window().cloned();
        self.focus_window(top);
        self.arrange_all();
        crate::ipc::print_status(self);
    }

    /// Unmap and drop every override-redirect popup (menu/tooltip).
    fn dismiss_x11_popups(&mut self) {
        if self.x11_or_windows.is_empty() {
            return;
        }
        for w in std::mem::take(&mut self.x11_or_windows) {
            self.space.unmap_elem(&w);
        }
        self.schedule_render();
    }
}

impl XwmHandler for Rwl {
    fn xwm_state(&mut self, _xwm: XwmId) -> &mut X11Wm {
        // Smithay only dispatches X11 events (the sole callers of this accessor)
        // while an X11Wm is installed, so `xwm` is always Some here.
        self.xwm
            .as_mut()
            .unwrap_or_else(|| unreachable!("xwm_state called without an active X11Wm"))
    }

    fn new_window(&mut self, _xwm: XwmId, _window: X11Surface) {}
    fn new_override_redirect_window(&mut self, _xwm: XwmId, _window: X11Surface) {}

    fn map_window_request(&mut self, _xwm: XwmId, window: X11Surface) {
        // Realize the window on the X side, then adopt it into rwl's model.
        if let Err(e) = window.set_mapped(true) {
            tracing::warn!("x11 set_mapped failed: {e}");
            return;
        }
        self.map_x11_window(&window);
    }

    fn mapped_override_redirect_window(&mut self, _xwm: XwmId, window: X11Surface) {
        self.map_x11_override_redirect(&window);
    }

    fn property_notify(
        &mut self,
        _xwm: XwmId,
        window: X11Surface,
        property: smithay::xwayland::xwm::WmWindowProperty,
    ) {
        use smithay::xwayland::xwm::WmWindowProperty;
        // Splash/startup windows (GIMP, LibreOffice, …) map first and set their
        // window type / fixed-size hints a beat later.  Re-evaluate the float
        // heuristic when those properties arrive — as dwl/mango do — so such a
        // window becomes floating and gets centred instead of staying tiled in
        // the corner.  Only ever promotes tiled→floating; never the reverse.
        if !matches!(
            property,
            WmWindowProperty::WindowType
                | WmWindowProperty::TransientFor
                | WmWindowProperty::NormalHints
                | WmWindowProperty::Hints
        ) {
            return;
        }
        let Some(w) = self.window_for_x11(&window) else { return };
        let (is_or, already) =
            with_state(&w, |s| (s.is_override_redirect, s.is_floating)).unwrap_or((true, true));
        if is_or || already || !Self::window_is_float_type(&w) {
            return;
        }
        with_state_mut(&w, |s| s.is_floating = true);
        self.center_x11(&w);
        self.arrange_all();
        crate::ipc::print_status(self);
    }

    fn unmapped_window(&mut self, _xwm: XwmId, window: X11Surface) {
        self.unmap_x11_window(&window);
        if !window.is_override_redirect() {
            let _ = window.set_mapped(false);
        }
    }

    fn destroyed_window(&mut self, _xwm: XwmId, window: X11Surface) {
        // Belt-and-braces: a client may destroy without a prior unmap.
        self.unmap_x11_window(&window);
    }

    fn configure_request(
        &mut self,
        _xwm: XwmId,
        window: X11Surface,
        x: Option<i32>,
        y: Option<i32>,
        w: Option<u32>,
        h: Option<u32>,
        _reorder: Option<Reorder>,
    ) {
        // Honour self-configure only for floating / unmanaged windows; tiled
        // windows are sized by the layout, so re-assert their current geometry.
        let managed = self.window_for_x11(&window);
        let is_floating = managed
            .as_ref()
            .is_none_or(|w| {
                with_state(w, |s| s.is_override_redirect || s.is_floating).unwrap_or(true)
            });

        if !is_floating {
            // Tiled: deny the move/resize, re-assert the layout geometry.
            let _ = window.configure(window.geometry());
            return;
        }

        let mut geo = window.geometry();
        // Base the position on rwl's tracked location (saved_loc), NOT the X
        // geometry: after we relocate a window (e.g. centring), window.geometry()
        // still lags at the old spot until the client round-trips, so a size-only
        // configure (x/y = None, as a resize sends) would otherwise snap it back.
        if let Some(sl) = managed
            .as_ref()
            .and_then(|w| with_state(w, |s| s.saved_loc).flatten())
        {
            geo.loc = sl;
        }
        if let Some(x) = x {
            geo.loc.x = x;
        }
        if let Some(y) = y {
            geo.loc.y = y;
        }
        if let Some(w) = w {
            geo.size.w = i32::try_from(w).unwrap_or(geo.size.w);
        }
        if let Some(h) = h {
            geo.size.h = i32::try_from(h).unwrap_or(geo.size.h);
        }
        let _ = window.configure(geo);

        // Actually move the window to where the client asked (dwl does this in
        // configurex11 → resize).  Without relocating it in the space and recording
        // saved_loc, rwl would ack the move but keep drawing it at the old spot —
        // which is why Steam's login window stayed in the corner instead of the
        // centre it configures itself to.
        if let Some(w) = managed {
            self.space.map_element(w.clone(), geo.loc, false);
            with_state_mut(&w, |s| s.saved_loc = Some(geo.loc));
            self.schedule_render();
        }
    }

    fn configure_notify(
        &mut self,
        _xwm: XwmId,
        window: X11Surface,
        geometry: Rectangle<i32, Logical>,
        _above: Option<u32>,
    ) {
        // Keep the space location in sync when an (override-redirect) window
        // moves itself — this is how menus/tooltips follow the pointer.
        if let Some(w) = self.window_for_x11(&window) {
            self.space.map_element(w, geometry.loc, false);
            self.schedule_render();
        }
    }

    fn resize_request(
        &mut self,
        _xwm: XwmId,
        window: X11Surface,
        _button: u32,
        _edges: ResizeEdge,
    ) {
        if let Some(w) = self.window_for_x11(&window) {
            // start_resize ignores the edge; pass an arbitrary one.
            crate::grab::start_resize(self, w, xdg_toplevel::ResizeEdge::BottomRight);
        }
    }

    fn move_request(&mut self, _xwm: XwmId, window: X11Surface, _button: u32) {
        if let Some(w) = self.window_for_x11(&window) {
            crate::grab::start_move(self, w);
        }
    }

    fn fullscreen_request(&mut self, _xwm: XwmId, window: X11Surface) {
        if let Some(w) = self.window_for_x11(&window) {
            crate::window::set_fullscreen(&w, true);
            self.arrange_all();
        }
    }

    fn unfullscreen_request(&mut self, _xwm: XwmId, window: X11Surface) {
        if let Some(w) = self.window_for_x11(&window) {
            crate::window::set_fullscreen(&w, false);
            self.arrange_all();
        }
    }

    fn maximize_request(&mut self, xwm: XwmId, window: X11Surface) {
        // dwl has no maximize; treat it as fullscreen.
        self.fullscreen_request(xwm, window);
    }

    fn unmaximize_request(&mut self, xwm: XwmId, window: X11Surface) {
        self.unfullscreen_request(xwm, window);
    }

    fn disconnected(&mut self, _xwm: XwmId) {
        self.xwm = None;
        self.xdisplay = None;

        // XWayland exited: every X11 window is now backed by a dead surface that
        // can no longer be configured or closed via X.  Tear them all down so no
        // stale, un-closeable clients linger in the layout.  (Pure-Wayland windows
        // are untouched.)
        let dead: Vec<Window> = self
            .windows
            .iter()
            .filter(|w| w.x11_surface().is_some())
            .cloned()
            .collect();
        if dead.is_empty() && self.x11_or_windows.is_empty() {
            return;
        }
        for w in &dead {
            self.space.unmap_elem(w);
            if let Some(wl) = w.wl_surface() {
                self.window_map.remove(&*wl);
            }
        }
        for w in std::mem::take(&mut self.x11_or_windows) {
            self.space.unmap_elem(&w);
        }
        self.windows.retain(|w| w.x11_surface().is_none());
        self.focus_stack.retain(|w| w.x11_surface().is_none());

        let top = self.focused_window().cloned();
        self.focus_window(top);
        self.arrange_all();
        crate::ipc::print_status(self);
    }
}
