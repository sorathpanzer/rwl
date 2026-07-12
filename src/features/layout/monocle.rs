//! Monocle layout `[M]` — scrollable full-screen windows.
//!
//! Every window fills the whole work area.  Windows are laid out side by side
//! and the viewport scrolls horizontally so the focused window (tracked via
//! `monitor.scroll_col`) is the one on screen — exactly like the scroll layout
//! but with each column spanning the full screen instead of `mfact` of it.

use smithay::utils::{Logical, Rectangle};

use crate::monitor::Monitor;

/// Overlay all `n` windows on top of each other, each filling the work area.
#[must_use]
#[allow(clippy::cast_possible_wrap)]
pub fn arrange(monitor: &Monitor, cfacts: &[f64]) -> Vec<Rectangle<i32, Logical>> {
    // Every window fills the work area, so per-window size factors don't apply.
    let n = cfacts.len();
    let raw = monitor.w;
    let geom = Rectangle::new(
        (raw.loc.x, raw.loc.y).into(),
        ((raw.size.w - 2).max(1), (raw.size.h - 2).max(1)).into(),
    );
    vec![geom; n]
}


