//! Lua event bus — turns the static `config.lua` into a programmable one.
//!
//! The config VM is kept alive after load (instead of being dropped once the
//! `Config` struct is parsed) so that user-defined Lua callbacks survive and can
//! be invoked on compositor events:
//!
//! ```lua
//! function on_window_open(win)
//!     if win:app_id() == "mpv" then win:set_floating(true) end
//!     -- dynamic layout: switch tag 2 to columns once it holds >= 3 windows
//!     if win:tags() == rwl.tag(2) and rwl.count(rwl.tag(2)) >= 3 then
//!         rwl.set_layout("col")
//!     end
//! end
//! function on_window_close(win)     -- fired after the window leaves the layout
//!     if rwl.count(rwl.tag(2)) < 3 then rwl.set_layout("tile") end
//! end
//! function on_tag_switch(old, new)  -- e.g. a wallpaper per tag
//!     -- also fired once at startup with old == 0, so the launch tag's
//!     -- wallpaper is applied instead of a black screen.
//!     local papers = { [rwl.tag(1)] = "~/pics/a.png", [rwl.tag(2)] = "~/pics/b.png" }
//!     if papers[new] then rwl.set_wallpaper(papers[new], "fill") end
//! end
//! function on_startup()             -- fired once, after the first output
//!     -- warm every per-tag wallpaper off-thread so the first switch has no lag
//!     for _, p in pairs(papers) do rwl.preload(p) end
//! end
//! function on_focus(win)            -- ...
//! function on_title_change(win, t)  -- ...
//! function on_fullscreen(win, fs)   -- fs is a bool; user/client-initiated
//! function on_layout_change(old, new) -- old/new are layout symbol names
//! function on_monitor_add(name)     -- output plugged in (also the first one)
//! function on_monitor_remove(name)  -- output unplugged
//! function on_arrange(n, area)      -- user tiling for the "lua" layout kind;
//!                                   -- returns n {x,y,w,h} rects (see lua_arrange)
//! ```
//!
//! **Read helpers** query live compositor state at call time (they do not defer
//! through the command queue); they answer from a snapshot taken right before
//! the callback fires. On the `rwl` table: `rwl.count(mask)` (windows carrying
//! any tag in `mask`), `rwl.clients()` (array of window handles on the current
//! tagset), `rwl.focused()` (the focused window or nil), `rwl.sel_tags()`,
//! `rwl.layout()` (layout symbol name), `rwl.monitors()` (monitor count). On a
//! window handle: `win:app_id()`, `win:title()`, `win:tags()`,
//! `win:is_floating()`, `win:is_fullscreen()`, `win:monitor()`.
//!
//! **Action helpers** mutate the compositor and therefore defer through the
//! queue. On the `rwl` table: `rwl.spawn`, `rwl.view`, `rwl.toggle_view`,
//! `rwl.set_layout` (name or index), `rwl.zoom`, `rwl.inc_nmaster`,
//! `rwl.set_mfact`, `rwl.focus_monitor`, `rwl.set_wallpaper`, `rwl.notify`. On a
//! window handle: `win:set_tags`, `win:toggle_tag`, `win:set_floating`,
//! `win:set_fullscreen`, `win:focus`, `win:close`, `win:move_to_monitor`,
//! `win:warp`, `win:set_scratch`.
//!
//! Callbacks act on the compositor through a **command queue**: `rwl.*` helpers
//! and `win:*` action methods do not mutate compositor state directly (that
//! would re-enter the `&mut Rwl` borrow that is running the event). Instead they
//! enqueue a [`HookCmd`]; [`drain`] applies the queue at a safe point (right
//! after firing, and once per event-loop iteration from `main.rs`).
//!
//! Compiled only when the `hooks` Cargo feature is enabled. Uses the existing
//! `mlua` dependency, so no extra crates.

use std::cell::{Cell, RefCell};

use mlua::{Lua, UserData, UserDataMethods, Variadic};

use smithay::desktop::Window;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::wayland::seat::WaylandFocus;

use crate::config::{Action, LayoutKind};
use crate::state::Rwl;
use crate::window::{
    set_fullscreen, set_window_tags, window_is_floating, window_is_fullscreen, window_tags,
    with_state, with_state_mut,
};

/// Snapshot of selected-monitor state taken by [`refresh`] right before each
/// hook fires, so the `rwl.*` read helpers can answer synchronously without a
/// live `&Rwl` inside the Lua closure.
#[derive(Default)]
struct Snapshot {
    /// Tag bitmask of every window on the selected monitor (backs `rwl.count`).
    tags: Vec<u32>,
    /// Windows visible on the selected monitor's current tagset (`rwl.clients`).
    clients: Vec<Window>,
    /// The focused window at fire time (`rwl.focused`).
    focused: Option<Window>,
    /// Selected monitor's active tag bitmask (`rwl.sel_tags`).
    sel_tags: u32,
    /// Selected monitor's layout symbol name (`rwl.layout`).
    layout: Option<String>,
    /// Number of connected monitors (`rwl.monitors`).
    monitors: usize,
}

thread_local! {
    /// The persistent config Lua VM (holds the user's hook functions).
    static LUA: RefCell<Option<Lua>> = const { RefCell::new(None) };
    /// Commands enqueued by callbacks, applied by [`drain`].
    static QUEUE: RefCell<Vec<HookCmd>> = const { RefCell::new(Vec::new()) };
    /// Re-entrancy guard so a command that itself fires a hook doesn't recurse
    /// into `drain` while we are already draining.
    static DRAINING: Cell<bool> = const { Cell::new(false) };
    /// Surface of the last window `on_focus` fired for, to de-duplicate the many
    /// `focus_window` calls that don't actually change the focused client.
    static LAST_FOCUS: RefCell<Option<WlSurface>> = const { RefCell::new(None) };
    /// Selected-monitor snapshot, refreshed by [`refresh`] right before each hook
    /// fires so the `rwl.*` read helpers can answer synchronously without a live
    /// `&Rwl` inside the Lua closure.
    static SNAPSHOT: RefCell<Snapshot> = const {
        RefCell::new(Snapshot {
            tags: Vec::new(),
            clients: Vec::new(),
            focused: None,
            sel_tags: 0,
            layout: None,
            monitors: 0,
        })
    };
    /// Set once [`startup`] has fired, so the startup hooks run a single time per
    /// process (not again on hotplug or config reload).
    static STARTUP_DONE: Cell<bool> = const { Cell::new(false) };
}

// ---------------------------------------------------------------------------
// Command queue
// ---------------------------------------------------------------------------

/// A deferred action requested by a Lua callback.
enum HookCmd {
    Spawn(Vec<String>),
    View(u32),
    Notify(String),
    SetTags(Window, u32),
    SetFloating(Window, bool),
    Focus(Window),
    Close(Window),
    SetLayout(usize),
    /// Route an existing keybinding [`Action`] through `dispatch` — used for the
    /// selected-monitor / focused-window helpers (`zoom`, `inc_nmaster`,
    /// `set_mfact`, `focus_monitor`, `toggle_view`). Only safe, non-destructive
    /// actions are ever constructed here.
    Dispatch(Action),
    /// Set the given window's fullscreen state.
    SetFullscreen(Window, bool),
    /// Toggle the given tag bits on the window (never leaving it tagless).
    ToggleTags(Window, u32),
    /// Move the given window to the monitor `dir` steps away.
    MoveToMonitor(Window, i32),
    /// Focus the given window and warp the pointer onto it.
    #[cfg(feature = "warp")]
    Warp(Window),
    /// Mark (or, with `'\0'`, unmark) the window as a scratchpad under `key`.
    #[cfg(feature = "scratchpad")]
    SetScratch(Window, char),
    /// Set (or, with a `None` path, clear) the desktop wallpaper. `None` mode
    /// keeps the current fit mode.
    #[cfg(feature = "wallpaper")]
    SetWallpaper(Option<String>, Option<crate::features::wallpaper::WallpaperMode>),
}

fn enqueue(cmd: HookCmd) {
    QUEUE.with(|q| q.borrow_mut().push(cmd));
}

/// Apply all queued commands to the compositor. Safe to call from anywhere with
/// `&mut Rwl`; no-ops re-entrantly. Bounded so a callback that keeps enqueuing
/// (e.g. `on_tag_switch` calling `rwl.view`) can't hang the loop.
pub fn drain(state: &mut Rwl) {
    if DRAINING.with(Cell::get) {
        return;
    }
    DRAINING.with(|d| d.set(true));

    for _ in 0..8 {
        let cmds = QUEUE.with(|q| std::mem::take(&mut *q.borrow_mut()));
        if cmds.is_empty() {
            break;
        }
        for cmd in cmds {
            apply(state, cmd);
        }
    }

    DRAINING.with(|d| d.set(false));
}

fn apply(state: &mut Rwl, cmd: HookCmd) {
    match cmd {
        HookCmd::Spawn(argv) => state.spawn(&argv),
        HookCmd::View(mask) => {
            let mask = mask & crate::config::tag_mask();
            if mask != 0 {
                state.dispatch(&Action::View(mask));
            }
        }
        HookCmd::Notify(msg) => tracing::info!("[hook] {msg}"),
        HookCmd::SetTags(window, mask) => {
            let mask = mask & crate::config::tag_mask();
            if mask != 0 && state.windows.contains(&window) {
                crate::window::set_window_tags(&window, mask);
                state.arrange_all();
                let top = state.focused_window().cloned();
                state.focus_window(top);
            }
        }
        HookCmd::SetFloating(window, floating) => {
            if state.windows.contains(&window) {
                crate::window::with_state_mut(&window, |s| {
                    s.is_floating = floating;
                    if floating {
                        s.needs_centering = true;
                    }
                });
                state.arrange_all();
            }
        }
        HookCmd::Focus(window) => {
            if state.windows.contains(&window) {
                state.focus_window(Some(window));
            }
        }
        HookCmd::Close(window) => {
            if let Some(tl) = window.toplevel() {
                tl.send_close();
            }
        }
        HookCmd::SetLayout(idx) => state.dispatch(&Action::SetLayout(idx)),
        HookCmd::Dispatch(action) => state.dispatch(&action),
        HookCmd::SetFullscreen(window, fs) => {
            if state.windows.contains(&window) {
                set_fullscreen(&window, fs);
                // arrange_all() sends the configure with the right size and the
                // correct (Un)Fullscreen state in one shot.
                state.arrange_all();
            }
        }
        HookCmd::ToggleTags(window, mask) => {
            let mask = mask & crate::config::tag_mask();
            if mask != 0 && state.windows.contains(&window) {
                let next = (window_tags(&window) ^ mask) & crate::config::tag_mask();
                // Mirror dwm's toggletag guard: never strand a window on no tags.
                if next != 0 {
                    set_window_tags(&window, next);
                    state.arrange_all();
                    let top = state.focused_window().cloned();
                    state.focus_window(top);
                }
            }
        }
        HookCmd::MoveToMonitor(window, dir) => {
            if state.windows.contains(&window) {
                let from = with_state(&window, |s| s.mon_idx).unwrap_or(state.sel_mon);
                if let Some(next) = state.dir_to_monitor(from, dir) {
                    let new_tags = state.monitors.get(next).map_or(1, crate::monitor::Monitor::tags);
                    set_window_tags(&window, new_tags);
                    with_state_mut(&window, |s| s.mon_idx = next);
                    state.arrange_all();
                    let top = state.focused_window().cloned();
                    state.focus_window(top);
                }
            }
        }
        #[cfg(feature = "warp")]
        HookCmd::Warp(window) => {
            if state.windows.contains(&window) {
                state.focus_window(Some(window));
                crate::features::warp::warp_cursor_to_focused(state);
            }
        }
        #[cfg(feature = "scratchpad")]
        HookCmd::SetScratch(window, key) => {
            if state.windows.contains(&window) {
                with_state_mut(&window, |s| s.scratch_key = key);
            }
        }
        #[cfg(feature = "wallpaper")]
        HookCmd::SetWallpaper(path, mode) => {
            crate::features::wallpaper::set_override(path, mode);
            state.schedule_render();
        }
    }
}

// ---------------------------------------------------------------------------
// Live-state snapshot (for the synchronous `rwl.count` read helper)
// ---------------------------------------------------------------------------

/// Refresh [`SNAPSHOT`] with the selected monitor's live state. Called by the
/// fire helpers before invoking a callback so the `rwl.*` read helpers reflect
/// the compositor state at the moment the hook runs.
pub fn refresh(state: &Rwl) {
    let sel = state.sel_mon;
    let sel_tags = state.monitors.get(sel).map_or(0, crate::monitor::Monitor::tags);
    SNAPSHOT.with(|snap| {
        let mut snap = snap.borrow_mut();
        snap.tags.clear();
        snap.clients.clear();
        for w in &state.windows {
            if with_state(w, |s| s.mon_idx).unwrap_or(0) == sel {
                let tags = window_tags(w);
                snap.tags.push(tags);
                if tags & sel_tags != 0 {
                    snap.clients.push(w.clone());
                }
            }
        }
        snap.focused = state.focused_window().cloned();
        snap.sel_tags = sel_tags;
        snap.layout = state
            .monitors
            .get(sel)
            .map(|m| layout_kind_name(m.layout_kind()).to_owned());
        snap.monitors = state.monitors.len();
    });
}

/// Resolve a layout symbol name (`"tile"`, `"col"`, …) to its index in the
/// configured `layouts` list, matching case-insensitively.
fn layout_index_by_name(name: &str) -> Option<usize> {
    crate::config::get()
        .layouts
        .iter()
        .position(|l| layout_kind_name(l.kind).eq_ignore_ascii_case(name))
}

/// Stable name for a layout kind. Mirrors `config::layout_kind_name` but is
/// available regardless of the `pertag-layouts` feature.
const fn layout_kind_name(kind: LayoutKind) -> &'static str {
    match kind {
        #[cfg(feature = "tile")]
        LayoutKind::Tile => "tile",
        #[cfg(feature = "monocle")]
        LayoutKind::Monocle => "monocle",
        #[cfg(feature = "col")]
        LayoutKind::Col => "col",
        #[cfg(feature = "scroll")]
        LayoutKind::Scroll => "scroll",
        #[cfg(feature = "dwindle")]
        LayoutKind::Dwindle => "dwindle",
        #[cfg(feature = "bstack")]
        LayoutKind::Bstack => "bstack",
        #[cfg(feature = "centeredmaster")]
        LayoutKind::CenteredMaster => "centeredmaster",
        LayoutKind::Lua => "lua",
        #[cfg(not(any(
            feature = "tile",
            feature = "monocle",
            feature = "col",
            feature = "scroll",
            feature = "dwindle",
            feature = "bstack",
            feature = "centeredmaster",
        )))]
        LayoutKind::Fallback => "default",
    }
}

/// Compute tiled-window geometries by calling the user's global `on_arrange`
/// Lua callback — the implementation of the `"lua"` layout kind.
///
/// `on_arrange(n, area, symbol)` receives the tiled-window count `n`, an `area`
/// table (`x`, `y`, `w`, `h` work-area rect plus the monitor's `mfact` and
/// `nmaster`), and the active layout's `symbol` string — so one callback can back
/// several `"lua"` layout entries by branching on their symbols. It must return an
/// array of `n` `{ x, y, w, h }` rectangles in tiling order.
/// Anything invalid (missing callback, Lua error, wrong element count) falls back
/// to filling the work area for every window so a window is never lost. Values
/// are read as numbers and rounded; widths/heights are clamped to at least 1px.
///
/// The returned rectangles are treated as **outer cells** (they may tile the work
/// area edge-to-edge): each is inset by the border width so the border — drawn
/// *outside* the window's content rect — stays on screen, matching the built-in
/// layouts. A lone tiled window gets no border, also like the built-ins.
#[allow(clippy::cast_possible_truncation, clippy::many_single_char_names)]
pub fn lua_arrange(
    monitor: &crate::monitor::Monitor,
    cfacts: &[f64],
) -> Vec<smithay::utils::Rectangle<i32, smithay::utils::Logical>> {
    use smithay::utils::Rectangle;
    let n = cfacts.len();
    let area = monitor.w;
    let fallback = || vec![area; n];

    LUA.with(|cell| {
        // try_borrow: never panic if the VM is somehow already borrowed.
        let Ok(guard) = cell.try_borrow() else { return fallback() };
        let Some(lua) = guard.as_ref() else { return fallback() };
        let Ok(func) = lua.globals().get::<mlua::Function>("on_arrange") else {
            return fallback();
        };

        let build_area = || -> mlua::Result<mlua::Table> {
            let a = lua.create_table()?;
            a.set("x", area.loc.x)?;
            a.set("y", area.loc.y)?;
            a.set("w", area.size.w)?;
            a.set("h", area.size.h)?;
            a.set("mfact", monitor.mfact)?;
            a.set("nmaster", monitor.nmaster)?;
            Ok(a)
        };
        let Ok(a) = build_area() else { return fallback() };

        // The active layout's symbol lets one callback serve several `"lua"`
        // layout entries (each config entry sets its own symbol).
        let symbol = monitor.layout_symbol();
        let ret: mlua::Result<mlua::Table> = func.call((n, a, symbol));
        let Ok(tbl) = ret else {
            tracing::warn!("[hook] on_arrange failed; filling work area");
            return fallback();
        };

        // Reserve the border inside each returned cell so edge borders stay on
        // screen (the border is drawn outside the content rect). No border for a
        // lone tiled window, mirroring the built-in layouts.
        let bw = if n > 1 {
            i32::try_from(crate::config::get().border_px).unwrap_or(0)
        } else {
            0
        };

        let mut out = Vec::with_capacity(n);
        for i in 1..=n {
            let Ok(r) = tbl.get::<mlua::Table>(i) else { return fallback() };
            let getf = |k: &str, d: i32| r.get::<f64>(k).map_or(d, |v| v.round() as i32);
            let x = getf("x", area.loc.x) + bw;
            let y = getf("y", area.loc.y) + bw;
            let w = (getf("w", area.size.w) - 2 * bw).max(1);
            let h = (getf("h", area.size.h) - 2 * bw).max(1);
            out.push(Rectangle::new((x, y).into(), (w, h).into()));
        }
        out
    })
}

// ---------------------------------------------------------------------------
// Lua-facing window handle
// ---------------------------------------------------------------------------

/// A live window handle passed to Lua callbacks. Wraps a cheap `Window` clone
/// (an `Rc` internally). Read methods query current state; action methods
/// enqueue a [`HookCmd`].
struct LuaWindow(Window);

impl UserData for LuaWindow {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("app_id", |_, this, ()| {
            Ok(with_state(&this.0, |s| s.last_appid.clone()).flatten())
        });
        methods.add_method("title", |_, this, ()| {
            Ok(with_state(&this.0, |s| s.last_title.clone()).flatten())
        });
        methods.add_method("tags", |_, this, ()| Ok(window_tags(&this.0)));
        methods.add_method("is_floating", |_, this, ()| Ok(window_is_floating(&this.0)));
        methods.add_method("is_fullscreen", |_, this, ()| Ok(window_is_fullscreen(&this.0)));
        // 0-based monitor index this window lives on, or nil if untracked.
        methods.add_method("monitor", |_, this, ()| {
            Ok(with_state(&this.0, |s| i64::try_from(s.mon_idx).unwrap_or(0)))
        });

        methods.add_method("set_tags", |_, this, mask: u32| {
            enqueue(HookCmd::SetTags(this.0.clone(), mask));
            Ok(())
        });
        methods.add_method("set_floating", |_, this, floating: bool| {
            enqueue(HookCmd::SetFloating(this.0.clone(), floating));
            Ok(())
        });
        methods.add_method("focus", |_, this, ()| {
            enqueue(HookCmd::Focus(this.0.clone()));
            Ok(())
        });
        methods.add_method("close", |_, this, ()| {
            enqueue(HookCmd::Close(this.0.clone()));
            Ok(())
        });
        methods.add_method("set_fullscreen", |_, this, fs: bool| {
            enqueue(HookCmd::SetFullscreen(this.0.clone(), fs));
            Ok(())
        });
        methods.add_method("toggle_tag", |_, this, mask: u32| {
            enqueue(HookCmd::ToggleTags(this.0.clone(), mask));
            Ok(())
        });
        methods.add_method("move_to_monitor", |_, this, dir: i32| {
            enqueue(HookCmd::MoveToMonitor(this.0.clone(), dir));
            Ok(())
        });
        #[cfg(feature = "warp")]
        methods.add_method("warp", |_, this, ()| {
            enqueue(HookCmd::Warp(this.0.clone()));
            Ok(())
        });
        #[cfg(feature = "scratchpad")]
        methods.add_method("set_scratch", |_, this, key: String| {
            let key = key.chars().next().unwrap_or('\0');
            enqueue(HookCmd::SetScratch(this.0.clone(), key));
            Ok(())
        });
    }
}

// ---------------------------------------------------------------------------
// Setup / install (called from config loading)
// ---------------------------------------------------------------------------

/// Register the global `rwl` helper table in `lua`. Must run **before** the
/// config source is executed so callbacks can reference `rwl.*`.
#[allow(clippy::too_many_lines)]
pub fn setup(lua: &Lua) -> mlua::Result<()> {
    let rwl = lua.create_table()?;

    rwl.set(
        "spawn",
        lua.create_function(|_, argv: Variadic<String>| {
            enqueue(HookCmd::Spawn(argv.into_iter().collect()));
            Ok(())
        })?,
    )?;
    rwl.set(
        "view",
        lua.create_function(|_, mask: u32| {
            enqueue(HookCmd::View(mask));
            Ok(())
        })?,
    )?;
    rwl.set(
        "notify",
        lua.create_function(|_, msg: String| {
            enqueue(HookCmd::Notify(msg));
            Ok(())
        })?,
    )?;
    // Convenience: rwl.tag(3) -> bitmask 1 << 2 (tag numbers are 1-based).
    rwl.set(
        "tag",
        lua.create_function(|_, n: u32| Ok(if n == 0 { 0u32 } else { 1u32 << (n - 1) }))?,
    )?;
    // rwl.count(mask): windows on the selected monitor carrying any tag in mask.
    // A zero (or omitted) mask counts every window on the monitor.
    rwl.set(
        "count",
        lua.create_function(|_, mask: Option<u32>| {
            let mask = mask.filter(|&m| m != 0).unwrap_or(u32::MAX);
            let n = SNAPSHOT
                .with(|s| s.borrow().tags.iter().filter(|&&t| t & mask != 0).count());
            Ok(i64::try_from(n).unwrap_or(i64::MAX))
        })?,
    )?;
    // rwl.set_layout("col" | 2): switch the selected monitor's layout by symbol
    // name or by raw index. Unknown names / out-of-range indices are ignored.
    rwl.set(
        "set_layout",
        lua.create_function(|_, v: mlua::Value| {
            let idx = match v {
                mlua::Value::Integer(i) => usize::try_from(i).ok(),
                mlua::Value::String(s) => layout_index_by_name(&s.to_string_lossy()),
                _ => None,
            };
            if let Some(idx) = idx {
                enqueue(HookCmd::SetLayout(idx));
            }
            Ok(())
        })?,
    )?;

    // Selected-monitor / focused-window actions — route straight through
    // `dispatch`, so they behave exactly like the equivalent keybinding.
    rwl.set(
        "zoom",
        lua.create_function(|_, ()| {
            enqueue(HookCmd::Dispatch(Action::Zoom));
            Ok(())
        })?,
    )?;
    rwl.set(
        "inc_nmaster",
        lua.create_function(|_, delta: i32| {
            enqueue(HookCmd::Dispatch(Action::IncNmaster(delta)));
            Ok(())
        })?,
    )?;
    rwl.set(
        "set_mfact",
        lua.create_function(|_, delta: f64| {
            enqueue(HookCmd::Dispatch(Action::SetMfact(delta)));
            Ok(())
        })?,
    )?;
    rwl.set(
        "focus_monitor",
        lua.create_function(|_, dir: i32| {
            enqueue(HookCmd::Dispatch(Action::FocusMon(dir)));
            Ok(())
        })?,
    )?;
    // rwl.toggle_view(mask): add/remove tags from the selected monitor's view.
    rwl.set(
        "toggle_view",
        lua.create_function(|_, mask: u32| {
            let mask = mask & crate::config::tag_mask();
            if mask != 0 {
                enqueue(HookCmd::Dispatch(Action::ToggleView(mask)));
            }
            Ok(())
        })?,
    )?;

    // Read helpers — snapshots of selected-monitor state at hook-fire time (see
    // [`refresh`]). They never defer through the queue.
    // rwl.focused(): the focused window handle, or nil.
    rwl.set(
        "focused",
        lua.create_function(|lua, ()| {
            match SNAPSHOT.with(|s| s.borrow().focused.clone()) {
                Some(w) => Ok(mlua::Value::UserData(lua.create_userdata(LuaWindow(w))?)),
                None => Ok(mlua::Value::Nil),
            }
        })?,
    )?;
    // rwl.clients(): array of window handles visible on the current tagset.
    rwl.set(
        "clients",
        lua.create_function(|lua, ()| {
            let ws = SNAPSHOT.with(|s| s.borrow().clients.clone());
            let t = lua.create_table()?;
            for (i, w) in ws.into_iter().enumerate() {
                t.set(i + 1, lua.create_userdata(LuaWindow(w))?)?;
            }
            Ok(t)
        })?,
    )?;
    // rwl.sel_tags(): the selected monitor's active tag bitmask.
    rwl.set(
        "sel_tags",
        lua.create_function(|_, ()| Ok(SNAPSHOT.with(|s| s.borrow().sel_tags)))?,
    )?;
    // rwl.layout(): the selected monitor's current layout symbol name, or nil.
    rwl.set(
        "layout",
        lua.create_function(|_, ()| Ok(SNAPSHOT.with(|s| s.borrow().layout.clone())))?,
    )?;
    // rwl.monitors(): number of connected monitors.
    rwl.set(
        "monitors",
        lua.create_function(|_, ()| {
            Ok(i64::try_from(SNAPSHOT.with(|s| s.borrow().monitors)).unwrap_or(i64::MAX))
        })?,
    )?;

    // rwl.preload("~/pic.png"): decode a wallpaper into the cache on a background
    // thread without displaying it. Call from on_startup for each per-tag
    // wallpaper so the first switch to that tag is instant (no decode stall).
    #[cfg(feature = "wallpaper")]
    rwl.set(
        "preload",
        lua.create_function(|_, path: String| {
            if !path.is_empty() {
                crate::features::wallpaper::request_decode(path);
            }
            Ok(())
        })?,
    )?;
    // rwl.set_wallpaper("~/pic.png", "fill"): set the desktop wallpaper. The
    // mode ("fill"/"fit"/"stretch"/"center") is optional and defaults to the
    // current one. A nil or empty path clears the wallpaper. Handy from
    // `on_tag_switch` for a per-tag background.
    #[cfg(feature = "wallpaper")]
    rwl.set(
        "set_wallpaper",
        lua.create_function(|_, (path, mode): (Option<String>, Option<String>)| {
            let path = path.filter(|p| !p.is_empty());
            let mode = mode
                .as_deref()
                .and_then(crate::features::wallpaper::WallpaperMode::parse);
            enqueue(HookCmd::SetWallpaper(path, mode));
            Ok(())
        })?,
    )?;

    lua.globals().set("rwl", rwl)?;
    Ok(())
}

/// Take ownership of the loaded VM, keeping it alive for hook dispatch. Replaces
/// any previous VM (config reload) and clears per-session tracking.
pub fn install(lua: Lua) {
    LUA.with(|c| *c.borrow_mut() = Some(lua));
    LAST_FOCUS.with(|c| *c.borrow_mut() = None);
    QUEUE.with(|q| q.borrow_mut().clear());
}

// ---------------------------------------------------------------------------
// Event firing
// ---------------------------------------------------------------------------

/// Call the global Lua function `name` with a single window argument, if defined.
fn fire_window(name: &str, window: &Window) {
    let window = window.clone();
    LUA.with(|cell| {
        let guard = cell.borrow();
        let Some(lua) = guard.as_ref() else { return };
        let Ok(func) = lua.globals().get::<mlua::Function>(name) else { return };
        match lua.create_userdata(LuaWindow(window)) {
            Ok(ud) => {
                if let Err(e) = func.call::<()>(ud) {
                    tracing::warn!("[hook] {name}: {e}");
                }
            }
            Err(e) => tracing::warn!("[hook] {name} userdata: {e}"),
        }
    });
}

/// Fire `on_window_open(win)` — the window's rules have just been applied but it
/// is not yet arranged, so `win:set_tags` / `win:set_floating` act like a rule.
/// The just-opened window is already in `state.windows`, so it is included in
/// the `rwl.count` snapshot.
pub fn window_open(state: &Rwl, window: &Window) {
    refresh(state);
    fire_window("on_window_open", window);
}

/// Fire `on_window_close(win)` — the window has already been removed from
/// `state.windows`, so `rwl.count` reflects the post-close tally (useful for
/// reverting a dynamic layout when a tag drops below a threshold).
pub fn window_close(state: &Rwl, window: &Window) {
    refresh(state);
    fire_window("on_window_close", window);
}

/// Fire `on_focus(win)`, de-duplicated so it only runs when the focused client
/// actually changes.
pub fn focus(state: &Rwl, window: &Window) {
    refresh(state);
    let surface = window.wl_surface().map(std::borrow::Cow::into_owned);
    let changed = LAST_FOCUS.with(|last| {
        let mut last = last.borrow_mut();
        if *last == surface {
            false
        } else {
            *last = surface;
            true
        }
    });
    if changed {
        fire_window("on_focus", window);
    }
}

/// Call a global Lua function that takes no arguments, if defined.
fn fire_nullary(name: &str) {
    LUA.with(|cell| {
        let guard = cell.borrow();
        let Some(lua) = guard.as_ref() else { return };
        let Ok(func) = lua.globals().get::<mlua::Function>(name) else { return };
        if let Err(e) = func.call::<()>(()) {
            tracing::warn!("[hook] {name}: {e}");
        }
    });
}

/// Fire the one-time startup hooks, called once the first output exists so live
/// state (current tag, window list) is valid.
///
/// Fires `on_startup()` if defined, then a synthetic `on_tag_switch(0, current)`
/// so per-tag handlers — e.g. a wallpaper per tag — apply to the tag shown at
/// launch instead of leaving it unset (a black screen). Runs at most once per
/// process; it is **not** re-fired on config reload (the wallpaper override, and
/// any other applied state, already survives a reload).
pub fn startup(state: &Rwl, current_tags: u32) {
    if STARTUP_DONE.with(Cell::get) {
        return;
    }
    STARTUP_DONE.with(|d| d.set(true));
    refresh(state);
    fire_nullary("on_startup");
    tag_switch(state, 0, current_tags);
}

/// Fire `on_tag_switch(old_mask, new_mask)`.
pub fn tag_switch(state: &Rwl, old: u32, new: u32) {
    refresh(state);
    LUA.with(|cell| {
        let guard = cell.borrow();
        let Some(lua) = guard.as_ref() else { return };
        let Ok(func) = lua.globals().get::<mlua::Function>("on_tag_switch") else { return };
        if let Err(e) = func.call::<()>((old, new)) {
            tracing::warn!("[hook] on_tag_switch: {e}");
        }
    });
}

/// Fire `on_title_change(win, new_title)`.
pub fn title_change(state: &Rwl, window: &Window, title: &str) {
    refresh(state);
    let window = window.clone();
    let title = title.to_owned();
    LUA.with(|cell| {
        let guard = cell.borrow();
        let Some(lua) = guard.as_ref() else { return };
        let Ok(func) = lua.globals().get::<mlua::Function>("on_title_change") else { return };
        match lua.create_userdata(LuaWindow(window)) {
            Ok(ud) => {
                if let Err(e) = func.call::<()>((ud, title)) {
                    tracing::warn!("[hook] on_title_change: {e}");
                }
            }
            Err(e) => tracing::warn!("[hook] on_title_change userdata: {e}"),
        }
    });
}

/// The layout symbol name at index `idx` in the configured `layouts` list, or an
/// empty string if the index is out of range.
fn layout_name(idx: usize) -> String {
    crate::config::get()
        .layouts
        .get(idx)
        .map_or_else(String::new, |l| layout_kind_name(l.kind).to_owned())
}

/// Fire `on_layout_change(old_name, new_name)` with the selected monitor's layout
/// symbol names (e.g. `"tile"`, `"col"`). Fired generically from `dispatch`
/// whenever the active layout index changes (including a per-tag layout that
/// changes as a side effect of switching tags).
pub fn layout_change(state: &Rwl, old_idx: usize, new_idx: usize) {
    refresh(state);
    let (old, new) = (layout_name(old_idx), layout_name(new_idx));
    LUA.with(|cell| {
        let guard = cell.borrow();
        let Some(lua) = guard.as_ref() else { return };
        let Ok(func) = lua.globals().get::<mlua::Function>("on_layout_change") else { return };
        if let Err(e) = func.call::<()>((old, new)) {
            tracing::warn!("[hook] on_layout_change: {e}");
        }
    });
}

/// Fire `on_fullscreen(win, is_fullscreen)` when a window's fullscreen state is
/// toggled by the user or the client. Not fired for `win:set_fullscreen` calls
/// made from Lua itself (which would risk a feedback loop).
pub fn fullscreen(state: &Rwl, window: &Window, is_fullscreen: bool) {
    refresh(state);
    let window = window.clone();
    LUA.with(|cell| {
        let guard = cell.borrow();
        let Some(lua) = guard.as_ref() else { return };
        let Ok(func) = lua.globals().get::<mlua::Function>("on_fullscreen") else { return };
        match lua.create_userdata(LuaWindow(window)) {
            Ok(ud) => {
                if let Err(e) = func.call::<()>((ud, is_fullscreen)) {
                    tracing::warn!("[hook] on_fullscreen: {e}");
                }
            }
            Err(e) => tracing::warn!("[hook] on_fullscreen userdata: {e}"),
        }
    });
}

/// Call a global Lua function that takes a single string argument, if defined.
fn fire_named(hook: &str, name: &str) {
    let name = name.to_owned();
    LUA.with(|cell| {
        let guard = cell.borrow();
        let Some(lua) = guard.as_ref() else { return };
        let Ok(func) = lua.globals().get::<mlua::Function>(hook) else { return };
        if let Err(e) = func.call::<()>(name) {
            tracing::warn!("[hook] {hook}: {e}");
        }
    });
}

/// Fire `on_monitor_add(name)` after an output has been added (including the
/// first one at startup) — `name` is the connector/output name.
pub fn monitor_add(state: &Rwl, name: &str) {
    refresh(state);
    fire_named("on_monitor_add", name);
}

/// Fire `on_monitor_remove(name)` after an output has been unplugged/removed and
/// its windows migrated to a surviving monitor.
pub fn monitor_remove(state: &Rwl, name: &str) {
    refresh(state);
    fire_named("on_monitor_remove", name);
}
