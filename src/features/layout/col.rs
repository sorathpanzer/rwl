//! Column layout `||` — windows arranged in vertical columns.
//!
//! Column widths are controlled by `monitor.col_facts` (per-column fractions
//! that sum to 1.0).  When the vector length doesn't match the window count
//! (e.g. after a window is added or removed), equal widths are used instead.

use smithay::utils::{Logical, Rectangle};

use crate::monitor::Monitor;

/// Arrange `n` windows in vertical columns whose widths follow `monitor.col_facts`.
#[must_use]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss
)]
pub fn arrange(monitor: &Monitor, n: usize) -> Vec<Rectangle<i32, Logical>> {
    if n == 0 {
        return Vec::new();
    }

    let cfg = crate::config::get();
    // No borders for a lone tiled window.
    #[cfg(feature = "gaps")]
    let gp = if n > 1 && crate::features::gaps::gaps_enabled() { cfg.gaps_px as i32 } else { 0 };
    #[cfg(not(feature = "gaps"))]
    let gp = 0i32;
    let bw = if n > 1 { cfg.border_px as i32 } else { 0 };
    drop(cfg);

    // Apply outer gaps.
    let raw = monitor.w;
    let area_x = raw.loc.x + gp;
    let area_y = raw.loc.y + gp;
    let area_w = (raw.size.w - 2 * gp).max(1);
    let area_h = (raw.size.h - 2 * gp).max(1);
    let col_h = (area_h - 2 * bw).max(1);

    // Usable width after reserving inner gaps between columns.
    let n_gaps = (n as i32 - 1) * gp;
    let usable_w = (area_w - n_gaps).max(1);

    // Use stored per-column fractions if they match, otherwise fall back to
    // equal widths and let the next resize grab write new col_facts.
    // `equal_facts` is only allocated in the fallback path; in the common case
    // we borrow `col_facts` directly, avoiding a Vec<f64> clone every arrange.
    let equal_facts;
    let facts: &[f64] = if monitor.col_facts.len() == n {
        &monitor.col_facts
    } else {
        #[allow(clippy::cast_precision_loss)]
        let frac = 1.0 / n as f64;
        equal_facts = vec![frac; n];
        &equal_facts
    };

    facts.iter().copied().enumerate()
        .scan(0i32, |x_offset, (i, frac)| {
            let win_w = if i + 1 == n {
                usable_w - *x_offset
            } else {
                #[allow(clippy::cast_precision_loss)]
                { (f64::from(usable_w) * frac).round() as i32 }
            };
            let rect = Rectangle::new(
                (area_x + *x_offset + bw, area_y + bw).into(),
                ((win_w - 2 * bw).max(1), col_h).into(),
            );
            *x_offset += win_w + if i + 1 < n { gp } else { 0 };
            Some(rect)
        })
        .collect()
}
