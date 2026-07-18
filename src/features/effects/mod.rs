//! Visual effects and rendering: fades, rounded corners, tag transitions
//! and gaps.

#[cfg(feature = "fade")]
pub mod fade;
#[cfg(feature = "rounded-corners")]
pub mod rounded_corners;
#[cfg(feature = "tag-transition")]
pub mod tag_transition;
#[cfg(feature = "gaps")]
pub mod gaps;
