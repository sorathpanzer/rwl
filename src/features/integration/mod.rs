//! Integration with the outside world: pointer input (warp), the Lua scripting
//! hooks, the winit windowed backend and IPC.

#[cfg(feature = "ipc")]
pub mod ipc;
#[cfg(feature = "warp")]
pub mod warp;
#[cfg(feature = "hooks")]
pub mod hooks;
#[cfg(feature = "winit")]
pub mod winit;
