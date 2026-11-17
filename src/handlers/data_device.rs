//! Additional Wayland protocol handler delegations.
//!
//! Fractional scale, XDG activation, DMA-BUF.

use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::Resource;
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
        let _unused = surface;
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
        // Signal, WhatsApp) and Firefox create tokens *without* a serial for
        // self-activation on startup and for notification badges; rejecting
        // those here prevents them from appearing as urgent.
        let Some((serial, _)) = data.serial else { return false };

        // Decide *now* (at creation time, right after the click/keypress)
        // whether this token may steal focus.  It must:
        //   * come from the client that currently holds keyboard focus, and
        //   * carry a serial no older than the last keyboard enter, i.e. an
        //     input event the user made while that client was focused.
        // A terminal/chat app where you click a link passes both checks, so
        // the browser it hands the token to may switch tags and focus.
        // A background page in an unfocused browser (which reuses a stale
        // serial from the last time you interacted with it) fails, and its
        // activation degrades to an urgent hint.
        let focused_client = self
            .seat
            .get_keyboard()
            .and_then(|kb| {
                let fresh = kb.last_enter().is_some_and(|le| serial.is_no_older_than(&le));
                if fresh { kb.current_focus() } else { None }
            })
            .and_then(|surf| surf.client())
            .map(|c| c.id());
        if focused_client.is_some() && focused_client == data.client_id {
            data.user_data.insert_if_missing_threadsafe(|| FocusAllowed);
        }
        true
    }

    fn request_activation(
        &mut self,
        token: XdgActivationToken,
        token_data: XdgActivationTokenData,
        surface: WlSurface,
    ) {
        // Consume the token immediately — prevents the same token from firing
        // request_activation again if the client calls activate() more than once.
        self.xdg_activation_state.remove_token(&token);

        let Some(w) = self.window_for_surface(&surface).cloned() else { return };
        // Mirror dwl.c urgent(): skip if this window is already focused.
        if self.focus_stack.first() == Some(&w) {
            return;
        }
        let may_focus = token_data.user_data.get::<FocusAllowed>().is_some()
            && token_data.timestamp.elapsed() < ACTIVATION_TIMEOUT;

        // Mark urgent so borders/IPC reflect it.
        crate::window::set_urgent(&w, true);
        #[cfg(feature = "hooks")]
        crate::features::hooks::urgency(self, &w);
        #[cfg(feature = "ipc")]
        crate::features::ipc::event::urgency(self, &w);
        // Only a token backed by a real user action switches to the window's
        // monitor+tags and focuses it; anything else stays an urgent hint.
        if may_focus {
            self.focus_urgent();
        }
    }
}

/// Marker stored in a token's user data when it was created from a fresh
/// input event of the keyboard-focused client.
struct FocusAllowed;

/// Tokens older than this never steal focus (they only mark urgent).
const ACTIVATION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

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
        let _unused = notifier.successful::<Self>();
    }
}
