//! One-time whole-screen zoom-in played once when the compositor starts.
//!
//! Unlike [`super::fade`] and [`super::tag_transition`], which animate
//! individual windows, this is a single global transform: every render element
//! of an output is wrapped in a [`RescaleRenderElement`] centred on the output,
//! with the scale factor animating from `startup_zoom_from` up to `1.0` over
//! `startup_zoom_ms`.  The clock starts on the very first rendered frame so the
//! whole animation is visible regardless of how long compositor init took.
//!
//! Compiled only when the `startup-zoom` Cargo feature is enabled.

use std::time::Instant;

/// Which way the startup zoom animates toward full size (`1.0`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ZoomDirection {
    /// Start smaller than the screen and grow up to full size (content zooms in).
    #[default]
    In,
    /// Start larger than the screen and shrink down to full size (content zooms out).
    Out,
}

impl ZoomDirection {
    /// Parse a config string (`in` / `out`).
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "in"  => Some(Self::In),
            "out" => Some(Self::Out),
            _     => None,
        }
    }
}

/// Advance the startup zoom and return the scale factor to apply this frame.
///
/// Returns `Some(scale)` while the animation is running (caller wraps every
/// output element in a centred rescale by `scale` and schedules another frame),
/// or `None` once it has finished or when the effect is disabled
/// (`startup_zoom_ms == 0`).
pub fn advance(state: &mut crate::state::Rwl) -> Option<f64> {
    if state.startup_zoom_done {
        return None;
    }

    let (duration_ms, amount, direction) = {
        let cfg = crate::config::get();
        (cfg.startup_zoom_ms, cfg.startup_zoom_amount, cfg.startup_zoom_direction)
    };
    if duration_ms == 0 {
        state.startup_zoom_done = true;
        return None;
    }

    // Start the clock on the first frame we actually render.
    let start = *state.startup_zoom_start.get_or_insert_with(Instant::now);

    let elapsed_ms = Instant::now().duration_since(start).as_secs_f64() * 1000.0;
    let t = (elapsed_ms / f64::from(duration_ms)).min(1.0);
    if t >= 1.0 {
        state.startup_zoom_done = true;
        state.startup_zoom_start = None;
        return None;
    }

    // The scale the frame starts at, animating toward full size (1.0).
    // Zoom in starts below 1.0 (grows outward); zoom out starts above (shrinks in).
    let from = match direction {
        ZoomDirection::In  => 1.0 - amount,
        ZoomDirection::Out => 1.0 + amount,
    };

    // Cubic ease-out: quick change that softly settles at 1.0.
    let eased = 1.0 - (1.0 - t).powi(3);
    Some((1.0 - from).mul_add(eased, from))
}
