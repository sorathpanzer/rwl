//! Default key / button bindings, keysym resolution, and the Lua parsing that
//! turns config tables into [`KeyBind`] / [`ButtonBind`] / [`Action`] values.

use smithay::input::keyboard::keysyms as KS;

use super::lua::{lua_f64, lua_i32, lua_str, lua_str_array, lua_u32};
use super::{Action, ButtonBind, KeyBind, MOD_TAP_KEYSYM, MODKEY};
#[cfg(feature = "scratchpad")]
use super::ScratchCmd;

// ─── default bindings ─────────────────────────────────────────────────────────

const fn k(mods: u32, keysym: u32, action: Action) -> KeyBind {
    KeyBind { mods, keysym, action }
}

#[allow(clippy::too_many_lines)]
#[cfg_attr(not(feature = "scratchpad"), allow(unused_mut))]
pub(super) fn default_keys() -> Vec<KeyBind> {
    use smithay::input::keyboard::keysyms as xkb;
    const M: u32 = MODKEY;
    const C: u32 = 1 << 2;
    const S: u32 = 1 << 0;
    const A: u32 = 1 << 3;
    let tm: u32 = (1 << 6) - 1; // tag_mask for default tag_count=6
    let mut v = vec![
        // Terminal / launcher
        k(M|S, xkb::KEY_Return,    Action::Spawn(vec!["wmenu-run".into(),"-f".into(),"12".into(),"-N".into(),"000000".into(),"-S".into(),"74438e".into()])),
        k(M,   xkb::KEY_Return,    Action::Spawn(vec!["foot".into()])),
        // Focus navigation
        k(M, xkb::KEY_Down,  Action::FocusStack(1)),
        k(M, xkb::KEY_Up,    Action::FocusStack(-1)),
        k(M, xkb::KEY_Right, Action::FocusStack(1)),
        k(M, xkb::KEY_Left,  Action::FocusStack(-1)),
        // Master count
        k(M, xkb::KEY_i, Action::IncNmaster(1)),
        k(M, xkb::KEY_d, Action::IncNmaster(-1)),
        // Master size
        k(M|C, xkb::KEY_plus,  Action::SetMfact(-0.05)),
        k(M|C, xkb::KEY_minus, Action::SetMfact(0.05)),
        // Tag navigation
        k(M|S, xkb::KEY_Escape, Action::ViewNextOccTag(-1)),
        k(M,   xkb::KEY_Escape, Action::ViewNextOccTag(1)),
        // Zoom / swap master
        k(M, xkb::KEY_z,        Action::Zoom),
        k(M, xkb::KEY_backslash, Action::ViewPrev),
        // Window management
        k(M, xkb::KEY_q, Action::KillClient),
        // Layouts
        k(M, xkb::KEY_t,   Action::SetLayout(0)),
        k(M, xkb::KEY_o,   Action::SetLayout(1)),
        k(M, xkb::KEY_c,   Action::SetLayout(2)),
        k(M, xkb::KEY_n,   Action::SetLayout(3)),
        k(M, xkb::KEY_a,   Action::SetLayout(usize::MAX)),
        k(M, xkb::KEY_Tab, Action::CycleLayout),
        // Floating / fullscreen
        k(M|S, xkb::KEY_space, Action::ToggleFloating),
        k(M,   xkb::KEY_e,     Action::ToggleFullscreen),
        // All-tags view
        k(M,   xkb::KEY_0,     Action::View(tm)),
        k(M|S, xkb::KEY_equal, Action::Tag(tm)),
        // Monitor focus / send
        k(M,   xkb::KEY_comma,   Action::FocusMon(-1)),
        k(M|C, xkb::KEY_less,    Action::TagMon(-1)),
        k(M|C, xkb::KEY_greater, Action::TagMon(1)),
        // Tags 1-2: ViewTagSpawn
        k(M, xkb::KEY_1, Action::ViewTagSpawn(1 << 0)),
        k(M, xkb::KEY_2, Action::ViewTagSpawn(1 << 1)),
        // Tags 1-6: toggle/move/toggle-on-window
        k(M|C,   xkb::KEY_1,          Action::ToggleView(1 << 0)),
        k(M|S,   xkb::KEY_exclam,     Action::Tag(1 << 0)),
        k(M|C|S, xkb::KEY_exclam,     Action::ToggleTag(1 << 0)),
        k(M|C,   xkb::KEY_2,          Action::ToggleView(1 << 1)),
        k(M|S,   xkb::KEY_quotedbl,   Action::Tag(1 << 1)),
        k(M|C|S, xkb::KEY_quotedbl,   Action::ToggleTag(1 << 1)),
        k(M,     xkb::KEY_3,          Action::View(1 << 2)),
        k(M|C,   xkb::KEY_3,          Action::ToggleView(1 << 2)),
        k(M|S,   xkb::KEY_numbersign, Action::Tag(1 << 2)),
        k(M|C|S, xkb::KEY_numbersign, Action::ToggleTag(1 << 2)),
        k(M,     xkb::KEY_4,          Action::View(1 << 3)),
        k(M|C,   xkb::KEY_4,          Action::ToggleView(1 << 3)),
        k(M|S,   xkb::KEY_dollar,     Action::Tag(1 << 3)),
        k(M|C|S, xkb::KEY_dollar,     Action::ToggleTag(1 << 3)),
        k(M,     xkb::KEY_5,          Action::View(1 << 4)),
        k(M|C,   xkb::KEY_5,          Action::ToggleView(1 << 4)),
        k(M|S,   xkb::KEY_percent,    Action::Tag(1 << 4)),
        k(M|C|S, xkb::KEY_percent,    Action::ToggleTag(1 << 4)),
        k(M,     xkb::KEY_6,          Action::View(1 << 5)),
        k(M|C,   xkb::KEY_6,          Action::ToggleView(1 << 5)),
        k(M|S,   xkb::KEY_ampersand,  Action::Tag(1 << 5)),
        k(M|C|S, xkb::KEY_ampersand,  Action::ToggleTag(1 << 5)),
        // Config reload
        k(M, xkb::KEY_r, Action::ReloadConfig),
        // Passthrough / quit
        k(M,   xkb::KEY_x, Action::TogglePassthrough),
        k(M|S, xkb::KEY_Q, Action::Quit),
        // Ctrl-Alt-Backspace
        k(C|A, xkb::KEY_Terminate_Server, Action::Quit),
        // VT switching
        k(C|A, xkb::KEY_XF86Switch_VT_1,  Action::Chvt(1)),
        k(C|A, xkb::KEY_XF86Switch_VT_2,  Action::Chvt(2)),
        k(C|A, xkb::KEY_XF86Switch_VT_3,  Action::Chvt(3)),
        k(C|A, xkb::KEY_XF86Switch_VT_4,  Action::Chvt(4)),
        k(C|A, xkb::KEY_XF86Switch_VT_5,  Action::Chvt(5)),
        k(C|A, xkb::KEY_XF86Switch_VT_6,  Action::Chvt(6)),
        k(C|A, xkb::KEY_XF86Switch_VT_7,  Action::Chvt(7)),
        k(C|A, xkb::KEY_XF86Switch_VT_8,  Action::Chvt(8)),
        k(C|A, xkb::KEY_XF86Switch_VT_9,  Action::Chvt(9)),
        k(C|A, xkb::KEY_XF86Switch_VT_10, Action::Chvt(10)),
        k(C|A, xkb::KEY_XF86Switch_VT_11, Action::Chvt(11)),
        k(C|A, xkb::KEY_XF86Switch_VT_12, Action::Chvt(12)),
    ];
    #[cfg(feature = "scratchpad")]
    {
        let sc = ScratchCmd {
            key: 's',
            cmd: vec!["foot".into(), "-a".into(), "scratchpad".into(),
                      "--override=colors-dark.alpha=1".into(),
                      "--override=colors-dark.background=000000".into()],
        };
        v.extend([
            k(M, xkb::KEY_less, Action::ToggleScratch(sc.clone())),
            k(M, xkb::KEY_h,    Action::FocusOrToggleScratch(sc.clone())),
            k(M, xkb::KEY_k,    Action::FocusOrToggleMatchingScratch(sc)),
        ]);
    }
    #[cfg(feature = "overview")]
    v.extend([
        k(M,   xkb::KEY_grave, Action::ToggleOverview),
        k(M|S, xkb::KEY_grave, Action::ToggleOverviewAll),
    ]);
    // Tap the mod key alone (press+release, no other key) to toggle the overview.
    #[cfg(all(feature = "overview", feature = "mod-tap"))]
    v.push(k(M, MOD_TAP_KEYSYM, Action::ToggleOverview));
    #[cfg(feature = "pip")]
    v.extend([
        k(M,   xkb::KEY_p, Action::TogglePip),
        k(M|S, xkb::KEY_p, Action::MovePip),
    ]);
    #[cfg(feature = "lock")]
    v.push(k(M|S, xkb::KEY_L, Action::Lock));
    v
}

pub(super) fn default_buttons() -> Vec<ButtonBind> {
    const M: u32 = MODKEY;
    vec![
        ButtonBind { mods: M, button: 0x110, action: Action::Move            },
        ButtonBind { mods: M, button: 0x112, action: Action::ToggleFloating  },
        ButtonBind { mods: M, button: 0x111, action: Action::Resize          },
    ]
}

// ─── modifier / keysym / button parsing ───────────────────────────────────────

/// Convert a modifier string ("M", "MC", "MS", "CA", …) to a bitmask.
fn parse_mods(s: &str) -> u32 {
    s.chars().fold(0u32, |bits, c| bits | match c {
        'M' => MODKEY,
        'C' => 1 << 2,
        'S' => 1 << 0,
        'A' => 1 << 3,
        _   => 0,
    })
}

/// Resolve an XKB keysym by name.  Single printable ASCII characters are
/// mapped directly; named keysyms use a static lookup table.
fn keysym_for_name(name: &str) -> Option<u32> {
    // Empty key — the mod-tap gesture ("tap the mod key alone").
    if name.is_empty() {
        return Some(MOD_TAP_KEYSYM);
    }
    // Single printable ASCII — XKB keysym values match Unicode / ASCII.
    if let [b] = name.as_bytes()
        && matches!(*b, b' '..=b'~')
    {
        return Some(u32::from(*b));
    }
    NAMED_KEYSYMS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, sym)| *sym)
}

fn parse_button_code(s: &str) -> Option<u32> {
    match s {
        "left"   => Some(0x110),
        "right"  => Some(0x111),
        "middle" => Some(0x112),
        other    => other.parse::<u32>().ok(),
    }
}

// ─── action parsing ───────────────────────────────────────────────────────────

#[cfg(feature = "scratchpad")]
fn parse_scratch_cmd(tbl: &mlua::Table) -> Option<ScratchCmd> {
    let key_str = lua_str(tbl, "scratch_key")?;
    let key = key_str.chars().next()?;
    let cmd = lua_str_array(tbl, "cmd");
    Some(ScratchCmd { key, cmd })
}

fn parse_action(tbl: &mlua::Table, action: &str) -> Option<Action> {
    match action {
        "spawn"                          => Some(Action::Spawn(lua_str_array(tbl, "cmd"))),
        "focus_stack"                    => Some(Action::FocusStack(lua_i32(tbl, "dir", 1))),
        "inc_nmaster"                    => Some(Action::IncNmaster(lua_i32(tbl, "delta", 1))),
        "set_mfact"                      => Some(Action::SetMfact(lua_f64(tbl, "delta", 0.05))),
        "zoom"                           => Some(Action::Zoom),
        "view_prev"                      => Some(Action::ViewPrev),
        "view_next_occ_tag"              => Some(Action::ViewNextOccTag(lua_i32(tbl, "dir", 1))),
        "kill_client"                    => Some(Action::KillClient),
        "set_layout"                     => Some(Action::SetLayout(lua_u32(tbl, "index", 0) as usize)),
        "cycle_layout"                   => Some(Action::CycleLayout),
        #[cfg(feature = "pertag-layouts")]
        "reset_layout"                   => Some(Action::ResetLayout),
        "toggle_floating"                => Some(Action::ToggleFloating),
        "toggle_fullscreen"              => Some(Action::ToggleFullscreen),
        "toggle_passthrough"             => Some(Action::TogglePassthrough),
        #[cfg(feature = "gaps")]
        "toggle_gaps"                    => Some(Action::ToggleGaps),
        "view"                           => Some(Action::View(lua_u32(tbl, "mask", 0))),
        "view_tag_spawn"                 => Some(Action::ViewTagSpawn(lua_u32(tbl, "mask", 0))),
        "toggle_view"                    => Some(Action::ToggleView(lua_u32(tbl, "mask", 0))),
        "tag"                            => Some(Action::Tag(lua_u32(tbl, "mask", 0))),
        "toggle_tag"                     => Some(Action::ToggleTag(lua_u32(tbl, "mask", 0))),
        "focus_mon"                      => Some(Action::FocusMon(lua_i32(tbl, "dir", -1))),
        "tag_mon"                        => Some(Action::TagMon(lua_i32(tbl, "dir", -1))),
        "move"                           => Some(Action::Move),
        "resize"                         => Some(Action::Resize),
        #[cfg(feature = "scratchpad")]
        "toggle_scratch"                 => parse_scratch_cmd(tbl).map(Action::ToggleScratch),
        #[cfg(feature = "scratchpad")]
        "focus_or_toggle_scratch"        => parse_scratch_cmd(tbl).map(Action::FocusOrToggleScratch),
        #[cfg(feature = "scratchpad")]
        "focus_or_toggle_matching_scratch" => parse_scratch_cmd(tbl).map(Action::FocusOrToggleMatchingScratch),
        "chvt"                           => Some(Action::Chvt(lua_u32(tbl, "vt", 1))),
        "reload_config"                  => Some(Action::ReloadConfig),
        "quit"                           => Some(Action::Quit),
        #[cfg(feature = "bar")]
        "toggle_bar"                     => Some(Action::ToggleBar),
        #[cfg(feature = "bar")]
        "bar_prompt"                     => Some(Action::BarPrompt),
        #[cfg(feature = "overview")]
        "toggle_overview"                => Some(Action::ToggleOverview),
        #[cfg(feature = "overview")]
        "toggle_overview_all"            => Some(Action::ToggleOverviewAll),
        #[cfg(feature = "pip")]
        "toggle_pip"                     => Some(Action::TogglePip),
        #[cfg(feature = "pip")]
        "move_pip"                       => Some(Action::MovePip),
        #[cfg(feature = "lock")]
        "lock"                           => Some(Action::Lock),
        other => {
            tracing::warn!("Unknown action in config: {other}");
            None
        }
    }
}

// ─── binding tables ───────────────────────────────────────────────────────────

pub(super) fn lua_keys(t: &mlua::Table, default: Vec<KeyBind>) -> Vec<KeyBind> {
    let Ok(arr) = t.get::<mlua::Table>("keys") else { return default };
    let out: Vec<KeyBind> = arr.sequence_values::<mlua::Table>()
        .filter_map(|v| {
            let tbl = v.ok()?;
            let mods = parse_mods(&lua_str(&tbl, "mods").unwrap_or_default());
            let key_str = lua_str(&tbl, "key")?;
            let keysym = keysym_for_name(&key_str).or_else(|| {
                tracing::warn!("Unknown keysym '{key_str}' in keys config");
                None
            })?;
            let action_str = lua_str(&tbl, "action")?;
            let action = parse_action(&tbl, &action_str)?;
            Some(KeyBind { mods, keysym, action })
        })
        .collect();
    if out.is_empty() { default } else { out }
}

pub(super) fn lua_buttons(t: &mlua::Table, default: Vec<ButtonBind>) -> Vec<ButtonBind> {
    let Ok(arr) = t.get::<mlua::Table>("buttons") else { return default };
    let out: Vec<ButtonBind> = arr.sequence_values::<mlua::Table>()
        .filter_map(|v| {
            let tbl = v.ok()?;
            let mods = parse_mods(&lua_str(&tbl, "mods").unwrap_or_default());
            let btn_str = lua_str(&tbl, "button")?;
            let button = parse_button_code(&btn_str).or_else(|| {
                tracing::warn!("Unknown button '{btn_str}' in buttons config");
                None
            })?;
            let action_str = lua_str(&tbl, "action")?;
            let action = parse_action(&tbl, &action_str)?;
            Some(ButtonBind { mods, button, action })
        })
        .collect();
    if out.is_empty() { default } else { out }
}

// ─── Named keysym table ───────────────────────────────────────────────────────

// Keysym values use smithay's constants (all pub const u32).
// Single printable ASCII chars are handled by the ASCII fast-path in
// keysym_for_name(); only named / non-ASCII keysyms belong here.
static NAMED_KEYSYMS: &[(&str, u32)] = &[
    // Navigation
    ("Return",    KS::KEY_Return),
    ("BackSpace", KS::KEY_BackSpace),
    ("Delete",    KS::KEY_Delete),
    ("Escape",    KS::KEY_Escape),
    ("Tab",       KS::KEY_Tab),
    ("Up",        KS::KEY_Up),
    ("Down",      KS::KEY_Down),
    ("Left",      KS::KEY_Left),
    ("Right",     KS::KEY_Right),
    ("Home",      KS::KEY_Home),
    ("End",       KS::KEY_End),
    ("Page_Up",   KS::KEY_Page_Up),
    ("Page_Down", KS::KEY_Page_Down),
    ("Insert",    KS::KEY_Insert),
    ("Print",     KS::KEY_Print),
    ("Pause",     KS::KEY_Pause),
    // Function keys
    ("F1",  KS::KEY_F1),  ("F2",  KS::KEY_F2),  ("F3",  KS::KEY_F3),
    ("F4",  KS::KEY_F4),  ("F5",  KS::KEY_F5),  ("F6",  KS::KEY_F6),
    ("F7",  KS::KEY_F7),  ("F8",  KS::KEY_F8),  ("F9",  KS::KEY_F9),
    ("F10", KS::KEY_F10), ("F11", KS::KEY_F11), ("F12", KS::KEY_F12),
    // Symbols not directly printable or with XKB names
    ("space",        KS::KEY_space),
    ("exclam",       KS::KEY_exclam),
    ("quotedbl",     KS::KEY_quotedbl),
    ("numbersign",   KS::KEY_numbersign),
    ("dollar",       KS::KEY_dollar),
    ("percent",      KS::KEY_percent),
    ("ampersand",    KS::KEY_ampersand),
    ("apostrophe",   KS::KEY_apostrophe),
    ("parenleft",    KS::KEY_parenleft),
    ("parenright",   KS::KEY_parenright),
    ("asterisk",     KS::KEY_asterisk),
    ("underscore",   KS::KEY_underscore),
    ("colon",        KS::KEY_colon),
    ("semicolon",    KS::KEY_semicolon),
    ("at",           KS::KEY_at),
    ("bracketleft",  KS::KEY_bracketleft),
    ("bracketright", KS::KEY_bracketright),
    ("asciicircum",  KS::KEY_asciicircum),
    ("grave",        KS::KEY_grave),
    ("braceleft",    KS::KEY_braceleft),
    ("bar",          KS::KEY_bar),
    ("braceright",   KS::KEY_braceright),
    ("asciitilde",   KS::KEY_asciitilde),
    // Special / multimedia
    ("Terminate_Server",  KS::KEY_Terminate_Server),
    ("XF86Switch_VT_1",  KS::KEY_XF86Switch_VT_1),
    ("XF86Switch_VT_2",  KS::KEY_XF86Switch_VT_2),
    ("XF86Switch_VT_3",  KS::KEY_XF86Switch_VT_3),
    ("XF86Switch_VT_4",  KS::KEY_XF86Switch_VT_4),
    ("XF86Switch_VT_5",  KS::KEY_XF86Switch_VT_5),
    ("XF86Switch_VT_6",  KS::KEY_XF86Switch_VT_6),
    ("XF86Switch_VT_7",  KS::KEY_XF86Switch_VT_7),
    ("XF86Switch_VT_8",  KS::KEY_XF86Switch_VT_8),
    ("XF86Switch_VT_9",  KS::KEY_XF86Switch_VT_9),
    ("XF86Switch_VT_10", KS::KEY_XF86Switch_VT_10),
    ("XF86Switch_VT_11", KS::KEY_XF86Switch_VT_11),
    ("XF86Switch_VT_12", KS::KEY_XF86Switch_VT_12),
    ("XF86AudioRaiseVolume", KS::KEY_XF86AudioRaiseVolume),
    ("XF86AudioLowerVolume", KS::KEY_XF86AudioLowerVolume),
    ("XF86AudioMute",        KS::KEY_XF86AudioMute),
    ("XF86AudioPlay",        KS::KEY_XF86AudioPlay),
    ("XF86AudioStop",        KS::KEY_XF86AudioStop),
    ("XF86AudioNext",        KS::KEY_XF86AudioNext),
    ("XF86AudioPrev",        KS::KEY_XF86AudioPrev),
    ("XF86MonBrightnessUp",  KS::KEY_XF86MonBrightnessUp),
    ("XF86MonBrightnessDown",KS::KEY_XF86MonBrightnessDown),
];
