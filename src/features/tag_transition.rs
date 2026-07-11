//! Smooth horizontal slide animation between tags.
//!
//! On every tag switch two animations run simultaneously:
//!  - Old-tag windows slide OUT in the direction of travel (kept mapped until done).
//!  - New-tag windows slide IN from the opposite side.
//!
//! Compiled only when the `tag-transition` Cargo feature is enabled.

use crate::window::{with_state, with_state_mut};

/// Advance all in-progress tag slide animations by one frame.
///
/// Updates `slide_offset_x` on every sliding window and bumps
/// `slide_damage_counter` to force a damage-tracker redraw.
///
/// Returns `true` if any window is still mid-slide (caller should then call
/// `schedule_render` and `finalize_slide_outs` to keep the loop alive and to
/// clean up completed slide-outs).
#[allow(clippy::cast_precision_loss)]
pub fn advance_tag_transitions(state: &crate::state::Rwl) -> bool {
    let (enabled, duration_ms) = {
        let cfg = crate::config::get();
        (cfg.tag_transition, cfg.tag_transition_ms)
    };
    if !enabled || duration_ms == 0 {
        return false;
    }

    let now = std::time::Instant::now();
    let mut any_active = false;

    // space.elements() includes slide-out windows because arrange() skips their unmap.
    for w in state.space.elements() {
        let mut still_sliding = false;
        with_state_mut(w, |s| {
            let Some(start) = s.slide_start else { return };
            let elapsed_ms = now.duration_since(start).as_secs_f32() * 1000.0;
            let t = (elapsed_ms / duration_ms as f32).min(1.0);
            // Cubic ease-out: fast initial movement, soft landing.
            let t_eased = 1.0 - (1.0 - t).powi(3);
            s.slide_offset_x = (s.slide_to_x - s.slide_from_x).mul_add(f64::from(t_eased), s.slide_from_x);
            s.slide_damage_counter.increment();
            if t >= 1.0 {
                s.slide_start = None;
                if s.slide_out {
                    // Keep slide_offset_x at slide_to_x (off-screen) and slide_out=true
                    // as a "needs unmap" marker; finalize_slide_outs() handles cleanup.
                } else {
                    s.slide_offset_x = 0.0;
                }
            } else {
                still_sliding = true;
            }
        });
        if still_sliding {
            any_active = true;
        }
    }

    any_active
}

/// Mark old-tag windows for slide-out BEFORE `arrange_all()` is called.
///
/// Windows on `old_tag` that are NOT on `new_tags` are marked with
/// `slide_out = true` so `arrange()` skips their `space.unmap_elem` call,
/// keeping them rendered during the animation.
///
/// `finalize_slide_outs()` on `Rwl` unmaps them once the animation finishes.
pub fn mark_slide_outs(
    windows: &[smithay::desktop::Window],
    old_tag: u32,
    new_tags: u32,
    output_w: f64,
) {
    let new_tags = new_tags & crate::config::tag_mask();
    if old_tag == 0 || new_tags == 0 || new_tags == old_tag {
        return;
    }

    let cfg = crate::config::get();
    if !cfg.tag_transition || cfg.tag_transition_ms == 0 {
        return;
    }
    drop(cfg);

    let old_pos = old_tag.trailing_zeros();
    let new_pos = new_tags.trailing_zeros();
    // New tag is to the right → old slides left (negative x); vice versa.
    let to_x = if new_pos >= old_pos { -output_w } else { output_w };

    let now = std::time::Instant::now();
    for w in windows {
        let w_tags = with_state(w, |s| s.tags).unwrap_or(0);
        if w_tags & old_tag != 0 && w_tags & new_tags == 0 {
            with_state_mut(w, |s| {
                s.slide_from_x = 0.0;
                s.slide_to_x = to_x;
                s.slide_offset_x = 0.0;
                s.slide_out = true;
                s.slide_start = Some(now);
                s.slide_damage_counter.increment();
            });
        }
    }
}

/// Kick off slide-in animations for windows that just became visible after a
/// tag switch from `old_tag` to `new_tag_raw`.  Call AFTER `arrange_all()`.
///
/// Direction: higher tag index → windows enter from the right; lower → left.
pub fn start_tag_transition(
    windows: &[smithay::desktop::Window],
    old_tag: u32,
    new_tag_raw: u32,
    output_w: f64,
) {
    let new_tags = new_tag_raw & crate::config::tag_mask();
    if old_tag == 0 || new_tags == 0 || new_tags == old_tag {
        return;
    }

    let cfg = crate::config::get();
    if !cfg.tag_transition || cfg.tag_transition_ms == 0 {
        return;
    }
    drop(cfg);

    let old_pos = old_tag.trailing_zeros();
    let new_pos = new_tags.trailing_zeros();
    let from_x = if new_pos >= old_pos { output_w } else { -output_w };

    let now = std::time::Instant::now();
    for w in windows {
        let w_tags = with_state(w, |s| s.tags).unwrap_or(0);
        if w_tags & new_tags != 0 && w_tags & old_tag == 0 {
            with_state_mut(w, |s| {
                // Don't overwrite an in-progress slide-out on the same window
                // (can't be on both old and new exclusively, but guard anyway).
                if s.slide_out {
                    return;
                }
                // Skip the slide-in for windows that have not yet committed a
                // buffer.  Animating them from off-screen would leave a grey
                // rectangle at their layout position for the entire animation
                // duration while the client initialises; they appear directly at
                // the correct position instead and render normally once ready.
                if !s.buffer_mapped {
                    return;
                }
                s.slide_from_x = from_x;
                s.slide_to_x = 0.0;
                s.slide_offset_x = from_x;
                s.slide_start = Some(now);
                s.slide_damage_counter.increment();
            });
        }
    }
}
