//! Runtime gaps toggle.  Compiled only when the `gaps` Cargo feature is enabled.

use std::sync::atomic::{AtomicBool, Ordering};

static GAPS_ENABLED: AtomicBool = AtomicBool::new(true);

/// Whether gaps are currently active (runtime toggle via `ToggleGaps`).
#[inline]
pub fn gaps_enabled() -> bool {
    GAPS_ENABLED.load(Ordering::Relaxed)
}

/// Toggle gaps on/off; returns the new state (`true` = gaps now on).
pub fn toggle_gaps() -> bool {
    let was = GAPS_ENABLED.fetch_xor(true, Ordering::Relaxed);
    !was
}

/// Effective gap size (px) for a layout arranging `n` tiled windows.
///
/// Returns `0` when gaps are toggled off, or — with `smart_gaps` (dwm's
/// smartgaps) — when there is a single tiled window, so a lone window fills the
/// work area edge-to-edge. Otherwise returns `gaps_px`. Used by every layout so
/// the gap policy lives in one place.
#[inline]
#[must_use]
pub fn effective(n: usize, smart_gaps: bool, gaps_px: u32) -> i32 {
    if gaps_enabled() && (n > 1 || !smart_gaps) {
        i32::try_from(gaps_px).unwrap_or(i32::MAX)
    } else {
        0
    }
}
