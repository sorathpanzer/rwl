//! `ipc` feature — WM command socket server and `rwl msg` client.
//!
//! - [`server`] runs inside the compositor: binds `$XDG_RUNTIME_DIR/rwl.sock`
//!   and dispatches incoming text commands to [`crate::state::Rwl`].
//! - This module is the client side: `rwl msg …` calls [`run`] before any
//!   compositor state is initialised.
//!
//! **WM commands** (sent to `$RWL_SOCK`):
//! ```text
//! rwl msg status [<output>]
//! rwl msg subscribe [<output>]
//! rwl msg clients                          (JSON array of all windows)
//! rwl msg watch [window,focus,tag,title]   (newline-delimited JSON events)
//! rwl msg view [<output>] <tagmask>
//! rwl msg toggleview [<output>] <tagmask>
//! rwl msg setlayout [<output>] <index>
//! rwl msg tag <tagmask>
//! rwl msg toggletag <tagmask>
//! rwl msg focusstack <1|-1>
//! rwl msg incnmaster <delta>
//! rwl msg setmfact <delta>
//! rwl msg cyclelayout
//! rwl msg zoom
//! rwl msg viewprev
//! rwl msg viewnextocctag <1|-1>
//! rwl msg killclient
//! rwl msg togglefloating
//! rwl msg togglefullscreen
//! rwl msg togglepassthrough
//! rwl msg focusmon <1|-1>
//! rwl msg tagmon <1|-1>
//! rwl msg chvt <n>
//! rwl msg viewtag_spawn <tagmask>
//! rwl msg spawn <cmd> [args…]
//! rwl msg focusurgent
//! rwl msg reloadconfig
//! rwl msg quit
//! rwl msg info [title|class|appid|tags|fullscreen|floating]
//! rwl msg togglebar        (bar feature)
//! rwl msg barprompt        (bar feature)
//! rwl msg togglegaps       (gaps feature)
//! rwl msg wallpaper <path> [fill|fit|stretch|center] | wallpaper off  (wallpaper feature)
//! ```
//!
//! **Bar commands** (sent to the embedded bar's socket, `bar` feature only):
//! ```text
//! rwl msg -prompt <output>
//! rwl msg -status <output> <text>
//! rwl msg -title <output> [<secs>] [<#rrggbb>] <text>
//! rwl msg -show|-hide|-toggle-visibility <output>
//! rwl msg -set-top|-set-bottom|-toggle-location <output>
//! ```
//! `<output>` may be `all`, `selected`, or a named output (e.g. `eDP-1`).

pub mod event;
pub mod server;

use std::io::{BufRead as _, Read, Write as _};
use std::os::unix::net::UnixStream;

// ── bar socket helpers (bar feature only) ─────────────────────────────────────

#[cfg(feature = "bar")]
use std::{fs, path::Path, path::PathBuf};

#[cfg(feature = "bar")]
fn bar_socket_dir() -> Option<PathBuf> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")?;
    if dir.is_empty() {
        return None;
    }
    Some(PathBuf::from(dir).join("dwlb"))
}

#[cfg(feature = "bar")]
fn bar_send(dir: &Path, output: &str, cmd: &str, data: Option<&str>) {
    let msg = data.map_or_else(
        || format!("{output} {cmd}"),
        |d| format!("{output} {cmd} {d}"),
    );
    let Ok(entries) = fs::read_dir(dir) else { return };
    entries
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("dwlb-"))
        .filter_map(|e| UnixStream::connect(e.path()).ok())
        .for_each(|mut s| { let _ = s.write_all(msg.as_bytes()); });
}

// ── WM socket helpers ─────────────────────────────────────────────────────────

fn wm_connect() -> Option<UnixStream> {
    let path = std::env::var("RWL_SOCK").ok()?;
    UnixStream::connect(path).ok()
}

fn wm_send(cmd_str: &str) {
    let Some(mut s) = wm_connect() else {
        eprintln!("rwl msg: RWL_SOCK not set or compositor not running");
        return;
    };
    let _ = s.write_all(cmd_str.as_bytes());
}

fn wm_query(cmd_str: &str) {
    let Some(mut s) = wm_connect() else {
        eprintln!("rwl msg: RWL_SOCK not set or compositor not running");
        return;
    };
    let _ = s.write_all(cmd_str.as_bytes());
    let _ = s.shutdown(std::net::Shutdown::Write);
    let mut buf = String::new();
    let _ = s.read_to_string(&mut buf);
    print!("{buf}");
}

/// Send a keep-alive command (`subscribe` / `watch`) and print each line the
/// compositor pushes until the connection closes.
fn wm_stream(cmd_str: &str) {
    let Some(mut s) = wm_connect() else {
        eprintln!("rwl msg: RWL_SOCK not set or compositor not running");
        return;
    };
    let _ = s.write_all(cmd_str.as_bytes());
    let _ = s.shutdown(std::net::Shutdown::Write);
    let stdout = std::io::stdout();
    for line in std::io::BufReader::new(s).lines() {
        let Ok(l) = line else { break };
        // Flush each line so consumers piping the stream (a window switcher, a
        // `while read` loop) receive events immediately instead of in blocks.
        let mut out = stdout.lock();
        if writeln!(out, "{l}").is_err() || out.flush().is_err() {
            break;
        }
    }
}

// ── usage ─────────────────────────────────────────────────────────────────────

fn usage() {
    eprintln!("Usage: rwl msg <wm-command> [args…]");
    eprintln!("       rwl msg status [<output>]");
    eprintln!("       rwl msg subscribe [<output>]");
    eprintln!("       rwl msg clients                         (JSON window list)");
    eprintln!("       rwl msg watch [window,focus,tag,title]  (JSON event stream)");
    eprintln!("       rwl msg view [<output>] <tagmask>");
    eprintln!("       rwl msg toggleview [<output>] <tagmask>");
    eprintln!("       rwl msg setlayout [<output>] <index>");
    eprintln!("       rwl msg tag|toggletag <tagmask>");
    eprintln!("       rwl msg focusstack|focusmon|tagmon|viewnextocctag <1|-1>");
    eprintln!("       rwl msg incnmaster <delta>");
    eprintln!("       rwl msg setmfact <delta>");
    eprintln!("       rwl msg cyclelayout|zoom|viewprev|killclient|togglefloating|togglefullscreen");
    eprintln!("       rwl msg togglepassthrough|reloadconfig|focusurgent|quit");
    eprintln!("       rwl msg spawn <cmd> [args…]");
    eprintln!("       rwl msg chvt <n>");
    eprintln!("       rwl msg viewtag_spawn <tagmask>");
    eprintln!("       rwl msg info [title|class|appid|tags|fullscreen|floating]");
    #[cfg(feature = "bar")]
    {
        eprintln!("       rwl msg togglebar|barprompt");
        eprintln!("       rwl msg -prompt <output>");
        eprintln!("       rwl msg -status <output> <text>");
        eprintln!("       rwl msg -title <output> [<secs>] [<#rrggbb>] <text>");
        eprintln!("       rwl msg -show|-hide|-toggle-visibility <output>");
        eprintln!("       rwl msg -set-top|-set-bottom|-toggle-location <output>");
    }
    #[cfg(feature = "gaps")]
    eprintln!("       rwl msg togglegaps");
    #[cfg(feature = "lock")]
    eprintln!("       rwl msg lock");
    #[cfg(feature = "wallpaper")]
    eprintln!("       rwl msg wallpaper <path> [fill|fit|stretch|center] | wallpaper off");
}

// ── public entry point ────────────────────────────────────────────────────────

/// Entry point for `rwl msg …`. Call this from `main()` when `argv[1] == "msg"`.
pub fn run(args: &[String]) {
    if args.is_empty() {
        usage();
        return;
    }

    // Bar commands start with '-'.
    if args[0].starts_with('-') {
        #[cfg(feature = "bar")]
        run_bar(args);
        #[cfg(not(feature = "bar"))]
        eprintln!("rwl msg: bar commands require the 'bar' feature");
        return;
    }

    // WM commands.
    run_wm(args);
}

// ── WM command dispatch ───────────────────────────────────────────────────────

fn run_wm(args: &[String]) {
    let cmd = args[0].as_str();

    match cmd {
        // Queries — read response back from compositor.
        "status" | "info" | "clients" => {
            wm_query(&args.join(" "));
        }

        // Subscribe: compositor pushes status on every state change.
        "subscribe" => {
            let cmd_str = if args.len() >= 2 {
                format!("subscribe {}", args[1])
            } else {
                "subscribe".to_owned()
            };
            wm_stream(&cmd_str);
        }

        // Watch: compositor pushes structured JSON events. Optional event-kind
        // filter, e.g. `watch window focus`.
        "watch" => {
            let cmd_str = if args.len() >= 2 {
                format!("watch {}", args[1..].join(" "))
            } else {
                "watch".to_owned()
            };
            wm_stream(&cmd_str);
        }

        // Per-output commands: optional second arg is the output name.
        "view" | "toggleview" | "setlayout" => {
            wm_send(&args.join(" "));
        }

        // Single-value commands.
        "tag" | "toggletag" if args.len() >= 2 => { wm_send(&args.join(" ")); }
        "focusstack" | "focusmon" | "tagmon" | "viewnextocctag" if args.len() >= 2 => {
            wm_send(&args.join(" "));
        }
        "incnmaster" if args.len() >= 2 => { wm_send(&args.join(" ")); }
        "setmfact"   if args.len() >= 2 => { wm_send(&args.join(" ")); }
        "chvt"       if args.len() >= 2 => { wm_send(&args.join(" ")); }
        "viewtag_spawn" if args.len() >= 2 => { wm_send(&args.join(" ")); }

        // Spawn: rest of args are the command.
        "spawn" if args.len() >= 2 => { wm_send(&args.join(" ")); }

        // Argumentless commands.
        "cyclelayout" | "zoom" | "viewprev" | "killclient" | "togglefloating" | "togglefullscreen"
        | "togglepassthrough" | "reloadconfig" | "focusurgent" | "quit" => {
            wm_send(cmd);
        }

        #[cfg(feature = "bar")]
        "togglebar" | "barprompt" => { wm_send(cmd); }

        #[cfg(feature = "gaps")]
        "togglegaps" => { wm_send(cmd); }

        #[cfg(feature = "lock")]
        "lock" => { wm_send(cmd); }

        #[cfg(feature = "wallpaper")]
        "wallpaper" if args.len() >= 2 => { wm_send(&args.join(" ")); }

        _ => {
            eprintln!("rwl msg: unknown or incomplete command: {cmd}");
            usage();
        }
    }
}

// ── bar command dispatch (bar feature only) ───────────────────────────────────

#[cfg(feature = "bar")]
fn run_bar(args: &[String]) {
    let Some(dir) = bar_socket_dir() else {
        eprintln!("rwl msg: XDG_RUNTIME_DIR not set; cannot locate bar sockets");
        return;
    };
    let mut i = 0usize;
    while i < args.len() {
        let arg = args[i].as_str();
        let has1 = i + 1 < args.len();
        let has2 = i + 2 < args.len();
        match arg {
            "-prompt" if has1 => {
                bar_send(&dir, &args[i + 1], "prompt", None);
                i += 2;
            }
            "-status" if has2 => {
                bar_send(&dir, &args[i + 1], "status", Some(&args[i + 2]));
                i += 3;
            }
            "-title" if has2 => {
                let output = &args[i + 1];
                let mut j = i + 2;
                let mut payload = String::new();
                // optional duration (all-digit token)
                if j < args.len()
                    && !args[j].is_empty()
                    && args[j].chars().all(|c| c.is_ascii_digit())
                {
                    payload.push_str(&args[j]);
                    payload.push(' ');
                    j += 1;
                }
                // optional #rrggbb color
                if j < args.len() && args[j].starts_with('#') {
                    payload.push_str(&args[j]);
                    payload.push(' ');
                    j += 1;
                }
                if j >= args.len() {
                    eprintln!("rwl msg: -title requires a text argument");
                    return;
                }
                payload.push_str(&args[j]);
                j += 1;
                bar_send(&dir, output, "title", Some(&payload));
                i = j;
            }
            "-show" if has1 => {
                bar_send(&dir, &args[i + 1], "show", None);
                i += 2;
            }
            "-hide" if has1 => {
                bar_send(&dir, &args[i + 1], "hide", None);
                i += 2;
            }
            "-toggle-visibility" if has1 => {
                bar_send(&dir, &args[i + 1], "toggle-visibility", None);
                i += 2;
            }
            "-set-top" if has1 => {
                bar_send(&dir, &args[i + 1], "set-top", None);
                i += 2;
            }
            "-set-bottom" if has1 => {
                bar_send(&dir, &args[i + 1], "set-bottom", None);
                i += 2;
            }
            "-toggle-location" if has1 => {
                bar_send(&dir, &args[i + 1], "toggle-location", None);
                i += 2;
            }
            other => {
                eprintln!("rwl msg: unknown or incomplete bar command: {other}");
                usage();
                return;
            }
        }
    }
}
