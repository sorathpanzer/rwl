//! Per-tag layout assignment.
//!
//! Each tag can be pinned to a single layout, or given a list of layouts chosen
//! by the number of tiled windows on that tag:
//!
//! ```lua
//! pertag_layouts = {
//!   [1] = "monocle",                     -- always monocle on tag 1
//!   [2] = { "monocle", "scroll", "col" },-- 1 win → monocle, 2 → scroll, 3+ → col
//! }
//! ```
//!
//! Fixed rules are applied on tag switch ([`apply`]).  Count-based rules are
//! re-evaluated from `arrange()` ([`apply_dynamic`]), where the live tiled count
//! is known, so they react in both directions as windows open and close.  A
//! manual layout change on a tag pins it (via `Monitor::tag_lt_override`) and
//! suppresses both kinds of automatic switching for that tag.

use crate::config::PertagRule;
use crate::monitor::Monitor;

/// Switch `monitor` to layout `idx`, unless it is already active or out of range.
/// Writes `monitor.lt` directly (not via `Monitor::set_layout`) so it does not
/// record a per-tag override — that would pin the tag and defeat auto-switching.
fn set_monitor_layout(monitor: &mut Monitor, idx: usize) {
    let len = crate::config::get().layouts.len();
    if idx < len && monitor.lt[monitor.sel_lt] != idx {
        monitor.sel_lt ^= 1;
        monitor.lt[monitor.sel_lt] = idx;
    }
}

/// True if the user has pinned a layout on this tag via a manual change.
fn is_pinned(monitor: &Monitor, tag_idx: usize) -> bool {
    monitor.tag_lt_override.get(tag_idx).and_then(|x| *x).is_some()
}

/// Apply the fixed layout for `tagmask` to `monitor` on a tag switch.
///
/// Checks the runtime per-tag override first (set by manual layout changes),
/// then the fixed config default.  Count-based rules are left untouched here and
/// resolved by [`apply_dynamic`] once the tiled-window count is available.
/// No-op for multi-tag views.
pub fn apply(monitor: &mut Monitor, tagmask: u32) {
    if !tagmask.is_power_of_two() { return; }
    let tag_idx = tagmask.trailing_zeros() as usize;

    // Manual override always wins.
    if let Some(idx) = monitor.tag_lt_override.get(tag_idx).and_then(|x| *x) {
        set_monitor_layout(monitor, idx);
        return;
    }

    let idx = match crate::config::get().pertag_layouts.get(tag_idx).and_then(Option::as_ref) {
        Some(PertagRule::Fixed(idx)) => *idx,
        // Count-based rules are resolved in `apply_dynamic`, and unset tags keep
        // the monitor's current layout.
        Some(PertagRule::ByCount(_)) | None => return,
    };
    set_monitor_layout(monitor, idx);
}

/// Drop the manual pin on `tagmask` and restore its configured layout.
///
/// Clears `Monitor::tag_lt_override` for the tag so automatic switching resumes,
/// then re-applies the fixed rule immediately.  Count-based rules resolve on the
/// next `arrange()` via [`apply_dynamic`].  No-op for multi-tag views.
pub fn reset(monitor: &mut Monitor, tagmask: u32) {
    if !tagmask.is_power_of_two() { return; }
    let tag_idx = tagmask.trailing_zeros() as usize;
    if let Some(slot) = monitor.tag_lt_override.get_mut(tag_idx) {
        *slot = None;
    }
    apply(monitor, tagmask);
}

/// Re-evaluate a count-based rule for `tagmask` given the current tiled-window
/// `count`.  Called from `arrange()`, which runs on every window open, close and
/// tag switch — so the layout tracks the count in both directions.  No-op for
/// multi-tag views, pinned tags, and tags without a `ByCount` rule.
pub fn apply_dynamic(monitor: &mut Monitor, tagmask: u32, count: usize) {
    if !tagmask.is_power_of_two() { return; }
    let tag_idx = tagmask.trailing_zeros() as usize;
    if is_pinned(monitor, tag_idx) { return; }

    let cfg = crate::config::get();
    let Some(Some(PertagRule::ByCount(list))) = cfg.pertag_layouts.get(tag_idx) else {
        return;
    };
    let Some(&fallback) = list.last() else { return; }; // empty guard (parser prevents)
    // Element i (0-based) → i+1 windows; the last element covers its count and up.
    let idx = list.get(count.saturating_sub(1)).copied().unwrap_or(fallback);
    // Release the read guard before mutating (get() is a non-reentrant RwLock).
    drop(cfg);
    set_monitor_layout(monitor, idx);
}

/// Parse `pertag_layouts = { [1] = "monocle", [2] = { "monocle", "scroll" } }`.
///
/// Keys are 1-based tag numbers.  A value may be a layout kind string, a 0-based
/// layout index, or a table of either (a count-indexed list).  Tags not listed
/// default to the built-in tile layout; unknown names also fall back to tile.
pub fn lua_parse(
    t: &mlua::Table,
    layouts: &[crate::config::LayoutDef],
    tag_count: u32,
) -> Vec<Option<PertagRule>> {
    let count = tag_count as usize;
    let tile_idx = layouts.iter()
        .position(|l| l.kind == crate::config::default_layout_kind())
        .unwrap_or(0);
    let Ok(tbl) = t.get::<mlua::Table>("pertag_layouts") else {
        return vec![Some(PertagRule::Fixed(tile_idx)); count];
    };
    (1..=count)
        .map(|tag_num| {
            let key = i64::try_from(tag_num).unwrap_or(i64::MAX);
            let rule = match tbl.get::<mlua::Value>(key) {
                Ok(mlua::Value::Table(list)) => {
                    let indices: Vec<usize> = list
                        .sequence_values::<mlua::Value>()
                        .filter_map(Result::ok)
                        .filter_map(|v| resolve_layout(&v, layouts, tile_idx, tag_num))
                        .collect();
                    if indices.is_empty() {
                        PertagRule::Fixed(tile_idx)
                    } else {
                        PertagRule::ByCount(indices)
                    }
                }
                Ok(v @ (mlua::Value::String(_) | mlua::Value::Integer(_))) => {
                    PertagRule::Fixed(resolve_layout(&v, layouts, tile_idx, tag_num).unwrap_or(tile_idx))
                }
                _ => PertagRule::Fixed(tile_idx),
            };
            Some(rule)
        })
        .collect()
}

/// Resolve one Lua value (layout name string or 0-based index) to a layout index.
/// Unknown names warn and fall back to tile; out-of-range indices are dropped.
fn resolve_layout(
    v: &mlua::Value,
    layouts: &[crate::config::LayoutDef],
    tile_idx: usize,
    tag_num: usize,
) -> Option<usize> {
    match v {
        mlua::Value::String(s) => {
            let name = s.to_string_lossy();
            Some(
                layouts.iter()
                    .position(|l| crate::config::layout_kind_name(l.kind) == name)
                    .unwrap_or_else(|| {
                        tracing::warn!("Unknown layout '{name}' in pertag_layouts[{tag_num}], defaulting to tile");
                        tile_idx
                    }),
            )
        }
        mlua::Value::Integer(n) => usize::try_from(*n).ok().filter(|&idx| idx < layouts.len()),
        _ => None,
    }
}
