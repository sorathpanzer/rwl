//! Backend abstractions.
//!
//! Two backends are available:
//! - **udev/DRM** (`BackendData::Udev`): direct display access for bare-metal TTY use.
//! - **Winit** (`BackendData::Winit`): nested inside an existing Wayland/X11 compositor
//!   (requires the `winit` feature).
//!
//! The correct backend is selected at startup based on the environment
//! (`WAYLAND_DISPLAY` / `DISPLAY`).

pub mod udev;

pub use udev::UdevData;
#[cfg(feature = "winit")]
pub use crate::features::winit::WinitData;

/// Discriminated union of all supported backend data structs.
pub enum BackendData {
    /// udev/DRM/libinput backend (TTY / bare-metal use).
    Udev(Box<UdevData>),
    /// Winit backend (nested inside a Wayland or X11 compositor).
    #[cfg(feature = "winit")]
    Winit(Box<WinitData>),
}
