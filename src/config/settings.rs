//! Lua parsing for the optional feature settings blocks (overview,
//! Picture-in-Picture, bar). The setting *types* and their compiled-in defaults
//! live with their features in `crate::features::{overview, pip, bar}`; this
//! module only reads them from the Lua config.

#[cfg(any(feature = "overview", feature = "pip", feature = "bar"))]
use super::lua::{lua_bool, lua_color, lua_str, lua_u32};

#[cfg(feature = "overview")]
use crate::features::overview::OverviewSettings;
#[cfg(feature = "pip")]
use crate::features::pip::PipSettings;
#[cfg(feature = "bar")]
use crate::features::bar::{BarSettings, BlockSettings};

// ─── Overview (Exposé) settings ──────────────────────────────────────────────

#[cfg(feature = "overview")]
pub(super) fn lua_overview_settings(g: &mlua::Table) -> OverviewSettings {
    let defaults = OverviewSettings::default();
    let Ok(t) = g.get::<mlua::Table>("overview") else { return defaults };
    OverviewSettings {
        hint_keys:       lua_str(&t, "hint_keys").unwrap_or(defaults.hint_keys),
        dim:             lua_color(&t, "dim", defaults.dim),
        padding:         lua_u32(&t, "padding", defaults.padding),
        anim_ms:         lua_u32(&t, "anim_ms", defaults.anim_ms),
        highlight_color: lua_color(&t, "highlight_color", defaults.highlight_color),
        highlight_px:    lua_u32(&t, "highlight_px", defaults.highlight_px),
        border_color:    lua_color(&t, "border_color", defaults.border_color),
        border_px:       lua_u32(&t, "border_px", defaults.border_px),
    }
}

// ─── Picture-in-Picture settings ─────────────────────────────────────────────

#[cfg(feature = "pip")]
pub(super) fn lua_pip_settings(g: &mlua::Table) -> PipSettings {
    let defaults = PipSettings::default();
    let Ok(t) = g.get::<mlua::Table>("pip") else { return defaults };
    PipSettings {
        width:        lua_u32(&t, "width", defaults.width),
        margin:       lua_u32(&t, "margin", defaults.margin),
        border_px:    lua_u32(&t, "border_px", defaults.border_px),
        border_color: lua_color(&t, "border_color", defaults.border_color),
        corner:       lua_str(&t, "corner").unwrap_or(defaults.corner),
    }
}

// ─── Bar settings (passed to the embedded bar thread) ────────────────────────

#[cfg(feature = "bar")]
pub(super) fn lua_bar_settings(g: &mlua::Table) -> BarSettings {
    let d = BarSettings::default();
    let Ok(tbl) = g.get::<mlua::Table>("bar") else { return d; };
    BarSettings {
        font:               lua_str(&tbl, "font").unwrap_or(d.font),
        vertical_padding:   lua_u32(&tbl, "vertical_padding", d.vertical_padding),
        buffer_scale:       lua_u32(&tbl, "buffer_scale", d.buffer_scale),
        hidden:             lua_bool(&tbl, "hidden", d.hidden),
        bottom:             lua_bool(&tbl, "bottom", d.bottom),
        hide_vacant:        lua_bool(&tbl, "hide_vacant", d.hide_vacant),
        status_commands:    lua_bool(&tbl, "status_commands", d.status_commands),
        center_title:       lua_bool(&tbl, "center_title", d.center_title),
        active_color_title: lua_bool(&tbl, "active_color_title", d.active_color_title),
        active_fg:          lua_u32(&tbl, "active_fg", d.active_fg),
        active_bg:          lua_u32(&tbl, "active_bg", d.active_bg),
        occupied_fg:        lua_u32(&tbl, "occupied_fg", d.occupied_fg),
        occupied_bg:        lua_u32(&tbl, "occupied_bg", d.occupied_bg),
        inactive_fg:        lua_u32(&tbl, "inactive_fg", d.inactive_fg),
        inactive_bg:        lua_u32(&tbl, "inactive_bg", d.inactive_bg),
        urgent_fg:          lua_u32(&tbl, "urgent_fg", d.urgent_fg),
        urgent_bg:          lua_u32(&tbl, "urgent_bg", d.urgent_bg),
        middle_bg:          lua_u32(&tbl, "middle_bg", d.middle_bg),
        middle_bg_selected: lua_u32(&tbl, "middle_bg_selected", d.middle_bg_selected),
        blocks:             lua_bar_blocks(&tbl, d.blocks),
        blocks_delim:       lua_str(&tbl, "blocks_delim").unwrap_or(d.blocks_delim),
        tag_names:          lua_bar_tag_names(&tbl, d.tag_names),
    }
}

#[cfg(feature = "bar")]
fn lua_bar_blocks(t: &mlua::Table, default: Vec<BlockSettings>) -> Vec<BlockSettings> {
    let Ok(arr) = t.get::<mlua::Table>("blocks") else { return default; };
    let out: Vec<BlockSettings> = arr.sequence_values::<mlua::Table>()
        .filter_map(|v| {
            let tbl = v.ok()?;
            Some(BlockSettings {
                icon:     lua_str(&tbl, "icon").unwrap_or_default(),
                command:  lua_str(&tbl, "command").unwrap_or_default(),
                interval: lua_u32(&tbl, "interval", 0),
                signal:   u8::try_from(lua_u32(&tbl, "signal", 0)).unwrap_or(0),
            })
        })
        .collect();
    if out.is_empty() { default } else { out }
}

#[cfg(feature = "bar")]
fn lua_bar_tag_names(t: &mlua::Table, default: Vec<String>) -> Vec<String> {
    let Ok(arr) = t.get::<mlua::Table>("tag_names") else { return default; };
    let n = arr.raw_len();
    if n == 0 { return default; }
    (1..=n)
        .filter_map(|i| arr.get::<String>(i64::try_from(i).unwrap_or(i64::MAX)).ok())
        .collect()
}
