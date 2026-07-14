//! Action dispatch — `Rwl::dispatch()` and the helper methods it calls.
//!
//! All methods here are `impl Rwl` blocks; they live in a separate file to keep
//! `state.rs` focused on compositor state rather than keybinding logic.

use smithay::desktop::Window;

use crate::config::Action;
use crate::monitor::Monitor;
use crate::state::Rwl;
use crate::window::{
    set_fullscreen, set_window_tags, window_is_floating, window_is_fullscreen,
    window_tags, window_visible_on, with_state, with_state_mut,
};

impl Rwl {
    pub fn zoom(&mut self) {
        let Some(focused) = self.focused_window().cloned() else {
            return;
        };
        if window_is_floating(&focused) || window_is_fullscreen(&focused) {
            return;
        }
        let Some(mon) = self.sel_monitor() else { return; };
        let tags = mon.tags();
        let sel_mon = self.sel_mon;

        // Mirror dwl C logic: iterate windows in tiling order.
        // - If the first visible tiled window IS focused (focused is master),
        //   clear `sel` and find the next tiled window (c) to promote instead.
        // - Otherwise `c` is the first non-focused tiled window found before
        //   we reach focused; sel stays Some(focused) → promote focused.
        //
        //  sel == None  after loop → focused was master; promote c
        //  sel == Some  after loop → focused was not master; promote focused
        // promote_c=true  → focused is the master; promote the next tiled window
        // promote_c=false → focused is not the master; promote focused itself
        let mut promote_c = false;
        let mut c: Option<Window> = None;
        for w in &self.windows {
            if window_visible_on(w, tags)
                && !window_is_floating(w)
                && with_state(w, |s| s.rules_applied && s.mon_idx == sel_mon).unwrap_or(false)
            {
                if w == &focused {
                    promote_c = true; // passed focused; look for the next tiled window
                } else {
                    c = Some(w.clone());
                    break;
                }
            }
        }

        // No second tiled window found → nothing to do
        let Some(c) = c else { return; };

        let to_promote = if promote_c { c } else { focused };
        let Some(pos) = self.windows.iter().position(|w| w == &to_promote) else { return; };
        // rotate_right(1) on the prefix slice moves element at `pos` to index 0
        // in a single O(n) pass, avoiding the double-shift of remove() + insert().
        self.windows[..=pos].rotate_right(1);
        // Mirror C dwl: focusclient(sel, 1) — always focus the promoted window.
        // Critical when focused was master: to_promote is a different window and
        // must receive keyboard focus; arrange_all ensures layout updates even when
        // focus did not change (focus_window only arranges on focus transitions).
        self.focus_window(Some(to_promote));
        self.arrange_all();
    }

    // -----------------------------------------------------------------------
    // Action dispatch
    // -----------------------------------------------------------------------

    #[allow(clippy::too_many_lines)]
    pub fn dispatch(&mut self, action: &Action) {
        // A keybind pressed while the overview is open exits it and runs the
        // action against the real layout. Exceptions kept inside the overview:
        // its own nav/toggles, and KillClient (closes the selected thumbnail's
        // window and stays open).
        #[cfg(feature = "overview")]
        if self.overview.is_some() {
            match action {
                Action::OverviewNav(_) | Action::ToggleOverview | Action::ToggleOverviewAll => {}
                Action::KillClient => {
                    crate::features::overview::kill_selected(self);
                    return;
                }
                _ => crate::features::overview::force_close(self),
            }
        }

        // Snapshot the selected monitor's tags so any tag-changing action fires
        // on_tag_switch generically (no per-action wiring).
        #[cfg(any(feature = "hooks", feature = "ipc"))]
        let tag_before = (self.sel_mon, self.sel_monitor().map(crate::monitor::Monitor::tags));
        // Likewise snapshot the active layout index so any layout-changing action
        // (or a per-tag layout that changes with the tag) fires on_layout_change.
        #[cfg(feature = "hooks")]
        let layout_before = (self.sel_mon, self.sel_monitor().map(crate::monitor::Monitor::layout_idx));

        match action {
            Action::Spawn(cmd) => self.spawn(cmd),
            Action::FocusStack(dir) => self.focus_stack_step(*dir),
            Action::IncNmaster(delta) => self.inc_nmaster(*delta),
            Action::SetMfact(delta) => self.set_mfact(*delta),
            Action::Zoom => self.zoom(),
            Action::ViewPrev => {
                let mon_idx = self.sel_mon;
                #[cfg(feature = "tag-transition")]
                let (old_tag, prev_tag, output_w) = {
                    let m = self.monitors.get(mon_idx);
                    (
                        m.map_or(0, Monitor::tags),
                        m.map_or(0, |m| m.tagset[m.sel_tags ^ 1]),
                        m.map_or(1920.0, |m| f64::from(m.w.size.w)),
                    )
                };
                // Record the "other" tagset slot before swapping so the close-fallback
                // can find it even after several ViewPrev cycles without explicit View calls.
                if let Some(after) = self.monitors.get(mon_idx).map(|m| m.tagset[m.sel_tags ^ 1]) {
                    self.update_tag_history(mon_idx, after);
                }
                if let Some(m) = self.sel_monitor_mut() {
                    m.view_prev();
                }
                #[cfg(feature = "tag-transition")]
                crate::features::tag_transition::mark_slide_outs(&self.windows, old_tag, prev_tag, output_w);
                self.arrange_all();
                #[cfg(feature = "tag-transition")]
                crate::features::tag_transition::start_tag_transition(
                    &self.windows, old_tag, prev_tag, output_w,
                );
                let top = self.focused_window().cloned();
                self.focus_window(top);
            }
            Action::ViewNextOccTag(dir) => {
                if let Some(mask) = self.next_occ_tag(*dir) {
                    let mon_idx = self.sel_mon;
                    #[cfg(feature = "tag-transition")]
                    let (old_tag, output_w) = {
                        let m = self.monitors.get(mon_idx);
                        (m.map_or(0, Monitor::tags), m.map_or(1920.0, |m| f64::from(m.w.size.w)))
                    };
                    self.update_tag_history(mon_idx, mask);
                    if let Some(m) = self.sel_monitor_mut() {
                        m.view(mask);
                    }
                    #[cfg(feature = "tag-transition")]
                    crate::features::tag_transition::mark_slide_outs(&self.windows, old_tag, mask, output_w);
                    self.arrange_all();
                    #[cfg(feature = "tag-transition")]
                    crate::features::tag_transition::start_tag_transition(
                        &self.windows, old_tag, mask, output_w,
                    );
                    let top = self.focused_window().cloned();
                    self.focus_window(top);
                }
            }
            Action::KillClient => self.kill_focused(),
            Action::SetLayout(idx) => {
                if let Some(m) = self.sel_monitor_mut() {
                    m.set_layout(*idx);
                }
                self.arrange_all();
            }
            Action::CycleLayout => {
                let next_idx = {
                    let n = crate::config::get().layouts.len();
                    let cur = self.sel_monitor().map_or(0, |m| m.lt[m.sel_lt]);
                    (n > 0).then(|| (cur + 1) % n)
                };
                if let (Some(idx), Some(m)) = (next_idx, self.sel_monitor_mut()) {
                    m.set_layout(idx);
                }
                self.arrange_all();
            }
            #[cfg(feature = "pertag-layouts")]
            Action::ResetLayout => {
                if let Some(m) = self.sel_monitor_mut() {
                    let tags = m.tags();
                    crate::features::pertag_layouts::reset(m, tags);
                }
                self.arrange_all();
            }
            Action::ToggleFloating => self.toggle_focused_floating(),
            Action::ToggleFullscreen => self.toggle_focused_fullscreen(),
            Action::TogglePassthrough => {
                self.passthrough = !self.passthrough;
                tracing::info!("Passthrough: {}", self.passthrough);
            }
            #[cfg(feature = "gaps")]
            Action::ToggleGaps => {
                let on = crate::features::gaps::toggle_gaps();
                tracing::info!("Gaps: {}", if on { "on" } else { "off" });
                self.arrange_all();
            }
            Action::View(mask) => {
                let mask = *mask;
                let mon_idx = self.sel_mon;
                #[cfg(feature = "tag-transition")]
                let (old_tag, output_w) = {
                    let m = self.monitors.get(mon_idx);
                    (m.map_or(0, Monitor::tags), m.map_or(1920.0, |m| f64::from(m.w.size.w)))
                };
                self.update_tag_history(mon_idx, mask);
                if let Some(m) = self.sel_monitor_mut() {
                    m.view(mask);
                }
                #[cfg(feature = "tag-transition")]
                crate::features::tag_transition::mark_slide_outs(&self.windows, old_tag, mask, output_w);
                self.arrange_all();
                #[cfg(feature = "tag-transition")]
                crate::features::tag_transition::start_tag_transition(
                    &self.windows, old_tag, mask, output_w,
                );
                let top = self.focused_window().cloned();
                self.focus_window(top);
            }
            Action::ViewTagSpawn(mask) => self.view_tag_spawn(*mask),
            Action::ToggleView(mask) => {
                let mask = *mask;
                let mon_idx = self.sel_mon;
                // Compute target before the change so update_tag_history sees the right old value
                let new_tags = self.monitors.get(mon_idx)
                    .map_or(0, |m| (m.tags() ^ mask) & crate::config::tag_mask());
                if new_tags != 0 {
                    self.update_tag_history(mon_idx, new_tags);
                }
                if let Some(m) = self.sel_monitor_mut() {
                    m.toggle_view(mask);
                }
                self.arrange_all();
                let top = self.focused_window().cloned();
                self.focus_window(top);
            }
            Action::Tag(mask) => {
                let mask = (*mask) & crate::config::tag_mask();
                if mask != 0 {
                    if let Some(w) = self.focused_window().cloned() {
                        set_window_tags(&w, mask);
                        let follow = crate::config::get().follow;
                        if follow {
                            // Record the tag we're leaving so ViewPrev and
                            // auto-back-empty-tag can navigate back correctly —
                            // matching every other tag-changing action.
                            let mon_idx = self.sel_mon;
                            self.update_tag_history(mon_idx, mask);
                            if let Some(m) = self.sel_monitor_mut() {
                                m.view(mask);
                            }
                        }
                    }
                    self.arrange_all();
                    let top = self.focused_window().cloned();
                    self.focus_window(top);
                }
            }
            Action::ToggleTag(mask) => {
                let mask = *mask;
                if let Some(w) = self.focused_window().cloned() {
                    let cur = window_tags(&w);
                    let new = (cur ^ mask) & crate::config::tag_mask();
                    if new != 0 {
                        set_window_tags(&w, new);
                    }
                }
                self.arrange_all();
            }
            Action::FocusMon(dir) => self.focus_mon(*dir),
            Action::TagMon(dir) => self.tag_mon(*dir),
            #[cfg(feature = "scratchpad")]
            Action::ToggleScratch(cmd) => self.toggle_scratch(cmd),
            #[cfg(feature = "scratchpad")]
            Action::FocusOrToggleScratch(cmd) => self.focus_or_toggle_scratch(cmd),
            #[cfg(feature = "scratchpad")]
            Action::FocusOrToggleMatchingScratch(cmd) => {
                self.focus_or_toggle_matching_scratch(cmd);
            }
            Action::Chvt(vt) => self.chvt(*vt),
            Action::ReloadConfig => {
                crate::config::reload();
                // Clear cursor cache so the new theme/size takes effect on the
                // next frame. Children spawned after reload pick up the new
                // cursor env via `configure_child` (no process-env mutation).
                self.cursor_cache.clear();
                self.reapply_monitor_rules();
                self.reapply_keyboard_config();
                self.reapply_input_device_config();
                #[cfg(feature = "pertag-layouts")]
                for mon in &mut self.monitors {
                    let tags = mon.tagset[mon.sel_tags];
                    crate::features::pertag_layouts::apply(mon, tags);
                }
                self.update_borders();
                self.arrange_all();
                #[cfg(feature = "bar")]
                crate::features::bar::send_bar_command("all reload");
            }
            // Move and Resize require the window under the cursor and are
            // handled directly in process_pointer_button where that window is
            // already known.  Reaching here means they were dispatched from a
            // key binding (unusual) — no-op is safe.
            Action::Move | Action::Resize => {}
            Action::Quit => {
                self.running = false;
                self.loop_signal.stop();
            }
            #[cfg(feature = "bar")]
            Action::ToggleBar => {
                crate::features::bar::send_bar_command("all toggle-visibility");
                #[cfg(feature = "gaps")]
                crate::features::gaps::toggle_gaps();
                self.arrange_all();
            }
            #[cfg(feature = "bar")]
            Action::BarPrompt => {
                crate::features::bar::send_bar_command("selected prompt");
            }
            #[cfg(feature = "overview")]
            Action::ToggleOverview => crate::features::overview::toggle(self, false),
            #[cfg(feature = "overview")]
            Action::ToggleOverviewAll => crate::features::overview::toggle(self, true),
            #[cfg(feature = "overview")]
            Action::OverviewNav(keysym) => crate::features::overview::handle_key(self, *keysym),
            #[cfg(feature = "pip")]
            Action::TogglePip => crate::features::pip::toggle(self),
            #[cfg(feature = "pip")]
            Action::MovePip => crate::features::pip::move_corner(self),
            #[cfg(feature = "lock")]
            Action::Lock => crate::features::lock::lock(self),
            // Keystroke already consumed into the locker's buffer; nothing to do.
            #[cfg(feature = "lock")]
            Action::LockConsume => {}
        }

        // Fire on_tag_switch if the selected monitor's tags changed (and we did
        // not switch monitors, which would compare unrelated tag sets).
        #[cfg(feature = "hooks")]
        {
            let (mon_before, old_tags) = tag_before;
            if mon_before == self.sel_mon
                && let (Some(old), Some(new)) =
                    (old_tags, self.sel_monitor().map(crate::monitor::Monitor::tags))
                && old != new
            {
                crate::features::hooks::tag_switch(self, old, new);
            }
        }

        // Emit a structured `tag` event if the selected monitor's tags changed.
        #[cfg(feature = "ipc")]
        {
            let (mon_before, old_tags) = tag_before;
            if mon_before == self.sel_mon
                && let (Some(old), Some(new)) =
                    (old_tags, self.sel_monitor().map(crate::monitor::Monitor::tags))
                && old != new
            {
                crate::features::ipc::event::tag(self, mon_before);
            }
        }

        // Fire on_layout_change if the selected monitor's active layout changed
        // (and we did not switch monitors).
        #[cfg(feature = "hooks")]
        {
            let (mon_before, old_lt) = layout_before;
            if mon_before == self.sel_mon
                && let (Some(old), Some(new)) =
                    (old_lt, self.sel_monitor().map(crate::monitor::Monitor::layout_idx))
                && old != new
            {
                crate::features::hooks::layout_change(self, old, new);
            }
        }
    }

    /// Apply the standard child-process environment to `command`: the Wayland
    /// socket, desktop identity, cursor theme/size, and IPC socket, while
    /// stripping any inherited `DISPLAY` so children don't connect to a parent
    /// X server. Replaces the historical global `set_var`/`remove_var` calls,
    /// which are unsound off the main thread (and `unsafe` under edition 2024).
    ///
    /// Must not be called while a [`crate::config::get`] read guard is held on
    /// this thread — it acquires one internally and `RwLock` reads are not
    /// reentrant-safe on all platforms.
    pub(crate) fn configure_child(&self, command: &mut std::process::Command) {
        // Point X11 clients at XWayland when it is running; otherwise strip any
        // inherited DISPLAY so pure-Wayland apps don't accidentally use X11.
        #[cfg(feature = "xwayland")]
        match self.xdisplay {
            Some(n) => { command.env("DISPLAY", format!(":{n}")); }
            None => { command.env_remove("DISPLAY"); }
        }
        #[cfg(not(feature = "xwayland"))]
        command.env_remove("DISPLAY");
        command.env("WAYLAND_DISPLAY", &self.socket_name);
        command.env("XDG_SESSION_TYPE", "wayland");
        command.env("XDG_CURRENT_DESKTOP", "rwl");
        {
            let cfg = crate::config::get();
            if let Some(ref theme) = cfg.cursor_theme {
                command.env("XCURSOR_THEME", theme);
            }
            if cfg.cursor_size > 0 {
                command.env("XCURSOR_SIZE", cfg.cursor_size.to_string());
            }
        }
        #[cfg(feature = "ipc")]
        if let Some(ref path) = self.rwl_sock {
            command.env("RWL_SOCK", path);
        }
    }

    pub(crate) fn spawn(&self, cmd: &[String]) {
        use std::os::unix::process::CommandExt as _;

        if cmd.is_empty() {
            return;
        }
        #[cfg(feature = "winit")]
        let is_winit = matches!(self.backend, Some(crate::backend::BackendData::Winit(_)));
        #[cfg(not(feature = "winit"))]
        let is_winit = false;
        let mut command = std::process::Command::new(&cmd[0]);
        command.args(&cmd[1..]);
        self.configure_child(&mut command);
        // In nested (winit) mode there is no dmabuf support, so Qt apps cannot
        // create a hardware-backed EGL context and emit noisy QRhi warnings.
        // Force them to use the software Quick backend so the warnings are gone.
        if is_winit {
            command.env("QT_QUICK_BACKEND", "software");
        }
        // Place the child in its own process group so signals sent to the
        // compositor's process group don't reach spawned programs.
        command.process_group(0);
        match command.spawn() {
            Ok(_) => {}
            Err(e) => tracing::warn!("spawn failed: {}", e),
        }
    }

    /// Focus the first urgent window, switching monitor and tags to reach it.
    /// Mirrors dwl.c `focusurgent()`.
    pub fn focus_urgent(&mut self) {
        // Find the first urgent window (search focus_stack first so the most
        // recently active urgent client wins, then fall back to windows list).
        let urgent = self
            .focus_stack
            .iter()
            .find(|w| with_state(w, |s| s.is_urgent).unwrap_or(false))
            .cloned()
            .or_else(|| {
                self.windows
                    .iter()
                    .find(|w| with_state(w, |s| s.is_urgent).unwrap_or(false))
                    .cloned()
            });

        let Some(w) = urgent else { return };

        let (mon_idx, tags) = with_state(&w, |s| (s.mon_idx, s.tags))
            .unwrap_or((self.sel_mon, 0));

        // Switch to the monitor that owns the urgent window.
        self.sel_mon = mon_idx;

        // Switch that monitor's tagset to the window's tags so it becomes visible.
        if tags != 0
            && let Some(m) = self.monitors.get_mut(mon_idx)
        {
            m.view(tags);
        }

        self.arrange_all();
        self.focus_window(Some(w));
    }

    fn focus_stack_step(&mut self, dir: i32) {
        let tags = self.sel_monitor().map_or(0, Monitor::tags);
        let sel_mon = self.sel_mon;
        let focused = self.focused_window().cloned();

        // Collect indices (usize) instead of cloning Window Arc handles.
        let on_mon: Vec<usize> = self.windows.iter().enumerate()
            .filter_map(|(i, w)| {
                with_state(w, |s| s.tags & tags != 0 && s.rules_applied && s.mon_idx == sel_mon)
                    .unwrap_or(false)
                    .then_some(i)
            })
            .collect();

        let count = on_mon.len();
        if count == 0 {
            return;
        }

        let cur_pos = focused.as_ref().and_then(|f| {
            on_mon.iter().position(|&i| &self.windows[i] == f)
        });
        let next = match cur_pos {
            Some(pos) if dir > 0 => (pos + 1) % count,
            Some(pos) => (pos + count - 1) % count,
            None if dir > 0 => 0,
            None => count - 1,
        };

        let next_window = self.windows.get(on_mon[next]).cloned();
        self.focus_window(next_window);
    }

    fn inc_nmaster(&mut self, delta: i32) {
        if let Some(m) = self.sel_monitor_mut() {
            m.nmaster = (m.nmaster + delta).max(0);
        }
        self.arrange_all();
    }

    fn set_mfact(&mut self, delta: f64) {
        if let Some(m) = self.sel_monitor_mut() {
            m.mfact = (m.mfact + delta).clamp(0.05, 0.95);
        }
        self.arrange_all();
    }

    fn kill_focused(&self) {
        if let Some(w) = self.focused_window().cloned() {
            #[cfg(feature = "fade")]
            with_state_mut(&w, |s| {
                let fade_out_ms = crate::config::get().fade_out_ms;
                if fade_out_ms > 0 && !s.fading_out {
                    s.fading_out = true;
                    s.fade_start = Some(std::time::Instant::now());
                }
            });
            if let Some(tl) = w.toplevel() {
                tl.send_close();
            }
        }
    }

    fn toggle_focused_floating(&mut self) {
        if let Some(w) = self.focused_window().cloned() {
            let was_floating = window_is_floating(&w);
            crate::window::toggle_floating(&w);
            if !was_floating {
                with_state_mut(&w, |s| s.needs_centering = true);
            }
            self.arrange_all();
        }
    }

    fn toggle_focused_fullscreen(&mut self) {
        if let Some(w) = self.focused_window().cloned() {
            let new_fs = !window_is_fullscreen(&w);
            set_fullscreen(&w, new_fs);
            // arrange_all() sends the correct configure (with/without
            // Fullscreen state and the right size) in one shot.
            self.arrange_all();
            #[cfg(feature = "hooks")]
            crate::features::hooks::fullscreen(self, &w, new_fs);
        }
    }

    fn view_tag_spawn(&mut self, mask: u32) {
        let mask = mask & crate::config::tag_mask();
        let mon_idx = self.sel_mon;
        #[cfg(feature = "tag-transition")]
        let (old_tag, output_w) = {
            let m = self.monitors.get(mon_idx);
            (m.map_or(0, Monitor::tags), m.map_or(1920.0, |m| f64::from(m.w.size.w)))
        };
        self.update_tag_history(mon_idx, mask);
        if let Some(m) = self.sel_monitor_mut() {
            m.view(mask);
        }
        let tiled = self.count_tiled(mask);
        if tiled == 0 {
            let tag_idx = mask.trailing_zeros() as usize;
            if let Some(Some(cmd)) = crate::config::get().auto_spawn.get(tag_idx) {
                self.spawn(cmd);
            }
        }
        #[cfg(feature = "tag-transition")]
        crate::features::tag_transition::mark_slide_outs(&self.windows, old_tag, mask, output_w);
        self.arrange_all();
        #[cfg(feature = "tag-transition")]
        crate::features::tag_transition::start_tag_transition(
            &self.windows, old_tag, mask, output_w,
        );
        let top = self.focused_window().cloned();
        self.focus_window(top);
    }

    fn focus_mon(&mut self, dir: i32) {
        if let Some(next) = self.dir_to_monitor(self.sel_mon, dir) {
            self.sel_mon = next;
            let tags = self
                .monitors
                .get(next)
                .map_or(0, Monitor::tags);
            let top = self
                .focus_stack
                .iter()
                .find(|w| {
                    window_visible_on(w, tags)
                        && with_state(w, |s| s.mon_idx == next).unwrap_or(false)
                })
                .cloned();
            self.focus_window(top);
            self.arrange_all();
        }
    }

    fn tag_mon(&mut self, dir: i32) {
        let Some(focused) = self.focused_window().cloned() else {
            return;
        };
        let Some(next) = self.dir_to_monitor(self.sel_mon, dir) else {
            return;
        };
        let new_tags = self.monitors.get(next).map_or(1, Monitor::tags);
        set_window_tags(&focused, new_tags);
        with_state_mut(&focused, |s| s.mon_idx = next);
        if crate::config::get().follow {
            self.sel_mon = next;
            self.focus_window(Some(focused));
        }
        self.arrange_all();
    }

    // -----------------------------------------------------------------------
    // VT switching
    // -----------------------------------------------------------------------

    #[allow(clippy::cast_possible_wrap)]
    fn chvt(&mut self, vt: u32) {
        if let Some(crate::backend::BackendData::Udev(ref mut backend)) = self.backend {
            use smithay::backend::session::Session as _;
            if let Err(e) = backend.session.change_vt(vt as i32) {
                tracing::error!("VT switch to {} failed: {}", vt, e);
            }
        }
    }
}
