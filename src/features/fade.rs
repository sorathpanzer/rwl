//! Per-window fade-in / fade-out animations.  Compiled only when the `fade`
//! Cargo feature is enabled.

use crate::window::with_state_mut;

/// Advance per-window fade-in / fade-out animations by one frame.
///
/// Updates `fade_alpha` on every fading window and bumps
/// `fade_damage_counter` to force a damage-tracker redraw even when the
/// surface itself has not committed a new buffer.
///
/// Returns `true` if any window is still mid-fade (caller should schedule
/// another render to continue the animation at the display refresh rate).
#[allow(clippy::cast_precision_loss)]
pub fn advance_fades(state: &crate::state::Rwl) -> bool {
    let (fade_in_ms, fade_out_ms) = {
        let cfg = crate::config::get();
        (cfg.fade_in_ms, cfg.fade_out_ms)
    };
    if fade_in_ms == 0 && fade_out_ms == 0 {
        return false;
    }

    let now = std::time::Instant::now();
    let mut any_active = false;

    for w in state.space.elements() {
        let mut still_fading = false;
        with_state_mut(w, |s| {
            let Some(start) = s.fade_start else { return };
            let elapsed_ms = now.duration_since(start).as_secs_f32() * 1000.0;
            if s.fading_out {
                let duration = fade_out_ms as f32;
                // A completed fade-out that is NOT followed by the window's
                // destruction means the client declined to close (e.g. it popped
                // up a "save changes?" dialog).  advance_fades only iterates
                // still-mapped windows — a genuinely closing window is removed by
                // toplevel_destroyed before the animation ends — so any window
                // still here at t >= 1.0 has survived the close request.  Restore
                // it to fully visible instead of leaving it stuck at alpha 0
                // (invisible but still focusable) forever.
                let t = if duration == 0.0 { 1.0 } else { (elapsed_ms / duration).min(1.0) };
                if t >= 1.0 {
                    s.fade_start = None;
                    s.fading_out = false;
                    s.fade_alpha = 1.0;
                } else {
                    s.fade_alpha = 1.0 - t;
                    still_fading = true;
                }
            } else {
                let duration = fade_in_ms as f32;
                if duration == 0.0 {
                    s.fade_alpha = 1.0;
                    s.fade_start = None;
                    return;
                }
                let t = (elapsed_ms / duration).min(1.0);
                s.fade_alpha = t;
                if t >= 1.0 {
                    s.fade_alpha = 1.0;
                    s.fade_start = None;
                } else {
                    still_fading = true;
                }
            }
            s.fade_damage_counter.increment();
        });
        if still_fading {
            any_active = true;
        }
    }

    any_active
}
