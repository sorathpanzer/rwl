// FFI bindings to fcft and pixman — identical to the old src/ffi.rs
// but lives here so all unsafe is isolated in this crate.

use std::ffi::c_char;

// ── Color types ────────────────────────────────────────────────────────────────

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct RustColor {
    pub red:   u16,
    pub green: u16,
    pub blue:  u16,
    pub alpha: u16,
}

impl RustColor {
    #[must_use]
    pub const fn opaque(r: u8, g: u8, b: u8) -> Self {
        Self {
            red:   r as u16 * 0x101,
            green: g as u16 * 0x101,
            blue:  b as u16 * 0x101,
            alpha: 0xFFFF,
        }
    }
}

#[repr(C)]
#[derive(Clone, Debug)]
pub struct RustTextColor {
    pub color:      RustColor,
    pub is_bg:      i32,
    pub start_byte: u32,
}

// ── fcft types ─────────────────────────────────────────────────────────────────

#[repr(C)]
pub struct FcftFont {
    pub name:    *const c_char,
    pub height:  i32,
    pub descent: i32,
    pub ascent:  i32,
}

#[repr(C)]
pub struct FcftGlyph {
    pub cp:        u32,
    pub cols:      i32,
    pub font_name: *const c_char,
    pub pix:       *mut PixmanImage,
    pub x:         i32,
    pub y:         i32,
    pub width:     i32,
    pub height:    i32,
    pub advance:   FcftAdvance,
}

#[repr(C)]
pub struct FcftAdvance {
    pub x: i32,
    pub y: i32,
}

pub const FCFT_SUBPIXEL_NONE: i32 = 1;

// ── pixman types ───────────────────────────────────────────────────────────────

pub enum PixmanImage {}

#[repr(C)]
pub struct PixmanBox32 {
    pub x1: i32,
    pub y1: i32,
    pub x2: i32,
    pub y2: i32,
}

pub const PIXMAN_A8R8G8B8: u32 = 0x2002_8888;
pub const PIXMAN_A8: u32 = 0x0801_8000;

pub const PIXMAN_OP_SRC:  i32 = 1;
pub const PIXMAN_OP_OVER: i32 = 3;

// ── fcft extern ────────────────────────────────────────────────────────────────

extern "C" {
    pub fn fcft_init(colorize: u32, do_syslog: i32, log_level: u32);
    pub fn fcft_fini();
    pub fn fcft_from_name(
        count: usize,
        names: *const *const c_char,
        options: *const c_char,
    ) -> *mut FcftFont;
    pub fn fcft_destroy(font: *mut FcftFont);
    pub fn fcft_rasterize_char_utf32(
        font: *mut FcftFont,
        cp: u32,
        subpixel: i32,
    ) -> *const FcftGlyph;
    pub fn fcft_kerning(
        font: *mut FcftFont,
        left: u32,
        right: u32,
        x: *mut libc::c_long,
        y: *mut libc::c_long,
    ) -> bool;
}

// ── pixman extern ──────────────────────────────────────────────────────────────

extern "C" {
    pub fn pixman_image_create_bits(
        format: u32,
        width: i32,
        height: i32,
        bits: *mut u32,
        rowstride_bytes: i32,
    ) -> *mut PixmanImage;
    pub fn pixman_image_create_solid_fill(color: *const RustColor) -> *mut PixmanImage;
    pub fn pixman_image_unref(image: *mut PixmanImage) -> i32;
    pub fn pixman_image_get_format(image: *mut PixmanImage) -> u32;
    pub fn pixman_image_fill_boxes(
        op: i32,
        dest: *mut PixmanImage,
        color: *const RustColor,
        n_boxes: i32,
        boxes: *const PixmanBox32,
    ) -> i32;
    pub fn pixman_image_composite32(
        op: i32,
        src: *mut PixmanImage,
        mask: *mut PixmanImage,
        dest: *mut PixmanImage,
        src_x: i32, src_y: i32,
        mask_x: i32, mask_y: i32,
        dest_x: i32, dest_y: i32,
        width: i32, height: i32,
    );
    pub fn pixman_image_set_alpha_map(
        image: *mut PixmanImage,
        alpha_map: *mut PixmanImage,
        x: i16,
        y: i16,
    );
}

pub const FCFT_LOG_COLORIZE_AUTO: u32 = 2;
pub const FCFT_LOG_CLASS_ERROR:   u32 = 1;
