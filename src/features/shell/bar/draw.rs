//! Drawing — allocates an SHM file and delegates to `azoth_render` for rendering.

use std::os::unix::io::OwnedFd;

use rustix::fs::ftruncate;
use rustix::io::Errno;

use azoth_render::{BarRenderParams, Font, RustColor};

use super::config::Config;
use super::text::CustomText;

// ── SHM allocation ────────────────────────────────────────────────────────────

/// Create an anonymous in-memory file of `size` bytes.
pub fn allocate_shm_file(size: u64) -> Result<OwnedFd, Errno> {
    let fd = create_anon_file()?;
    loop {
        match ftruncate(&fd, size) {
            Ok(()) => break,
            Err(Errno::INTR) => {}
            Err(e) => return Err(e),
        }
    }
    Ok(fd)
}

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
fn create_anon_file() -> Result<OwnedFd, Errno> {
    use rustix::fs::{memfd_create, MemfdFlags};
    memfd_create("surface", MemfdFlags::CLOEXEC)
}

#[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
fn create_anon_file() -> Result<OwnedFd, Errno> {
    use rustix::fs::{open, unlink, Mode, OFlags};

    let dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
    let path = format!("{}/bar-shm-{}", dir, std::process::id());
    let fd = open(&path, OFlags::RDWR | OFlags::CREATE | OFlags::EXCL, Mode::RUSR | Mode::WUSR)?;
    let _ = unlink(&path);
    Ok(fd)
}

// ── Parameters for a single bar draw ─────────────────────────────────────────

pub struct BarDrawParams<'a> {
    pub width:        u32,
    pub height:       u32,
    pub stride:       u32,
    pub textpadding:  u32,
    pub mtags:        u32,
    pub ctags:        u32,
    pub urg:          u32,
    pub sel:          u32,
    pub layout:       Option<&'a str>,
    pub window_title:      Option<&'a str>,
    pub title_fg_override: Option<RustColor>,
    pub status:            &'a CustomText,
    pub font:         &'a Font,
    pub config:       &'a Config,
    pub tags:         &'a [String],
}

// ── Main render call ──────────────────────────────────────────────────────────

pub fn render_frame(pixels: &mut [u32], p: &BarDrawParams<'_>) {
    let cfg = p.config;
    azoth_render::render_bar(pixels, &BarRenderParams {
        width:        p.width,
        height:       p.height,
        stride:       p.stride,
        textpadding:  p.textpadding,
        mtags:        p.mtags,
        ctags:        p.ctags,
        urg:          p.urg,
        sel:          p.sel,
        layout:       p.layout,
        window_title: p.window_title,
        title_fg: p.title_fg_override.as_ref(),
        status_text:   &p.status.text,
        status_colors: &p.status.colors[..],
        hide_vacant:        cfg.hide_vacant,
        center_title:       cfg.center_title,
        active_color_title: cfg.active_color_title,
        active_fg:   &cfg.active_fg_color,
        active_bg:   &cfg.active_bg_color,
        occupied_fg: &cfg.occupied_fg_color,
        occupied_bg: &cfg.occupied_bg_color,
        inactive_fg: &cfg.inactive_fg_color,
        inactive_bg: &cfg.inactive_bg_color,
        urgent_fg:   &cfg.urgent_fg_color,
        urgent_bg:   &cfg.urgent_bg_color,
        middle_bg:    &cfg.middle_bg_color,
        middle_bg_sel: &cfg.middle_bg_color_selected,
        tags: p.tags,
        font: p.font,
    });
}
