//! Terminal window swallowing (dwm's `swallow` patch).
//!
//! When a terminal spawns a graphical child — e.g. `mpv video.mp4` or `zathura
//! doc.pdf` launched from a shell — the terminal is hidden and the child takes
//! its place in the layout; closing the child restores the terminal.
//!
//! Matching is by **process ancestry**: the child's PID must descend from the
//! terminal's PID (walked via `/proc/<pid>/stat`), and the terminal's `app_id`
//! must be in the configured `swallow.terminals` list. Because rwl inserts new
//! windows at the front of the stack, a just-spawned child already sits above
//! its terminal in tiling order, so hiding the terminal lets the child occupy
//! its slot with no reordering.
//!
//! Compiled only when the `swallow` Cargo feature is enabled. Linux-only (reads
//! `/proc`), which matches rwl's udev/DRM target.

use smithay::desktop::Window;
use smithay::reexports::wayland_server::Resource as _;

use crate::state::Rwl;
use crate::window::{with_state, with_state_mut};

/// Built-in default set of terminal `app_id`s allowed to swallow a child.
/// Matched case-insensitively. Overridden by `swallow = { terminals = { … } }`.
#[must_use]
pub fn default_terminals() -> Vec<String> {
    [
        "foot", "footclient", "st", "xterm", "urxvt", "rxvt", "alacritty",
        "kitty", "wezterm", "org.wezfurlong.wezterm", "contour", "ghostty",
        "com.mitchellh.ghostty", "org.gnome.console", "w+termite",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect()
}

/// Parse `swallow = { terminals = { "foot", … } }`. A missing table or empty
/// list yields the built-in [`default_terminals`].
#[must_use]
pub fn lua_parse(g: &mlua::Table) -> Vec<String> {
    let Ok(t) = g.get::<mlua::Table>("swallow") else {
        return default_terminals();
    };
    let Ok(arr) = t.get::<mlua::Table>("terminals") else {
        return default_terminals();
    };
    let list: Vec<String> = arr.sequence_values::<String>().flatten().collect();
    if list.is_empty() {
        default_terminals()
    } else {
        list
    }
}

/// Whether `app_id` names a terminal permitted to swallow (case-insensitive).
fn is_terminal(app_id: &str) -> bool {
    crate::config::get()
        .swallow_terminals
        .iter()
        .any(|t| t.eq_ignore_ascii_case(app_id))
}

/// PID of the client owning `window`, from its `wl_client` credentials.
fn window_pid(state: &Rwl, window: &Window) -> Option<i32> {
    let client = window.toplevel()?.wl_surface().client()?;
    client.get_credentials(&state.display_handle).ok().map(|c| c.pid)
}

/// Parent PID of `pid`, read from `/proc/<pid>/stat`.
///
/// The `comm` field (2nd) is wrapped in parentheses and may itself contain
/// spaces or `)`, so parse the fields *after the last* `)`: `state ppid …`.
fn parent_pid(pid: i32) -> Option<i32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let tail = stat.rsplit_once(')')?.1;
    let mut fields = tail.split_whitespace();
    let _state = fields.next()?; // field 3
    fields.next()?.parse::<i32>().ok() // field 4: ppid
}

/// The terminal `child` would swallow, if any: the nearest non-swallowed terminal
/// ancestor, honouring the terminal-guard and per-rule `no_swallow` exemptions.
/// Pure query (no state change) so `apply_rules` can consult it *before* firing a
/// `switch_to_tag` view jump the swallow would otherwise contradict.
pub fn find_target(state: &Rwl, child: &Window, appid: &str, title: &str) -> Option<Window> {
    // Don't let a terminal swallow another terminal — opening a new terminal from
    // your shell should never hide the one you launched it from.
    if is_terminal(appid) {
        return None;
    }

    // Honour per-rule `no_swallow` exemptions (same matcher as `apply_rules`).
    if crate::config::get().rules.iter().any(|r| {
        r.no_swallow
            && r.id.as_deref().is_none_or(|id| appid.contains(id))
            && r.title.as_deref().is_none_or(|t| title.contains(t))
    }) {
        return None;
    }

    let child_pid = window_pid(state, child)?;

    // Snapshot swallow-eligible windows once: (pid, window) for non-swallowed
    // terminals other than the child itself.
    let candidates: Vec<(i32, Window)> = state
        .windows
        .iter()
        .filter(|w| *w != child)
        .filter(|w| !with_state(w, |s| s.is_swallowed).unwrap_or(false))
        .filter(|w| {
            with_state(w, |s| s.last_appid.clone())
                .flatten()
                .is_some_and(|a| is_terminal(&a))
        })
        .filter_map(|w| window_pid(state, w).map(|p| (p, w.clone())))
        .collect();
    if candidates.is_empty() {
        return None;
    }

    // Walk up the child's ancestry; return the first (nearest) matching terminal.
    let mut pid = parent_pid(child_pid);
    for _ in 0..32 {
        let p = pid?;
        if p <= 1 {
            return None;
        }
        if let Some((_, term)) = candidates.iter().find(|(cpid, _)| *cpid == p) {
            return Some(term.clone());
        }
        pid = parent_pid(p);
    }
    None
}

/// At map time (after rules), hide the terminal `child` descends from and let
/// `child` take its slot. No-op if the child has no terminal ancestor.
pub fn try_swallow(state: &Rwl, child: &Window) {
    let (appid, title) = with_state(child, |s| {
        (
            s.last_appid.clone().unwrap_or_default(),
            s.last_title.clone().unwrap_or_default(),
        )
    })
    .unwrap_or_default();
    if let Some(term) = find_target(state, child, &appid, &title) {
        do_swallow(child, &term);
    }
}

/// Make `child` inherit `term`'s placement and hide `term`.
fn do_swallow(child: &Window, term: &Window) {
    let (tags, mon, floating, cfact) =
        with_state(term, |s| (s.tags, s.mon_idx, s.is_floating, s.cfact))
            .unwrap_or((1, 0, false, 1.0));
    with_state_mut(child, |s| {
        s.tags = tags;
        s.mon_idx = mon;
        s.is_floating = floating;
        s.cfact = cfact;
        s.needs_centering = floating;
        s.swallowing = Some(term.clone());
    });
    with_state_mut(term, |s| s.is_swallowed = true);
    tracing::debug!("[swallow] hid a terminal behind its child");
}

/// When `window` is unmapped/destroyed: if it was swallowing a terminal, restore
/// that terminal (onto the child's current tags so it reappears where the user
/// is looking) and return it so the caller can focus it; if it *was* a hidden
/// terminal that exited, drop the child's dangling link. The caller re-arranges
/// afterwards.
pub fn on_unmap(state: &Rwl, window: &Window) -> Option<Window> {
    let restored = with_state(window, |s| s.swallowing.clone())
        .flatten()
        .filter(|term| state.windows.contains(term))
        .inspect(|term| {
            let (tags, mon) = with_state(window, |s| (s.tags, s.mon_idx)).unwrap_or((1, 0));
            with_state_mut(term, |s| {
                s.is_swallowed = false;
                s.tags = tags;
                s.mon_idx = mon;
            });
        });

    // A hidden terminal exited on its own: clear any child's stale back-link so
    // it can't try to restore a destroyed window later.
    if with_state(window, |s| s.is_swallowed).unwrap_or(false) {
        for w in &state.windows {
            if with_state(w, |s| s.swallowing.as_ref() == Some(window)).unwrap_or(false) {
                with_state_mut(w, |s| s.swallowing = None);
            }
        }
    }

    restored
}
