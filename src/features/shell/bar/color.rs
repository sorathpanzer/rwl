use super::ffi::RustColor;

pub fn parse_color(s: &str) -> Result<RustColor, &'static str> {
    let s = s.strip_prefix('#').unwrap_or(s);
    if (s.len() != 6 && s.len() != 8)
        || !s.chars().next().is_some_and(|c| c.is_ascii_hexdigit())
    {
        return Err("invalid color string");
    }
    let parsed = u32::from_str_radix(s, 16).map_err(|_| "invalid hex")?;
    let (alpha, rgb) = if s.len() == 8 {
        let a = ((parsed & 0xff) as u16).wrapping_mul(0x101);
        (a, parsed >> 8)
    } else {
        (0xffff, parsed)
    };
    Ok(RustColor {
        red:   (((rgb >> 16) & 0xff) as u16).wrapping_mul(0x101),
        green: (((rgb >>  8) & 0xff) as u16).wrapping_mul(0x101),
        blue:  ((rgb & 0xff) as u16).wrapping_mul(0x101),
        alpha,
    })
}
