//! Minimal embedded 5×7 bitmap font used to draw window hint labels in the
//! overview. Self-contained (no fcft / `bar` dependency) so hint badges work on
//! a bare TTY. Covers `a`–`z` and `0`–`9`; unknown characters render blank.

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::memory::{
    MemoryRenderBuffer, MemoryRenderBufferRenderElement,
};
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::utils::{Physical, Point, Transform};

use crate::config::Color;

const GLYPH_W: usize = 5;
const GLYPH_H: usize = 7;

/// 7 rows per glyph, low 5 bits used (bit 4 = leftmost column).
#[allow(clippy::unreadable_literal)]
const fn glyph(c: char) -> [u8; GLYPH_H] {
    match c {
        'a' => [0b00000, 0b00000, 0b01110, 0b00001, 0b01111, 0b10001, 0b01111],
        'b' => [0b10000, 0b10000, 0b10110, 0b11001, 0b10001, 0b10001, 0b11110],
        'c' => [0b00000, 0b00000, 0b01110, 0b10001, 0b10000, 0b10001, 0b01110],
        'd' => [0b00001, 0b00001, 0b01101, 0b10011, 0b10001, 0b10001, 0b01111],
        'e' => [0b00000, 0b00000, 0b01110, 0b10001, 0b11111, 0b10000, 0b01110],
        'f' => [0b00110, 0b01001, 0b01000, 0b11100, 0b01000, 0b01000, 0b01000],
        'g' => [0b00000, 0b01111, 0b10001, 0b10001, 0b01111, 0b00001, 0b01110],
        'h' => [0b10000, 0b10000, 0b10110, 0b11001, 0b10001, 0b10001, 0b10001],
        'i' => [0b00100, 0b00000, 0b01100, 0b00100, 0b00100, 0b00100, 0b01110],
        'j' => [0b00010, 0b00000, 0b00110, 0b00010, 0b00010, 0b10010, 0b01100],
        'k' => [0b10000, 0b10000, 0b10010, 0b10100, 0b11000, 0b10100, 0b10010],
        'l' => [0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110],
        'm' => [0b00000, 0b00000, 0b11010, 0b10101, 0b10101, 0b10101, 0b10101],
        'n' => [0b00000, 0b00000, 0b10110, 0b11001, 0b10001, 0b10001, 0b10001],
        'o' => [0b00000, 0b00000, 0b01110, 0b10001, 0b10001, 0b10001, 0b01110],
        'p' => [0b00000, 0b11110, 0b10001, 0b10001, 0b11110, 0b10000, 0b10000],
        'q' => [0b00000, 0b01101, 0b10011, 0b10001, 0b01111, 0b00001, 0b00001],
        'r' => [0b00000, 0b00000, 0b10110, 0b11001, 0b10000, 0b10000, 0b10000],
        's' => [0b00000, 0b00000, 0b01111, 0b10000, 0b01110, 0b00001, 0b11110],
        't' => [0b01000, 0b01000, 0b11100, 0b01000, 0b01000, 0b01001, 0b00110],
        'u' => [0b00000, 0b00000, 0b10001, 0b10001, 0b10001, 0b10011, 0b01101],
        'v' => [0b00000, 0b00000, 0b10001, 0b10001, 0b10001, 0b01010, 0b00100],
        'w' => [0b00000, 0b00000, 0b10001, 0b10101, 0b10101, 0b10101, 0b01010],
        'x' => [0b00000, 0b00000, 0b10001, 0b01010, 0b00100, 0b01010, 0b10001],
        'y' => [0b00000, 0b10001, 0b10001, 0b01111, 0b00001, 0b00001, 0b01110],
        'z' => [0b00000, 0b00000, 0b11111, 0b00010, 0b00100, 0b01000, 0b11111],
        '0' => [0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110],
        '1' => [0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110],
        '2' => [0b01110, 0b10001, 0b00001, 0b00110, 0b01000, 0b10000, 0b11111],
        '3' => [0b11111, 0b00010, 0b00100, 0b00010, 0b00001, 0b10001, 0b01110],
        '4' => [0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010],
        '5' => [0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110],
        '6' => [0b00110, 0b01000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110],
        '7' => [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000],
        '8' => [0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110],
        '9' => [0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00010, 0b01100],
        _ => [0; GLYPH_H],
    }
}

/// Pack a `Color` (linear RGBA floats) into a native-endian `0xAARRGGBB` u32,
/// matching `Fourcc::Argb8888` on little-endian (same convention as the cursor
/// bitmap in `render.rs`).
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn pack(c: Color) -> u32 {
    let ch = |f: f32| (f.clamp(0.0, 1.0) * 255.0).round() as u32;
    (ch(c[3]) << 24) | (ch(c[0]) << 16) | (ch(c[1]) << 8) | ch(c[2])
}

/// Rasterize `text` into a badge (solid `bg` panel with `fg` glyphs) and return
/// it as a placed memory render element.
///
/// `loc` is the badge's top-left in physical coordinates; `scale` is the output
/// scale used to size the glyph pixels so the badge stays legible on `HiDPI`.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::many_single_char_names,
)]
pub fn hint_badge(
    renderer: &mut GlesRenderer,
    text: &str,
    loc: Point<i32, Physical>,
    bg: Color,
    fg: Color,
    scale: f64,
) -> Option<MemoryRenderBufferRenderElement<GlesRenderer>> {
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return None;
    }

    // Physical pixels per font pixel (min 2 so it never vanishes).
    let px = ((3.0 * scale).round() as usize).max(2);
    let pad = 2 * px;
    let gap = px; // spacing between glyphs
    let gw = GLYPH_W * px;
    let gh = GLYPH_H * px;

    let n = chars.len();
    let w = pad * 2 + n * gw + (n.saturating_sub(1)) * gap;
    let h = pad * 2 + gh;

    let bg_u = pack(bg);
    let fg_u = pack(fg);
    let mut pixels = vec![bg_u; w * h];

    for (ci, &c) in chars.iter().enumerate() {
        let g = glyph(c);
        let x0 = pad + ci * (gw + gap);
        for (row, bits) in g.iter().enumerate() {
            for col in 0..GLYPH_W {
                if (bits >> (GLYPH_W - 1 - col)) & 1 == 0 {
                    continue;
                }
                // Fill the px×px block for this font pixel.
                for dy in 0..px {
                    let y = pad + row * px + dy;
                    let base = y * w + x0 + col * px;
                    for dx in 0..px {
                        pixels[base + dx] = fg_u;
                    }
                }
            }
        }
    }

    let mut bytes = Vec::with_capacity(pixels.len() * 4);
    for p in &pixels {
        bytes.extend_from_slice(&p.to_ne_bytes());
    }

    let buffer = MemoryRenderBuffer::from_slice(
        &bytes,
        Fourcc::Argb8888,
        (w as i32, h as i32),
        1,
        Transform::Normal,
        None,
    );

    // Offset the badge a few pixels into the cell so it doesn't sit on the edge.
    let off = Point::from((px as i32, px as i32));
    MemoryRenderBufferRenderElement::from_buffer(
        renderer,
        (loc + off).to_f64(),
        &buffer,
        None,
        None,
        None,
        Kind::Unspecified,
    )
    .ok()
}
