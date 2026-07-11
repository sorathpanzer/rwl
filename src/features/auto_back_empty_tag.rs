//! Auto-navigate away from a tag when it becomes empty after a window closes.
//! Compiled only when the `auto-back-empty-tag` Cargo feature is enabled.

use crate::window::with_state;

/// Called after a window is removed from the space.
///
/// If `mon_idx`'s current view is now empty, switches to the most recently
/// occupied tag (history → previous slot → any tag with windows).
pub fn on_window_closed(
    state: &mut crate::state::Rwl,
    mon_idx: usize,
    current_tags: u32,
    switch_to_tag: Option<u32>,
) {
    if current_tags == 0 || !crate::config::get().auto_back_empty_tag {
        return;
    }

    let tag_has_windows = state.windows.iter().any(|w| {
        let same_mon = with_state(w, |s| s.mon_idx).unwrap_or(0) == mon_idx;
        let overlapping = with_state(w, |s| s.tags & current_tags).unwrap_or(0) != 0;
        same_mon && overlapping
    });
    if tag_has_windows {
        return;
    }

    // Remove the now-empty tag from history so we don't navigate back to it.
    if let Some(history) = state.tag_history.get_mut(mon_idx) {
        history.retain(|&t| t != current_tags);
    }

    // 0. Prefer the tag recorded by switch_to_tag (the view we left when this
    //    window was spawned) if it still has windows on this monitor.
    let target = switch_to_tag.filter(|&prev| {
        prev != current_tags
            && state.windows.iter().any(|w| {
                let same_mon = with_state(w, |s| s.mon_idx).unwrap_or(0) == mon_idx;
                let overlapping = with_state(w, |s| s.tags & prev).unwrap_or(0) != 0;
                same_mon && overlapping
            })
    });

    // 1. Most recently visited tag (from history) that still has windows.
    let target = target.or_else(|| {
        state.tag_history.get(mon_idx).and_then(|history| {
            history.iter().rev().copied().find(|&past_tags| {
                state.windows.iter().any(|w| {
                    let same_mon = with_state(w, |s| s.mon_idx).unwrap_or(0) == mon_idx;
                    let overlapping = with_state(w, |s| s.tags & past_tags).unwrap_or(0) != 0;
                    same_mon && overlapping
                })
            })
        })
    });

    // 2. Monitor's internal "previous" slot (covers ViewPrev without history entry).
    let target = target.or_else(|| {
        state.monitors.get(mon_idx).and_then(|m| {
            let prev = m.tagset[m.sel_tags ^ 1];
            if prev == 0 || prev == current_tags {
                return None;
            }
            let has_windows = state.windows.iter().any(|w| {
                let same_mon = with_state(w, |s| s.mon_idx).unwrap_or(0) == mon_idx;
                let overlapping = with_state(w, |s| s.tags & prev).unwrap_or(0) != 0;
                same_mon && overlapping
            });
            if has_windows { Some(prev) } else { None }
        })
    });

    // 3. Ultimate fallback: any tag containing a window on this monitor.
    let target = target.or_else(|| {
        state.windows.iter().find_map(|w| {
            if with_state(w, |s| s.mon_idx).unwrap_or(0) != mon_idx {
                return None;
            }
            let t = with_state(w, |s| s.tags).unwrap_or(0);
            if t != 0 { Some(t) } else { None }
        })
    });

    let Some(target_tags) = target else { return };

    // Direct slot assignment (not view()) so the empty tag is not stored as
    // the new "previous" and ViewPrev doesn't return to it.
    if let Some(m) = state.monitors.get_mut(mon_idx) {
        m.tagset[m.sel_tags] = target_tags;
        #[cfg(feature = "pertag-layouts")]
        crate::features::pertag_layouts::apply(m, target_tags);
    }

    #[cfg(feature = "tag-transition")]
    let output_w = state.monitors.get(mon_idx).map_or(1920.0, |m| f64::from(m.w.size.w));
    #[cfg(feature = "tag-transition")]
    crate::features::tag_transition::mark_slide_outs(&state.windows, current_tags, target_tags, output_w);

    // A tag switch happened mid-repeat: cancel the key-repeat timer so
    // held KillClient doesn't start closing windows on the new tag.
    if let Some(token) = state.key_repeat_timer.take() {
        state.loop_handle.remove(token);
    }
    state.key_repeat_action = None;
    state.arrange_all();

    #[cfg(feature = "tag-transition")]
    crate::features::tag_transition::start_tag_transition(&state.windows, current_tags, target_tags, output_w);

    let top = state.focused_window().cloned();
    state.focus_window(top);
}
