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
/// `windows` is the number of windows to tile.  The returned `Vec` has the
/// same length; each entry is the geometry to apply to the corresponding window
/// (in tiling order).
#[must_use]
pub fn arrange(monitor: &Monitor, windows: usize) -> Vec<Rectangle<i32, Logical>> {
    if windows == 0 {
        return Vec::new();
    }

    match monitor.layout_kind() {
        #[cfg(feature = "tile")]
        LayoutKind::Tile    => tile::arrange(monitor, windows),
        #[cfg(feature = "monocle")]
        LayoutKind::Monocle => monocle::arrange(monitor, windows),
        #[cfg(feature = "col")]
        LayoutKind::Col     => col::arrange(monitor, windows),
        #[cfg(feature = "scroll")]
        LayoutKind::Scroll  => scroll::arrange(monitor, windows),
        #[cfg(feature = "dwindle")]
        LayoutKind::Dwindle => dwindle::arrange(monitor, windows),
        #[cfg(feature = "bstack")]
        LayoutKind::Bstack  => bstack::arrange(monitor, windows),
        #[cfg(feature = "centeredmaster")]
        LayoutKind::CenteredMaster => centeredmaster::arrange(monitor, windows),
        #[cfg(not(any(
            feature = "tile",
            feature = "monocle",
            feature = "col",
            feature = "scroll",
            feature = "dwindle",
            feature = "bstack",
            feature = "centeredmaster",
        )))]
        LayoutKind::Fallback => fallback_arrange(monitor, windows),
    }
}

/// Built-in arrangement used when no layout feature is compiled in: every
/// window is stacked to fill the full work area.
#[cfg(not(any(
    feature = "tile",
    feature = "monocle",
    feature = "col",
    feature = "scroll",
    feature = "dwindle",
    feature = "bstack",
    feature = "centeredmaster",
)))]
fn fallback_arrange(monitor: &Monitor, windows: usize) -> Vec<Rectangle<i32, Logical>> {
    vec![monitor.w; windows]
}
