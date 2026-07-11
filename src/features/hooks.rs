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
//! function on_tag_switch(old, new)  rwl.notify("tag " .. new) end
//! function on_focus(win)            -- ...
//! function on_title_change(win, t)  -- ...
//! ```
//!
//! Two read helpers query live compositor state at call time (they do not defer
//! through the command queue): `rwl.count(mask)` returns how many windows on the
//! selected monitor carry any tag in `mask`, and `win:tags()` reports a window's
//! tag bitmask. `rwl.set_layout(name_or_index)` switches the selected monitor's
//! layout (by symbol name like `"tile"` / `"col"`, or by raw index).
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
use crate::window::{window_is_floating, window_tags, with_state};

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
    /// Tag bitmask of every window on the selected monitor, refreshed by
    /// [`refresh`] right before each hook fires so `rwl.count(mask)` can answer
    /// synchronously without a live `&Rwl` inside the Lua closure.
    static SNAPSHOT: RefCell<Vec<u32>> = const { RefCell::new(Vec::new()) };
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
    }
}

// ---------------------------------------------------------------------------
// Live-state snapshot (for the synchronous `rwl.count` read helper)
// ---------------------------------------------------------------------------

/// Refresh [`SNAPSHOT`] with the tag bitmask of every window on the selected
/// monitor. Called by the fire helpers before invoking a callback so that
/// `rwl.count(mask)` reflects the compositor state at the moment the hook runs.
pub fn refresh(state: &Rwl) {
    SNAPSHOT.with(|snap| {
        let mut snap = snap.borrow_mut();
        snap.clear();
        for w in &state.windows {
            if with_state(w, |s| s.mon_idx).unwrap_or(0) == state.sel_mon {
                snap.push(window_tags(w));
            }
        }
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
    }
}

// ---------------------------------------------------------------------------
// Setup / install (called from config loading)
// ---------------------------------------------------------------------------

/// Register the global `rwl` helper table in `lua`. Must run **before** the
/// config source is executed so callbacks can reference `rwl.*`.
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
            let n = SNAPSHOT.with(|s| s.borrow().iter().filter(|&&t| t & mask != 0).count());
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
