//! Lua config loading: the top-level driver, scalar / enum helpers, generic
//! value parsers, unknown-key validation, and [`Config::from_lua_globals`].

use smithay::reexports::input::{AccelProfile, ClickMethod, ScrollMethod, TapButtonMap};
use smithay::reexports::wayland_server::protocol::wl_output::Transform;

use super::{hex_color, Color, Config, LayoutDef, LayoutKind, MonitorRule, Rule, XkbRules};

// ─── loading ──────────────────────────────────────────────────────────────────

fn tilde_path(path: &std::path::Path) -> String {
    if let Ok(home) = std::env::var("HOME")
        && let Ok(stripped) = path.strip_prefix(&home)
    {
        return format!("~/{}", stripped.display());
    }
    path.display().to_string()
}

fn format_lua_error(e: &mlua::Error, path: &std::path::Path) -> String {
    let raw = e.to_string();
    let display = tilde_path(path);
    // mlua format with set_name("cfg"): [string "cfg"]:LINE: MESSAGE
    if let Some(rest) = raw.split_once("]:").map(|x| x.1)
        && let Some((line, msg)) = rest.split_once(": ")
    {
        return format!("syntax error: {display}:[{line}]: {msg}");
    }
    format!("syntax error: {display}: {raw}")
}

pub(super) fn load_from_lua(path: &std::path::Path) -> Result<(Config, Vec<String>), String> {
    let source = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let lua = mlua::Lua::new();
    // Register the `rwl` helper table before executing so config code and hook
    // callbacks can reference it.
    #[cfg(feature = "hooks")]
    crate::features::hooks::setup(&lua).map_err(|e| e.to_string())?;
    lua.load(&source).set_name("cfg").exec()
        .map_err(|e| format_lua_error(&e, path))?;
    let globals = lua.globals();
    let unknowns = collect_unknown_keys(&globals);
    let config = Config::from_lua_globals(&globals);
    // Keep the VM alive so user hook functions survive for event dispatch.
    // `globals` holds an independent handle, so the VM can be moved out here.
    #[cfg(feature = "hooks")]
    {
        drop(globals);
        crate::features::hooks::install(lua);
    }
    Ok((config, unknowns))
}

// ─── Unknown-key validation ───────────────────────────────────────────────────

// Lua 5.4 standard library names at the global level; anything matching these
// is skipped when scanning for user-supplied keys.
const LUA_BUILTINS: &[&str] = &[
    "_G", "_VERSION", "_ENV",
    "string", "table", "math", "io", "os", "coroutine", "package", "debug", "utf8",
    "print", "warn", "error", "assert", "type", "tostring", "tonumber",
    "pairs", "ipairs", "next", "select", "unpack",
    "require", "load", "loadfile", "dofile",
    "pcall", "xpcall",
    "setmetatable", "getmetatable",
    "rawget", "rawset", "rawequal", "rawlen",
    "collectgarbage",
];

const TOPLEVEL_KEYS: &[&str] = &[
    "tag_count", "rules", "layouts", "monitor_rules",
    "keys", "buttons", "auto_spawn", "startup_cmds", "bar_cmd",
    "bar", "windows", "effects", "keyboard", "mouse", "pertag_layouts",
    #[cfg(feature = "wallpaper")]
    "wallpaper",
    #[cfg(feature = "overview")]
    "overview",
    #[cfg(feature = "pip")]
    "pip",
    #[cfg(feature = "swallow")]
    "swallow",
    #[cfg(feature = "hooks")]
    "rwl",
];

#[cfg(feature = "overview")]
const OVERVIEW_KEYS: &[&str] = &[
    "hint_keys", "dim", "padding", "anim_ms", "highlight_color",
    "highlight_px", "border_color", "border_px",
];

#[cfg(feature = "pip")]
const PIP_KEYS: &[&str] = &[
    "width", "margin", "border_px", "border_color", "corner",
];

const WINDOWS_KEYS: &[&str] = &[
    "follow", "border_px", "border_color", "focus_color", "fullscreen_bg",
    "auto_back_empty_tag", "corner_radius", "gaps_px",
];

#[cfg(feature = "wallpaper")]
const WALLPAPER_KEYS: &[&str] = &[
    "tags", "mode", "default",
];

const EFFECTS_KEYS: &[&str] = &[
    "fade_in_ms", "fade_out_ms", "tag_transition", "tag_transition_ms",
];

const KEYBOARD_KEYS: &[&str] = &[
    "xkb_rules", "xkb_model", "xkb_layout", "xkb_variant", "xkb_options",
    "numlock", "capslock", "repeat_rate", "repeat_delay",
];

const MOUSE_KEYS: &[&str] = &[
    "mouse_focus", "warp_cursor",
    "tap_to_click", "tap_and_drag", "drag_lock", "natural_scrolling",
    "disable_while_typing", "left_handed", "middle_button_emulation",
    "scroll_method", "click_method", "accel_profile", "accel_speed",
    "tap_button_map", "cursor_timeout", "cursor_theme", "cursor_size",
];

#[cfg(feature = "bar")]
const BAR_KEYS: &[&str] = &[
    "font", "vertical_padding", "buffer_scale", "hidden", "bottom",
    "hide_vacant", "status_commands", "center_title", "active_color_title",
    "active_fg", "active_bg", "occupied_fg", "occupied_bg",
    "inactive_fg", "inactive_bg", "urgent_fg", "urgent_bg",
    "middle_bg", "middle_bg_selected", "blocks", "blocks_delim", "tag_names",
];

fn scan_keys(t: &mlua::Table, prefix: Option<&str>, known: &[&str], skip_builtins: bool, out: &mut Vec<String>) {
    for pair in t.pairs::<String, mlua::Value>() {
        let Ok((key, v)) = pair else { continue };
        if skip_builtins {
            if LUA_BUILTINS.contains(&key.as_str()) { continue; }
            if matches!(v, mlua::Value::Function(_)) { continue; }
        }
        if !known.contains(&key.as_str()) {
            out.push(match prefix {
                Some(p) => format!("{p}.{key}"),
                None    => key,
            });
        }
    }
}

fn collect_unknown_keys(g: &mlua::Table) -> Vec<String> {
    let mut out = Vec::new();
    scan_keys(g, None, TOPLEVEL_KEYS, true, &mut out);
    if let Ok(t) = g.get::<mlua::Table>("windows")  { scan_keys(&t, Some("windows"),  WINDOWS_KEYS,  false, &mut out); }
    #[cfg(feature = "wallpaper")]
    if let Ok(t) = g.get::<mlua::Table>("wallpaper") { scan_keys(&t, Some("wallpaper"), WALLPAPER_KEYS, false, &mut out); }
    if let Ok(t) = g.get::<mlua::Table>("effects")  { scan_keys(&t, Some("effects"),  EFFECTS_KEYS,  false, &mut out); }
    if let Ok(t) = g.get::<mlua::Table>("keyboard") { scan_keys(&t, Some("keyboard"), KEYBOARD_KEYS, false, &mut out); }
    if let Ok(t) = g.get::<mlua::Table>("mouse")    { scan_keys(&t, Some("mouse"),    MOUSE_KEYS,    false, &mut out); }
    #[cfg(feature = "bar")]
    if let Ok(t) = g.get::<mlua::Table>("bar")      { scan_keys(&t, Some("bar"),      BAR_KEYS,      false, &mut out); }
    #[cfg(feature = "overview")]
    if let Ok(t) = g.get::<mlua::Table>("overview") { scan_keys(&t, Some("overview"), OVERVIEW_KEYS, false, &mut out); }
    #[cfg(feature = "pip")]
    if let Ok(t) = g.get::<mlua::Table>("pip") { scan_keys(&t, Some("pip"), PIP_KEYS, false, &mut out); }
    out
}

// ─── Config assembly ──────────────────────────────────────────────────────────

impl Config {
    pub(super) fn from_lua_globals(g: &mlua::Table) -> Self {
        let defaults = Self::default();
        let tag_count = lua_u32(g, "tag_count", defaults.tag_count);
        let layouts   = lua_layouts(g, defaults.layouts);
        #[cfg(feature = "pertag-layouts")]
        let pertag_layouts = crate::features::pertag_layouts::lua_parse(g, &layouts, tag_count);
        let win: Option<mlua::Table> = g.get::<mlua::Table>("windows").ok();
        let w = win.as_ref();
        #[cfg(any(feature = "fade", feature = "tag-transition"))]
        let eff: Option<mlua::Table> = g.get::<mlua::Table>("effects").ok();
        #[cfg(any(feature = "fade", feature = "tag-transition"))]
        let ef = eff.as_ref();
        let kbd: Option<mlua::Table> = g.get::<mlua::Table>("keyboard").ok();
        let k = kbd.as_ref();
        let mse: Option<mlua::Table> = g.get::<mlua::Table>("mouse").ok();
        let m = mse.as_ref();
        #[cfg(feature = "wallpaper")]
        let wall: Option<mlua::Table> = g.get::<mlua::Table>("wallpaper").ok();
        #[cfg(feature = "wallpaper")]
        let wp = wall.as_ref();
        Self {
            mouse_focus:   m.map_or(defaults.mouse_focus, |t| lua_bool(t, "mouse_focus", defaults.mouse_focus)),
            #[cfg(feature = "warp")]
            warp_cursor:   m.map_or(defaults.warp_cursor, |t| lua_bool(t, "warp_cursor", defaults.warp_cursor)),
            #[cfg(feature = "auto-back-empty-tag")]
            auto_back_empty_tag: w.map_or(defaults.auto_back_empty_tag, |t| lua_bool(t, "auto_back_empty_tag", defaults.auto_back_empty_tag)),
            follow:        w.map_or(defaults.follow,        |t| lua_bool(t,  "follow",        defaults.follow)),
            border_px:     w.map_or(defaults.border_px,     |t| lua_u32( t,  "border_px",     defaults.border_px)),
            #[cfg(feature = "rounded-corners")]
            corner_radius: w.map_or(defaults.corner_radius, |t| lua_u32( t,  "corner_radius", defaults.corner_radius)),
            #[cfg(feature = "gaps")]
            gaps_px:       w.map_or(defaults.gaps_px,       |t| lua_u32( t,  "gaps_px",       defaults.gaps_px)),
            border_color:  w.map_or(defaults.border_color,  |t| lua_color(t, "border_color",  defaults.border_color)),
            focus_color:   w.map_or(defaults.focus_color,   |t| lua_color(t, "focus_color",   defaults.focus_color)),
            fullscreen_bg: w.map_or(defaults.fullscreen_bg, |t| lua_color(t, "fullscreen_bg", defaults.fullscreen_bg)),
            #[cfg(feature = "wallpaper")]
            wallpaper:      wp.and_then(|t| lua_str(t, "default")).or(defaults.wallpaper),
            #[cfg(feature = "wallpaper")]
            wallpapers:     wp.map_or(defaults.wallpapers, |t| lua_wallpapers(t, tag_count)),
            #[cfg(feature = "wallpaper")]
            wallpaper_mode: wp.map_or(defaults.wallpaper_mode, |t| lua_wallpaper_mode(t, defaults.wallpaper_mode)),
            #[cfg(feature = "fade")]
            fade_in_ms:    ef.map_or(defaults.fade_in_ms,    |t| lua_u32(t,   "fade_in_ms",    defaults.fade_in_ms)),
            #[cfg(feature = "fade")]
            fade_out_ms:   ef.map_or(defaults.fade_out_ms,   |t| lua_u32(t,   "fade_out_ms",   defaults.fade_out_ms)),
            #[cfg(feature = "tag-transition")]
            tag_transition:    ef.map_or(defaults.tag_transition,    |t| lua_bool(t, "tag_transition",    defaults.tag_transition)),
            #[cfg(feature = "tag-transition")]
            tag_transition_ms: ef.map_or(defaults.tag_transition_ms, |t| lua_u32( t, "tag_transition_ms", defaults.tag_transition_ms)),
            tag_count,
            #[cfg(feature = "pertag-layouts")]
            pertag_layouts,
            rules:         lua_rules(g, defaults.rules),
            layouts,
            monitor_rules: lua_monitor_rules(g, defaults.monitor_rules),
            xkb_rules:    if let Some(t) = k { lua_xkb_rules(t, defaults.xkb_rules) } else { defaults.xkb_rules },
            numlock:      k.map_or(defaults.numlock,      |t| lua_bool(t, "numlock",      defaults.numlock)),
            capslock:     k.map_or(defaults.capslock,     |t| lua_bool(t, "capslock",     defaults.capslock)),
            repeat_rate:  k.map_or(defaults.repeat_rate,  |t| lua_i32( t, "repeat_rate",  defaults.repeat_rate)),
            repeat_delay: k.map_or(defaults.repeat_delay, |t| lua_i32( t, "repeat_delay", defaults.repeat_delay)),
            tap_to_click:            m.map_or(defaults.tap_to_click,            |t| lua_bool(t, "tap_to_click",            defaults.tap_to_click)),
            tap_and_drag:            m.map_or(defaults.tap_and_drag,            |t| lua_bool(t, "tap_and_drag",            defaults.tap_and_drag)),
            drag_lock:               m.map_or(defaults.drag_lock,               |t| lua_bool(t, "drag_lock",               defaults.drag_lock)),
            natural_scrolling:       m.map_or(defaults.natural_scrolling,       |t| lua_bool(t, "natural_scrolling",       defaults.natural_scrolling)),
            disable_while_typing:    m.map_or(defaults.disable_while_typing,    |t| lua_bool(t, "disable_while_typing",    defaults.disable_while_typing)),
            left_handed:             m.map_or(defaults.left_handed,             |t| lua_bool(t, "left_handed",             defaults.left_handed)),
            middle_button_emulation: m.map_or(defaults.middle_button_emulation, |t| lua_bool(t, "middle_button_emulation", defaults.middle_button_emulation)),
            scroll_method:      m.map_or(defaults.scroll_method,      |t| lua_scroll_method(t, defaults.scroll_method)),
            click_method:       m.map_or(defaults.click_method,       |t| lua_click_method( t, defaults.click_method)),
            accel_profile:      m.map_or(defaults.accel_profile,      |t| lua_accel_profile(t, defaults.accel_profile)),
            accel_speed:        m.map_or(defaults.accel_speed,        |t| lua_f64(t, "accel_speed", defaults.accel_speed)),
            tap_button_map:     m.map_or(defaults.tap_button_map,     |t| lua_tap_button_map(t, defaults.tap_button_map)),
            cursor_timeout_secs: m.map_or(defaults.cursor_timeout_secs, |t| lua_u64(t, "cursor_timeout", defaults.cursor_timeout_secs)),
            cursor_theme: m.and_then(|t| lua_str(t, "cursor_theme")).or(defaults.cursor_theme),
            cursor_size:  m.map_or(defaults.cursor_size, |t| lua_u32(t, "cursor_size", defaults.cursor_size)),
            keys:         super::keybinds::lua_keys(g, defaults.keys),
            buttons:      super::keybinds::lua_buttons(g, defaults.buttons),
            auto_spawn:   lua_auto_spawn(g, defaults.auto_spawn),
            #[cfg(feature = "startup-cmds")]
            startup_cmds: crate::features::startup_cmds::lua_parse(g),
            bar_cmd:      lua_argv(g, "bar_cmd"),
            #[cfg(feature = "bar")]
            bar:          super::settings::lua_bar_settings(g),
            #[cfg(feature = "overview")]
            overview:     super::settings::lua_overview_settings(g),
            #[cfg(feature = "pip")]
            pip:          super::settings::lua_pip_settings(g),
            #[cfg(feature = "swallow")]
            swallow_enabled: crate::features::swallow::lua_enabled(g),
            #[cfg(feature = "swallow")]
            swallow_terminals: crate::features::swallow::lua_parse(g),
        }
    }
}

// ─── Lua scalar helpers ───────────────────────────────────────────────────────

pub(super) fn lua_bool(t: &mlua::Table, key: &str, default: bool) -> bool {
    t.get::<bool>(key).unwrap_or(default)
}

pub(super) fn lua_u32(t: &mlua::Table, key: &str, default: u32) -> u32 {
    t.get::<mlua::Value>(key)
        .ok()
        .and_then(|v| match v {
            mlua::Value::Integer(n) => u32::try_from(n).ok(),
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            mlua::Value::Number(f)  => Some(f as u32),
            _                       => None,
        })
        .unwrap_or(default)
}

pub(super) fn lua_i32(t: &mlua::Table, key: &str, default: i32) -> i32 {
    t.get::<mlua::Value>(key)
        .ok()
        .and_then(|v| match v {
            mlua::Value::Integer(n) => i32::try_from(n).ok(),
            #[allow(clippy::cast_possible_truncation)]
            mlua::Value::Number(f)  => Some(f as i32),
            _                       => None,
        })
        .unwrap_or(default)
}

fn lua_u64(t: &mlua::Table, key: &str, default: u64) -> u64 {
    t.get::<mlua::Value>(key)
        .ok()
        .and_then(|v| match v {
            mlua::Value::Integer(n) => u64::try_from(n).ok(),
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            mlua::Value::Number(f)  => Some(f as u64),
            _                       => None,
        })
        .unwrap_or(default)
}

pub(super) fn lua_f64(t: &mlua::Table, key: &str, default: f64) -> f64 {
    t.get::<mlua::Value>(key)
        .ok()
        .and_then(|v| match v {
            #[allow(clippy::cast_precision_loss)]
            mlua::Value::Integer(n) => Some(n as f64),
            mlua::Value::Number(f)  => Some(f),
            _                       => None,
        })
        .unwrap_or(default)
}

pub(super) fn lua_str(t: &mlua::Table, key: &str) -> Option<String> {
    t.get::<String>(key).ok()
}

#[allow(clippy::many_single_char_names)]
pub(super) fn lua_color(t: &mlua::Table, key: &str, default: Color) -> Color {
    if let Ok(mlua::Value::Integer(n)) = t.get::<mlua::Value>(key)
        && let Ok(hex) = u32::try_from(n)
    {
        return hex_color(hex);
    }
    if let Ok(arr) = t.get::<mlua::Table>(key) {
        let r = arr.get::<f32>(1).unwrap_or(default[0]);
        let g = arr.get::<f32>(2).unwrap_or(default[1]);
        let b = arr.get::<f32>(3).unwrap_or(default[2]);
        let a = arr.get::<f32>(4).unwrap_or(default[3]);
        return [r, g, b, a];
    }
    default
}

// ─── Lua enum helpers ─────────────────────────────────────────────────────────

fn lua_scroll_method(t: &mlua::Table, default: ScrollMethod) -> ScrollMethod {
    match t.get::<String>("scroll_method").as_deref() {
        Ok("no_scroll")      => ScrollMethod::NoScroll,
        Ok("two_finger")     => ScrollMethod::TwoFinger,
        Ok("edge")           => ScrollMethod::Edge,
        Ok("on_button_down") => ScrollMethod::OnButtonDown,
        _                    => default,
    }
}

fn lua_click_method(t: &mlua::Table, default: ClickMethod) -> ClickMethod {
    match t.get::<String>("click_method").as_deref() {
        Ok("button_areas") => ClickMethod::ButtonAreas,
        Ok("clickfinger")  => ClickMethod::Clickfinger,
        _                  => default,
    }
}

fn lua_accel_profile(t: &mlua::Table, default: AccelProfile) -> AccelProfile {
    match t.get::<String>("accel_profile").as_deref() {
        Ok("flat")     => AccelProfile::Flat,
        Ok("adaptive") => AccelProfile::Adaptive,
        _              => default,
    }
}

fn lua_tap_button_map(t: &mlua::Table, default: TapButtonMap) -> TapButtonMap {
    match t.get::<String>("tap_button_map").as_deref() {
        Ok("left_middle_right") => TapButtonMap::LeftMiddleRight,
        Ok("left_right_middle") => TapButtonMap::LeftRightMiddle,
        _                       => default,
    }
}

#[cfg(feature = "wallpaper")]
fn lua_wallpaper_mode(
    t: &mlua::Table,
    default: crate::features::wallpaper::WallpaperMode,
) -> crate::features::wallpaper::WallpaperMode {
    lua_str(t, "mode")
        .and_then(|s| crate::features::wallpaper::WallpaperMode::parse(&s))
        .unwrap_or(default)
}

/// Parse `wallpaper.tags = { [tag] = "path", … }` into a vec indexed by tag
/// position (tag 1 → index 0), length `tag_count`. Missing/empty entries are
/// `None`, meaning the tag falls back to the single `wallpaper.default` value.
#[cfg(feature = "wallpaper")]
fn lua_wallpapers(t: &mlua::Table, tag_count: u32) -> Vec<Option<String>> {
    let Ok(tbl) = t.get::<mlua::Table>("tags") else { return Vec::new() };
    (1..=tag_count)
        .map(|tag| {
            tbl.get::<Option<String>>(i64::from(tag))
                .ok()
                .flatten()
                .filter(|s| !s.is_empty())
        })
        .collect()
}

fn lua_xkb_rules(t: &mlua::Table, default: XkbRules) -> XkbRules {
    XkbRules {
        rules:   lua_str(t, "xkb_rules").or(default.rules),
        model:   lua_str(t, "xkb_model").or(default.model),
        layout:  lua_str(t, "xkb_layout").or(default.layout),
        variant: lua_str(t, "xkb_variant").or(default.variant),
        options: lua_str(t, "xkb_options").or(default.options),
    }
}

// ─── Lua rules ────────────────────────────────────────────────────────────────

fn lua_rules(t: &mlua::Table, default: Vec<Rule>) -> Vec<Rule> {
    let Ok(arr) = t.get::<mlua::Table>("rules") else { return default; };
    let out: Vec<Rule> = arr.sequence_values::<mlua::Table>()
        .filter_map(|v| {
            let tbl = v.ok()?;
            Some(Rule {
                id:           lua_str(&tbl, "id"),
                title:        lua_str(&tbl, "title"),
                tags:         lua_u32(&tbl, "tags", 0),
                switch_to_tag: lua_bool(&tbl, "switch_to_tag", false),
                is_floating:  lua_bool(&tbl, "floating", false),
                monitor:      lua_i32(&tbl, "monitor", -1),
                #[cfg(feature = "scratchpad")]
                scratch_key:  lua_str(&tbl, "scratch_key")
                                .and_then(|s| s.chars().next())
                                .unwrap_or('\0'),
                #[cfg(not(feature = "scratchpad"))]
                scratch_key: '\0',
                #[cfg(feature = "swallow")]
                no_swallow:   lua_bool(&tbl, "no_swallow", false),
                #[cfg(not(feature = "swallow"))]
                no_swallow:   false,
            })
        })
        .collect();
    if out.is_empty() { default } else { out }
}

// ─── Lua layouts ─────────────────────────────────────────────────────────────

fn parse_layout_kind(s: &str) -> Option<LayoutKind> {
    match s {
        #[cfg(feature = "tile")]
        "tile"     => Some(LayoutKind::Tile),
        #[cfg(feature = "monocle")]
        "monocle"  => Some(LayoutKind::Monocle),
        #[cfg(feature = "col")]
        "col"      => Some(LayoutKind::Col),
        #[cfg(feature = "scroll")]
        "scroll"   => Some(LayoutKind::Scroll),
        #[cfg(feature = "dwindle")]
        "dwindle"  => Some(LayoutKind::Dwindle),
        #[cfg(feature = "bstack")]
        "bstack"   => Some(LayoutKind::Bstack),
        #[cfg(feature = "centeredmaster")]
        "centeredmaster" => Some(LayoutKind::CenteredMaster),
        #[cfg(feature = "hooks")]
        "lua"      => Some(LayoutKind::Lua),
        _          => None,
    }
}

fn lua_layouts(t: &mlua::Table, default: Vec<LayoutDef>) -> Vec<LayoutDef> {
    let Ok(arr) = t.get::<mlua::Table>("layouts") else { return default; };
    let out: Vec<LayoutDef> = arr.sequence_values::<mlua::Table>()
        .filter_map(|v| {
            let tbl = v.ok()?;
            let symbol = lua_str(&tbl, "symbol")?;
            let kind = lua_str(&tbl, "kind")
                .and_then(|s| parse_layout_kind(&s))
                .unwrap_or_else(super::default_layout_kind);
            Some(LayoutDef { symbol, kind })
        })
        .collect();
    if out.is_empty() { default } else { out }
}

// ─── Lua monitor rules ────────────────────────────────────────────────────────

fn parse_transform(s: &str) -> Option<Transform> {
    match s {
        "normal"      => Some(Transform::Normal),
        "90"          => Some(Transform::_90),
        "180"         => Some(Transform::_180),
        "270"         => Some(Transform::_270),
        "flipped"     => Some(Transform::Flipped),
        "flipped90"   => Some(Transform::Flipped90),
        "flipped180"  => Some(Transform::Flipped180),
        "flipped270"  => Some(Transform::Flipped270),
        _             => None,
    }
}

/// Parse an optional `vrr = "off" | "on" | "ondemand"` monitor-rule field.
fn parse_vrr(s: Option<&str>) -> super::VrrMode {
    use super::VrrMode;
    match s {
        Some("on")       => VrrMode::On,
        Some("ondemand") => VrrMode::OnDemand,
        _                => VrrMode::Off,
    }
}

fn lua_monitor_rules(t: &mlua::Table, default: Vec<MonitorRule>) -> Vec<MonitorRule> {
    let Ok(arr) = t.get::<mlua::Table>("monitor_rules") else { return default; };
    let out: Vec<MonitorRule> = arr.sequence_values::<mlua::Table>()
        .filter_map(|v| {
            let tbl = v.ok()?;
            let transform = lua_str(&tbl, "transform")
                .and_then(|s| parse_transform(&s))
                .unwrap_or(Transform::Normal);
            Some(MonitorRule {
                name:       lua_str(&tbl, "name"),
                mfact:      lua_f64(&tbl, "mfact", 0.55),
                nmaster:    lua_i32(&tbl, "nmaster", 1),
                scale:      lua_f64(&tbl, "scale", 1.0),
                layout_idx: lua_u32(&tbl, "layout", 0) as usize,
                transform,
                x: lua_i32(&tbl, "x", -1),
                y: lua_i32(&tbl, "y", -1),
                vrr: parse_vrr(lua_str(&tbl, "vrr").as_deref()),
            })
        })
        .collect();
    if out.is_empty() { default } else { out }
}

// ─── Lua argv / auto-spawn ────────────────────────────────────────────────────

pub(super) fn lua_str_array(tbl: &mlua::Table, key: &str) -> Vec<String> {
    let Ok(arr) = tbl.get::<mlua::Table>(key) else { return Vec::new() };
    arr.sequence_values::<String>()
        .filter_map(std::result::Result::ok)
        .collect()
}

fn lua_auto_spawn(t: &mlua::Table, default: Vec<Option<Vec<String>>>) -> Vec<Option<Vec<String>>> {
    let Ok(arr) = t.get::<mlua::Table>("auto_spawn") else { return default };
    let len = arr.raw_len();
    if len == 0 { return default; }
    (1..=len)
        .map(|i| {
            let idx = i64::try_from(i).unwrap_or(i64::MAX);
            // Each slot may be nil, a bare string, or a table of strings.
            if let Ok(Some(s)) = arr.get::<Option<String>>(idx) {
                return Some(vec![s]);
            }
            if let Ok(tbl) = arr.get::<mlua::Table>(idx) {
                let n = tbl.raw_len();
                let cmd: Vec<String> = (1..=n)
                    .filter_map(|j| tbl.get::<String>(i64::try_from(j).unwrap_or(i64::MAX)).ok())
                    .collect();
                if !cmd.is_empty() { return Some(cmd); }
            }
            None
        })
        .collect()
}

/// Read a single argv from the table: either a bare string or a table of strings.
fn lua_argv(t: &mlua::Table, key: &str) -> Option<Vec<String>> {
    if let Ok(Some(s)) = t.get::<Option<String>>(key) {
        return Some(vec![s]);
    }
    if let Ok(tbl) = t.get::<mlua::Table>(key) {
        let n = tbl.raw_len();
        let cmd: Vec<String> = (1..=n)
            .filter_map(|j| tbl.get::<String>(i64::try_from(j).unwrap_or(i64::MAX)).ok())
            .collect();
        if !cmd.is_empty() { return Some(cmd); }
    }
    None
}
