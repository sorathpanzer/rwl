//! Compositor WM command socket — accepts IPC commands from `rwl msg`.
//!
//! Creates a Unix socket at `$XDG_RUNTIME_DIR/rwl.sock` and exports its path
//! via the `RWL_SOCK` environment variable so that child processes can reach it
//! with `rwl msg <command>`.  One text command is accepted per connection;
//! query commands (`status`, `info`) write a response back before closing.
//! The special `subscribe [output]` command keeps the connection open: the
//! compositor pushes a fresh status snapshot every time state changes.

use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;

use smithay::reexports::calloop::{generic::Generic, Interest, Mode, PostAction};

use crate::{config::Action, state::Rwl, window::with_state};

/// Returns the canonical socket path: `$XDG_RUNTIME_DIR/rwl.sock`.
///
/// Returns `None` when `XDG_RUNTIME_DIR` is unset.  This command socket accepts
/// privileged commands (including `spawn`, which runs arbitrary programs as the
/// compositor user), so it must live in the user-private, mode-0700
/// `XDG_RUNTIME_DIR`.  Falling back to a world-accessible location such as
/// `/tmp` would let any local user issue commands — we refuse instead.
#[must_use]
pub fn socket_path() -> Option<PathBuf> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")?;
    if dir.is_empty() {
        return None;
    }
    Some(PathBuf::from(dir).join("rwl.sock"))
}

/// Bind the WM IPC socket, register it with the calloop event loop, and export
/// `RWL_SOCK`.  Returns the socket path on success (caller should delete it on
/// shutdown) or `None` if binding failed.
pub fn register(
    loop_handle: &smithay::reexports::calloop::LoopHandle<'static, Rwl>,
) -> Option<PathBuf> {
    let Some(path) = socket_path() else {
        tracing::warn!(
            "[wm-ipc] XDG_RUNTIME_DIR is unset; refusing to create the command \
             socket in a world-accessible location (rwl msg disabled)"
        );
        return None;
    };
    let _ = std::fs::remove_file(&path);
    let listener = match UnixListener::bind(&path) {
        Ok(l) => l,
        Err(e) => { tracing::warn!("[wm-ipc] bind {path:?}: {e}"); return None; }
    };
    // Restrict the socket to the owner (0600). XDG_RUNTIME_DIR is already
    // per-user and mode 0700, but tighten the socket itself as defence in depth
    // so no other user or group can connect and issue commands.
    if let Err(e) = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)) {
        tracing::warn!("[wm-ipc] chmod {path:?}: {e}");
        let _ = std::fs::remove_file(&path);
        return None;
    }
    let _ = listener.set_nonblocking(true);
    if let Err(e) = loop_handle.insert_source(
        Generic::new(listener, Interest::READ, Mode::Level),
        |_, listener, state| {
            loop {
                match listener.accept() {
                    Ok((stream, _)) => cmd_handle(stream, state),
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(_) => break,
                }
            }
            Ok(PostAction::Continue)
        },
    ) {
        tracing::warn!("[wm-ipc] register source: {e}");
        return None;
    }
    Some(path)
}

// ── per-connection handler ────────────────────────────────────────────────────

/// Maximum bytes read from one IPC connection. Commands are short lines; this
/// cap only exists so a flooding client cannot exhaust memory.
const MAX_CMD_BYTES: u64 = 64 * 1024;

fn cmd_handle(mut stream: UnixStream, state: &mut Rwl) {
    // This runs on the compositor event loop, so the read MUST be bounded in
    // both time and size: a client that connects and then stalls (or floods)
    // must not freeze the whole session or exhaust memory. The socket is 0600
    // (owner-only), so this is defence-in-depth against buggy/hostile local
    // clients — a legitimate `rwl msg` writes its command and closes at once.
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_millis(200)));
    let mut buf = String::new();
    if Read::by_ref(&mut stream)
        .take(MAX_CMD_BYTES)
        .read_to_string(&mut buf)
        .is_err()
    {
        return;
    }
    let line = buf.trim();

    // Subscribe: keep stream alive and push status on every state change.
    if line == "subscribe" || line.starts_with("subscribe ") {
        let rest = line["subscribe".len()..].trim();
        let filter: Option<String> = if rest.is_empty() { None } else { Some(rest.to_owned()) };
        // Send the current snapshot immediately so the subscriber is not blank at start.
        let text = crate::ipc::format_status(state);
        let out: String = match filter.as_deref() {
            None => text,
            Some(name) => text
                .lines()
                .filter(|l| l.starts_with(name))
                .flat_map(|l| [l, "\n"])
                .collect(),
        };
        let _ = stream.write_all(out.as_bytes());
        let _ = stream.set_nonblocking(true);
        state.ipc_subscribers.push((stream, filter));
        return;
    }

    cmd_dispatch(line, &mut stream, state);
}

fn find_monitor_idx(state: &Rwl, name: &str) -> Option<usize> {
    match name {
        "selected" => Some(state.sel_mon),
        n => state.monitors.iter().position(|m| m.output.name() == n),
    }
}

#[allow(clippy::too_many_lines)]
fn cmd_dispatch(line: &str, stream: &mut UnixStream, state: &mut Rwl) {
    let words: Vec<&str> = line.split_whitespace().collect();
    let Some(&cmd) = words.first() else { return };
    let args = &words[1..];

    match cmd {
        // ── status query ─────────────────────────────────────────────────────
        "status" => {
            let filter = args.first().copied();
            let text = crate::ipc::format_status(state);
            let out: String = match filter {
                None => text,
                Some(name) => text
                    .lines()
                    .filter(|l| l.starts_with(name))
                    .flat_map(|l| [l, "\n"])
                    .collect(),
            };
            let _ = stream.write_all(out.as_bytes());
        }

        // ── focused window query ──────────────────────────────────────────────
        "info" => {
            let field = args.first().copied();
            let Some(win) = state.focused_window().cloned() else { return };
            let (title, appid, win_tags, fullscreen, floating) =
                with_state(&win, |s| (
                    s.last_title.clone().unwrap_or_default(),
                    s.last_appid.clone().unwrap_or_default(),
                    s.tags,
                    s.is_fullscreen,
                    s.is_floating,
                )).unwrap_or_default();
            let out = match field {
                None => format!(
                    "title: {title}\nclass: {appid}\ntags: {win_tags}\nfullscreen: {}\nfloating: {}\n",
                    u32::from(fullscreen), u32::from(floating),
                ),
                Some("title")          => format!("{title}\n"),
                Some("class" | "appid") => format!("{appid}\n"),
                Some("tags")           => format!("{win_tags}\n"),
                Some("fullscreen")     => format!("{}\n", u32::from(fullscreen)),
                Some("floating")       => format!("{}\n", u32::from(floating)),
                Some(f) => { tracing::debug!("[wm-ipc] info: unknown field '{f}'"); return; }
            };
            let _ = stream.write_all(out.as_bytes());
        }

        // ── per-output commands ───────────────────────────────────────────────
        // Syntax: <cmd> [output] <value>
        //   Two args: args[0] = output name / "all" / "selected", args[1] = value
        //   One arg:  args[0] = value, sel_mon is used
        "view" | "toggleview" | "setlayout" => {
            let (mon_name, val_str) = if args.len() >= 2 {
                (args[0], args[1])
            } else {
                ("selected", args.first().copied().unwrap_or(""))
            };

            let indices: Vec<usize> = if mon_name == "all" {
                (0..state.monitors.len()).collect()
            } else {
                find_monitor_idx(state, mon_name).into_iter().collect()
            };

            let affects_sel = indices.contains(&state.sel_mon);

            for &idx in &indices {
                match cmd {
                    "view" => {
                        if let Ok(mask) = val_str.parse::<u32>() {
                            let mask = mask & crate::config::tag_mask();
                            if mask == 0 { continue; }
                            if idx == state.sel_mon {
                                state.update_tag_history(idx, mask);
                            }
                            #[cfg(feature = "tag-transition")]
                            let old_tag = state.monitors.get(idx).map_or(0, crate::monitor::Monitor::tags);
                            #[cfg(feature = "tag-transition")]
                            let output_w = state.monitors.get(idx).map_or(1920.0, |m| f64::from(m.w.size.w));
                            if let Some(m) = state.monitors.get_mut(idx) {
                                m.view(mask);
                            }
                            #[cfg(feature = "tag-transition")]
                            crate::features::tag_transition::mark_slide_outs(&state.windows, old_tag, mask, output_w);
                            #[cfg(feature = "tag-transition")]
                            { state.pending_tag_transition = Some((old_tag, mask, output_w)); }
                        }
                    }
                    "toggleview" => {
                        if let Ok(mask) = val_str.parse::<u32>() {
                            let new = state.monitors.get(idx)
                                .map_or(0, |m| (m.tags() ^ mask) & crate::config::tag_mask());
                            if new != 0 {
                                if idx == state.sel_mon {
                                    state.update_tag_history(idx, new);
                                }
                                if let Some(m) = state.monitors.get_mut(idx) {
                                    m.toggle_view(mask);
                                }
                            }
                        }
                    }
                    "setlayout" => {
                        if let Ok(li) = val_str.parse::<usize>()
                            && let Some(m) = state.monitors.get_mut(idx)
                        {
                            m.set_layout(li);
                        }
                    }
                    _ => {}
                }
            }

            state.arrange_all();
            if affects_sel {
                let top = state.focused_window().cloned();
                state.focus_window(top);
            }
        }

        // ── action dispatch ───────────────────────────────────────────────────
        "tag" => {
            if let Some(mask) = args.first().and_then(|s| s.parse::<u32>().ok()) {
                state.dispatch(&Action::Tag(mask));
            }
        }
        "toggletag" => {
            if let Some(mask) = args.first().and_then(|s| s.parse::<u32>().ok()) {
                state.dispatch(&Action::ToggleTag(mask));
            }
        }
        "focusstack" => {
            if let Some(d) = args.first().and_then(|s| s.parse::<i32>().ok()) {
                state.dispatch(&Action::FocusStack(d));
            }
        }
        "incnmaster" => {
            if let Some(d) = args.first().and_then(|s| s.parse::<i32>().ok()) {
                state.dispatch(&Action::IncNmaster(d));
            }
        }
        "setmfact" => {
            if let Some(d) = args.first().and_then(|s| s.parse::<f64>().ok()) {
                state.dispatch(&Action::SetMfact(d));
            }
        }
        "viewnextocctag" => {
            if let Some(d) = args.first().and_then(|s| s.parse::<i32>().ok()) {
                state.dispatch(&Action::ViewNextOccTag(d));
            }
        }
        "focusmon" => {
            if let Some(d) = args.first().and_then(|s| s.parse::<i32>().ok()) {
                state.dispatch(&Action::FocusMon(d));
            }
        }
        "tagmon" => {
            if let Some(d) = args.first().and_then(|s| s.parse::<i32>().ok()) {
                state.dispatch(&Action::TagMon(d));
            }
        }
        "chvt" => {
            if let Some(n) = args.first().and_then(|s| s.parse::<u32>().ok()) {
                state.dispatch(&Action::Chvt(n));
            }
        }
        "viewtag_spawn" => {
            if let Some(mask) = args.first().and_then(|s| s.parse::<u32>().ok()) {
                state.dispatch(&Action::ViewTagSpawn(mask));
            }
        }
        "spawn" => {
            if !args.is_empty() {
                let cmd_vec: Vec<String> = args.iter().map(|s| (*s).to_owned()).collect();
                state.dispatch(&Action::Spawn(cmd_vec));
            }
        }
        "cyclelayout"       => state.dispatch(&Action::CycleLayout),
        "zoom"              => state.dispatch(&Action::Zoom),
        "viewprev"          => state.dispatch(&Action::ViewPrev),
        "killclient"        => state.dispatch(&Action::KillClient),
        "togglefloating"    => state.dispatch(&Action::ToggleFloating),
        "togglefullscreen"  => state.dispatch(&Action::ToggleFullscreen),
        "togglepassthrough" => state.dispatch(&Action::TogglePassthrough),
        "reloadconfig"      => state.dispatch(&Action::ReloadConfig),
        "quit"              => state.dispatch(&Action::Quit),
        "focusurgent"       => state.focus_urgent(),
        #[cfg(feature = "bar")]
        "togglebar"         => state.dispatch(&Action::ToggleBar),
        #[cfg(feature = "bar")]
        "barprompt"         => state.dispatch(&Action::BarPrompt),
        #[cfg(feature = "gaps")]
        "togglegaps"        => state.dispatch(&Action::ToggleGaps),
        #[cfg(feature = "lock")]
        "lock"              => state.dispatch(&Action::Lock),
        // ── wallpaper ─────────────────────────────────────────────────────────
        // Syntax: wallpaper <path> [fill|fit|stretch|center]
        //         wallpaper off|none|clear
        #[cfg(feature = "wallpaper")]
        "wallpaper" => {
            match args.first().copied() {
                Some("off" | "none" | "clear") => {
                    crate::features::wallpaper::set_override(None, None);
                    state.schedule_render();
                }
                Some(path) => {
                    let mode = args.get(1)
                        .and_then(|s| crate::features::wallpaper::WallpaperMode::parse(s));
                    crate::features::wallpaper::set_override(Some((*path).to_owned()), mode);
                    state.schedule_render();
                }
                None => tracing::debug!("[wm-ipc] wallpaper: missing path"),
            }
        }
        _ => tracing::debug!("[wm-ipc] unknown command: {cmd}"),
    }
}
