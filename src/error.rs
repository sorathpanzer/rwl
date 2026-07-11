//! Error types for the rwl compositor.

/// Top-level error type for the compositor.
#[derive(Debug, thiserror::Error)]
pub enum RwlError {
    /// Failed to initialise the Wayland display.
    #[error("Wayland display error: {0}")]
    Display(String),

    /// Failed to create or run the event loop.
    #[error("Event loop error: {0}")]
    EventLoop(String),

    /// Backend (udev / DRM / libinput) initialisation failed.
    #[error("Backend error: {0}")]
    Backend(String),

    /// A DRM / GBM / EGL operation failed.
    #[error("DRM error: {0}")]
    Drm(String),

    /// Could not open or access a device node.
    #[error("Device error: {0}")]
    Device(String),

    /// Wayland listening socket could not be created.
    #[error("Socket error: {0}")]
    Socket(String),

    /// A session / seat operation failed.
    #[error("Session error: {0}")]
    Session(String),

    /// A renderer initialisation or operation failed.
    #[error("Renderer error: {0}")]
    Renderer(String),

    /// Generic I/O error.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// Convenient `Result` alias used throughout the crate.
pub type Result<T> = std::result::Result<T, RwlError>;
