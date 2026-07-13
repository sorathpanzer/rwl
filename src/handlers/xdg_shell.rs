//! `xdg_shell` handler — manages application windows.

use smithay::desktop::Window;
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::reexports::wayland_server::protocol::wl_seat::WlSeat;
use smithay::utils::Serial;
use smithay::wayland::shell::xdg::{
    PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
};

#[cfg(feature = "auto-back-empty-tag")]
use crate::monitor::Monitor;
use crate::state::Rwl;
use crate::window::{set_fullscreen, with_state, with_state_mut, window_is_floating};

impl XdgShellHandler for Rwl {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        let window = Window::new_wayland_window(surface.clone());

        // Assign default tags (monitor's current view) and map at a sensible
        // position.  Rules (app_id / title matching) are deferred to the first
        // surface commit because set_app_id / set_title haven't been processed
        // yet at this point in the protocol flow.
        let sel_mon = self.sel_mon;
        let (loc, initial_tags) = self
            .sel_monitor()
            .map_or_else(|| ((0, 0).into(), 1), |mon| (mon.w.loc, mon.tags()));
        // Initialize mon_idx to sel_mon here — before focus_stack manipulation reads it.
        // WindowState::new() defaults mon_idx=0, which would cause sel_mon to be reset
        // to 0 on every window open if the user is on monitor 1+.
        with_state_mut(&window, |s| {
            s.tags = initial_tags;
            s.mon_idx = sel_mon;
        });

        tracing::info!("new_toplevel: mapping window at {:?}", loc);
        self.space.map_element(window.clone(), loc, true);
        self.windows.insert(0, window.clone());
        self.window_map.insert(surface.wl_surface().clone(), window.clone());

        // Announce server-side decorations now so the decoration_mode is
        // included in the very first configure the client receives.
        surface.with_pending_state(|state| {
            state.decoration_mode =
                Some(smithay::reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode::ServerSide);
        });

        // Send an initial configure with decoration mode but WITHOUT Activated.
        // This mirrors wlroots' automatic initial configure in C dwl and gives
        // clients an explicit "unfocused" state to transition FROM.  Without
        // this, clients that track focus via xdg_toplevel Activated state (e.g.
        // mpv with profile-cond=not focused) never see a false→true transition
        // and the profile restore never fires.  commit() will send the second
        // configure with Activated=true once apply_rules() has run.
        surface.send_configure();

        // Pre-insert into the focus stack so arrange() will set Activated on
        // this window in the second configure (from commit()).  wl_keyboard.enter
        // is sent from commit() via focus_window(), after the configure.
        let prev_focused = self.focus_stack.first().cloned();
        self.focus_stack.retain(|x| x != &window);
        self.focus_stack.insert(0, window.clone());

        if self.focus_stack.first() != prev_focused.as_ref() {
            let new_mon = with_state(&window, |s| s.mon_idx);
            let prev_mon = prev_focused.as_ref().and_then(|w| with_state(w, |s| s.mon_idx));
            if let Some(mon_idx) = new_mon {
                self.sel_mon = mon_idx;
                self.arrange(mon_idx);
                if let Some(prev_idx) = prev_mon
                    && prev_idx != mon_idx
                {
                    self.arrange(prev_idx);
                }
            } else {
                self.arrange_all();
            }
        }
        self.update_borders();
        crate::ipc::print_status(self);
    }

    fn new_popup(&mut self, surface: PopupSurface, _positioner: PositionerState) {
        if let Err(err) = self.popup_manager.track_popup(surface.clone().into()) {
            tracing::warn!("track_popup failed: {:?}", err);
            return;
        }

        // Constrain the popup to its parent's output before the first configure,
        // so a menu opened near a screen edge flips/slides back on-screen instead
        // of spilling off it (wlroots does this via wlr_xdg_popup_unconstrain_from_box).
        self.unconstrain_popup(&surface);

        if let Err(err) = surface.send_configure() {
            tracing::warn!("popup send_configure failed: {:?}", err);
        }
    }

    fn move_request(&mut self, surface: ToplevelSurface, seat: WlSeat, serial: Serial) {
        let _ = (seat, serial);
        let window = self.window_map.get(surface.wl_surface()).cloned();

        if let Some(w) = window {
            crate::grab::start_move(self, w);
        }
    }

    fn resize_request(
        &mut self,
        surface: ToplevelSurface,
        seat: WlSeat,
        serial: Serial,
        edges: xdg_toplevel::ResizeEdge,
    ) {
        let _ = (seat, serial);
        let window = self.window_map.get(surface.wl_surface()).cloned();

        if let Some(w) = window {
            crate::grab::start_resize(self, w, edges);
        }
    }

    fn grab(&mut self, surface: PopupSurface, seat: WlSeat, serial: Serial) {
        use smithay::desktop::{
            PopupKeyboardGrab, PopupKind, PopupPointerGrab, PopupUngrabStrategy,
            find_popup_root_surface,
        };
        use smithay::input::Seat;
        use smithay::input::pointer::Focus;

        let Some(seat) = Seat::<Self>::from_resource(&seat) else {
            return;
        };
        let kind = PopupKind::Xdg(surface);
        let Ok(root) = find_popup_root_surface(&kind) else { return };
        let mut grab = match self.popup_manager.grab_popup(root, kind, &seat, serial) {
            Ok(g) => g,
            Err(e) => {
                tracing::debug!("popup grab failed: {:?}", e);
                return;
            }
        };
        if let Some(keyboard) = seat.get_keyboard() {
            if keyboard.is_grabbed()
                && !keyboard.has_grab(serial)
                && !keyboard.has_grab(grab.previous_serial().unwrap_or(serial))
            {
                grab.ungrab(PopupUngrabStrategy::All);
                return;
            }
            keyboard.set_focus(self, grab.current_grab(), serial);
            keyboard.set_grab(self, PopupKeyboardGrab::new(&grab), serial);
        }
        if let Some(pointer) = seat.get_pointer() {
            if pointer.is_grabbed()
                && !pointer.has_grab(serial)
                && !pointer.has_grab(grab.previous_serial().unwrap_or(serial))
            {
                grab.ungrab(PopupUngrabStrategy::All);
                return;
            }
            pointer.set_grab(self, PopupPointerGrab::new(&grab), serial, Focus::Keep);
        }
    }

    fn reposition_request(
        &mut self,
        surface: PopupSurface,
        positioner: PositionerState,
        token: u32,
    ) {
        surface.with_pending_state(|state| {
            state.geometry = positioner.get_geometry();
            state.positioner = positioner;
        });
        // Re-apply the edge constraints for the new positioner (e.g. submenus).
        self.unconstrain_popup(&surface);
        surface.send_repositioned(token);
        // XDG protocol requires a configure to follow repositioned.
        // Without it the client waits indefinitely and the popup never moves.
        if let Err(e) = surface.send_configure() {
            tracing::warn!("reposition send_configure failed: {:?}", e);
        }
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        let wl_surface = surface.wl_surface();

        // O(1) map removal gives us the Window handle without an O(n) scan.
        let window = self.window_map.remove(wl_surface);

        self.windows.retain(|w| {
            w.toplevel()
                .is_none_or(|t| t.wl_surface() != wl_surface)
        });
        self.focus_stack.retain(|w| {
            w.toplevel()
                .is_none_or(|t| t.wl_surface() != wl_surface)
        });

        // Clipboard tools like wl-paste create and immediately destroy xdg_toplevels
        // without ever committing a buffer.  Treat those as invisible: skip the
        // focus/arrange/warp machinery so paste/copy in editors doesn't steal focus
        // or move the cursor.
        let was_visible = window.as_ref()
            .is_some_and(|w| with_state(w, |s| s.buffer_mapped).unwrap_or(false));

        if let Some(w) = window {
            // Clear PiP if the mirrored window is the one closing.
            #[cfg(feature = "pip")]
            crate::features::pip::on_window_closed(self, &w);

            #[cfg(feature = "auto-back-empty-tag")]
            let mon_idx = with_state(&w, |s| s.mon_idx).unwrap_or(self.sel_mon);
            #[cfg(feature = "auto-back-empty-tag")]
            let switch_to_tag = with_state(&w, |s| s.switch_to_tag).flatten();

            self.space.unmap_elem(&w);

            #[cfg(feature = "auto-back-empty-tag")]
            let current_tags = self.monitors.get(mon_idx).map_or(0, Monitor::tags);

            #[cfg(feature = "auto-back-empty-tag")]
            if was_visible {
                crate::features::auto_back_empty_tag::on_window_closed(
                    self, mon_idx, current_tags, switch_to_tag,
                );
            }

            // Fire on_window_close after the window has left self.windows so a
            // callback's rwl.count reflects the post-close tally (e.g. reverting
            // a dynamic layout when a tag drops below a threshold).
            #[cfg(feature = "hooks")]
            if was_visible {
                crate::features::hooks::window_close(self, &w);
                crate::features::hooks::drain(self);
            }
            #[cfg(feature = "ipc")]
            if was_visible {
                crate::features::ipc::event::window(self, "close", &w);
            }
        }

        // Always re-focus and re-arrange so the layout is correct after removing
        // the window from self.windows / space (even phantom windows were laid out).
        // For phantom closes: keep focus on whoever is already at the top of the
        // stack (no focus change, no warp).  For real closes: land on master in
        // tile layout as usual, then warp.
        let top = if was_visible {
            self.tile_master_window()
                .or_else(|| self.focused_window())
                .cloned()
        } else {
            self.focused_window().cloned()
        };
        self.focus_window(top);
        self.arrange_all();
        #[cfg(feature = "warp")]
        if was_visible {
            crate::features::warp::warp_cursor_to_focused(self);
        }

        // Keep the overview grid in sync when a window closes while it's open
        // (whether killed from the overview or closed by the app itself).
        #[cfg(feature = "overview")]
        if was_visible {
            crate::features::overview::on_window_closed(self);
        }
    }

    fn fullscreen_request(
        &mut self,
        surface: ToplevelSurface,
        mut _output: Option<smithay::reexports::wayland_server::protocol::wl_output::WlOutput>,
    ) {
        let wl = surface.wl_surface();
        if let Some(w) = self.window_map.get(wl).cloned() {
            #[cfg(feature = "hooks")]
            let was_fs = crate::window::window_is_fullscreen(&w);
            set_fullscreen(&w, true);
            // arrange_all() will send the configure with the correct size and
            // xdg_toplevel::State::Fullscreen set; no need to send it here.
            self.arrange_all();
            #[cfg(feature = "hooks")]
            if !was_fs {
                crate::features::hooks::fullscreen(self, &w, true);
            }
        }
    }

    fn unfullscreen_request(&mut self, surface: ToplevelSurface) {
        let wl = surface.wl_surface();
        if let Some(w) = self.window_map.get(wl).cloned() {
            #[cfg(feature = "hooks")]
            let was_fs = crate::window::window_is_fullscreen(&w);
            set_fullscreen(&w, false);
            self.arrange_all();
            #[cfg(feature = "hooks")]
            if was_fs {
                crate::features::hooks::fullscreen(self, &w, false);
            }
        }
    }

    fn maximize_request(&mut self, surface: ToplevelSurface) {
        // Treat maximize as fullscreen
        self.fullscreen_request(surface, None);
    }

    fn unmaximize_request(&mut self, surface: ToplevelSurface) {
        self.unfullscreen_request(surface);
    }

    fn minimize_request(&mut self, _surface: ToplevelSurface) {
        // dwl does not support minimize
    }

    fn parent_changed(&mut self, surface: ToplevelSurface) {
        // When a toplevel gains a parent after initial mapping, re-evaluate its
        // floating state so it inherits the parent's tags and monitor.
        let window = self.window_map.get(surface.wl_surface()).cloned();
        if let Some(w) = window
            && surface.parent().is_some()
            && !window_is_floating(&w)
        {
            with_state_mut(&w, |s| s.rules_applied = false);
        }
    }
}

impl Rwl {
    /// Constrain a popup so it stays within its parent window's output(s).
    ///
    /// The client's positioner carries `constraint_adjustment` flags (flip / slide /
    /// resize); `get_unconstrained_geometry` applies them against the target box so a
    /// menu opened near the right edge of the screen flips to the left of the pointer
    /// instead of rendering off-screen. Mirrors wlroots' `unconstrain_from_box`.
    fn unconstrain_popup(&self, popup: &PopupSurface) {
        use smithay::desktop::{PopupKind, find_popup_root_surface, get_popup_toplevel_coords};

        let Ok(root) = find_popup_root_surface(&PopupKind::Xdg(popup.clone())) else {
            return;
        };
        let Some(window) = self.window_for_surface(&root) else {
            return;
        };

        let mut outputs = self.space.outputs_for_element(window);
        let Some(first) = outputs.pop() else { return };
        let Some(mut outputs_geo) = self.space.output_geometry(&first) else {
            return;
        };
        for output in outputs {
            if let Some(geo) = self.space.output_geometry(&output) {
                outputs_geo = outputs_geo.merge(geo);
            }
        }

        let Some(window_geo) = self.space.element_geometry(window) else {
            return;
        };

        // The positioner target must be relative to the popup's parent geometry.
        let mut target = outputs_geo;
        target.loc -= get_popup_toplevel_coords(&PopupKind::Xdg(popup.clone()));
        target.loc -= window_geo.loc;

        popup.with_pending_state(|state| {
            state.geometry = state.positioner.get_unconstrained_geometry(target);
        });
    }
}
