use super::ffi::RustTextColor;

pub(super) const TEXT_MAX: usize = 2048;

/// Processed text with inline color and click-region metadata.
#[derive(Default, Clone)]
pub(super) struct CustomText {
    pub text: String,
    pub colors: Vec<RustTextColor>,
}
