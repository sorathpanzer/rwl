//! Per-monitor (output) state.

use smithay::output::Output;
use smithay::utils::{Logical, Rectangle};

use crate::config::{LayoutKind, MonitorRule};

// ---------------------------------------------------------------------------
// Monitor struct
// ---------------------------------------------------------------------------

/// All compositor-side state for one output.
#[derive(Debug)]
pub struct Monitor {
    /// The underlying smithay output handle.
    pub output: Output,

    /// The two tag sets (current and previous), indexed by `sel_tags`.
    pub tagset: [u32; 2],
    /// Which of the two tag sets is currently active (0 or 1).
    pub sel_tags: usize,

    /// The two active layout slots (current and previous), indexed by `sel_lt`.
    pub lt: [usize; 2],
    /// Which of the two layout slots is active (0 or 1).
    pub sel_lt: usize,

    /// Master area fraction (`0.0`..`1.0`).
    pub mfact: f64,
    /// Number of master windows.
    pub nmaster: i32,

    /// Full monitor geometry (including bars), layout-relative.
    pub m: Rectangle<i32, Logical>,
    /// Usable window area (after subtracting exclusive layer-shell zones).
    pub w: Rectangle<i32, Logical>,

    /// Whether the monitor is in a "sleep" / DPMS-off state.
    #[allow(dead_code)]
    pub asleep: bool,

    /// Runtime per-tag layout override (by tag bit position).
    /// Set when the user manually changes layout on a single-tag view.
    /// Takes priority over `Config.pertag_layouts` when switching back to that tag.
    #[cfg(feature = "pertag-layouts")]
    pub tag_lt_override: Vec<Option<usize>>,

    /// Per-column width fractions for the col layout (values sum to 1.0).
    /// When the length doesn't match the current window count, col layout
    /// falls back to equal widths and this vec is ignored.
    #[cfg(feature = "col")]
    pub col_facts: Vec<f64>,

    /// Index of the focused column in the scrollable layouts (monocle and
    /// scroll).  Updated by `arrange()` before `layout::arrange` is called so
    /// `monocle::arrange()` / `scroll::arrange()` can center the focused window.
    #[cfg(any(feature = "monocle", feature = "scroll"))]
    pub scroll_col: usize,
}

impl Monitor {
    /// Create a new [`Monitor`] applying the first matching [`MonitorRule`].
    #[must_use]
    pub fn new(output: Output) -> Self {
        let name = output.name();
        let rule = matching_rule(name.as_str());

        let initial_tagset = 1u32; // show tag 1 by default
        let lt_idx = rule.layout_idx.min(crate::config::get().layouts.len().saturating_sub(1));

        Self {
            output,
            tagset: [initial_tagset, initial_tagset],
            sel_tags: 0,
            lt: [lt_idx, lt_idx],
            sel_lt: 0,
            mfact: rule.mfact,
            nmaster: rule.nmaster,
            m: Rectangle::default(),
            w: Rectangle::default(),
            asleep: false,
            #[cfg(feature = "pertag-layouts")]
            tag_lt_override: vec![None; crate::config::get().tag_count as usize],
            #[cfg(feature = "col")]
            col_facts: Vec::new(),
            #[cfg(any(feature = "monocle", feature = "scroll"))]
            scroll_col: 0,
        }
    }

    /// Active tag bitmask.
    #[inline]
    #[must_use]
    pub const fn tags(&self) -> u32 {
        self.tagset[self.sel_tags]
    }

    /// Mutable reference to the active tag bitmask.
    #[inline]
    #[allow(dead_code)]
    pub const fn tags_mut(&mut self) -> &mut u32 {
        &mut self.tagset[self.sel_tags]
    }

    /// Active layout kind (allocation-free — `LayoutKind` is `Copy`).
    ///
    /// Falls back to [`default_layout_kind`](crate::config::default_layout_kind)
    /// when the stored index is no longer valid after a `ReloadConfig` that
    /// shrinks the layouts list.
    /// Index (into the configured `layouts` list) of the active layout. Used to
    /// detect layout changes for the `on_layout_change` hook.
    #[cfg(any(feature = "hooks", feature = "ipc"))]
    #[inline]
    #[must_use]
    pub const fn layout_idx(&self) -> usize {
        self.lt[self.sel_lt]
    }

    #[inline]
    #[must_use]
    pub fn layout_kind(&self) -> LayoutKind {
        let cfg = crate::config::get();
        cfg.layouts
            .get(self.lt[self.sel_lt])
            .map_or_else(crate::config::default_layout_kind, |l| l.kind)
    }

    /// Layout symbol string (at most 15 chars to fit dwl's `ltsymbol[16]`).
    #[inline]
    #[must_use]
    pub fn layout_symbol(&self) -> String {
        let cfg = crate::config::get();
        cfg.layouts
            .get(self.lt[self.sel_lt])
            .map_or_else(|| "><>".to_owned(), |l| l.symbol.clone())
    }

    /// Switch to a new layout by index.  If `idx == usize::MAX` toggle between
    /// the two most-recent layouts.
    pub fn set_layout(&mut self, idx: usize) {
        if idx == usize::MAX {
            self.sel_lt ^= 1;
        } else if idx < crate::config::get().layouts.len() && self.lt[self.sel_lt] != idx {
            self.sel_lt ^= 1;
            self.lt[self.sel_lt] = idx;
        } else {
            return;
        }
        // Save the chosen layout per-tag so switching away and back restores it.
        #[cfg(feature = "pertag-layouts")]
        {
            let tag = self.tags();
            if tag.is_power_of_two() {
                let tag_idx = tag.trailing_zeros() as usize;
                if let Some(slot) = self.tag_lt_override.get_mut(tag_idx) {
                    *slot = Some(self.lt[self.sel_lt]);
                }
            }
        }
    }

    /// View a new tag bitmask, saving the previous one.
    pub fn view(&mut self, tagmask: u32) {
        let new_tags = tagmask & crate::config::tag_mask();
        if new_tags == 0 || new_tags == self.tagset[self.sel_tags] {
            return;
        }
        self.sel_tags ^= 1;
        self.tagset[self.sel_tags] = new_tags;
        #[cfg(feature = "pertag-layouts")]
        crate::features::pertag_layouts::apply(self, new_tags);
    }

    /// Toggle visibility of `tagmask` in the current view.
    pub fn toggle_view(&mut self, tagmask: u32) {
        let new = (self.tagset[self.sel_tags] ^ tagmask) & crate::config::tag_mask();
        if new != 0 {
            self.sel_tags ^= 1;
            self.tagset[self.sel_tags] = new;
        }
    }

    /// Return to the previous tag set (swap `sel_tags`).
    pub fn view_prev(&mut self) {
        self.sel_tags ^= 1;
        #[cfg(feature = "pertag-layouts")]
        crate::features::pertag_layouts::apply(self, self.tagset[self.sel_tags]);
    }

    /// Return the next occupied tag after (or before) the currently active one.
    ///
    /// `dir` should be `1` (forward) or `-1` (backward).  Returns `None` if
    /// there are no windows at all.
    #[must_use]
    #[allow(clippy::cast_possible_wrap)]
    pub fn next_occupied_tag(&self, occupied: u32, dir: i32) -> Option<u32> {
        let current = self.tagset[self.sel_tags];
        if current == 0 {
            return None;
        }
        // Use the highest set bit as the "current" position so that navigation
        // from a multi-tag view starts from the most significant active tag.
        let pos = (u32::BITS - 1 - current.leading_zeros()) as i32;
        let count = crate::config::get().tag_count as i32;

        (1..=count).find_map(|step| {
            let mask = 1u32 << ((pos + dir * step).rem_euclid(count) as u32);
            (occupied & mask != 0).then_some(mask)
        })
    }
}

// ---------------------------------------------------------------------------
// Rule matching
// ---------------------------------------------------------------------------

fn matching_rule(name: &str) -> MonitorRule {
    let cfg = crate::config::get();
    cfg.monitor_rules
        .iter()
        .find_map(|rule| {
            rule.name.as_deref()
                .filter(|&prefix| name.starts_with(prefix))
                .map(|_| rule.clone())
        })
        .or_else(|| cfg.monitor_rules.iter().rev().find(|r| r.name.is_none()).cloned())
        .or_else(|| cfg.monitor_rules.first().cloned())
        .unwrap_or_default()
}

/// Public wrapper so `handlers/output.rs` can apply the rule for a given
/// smithay [`Output`] without duplicating the matching logic.
#[must_use]
pub fn matching_rule_for_output(output: &Output) -> MonitorRule {
    matching_rule(output.name().as_str())
}
