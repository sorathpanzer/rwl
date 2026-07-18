//! Window management and placement: swallowing, scratchpad, picture-in-picture,
//! per-tag layouts and auto-back on empty tag.

#[cfg(feature = "swallow")]
pub mod swallow;
#[cfg(feature = "scratchpad")]
pub mod scratchpad;
#[cfg(feature = "pip")]
pub mod pip;
#[cfg(feature = "pertag-layouts")]
pub mod pertag_layouts;
#[cfg(feature = "auto-back-empty-tag")]
pub mod auto_back_empty_tag;
#[cfg(feature = "overview")]
pub mod overview;
