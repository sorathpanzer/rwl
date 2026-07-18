//! Centered-master tiling layout `|M|`.
//!
//! The master window(s) occupy a column in the *centre* of the screen and the
//! stack windows are split into two side columns, filled alternately right,
//! left, right, … — mirroring dwl's `centeredmaster`.
//!
//! * With a single window (or `n <= nmaster`) the lone master spans the full
//!   width.
//! * With exactly one stack window the master sits on the left and the stack on
//!   the right — a lone side window can't be centred either way.
//! * With two or more stack windows the master is centred and the stack is
//!   split into left and right columns.

use smithay::utils::{Logical, Rectangle};

use crate::monitor::Monitor;

/// Tile `n` windows in a centred-master arrangement.
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
    // No borders / gaps for a lone tiled window.
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

    let mcount = n.min(nmaster);          // windows in the centre master column
    let scount = n - mcount;              // windows in the side columns
    let rcount = scount.div_ceil(2);      // right column gets the extra (ceil)
    let lcount = scount / 2;              // left column (floor)

    // Vertical distribution within a column: heights proportional to each
    // window's `cfact`, with rounding pixels accumulating at the last window
    // (mirrors `tile`), separated by `gp` inner gaps and inset by the border
    // width `bw`.  `cf` holds the height factors for this column's windows.
    let column = |cf: &[f64], col_x: i32, col_w: i32| -> Vec<Rectangle<i32, Logical>> {
        let count = cf.len();
        if count == 0 {
            return Vec::new();
        }
        let n_gaps = (count as i32 - 1) * gp;
        let win_h_total = area_h - n_gaps;
        let mut out = Vec::with_capacity(count);
        let mut y_offset = 0i32;
        for (i, win_h) in super::weighted_spans(win_h_total, cf).into_iter().enumerate() {
            out.push(Rectangle::new(
                (col_x + bw, area_y + y_offset + bw).into(),
                ((col_w - 2 * bw).max(1), (win_h - 2 * bw).max(1)).into(),
            ));
            y_offset += win_h + if i + 1 < count { gp } else { 0 };
        }
        out
    };

    // Horizontal geometry of the master / left / right columns.
    let (master_x, master_w, left_x, left_w, right_x, right_w) = if scount == 0 {
        // Master only, spanning the full width.
        (area_x, area_w, 0, 0, 0, 0)
    } else if scount == 1 {
        // Master left, single stack right.
        let has_master = mcount > 0;
        let inner_gap = if has_master { gp } else { 0 };
        let mw = if has_master {
            (f64::from(area_w - inner_gap) * mfact).round() as i32
        } else {
            0
        };
        let sw = (area_w - mw - inner_gap).max(1);
        let sx = area_x + mw + inner_gap;
        // The lone stack window fills the "right" column (stack index 0, even).
        (area_x, mw, 0, 0, sx, sw)
    } else if mcount > 0 {
        // Centred master with stack split into left + right columns.
        let mw = (f64::from(area_w - 2 * gp) * mfact).round() as i32;
        let side = (area_w - 2 * gp - mw).max(2);
        let lw = side / 2;
        let rw = side - lw;
        let lx = area_x;
        let mx = area_x + lw + gp;
        let rx = mx + mw + gp;
        (mx, mw, lx, lw, rx, rw)
    } else {
        // No master: two equal side columns.
        let lw = (area_w - gp) / 2;
        let rw = (area_w - gp - lw).max(1);
        (0, 0, area_x, lw, area_x + lw + gp, rw)
    };

    // Gather each column's height factors from `cfacts`, matching the fill order
    // used when reassembling below: master windows are 0..mcount; stack windows
    // alternate right, left, right, … starting at index mcount.
    let master_cf = &cfacts[..mcount];
    let right_cf: Vec<f64> = (0..rcount).map(|k| cfacts[mcount + 2 * k]).collect();
    let left_cf: Vec<f64> = (0..lcount).map(|k| cfacts[mcount + 2 * k + 1]).collect();

    let master = column(master_cf, master_x, master_w);
    let right = column(&right_cf, right_x, right_w);
    let left = column(&left_cf, left_x, left_w);

    // Reassemble in tiling order: master windows first, then stack windows
    // alternating right, left, right, … to match dwl's fill order. Every index
    // in `0..n` is written exactly once (mcount + rcount + lcount == n).
    let zero = Rectangle::new((0, 0).into(), (0, 0).into());
    let mut geoms = vec![zero; n];
    for (i, r) in master.into_iter().enumerate() {
        geoms[i] = r;
    }
    for (k, r) in right.into_iter().enumerate() {
        geoms[mcount + 2 * k] = r;
    }
    for (k, r) in left.into_iter().enumerate() {
        geoms[mcount + 2 * k + 1] = r;
    }
    geoms
}
