//! Status output — prints compositor state to stdout so that Wayland bars
//! (e.g. sombar, dwl-bar, waybar via the dwl IPC protocol) can display it.
//!
//! Format matches what the patched dwl's `printstatus()` emits.
//! For each monitor, seven lines are written:
//! ```text
//! <name> title <focused_title>
//! <name> appid <focused_appid>
//! <name> fullscreen <0|1>
//! <name> floating <0|1>
//! <name> selmon <0|1>
//! <name> tags <occupied> <selected> <focused_client_tags> <urgent>
//! <name> layout <symbol>
//! ```

use std::fmt::Write as _;
use std::io::Write as _;

use crate::state::Rwl;
use crate::window::{window_visible_on, with_state};

/// Print the current status for all monitors.
///
/// Writes to the startup-command pipe (`state.ipc_out`) when set,
/// otherwise falls back to stdout.  Also fans out to any persistent
/// `subscribe` connections registered in `state.ipc_subscribers`.
pub fn print_status(state: &mut Rwl) {
    let buf = format_status(state);
    if let Some(ref mut w) = state.ipc_out {
        let _ = w.write_all(buf.as_bytes());
        let _ = w.flush();
    } else {
        let stdout = std::io::stdout();
        let mut out = stdout.lock();
        let _ = out.write_all(buf.as_bytes());
        let _ = out.flush();
    }
    #[cfg(feature = "ipc")]
    broadcast_to_subscribers(state, &buf);
}

/// Fan status text out to all persistent `subscribe` connections.
/// Subscribers with an output filter receive only matching lines.
/// Any subscriber whose write fails (client disconnected) is pruned.
#[cfg(feature = "ipc")]
fn broadcast_to_subscribers(state: &mut Rwl, text: &str) {
    if state.ipc_subscribers.is_empty() {
        return;
    }
    state.ipc_subscribers.retain_mut(|(stream, filter)| {
        let out: std::borrow::Cow<str> = filter.as_deref().map_or(
            std::borrow::Cow::Borrowed(text),
            |name| std::borrow::Cow::Owned(
                text.lines()
                    .filter(|l| l.starts_with(name))
                    .flat_map(|l| [l, "\n"])
                    .collect(),
            ),
        );
        if out.is_empty() {
            return true; // filter matched nothing; keep alive
        }
        stream.write_all(out.as_bytes()).is_ok()
    });
}

pub fn format_status(state: &Rwl) -> String {
    let mut buf = String::new();
    let n_mons = state.monitors.len();
    let tag_mask = crate::config::tag_mask();

    // O(N): accumulate occupied/urgent tag bitmasks per monitor in one pass.
    let mut per_mon_tags = vec![(0u32, 0u32); n_mons];
    for w in &state.windows {
        if let Some((mon_idx, tags, is_urgent)) =
            with_state(w, |s| (s.mon_idx, s.tags, s.is_urgent))
            && mon_idx < n_mons
        {
            let slot = &mut per_mon_tags[mon_idx];
            slot.0 |= tags;
            if is_urgent {
                slot.1 |= tags;
            }
        }
    }

    // O(N): find first focus-stack window visible on each monitor's current tagset.
    let mut focused_per_mon: Vec<Option<&smithay::desktop::Window>> = vec![None; n_mons];
    for w in &state.focus_stack {
        if let Some(mon_idx) = with_state(w, |s| s.mon_idx)
            && mon_idx < n_mons
            && focused_per_mon[mon_idx].is_none()
        {
            let tags_selected = state.monitors[mon_idx].tags();
            if window_visible_on(w, tags_selected) {
                focused_per_mon[mon_idx] = Some(w);
            }
        }
    }

    let config_err = crate::config::config_error();

    // O(M): format each monitor using the pre-computed data above.
    for (mon_idx, mon) in state.monitors.iter().enumerate() {
        let name = mon.output.name();
        let tags_selected = mon.tags();
        let is_selmon = mon_idx == state.sel_mon;

        let (occ_raw, urg_raw) = per_mon_tags[mon_idx];
        let tags_occupied = occ_raw & tag_mask;
        let tags_urgent   = urg_raw & tag_mask;

        let focused = focused_per_mon[mon_idx];

        let (focused_title, focused_appid, focused_tags, is_fullscreen, is_floating) =
            focused.map_or_else(
                || (String::new(), String::new(), 0u32, 0u32, 0u32),
                |w| {
                    // Use cached title/appid to avoid re-locking Wayland surface
                    // state (XdgToplevelSurfaceData mutex) on every IPC print.
                    // The cache is maintained by commit() and apply_rules().
                    let (title, appid, ft, fullscreen, floating) = with_state(w, |s| (
                        s.last_title.clone().unwrap_or_default(),
                        s.last_appid.clone().unwrap_or_default(),
                        s.tags,
                        s.is_fullscreen,
                        s.is_floating,
                    )).unwrap_or_default();
                    (title, appid, ft, u32::from(fullscreen), u32::from(floating))
                },
            );

        let layout_sym = mon.layout_symbol();

        if let Some(ref err) = config_err {
            let _ = writeln!(buf, "{name} title_error {err}");
        } else {
            let _ = writeln!(buf, "{name} title {focused_title}");
        }
        let _ = writeln!(buf, "{name} appid {focused_appid}");
        let _ = writeln!(buf, "{name} fullscreen {is_fullscreen}");
        let _ = writeln!(buf, "{name} floating {is_floating}");
        let _ = writeln!(buf, "{name} selmon {}", u32::from(is_selmon));
        let _ = writeln!(
            buf,
            "{name} tags {tags_occupied} {tags_selected} {focused_tags} {tags_urgent}"
        );
        let _ = writeln!(buf, "{name} layout {layout_sym}");
    }

    buf
}
