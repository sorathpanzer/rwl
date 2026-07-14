//! Protocol handler implementations for [`crate::state::Rwl`].

pub mod compositor;
pub mod data_device;
pub mod gamma_control;
pub mod idle;
pub mod input;
pub mod layer_shell;
pub mod output;
pub mod screencopy;
pub mod seat;
pub mod session_lock;
pub mod xdg_decoration;
pub mod xdg_shell;
#[cfg(feature = "xwayland")]
pub mod xwayland;
