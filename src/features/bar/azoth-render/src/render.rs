//! render.rs — fcft + pixman rendering (unsafe allowed in this crate).

use std::{ffi::c_char, ptr};

use crate::ffi::{
    FcftFont,
    PixmanBox32, PixmanImage,
    RustColor, RustTextColor,
    FCFT_SUBPIXEL_NONE,
    PIXMAN_OP_OVER, PIXMAN_OP_SRC,
    PIXMAN_A8, PIXMAN_A8R8G8B8,
    fcft_kerning, fcft_rasterize_char_utf32,
    pixman_image_composite32, pixman_image_create_bits,
    pixman_image_create_solid_fill, pixman_image_fill_boxes,
    pixman_image_get_format, pixman_image_set_alpha_map,
    pixman_image_unref,
};

static EMPTY: &[u8] = b"\0";

#[allow(clippy::too_many_arguments, clippy::too_many_lines, clippy::similar_names)]
unsafe fn draw_text(
    text: *const c_char,
    mut x: u32,
    y: u32,
    foreground: *mut PixmanImage,
    foreground_mask: *mut PixmanImage,
    background: *mut PixmanImage,
    fg_color: *const RustColor,
    bg_color: *const RustColor,
    max_x: u32,
    buf_height: u32,
    // Top of the row band for background fills. Confines a line's background
    // (and reverse-video highlight) to its own row so multi-line widgets don't
    // overwrite earlier lines' highlights. 0 for single-line callers.
    row_top: u32,
    padding: u32,
    font: *mut FcftFont,
    colors: *const RustTextColor,
    colors_l: u32,
) -> u32 {
    if text.is_null() || max_x == 0 {
        return x;
    }
    let text_bytes = std::ffi::CStr::from_ptr(text).to_bytes();
    if text_bytes.is_empty() {
        return x;
    }

    let ix = x;
    let nx = x.wrapping_add(padding);
    if nx.wrapping_add(padding) >= max_x {
        return x;
    }
    x = nx;

    let draw_fg = !foreground.is_null() && !fg_color.is_null();
    let draw_bg = !background.is_null() && !bg_color.is_null();

    let fg_mask_fill: *mut PixmanImage = if draw_fg {
        let white = RustColor { red: 0xFFFF, green: 0xFFFF, blue: 0xFFFF, alpha: 0xFFFF };
        pixman_image_create_solid_fill(&raw const white)
    } else {
        ptr::null_mut()
    };

    let mut color_ind: u32 = 0;
    let mut cur_fg: *const RustColor = fg_color;
    let mut cur_bg: *const RustColor = bg_color;
    let mut last_cp: u32 = 0;

    let text_str = std::str::from_utf8(text_bytes).unwrap_or("");

    for (byte_idx, ch) in text_str.char_indices() {
        #[allow(clippy::cast_possible_truncation)]
        let byte_idx = byte_idx as u32;
        let codepoint = ch as u32;

        if !colors.is_null() {
            while color_ind < colors_l {
                let tc = &*colors.add(color_ind as usize);
                if tc.start_byte != byte_idx {
                    break;
                }
                if tc.is_bg != 0 {
                    if draw_bg { cur_bg = &raw const tc.color; }
                } else if draw_fg {
                    cur_fg = &raw const tc.color;
                }
                color_ind += 1;
            }
        }

        let glyph = fcft_rasterize_char_utf32(font, codepoint, FCFT_SUBPIXEL_NONE);
        if glyph.is_null() { continue; }
        let glyph = &*glyph;

        let mut kern_x: libc::c_long = 0;
        if last_cp != 0 {
            fcft_kerning(font, last_cp, codepoint, &raw mut kern_x, ptr::null_mut());
        }

        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let nx = (i64::from(x) + kern_x as i64 + i64::from(glyph.advance.x)) as u32;
        if nx + padding > max_x { break; }
        last_cp = codepoint;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let x_kern = (i64::from(x) + kern_x as i64) as u32;

        if draw_fg {
            if pixman_image_get_format(glyph.pix) == PIXMAN_A8R8G8B8 {
                pixman_image_composite32(
                    PIXMAN_OP_OVER,
                    glyph.pix, ptr::null_mut(), foreground,
                    0, 0, 0, 0,
                    x_kern.cast_signed() + glyph.x,
                    y.cast_signed() - glyph.y,
                    glyph.width, glyph.height,
                );
            } else {
                pixman_image_fill_boxes(
                    PIXMAN_OP_OVER, foreground, cur_fg, 1,
                    &PixmanBox32 { x1: x_kern.cast_signed(), y1: row_top.cast_signed(), x2: nx.cast_signed(), y2: buf_height.cast_signed() },
                );
            }
            pixman_image_composite32(
                PIXMAN_OP_OVER,
                glyph.pix, fg_mask_fill, foreground_mask,
                0, 0, 0, 0,
                x_kern.cast_signed() + glyph.x,
                y.cast_signed() - glyph.y,
                glyph.width, glyph.height,
            );
        }

        if draw_bg {
            pixman_image_fill_boxes(
                PIXMAN_OP_OVER, background, cur_bg, 1,
                &PixmanBox32 { x1: x_kern.cast_signed(), y1: row_top.cast_signed(), x2: nx.cast_signed(), y2: buf_height.cast_signed() },
            );
        }

        x = nx;
    }

    if !fg_mask_fill.is_null() { pixman_image_unref(fg_mask_fill); }
    if last_cp == 0 { return ix; }

    let nx = x + padding;
    if draw_bg {
        pixman_image_fill_boxes(
            PIXMAN_OP_OVER, background, bg_color, 1,
            &PixmanBox32 { x1: ix.cast_signed(), y1: row_top.cast_signed(), x2: (ix + padding).cast_signed(), y2: buf_height.cast_signed() },
        );
        pixman_image_fill_boxes(
            PIXMAN_OP_OVER, background, bg_color, 1,
            &PixmanBox32 { x1: x.cast_signed(), y1: row_top.cast_signed(), x2: nx.cast_signed(), y2: buf_height.cast_signed() },
        );
    }
    nx
}

pub(crate) unsafe fn text_width(
    text: *const c_char,
    max_width: u32,
    padding: u32,
    font: *mut FcftFont,
) -> u32 {
    draw_text(
        text, 0, 0,
        ptr::null_mut(), ptr::null_mut(), ptr::null_mut(),
        ptr::null(), ptr::null(),
        max_width, 0, 0, padding, font, ptr::null(), 0,
    )
}

/// Render a bordered box filled with `bg_color`, a 1px `border_color` frame, and
/// left-aligned lines of text drawn in `fg_color`.  Each line may carry a run of
/// `RustTextColor` overrides (used for ANSI reverse-video).  Used for hover
/// widgets.
#[allow(clippy::too_many_arguments, clippy::similar_names, clippy::too_many_lines)]
pub unsafe fn draw_lines(
    pixels: *mut u32,
    width: u32,
    height: u32,
    stride: u32,
    font: *mut FcftFont,
    padding: u32,
    line_height: u32,
    fg_color: *const RustColor,
    bg_color: *const RustColor,
    border_color: *const RustColor,
    lines: *const *const c_char,
    colors: *const *const RustTextColor,
    colors_lens: *const u32,
    n_lines: u32,
) {
    let w = width.cast_signed();
    let h = height.cast_signed();

    let final_img = pixman_image_create_bits(PIXMAN_A8R8G8B8, w, h, pixels, stride.cast_signed());
    let fg        = pixman_image_create_bits(PIXMAN_A8R8G8B8, w, h, ptr::null_mut(), 0);
    let mask      = pixman_image_create_bits(PIXMAN_A8,       w, h, ptr::null_mut(), 0);
    let bg        = pixman_image_create_bits(PIXMAN_A8R8G8B8, w, h, ptr::null_mut(), 0);

    if final_img.is_null() || fg.is_null() || mask.is_null() || bg.is_null() {
        if !fg.is_null()        { pixman_image_unref(fg); }
        if !mask.is_null()      { pixman_image_unref(mask); }
        if !bg.is_null()        { pixman_image_unref(bg); }
        if !final_img.is_null() { pixman_image_unref(final_img); }
        return;
    }

    // Inner background (the border frame is stroked last, over any per-glyph
    // background fills that draw_text paints across the full row height).
    pixman_image_fill_boxes(PIXMAN_OP_SRC, bg, bg_color, 1,
        &PixmanBox32 { x1: 0, y1: 0, x2: w, y2: h });

    let ascent = (*font).ascent;
    for i in 0..n_lines {
        let line = *lines.add(i as usize);
        // Row band for this line: background/highlight fills are confined here so
        // a later line cannot paint over an earlier line's highlight cell.
        let row_top = padding + i * line_height;
        let row_bot = (row_top + line_height).min(height);
        #[allow(clippy::cast_possible_wrap)]
        let baseline = ascent + row_top.cast_signed();
        if baseline < 0 { continue; }
        // Pass bg_color so draw_bg is enabled: this lets reverse-video runs paint
        // their highlight background via the per-glyph colour list.
        draw_text(
            line, padding, baseline.cast_unsigned(), fg, mask, bg,
            fg_color, bg_color, width, row_bot, row_top, 0, font,
            *colors.add(i as usize), *colors_lens.add(i as usize),
        );
    }

    // Stroke the 1px border frame on top of the background layer.
    if w >= 1 && h >= 1 && !border_color.is_null() {
        let frame = [
            PixmanBox32 { x1: 0,     y1: 0,     x2: w,     y2: 1 },   // top
            PixmanBox32 { x1: 0,     y1: h - 1, x2: w,     y2: h },   // bottom
            PixmanBox32 { x1: 0,     y1: 0,     x2: 1,     y2: h },   // left
            PixmanBox32 { x1: w - 1, y1: 0,     x2: w,     y2: h },   // right
        ];
        for b in &frame {
            pixman_image_fill_boxes(PIXMAN_OP_SRC, bg, border_color, 1, b);
        }
    }

    pixman_image_composite32(PIXMAN_OP_OVER, bg, ptr::null_mut(), final_img,
        0, 0, 0, 0, 0, 0, w, h);
    pixman_image_set_alpha_map(fg, mask, 0, 0);
    pixman_image_composite32(PIXMAN_OP_OVER, fg, mask, final_img,
        0, 0, 0, 0, 0, 0, w, h);

    pixman_image_unref(fg);
    pixman_image_unref(mask);
    pixman_image_unref(bg);
    pixman_image_unref(final_img);
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines, clippy::similar_names)]
pub unsafe fn draw_bar(
    pixels: *mut u32,
    width: u32,
    height: u32,
    stride: u32,
    font: *mut FcftFont,
    textpadding: u32,
    num_tags: u32,
    tag_names: *const *const c_char,
    mtags: u32,
    ctags: u32,
    urg: u32,
    sel: u32,
    hide_vacant: i32,
    layout: *const c_char,
    window_title: *const c_char,
    status_text: *const c_char,
    status_colors: *const RustTextColor,
    status_colors_l: u32,
    title_fg_override: *const RustColor,
    center_title: i32,
    active_color_title: i32,
    active_fg: *const RustColor,
    active_bg: *const RustColor,
    occupied_fg: *const RustColor,
    occupied_bg: *const RustColor,
    inactive_fg: *const RustColor,
    inactive_bg: *const RustColor,
    urgent_fg: *const RustColor,
    urgent_bg: *const RustColor,
    middle_bg: *const RustColor,
    middle_bg_sel: *const RustColor,
) {
    let w = width.cast_signed();
    let h = height.cast_signed();

    let final_img = pixman_image_create_bits(PIXMAN_A8R8G8B8, w, h, pixels, stride.cast_signed());
    let fg        = pixman_image_create_bits(PIXMAN_A8R8G8B8, w, h, ptr::null_mut(), 0);
    let mask      = pixman_image_create_bits(PIXMAN_A8,       w, h, ptr::null_mut(), 0);
    let bg        = pixman_image_create_bits(PIXMAN_A8R8G8B8, w, h, ptr::null_mut(), 0);

    if final_img.is_null() || fg.is_null() || mask.is_null() || bg.is_null() {
        eprintln!("[azoth] pixman_image_create_bits failed");
        if !fg.is_null()        { pixman_image_unref(fg); }
        if !mask.is_null()      { pixman_image_unref(mask); }
        if !bg.is_null()        { pixman_image_unref(bg); }
        if !final_img.is_null() { pixman_image_unref(final_img); }
        return;
    }

    pixman_image_fill_boxes(PIXMAN_OP_SRC, bg, inactive_bg, 1,
        &PixmanBox32 { x1: 0, y1: 0, x2: w, y2: h });

    let mut x: u32 = 0;
    let ascent  = (*font).ascent;
    let descent = (*font).descent;
    #[allow(clippy::cast_sign_loss)]
    let y: u32  = (height.cast_signed() + ascent - descent).cast_unsigned() / 2;
    #[allow(clippy::cast_sign_loss)]
    let boxs: u32 = (*font).height.cast_unsigned() / 9;
    #[allow(clippy::cast_sign_loss)]
    let boxw: u32 = (*font).height.cast_unsigned() / 6 + 2;

    for i in 0..num_tags {
        let active   = (mtags >> i) & 1 != 0;
        let occupied = (ctags >> i) & 1 != 0;
        let urgent_t = (urg   >> i) & 1 != 0;

        if hide_vacant != 0 && !active && !occupied && !urgent_t { continue; }

        let (fg_col, bg_col): (*const RustColor, *const RustColor) = match (urgent_t, active, occupied) {
            (true, _, _) => (urgent_fg,   urgent_bg),
            (_, true, _) => (active_fg,   active_bg),
            (_, _, true) => (occupied_fg, occupied_bg),
            _            => (inactive_fg, inactive_bg),
        };

        if hide_vacant == 0 && occupied {
            let white = RustColor { red: 0xFFFF, green: 0xFFFF, blue: 0xFFFF, alpha: 0xFFFF };
            let zero  = RustColor { red: 0, green: 0, blue: 0, alpha: 0 };
            let box_rect = PixmanBox32 {
                x1: (x + boxs).cast_signed(),        y1: boxs.cast_signed(),
                x2: (x + boxs + boxw).cast_signed(), y2: (boxs + boxw).cast_signed(),
            };
            pixman_image_fill_boxes(PIXMAN_OP_SRC, fg,   fg_col,          1, &raw const box_rect);
            pixman_image_fill_boxes(PIXMAN_OP_SRC, mask, &raw const white, 1, &raw const box_rect);
            if (sel == 0 || !active) && boxw >= 3 {
                let inner = PixmanBox32 {
                    x1: (x + boxs + 1).cast_signed(),          y1: (boxs + 1).cast_signed(),
                    x2: (x + boxs + boxw - 1).cast_signed(),   y2: (boxs + boxw - 1).cast_signed(),
                };
                pixman_image_fill_boxes(PIXMAN_OP_SRC, fg,   &raw const zero, 1, &raw const inner);
                pixman_image_fill_boxes(PIXMAN_OP_SRC, mask, &raw const zero, 1, &raw const inner);
            }
        }

        x = draw_text(
            *tag_names.add(i as usize), x, y, fg, mask, bg,
            fg_col, bg_col,
            width, height, 0, textpadding, font, ptr::null(), 0,
        );
    }

    if !layout.is_null() {
        x = draw_text(layout, x, y, fg, mask, bg,
            inactive_fg, inactive_bg,
            width, height, 0, textpadding, font, ptr::null(), 0);
    }

    let st = if status_text.is_null() { EMPTY.as_ptr().cast::<c_char>() } else { status_text };
    let status_w = text_width(st, width.saturating_sub(x), textpadding, font);
    if !status_text.is_null() {
        draw_text(
            status_text, width - status_w, y, fg, mask, bg,
            inactive_fg, inactive_bg,
            width, height, 0, textpadding, font,
            status_colors, status_colors_l,
        );
    }

    let title = if window_title.is_null() { EMPTY.as_ptr().cast::<c_char>() } else { window_title };

    let nx: u32 = if center_title != 0 {
        let tw = text_width(
            title,
            width.saturating_sub(status_w).saturating_sub(x),
            0, font,
        );
        let centered = width.saturating_sub(tw) / 2;
        let max_nx   = width.saturating_sub(status_w).saturating_sub(tw);
        centered.clamp(x, max_nx)
    } else {
        (x + textpadding).min(width.saturating_sub(status_w))
    };

    let mid: *const RustColor = if sel != 0 { middle_bg_sel } else { middle_bg };
    pixman_image_fill_boxes(PIXMAN_OP_SRC, bg, mid, 1,
        &PixmanBox32 { x1: x.cast_signed(), y1: 0, x2: nx.cast_signed(), y2: h });
    x = nx;

    let t_fg = if !title_fg_override.is_null() {
        title_fg_override
    } else if sel != 0 && active_color_title != 0 {
        active_fg
    } else {
        inactive_fg
    };
    let t_bg = if sel != 0 && active_color_title != 0 { active_bg } else { inactive_bg };
    x = draw_text(
        title, x, y, fg, mask, bg,
        t_fg, t_bg,
        width.saturating_sub(status_w), height, 0, 0, font,
        ptr::null(), 0,
    );

    pixman_image_fill_boxes(PIXMAN_OP_SRC, bg, mid, 1,
        &PixmanBox32 {
            x1: x.cast_signed(), y1: 0,
            x2: width.saturating_sub(status_w).cast_signed(), y2: h,
        });

    pixman_image_composite32(PIXMAN_OP_OVER, bg, ptr::null_mut(), final_img,
        0, 0, 0, 0, 0, 0, w, h);
    pixman_image_set_alpha_map(fg, mask, 0, 0);
    pixman_image_composite32(PIXMAN_OP_OVER, fg, mask, final_img,
        0, 0, 0, 0, 0, 0, w, h);

    pixman_image_unref(fg);
    pixman_image_unref(mask);
    pixman_image_unref(bg);
    pixman_image_unref(final_img);
}
