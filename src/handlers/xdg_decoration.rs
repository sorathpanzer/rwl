//! XDG decoration handler — force server-side decorations.

use smithay::reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode;
use smithay::wayland::shell::xdg::decoration::XdgDecorationHandler;
use smithay::wayland::shell::xdg::ToplevelSurface;

use crate::state::Rwl;

impl XdgDecorationHandler for Rwl {
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        // Always request server-side decorations.
        // Use arrange_all() so the configure includes the correct tiled size.
        // Calling send_configure() here would send size=None, overriding the
        // tiled size already queued by new_toplevel's arrange_all().
        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(Mode::ServerSide);
        });
        self.arrange_all();
    }

    fn request_mode(&mut self, toplevel: ToplevelSurface, _mode: Mode) {
        // Override client requests: always use server-side
        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(Mode::ServerSide);
        });
        self.arrange_all();
    }

    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(Mode::ServerSide);
        });
        self.arrange_all();
    }
}

