//! Bottom-stack tiling layout `TTT` — master on top, stack row below.
//!
//! This is the [`tile`](super::tile) layout transposed 90°: the master area is
//! a full-width band across the top and the stack is a row of equal-width
//! windows along the bottom.
//!
//! Exception: with exactly two windows it behaves like `tile` (master left,
//! stack right) — a side-by-side split reads better than a short top band when
//! there is only one stack window. Three or more windows use the top/bottom
//! bottom-stack arrangement.

use smithay::utils::{Logical, Rectangle};

use crate::monitor::Monitor;

/// Tile `n` windows in a master-top / stack-bottom arrangement.
#[must_use]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
)]
pub fn arrange(monitor: &Monitor, n: usize) -> Vec<Rectangle<i32, Logical>> {
    let cfg = crate::config::get();
    // No borders for a lone tiled window.
    #[cfg(feature = "gaps")]
    let gp = if n > 1 && crate::features::gaps::gaps_enabled() { cfg.gaps_px as i32 } else { 0 };
    #[cfg(not(feature = "gaps"))]
    let gp = 0i32;
    let bw = if n > 1 { cfg.border_px as i32 } else { 0 };
    drop(cfg);

    // Apply outer gaps to the work area.
    let raw = monitor.w;
    let area_x = raw.loc.x + gp;
    let area_y = raw.loc.y + gp;
    let area_w = (raw.size.w - 2 * gp).max(1);
    let area_h = (raw.size.h - 2 * gp).max(1);

    let nmaster = monitor.nmaster.max(0) as usize;
    let mfact = monitor.mfact.clamp(0.05, 0.95);

    let in_master = n.min(nmaster);
    let in_stack = n.saturating_sub(nmaster);

    // Two windows read better side-by-side (master left, stack right) than as a
    // short top band over a single wide window — behave like `tile` here.
    if n == 2 && in_stack > 0 {
        let master_w = (f64::from(area_w - gp) * mfact).round() as i32;
        let stack_w = (area_w - master_w - gp).max(1);
        let stack_x = area_x + master_w + gp;
        return vec![
            Rectangle::new(
                (area_x + bw, area_y + bw).into(),
                ((master_w - 2 * bw).max(1), (area_h - 2 * bw).max(1)).into(),
            ),
            Rectangle::new(
                (stack_x + bw, area_y + bw).into(),
                ((stack_w - 2 * bw).max(1), (area_h - 2 * bw).max(1)).into(),
            ),
        ];
    }

    // Row heights; when both rows exist leave a gp-tall gap between them.
    let (master_h, stack_y, stack_h) = if in_stack > 0 {
        let mh = (f64::from(area_h - gp) * mfact).round() as i32;
        let sh = (area_h - mh - gp).max(1);
        (mh, area_y + mh + gp, sh)
    } else {
        (area_h, area_y, 0)
    };

    let mut geoms = Vec::with_capacity(n);

    // Place `count` windows in a row of height `row_h` starting at `row_y`.
    // Columns are separated by `gp` inner gaps; rounding pixels accumulate at
    // the last window rather than leaving a gap on the right.
    let mut row = |count: usize, row_h: i32, row_y: i32| {
        if count == 0 {
            return;
        }
        let n_gaps = (count as i32 - 1) * gp;
        let win_w_total = area_w - n_gaps;
        geoms.extend(
            (0..count).scan((0i32, 0i32), |(x_offset, w_used), i| {
                let remaining = (count - i) as i32;
                let win_w = (win_w_total - *w_used) / remaining;
                let rect = Rectangle::new(
                    (area_x + *x_offset + bw, row_y + bw).into(),
                    ((win_w - 2 * bw).max(1), (row_h - 2 * bw).max(1)).into(),
                );
                *w_used += win_w;
                *x_offset += win_w + if i + 1 < count { gp } else { 0 };
                Some(rect)
            }),
        );
    };

    row(in_master, master_h, area_y);
    row(in_stack, stack_h, stack_y);

    geoms
}
