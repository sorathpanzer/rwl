//! Core configuration data types and their compiled-in default data.

use smithay::reexports::wayland_server::protocol::wl_output::Transform;

// ─── colour ──────────────────────────────────────────────────────────────────

/// RGBA colour as four `f32` values in `[0.0, 1.0]`.
pub type Color = [f32; 4];

/// Build a [`Color`] from a packed `0xRRGGBBAA` hex integer.
#[must_use]
#[allow(clippy::cast_possible_truncation)]
pub fn hex_color(hex: u32) -> Color {
    [
        f32::from((hex >> 24) as u8) / 255.0,
        f32::from((hex >> 16) as u8) / 255.0,
        f32::from((hex >>  8) as u8) / 255.0,
        f32::from( hex        as u8) / 255.0,
    ]
}

// ─── types ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Rule {
    pub id:           Option<String>,
    pub title:        Option<String>,
    pub tags:         u32,
    pub switch_to_tag: bool,
    pub is_floating:  bool,
    pub monitor:      i32,
    pub scratch_key:  char,
    /// Exempt matching windows from terminal swallowing (`swallow` feature).
    #[cfg_attr(not(feature = "swallow"), allow(dead_code))]
    pub no_swallow:   bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutKind {
    #[cfg(feature = "tile")]
    Tile,
    #[cfg(feature = "monocle")]
    Monocle,
    #[cfg(feature = "col")]
    Col,
    #[cfg(feature = "scroll")]
    Scroll,
    #[cfg(feature = "dwindle")]
    Dwindle,
    #[cfg(feature = "bstack")]
    Bstack,
    #[cfg(feature = "centeredmaster")]
    CenteredMaster,
    /// A user-defined layout implemented in Lua via the global `on_arrange`
    /// callback (see the `hooks` feature). Selected with `kind = "lua"`; its
    /// symbol is whatever the `layouts` entry sets.
    #[cfg(feature = "hooks")]
    Lua,
    /// Built-in fallback used only when no layout feature is compiled in, so
    /// the compositor still tiles (every window gets the full work area).
    #[cfg(not(any_layout))]
    Fallback,
}

/// The fallback layout kind, used when a stored layout index is no longer valid
/// or a tag has no explicit layout.  Resolves at compile time to the first
/// enabled layout feature (tile → monocle → col → scroll → dwindle), or the
/// built-in [`LayoutKind::Fallback`] when none is enabled.
#[must_use]
pub const fn default_layout_kind() -> LayoutKind {
    #[cfg(feature = "tile")]
    { LayoutKind::Tile }
    #[cfg(all(not(feature = "tile"), feature = "monocle"))]
    { LayoutKind::Monocle }
    #[cfg(all(not(feature = "tile"), not(feature = "monocle"), feature = "col"))]
    { LayoutKind::Col }
    #[cfg(all(
        not(feature = "tile"),
        not(feature = "monocle"),
        not(feature = "col"),
        feature = "scroll",
    ))]
    { LayoutKind::Scroll }
    #[cfg(all(
        not(feature = "tile"),
        not(feature = "monocle"),
        not(feature = "col"),
        not(feature = "scroll"),
        feature = "dwindle",
    ))]
    { LayoutKind::Dwindle }
    #[cfg(all(
        not(feature = "tile"),
        not(feature = "monocle"),
        not(feature = "col"),
        not(feature = "scroll"),
        not(feature = "dwindle"),
        feature = "bstack",
    ))]
    { LayoutKind::Bstack }
    #[cfg(all(
        not(feature = "tile"),
        not(feature = "monocle"),
        not(feature = "col"),
        not(feature = "scroll"),
        not(feature = "dwindle"),
        not(feature = "bstack"),
        feature = "centeredmaster",
    ))]
    { LayoutKind::CenteredMaster }
    #[cfg(not(any_layout))]
    { LayoutKind::Fallback }
}

#[derive(Debug, Clone)]
pub struct LayoutDef {
    pub symbol: String,
    pub kind:   LayoutKind,
}

/// Variable-refresh-rate (adaptive sync, aka `FreeSync` / `G-Sync`) policy for an output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VrrMode {
    /// Never enable VRR (fixed refresh).
    #[default]
    Off,
    /// Always enable VRR when the output supports it.
    On,
    /// Enable VRR only while a fullscreen window occupies the output — the usual
    /// choice for gaming, avoiding VRR flicker on the static desktop.
    OnDemand,
}

#[derive(Debug, Clone)]
pub struct MonitorRule {
    pub name:       Option<String>,
    pub mfact:      f64,
    pub nmaster:    i32,
    pub scale:      f64,
    pub layout_idx: usize,
    pub transform:  Transform,
    pub x:          i32,
    pub y:          i32,
    pub vrr:        VrrMode,
}

impl Default for MonitorRule {
    fn default() -> Self {
        Self {
            name: None, mfact: 0.55, nmaster: 1, scale: 1.0,
            layout_idx: 0, transform: Transform::Normal, x: -1, y: -1,
            vrr: VrrMode::Off,
        }
    }
}

#[derive(Debug, Clone)]
pub struct XkbRules {
    pub rules:   Option<String>,
    pub model:   Option<String>,
    pub layout:  Option<String>,
    pub variant: Option<String>,
    pub options: Option<String>,
}

#[derive(Debug, Clone)]
pub enum Action {
    Spawn(Vec<String>),
    FocusStack(i32),
    IncNmaster(i32),
    SetMfact(f64),
    Zoom,
    ViewPrev,
    ViewNextOccTag(i32),
    KillClient,
    SetLayout(usize),
    CycleLayout,
    #[cfg(feature = "pertag-layouts")]
    ResetLayout,
    ToggleFloating,
    ToggleFullscreen,
    TogglePassthrough,
    #[cfg(feature = "gaps")]
    ToggleGaps,
    View(u32),
    ViewTagSpawn(u32),
    ToggleView(u32),
    Tag(u32),
    ToggleTag(u32),
    FocusMon(i32),
    TagMon(i32),
    Move,
    Resize,
    #[cfg(feature = "scratchpad")]
    ToggleScratch(ScratchCmd),
    #[cfg(feature = "scratchpad")]
    FocusOrToggleScratch(ScratchCmd),
    #[cfg(feature = "scratchpad")]
    FocusOrToggleMatchingScratch(ScratchCmd),
    Chvt(u32),
    ReloadConfig,
    Quit,
    /// Call a user-defined global Lua function by name (the `call` keybind
    /// action) — turns any hook helper (`rwl.spawn`, `win:set_tags`, …) into a
    /// key-triggerable "window choreography" macro.
    #[cfg(feature = "hooks")]
    CallLua(String),
    #[cfg(feature = "bar")]
    ToggleBar,
    #[cfg(feature = "bar")]
    BarPrompt,
    /// Open the workspace overview (Exposé) for the current tag, or close it if
    /// already open.
    #[cfg(feature = "overview")]
    ToggleOverview,
    /// Open the workspace overview across all tags (mission control), or close
    /// it if already open.
    #[cfg(feature = "overview")]
    ToggleOverviewAll,
    /// Internal: routes a swallowed keysym to the overview key handler while the
    /// overview is active. Not user-bindable.
    #[cfg(feature = "overview")]
    OverviewNav(u32),
    /// Toggle Picture-in-Picture on the focused window.
    #[cfg(feature = "pip")]
    TogglePip,
    /// Move the PiP thumbnail to the next corner.
    #[cfg(feature = "pip")]
    MovePip,
    /// Engage the native screen locker.
    #[cfg(feature = "lock")]
    Lock,
    /// Internal: swallow a keystroke while the native lock is active (routed to
    /// the locker's own buffer, never to a client). Not user-bindable.
    #[cfg(feature = "lock")]
    LockConsume,
}

#[cfg(feature = "ipc")]
impl Action {
    /// The canonical `rwl msg` command word that triggers this action, or `None`
    /// when the action is intentionally not exposed over the command socket:
    /// pointer-only grabs (`Move`/`Resize`), internal routing actions
    /// (`OverviewNav`/`LockConsume`), the Lua-macro `call` action, and the
    /// scratchpad / overview / PiP toggles that currently have no socket command.
    ///
    /// The match is exhaustive on purpose: adding an `Action` variant forces a
    /// decision here, and the parity test in the IPC server
    /// ([`server.rs`](../features/integration/ipc/server.rs)) checks that every
    /// name returned is actually handled. Keep it aligned with `docs/IPC.md` and
    /// the action table in `docs/CONFIGURATION.md`.
    #[must_use]
    pub fn ipc_command(&self) -> Option<&'static str> {
        match self {
            Self::Spawn(_) => Some("spawn"),
            Self::FocusStack(_) => Some("focusstack"),
            Self::IncNmaster(_) => Some("incnmaster"),
            Self::SetMfact(_) => Some("setmfact"),
            Self::Zoom => Some("zoom"),
            Self::ViewPrev => Some("viewprev"),
            Self::ViewNextOccTag(_) => Some("viewnextocctag"),
            Self::KillClient => Some("killclient"),
            Self::SetLayout(_) => Some("setlayout"),
            Self::CycleLayout => Some("cyclelayout"),
            Self::ToggleFloating => Some("togglefloating"),
            Self::ToggleFullscreen => Some("togglefullscreen"),
            Self::TogglePassthrough => Some("togglepassthrough"),
            Self::View(_) => Some("view"),
            Self::ViewTagSpawn(_) => Some("viewtag_spawn"),
            Self::ToggleView(_) => Some("toggleview"),
            Self::Tag(_) => Some("tag"),
            Self::ToggleTag(_) => Some("toggletag"),
            Self::FocusMon(_) => Some("focusmon"),
            Self::TagMon(_) => Some("tagmon"),
            Self::Chvt(_) => Some("chvt"),
            Self::ReloadConfig => Some("reloadconfig"),
            Self::Quit => Some("quit"),
            // Pointer-only grabs — no socket command.
            Self::Move | Self::Resize => None,
            #[cfg(feature = "pertag-layouts")]
            Self::ResetLayout => None,
            #[cfg(feature = "gaps")]
            Self::ToggleGaps => Some("togglegaps"),
            #[cfg(feature = "scratchpad")]
            Self::ToggleScratch(_)
            | Self::FocusOrToggleScratch(_)
            | Self::FocusOrToggleMatchingScratch(_) => None,
            #[cfg(feature = "hooks")]
            Self::CallLua(_) => None,
            #[cfg(feature = "bar")]
            Self::ToggleBar => Some("togglebar"),
            #[cfg(feature = "bar")]
            Self::BarPrompt => Some("barprompt"),
            #[cfg(feature = "overview")]
            Self::ToggleOverview | Self::ToggleOverviewAll | Self::OverviewNav(_) => None,
            #[cfg(feature = "pip")]
            Self::TogglePip | Self::MovePip => None,
            #[cfg(feature = "lock")]
            Self::Lock => Some("lock"),
            #[cfg(feature = "lock")]
            Self::LockConsume => None,
        }
    }
}

#[cfg(feature = "scratchpad")]
#[derive(Debug, Clone)]
pub struct ScratchCmd {
    pub key: char,
    pub cmd: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct KeyBind {
    pub mods:   u32,
    pub keysym: u32,
    pub action: Action,
}

#[derive(Debug, Clone)]
pub struct ButtonBind {
    pub mods:   u32,
    pub button: u32,
    pub action: Action,
}

/// Per-tag layout assignment.
///
/// A tag either uses one [`Fixed`](PertagRule::Fixed) layout, or picks a layout
/// from a list indexed by the number of tiled windows on that tag
/// ([`ByCount`](PertagRule::ByCount)).
#[cfg(feature = "pertag-layouts")]
#[derive(Debug, Clone)]
pub enum PertagRule {
    /// One layout index, used regardless of window count.
    Fixed(usize),
    /// Layout indices chosen by tiled-window count.  Element `i` (0-based) is
    /// used when there are `i + 1` windows; the last element applies to its own
    /// count and every count above it.  Zero windows use the first element.
    /// Guaranteed non-empty by the parser.
    ByCount(Vec<usize>),
}

/// Stable name for a layout kind (used by the pertag-layouts feature).
#[cfg(feature = "pertag-layouts")]
pub const fn layout_kind_name(kind: LayoutKind) -> &'static str {
    match kind {
        #[cfg(feature = "tile")]
        LayoutKind::Tile    => "tile",
        #[cfg(feature = "monocle")]
        LayoutKind::Monocle => "monocle",
        #[cfg(feature = "col")]
        LayoutKind::Col     => "col",
        #[cfg(feature = "scroll")]
        LayoutKind::Scroll  => "scroll",
        #[cfg(feature = "dwindle")]
        LayoutKind::Dwindle => "dwindle",
        #[cfg(feature = "bstack")]
        LayoutKind::Bstack  => "bstack",
        #[cfg(feature = "centeredmaster")]
        LayoutKind::CenteredMaster => "centeredmaster",
        #[cfg(feature = "hooks")]
        LayoutKind::Lua => "lua",
        #[cfg(not(any_layout))]
        LayoutKind::Fallback => "default",
    }
}

// ─── default data ─────────────────────────────────────────────────────────────

#[cfg(feature = "scratchpad")]
fn r(id: Option<&str>, title: Option<&str>, tags: u32, stag: bool, float: bool, sc: char) -> Rule {
    Rule {
        id:           id.map(String::from),
        title:        title.map(String::from),
        tags,
        switch_to_tag: stag,
        is_floating:  float,
        monitor:      -1,
        scratch_key:  sc,
        no_swallow:   false,
    }
}

#[cfg_attr(not(feature = "scratchpad"), allow(unused_mut))]
#[allow(clippy::vec_init_then_push)] // push is #[cfg]-gated on scratchpad
pub(super) fn default_rules() -> Vec<Rule> {
    let mut v = Vec::new();
    #[cfg(feature = "scratchpad")]
    v.push(r(Some("scratchpad"), None, 0, false, true, 's'));
    v
}

#[allow(clippy::vec_init_then_push)] // pushes are individually #[cfg]-gated
pub(super) fn default_layouts() -> Vec<LayoutDef> {
    let mut v = Vec::new();
    #[cfg(not(any_layout))]
    v.push(LayoutDef { symbol: "[]=".into(), kind: LayoutKind::Fallback });
    #[cfg(feature = "tile")]
    v.push(LayoutDef { symbol: "[]=".into(), kind: LayoutKind::Tile });
    #[cfg(feature = "monocle")]
    v.push(LayoutDef { symbol: "[M]".into(), kind: LayoutKind::Monocle });
    #[cfg(feature = "col")]
    v.push(LayoutDef { symbol: "||".into(), kind: LayoutKind::Col });
    #[cfg(feature = "scroll")]
    v.push(LayoutDef { symbol: ">>>".into(), kind: LayoutKind::Scroll });
    #[cfg(feature = "dwindle")]
    v.push(LayoutDef { symbol: "[@]".into(), kind: LayoutKind::Dwindle });
    #[cfg(feature = "bstack")]
    v.push(LayoutDef { symbol: "(B)".into(), kind: LayoutKind::Bstack });
    #[cfg(feature = "centeredmaster")]
    v.push(LayoutDef { symbol: "|-|".into(), kind: LayoutKind::CenteredMaster });
    v
}

pub(super) fn default_monitor_rules() -> Vec<MonitorRule> {
    vec![MonitorRule::default()]
}

pub(super) fn default_auto_spawn() -> Vec<Option<Vec<String>>> {
    vec![None, None, None, None, None, None]
}
