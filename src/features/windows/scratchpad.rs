//! Scratchpad window management.
//!
//! All scratchpad logic lives here so that the rest of the compositor is
//! unaware of it when the `scratchpad` Cargo feature is disabled.

use smithay::desktop::Window;

use crate::config::ScratchCmd;
use crate::monitor::Monitor;
use crate::state::Rwl;
use crate::window::{set_window_tags, window_is_scratch, window_visible_on, with_state};

impl Rwl {
    pub(crate) fn find_scratch(&self, key: char) -> Option<Window> {
        self.windows
            .iter()
            .find(|w| window_is_scratch(w, key))
            .cloned()
    }

    pub(crate) fn toggle_scratch(&mut self, cmd: &ScratchCmd) {
        if let Some(w) = self.find_scratch(cmd.key) {
            let tags = self.sel_monitor().map_or(0, Monitor::tags);
            let sel = self.sel_mon;
            let visible = window_visible_on(&w, tags);
            if visible {
                set_window_tags(&w, 0);
                self.focus_stack.retain(|fw| fw != &w);
                let top = self.focus_stack.iter()
                    .find(|fw| window_visible_on(fw, tags) && crate::window::with_state(fw, |s| s.mon_idx == sel).unwrap_or(false))
                    .cloned();
                self.focus_window(top);
            } else {
                let show_tags = self.sel_monitor().map_or(1, Monitor::tags);
                set_window_tags(&w, show_tags);
                self.focus_window(Some(w));
            }
            self.arrange_all();
            #[cfg(feature = "warp")]
            crate::features::warp::warp_cursor_to_focused(self);
        } else {
            self.spawn(&cmd.cmd);
        }
    }

    pub(crate) fn focus_or_toggle_scratch(&mut self, cmd: &ScratchCmd) {
        let focused = self.focused_window().cloned();
        if let Some(w) = self.find_scratch(cmd.key) {
            if focused.as_ref() == Some(&w) {
                let tags = self.sel_monitor().map_or(0, Monitor::tags);
                let sel = self.sel_mon;
                set_window_tags(&w, 0);
                self.focus_stack.retain(|fw| fw != &w);
                let top = self.focus_stack.iter()
                    .find(|fw| window_visible_on(fw, tags) && crate::window::with_state(fw, |s| s.mon_idx == sel).unwrap_or(false))
                    .cloned();
                self.focus_window(top);
            } else {
                let show_tags = self.sel_monitor().map_or(1, Monitor::tags);
                set_window_tags(&w, show_tags);
                self.focus_window(Some(w));
            }
            self.arrange_all();
            #[cfg(feature = "warp")]
            crate::features::warp::warp_cursor_to_focused(self);
        } else {
            self.spawn(&cmd.cmd);
        }
    }

    pub(crate) fn focus_or_toggle_matching_scratch(&mut self, cmd: &ScratchCmd) {
        let match_id = cmd.cmd.first().map_or("", String::as_str);
        let tags = self.sel_monitor().map_or(0, Monitor::tags);

        let candidate = self
            .windows
            .iter()
            .find(|w| {
                let is_scratch = window_is_scratch(w, cmd.key);
                let id_match = with_state(w, |s| {
                    s.last_appid.as_deref().is_some_and(|id| id.contains(match_id))
                })
                .unwrap_or(false);
                is_scratch || id_match
            })
            .cloned();

        if let Some(w) = candidate {
            let sel = self.sel_mon;
            let visible = window_visible_on(&w, tags);
            if visible {
                set_window_tags(&w, 0);
                self.focus_stack.retain(|fw| fw != &w);
                let top = self.focus_stack.iter()
                    .find(|fw| window_visible_on(fw, tags) && crate::window::with_state(fw, |s| s.mon_idx == sel).unwrap_or(false))
                    .cloned();
                self.focus_window(top);
            } else {
                let show_tags = self.sel_monitor().map_or(1, Monitor::tags);
                set_window_tags(&w, show_tags);
                self.focus_window(Some(w));
            }
            self.arrange_all();
            #[cfg(feature = "warp")]
            crate::features::warp::warp_cursor_to_focused(self);
        } else {
            self.spawn(&cmd.cmd);
        }
    }
}
