//! Cursor-warp feature: move the pointer to the focused window after keyboard
//! focus changes.  Compiled only when the `warp` Cargo feature is enabled.

use smithay::input::pointer::MotionEvent;
use smithay::utils::{Logical, Point, SERIAL_COUNTER};

use crate::window::with_state;

/// Move the pointer to the centre of the currently focused window.
///
/// Called after keyboard-initiated focus changes so the pointer follows focus
/// (mirrors C dwl's `warpcursor` behaviour).
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
pub fn warp_cursor_to_focused(state: &mut crate::state::Rwl) {
    if !crate::config::get().warp_cursor {
        return;
    }
    // Treat a warp as cursor activity so the inactivity hide-timer is reset;
    // otherwise switching focus via keyboard just before the timeout causes the
    // cursor to hide immediately after warping.
    state.handle_cursor_activity();

    let Some(pointer) = state.pointer.clone() else {
        return;
    };

    let center: Point<f64, Logical> = match state.focused_window().cloned() {
        Some(window) => {
            // Prefer pending_geom (the geometry sent in the last configure) over
            // element_geometry (which uses the client's acked size).
            let geom = with_state(&window, |s| s.pending_geom)
                .flatten()
                .or_else(|| state.space.element_geometry(&window));
            let Some(geom) = geom else { return };
            (
                f64::from(geom.loc.x) + f64::from(geom.size.w) / 2.0,
                f64::from(geom.loc.y) + f64::from(geom.size.h) / 2.0,
            )
                .into()
        }
        // Empty tag: warp to the centre of the selected monitor.
        None => match state.monitors.get(state.sel_mon) {
            Some(mon) => (
                f64::from(mon.m.loc.x) + f64::from(mon.m.size.w) / 2.0,
                f64::from(mon.m.loc.y) + f64::from(mon.m.size.h) / 2.0,
            )
                .into(),
            None => return,
        },
    };

    let under = state.pointer_focus_under(center);

    let serial = SERIAL_COUNTER.next_serial();
    // Use the compositor's monotonic clock so the timestamp is consistent with
    // all other input event timestamps (which come from the same source).
    #[allow(clippy::cast_possible_truncation)]
    let time_ms: u32 = std::time::Duration::from(state.clock.now()).as_millis() as u32;

    pointer.motion(
        state,
        under,
        &MotionEvent {
            location: center,
            serial,
            time: time_ms,
        },
    );
    pointer.frame(state);
}
