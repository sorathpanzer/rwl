//! Interactive window grab (move / resize).

pub mod move_grab;
pub mod resize_grab;

use smithay::desktop::Window;
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::ResizeEdge;
use smithay::utils::{Logical, Point};

use crate::state::{CursorMode, Rwl};
use crate::window::{window_is_floating, with_state};
#[cfg(feature = "col")]
use crate::window::{window_is_fullscreen, window_visible_on};

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Return the index of `window` in the ordered tiled-windows list for its
/// monitor (same ordering that `arrange()` uses), or `None` if not found.
#[cfg(feature = "col")]
fn col_window_idx(state: &Rwl, window: &Window, mon_idx: usize) -> Option<usize> {
    let tags = state.monitors.get(mon_idx)?.tags();
    state
        .windows
        .iter()
        .filter(|w| {
            window_visible_on(w, tags)
                && !window_is_floating(w)
                && !window_is_fullscreen(w)
                && with_state(w, |s| s.rules_applied).unwrap_or(false)
        })
        .position(|w| w == window)
}

// ---------------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------------

/// Begin an interactive move of `window`.
pub fn start_move(state: &mut Rwl, window: Window) {
    let Some(pointer) = state.pointer.clone() else {
        return;
    };
    let loc = pointer.current_location();
    let _win_loc = state.space.element_location(&window).unwrap_or_default();

    // Record the original floating state so end_grab can restore it if needed.
    state.grab_was_floating = window_is_floating(&window);

    // Temporarily float the window so arrange_all() does not snap it back to
    // its tiled position while the drag is in progress.
    crate::window::with_state_mut(&window, |s| s.is_floating = true);

    state.grabbed_window = Some(window.clone());
    state.grab_start = Some(loc);
    state.grab_geom = state.space.element_geometry(&window);
    state.cursor_mode = CursorMode::Move;

    // Lift window to top
    state.focus_window(Some(window));
}

/// Begin an interactive resize of `window`.
pub fn start_resize(state: &mut Rwl, window: Window, _edges: ResizeEdge) {
    let Some(pointer) = state.pointer.clone() else {
        return;
    };
    let loc = pointer.current_location();

    if window_is_floating(&window) {
        // Floating window: keep it floating, resize directly.
        state.grab_mfact = None;
        #[cfg(feature = "col")]
        { state.grab_col_idx = None; state.grab_col_facts = None; }
    } else {
        #[cfg(any(feature = "tile", feature = "col"))]
        use crate::config::LayoutKind;
        let mon_idx = with_state(&window, |s| s.mon_idx).unwrap_or(0);
        match state.monitors.get(mon_idx).map(super::monitor::Monitor::layout_kind) {
            #[cfg(feature = "tile")]
            Some(LayoutKind::Tile) => {
                // Tile: record mfact so handle_resize can compute a delta.
                state.grab_mfact = state.monitors.get(mon_idx).map(|m| m.mfact);
                #[cfg(feature = "col")]
                { state.grab_col_idx = None; state.grab_col_facts = None; }
            }
            #[cfg(feature = "col")]
            Some(LayoutKind::Col) => {
                // Col: record the column index and a snapshot of col_facts.
                state.grab_mfact = None;
                let col_idx = col_window_idx(state, &window, mon_idx);
                state.grab_col_idx = col_idx;
                // Extract owned data from the monitor via .get() so a stale
                // mon_idx (e.g. after a hotplug disconnect) cannot panic.
                let mon_data = state.monitors.get(mon_idx)
                    .map(|mon| (mon.tags(), mon.col_facts.clone()));
                state.grab_col_facts = Some(if let Some((tags, existing)) = mon_data {
                    let n = state.windows.iter().filter(|w| {
                        window_visible_on(w, tags)
                            && !window_is_floating(w)
                            && !window_is_fullscreen(w)
                            && with_state(w, |s| s.rules_applied).unwrap_or(false)
                    }).count();
                    if existing.len() == n && n > 0 {
                        existing
                    } else if n > 0 {
                        #[allow(clippy::cast_precision_loss)]
                        let frac = 1.0 / n as f64;
                        vec![frac; n]
                    } else {
                        Vec::new()
                    }
                } else {
                    Vec::new()
                });
            }
            _ => {
                state.grab_mfact = None;
                #[cfg(feature = "col")]
                { state.grab_col_idx = None; state.grab_col_facts = None; }
            }
        }
    }

    state.grabbed_window = Some(window.clone());
    state.grab_start = Some(loc);
    state.grab_geom = state.space.element_geometry(&window);
    state.cursor_mode = CursorMode::Resize;

    state.focus_window(Some(window));
}

/// Update position of the grabbed window during a move.
#[allow(clippy::cast_possible_truncation)]
pub fn handle_move(state: &mut Rwl, new_pointer_loc: Point<f64, Logical>) {
    let (Some(window), Some(start), Some(orig_geom)) = (
        state.grabbed_window.clone(),
        state.grab_start,
        state.grab_geom,
    ) else {
        return;
    };

    let dx = (new_pointer_loc.x - start.x) as i32;
    let dy = (new_pointer_loc.y - start.y) as i32;
    let new_loc = (orig_geom.loc.x + dx, orig_geom.loc.y + dy);
    state.space.map_element(window, new_loc, true);
}

/// Update size of the grabbed window during a resize.
#[allow(clippy::cast_possible_truncation)]
pub fn handle_resize(state: &mut Rwl, new_pointer_loc: Point<f64, Logical>) {
    let (Some(window), Some(start), Some(orig_geom)) = (
        state.grabbed_window.clone(),
        state.grab_start,
        state.grab_geom,
    ) else {
        return;
    };

    if crate::window::window_is_floating(&window) {
        // Floating window: resize it directly by sending a new size configure.
        let dx = (new_pointer_loc.x - start.x) as i32;
        let dy = (new_pointer_loc.y - start.y) as i32;
        let new_w = (orig_geom.size.w + dx).max(1);
        let new_h = (orig_geom.size.h + dy).max(1);

        if let Some(tl) = window.toplevel() {
            tl.with_pending_state(|s| {
                s.size = Some((new_w, new_h).into());
            });
            tl.send_pending_configure();
        }
    } else {
        #[cfg(any(feature = "tile", feature = "col"))]
        use crate::config::LayoutKind;
        let mon_idx = with_state(&window, |s| s.mon_idx).unwrap_or(0);
        let Some(mon) = state.monitors.get(mon_idx) else { return };
        let kind = mon.layout_kind();
        let area_w = f64::from(mon.w.size.w);
        if area_w <= 0.0 {
            return;
        }
        #[cfg(any(feature = "tile", feature = "col"))]
        let dx = new_pointer_loc.x - start.x;

        match kind {
            #[cfg(feature = "tile")]
            LayoutKind::Tile => {
                // Adjust the master/stack split proportionally to the horizontal drag.
                let base_mfact = state.grab_mfact.unwrap_or(mon.mfact);
                let new_mfact = (base_mfact + dx / area_w).clamp(0.05, 0.95);
                if let Some(m) = state.monitors.get_mut(mon_idx) {
                    m.mfact = new_mfact;
                }
                state.arrange(mon_idx);
            }
            #[cfg(feature = "col")]
            LayoutKind::Col => {
                // Move the right boundary of the grabbed column: grow it and
                // shrink its right neighbour by the same fractional amount.
                let Some(col_idx) = state.grab_col_idx else { return };
                let snap = match state.grab_col_facts.as_deref() {
                    Some(s) if !s.is_empty() => s,
                    _ => return,
                };
                let n = snap.len();
                // Can only drag the boundary between col_idx and col_idx+1.
                if col_idx + 1 >= n {
                    return;
                }
                let total = snap[col_idx] + snap[col_idx + 1];
                // Clamp delta so neither column falls below 5% of screen width.
                let min_frac = 0.05_f64.min(total / 2.0);
                let delta = (dx / area_w)
                    .clamp(min_frac - snap[col_idx], snap[col_idx + 1] - min_frac);
                let new_left = snap[col_idx] + delta;
                let new_right = total - new_left; // sum preserved exactly
                let new_facts: Vec<f64> = snap.iter().copied().enumerate()
                    .map(|(j, f)| if j == col_idx { new_left } else if j == col_idx + 1 { new_right } else { f })
                    .collect();
                if let Some(m) = state.monitors.get_mut(mon_idx) {
                    m.col_facts = new_facts;
                }
                state.arrange(mon_idx);
            }
            // Catches Monocle / scrollable / fallback layouts (and is
            // unreachable when only Tile/Col are compiled in).
            #[allow(unreachable_patterns)]
            _ => {} // ignore resize request
        }
    }
}

/// End any active grab.
pub fn end_grab(state: &mut Rwl) {
    // If a tiled window was temporarily floated for a move drag, restore it to
    // tiled on release so the drag does not permanently change the layout.
    let restore_tiled = matches!(state.cursor_mode, CursorMode::Move)
        && !state.grab_was_floating;
    if restore_tiled
        && let Some(w) = &state.grabbed_window
    {
        crate::window::with_state_mut(w, |s| s.is_floating = false);
    }

    state.grabbed_window = None;
    state.grab_start = None;
    state.grab_geom = None;
    state.grab_mfact = None;
    #[cfg(feature = "col")]
    { state.grab_col_idx = None; state.grab_col_facts = None; }
    state.grab_was_floating = false;
    state.cursor_mode = CursorMode::Normal;

    if restore_tiled {
        state.arrange_all();
    }
}
