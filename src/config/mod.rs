//! Compositor configuration loaded from `~/.config/rwl/config.lua` at start-up.
//!
//! Every field falls back to the compiled-in default when the Lua file is
//! absent or when a particular key is missing / of the wrong type.
//!
//! The module is split into:
//! - [`types`]    — core data types and their default data,
//! - [`settings`] — optional feature settings (overview / PiP / bar),
//! - [`keybinds`] — default bindings, keysym resolution and binding parsing,
//! - [`lua`]      — Lua loading, scalar/enum helpers and [`Config`] assembly.

#![allow(clippy::struct_excessive_bools)]

mod keybinds;
mod lua;
mod settings;
mod types;

pub use types::*;

// Feature settings types live with their features; re-export them here so the
// rest of the config module (and `crate::config::*` consumers) can name them.
#[cfg(feature = "overview")]
pub use crate::features::overview::OverviewSettings;
#[cfg(feature = "pip")]
pub use crate::features::pip::PipSettings;
#[cfg(feature = "bar")]
pub use crate::features::bar::BarSettings;

use std::path::PathBuf;
use std::sync::{OnceLock, RwLock, RwLockReadGuard};

use smithay::reexports::input::{AccelProfile, ClickMethod, ScrollMethod, TapButtonMap};

use keybinds::{default_buttons, default_keys};
use types::{default_auto_spawn, default_layouts, default_monitor_rules, default_rules};

// ─── Config struct ────────────────────────────────────────────────────────────

pub struct Config {
    // Appearance
    pub mouse_focus:               bool,
    #[cfg(feature = "warp")]
    pub warp_cursor:               bool,
    pub follow:                     bool,
    pub border_px:                  u32,
    #[cfg(feature = "rounded-corners")]
    pub corner_radius:              u32,
    #[cfg(feature = "gaps")]
    pub gaps_px:                    u32,
    pub border_color:               Color,
    pub focus_color:                Color,
    pub fullscreen_bg:              Color,
    /// Desktop wallpaper image path (`~` expanded). Used as the fallback for any
    /// tag without a `wallpaper.tags` entry (`wallpaper.default` in Lua). `None`
    /// disables it.
    #[cfg(feature = "wallpaper")]
    pub wallpaper:                  Option<String>,
    /// Per-tag wallpaper image paths, indexed by tag bit position (tag 1 = index
    /// 0). `None` at an index means that tag uses the `wallpaper.default`
    /// fallback. Empty when no per-tag wallpapers are configured.
    #[cfg(feature = "wallpaper")]
    pub wallpapers:                 Vec<Option<String>>,
    /// How the wallpaper image is fitted to each output.
    #[cfg(feature = "wallpaper")]
    pub wallpaper_mode:             crate::features::wallpaper::WallpaperMode,
    #[cfg(feature = "fade")]
    pub fade_in_ms:                 u32,
    #[cfg(feature = "fade")]
    pub fade_out_ms:                u32,
    #[cfg(feature = "tag-transition")]
    pub tag_transition:             bool,
    #[cfg(feature = "tag-transition")]
    pub tag_transition_ms:          u32,
    // Tags
    #[cfg(feature = "auto-back-empty-tag")]
    pub auto_back_empty_tag: bool,
    pub tag_count: u32,
    /// Per-tag layout assignment. Indexed by tag bit position (tag 1 = index 0).
    /// `None` means no override — the monitor's current layout is kept.
    #[cfg(feature = "pertag-layouts")]
    pub pertag_layouts: Vec<Option<PertagRule>>,
    // Passthrough
    // Rules / layouts / monitors
    pub rules:         Vec<Rule>,
    pub layouts:       Vec<LayoutDef>,
    pub monitor_rules: Vec<MonitorRule>,
    // Keyboard
    pub xkb_rules:    XkbRules,
    pub numlock:      bool,
    pub capslock:     bool,
    pub repeat_rate:  i32,
    pub repeat_delay: i32,
    // Pointer / libinput
    pub tap_to_click:           bool,
    pub tap_and_drag:           bool,
    pub drag_lock:              bool,
    pub natural_scrolling:      bool,
    pub disable_while_typing:   bool,
    pub left_handed:            bool,
    pub middle_button_emulation: bool,
    pub scroll_method:          ScrollMethod,
    pub click_method:           ClickMethod,
    pub accel_profile:          AccelProfile,
    pub accel_speed:            f64,
    pub tap_button_map:         TapButtonMap,
    pub cursor_timeout_secs:    u64,
    pub cursor_theme:           Option<String>,
    pub cursor_size:            u32,
    // Bindings
    pub keys:    Vec<KeyBind>,
    pub buttons: Vec<ButtonBind>,
    // Auto-spawn — each slot is an argv list (None = no auto-spawn for that tag).
    pub auto_spawn: Vec<Option<Vec<String>>>,
    // Commands run once at compositor startup (order preserved).
    #[cfg(feature = "startup-cmds")]
    pub startup_cmds: Vec<Vec<String>>,
    // Status bar command — receives IPC output on stdin (like `-s`).
    pub bar_cmd: Option<Vec<String>>,
    // Embedded bar settings (only meaningful when `bar` feature is enabled).
    #[cfg(feature = "bar")]
    pub bar: BarSettings,
    // Workspace-overview (Exposé) settings.
    #[cfg(feature = "overview")]
    pub overview: OverviewSettings,
    // Picture-in-Picture settings.
    #[cfg(feature = "pip")]
    pub pip: PipSettings,
}

// ─── Default ─────────────────────────────────────────────────────────────────

impl Default for Config {
    fn default() -> Self {
        Self {
            mouse_focus:               true,
            #[cfg(feature = "warp")]
            warp_cursor:               true,
            follow:                     true,
            border_px:                  1,
            #[cfg(feature = "rounded-corners")]
            corner_radius:              8,
            #[cfg(feature = "gaps")]
            gaps_px:                    0,
            border_color: hex_color(0x2222_22ff),
            // focus_color:  hex_color(0xdf73_ffff),
            focus_color:  hex_color(0xffff_ffff),
            fullscreen_bg: [0.0, 0.0, 0.0, 1.0],
            #[cfg(feature = "wallpaper")]
            wallpaper:      None,
            #[cfg(feature = "wallpaper")]
            wallpapers:     Vec::new(),
            #[cfg(feature = "wallpaper")]
            wallpaper_mode: crate::features::wallpaper::WallpaperMode::default(),
            #[cfg(feature = "fade")]
            fade_in_ms:   0,
            #[cfg(feature = "fade")]
            fade_out_ms:  0,
            #[cfg(feature = "tag-transition")]
            tag_transition:    true,
            #[cfg(feature = "tag-transition")]
            tag_transition_ms: 250,
            #[cfg(feature = "auto-back-empty-tag")]
            auto_back_empty_tag: true,
            tag_count:          6,
            #[cfg(feature = "pertag-layouts")]
            pertag_layouts:        vec![Some(PertagRule::Fixed(0)); 6], // tile (index 0) for all tags by default
            rules:         default_rules(),
            layouts:       default_layouts(),
            monitor_rules: default_monitor_rules(),
            xkb_rules: XkbRules {
                rules:   None,
                model:   None,
                layout:  Some("pt".into()),
                variant: None,
                options: None,
            },
            numlock:      true,
            capslock:     false,
            repeat_rate:  25,
            repeat_delay: 600,
            tap_to_click:            true,
            tap_and_drag:            true,
            drag_lock:               true,
            natural_scrolling:       true,
            disable_while_typing:    true,
            left_handed:             false,
            middle_button_emulation: false,
            scroll_method: ScrollMethod::TwoFinger,
            click_method:  ClickMethod::ButtonAreas,
            accel_profile: AccelProfile::Adaptive,
            accel_speed:   0.0,
            tap_button_map: TapButtonMap::LeftRightMiddle,
            cursor_timeout_secs: 5,
            cursor_theme: None,
            cursor_size: 24,
            keys:         default_keys(),
            buttons:      default_buttons(),
            auto_spawn:   default_auto_spawn(),
            #[cfg(feature = "startup-cmds")]
            startup_cmds: Vec::new(),
            bar_cmd:      None,
            #[cfg(feature = "bar")]
            bar:          BarSettings::default(),
            #[cfg(feature = "overview")]
            overview:     OverviewSettings::default(),
            #[cfg(feature = "pip")]
            pip:          PipSettings::default(),
        }
    }
}

// ─── Global accessor ─────────────────────────────────────────────────────────

static CONFIG: OnceLock<RwLock<Config>> = OnceLock::new();
static CONFIG_ERROR: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Modifier bitmask for the super / logo key (must match `mods_to_bits`).
pub const MODKEY: u32 = 1 << 6;

/// Sentinel keysym for the mod-tap gesture, produced by an empty `key = ""` in a
/// keybind. Equals XKB `NoSymbol` (0), which real key events never match in the
/// keybinding filter (they are forwarded early), so this only ever fires via the
/// tap-modkey handler.
pub const MOD_TAP_KEYSYM: u32 = 0;

/// Return the last config error string, if any.  `None` means the config
/// loaded cleanly (or no config file was found).  Cleared on each successful
/// load so callers always see the current state.
pub fn config_error() -> Option<String> {
    CONFIG_ERROR.lock().ok()?.clone()
}

#[inline]
fn config_lock() -> &'static RwLock<Config> {
    CONFIG.get_or_init(|| RwLock::new(Config::default()))
}

/// Return a read guard on the compositor configuration.
#[inline]
pub fn get() -> RwLockReadGuard<'static, Config> {
    config_lock().read().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Load configuration from `~/.config/rwl/config.lua`.
/// Falls back to compiled-in defaults on any error.
pub fn init() {
    let cfg = load_config();
    if let Ok(mut w) = config_lock().write() {
        *w = cfg;
    }
}

/// Reload configuration from `~/.config/rwl/config.lua` at runtime.
/// Falls back to compiled-in defaults on any error.
pub fn reload() {
    let cfg = load_config();
    if let Ok(mut w) = config_lock().write() {
        *w = cfg;
        tracing::info!("Config reloaded from {}", config_path().display());
    }
    // The runtime wallpaper override (rwl msg / Lua hook) deliberately survives
    // reload; only forget paths that previously failed to decode so fixed paths
    // are retried.
    #[cfg(feature = "wallpaper")]
    crate::features::wallpaper::on_reload();
}

fn load_config() -> Config {
    let path = config_path();
    if path.exists() {
        match lua::load_from_lua(&path) {
            Ok((c, unknowns)) => {
                tracing::info!(
                    "Config loaded: {} keybinds, {} rules",
                    c.keys.len(),
                    c.rules.len()
                );
                if let Ok(mut e) = CONFIG_ERROR.lock() {
                    *e = if unknowns.is_empty() {
                        None
                    } else {
                        let msg = unknowns.join(", ");
                        tracing::warn!("Unknown config keys: {msg}");
                        Some(format!("unknown keys: {msg}"))
                    };
                }
                c
            }
            Err(e) => {
                tracing::error!("{e}");
                eprintln!("{e}");
                if let Ok(mut err) = CONFIG_ERROR.lock() {
                    *err = Some(e);
                }
                Config::default()
            }
        }
    } else {
        tracing::warn!("Config file not found at {}, using defaults", path.display());
        if let Ok(mut e) = CONFIG_ERROR.lock() { *e = None; }
        Config::default()
    }
}

/// Bitmask covering all tags (computed from `tag_count`).
#[inline]
pub fn tag_mask() -> u32 {
    (1 << get().tag_count) - 1
}

fn config_path() -> PathBuf {
    std::env::var("HOME").map_or_else(
        |_| PathBuf::from("/root/.config/rwl/config.lua"),
        |h| PathBuf::from(h).join(".config/rwl/config.lua"),
    )
}
