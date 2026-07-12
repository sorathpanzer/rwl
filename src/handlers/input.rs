//! Keyboard and pointer input processing.
//!
//! This module bridges smithay's input events to compositor actions.

use smithay::backend::input::{
    AbsolutePositionEvent, Axis, ButtonState, Event,
    InputBackend, KeyState, KeyboardKeyEvent, PointerAxisEvent, PointerButtonEvent,
    PointerMotionEvent,
};
use smithay::input::keyboard::{FilterResult, ModifiersState};
use smithay::input::pointer::{AxisFrame, ButtonEvent, MotionEvent};
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::utils::{Logical, Point, SERIAL_COUNTER};

use crate::config::Action;
use crate::state::{CursorMode, Rwl};

// ---------------------------------------------------------------------------
// Keyboard events
// ---------------------------------------------------------------------------

impl Rwl {
    /// Handle a raw keyboard event from any backend.
    pub fn process_key_event<B: InputBackend>(
        &mut self,
        event: &B::KeyboardKeyEvent,
    ) {
        self.notify_idle_activity();
        let serial = SERIAL_COUNTER.next_serial();
        let time = event.time_msec();
        let key_code = event.key_code();
        let key_state = event.state();

        let Some(kb) = self.keyboard.clone() else {
            return;
        };

        // Snapshot the modifier bits before this event so the tap-modkey gesture
        // can track which modifiers were held during a press→release with no
        // other key.
        #[cfg(feature = "mod-tap")]
        let mod_bits_before = mods_to_bits(&kb.modifier_state());

        // Notify the seat keyboard — our filter runs keybindings.
        // The read guard from config::get() MUST be dropped before dispatch() is
        // called: if the action is ReloadConfig, reload() tries to acquire the
        // write lock on the same RwLock, deadlocking the single-threaded loop.
        let action_opt = {
            let cfg = crate::config::get();
            kb.input(
                self,
                key_code,
                key_state,
                serial,
                time,
                |state, mods, handle| {
                    // Native screen locker: while active it owns all keyboard
                    // input. Feed printable keys / editing keys into the locker's
                    // password buffer and swallow every key (press and release)
                    // so nothing — not a keybind, not a client — ever sees it.
                    #[cfg(feature = "lock")]
                    if state.native_lock.is_some() {
                        if key_state == KeyState::Pressed {
                            let sym = handle.modified_sym();
                            crate::features::lock::feed_key(state, sym.raw(), sym.key_char());
                        }
                        return FilterResult::Intercept(Action::LockConsume);
                    }

                    if matches!(handle.modified_sym().raw(), k if k == smithay::input::keyboard::keysyms::KEY_NoSymbol) {
                        return FilterResult::Forward;
                    }

                    // Only intercept key presses, not releases — applies everywhere,
                    // including passthrough mode, so TogglePassthrough fires once.
                    if key_state == KeyState::Released {
                        return FilterResult::Forward;
                    }

                    // While the session is locked all keys must reach the screen locker;
                    // intercepting compositor bindings here would prevent the user from
                    // typing their password (or modifier combos within it).
                    if state.locked {
                        return FilterResult::Forward;
                    }

                    // While the overview is open it swallows keys: a configured
                    // keybind exits the overview and runs that action (dispatch
                    // force-closes the overview first); every other key (hint
                    // letters, arrows, Escape, Return, Backspace) is routed to
                    // the overview's own handler and never reaches clients.
                    #[cfg(feature = "overview")]
                    if state.overview.is_some() {
                        use smithay::input::keyboard::keysyms as xkb;
                        let keysym = handle.modified_sym().raw();
                        // Arrow keys always drive overview navigation, even with a
                        // modifier held — so Mod+arrows move the selection just like
                        // bare arrows (rather than matching a focus_stack keybind and
                        // exiting the overview).
                        if matches!(keysym, xkb::KEY_Left | xkb::KEY_Right | xkb::KEY_Up | xkb::KEY_Down) {
                            return FilterResult::Intercept(Action::OverviewNav(keysym));
                        }
                        let mod_bits = mods_to_bits(mods);
                        return cfg.keys.iter()
                            .find(|bind| bind.keysym == keysym
                                && clean_mask(mod_bits) == clean_mask(bind.mods))
                            .map_or_else(
                                || FilterResult::Intercept(Action::OverviewNav(keysym)),
                                |bind| FilterResult::Intercept(bind.action.clone()),
                            );
                    }

                    let keysym = handle.modified_sym().raw();
                    let mod_bits = mods_to_bits(mods);

                    // A bare Escape (no modifiers) closes PiP when it is active,
                    // without disturbing any modified binding such as Mod+Escape.
                    #[cfg(feature = "pip")]
                    if state.pip.is_some()
                        && keysym == smithay::input::keyboard::keysyms::KEY_Escape
                        && clean_mask(mod_bits) == 0
                    {
                        return FilterResult::Intercept(Action::TogglePip);
                    }

                    // In passthrough mode only allow the passthrough toggle binding
                    if state.passthrough {
                        return cfg.keys.iter()
                            .find(|bind| bind.keysym == keysym
                                && clean_mask(mod_bits) == clean_mask(bind.mods)
                                && matches!(bind.action, Action::TogglePassthrough))
                            .map_or(FilterResult::Forward, |bind| FilterResult::Intercept(bind.action.clone()));
                    }

                    cfg.keys.iter()
                        .find(|bind| bind.keysym == keysym
                            && clean_mask(mod_bits) == clean_mask(bind.mods))
                        .map_or(FilterResult::Forward, |bind| FilterResult::Intercept(bind.action.clone()))
                },
            )
        }; // cfg (RwLockReadGuard) dropped here, before dispatch()

        // On any key release, stop the key-repeat timer so held compositor
        // keybindings (e.g. Mod+Return, Mod+Q) stop firing.
        if key_state == KeyState::Released {
            if let Some(token) = self.key_repeat_timer.take() {
                self.loop_handle.remove(token);
            }
            self.key_repeat_action = None;
        }

        if let Some(action) = action_opt {
            self.dispatch(&action);
            if action_should_repeat(&action) {
                self.start_key_repeat(action);
            } else {
                // Non-repeating action: cancel any repeat timer left from a
                // previously held key so it doesn't fire after this press.
                if let Some(token) = self.key_repeat_timer.take() {
                    self.loop_handle.remove(token);
                }
                self.key_repeat_action = None;
            }
        }

        // Tap-modkey gesture (see `update_mod_tap`). Evaluated after dispatch so
        // any real keybind has already cancelled the candidate.
        #[cfg(feature = "mod-tap")]
        self.update_mod_tap(key_state, mod_bits_before, &kb.modifier_state());
    }

    /// Track the tap-modkey gesture, anchored on the mod key (Super):
    ///  - pressing the mod key arms a candidate;
    ///  - extra modifiers pressed while it is held (e.g. Shift, Ctrl) accumulate
    ///    into the set — no need to press them simultaneously;
    ///  - pressing any non-modifier key cancels it;
    ///  - releasing the mod key fires the action bound to `{ mods=<set>, key="" }`
    ///    (within ~250 ms of the last modifier press, so adding a second modifier
    ///    restarts the window — order Super-then-Shift is not penalised).
    ///
    /// So `M..S` = hold Super, tap Shift, release Super; `M` = tap Super alone.
    #[cfg(feature = "mod-tap")]
    fn update_mod_tap(&mut self, key_state: KeyState, bits_before: u32, mods_after: &ModifiersState) {
        const THRESHOLD_MS: u128 = 250;
        let bits_after = mods_to_bits(mods_after);
        let modkey = crate::config::MODKEY;
        match key_state {
            KeyState::Pressed => {
                let newly = bits_after & !bits_before;
                if newly == 0 {
                    // A non-modifier key (or repeat) — cancel the gesture.
                    self.mod_tap = None;
                } else if newly & modkey != 0 {
                    // The mod key itself was pressed — arm a candidate (carrying
                    // any modifiers already held).
                    self.mod_tap = Some((std::time::Instant::now(), bits_after));
                } else if let Some((_, peak)) = self.mod_tap {
                    // Another modifier added while the mod key is held — accumulate
                    // and restart the window (measured from the last modifier), so
                    // pressing Super *then* Shift isn't penalised by the delay.
                    self.mod_tap = Some((std::time::Instant::now(), peak | bits_after));
                }
                // (Other modifier pressed with no live candidate: ignore.)
            }
            KeyState::Released => {
                // Fire when the mod key itself is released.
                let modkey_released = bits_before & modkey != 0 && bits_after & modkey == 0;
                if modkey_released
                    && let Some((start, peak)) = self.mod_tap.take()
                    && start.elapsed().as_millis() <= THRESHOLD_MS
                {
                    let action = {
                        let cfg = crate::config::get();
                        cfg.keys
                            .iter()
                            .find(|b| b.keysym == crate::config::MOD_TAP_KEYSYM
                                && clean_mask(b.mods) == clean_mask(peak))
                            .map(|b| b.action.clone())
                    }; // guard dropped before dispatch (ReloadConfig deadlock)
                    if let Some(action) = action {
                        self.dispatch(&action);
                    }
                }
            }
        }
    }

    /// Start a calloop timer that re-dispatches `action` after `repeat_delay`
    /// ms and then every `1000 / repeat_rate` ms until the key is released.
    fn start_key_repeat(&mut self, action: crate::config::Action) {
        if let Some(token) = self.key_repeat_timer.take() {
            self.loop_handle.remove(token);
        }

        let (delay_ms, rate) = {
            let cfg = crate::config::get();
            (cfg.repeat_delay, cfg.repeat_rate)
        };
        let delay_ms = u64::try_from(delay_ms).unwrap_or(0);
        if delay_ms == 0 || rate <= 0 {
            self.key_repeat_action = None;
            return;
        }
        let rate_ms = (1000u64 / u64::try_from(rate).unwrap_or(1)).max(1);

        self.key_repeat_action = Some(action);

        self.key_repeat_timer = self
            .loop_handle
            .insert_source(
                Timer::from_duration(std::time::Duration::from_millis(delay_ms)),
                move |_, (), state| {
                    if let Some(action) = state.key_repeat_action.clone() {
                        state.dispatch(&action);
                    }
                    TimeoutAction::ToDuration(std::time::Duration::from_millis(rate_ms))
                },
            )
            .ok();
    }
}

const fn action_should_repeat(action: &crate::config::Action) -> bool {
    use crate::config::Action;
    matches!(action, Action::Spawn(_) | Action::SetMfact(_) | Action::KillClient)
}

// ---------------------------------------------------------------------------
// Pointer events
// ---------------------------------------------------------------------------

impl Rwl {
    /// Handle relative pointer motion.
    pub fn process_pointer_motion<B: InputBackend>(
        &mut self,
        event: &B::PointerMotionEvent,
    ) {
        let Some(pointer) = self.pointer.clone() else {
            return;
        };
        self.handle_cursor_activity();
        let delta = event.delta();
        let time = event.time_msec();
        let serial = SERIAL_COUNTER.next_serial();

        // Move pointer
        let mut new_loc = pointer.current_location();
        new_loc.x += delta.x;
        new_loc.y += delta.y;
        new_loc = self.clamp_pointer(new_loc);

        // Update selected monitor
        self.sel_mon = self.monitor_at(new_loc.x, new_loc.y).unwrap_or(self.sel_mon);

        // While the session is locked, only move the cursor and route hover to a
        // lock surface (external locker) or nowhere (native locker). No
        // focus-follows-pointer and no move/resize grab handling — client
        // windows stay unreachable behind the lock.
        if self.locked {
            let under = self.pointer_focus_under(new_loc);
            pointer.motion(self, under, &MotionEvent { location: new_loc, serial, time });
            pointer.frame(self);
            self.schedule_render();
            return;
        }

        match self.cursor_mode {
            CursorMode::Move => {
                crate::grab::handle_move(self, new_loc);
                // Still update pointer's internal position so current_location() stays current.
                pointer.motion(self, None, &MotionEvent { location: new_loc, serial, time });
                pointer.frame(self);
                self.schedule_render();
                return;
            }
            CursorMode::Resize => {
                crate::grab::handle_resize(self, new_loc);
                pointer.motion(self, None, &MotionEvent { location: new_loc, serial, time });
                pointer.frame(self);
                self.schedule_render();
                return;
            }
            CursorMode::Normal | CursorMode::Pressed => {}
        }

        // While the overview is open, move the cursor but route hover to the
        // overview (highlighting thumbnails) instead of focusing windows.
        #[cfg(feature = "overview")]
        if self.overview.is_some() {
            pointer.motion(self, None, &MotionEvent { location: new_loc, serial, time });
            pointer.frame(self);
            crate::features::overview::handle_pointer_motion(self, new_loc);
            return;
        }

        // Mouse focus: focus window under cursor on motion.
        // Only change focus when the cursor is actually over a window — moving
        // to empty space must NOT send wl_keyboard.leave to the current client
        // (mirrors C dwl's focusclient(c, false) where c may be NULL).
        // Guard against already-focused window to avoid calling update_borders()
        // and bumping border_counter on every motion event (defeats damage tracking).
        //
        // Also suppress focus-follows-mouse while a pointer button is held
        // (cursor_mode == Pressed).  A button-down starts an implicit pointer
        // grab owned by the client under the cursor: every drag — text
        // selection, slider drag, DnD threshold — belongs to that client until
        // release, so the keyboard focus must stay put even if the pointer
        // grazes an adjacent tiled window or the window's own edge.  Otherwise
        // the client loses keyboard focus mid-drag, which makes terminals clear
        // their selection highlight and prevents them from setting the primary
        // selection (the compositor denies set_selection from a non-focused
        // client).  wlroots/dwl suppress focus changes during an implicit grab
        // for exactly this reason.
        if crate::config::get().mouse_focus
            && self.cursor_mode == CursorMode::Normal
            && !self.passthrough
            && let Some(w) = self.space.element_under(new_loc).map(|(w, _)| w.clone())
            && self.focused_window() != Some(&w)
        {
            self.focus_window(Some(w));
        }

        // Find the topmost surface under the pointer, respecting z-order:
        // Overlay/Top layer shells → XDG windows → Bottom/Background layer shells.
        // This ensures bars, overlays, and panels receive wl_pointer.enter/motion.
        let under = self.pointer_focus_under(new_loc);

        pointer.motion(
            self,
            under,
            &MotionEvent {
                location: new_loc,
                serial,
                time,
            },
        );
        pointer.frame(self);
        // Cursor position changed — schedule a render so the cursor element
        // is redrawn at the new location even when no window is committing.
        self.schedule_render();
    }

    /// Handle absolute pointer motion (e.g. touchpad in absolute mode).
    pub fn process_pointer_motion_abs<B: InputBackend>(
        &mut self,
        event: &B::PointerMotionAbsoluteEvent,
    ) {
        let Some(pointer) = self.pointer.clone() else {
            return;
        };
        self.handle_cursor_activity();
        let serial = SERIAL_COUNTER.next_serial();
        let time = event.time_msec();

        // Convert absolute position to output-space
        let output_loc: Point<f64, Logical> = self.sel_monitor().map_or_else(
            || event.position_transformed((1920, 1080).into()),
            |mon| {
                let m = mon.m;
                let abs = event.position_transformed(m.size);
                (f64::from(m.loc.x) + abs.x, f64::from(m.loc.y) + abs.y).into()
            },
        );
        let new_loc = self.clamp_pointer(output_loc);

        self.sel_mon = self.monitor_at(new_loc.x, new_loc.y).unwrap_or(self.sel_mon);

        // Locked: move the cursor only, no focus/grab (see relative motion handler).
        if self.locked {
            let under = self.pointer_focus_under(new_loc);
            pointer.motion(self, under, &MotionEvent { location: new_loc, serial, time });
            pointer.frame(self);
            self.schedule_render();
            return;
        }

        match self.cursor_mode {
            CursorMode::Move => {
                crate::grab::handle_move(self, new_loc);
                pointer.motion(self, None, &MotionEvent { location: new_loc, serial, time });
                pointer.frame(self);
                self.schedule_render();
                return;
            }
            CursorMode::Resize => {
                crate::grab::handle_resize(self, new_loc);
                pointer.motion(self, None, &MotionEvent { location: new_loc, serial, time });
                pointer.frame(self);
                self.schedule_render();
                return;
            }
            CursorMode::Normal | CursorMode::Pressed => {}
        }

        // Mouse focus: only focus when cursor is over a window, and never while a
        // pointer button is held (implicit grab — see relative motion handler).
        if crate::config::get().mouse_focus
            && self.cursor_mode == CursorMode::Normal
            && !self.passthrough
            && let Some(w) = self.space.element_under(new_loc).map(|(w, _)| w.clone())
            && self.focused_window() != Some(&w)
        {
            self.focus_window(Some(w));
        }

        // Find the topmost surface under the pointer, respecting z-order:
        // Overlay/Top layer shells → XDG windows → Bottom/Background layer shells.
        let under = self.pointer_focus_under(new_loc);

        pointer.motion(
            self,
            under,
            &MotionEvent { location: new_loc, serial, time },
        );
        pointer.frame(self);
        // Cursor position changed — schedule a render so the cursor element
        // is redrawn at the new location even when no window is committing.
        self.schedule_render();
    }

    /// Handle pointer button events.
    pub fn process_pointer_button<B: InputBackend>(
        &mut self,
        event: &B::PointerButtonEvent,
    ) {
        let Some(pointer) = self.pointer.clone() else {
            return;
        };
        self.handle_cursor_activity();
        let serial = SERIAL_COUNTER.next_serial();
        let time = event.time_msec();
        let button = event.button_code();
        let state = event.state();

        // While the session is locked, forward the raw button only to whatever
        // the pointer is focused on (a lock surface for an external locker,
        // nothing for the native locker). Never run click-to-focus, mouse-button
        // bindings (which include `spawn`), or move/resize grabs while locked.
        if self.locked {
            pointer.button(self, &ButtonEvent { button, state, serial, time });
            pointer.frame(self);
            return;
        }

        // A click cancels a pending tap-modkey gesture (the mod key is being used
        // as a modifier for a mouse binding, not tapped).
        #[cfg(feature = "mod-tap")]
        { self.mod_tap = None; }

        // While the overview is open, a click selects the thumbnail under the
        // cursor (or dismisses via Escape); no clicks reach clients.
        #[cfg(feature = "overview")]
        if self.overview.is_some() {
            if state == ButtonState::Pressed {
                let loc = pointer.current_location();
                crate::features::overview::handle_pointer_click(self, loc);
            }
            return;
        }

        if state == ButtonState::Pressed {
            self.cursor_mode = CursorMode::Pressed;

            // Focus window under cursor on click (click-to-focus, always active).
            // MOUSE_FOCUS only controls focus-follows-pointer on motion events.
            let loc = pointer.current_location();
            // Clone to end the element_under borrow before any &mut self call.
            let window_under = self.space.element_under(loc).map(|(w, _)| w.clone());
            if !self.passthrough {
                if let Some(w) = window_under.clone() {
                    self.focus_window(Some(w));
                }

                // Check button bindings.  These fire regardless of whether a
                // window is under the cursor so bindings meant for the root /
                // layer-shell surfaces (e.g. a spawn-menu on right-click) work
                // over empty space too, matching C dwl's root button handling.
                // Collect the matching action while the read guard is held, then
                // drop the guard before dispatch() — same ReloadConfig deadlock
                // risk as in the keyboard path.
                let mods = self
                    .keyboard
                    .as_ref()
                    .map_or(0, |kb| mods_to_bits(&kb.modifier_state()));

                let matched = {
                    let cfg = crate::config::get();
                    cfg.buttons
                        .iter()
                        .find(|b| b.button == button && clean_mask(mods) == clean_mask(b.mods))
                        .map(|b| b.action.clone())
                }; // guard dropped here

                if let Some(action) = matched {
                    match action {
                        // Move / Resize need a window to act on; when the click
                        // landed on empty space there is nothing to grab, so
                        // consume the binding without starting a grab.
                        crate::config::Action::Move => {
                            if let Some(w) = window_under {
                                crate::grab::start_move(self, w);
                            }
                            return;
                        }
                        crate::config::Action::Resize => {
                            if let Some(w) = window_under {
                                crate::grab::start_resize(
                                    self,
                                    w,
                                    smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::ResizeEdge::BottomRight,
                                );
                            }
                            return;
                        }
                        action => {
                            self.dispatch(&action);
                            return;
                        }
                    }
                }
            }
        } else {
            // Released
            if matches!(self.cursor_mode, CursorMode::Move | CursorMode::Resize) {
                crate::grab::end_grab(self);
            } else {
                self.cursor_mode = CursorMode::Normal;
            }
        }

        pointer.button(
            self,
            &ButtonEvent {
                button,
                state,
                serial,
                time,
            },
        );
        pointer.frame(self);
    }

    /// Handle pointer axis (scroll) events.
    #[allow(clippy::cast_possible_truncation)]
    pub fn process_pointer_axis<B: InputBackend>(
        &mut self,
        event: &B::PointerAxisEvent,
    ) {
        let Some(pointer) = self.pointer.clone() else {
            return;
        };
        self.handle_cursor_activity();

        // Re-establish pointer focus if it was cleared since the last motion
        // event.  This happens when a client destroys and recreates subsurfaces
        // (e.g. QtWebEngine during page navigation): the old surface is gone so
        // Smithay clears the focus, but no motion event fires to pick up the
        // new surface, so scroll events are silently dropped until the user
        // moves the mouse.
        if pointer.current_focus().is_none() {
            let loc = pointer.current_location();
            let under = self.pointer_focus_under(loc);
            let serial = SERIAL_COUNTER.next_serial();
            pointer.motion(
                self,
                under,
                &MotionEvent { location: loc, serial, time: event.time_msec() },
            );
            pointer.frame(self);
        }

        let mut frame = AxisFrame::new(event.time_msec()).source(event.source());

        // For finger-sourced (touchpad) scroll, libinput sends a final event
        // with amount = 0.0 when the fingers lift.  This MUST be forwarded as
        // WL_POINTER_AXIS_STOP, NOT as a zero-value axis event.  Chromium-based
        // clients (qutebrowser/QtWebEngine) strictly require AXIS_STOP to close
        // the gesture; without it they consider the gesture still ongoing and
        // silently drop subsequent scroll gestures.
        if let Some(v) = event.amount(Axis::Horizontal) {
            if v == 0.0 {
                frame = frame.stop(Axis::Horizontal);
            } else {
                frame = frame.value(Axis::Horizontal, v);
                if let Some(d) = event.amount_v120(Axis::Horizontal) {
                    frame = frame.v120(Axis::Horizontal, d as i32);
                }
            }
        }
        if let Some(v) = event.amount(Axis::Vertical) {
            if v == 0.0 {
                frame = frame.stop(Axis::Vertical);
            } else {
                frame = frame.value(Axis::Vertical, v);
                if let Some(d) = event.amount_v120(Axis::Vertical) {
                    frame = frame.v120(Axis::Vertical, d as i32);
                }
            }
        }

        pointer.axis(self, frame);
        pointer.frame(self);
    }

    /// Clamp a pointer position to the union of all monitor geometries.
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    fn clamp_pointer(&self, mut pos: Point<f64, Logical>) -> Point<f64, Logical> {
        if self.monitors.is_empty() {
            return pos;
        }
        let (min_x, min_y, max_x, max_y) = self.monitor_bounds;
        pos.x = pos.x.clamp(f64::from(min_x), f64::from(max_x));
        pos.y = pos.y.clamp(f64::from(min_y), f64::from(max_y));
        pos
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Convert smithay's [`ModifiersState`] to a simple bitmask matching our
/// config constants.
#[must_use]
pub const fn mods_to_bits(mods: &ModifiersState) -> u32 {
    let mut bits: u32 = 0;
    if mods.logo {
        bits |= crate::config::MODKEY;
    }
    if mods.ctrl {
        bits |= 1 << 2; // CTRL_BIT position used in config
    }
    if mods.shift {
        bits |= 1 << 0; // SHIFT_BIT
    }
    if mods.alt {
        bits |= 1 << 3; // ALT_BIT
    }
    bits
}

/// Strip `CapsLock` and `NumLock` from a modifier bitmask.
#[must_use]
pub const fn clean_mask(mask: u32) -> u32 {
    // CapsLock bit is not in our bitmask scheme, so no-op here;
    // the keysym comparison handles the rest.
    mask
}
