//! Tiling layout algorithms.
//!
//! Each layout lives behind its own Cargo feature and contributes a
//! `LayoutKind` variant plus an `arrange` function.  Every function receives
//! the number of tiled (non-floating, non-fullscreen) windows visible on the
//! monitor and returns a parallel `Vec` of geometries to apply, in tiling
//! order.

#[cfg(feature = "tile")]
pub mod tile;
#[cfg(feature = "monocle")]
pub mod monocle;
#[cfg(feature = "col")]
pub mod col;
#[cfg(feature = "scroll")]
pub mod scroll;
#[cfg(feature = "dwindle")]
pub mod dwindle;
#[cfg(feature = "bstack")]
pub mod bstack;
#[cfg(feature = "centeredmaster")]
pub mod centeredmaster;

use smithay::utils::{Logical, Rectangle};

use crate::config::LayoutKind;
use crate::monitor::Monitor;

/// Compute window geometries for all tiled windows on `monitor`.
///
/// `cfacts` holds one per-window size factor per tiled window, in tiling order
/// (see [`crate::window::WindowState::cfact`]); its length is the number of
/// windows to tile.  The returned `Vec` has the same length; each entry is the
/// geometry to apply to the corresponding window (in tiling order).  Layouts
/// that stack windows in a shared column/row weight each window's span by its
/// factor; single-window-per-column layouts ignore the values.
#[must_use]
pub fn arrange(monitor: &Monitor, cfacts: &[f64]) -> Vec<Rectangle<i32, Logical>> {
    if cfacts.is_empty() {
        return Vec::new();
    }

    match monitor.layout_kind() {
        #[cfg(feature = "tile")]
        LayoutKind::Tile    => tile::arrange(monitor, cfacts),
        #[cfg(feature = "monocle")]
        LayoutKind::Monocle => monocle::arrange(monitor, cfacts),
        #[cfg(feature = "col")]
        LayoutKind::Col     => col::arrange(monitor, cfacts),
        #[cfg(feature = "scroll")]
        LayoutKind::Scroll  => scroll::arrange(monitor, cfacts),
        #[cfg(feature = "dwindle")]
        LayoutKind::Dwindle => dwindle::arrange(monitor, cfacts),
        #[cfg(feature = "bstack")]
        LayoutKind::Bstack  => bstack::arrange(monitor, cfacts),
        #[cfg(feature = "centeredmaster")]
        LayoutKind::CenteredMaster => centeredmaster::arrange(monitor, cfacts),
        #[cfg(feature = "hooks")]
        LayoutKind::Lua => crate::features::hooks::lua_arrange(monitor, cfacts),
        #[cfg(not(any_layout))]
        LayoutKind::Fallback => fallback_arrange(monitor, cfacts),
    }
}

/// Split `total` pixels among `weights`, giving each entry a share proportional
/// to its weight and accumulating the rounding remainder in the last entry so
/// the spans always sum to exactly `total`.  A non-positive remaining weight
/// (e.g. all-zero factors) falls back to equal division of what is left.
///
/// Used by the layouts that stack several windows in one column/row (tile,
/// bstack, centeredmaster) to apply per-window `cfact` factors.
#[cfg(any(feature = "tile", feature = "bstack", feature = "centeredmaster"))]
#[must_use]
#[allow(clippy::cast_possible_truncation)]
pub fn weighted_spans(total: i32, weights: &[f64]) -> Vec<i32> {
    let n = weights.len();
    let mut spans = Vec::with_capacity(n);
    let mut used = 0i32;
    for i in 0..n {
        let remaining: f64 = weights[i..].iter().map(|w| w.max(0.0)).sum();
        let span = if remaining > 0.0 {
            (f64::from(total - used) * (weights[i].max(0.0) / remaining)).round() as i32
        } else {
            #[allow(clippy::cast_possible_wrap)]
            { (total - used) / (n - i) as i32 }
        };
        spans.push(span);
        used += span;
    }
    spans
}

/// Built-in arrangement used when no layout feature is compiled in: every
/// window is stacked to fill the full work area.
#[cfg(not(any_layout))]
fn fallback_arrange(monitor: &Monitor, cfacts: &[f64]) -> Vec<Rectangle<i32, Logical>> {
    vec![monitor.w; cfacts.len()]
}
