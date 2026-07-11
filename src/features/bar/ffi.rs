// Re-export colour/text-colour types from azoth-render so the rest of the bar
// module keeps its `super::ffi::*` import paths unchanged.
pub use azoth_render::{RustColor, RustTextColor};
