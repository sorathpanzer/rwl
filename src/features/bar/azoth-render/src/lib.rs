//! azoth-render — safe public API over fcft + pixman.
//!
//! All unsafe code is contained within this crate.

#![allow(unsafe_code)]

mod ffi;
mod render;

use std::ffi::{c_char, CString};

pub use ffi::{RustColor, RustTextColor};

// ── Font handle ────────────────────────────────────────────────────────────────

/// Owned handle to an fcft font.  Safe to send across threads.
pub struct Font {
    ptr: *mut ffi::FcftFont,
}

// Safety: fcft rasterisation is thread-safe after the initial load.
unsafe impl Send for Font {}
unsafe impl Sync for Font {}

impl Font {
    /// Load a font from a slice of fontconfig name strings and a DPI value.
    /// Returns `None` if fcft cannot find or open any of the named fonts.
    #[must_use]
    pub fn load(names: impl IntoIterator<Item = impl AsRef<str>>, dpi: u32) -> Option<Self> {
        let cstrings: Vec<CString> = names.into_iter()
            .filter_map(|s| CString::new(s.as_ref()).ok())
            .collect();
        let ptrs: Vec<*const c_char> = cstrings.iter()
            .map(|cs| cs.as_ptr())
            .collect();
        let opt = CString::new(format!("dpi={dpi}")).ok()?;
        let ptr = unsafe {
            ffi::fcft_from_name(ptrs.len(), ptrs.as_ptr(), opt.as_ptr())
        };
        if ptr.is_null() { None } else { Some(Self { ptr }) }
    }

    /// Returns the font's line height in pixels.
    #[must_use]
    pub fn height(&self) -> u32 {
        #[allow(clippy::cast_sign_loss)]
        unsafe { (*self.ptr).height.cast_unsigned() }
    }
}

impl Drop for Font {
    fn drop(&mut self) {
        unsafe { ffi::fcft_destroy(self.ptr); }
    }
}

// ── Lifecycle ──────────────────────────────────────────────────────────────────

/// Initialise the fcft library (call once at startup).
pub fn init_fcft() {
    unsafe { ffi::fcft_init(ffi::FCFT_LOG_COLORIZE_AUTO, 0, ffi::FCFT_LOG_CLASS_ERROR); }
}

/// Tear down the fcft library (call once at shutdown).
pub fn fini_fcft() {
    unsafe { ffi::fcft_fini(); }
}

// ── render_bar ────────────────────────────────────────────────────────────────

/// All parameters needed to render one bar frame.
#[allow(clippy::struct_excessive_bools)]
pub struct BarRenderParams<'a> {
    pub width:       u32,
    pub height:      u32,
    pub stride:      u32,
    pub textpadding: u32,
    pub mtags:       u32,
    pub ctags:       u32,
    pub urg:         u32,
    pub sel:         u32,
    pub layout:      Option<&'a str>,
    pub window_title: Option<&'a str>,
    pub status_text:  &'a str,
    pub status_colors: &'a [RustTextColor],
    pub title_fg:          Option<&'a RustColor>,
    pub hide_vacant:       bool,
    pub center_title:      bool,
    pub active_color_title: bool,
    pub active_fg:   &'a RustColor,
    pub active_bg:   &'a RustColor,
    pub occupied_fg: &'a RustColor,
    pub occupied_bg: &'a RustColor,
    pub inactive_fg: &'a RustColor,
    pub inactive_bg: &'a RustColor,
    pub urgent_fg:   &'a RustColor,
    pub urgent_bg:   &'a RustColor,
    pub middle_bg:   &'a RustColor,
    pub middle_bg_sel: &'a RustColor,
    pub tags:        &'a [String],
    pub font:        &'a Font,
}

/// Render a complete bar frame into `pixels`.
///
/// `pixels` must have length `>= (params.stride / 4) * params.height`.
pub fn render_bar(pixels: &mut [u32], params: &BarRenderParams<'_>) {
    // Build NUL-terminated tag name strings.
    let tag_cstrings: Vec<CString> = params.tags.iter()
        .filter_map(|s| CString::new(s.as_str()).ok())
        .collect();
    let tag_ptrs: Vec<*const c_char> = tag_cstrings.iter()
        .map(|cs| cs.as_ptr())
        .collect();

    let layout_cs  = params.layout.and_then(|s| CString::new(s).ok());
    let title_cs   = params.window_title.and_then(|s| CString::new(s).ok());
    let status_cs  = CString::new(params.status_text).unwrap_or_default();

    let layout_ptr = layout_cs.as_ref().map_or(std::ptr::null(), |cs| cs.as_ptr());
    let title_ptr  = title_cs.as_ref().map_or(std::ptr::null(), |cs| cs.as_ptr());

    unsafe {
        render::draw_bar(
            pixels.as_mut_ptr(),
            params.width, params.height, params.stride,
            params.font.ptr,
            params.textpadding,
            #[allow(clippy::cast_possible_truncation)]
            { params.tags.len() as u32 },
            tag_ptrs.as_ptr(),
            params.mtags, params.ctags, params.urg, params.sel,
            i32::from(params.hide_vacant),
            layout_ptr,
            title_ptr,
            status_cs.as_ptr(),
            params.status_colors.as_ptr(),
            #[allow(clippy::cast_possible_truncation)]
            { params.status_colors.len() as u32 },
            params.title_fg.map_or(std::ptr::null(), |c| c as *const _),
            i32::from(params.center_title),
            i32::from(params.active_color_title),
            params.active_fg,
            params.active_bg,
            params.occupied_fg,
            params.occupied_bg,
            params.inactive_fg,
            params.inactive_bg,
            params.urgent_fg,
            params.urgent_bg,
            params.middle_bg,
            params.middle_bg_sel,
        );
    }
}
