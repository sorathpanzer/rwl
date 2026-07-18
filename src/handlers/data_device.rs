//! Additional Wayland protocol handler delegations.
//!
//! Fractional scale, XDG activation, DMA-BUF.

use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::wayland::dmabuf::{DmabufGlobal, DmabufHandler, DmabufState, ImportNotifier};
use smithay::wayland::fractional_scale::FractionalScaleHandler;
use smithay::wayland::xdg_activation::{
    XdgActivationHandler, XdgActivationState, XdgActivationToken, XdgActivationTokenData,
};

use crate::state::Rwl;

// ---------------------------------------------------------------------------
// Fractional scale
// ---------------------------------------------------------------------------

impl FractionalScaleHandler for Rwl {
    fn new_fractional_scale(&mut self, surface: WlSurface) {
        let _ = surface;
    }
}

// ---------------------------------------------------------------------------
// XDG Activation (focus stealing / app launching)
// ---------------------------------------------------------------------------

impl XdgActivationHandler for Rwl {
    fn activation_state(&mut self) -> &mut XdgActivationState {
        &mut self.xdg_activation_state
    }

    fn token_created(&mut self, _token: XdgActivationToken, data: XdgActivationTokenData) -> bool {
        // Only honour tokens that carry a user-input serial (keyboard/pointer
        // event that triggered the activation).  Electron-based apps (Chromium,
        // Signal, WhatsApp) create tokens *without* a serial for self-activation
        // on startup and for notification badges; rejecting those here prevents
        // them from appearing as urgent.  Tokens created by a launcher (e.g.
        // foot running `chromium`) DO carry the serial of the key-press and are
        // accepted normally.
        data.serial.is_some()
    }

    fn request_activation(
        &mut self,
        token: XdgActivationToken,
        _token_data: XdgActivationTokenData,
        surface: WlSurface,
    ) {
        // Consume the token immediately — prevents the same token from firing
        // request_activation again if the client calls activate() more than once.
        self.xdg_activation_state.remove_token(&token);

        if let Some(w) = self.window_for_surface(&surface).cloned() {
            // Mirror dwl.c urgent(): skip if this window is already focused.
            if self.focus_stack.first() == Some(&w) {
                return;
            }
            // Mark urgent so borders/IPC reflect it, then immediately switch to
            // the window's monitor+tags and focus it (mirrors dwl.c urgent()).
            crate::window::set_urgent(&w, true);
            #[cfg(feature = "hooks")]
            crate::features::hooks::urgency(self, &w);
            #[cfg(feature = "ipc")]
            crate::features::ipc::event::urgency(self, &w);
            self.focus_urgent();
        }
    }
}

// ---------------------------------------------------------------------------
// DMA-BUF
// ---------------------------------------------------------------------------

impl DmabufHandler for Rwl {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.dmabuf_state
    }

    fn dmabuf_imported(
        &mut self,
        _global: &DmabufGlobal,
        _dmabuf: smithay::backend::allocator::dmabuf::Dmabuf,
        notifier: ImportNotifier,
    ) {
        let _ = notifier.successful::<Self>();
    }
}
