//! Scrollable-column layout `>>>` — one window per full-height column, viewport
//! scrolls horizontally to keep the focused column centred on screen.
//!
//! Column width is `mfact * monitor_width`.  With a single window the layout
//! degenerates to monocle (full area, no border/gaps).

use smithay::utils::{Logical, Rectangle};

use crate::monitor::Monitor;

/// Arrange `n` windows in scrollable full-height columns.
#[must_use]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
)]
pub fn arrange(monitor: &Monitor, cfacts: &[f64]) -> Vec<Rectangle<i32, Logical>> {
    // One full-height window per column; per-window height factors don't apply.
    let n = cfacts.len();
    if n == 0 {
        return Vec::new();
    }

    let cfg = crate::config::get();
    #[cfg(feature = "gaps")]
    let gp = crate::features::gaps::effective(n, cfg.smart_gaps, cfg.gaps_px);
    #[cfg(not(feature = "gaps"))]
    let gp = 0i32;
    let bw = if n > 1 { cfg.border_px as i32 } else { 0 };
    drop(cfg);

    let raw = monitor.w;
    let area_x = raw.loc.x + gp;
    let area_y = raw.loc.y + gp;
    let area_w = (raw.size.w - 2 * gp).max(1);
    let area_h = (raw.size.h - 2 * gp).max(1);

    if n == 1 {
        return vec![Rectangle::new(
            (area_x, area_y).into(),
            (area_w, area_h).into(),
        )];
    }

    let mfact = monitor.mfact.clamp(0.05, 0.95);
    let col_w = (f64::from(area_w) * mfact).round() as i32;
    let col_w = col_w.max(1);

    // Total canvas width of all columns plus inter-column gaps.
    let total_w = col_w * n as i32 + gp * (n as i32 - 1);

    // Scroll to centre the focused column.
    let focused_col = monitor.scroll_col.min(n - 1) as i32;
    let focused_left = focused_col * (col_w + gp);
    let center_offset = focused_left + col_w / 2 - area_w / 2;
    let max_offset = (total_w - area_w).max(0);
    let scroll_offset = center_offset.clamp(0, max_offset);

    (0..n as i32)
        .map(|i| {
            let canvas_x = i * (col_w + gp);
            let screen_x = area_x + canvas_x - scroll_offset;
            Rectangle::new(
                (screen_x + bw, area_y + bw).into(),
                ((col_w - 2 * bw).max(1), (area_h - 2 * bw).max(1)).into(),
            )
        })
        .collect()
}
