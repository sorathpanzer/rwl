//! Output (monitor) creation and destruction.

use smithay::output::Output;
use smithay::utils::Rectangle;
use smithay::wayland::output::OutputHandler;

use crate::monitor::Monitor;
use crate::state::Rwl;

impl OutputHandler for Rwl {}

// ---------------------------------------------------------------------------
// Public helpers called from the backend
// ---------------------------------------------------------------------------

impl Rwl {
    /// Register a new output produced by the backend.
    #[allow(clippy::cast_possible_truncation)]
    pub fn output_added(&mut self, output: &Output) {
        // Apply scale and transform from monitor rules
        let rule = crate::monitor::matching_rule_for_output(output);
        output.change_current_state(
            None,
            Some(rule.transform.into()),
            Some(smithay::output::Scale::Fractional(rule.scale)),
            None,
        );

        // Place the output in the layout (auto-position or fixed)
        let loc = if rule.x >= 0 && rule.y >= 0 {
            (rule.x, rule.y)
        } else {
            // Auto-position: append to the right of existing monitors
            let x = self
                .monitors
                .iter()
                .map(|m| m.m.loc.x + m.m.size.w)
                .max()
                .unwrap_or(0);
            (x, 0)
        };

        self.space.map_output(output, loc);
        output.change_current_state(None, None, None, Some(loc.into()));

        // Update monitor geometry from output mode
        let (w, h) = output
            .current_mode()
            .map_or((1920, 1080), |mode| (mode.size.w, mode.size.h));
        let scale = output.current_scale().fractional_scale();
        let logical_w = (f64::from(w) / scale) as i32;
        let logical_h = (f64::from(h) / scale) as i32;

        let mut mon = Monitor::new(output.clone());
        mon.m = Rectangle::new(loc.into(), (logical_w, logical_h).into());
        mon.w = mon.m; // will be adjusted by layer-shell exclusive zones

        self.monitors.push(mon);
        self.tag_history.push(std::collections::VecDeque::new());
        self.update_monitor_bounds();

        self.arrange_all();

        // Udev backend: pointer is created before the first output fires, so
        // do the initial centre warp here (flag prevents repeats on hotplug).
        #[cfg(feature = "warp")]
        if !self.initial_warp_done && self.pointer.is_some() {
            self.initial_warp_done = true;
            crate::features::warp::warp_cursor_to_focused(self);
        }

        // Decode + GPU-warm every configured wallpaper in the background so the
        // first switch to any tag is instant. Idempotent across outputs/hotplug.
        #[cfg(feature = "wallpaper")]
        crate::features::wallpaper::preload_configured();

        // Fire the one-time startup hooks now that a monitor and its initial tag
        // exist, so a per-tag wallpaper (or other on_tag_switch state) is applied
        // to the launch tag instead of leaving it unset. No-ops after the first.
        #[cfg(feature = "hooks")]
        {
            let tags = self.sel_monitor().map_or(0, crate::monitor::Monitor::tags);
            crate::features::hooks::startup(self, tags);
        }
    }

    /// Re-apply monitor rules (scale, transform, position) to all existing outputs.
    /// Called after a config reload so changes take effect without reconnecting.
    #[allow(clippy::cast_possible_truncation)]
    pub fn reapply_monitor_rules(&mut self) {
        for i in 0..self.monitors.len() {
            let output = self.monitors[i].output.clone();
            let rule = crate::monitor::matching_rule_for_output(&output);
            output.change_current_state(
                None,
                Some(rule.transform.into()),
                Some(smithay::output::Scale::Fractional(rule.scale)),
                None,
            );
            if rule.x >= 0 && rule.y >= 0 {
                let loc = smithay::utils::Point::from((rule.x, rule.y));
                self.space.map_output(&output, loc);
                output.change_current_state(None, None, None, Some(loc));
                if let Some(mon) = self.monitors.iter_mut().find(|m| m.output == output) {
                    mon.m.loc = loc;
                    mon.w.loc = loc;
                }
            }
            // Recompute logical size from current mode + new scale
            if let Some(mode) = output.current_mode() {
                let scale = output.current_scale().fractional_scale();
                let logical_w = (f64::from(mode.size.w) / scale) as i32;
                let logical_h = (f64::from(mode.size.h) / scale) as i32;
                if let Some(mon) = self.monitors.iter_mut().find(|m| m.output == output) {
                    mon.m.size = (logical_w, logical_h).into();
                }
            }
        }
        self.update_monitor_bounds();
    }

    /// Remove an output (monitor disconnected).
    pub fn output_removed(&mut self, output: &Output) {
        if let Some(idx) = self.monitor_for_output(output) {
            // Pick a migration target that is not the monitor being removed.
            // Prefer sel_mon; if sel_mon is the removed monitor, take any other.
            // When this is the only monitor there is no valid target — use 0 as a
            // placeholder so windows get mon_idx=0 which will be valid again once
            // a new monitor is plugged in and added as monitors[0].
            let target_idx = if self.monitors.len() <= 1 {
                0
            } else if self.sel_mon != idx {
                self.sel_mon
            } else {
                (0..self.monitors.len()).find(|&i| i != idx).unwrap_or(0)
            };

            let target_tags = self.monitors.get(target_idx).map_or(1, Monitor::tags);

            // Migrate windows that were on the removed monitor.
            for w in &self.windows {
                if crate::window::with_state(w, |s| s.mon_idx == idx).unwrap_or(false) {
                    // Preserve tags=0 for hidden scratchpads — overwriting them with
                    // target_tags would make the scratchpad unexpectedly appear on the
                    // target monitor.  Only reassign tags for normally-visible windows.
                    let current_tags = crate::window::with_state(w, |s| s.tags).unwrap_or(0);
                    if current_tags != 0 {
                        crate::window::set_window_tags(w, target_tags);
                    }
                    crate::window::with_state_mut(w, |s| s.mon_idx = target_idx);
                }
            }

            // monitors.remove(idx) shifts every entry after idx down by one.
            // Update all window mon_idx values so they stay consistent.
            for w in &self.windows {
                crate::window::with_state_mut(w, |s| {
                    if s.mon_idx > idx {
                        s.mon_idx -= 1;
                    }
                });
            }

            self.monitors.remove(idx);
            self.space.unmap_output(output);
            self.update_monitor_bounds();

            // Drop any gamma state keyed by the removed output. The per-resource
            // destroy handler only fires when a client tears down its gamma
            // control; an unplugged output would otherwise leave a stale ramp
            // (and, via the Arc-backed Output key, pin the Output alive).
            self.gamma_ramps.remove(output);
            self.gamma_controls.remove(output);

            if idx < self.tag_history.len() {
                self.tag_history.remove(idx);
            }

            // Keep sel_mon valid after the removal.
            if self.monitors.is_empty() {
                self.sel_mon = 0;
            } else if self.sel_mon > idx {
                self.sel_mon -= 1;
            } else if self.sel_mon >= self.monitors.len() {
                self.sel_mon = self.monitors.len() - 1;
            }
        }
        self.arrange_all();
    }
}
