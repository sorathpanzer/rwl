//! Parse status text with inline ^cmd(arg) color/button commands.

use super::{
    color::parse_color,
    config::Config,
    ffi::RustTextColor,
    text::{CustomText, TEXT_MAX},
};

/// Parse inline ^cmd(arg) commands, building color override list.
/// Button x-positions are not tracked (requires font metrics; done in C).
pub fn parse_into_customtext(text: &str, cfg: &Config) -> CustomText {
    if !cfg.status_commands {
        return CustomText { text: text.chars().take(TEXT_MAX - 1).collect(), colors: vec![] };
    }

    let bytes = text.as_bytes();
    let mut i = 0usize;
    let mut out_text = String::new();
    let mut colors: Vec<RustTextColor> = Vec::new();

    while i < bytes.len() && out_text.len() < TEXT_MAX - 1 {
        if bytes[i] == b'^' {
            i += 1;
            if i >= bytes.len() { break; }
            if bytes[i] != b'^' {
                if let Some((cmd, arg, consumed)) = parse_command(&bytes[i..]) {
                    #[allow(clippy::cast_possible_truncation)]
                    let byte_pos = out_text.len() as u32;
                    match cmd {
                        "bg" => {
                            let color = if arg.is_empty() { cfg.inactive_bg_color }
                                        else { parse_color(arg).unwrap_or(cfg.inactive_bg_color) };
                            colors.push(RustTextColor { color, is_bg: 1, start_byte: byte_pos });
                        }
                        "fg" => {
                            let color = if arg.is_empty() { cfg.inactive_fg_color }
                                        else { parse_color(arg).unwrap_or(cfg.inactive_fg_color) };
                            colors.push(RustTextColor { color, is_bg: 0, start_byte: byte_pos });
                        }
                        _ => {}
                    }
                    i += consumed;
                    continue;
                }
                continue;
            }
        }
        let Some(ch) = text[i..].chars().next() else { break; };
        out_text.push(ch);
        i += ch.len_utf8();
    }

    CustomText { text: out_text, colors }
}

fn parse_command(src: &[u8]) -> Option<(&str, &str, usize)> {
    let paren_open  = src.iter().position(|&b| b == b'(')?;
    let paren_close = src[paren_open+1..].iter().position(|&b| b == b')')
        .map(|p| p + paren_open + 1)?;
    let cmd = std::str::from_utf8(&src[..paren_open]).ok()?;
    let arg = std::str::from_utf8(&src[paren_open+1..paren_close]).ok()?;
    Some((cmd, arg, paren_close + 1))
}
