//! `ext-session-lock-v1` handler — allows screen lockers like swaylock to
//! lock the session and present a fullscreen surface on every output.

use smithay::input::pointer::MotionEvent;
use smithay::output::Output;
use smithay::reexports::wayland_server::protocol::wl_output::WlOutput;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::SERIAL_COUNTER;
use smithay::wayland::seat::WaylandFocus;
use smithay::wayland::session_lock::{
    LockSurface, LockSurfaceConfigure, SessionLockHandler, SessionLockManagerState, SessionLocker,
};

use crate::state::Rwl;

impl SessionLockHandler for Rwl {
    fn lock_state(&mut self) -> &mut SessionLockManagerState {
        &mut self.session_lock_state
    }

    fn lock(&mut self, confirmation: SessionLocker) {
        // The native locker and an external `ext-session-lock-v1` client must be
        // mutually exclusive. If the native locker already owns the session,
        // refuse this request: drop `confirmation` WITHOUT calling `.lock()` so
        // smithay sends the client `finished`, and leave our state untouched.
        // Otherwise a client could later clear `locked` via `unlock()` while the
        // native lock is still up, exposing the desktop.
        #[cfg(feature = "lock")]
        if self.native_lock.is_some() {
            return;
        }

        self.locked = true;
        // Confirm immediately — lock surfaces will cover the screen contents.
        confirmation.lock();
        #[cfg(feature = "hooks")]
        crate::features::hooks::lock(self);

        // Force pointer to re-evaluate focus so it routes to the lock surface.
        if let Some(ptr) = self.pointer.clone() {
            let loc = ptr.current_location();
            let under = self.pointer_focus_under(loc);
            let serial = SERIAL_COUNTER.next_serial();
            #[allow(clippy::cast_possible_truncation)]
            let time_ms = std::time::Duration::from(self.clock.now()).as_millis() as u32;
            ptr.motion(self, under, &MotionEvent { location: loc, serial, time: time_ms });
            ptr.frame(self);
        }

        self.schedule_render();
    }

    fn unlock(&mut self) {
        // Never let an external session-lock client clear the lock state while
        // the native locker owns the session (defence in depth — `lock()` above
        // already refuses to confirm such a client).
        #[cfg(feature = "lock")]
        if self.native_lock.is_some() {
            return;
        }

        self.locked = false;
        self.lock_surfaces.clear();
        #[cfg(feature = "hooks")]
        crate::features::hooks::unlock(self);

        // Restore keyboard focus to the previously focused window (or clear it
        // if there is none).
        if let Some(kb) = self.keyboard.clone() {
            let surface = self
                .focused_window()
                .and_then(|w| w.wl_surface().map(std::borrow::Cow::into_owned));
            kb.set_focus(self, surface, SERIAL_COUNTER.next_serial());
        }

        // Restore pointer focus.
        if let Some(ptr) = self.pointer.clone() {
            let loc = ptr.current_location();
            let under = self.pointer_focus_under(loc);
            let serial = SERIAL_COUNTER.next_serial();
            #[allow(clippy::cast_possible_truncation)]
            let time_ms = std::time::Duration::from(self.clock.now()).as_millis() as u32;
            ptr.motion(self, under, &MotionEvent { location: loc, serial, time: time_ms });
            ptr.frame(self);
        }

        self.schedule_render();
    }

    fn new_surface(&mut self, surface: LockSurface, output: WlOutput) {
        // If the native locker owns the session, ignore surfaces from an
        // external client — it never received a confirmed lock, so its surfaces
        // must not be tracked, focused, or rendered over the native lock.
        #[cfg(feature = "lock")]
        if self.native_lock.is_some() {
            return;
        }

        // Resolve WlOutput → smithay Output to determine the output's size.
        let smithay_out = Output::from_resource(&output);

        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let size: smithay::utils::Size<u32, smithay::utils::Logical> = smithay_out
            .as_ref()
            .and_then(Output::current_mode)
            .map_or_else(
                || smithay::utils::Size::from((1920_u32, 1080_u32)),
                |m| {
                    let scale = smithay_out
                        .as_ref()
                        .map_or(1.0, |o| o.current_scale().fractional_scale());
                    let w = (f64::from(m.size.w) / scale).round() as u32;
                    let h = (f64::from(m.size.h) / scale).round() as u32;
                    smithay::utils::Size::from((w, h))
                },
            );

        // Set the size before smithay sends the initial configure (which happens
        // right after new_surface() returns).
        surface.with_pending_state(|state| {
            state.size = Some(size);
        });

        // Track (surface, output) so the render path can draw it on the right output.
        let assoc_output = smithay_out
            .or_else(|| self.monitors.first().map(|m| m.output.clone()));
        if let Some(out) = assoc_output {
            self.lock_surfaces.push((surface.clone(), out));
        }

        // Give keyboard focus to this lock surface.
        if let Some(kb) = self.keyboard.clone() {
            kb.set_focus(
                self,
                Some(surface.wl_surface().clone()),
                SERIAL_COUNTER.next_serial(),
            );
        }
    }

    fn ack_configure(&mut self, _surface: WlSurface, _configure: LockSurfaceConfigure) {}
}
