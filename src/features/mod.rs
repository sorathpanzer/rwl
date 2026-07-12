//! Optional compositor features, each gated behind a Cargo feature flag.

#[cfg(feature = "ipc")]
pub mod ipc;

#[cfg(feature = "scratchpad")]
pub mod scratchpad;
#[cfg(feature = "bar")]
pub mod bar;
#[cfg(feature = "rounded-corners")]
pub mod rounded_corners;
#[cfg(feature = "warp")]
pub mod warp;
#[cfg(feature = "fade")]
pub mod fade;
#[cfg(feature = "tag-transition")]
pub mod tag_transition;
#[cfg(feature = "gaps")]
pub mod gaps;
#[cfg(feature = "winit")]
pub mod winit;
#[cfg(feature = "pertag-layouts")]
pub mod pertag_layouts;
#[cfg(feature = "startup-cmds")]
pub mod startup_cmds;
#[cfg(feature = "auto-back-empty-tag")]
pub mod auto_back_empty_tag;
pub mod layout;
#[cfg(feature = "overview")]
pub mod overview;
#[cfg(feature = "hooks")]
pub mod hooks;
#[cfg(feature = "pip")]
pub mod pip;
#[cfg(feature = "lock")]
pub mod lock;
#[cfg(feature = "wallpaper")]
pub mod wallpaper;
