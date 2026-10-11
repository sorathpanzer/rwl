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

    /// Pixel advance width of `text` when laid out with this font, including a
    /// leading and trailing `padding`.  Mirrors the layout used by `render_bar`
    /// so callers can hit-test glyph runs (e.g. status blocks) against a
    /// pointer position.  Returns 0 for empty text.
    #[must_use]
    pub fn measure(&self, text: &str, padding: u32) -> u32 {
        if text.is_empty() { return 0; }
        let cs = CString::new(text).unwrap_or_default();
        unsafe { render::text_width(cs.as_ptr(), u32::MAX, padding, self.ptr) }
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

// ── render_text_widget ──────────────────────────────────────────────────────

/// Parameters for a bordered multi-line text widget (e.g. a hover popup).
pub struct TextWidgetParams<'a> {
    pub width:       u32,
    pub height:      u32,
    pub stride:      u32,
    pub padding:     u32,
    pub line_height: u32,
    /// Corner radius in pixels; 0 = square corners.
    pub corner_radius: u32,
    pub fg:     &'a RustColor,
    pub bg:     &'a RustColor,
    pub border: &'a RustColor,
    pub lines:  &'a [String],
    pub font:   &'a Font,
}

/// Render a bordered box of left-aligned text lines into `pixels`.
///
/// Recognises ANSI SGR reverse-video (`ESC[7m` / `ESC[27m` / `ESC[0m`) so tools
/// like `cal --color=always` show their highlighted cell.  When
/// `corner_radius > 0`, the corners are rounded (transparent outside the arc).
pub fn render_text_widget(pixels: &mut [u32], p: &TextWidgetParams<'_>) {
    // Parse each line's ANSI escapes into clean text + colour runs.  Reverse
    // video paints the accent (`border`) colour behind the run for a clearly
    // visible highlight cell, like a Pango-markup calendar in other bars.
    let parsed: Vec<(CString, Vec<RustTextColor>)> = p.lines.iter()
        .map(|l| parse_ansi(l, p.fg, p.bg, p.border))
        .collect();
    let line_ptrs: Vec<*const c_char> = parsed.iter().map(|(cs, _)| cs.as_ptr()).collect();
    let col_ptrs:  Vec<*const RustTextColor> = parsed.iter().map(|(_, c)| c.as_ptr()).collect();
    #[allow(clippy::cast_possible_truncation)]
    let col_lens:  Vec<u32> = parsed.iter().map(|(_, c)| c.len() as u32).collect();

    unsafe {
        render::draw_lines(
            pixels.as_mut_ptr(),
            p.width, p.height, p.stride,
            p.font.ptr,
            p.padding, p.line_height,
            p.fg, p.bg, p.border,
            line_ptrs.as_ptr(),
            col_ptrs.as_ptr(),
            col_lens.as_ptr(),
            #[allow(clippy::cast_possible_truncation)]
            { line_ptrs.len() as u32 },
        );
    }

    if p.corner_radius > 0 {
        round_corners(pixels, p.width, p.height, p.corner_radius, p.border);
    }
}

/// Split a line into clean text plus colour runs, interpreting ANSI SGR
/// reverse-video toggles.  Within a reverse-video run the text is drawn in the
/// widget background over an `accent`-coloured cell (how terminals render
/// `cal`'s current-day cell, made prominent).
fn parse_ansi(line: &str, fg: &RustColor, bg: &RustColor, accent: &RustColor)
    -> (CString, Vec<RustTextColor>)
{
    let mut out = String::new();
    let mut colors: Vec<RustTextColor> = Vec::new();
    let mut reverse = false;
    let bytes = line.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == 0x1b {
            // CSI sequence: ESC '[' params final-byte.
            if bytes.get(i + 1) == Some(&b'[') {
                let mut j = i + 2;
                while j < bytes.len() && !(0x40..=0x7e).contains(&bytes[j]) { j += 1; }
                if j < bytes.len() && bytes[j] == b'm' {
                    let params = &line[i + 2..j];
                    let mut changed = false;
                    if params.is_empty() {
                        reverse = false;
                        changed = true;
                    }
                    for p in params.split(';') {
                        match p {
                            "" | "0" | "27" => { reverse = false; changed = true; }
                            "7"             => { reverse = true;  changed = true; }
                            _ => {}
                        }
                    }
                    if changed {
                        // Reverse: text in widget bg over an accent cell. Normal: fg on bg.
                        let (f, b) = if reverse { (bg, accent) } else { (fg, bg) };
                        #[allow(clippy::cast_possible_truncation)]
                        let pos = out.len() as u32;
                        colors.push(RustTextColor { color: *f, is_bg: 0, start_byte: pos });
                        colors.push(RustTextColor { color: *b, is_bg: 1, start_byte: pos });
                    }
                }
                i = if j < bytes.len() { j + 1 } else { j };
                continue;
            }
            i += 1; // lone ESC
            continue;
        }
        let Some(ch) = line[i..].chars().next() else { break };
        out.push(ch);
        i += ch.len_utf8();
    }

    (CString::new(out).unwrap_or_default(), colors)
}

/// Remove ANSI CSI escape sequences, returning only the visible text.  Use this
/// to measure a line's on-screen width (the escapes are not drawn).
#[must_use]
pub fn strip_ansi(line: &str) -> String {
    if !line.as_bytes().contains(&0x1b) {
        return line.to_owned();
    }
    let bytes = line.as_bytes();
    let mut out = String::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b {
            if bytes.get(i + 1) == Some(&b'[') {
                let mut j = i + 2;
                while j < bytes.len() && !(0x40..=0x7e).contains(&bytes[j]) { j += 1; }
                i = if j < bytes.len() { j + 1 } else { j };
                continue;
            }
            i += 1;
            continue;
        }
        let Some(ch) = line[i..].chars().next() else { break };
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// Multiply each premultiplied-ARGB channel of `px` by `cov` (0.0–1.0).
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn scale_argb(px: u32, cov: f32) -> u32 {
    let s = |shift: u32| -> u32 { (f32::from(((px >> shift) & 0xff) as u8) * cov).round() as u32 & 0xff };
    (s(24) << 24) | (s(16) << 16) | (s(8) << 8) | s(0)
}

/// Premultiplied-ARGB u32 for an opaque-or-alpha `RustColor` (channels are `*0x101`).
fn premul_argb(c: &RustColor) -> u32 {
    let a = u32::from(c.alpha >> 8);
    let chan = |v: u16| -> u32 { (u32::from(v >> 8) * a / 255) & 0xff };
    (a << 24) | (chan(c.red) << 16) | (chan(c.green) << 8) | chan(c.blue)
}

/// Round the four corners of the buffer in place: pixels outside the arc become
/// transparent, the arc edge is anti-aliased, and a 1px `border` ring is drawn.
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn round_corners(pixels: &mut [u32], width: u32, height: u32, radius: u32, border: &RustColor) {
    let r = radius.min(width / 2).min(height / 2);
    if r < 1 { return; }
    let rf = r as f32;
    let bw = width as usize;
    let border_px = premul_argb(border);

    // (corner-box origin x, y, arc-center x, y)
    let corners = [
        (0u32,          0u32,          rf,                    rf),
        (width - r,     0,             (width - r) as f32,    rf),
        (0,             height - r,    rf,                    (height - r) as f32),
        (width - r,     height - r,    (width - r) as f32,    (height - r) as f32),
    ];

    for (ox, oy, cx, cy) in corners {
        for y in oy..oy + r {
            for x in ox..ox + r {
                let dx = (x as f32 + 0.5) - cx;
                let dy = (y as f32 + 0.5) - cy;
                let d = dx.hypot(dy);
                let idx = y as usize * bw + x as usize;
                let Some(px) = pixels.get_mut(idx) else { continue };
                if d >= rf + 0.5 {
                    *px = 0; // fully outside → transparent
                } else if d >= rf - 1.0 {
                    // Border ring, anti-aliased against the outer edge.
                    let cov = (rf + 0.5 - d).clamp(0.0, 1.0);
                    *px = scale_argb(border_px, cov);
                }
                // else: interior, leave the rendered content untouched.
            }
        }
    }
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
