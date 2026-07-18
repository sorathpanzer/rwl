//! Structured JSON event stream and window-tree query for the `ipc` feature.
//!
//! Two outward-facing, script-friendly surfaces built on the command socket:
//!
//! - **`rwl msg clients`** — a one-shot JSON array describing every window
//!   (app-id, title, tags, monitor, floating/fullscreen/focused flags, and
//!   geometry). Handy for external window switchers (`rofi`, `fuzzel`) and
//!   status scripts.
//! - **`rwl msg watch [kinds…]`** — a live stream of newline-delimited JSON
//!   event objects (`window` open/close, `focus`, `tag`, `title`), optionally
//!   filtered to a set of event kinds.
//!
//! The emit helpers are cheap when nobody is watching: each returns immediately
//! if `event_subscribers` is empty, so the JSON is only built on demand.

use std::io::Write as _;

use smithay::desktop::Window;

use crate::state::Rwl;
use crate::window::with_state;

/// Escape a string for embedding inside a JSON double-quoted value.
fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                // Control chars are < 0x20, so always of the form \u00XX.
                let b = c as u8;
                out.push_str("\\u00");
                out.push(char::from_digit(u32::from(b >> 4), 16).unwrap_or('0'));
                out.push(char::from_digit(u32::from(b & 0xf), 16).unwrap_or('0'));
            }
            c => out.push(c),
        }
    }
    out
}

/// Build a JSON object describing one window.
fn window_obj(state: &Rwl, w: &Window, focused: Option<&Window>) -> String {
    let (appid, title, tags, mon, floating, fullscreen) = with_state(w, |s| {
        (
            s.last_appid.clone().unwrap_or_default(),
            s.last_title.clone().unwrap_or_default(),
            s.tags,
            s.mon_idx,
            s.is_floating,
            s.is_fullscreen,
        )
    })
    .unwrap_or_default();
    let is_focused = focused == Some(w);
    let (x, y, gw, gh) = state
        .space
        .element_geometry(w)
        .map_or((0, 0, 0, 0), |g| (g.loc.x, g.loc.y, g.size.w, g.size.h));
    format!(
        "{{\"app_id\":\"{}\",\"title\":\"{}\",\"tags\":{tags},\"monitor\":{mon},\
         \"floating\":{floating},\"fullscreen\":{fullscreen},\"focused\":{is_focused},\
         \"x\":{x},\"y\":{y},\"w\":{gw},\"h\":{gh}}}",
        esc(&appid),
        esc(&title),
    )
}

/// Render the full window list as a JSON array (the `clients` query response).
#[must_use]
pub fn clients_json(state: &Rwl) -> String {
    let focused = state.focused_window();
    let mut out = String::from("[");
    for (i, w) in state.windows.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&window_obj(state, w, focused));
    }
    out.push_str("]\n");
    out
}

/// Fan a single JSON event line out to every matching `watch` subscriber,
/// pruning any whose write fails (client disconnected).
fn fan_out(state: &mut Rwl, kind: &str, line: &str) {
    if state.event_subscribers.is_empty() {
        return;
    }
    let mut buf = String::with_capacity(line.len() + 1);
    buf.push_str(line);
    buf.push('\n');
    state.event_subscribers.retain_mut(|(stream, filter)| {
        let wanted = filter
            .as_ref()
            .is_none_or(|kinds| kinds.iter().any(|k| k == kind));
        if !wanted {
            return true;
        }
        stream.write_all(buf.as_bytes()).is_ok()
    });
}

/// Emit a `window` event. `action` is `"open"` or `"close"`.
pub fn window(state: &mut Rwl, action: &str, w: &Window) {
    if state.event_subscribers.is_empty() {
        return;
    }
    let obj = window_obj(state, w, state.focused_window());
    let line = format!("{{\"event\":\"window\",\"action\":\"{action}\",\"window\":{obj}}}");
    fan_out(state, "window", &line);
}

/// Emit a `title` event for a window whose title just changed.
pub fn title(state: &mut Rwl, w: &Window) {
    if state.event_subscribers.is_empty() {
        return;
    }
    let obj = window_obj(state, w, state.focused_window());
    let line = format!("{{\"event\":\"title\",\"window\":{obj}}}");
    fan_out(state, "title", &line);
}

/// Emit a `focus` event. `w` is the newly focused window, or `None` when focus
/// was cleared.
pub fn focus(state: &mut Rwl, w: Option<&Window>) {
    if state.event_subscribers.is_empty() {
        return;
    }
    let line = w.map_or_else(
        || "{\"event\":\"focus\",\"window\":null}".to_owned(),
        |w| {
            let obj = window_obj(state, w, Some(w));
            format!("{{\"event\":\"focus\",\"window\":{obj}}}")
        },
    );
    fan_out(state, "focus", &line);
}

/// Emit a `tag` event describing a monitor's selected/occupied tag bitmasks.
pub fn tag(state: &mut Rwl, mon_idx: usize) {
    if state.event_subscribers.is_empty() {
        return;
    }
    let Some(selected) = state.monitors.get(mon_idx).map(crate::monitor::Monitor::tags) else {
        return;
    };
    let mask = crate::config::tag_mask();
    let mut occupied = 0u32;
    for w in &state.windows {
        if let Some((mi, tags)) = with_state(w, |s| (s.mon_idx, s.tags))
            && mi == mon_idx
        {
            occupied |= tags;
        }
    }
    let line = format!(
        "{{\"event\":\"tag\",\"monitor\":{mon_idx},\"selected\":{selected},\"occupied\":{}}}",
        occupied & mask
    );
    fan_out(state, "tag", &line);
}
