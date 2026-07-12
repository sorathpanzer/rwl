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
        state.grab_cfact = None;
        #[cfg(feature = "col")]
        { state.grab_col_idx = None; state.grab_col_facts = None; }
    } else {
        let mon_idx = with_state(&window, |s| s.mon_idx).unwrap_or(0);
        match state.monitors.get(mon_idx).map(super::monitor::Monitor::layout_kind) {
            #[cfg(feature = "col")]
            Some(crate::config::LayoutKind::Col) => {
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
            // All other tiled layouts resize via the master/stack split (mfact)
            // and/or the grabbed window's per-window size factor (cfact).
            _ => {
                state.grab_mfact = state.monitors.get(mon_idx).map(|m| m.mfact);
                #[cfg(feature = "col")]
                { state.grab_col_idx = None; state.grab_col_facts = None; }
            }
        }
        // Snapshot the grabbed window's size factor so drag deltas apply relative
        // to a fixed base rather than compounding on every pointer motion.
        state.grab_cfact = with_state(&window, |s| s.cfact);
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
#[allow(clippy::cast_possible_truncation, clippy::too_many_lines)]
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
        #[cfg(any(
            feature = "tile",
            feature = "col",
            feature = "scroll",
            feature = "dwindle",
            feature = "bstack",
            feature = "centeredmaster",
        ))]
        {
            let mon_idx = with_state(&window, |s| s.mon_idx).unwrap_or(0);
            if state.monitors.get(mon_idx).is_none() {
                return;
            }
            match state.monitors[mon_idx].layout_kind() {
                #[cfg(feature = "tile")]
                crate::config::LayoutKind::Tile => {
                    // Horizontal drag → master/stack split; vertical drag → the
                    // grabbed window's height factor within its column.
                    let (base_mfact, area_w) = {
                        let m = &state.monitors[mon_idx];
                        (state.grab_mfact.unwrap_or(m.mfact), f64::from(m.w.size.w))
                    };
                    if area_w > 0.0 {
                        let dx = new_pointer_loc.x - start.x;
                        state.monitors[mon_idx].mfact = (base_mfact + dx / area_w).clamp(0.05, 0.95);
                    }
                    let dy = new_pointer_loc.y - start.y;
                    apply_cfact(state, &window, orig_geom.size.h, dy);
                    state.arrange(mon_idx);
                }
                #[cfg(feature = "bstack")]
                crate::config::LayoutKind::Bstack => {
                    // Master/stack split runs top/bottom, so vertical drag moves
                    // it; the stack tiles sideways, so horizontal drag → cfact.
                    let (base_mfact, area_h) = {
                        let m = &state.monitors[mon_idx];
                        (state.grab_mfact.unwrap_or(m.mfact), f64::from(m.w.size.h))
                    };
                    if area_h > 0.0 {
                        let dy = new_pointer_loc.y - start.y;
                        state.monitors[mon_idx].mfact = (base_mfact + dy / area_h).clamp(0.05, 0.95);
                    }
                    let dx = new_pointer_loc.x - start.x;
                    apply_cfact(state, &window, orig_geom.size.w, dx);
                    state.arrange(mon_idx);
                }
                #[cfg(feature = "centeredmaster")]
                crate::config::LayoutKind::CenteredMaster => {
                    // Horizontal drag → centre-master width; vertical → cfact.
                    let (base_mfact, area_w) = {
                        let m = &state.monitors[mon_idx];
                        (state.grab_mfact.unwrap_or(m.mfact), f64::from(m.w.size.w))
                    };
                    if area_w > 0.0 {
                        let dx = new_pointer_loc.x - start.x;
                        state.monitors[mon_idx].mfact = (base_mfact + dx / area_w).clamp(0.05, 0.95);
                    }
                    let dy = new_pointer_loc.y - start.y;
                    apply_cfact(state, &window, orig_geom.size.h, dy);
                    state.arrange(mon_idx);
                }
                #[cfg(feature = "col")]
                crate::config::LayoutKind::Col => {
                    // Move the right boundary of the grabbed column: grow it and
                    // shrink its right neighbour by the same fractional amount.
                    let area_w = f64::from(state.monitors[mon_idx].w.size.w);
                    if area_w <= 0.0 {
                        return;
                    }
                    let dx = new_pointer_loc.x - start.x;
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
                #[cfg(feature = "scroll")]
                crate::config::LayoutKind::Scroll => {
                    // One window per full-height column: horizontal drag sets the
                    // shared column width (mfact).
                    let (base_mfact, area_w) = {
                        let m = &state.monitors[mon_idx];
                        (state.grab_mfact.unwrap_or(m.mfact), f64::from(m.w.size.w))
                    };
                    if area_w > 0.0 {
                        let dx = new_pointer_loc.x - start.x;
                        state.monitors[mon_idx].mfact = (base_mfact + dx / area_w).clamp(0.05, 0.95);
                        state.arrange(mon_idx);
                    }
                }
                #[cfg(feature = "dwindle")]
                crate::config::LayoutKind::Dwindle => {
                    // Horizontal drag adjusts the master (first) split fraction.
                    let (base_mfact, area_w) = {
                        let m = &state.monitors[mon_idx];
                        (state.grab_mfact.unwrap_or(m.mfact), f64::from(m.w.size.w))
                    };
                    if area_w > 0.0 {
                        let dx = new_pointer_loc.x - start.x;
                        state.monitors[mon_idx].mfact = (base_mfact + dx / area_w).clamp(0.05, 0.95);
                        state.arrange(mon_idx);
                    }
                }
                // Monocle (full-screen overlay) and the fallback layout have
                // nothing resizable.
                #[allow(unreachable_patterns)]
                _ => {}
            }
        }
    }
}

/// Apply a per-window size factor (`cfact`) from an interactive resize drag.
///
/// The grabbed window originally spanned `orig_len` px along the drag axis and
/// the pointer has moved `delta` px along it.  Scaling the base factor by
/// `(orig_len + delta) / orig_len` grows/shrinks the window roughly in step with
/// the pointer while its column/row neighbours re-flow proportionally.  The
/// result is clamped to `[0.25, 4.0]` (matching dwm's cfact range).
#[cfg(any(feature = "tile", feature = "bstack", feature = "centeredmaster"))]
fn apply_cfact(state: &Rwl, window: &Window, orig_len: i32, delta: f64) {
    let orig = f64::from(orig_len);
    if orig <= 0.0 {
        return;
    }
    let base = state.grab_cfact.unwrap_or(1.0);
    let new = (base * (orig + delta) / orig).clamp(0.25, 4.0);
    crate::window::with_state_mut(window, |s| s.cfact = new);
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
    state.grab_cfact = None;
    #[cfg(feature = "col")]
    { state.grab_col_idx = None; state.grab_col_facts = None; }
    state.grab_was_floating = false;
    state.cursor_mode = CursorMode::Normal;

    if restore_tiled {
        state.arrange_all();
    }
}
