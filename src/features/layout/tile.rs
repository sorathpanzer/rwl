//! Master–stack tiling layout `[]=`.

use smithay::utils::{Logical, Rectangle};

use crate::monitor::Monitor;

/// Tile `n` windows in a master-left / stack-right arrangement.
#[must_use]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
)]
pub fn arrange(monitor: &Monitor, cfacts: &[f64]) -> Vec<Rectangle<i32, Logical>> {
    let n = cfacts.len();
    let cfg = crate::config::get();
    // No borders for a lone tiled window.
    #[cfg(feature = "gaps")]
    let gp = crate::features::gaps::effective(n, cfg.smart_gaps, cfg.gaps_px);
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

    // Column widths; when both columns exist leave a gp-wide gap between them.
    let (master_w, stack_x, stack_w) = if in_stack > 0 {
        let mw = (f64::from(area_w - gp) * mfact).round() as i32;
        let sw = (area_w - mw - gp).max(1);
        (mw, area_x + mw + gp, sw)
    } else {
        (area_w, area_x, 0)
    };

    let mut geoms = Vec::with_capacity(n);

    // Place the windows whose height factors are `cf` in a column of width
    // `col_w` starting at `col_x`.  Row heights are proportional to each
    // window's `cfact`; rows are separated by `gp` inner gaps and rounding
    // pixels accumulate at the last window rather than leaving a gap.
    let column = |cf: &[f64], col_w: i32, col_x: i32, geoms: &mut Vec<Rectangle<i32, Logical>>| {
        let count = cf.len();
        if count == 0 {
            return;
        }
        let n_gaps = (count as i32 - 1) * gp;
        let win_h_total = area_h - n_gaps;
        let mut y_offset = 0i32;
        for (i, win_h) in super::weighted_spans(win_h_total, cf).into_iter().enumerate() {
            geoms.push(Rectangle::new(
                (col_x + bw, area_y + y_offset + bw).into(),
                ((col_w - 2 * bw).max(1), (win_h - 2 * bw).max(1)).into(),
            ));
            y_offset += win_h + if i + 1 < count { gp } else { 0 };
        }
    };

    column(&cfacts[..in_master], master_w, area_x, &mut geoms);
    column(&cfacts[in_master..], stack_w, stack_x, &mut geoms);

    geoms
}
