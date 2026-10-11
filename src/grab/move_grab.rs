//! Smithay `PointerGrab` implementation for interactive window moves.
//!
//! The simpler grab logic lives in `grab/mod.rs`; this file contains the
//! trait implementation required when using smithay's pointer grab system
//! directly (future use / alternative approach).

// Currently the move logic is handled inline in mod.rs without using
// smithay's PointerGrab trait, to avoid the complex generic constraints.
// This module is reserved for a future refactor.
