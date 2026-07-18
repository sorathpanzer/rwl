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
