//! Optional compositor features, each gated behind a Cargo feature flag.
//!
//! Features are grouped into category modules ([`shell`], [`effects`],
//! [`windows`], [`session`], [`integration`]). Each category re-exports its
//! members so they remain reachable as `crate::features::<name>`.

pub mod shell;
pub mod effects;
pub mod windows;
pub mod session;
pub mod integration;

pub use effects::*;
pub use integration::*;
pub use session::*;
pub use shell::*;
pub use windows::*;

pub mod layout;
