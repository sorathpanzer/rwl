//! Dwindle (fibonacci) layout `[@]` — recursive binary splits spiralling inward.
//!
//! Window 0 (master) takes the left `mfact` fraction of the work area.
//! Each subsequent window halves the remaining space, alternating between
//! horizontal splits (left / right) and vertical splits (top / bottom).

use smithay::utils::{Logical, Rectangle};

use crate::monitor::Monitor;

/// Arrange `n` windows in a dwindle / fibonacci pattern.
#[must_use]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
)]
pub fn arrange(monitor: &Monitor, n: usize) -> Vec<Rectangle<i32, Logical>> {
    if n == 0 {
        return Vec::new();
    }

    let cfg = crate::config::get();
    #[cfg(feature = "gaps")]
    let gp = if n > 1 && crate::features::gaps::gaps_enabled() { cfg.gaps_px as i32 } else { 0 };
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

    let mut geoms = Vec::with_capacity(n);

    // Remaining region to subdivide (grows smaller each iteration).
    let mut rx = area_x;
    let mut ry = area_y;
    let mut rw = area_w;
    let mut rh = area_h;

    for i in 0..n {
        if i == n - 1 {
            // Last window fills whatever space remains.
            geoms.push(Rectangle::new(
                (rx + bw, ry + bw).into(),
                ((rw - 2 * bw).max(1), (rh - 2 * bw).max(1)).into(),
            ));
        } else if i % 2 == 0 {
            // Horizontal split: current window takes the left portion.
            let ratio = if i == 0 { mfact } else { 0.5 };
            let win_w = (f64::from((rw - gp).max(1)) * ratio).round() as i32;
            let win_w = win_w.max(1);
            geoms.push(Rectangle::new(
                (rx + bw, ry + bw).into(),
                ((win_w - 2 * bw).max(1), (rh - 2 * bw).max(1)).into(),
            ));
            rx += win_w + gp;
            rw = (rw - win_w - gp).max(1);
        } else {
            // Vertical split: current window takes the top portion.
            let win_h = (f64::from((rh - gp).max(1)) * 0.5).round() as i32;
            let win_h = win_h.max(1);
            geoms.push(Rectangle::new(
                (rx + bw, ry + bw).into(),
                ((rw - 2 * bw).max(1), (win_h - 2 * bw).max(1)).into(),
            ));
            ry += win_h + gp;
            rh = (rh - win_h - gp).max(1);
        }
    }

    geoms
}
