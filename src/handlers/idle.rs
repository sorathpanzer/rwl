//! Idle-inhibit and idle-notify protocol handlers.
//!
//! `zwp_idle_inhibit_manager_v1` lets clients (video players, presentations)
//! suppress the idle timer.  When any live inhibitor surface exists the idle
//! notifier is told to hold off; when the last one is destroyed it resumes.
//!
//! `ext_idle_notify_v1` lets clients (swayidle, hypridle, bars) register
//! timeouts and receive `idled` / `resumed` events.  Activity is reported from
//! the input handler via [`crate::state::Rwl::notify_idle_activity`].

use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::wayland::idle_inhibit::IdleInhibitHandler;
use smithay::wayland::idle_notify::IdleNotifierHandler;
use smithay::wayland::idle_notify::IdleNotifierState;

use crate::state::Rwl;

impl IdleInhibitHandler for Rwl {
    fn inhibit(&mut self, surface: WlSurface) {
        self.inhibited_surfaces.insert(surface);
        self.idle_notifier.set_is_inhibited(true);
    }

    fn uninhibit(&mut self, surface: WlSurface) {
        self.inhibited_surfaces.remove(&surface);
        // Clean up any dead surfaces while we're here.
        self.inhibited_surfaces
            .retain(smithay::reexports::wayland_server::Resource::is_alive);
        if self.inhibited_surfaces.is_empty() {
            self.idle_notifier.set_is_inhibited(false);
        }
    }
}

impl IdleNotifierHandler for Rwl {
    fn idle_notifier_state(&mut self) -> &mut IdleNotifierState<Self> {
        &mut self.idle_notifier
    }
}
