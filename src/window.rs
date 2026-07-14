//! Per-window metadata stored in smithay's `Window::user_data()`.

use std::cell::RefCell;

use smithay::backend::renderer::element::Id;
use smithay::backend::renderer::utils::CommitCounter;
use smithay::desktop::Window;
use smithay::utils::{Logical, Point, Rectangle};

use crate::config::Color;

// ---------------------------------------------------------------------------
// Per-window state
// ---------------------------------------------------------------------------

/// Mutable metadata attached to every mapped window.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug)]
pub struct WindowState {
    /// Tag bitmask this window belongs to.
    pub tags: u32,
    /// Whether the window is floating.
    pub is_floating: bool,
    /// Whether the window is fullscreen.
    pub is_fullscreen: bool,
    /// Whether the window has an urgent hint.
    pub is_urgent: bool,
    /// Scratchpad identifier key (`'\0'` = not a scratchpad).
    pub scratch_key: char,
    /// Saved tag bitmask to return to when the window is hidden (switch-to-tag).
    pub switch_to_tag: Option<u32>,
    /// Border width in pixels.
    #[allow(dead_code)]
    pub bw: u32,
    /// Current border colour — set by `update_borders()` each time focus changes.
    pub border_color: Color,
    /// Stable per-strip render IDs (top/bottom/left/right).  Keeping these
    /// constant across frames lets the damage tracker skip unchanged borders.
    pub border_ids: [Id; 4],
    /// Bumped whenever geometry or colour changes so the damage tracker
    /// re-renders only when something actually changed.
    pub border_counter: CommitCounter,
    /// `false` until window rules have been applied on the first surface commit.
    /// Rules are deferred because `app_id`/`title` are only available after
    /// `xdg_toplevel.set_app_id` / `set_title` are processed, which happens
    /// after `new_toplevel` fires but before the client's first commit.
    pub rules_applied: bool,
    /// Set when `apply_rules` marks the window as floating for the first time.
    /// Cleared once the window is centered on the monitor after its first
    /// buffer commit (when the actual surface size is known).
    pub needs_centering: bool,
    /// Saved position for floating windows — preserved when the window's tag
    /// is hidden so it can be restored when the tag becomes visible again.
    pub saved_loc: Option<Point<i32, Logical>>,
    /// Index into `Rwl::monitors` for the monitor this window is assigned to.
    pub mon_idx: usize,
    /// Per-window size factor along the current layout's stacking axis (dwm's
    /// `cfact`).  `1.0` is an equal share; interactive mouse resize in tiled
    /// layouts grows/shrinks this window relative to its column/row neighbours.
    /// Clamped to `[0.25, 4.0]`.
    pub cfact: f64,
    /// Last title seen at commit time; used to detect changes and push updates
    /// to bars (dwlb, sombar, …) without waiting for an unrelated event.
    pub last_title: Option<String>,
    /// Last `app_id` seen; cached so `format_status()` avoids re-locking Wayland
    /// surface state on every IPC print.
    pub last_appid: Option<String>,
    /// Geometry most recently sent to the client in a configure (set by
    /// `arrange()`).  Borders are drawn at this geometry rather than the
    /// committed-buffer geometry so they track the intended layout immediately,
    /// avoiding a one-frame flash at the wrong position between the configure
    /// and the client's next commit.  `None` for floating windows (borders use
    /// `space.element_geometry()` instead).
    pub pending_geom: Option<Rectangle<i32, Logical>>,
    /// `false` until the client first attaches a buffer.  mpv (and possibly
    /// other clients) discard the state array in `xdg_toplevel.configure` events
    /// received before their VO/renderer is fully reconfigured, which leaves
    /// our initial Activated state un-applied.  On the first buffer commit we
    /// re-emit a fresh configure so the post-init parser sees Activated.
    pub buffer_mapped: bool,
    /// Whether this window held keyboard focus on the last `arrange()` call.
    /// Used to detect focus-change events and bump `border_counter` only when
    /// the focused/unfocused state actually changed (avoids spurious redraws).
    pub last_focused: bool,
    /// Current opacity used when rendering this window (0.0 = invisible, 1.0 = opaque).
    /// Driven by `advance_fades()` each frame during enter/exit transitions.
    #[cfg(feature = "fade")]
    pub fade_alpha: f32,
    /// Timestamp of when the current fade started.  `None` = not fading.
    #[cfg(feature = "fade")]
    pub fade_start: Option<std::time::Instant>,
    /// `true` while a fade-out is in progress (set by `kill_focused()`).
    #[cfg(feature = "fade")]
    pub fading_out: bool,
    /// Stable render-element ID for the per-frame fade-damage sentinel element.
    /// Keeping a fixed ID lets the damage tracker detect counter changes.
    #[cfg(feature = "fade")]
    pub fade_damage_id: Id,
    /// Bumped every frame while `fade_start.is_some()` so the damage tracker
    /// marks the window region as dirty even when the surface has no new commit.
    #[cfg(feature = "fade")]
    pub fade_damage_counter: CommitCounter,
    /// Current horizontal render offset in logical pixels (0 = at rest).
    /// Driven by `advance_tag_transitions()` each frame while the window slides in.
    #[cfg(feature = "tag-transition")]
    pub slide_offset_x: f64,
    /// Starting horizontal offset set when the animation begins.
    #[cfg(feature = "tag-transition")]
    pub slide_from_x: f64,
    /// Timestamp of when the current slide started.  `None` = not sliding.
    #[cfg(feature = "tag-transition")]
    pub slide_start: Option<std::time::Instant>,
    /// Stable render-element ID for the per-frame slide-damage sentinel element.
    #[cfg(feature = "tag-transition")]
    pub slide_damage_id: Id,
    /// Bumped every frame while `slide_start.is_some()` so the damage tracker
    /// marks the window region dirty even when the surface has no new commit.
    #[cfg(feature = "tag-transition")]
    pub slide_damage_counter: CommitCounter,
    /// Destination horizontal offset: 0.0 for slide-in (arrives on-screen),
    /// ±`output_w` for slide-out (leaves off-screen). Driven by `advance_tag_transitions`.
    #[cfg(feature = "tag-transition")]
    pub slide_to_x: f64,
    /// True while the window is sliding out and must stay mapped in the space.
    /// Cleared by `finalize_slide_outs()` after the animation completes.
    #[cfg(feature = "tag-transition")]
    pub slide_out: bool,
    /// `true` when this window is a terminal hidden because a child swallowed it
    /// (see [`crate::features::swallow`]). Excluded from the layout and render
    /// until restored.
    #[cfg(feature = "swallow")]
    pub is_swallowed: bool,
    /// The terminal this window is swallowing, if any. Restored when this window
    /// (the child) is unmapped.
    #[cfg(feature = "swallow")]
    pub swallowing: Option<Window>,
}

impl WindowState {
    /// Create a default [`WindowState`] for a newly mapped window.
    #[must_use]
    pub fn new() -> Self {
        let cfg = crate::config::get();
        Self {
            tags: 0,
            is_floating: false,
            is_fullscreen: false,
            is_urgent: false,
            scratch_key: '\0',
            switch_to_tag: None,
            bw: cfg.border_px,
            border_color: cfg.border_color,
            border_ids: std::array::from_fn(|_| Id::new()),
            border_counter: CommitCounter::default(),
            rules_applied: false,
            needs_centering: false,
            saved_loc: None,
            mon_idx: 0,
            cfact: 1.0,
            last_title: None,
            last_appid: None,
            pending_geom: None,
            buffer_mapped: false,
            last_focused: false,
            #[cfg(feature = "fade")]
            fade_alpha: 1.0,
            #[cfg(feature = "fade")]
            fade_start: None,
            #[cfg(feature = "fade")]
            fading_out: false,
            #[cfg(feature = "fade")]
            fade_damage_id: Id::new(),
            #[cfg(feature = "fade")]
            fade_damage_counter: CommitCounter::default(),
            #[cfg(feature = "tag-transition")]
            slide_offset_x: 0.0,
            #[cfg(feature = "tag-transition")]
            slide_from_x: 0.0,
            #[cfg(feature = "tag-transition")]
            slide_start: None,
            #[cfg(feature = "tag-transition")]
            slide_damage_id: Id::new(),
            #[cfg(feature = "tag-transition")]
            slide_damage_counter: CommitCounter::default(),
            #[cfg(feature = "tag-transition")]
            slide_to_x: 0.0,
            #[cfg(feature = "tag-transition")]
            slide_out: false,
            #[cfg(feature = "swallow")]
            is_swallowed: false,
            #[cfg(feature = "swallow")]
            swallowing: None,
        }
    }
}

impl Default for WindowState {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Helper accessors
// ---------------------------------------------------------------------------

/// Initialise [`WindowState`] in the window's user-data map if not already
/// present, then call `f` with a mutable borrow of it.
///
/// Returns `false` if the borrow could not be acquired (should never happen
/// in single-threaded use).
#[inline]
pub fn with_state_mut<F>(window: &Window, f: F) -> bool
where
    F: FnOnce(&mut WindowState),
{
    window
        .user_data()
        .insert_if_missing(|| RefCell::new(WindowState::new()));
    window
        .user_data()
        .get::<RefCell<WindowState>>()
        .and_then(|cell| cell.try_borrow_mut().ok())
        .map(|mut guard| f(&mut guard))
        .is_some()
}

/// Call `f` with an immutable borrow of the [`WindowState`].
///
/// Returns `None` if there is no state attached yet or the borrow fails.
#[inline]
#[must_use]
pub fn with_state<F, R>(window: &Window, f: F) -> Option<R>
where
    F: FnOnce(&WindowState) -> R,
{
    let cell = window.user_data().get::<RefCell<WindowState>>()?;
    let guard = cell.try_borrow().ok()?;
    Some(f(&guard))
}

/// Return the tags bitmask for `window`, or `0` if no state is attached.
#[inline]
#[must_use]
pub fn window_tags(window: &Window) -> u32 {
    with_state(window, |s| s.tags).unwrap_or(0)
}

/// Return `true` if `window` is floating.
#[inline]
#[must_use]
pub fn window_is_floating(window: &Window) -> bool {
    with_state(window, |s| s.is_floating).unwrap_or(false)
}

/// Return `true` if `window` is fullscreen.
#[inline]
#[must_use]
pub fn window_is_fullscreen(window: &Window) -> bool {
    with_state(window, |s| s.is_fullscreen).unwrap_or(false)
}

/// Return `true` if `window` is a scratchpad with the given key.
#[cfg(feature = "scratchpad")]
#[inline]
#[must_use]
pub fn window_is_scratch(window: &Window, key: char) -> bool {
    with_state(window, |s| s.scratch_key == key).unwrap_or(false)
}

/// Return `true` if `window` is any scratchpad (`scratch_key` != `'\0'`).
#[cfg(feature = "scratchpad")]
#[inline]
#[allow(dead_code)]
#[must_use]
pub fn window_is_any_scratch(window: &Window) -> bool {
    with_state(window, |s| s.scratch_key != '\0').unwrap_or(false)
}

/// Return `true` if `window` is visible on the given tag bitmask.
#[inline]
#[must_use]
pub fn window_visible_on(window: &Window, tagset: u32) -> bool {
    window_tags(window) & tagset != 0
}

/// Set the tags of a window.
#[inline]
pub fn set_window_tags(window: &Window, tags: u32) {
    with_state_mut(window, |s| s.tags = tags);
}

/// Toggle floating state of a window.
#[inline]
pub fn toggle_floating(window: &Window) {
    with_state_mut(window, |s| s.is_floating = !s.is_floating);
}

/// Set fullscreen state of a window.
#[inline]
pub fn set_fullscreen(window: &Window, fs: bool) {
    with_state_mut(window, |s| s.is_fullscreen = fs);
}

/// Set urgent hint on a window.
#[inline]
pub fn set_urgent(window: &Window, urgent: bool) {
    with_state_mut(window, |s| s.is_urgent = urgent);
}
