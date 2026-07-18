//! Workspace overview / Exposé.
//!
//! Shrinks every window into a live, animated grid of thumbnails so the user
//! can see all windows at once and jump to any of them by typing a letter hint
//! or clicking. Two modes:
//!  - current-tag Exposé (`ToggleOverview`): windows on the focused monitor's
//!    active tags.
//!  - all-tags mission control (`ToggleOverviewAll`): every mapped window,
//!    selecting one switches to its tag.
//!
//! Compiled only when the `overview` Cargo feature is enabled.
//!
//! Rendering reuses smithay's [`constrain_space_element`], which scales,
//! relocates and crops a window's live render elements into a target rectangle
//! — exactly a thumbnail. The zoom animation interpolates that rectangle
//! between the window's real on-screen geometry and its grid cell.

use std::time::Instant;

use smithay::backend::renderer::element::solid::SolidColorRenderElement;
use smithay::backend::renderer::element::{Id, Kind};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::utils::CommitCounter;
use smithay::desktop::space::{
    constrain_space_element, ConstrainBehavior, ConstrainReference,
};
use smithay::backend::renderer::element::utils::{ConstrainAlign, ConstrainScaleBehavior};
use smithay::desktop::Window;
use smithay::output::Output;
use smithay::utils::{Logical, Physical, Point, Rectangle, Size};

use crate::config::{hex_color, Color};
use crate::render::RwlRenderElement;
use crate::state::Rwl;
use crate::window::{window_tags, window_visible_on, with_state};

mod font;

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

/// User-tunable overview appearance, loaded from the `overview` Lua table.
/// The Lua parser lives in `config::settings`; this owns the shape + defaults.
#[derive(Debug, Clone)]
pub struct OverviewSettings {
    /// Characters used for window hint labels, in priority order. Limited to
    /// distinct `a`–`z` and `0`–`9` (the embedded [`font`] covers no other
    /// glyphs); the config loader lowercases and filters this to that set.
    pub hint_keys: String,
    /// Full-output backdrop colour drawn behind the thumbnails (dims the desktop).
    pub dim: Color,
    /// Padding in logical pixels between the grid edge and the work area, and
    /// between individual thumbnails.
    pub padding: u32,
    /// Open/close zoom animation duration in milliseconds (0 = instant).
    pub anim_ms: u32,
    /// Highlight ring colour drawn around the hovered / matched thumbnail.
    pub highlight_color: Color,
    /// Highlight ring thickness in logical pixels.
    pub highlight_px: u32,
    /// Border colour drawn around every *non*-highlighted thumbnail.
    pub border_color: Color,
    /// Border thickness for non-highlighted thumbnails (0 = none). Usually
    /// thinner than `highlight_px`.
    pub border_px: u32,
}

impl Default for OverviewSettings {
    fn default() -> Self {
        Self {
            hint_keys:       "asdfghjkl".into(),
            dim:             [0.0, 0.0, 0.0, 0.6],
            padding:         40,
            anim_ms:         180,
            highlight_color: hex_color(0xffff_ffff),
            highlight_px:    3,
            border_color:    hex_color(0x8888_88aa),
            border_px:       1,
        }
    }
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// One thumbnail in the overview grid.
struct OverviewCell {
    window: Window,
    /// Window's real on-screen geometry (animation start / end for close).
    source: Rectangle<i32, Logical>,
    /// Grid-cell geometry the thumbnail settles into (animation end for open).
    target: Rectangle<i32, Logical>,
    /// Hint characters typed to select this window (e.g. "a", "s").
    hint: String,
    /// Tag mask to switch to when this cell is selected (all-tags mode).
    tag: u32,
    /// Stable damage ids for the 4 highlight-ring strips.
    ring_ids: [Id; 4],
}

/// What a closing animation is heading toward.
enum Selection {
    /// Fly back and focus this window (switching to `tag` first if needed).
    Window(Window, u32),
    /// Just dismiss the overview, focus unchanged.
    Cancel,
}

/// An active overview session.
pub struct OverviewState {
    cells: Vec<OverviewCell>,
    /// Output the overview is drawn on (the focused monitor's output).
    output: Output,
    /// Hint characters typed so far.
    typed: String,
    /// Index of the cell currently under the pointer, if any.
    hovered: Option<usize>,
    /// Index of the arrow-key selected cell (the keyboard cursor).
    selected: Option<usize>,
    /// Whether this session shows all tags (mission control) — needed to rebuild
    /// the grid when a window is closed while the overview is open.
    all_tags: bool,
    /// When the current (open or close) animation started.
    anim_start: Instant,
    /// `Some` once the overview is closing; carries the pending selection.
    closing: Option<Selection>,
    /// Stable damage id for the full-screen backdrop.
    backdrop_id: Id,
    /// Bumped every animation frame so moving/fading solid elements (backdrop,
    /// rings) are always re-damaged even though their `Id`s stay stable.
    frame_counter: CommitCounter,
}

impl OverviewState {
    /// Whether this overview session is being displayed on `output`.
    pub fn on_output(&self, output: &Output) -> bool {
        &self.output == output
    }
}

#[allow(clippy::missing_fields_in_debug)]
impl std::fmt::Debug for OverviewState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OverviewState")
            .field("cells", &self.cells.len())
            .field("typed", &self.typed)
            .field("hovered", &self.hovered)
            .field("closing", &self.closing.is_some())
            .finish()
    }
}

// ---------------------------------------------------------------------------
// Toggle / open / close
// ---------------------------------------------------------------------------

/// Open the overview (or begin closing it if already open).
pub fn toggle(state: &mut Rwl, all_tags: bool) {
    if state.overview.is_some() {
        begin_close(state, Selection::Cancel);
        return;
    }

    let Some(mon) = state.sel_monitor() else { return };
    let output = mon.output.clone();
    let work = mon.w;
    let tags = mon.tags();

    let cells = build_cells(state, all_tags, tags, work);
    if cells.is_empty() {
        return;
    }

    state.overview = Some(OverviewState {
        cells,
        all_tags,
        output,
        typed: String::new(),
        hovered: None,
        // No cell highlighted initially; the first arrow key / hover starts it.
        selected: None,
        anim_start: Instant::now(),
        closing: None,
        backdrop_id: Id::new(),
        frame_counter: CommitCounter::default(),
    });
    state.schedule_render();
}

/// Collect the windows to show and lay them out in a near-square grid.
#[allow(clippy::cast_possible_wrap, clippy::cast_possible_truncation)]
fn build_cells(
    state: &Rwl,
    all_tags: bool,
    active_tags: u32,
    work: Rectangle<i32, Logical>,
) -> Vec<OverviewCell> {
    // Candidate windows: mapped, and (unless all-tags) visible on the active tags.
    // `state.windows` is already in master-to-stack tiling order, so keeping that
    // order within each tag gives the desired master-first layout.
    let mut windows: Vec<Window> = state
        .windows
        .iter()
        .filter(|w| with_state(w, |s| s.buffer_mapped).unwrap_or(false))
        // Never show scratchpad windows in the overview.
        .filter(|w| with_state(w, |s| s.scratch_key == '\0').unwrap_or(true))
        .filter(|w| all_tags || window_visible_on(w, active_tags))
        .cloned()
        .collect();

    // Order the grid by tag (1..N), then master-to-stack within each tag. A
    // window's tag is its lowest set bit; the sort is stable so the existing
    // `state.windows` order (master first) is preserved for windows on the same
    // tag. Empty tag masks (0) sort last.
    windows.sort_by_key(|w| {
        let tags = window_tags(w);
        if tags == 0 { u32::MAX } else { tags.trailing_zeros() }
    });

    let n = windows.len();
    if n == 0 {
        return Vec::new();
    }

    let hints = assign_hints(n);
    let (cols, cell_w, cell_h, pad) = grid_dims(n, work);

    windows
        .into_iter()
        .enumerate()
        .map(|(i, window)| {
            let col = i % cols;
            let row = i / cols;
            let target = Rectangle::new(
                Point::from((
                    work.loc.x + pad + (cell_w + pad) * col as i32,
                    work.loc.y + pad + (cell_h + pad) * row as i32,
                )),
                Size::from((cell_w, cell_h)),
            );
            let source = state.space.element_geometry(&window).unwrap_or(target);
            let tag = window_tags(&window);
            let hint = hints.get(i).cloned().unwrap_or_default();
            OverviewCell {
                window,
                source,
                target,
                hint,
                tag,
                ring_ids: std::array::from_fn(|_| Id::new()),
            }
        })
        .collect()
}

/// Compute grid columns/rows and the per-cell size for `n` windows in `work`.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
)]
fn grid_dims(n: usize, work: Rectangle<i32, Logical>) -> (usize, i32, i32, i32) {
    let pad = crate::config::get().overview.padding as i32;
    let cols = (n as f64).sqrt().ceil().max(1.0) as usize;
    let rows = n.div_ceil(cols).max(1);
    let cell_w = ((work.size.w - pad) / cols as i32 - pad).max(1);
    let cell_h = ((work.size.h - pad) / rows as i32 - pad).max(1);
    (cols, cell_w, cell_h, pad)
}

/// Assign single-char hints from the configured key set, falling back to
/// two-char hints once the window count exceeds the key count.
fn assign_hints(n: usize) -> Vec<String> {
    let keys = crate::config::get().overview.hint_keys.clone();
    let chars: Vec<char> = keys.chars().collect();
    if chars.is_empty() {
        return vec![String::new(); n];
    }
    if n <= chars.len() {
        return chars.iter().take(n).map(ToString::to_string).collect();
    }
    // Two-char hints: prefix + suffix from the same key set.
    let mut out = Vec::with_capacity(n);
    'outer: for &a in &chars {
        for &b in &chars {
            out.push(format!("{a}{b}"));
            if out.len() == n {
                break 'outer;
            }
        }
    }
    out
}

/// Start the close animation heading toward `sel`.
fn begin_close(state: &mut Rwl, sel: Selection) {
    if let Some(ov) = state.overview.as_mut() {
        ov.closing = Some(sel);
        ov.anim_start = Instant::now();
    }
    state.schedule_render();
}

/// Tear the overview down immediately, with no close animation. Used when a
/// compositor keybind is pressed while the overview is open so the action
/// applies to the real layout without waiting for the zoom-out to finish.
/// Focus is left untouched (the overview never changed it).
pub fn force_close(state: &mut Rwl) {
    if state.overview.take().is_some() {
        state.schedule_render();
    }
}

/// Close the window of the currently selected thumbnail **without** leaving the
/// overview (the grid rebuilds via [`on_window_closed`] once the client exits).
/// Does nothing when no thumbnail is selected.
pub fn kill_selected(state: &Rwl) {
    let window = state
        .overview
        .as_ref()
        .and_then(|ov| ov.selected.and_then(|i| ov.cells.get(i)))
        .map(|c| c.window.clone());
    if let Some(window) = window
        && let Some(tl) = window.toplevel()
    {
        tl.send_close();
    }
}

/// Rebuild the overview grid after a window closed while it was open (from the
/// overview's own kill, or the app closing itself). Keeps the selection near
/// where it was; closes the overview if no windows remain.
pub fn on_window_closed(state: &mut Rwl) {
    let Some(ov) = state.overview.as_ref() else { return };
    let (all_tags, output, prev_selected) = (ov.all_tags, ov.output.clone(), ov.selected);

    let Some((tags, work)) = state
        .monitors
        .iter()
        .find(|m| m.output == output)
        .map(|m| (m.tags(), m.w))
    else {
        force_close(state);
        return;
    };

    let cells = build_cells(state, all_tags, tags, work);
    if cells.is_empty() {
        force_close(state);
        return;
    }

    if let Some(ov) = state.overview.as_mut() {
        let last = cells.len() - 1;
        ov.cells = cells;
        ov.hovered = None;
        ov.typed.clear();
        ov.selected = prev_selected.map(|i| i.min(last));
    }
    state.schedule_render();
}

// ---------------------------------------------------------------------------
// Animation
// ---------------------------------------------------------------------------

/// Advance the overview open/close animation by one frame.
///
/// Returns `true` while the animation is still running (caller should then call
/// `schedule_render`). When a close animation completes this consumes the
/// session and applies the pending selection (focus / tag switch).
#[allow(clippy::cast_precision_loss)]
pub fn advance(state: &mut Rwl) -> bool {
    let (done, closing) = {
        let Some(ov) = state.overview.as_mut() else { return false };
        let duration = crate::config::get().overview.anim_ms.max(1) as f32;
        let elapsed = ov.anim_start.elapsed().as_secs_f32() * 1000.0;
        let done = elapsed >= duration;
        if !done {
            // Force re-damage of the backdrop and rings while they move/fade.
            ov.frame_counter.increment();
        }
        (done, ov.closing.is_some())
    };

    if closing {
        if done {
            finish_close(state);
            return false;
        }
        return true;
    }
    // Opening: keep animating until the duration elapses.
    !done
}

/// Fraction 0.0..1.0 describing how "spread out" the grid is right now.
/// 1.0 = fully in the grid, 0.0 = at the windows' real positions.
#[allow(clippy::cast_precision_loss)]
fn progress(ov: &OverviewState) -> f32 {
    let duration = crate::config::get().overview.anim_ms.max(1) as f32;
    let t = (ov.anim_start.elapsed().as_secs_f32() * 1000.0 / duration).clamp(0.0, 1.0);
    let eased = 1.0 - (1.0 - t).powi(3); // cubic ease-out
    if ov.closing.is_some() {
        1.0 - eased
    } else {
        eased
    }
}

/// Apply the pending selection and tear down the overview.
fn finish_close(state: &mut Rwl) {
    let Some(ov) = state.overview.take() else { return };
    if let Some(Selection::Window(window, tag)) = ov.closing {
        // Switch to the window's tag first (all-tags mode may select off-tag).
        let cur = state.sel_monitor().map_or(0, crate::monitor::Monitor::tags);
        if tag != 0 && tag != cur {
            state.dispatch(&crate::config::Action::View(tag));
        }
        state.focus_window(Some(window));
        #[cfg(feature = "warp")]
        crate::features::warp::warp_cursor_to_focused(state);
    }
    state.schedule_render();
}

// ---------------------------------------------------------------------------
// Input
// ---------------------------------------------------------------------------

/// Handle a keysym while the overview is active. All keys are swallowed.
pub fn handle_key(state: &mut Rwl, keysym: u32) {
    const ESCAPE: u32 = 0xff1b;
    const BACKSPACE: u32 = 0xff08;
    const RETURN: u32 = 0xff0d;
    const LEFT: u32 = 0xff51;
    const UP: u32 = 0xff52;
    const RIGHT: u32 = 0xff53;
    const DOWN: u32 = 0xff54;

    // Ignore keys once we are already closing.
    if state.overview.as_ref().is_none_or(|o| o.closing.is_some()) {
        return;
    }

    match keysym {
        ESCAPE => begin_close(state, Selection::Cancel),
        RETURN => select_best(state),
        LEFT | UP => move_selected(state, Dir::Back),
        RIGHT | DOWN => move_selected(state, Dir::Forward),
        BACKSPACE => {
            if let Some(ov) = state.overview.as_mut() {
                ov.typed.pop();
            }
            state.schedule_render();
        }
        sym => {
            let Some(c) = char::from_u32(sym).filter(char::is_ascii_graphic) else { return };
            type_char(state, c.to_ascii_lowercase());
        }
    }
}

/// Arrow-key navigation direction. All arrows cycle linearly through every
/// thumbnail (wrapping): Right/Down step forward, Left/Up step back.
#[derive(Clone, Copy)]
enum Dir {
    Back,
    Forward,
}

/// Cycle the keyboard selection to the next/previous thumbnail, wrapping around.
/// Clears any typed hint prefix so the two selection modes don't fight.
fn move_selected(state: &mut Rwl, dir: Dir) {
    let Some(ov) = state.overview.as_mut() else { return };
    let n = ov.cells.len();
    if n == 0 {
        return;
    }
    ov.typed.clear();

    let next = match (ov.selected, dir) {
        // Nothing selected yet: first press lands on an end of the list.
        (None, Dir::Forward) => 0,
        (None, Dir::Back) => n - 1,
        (Some(cur), Dir::Forward) => (cur + 1) % n,
        (Some(cur), Dir::Back) => (cur + n - 1) % n,
    };
    ov.selected = Some(next);
    ov.hovered = None; // arrow keys take over from the mouse
    state.schedule_render();
}

/// Append `c` to the typed hint and select if it uniquely identifies a cell.
fn type_char(state: &mut Rwl, c: char) {
    let selected = {
        let Some(ov) = state.overview.as_mut() else { return };
        ov.typed.push(c);
        // No cell's hint starts with the new prefix → reject the char.
        if ov.cells.iter().all(|cell| !cell.hint.starts_with(&ov.typed)) {
            ov.typed.pop();
            None
        } else {
            // Exact full-hint match → select.
            ov.cells
                .iter()
                .find(|cell| cell.hint == ov.typed)
                .map(|cell| (cell.window.clone(), cell.tag))
        }
    };
    if let Some((window, tag)) = selected {
        begin_close(state, Selection::Window(window, tag));
    } else {
        state.schedule_render();
    }
}

/// Select the single cell matching the typed prefix, else the arrow-key
/// selected cell, else the hovered cell.
fn select_best(state: &mut Rwl) {
    let picked = state.overview.as_ref().and_then(|ov| {
        let matches: Vec<&OverviewCell> = ov
            .cells
            .iter()
            .filter(|c| c.hint.starts_with(&ov.typed))
            .collect();
        if !ov.typed.is_empty() && matches.len() == 1 {
            matches.first().map(|c| (c.window.clone(), c.tag))
        } else {
            ov.selected
                .or(ov.hovered)
                .and_then(|i| ov.cells.get(i))
                .map(|c| (c.window.clone(), c.tag))
        }
    });
    if let Some((window, tag)) = picked {
        begin_close(state, Selection::Window(window, tag));
    }
}

/// Update the hovered cell from a pointer position (logical, global).
#[allow(clippy::cast_possible_truncation)]
pub fn handle_pointer_motion(state: &mut Rwl, loc: Point<f64, Logical>) {
    let changed = {
        let Some(ov) = state.overview.as_mut() else { return };
        let loc_i = Point::from((loc.x.round() as i32, loc.y.round() as i32));
        let hit = ov.cells.iter().position(|c| c.target.contains(loc_i));
        let changed = hit != ov.hovered;
        ov.hovered = hit;
        // Mouse and keyboard share one highlight: hovering moves the selection.
        if let Some(i) = hit {
            ov.selected = Some(i);
        }
        changed
    };
    if changed {
        state.schedule_render();
    }
}

/// Handle a pointer click at `loc` (logical, global): select the cell under it.
#[allow(clippy::cast_possible_truncation)]
pub fn handle_pointer_click(state: &mut Rwl, loc: Point<f64, Logical>) {
    let picked = state.overview.as_ref().and_then(|ov| {
        let loc_i = Point::from((loc.x.round() as i32, loc.y.round() as i32));
        ov.cells
            .iter()
            .find(|c| c.target.contains(loc_i))
            .map(|c| (c.window.clone(), c.tag))
    });
    if let Some((window, tag)) = picked {
        begin_close(state, Selection::Window(window, tag));
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// Linearly interpolate between two rectangles.
#[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
fn lerp_rect(a: Rectangle<i32, Logical>, b: Rectangle<i32, Logical>, t: f32) -> Rectangle<i32, Logical> {
    let lx = |x0: i32, x1: i32| ((x1 - x0) as f32).mul_add(t, x0 as f32).round() as i32;
    Rectangle::new(
        Point::from((lx(a.loc.x, b.loc.x), lx(a.loc.y, b.loc.y))),
        Size::from((lx(a.size.w, b.size.w).max(1), lx(a.size.h, b.size.h).max(1))),
    )
}

/// Build the full overview render-element list for `output`.
///
/// Returns an empty vec when the overview is not being shown on this output;
/// callers should then fall back to the normal window/border element path.
/// Front-to-back order: highlight ring → hint badges → thumbnails → backdrop.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::similar_names,
    clippy::too_many_lines,
)]
#[cfg_attr(not(feature = "rounded-corners"), allow(unused_variables))]
pub fn overview_elements(
    renderer: &mut GlesRenderer,
    ov: &OverviewState,
    space: &smithay::desktop::space::Space<Window>,
    output: &Output,
    scale: f64,
    round: Option<crate::render::ThumbRound<'_>>,
) -> Vec<RwlRenderElement> {
    let t = progress(ov);
    // Output height in physical pixels — needed to flip Y for the corner shader.
    #[cfg(feature = "rounded-corners")]
    let output_h_phys = output.current_mode().map_or(0, |m| m.size.h);
    #[allow(clippy::cast_possible_wrap)]
    let (dim, highlight, highlight_px, border_color, border_px, hint_bg, hint_fg) = {
        let cfg = crate::config::get();
        (
            cfg.overview.dim,
            cfg.overview.highlight_color,
            cfg.overview.highlight_px as i32,
            cfg.overview.border_color,
            cfg.overview.border_px as i32,
            [0.0, 0.0, 0.0_f32, 0.85],
            cfg.overview.highlight_color,
        )
    };

    let behavior = ConstrainBehavior {
        reference: ConstrainReference::BoundingBox,
        behavior: ConstrainScaleBehavior::Fit,
        align: ConstrainAlign::CENTER,
    };

    let mut elems: Vec<RwlRenderElement> = Vec::new();

    // Which cell(s) are "active": hovered, or matching the typed prefix.
    let matches_prefix = |cell: &OverviewCell| !ov.typed.is_empty() && cell.hint.starts_with(&ov.typed);

    for (i, cell) in ov.cells.iter().enumerate() {
        let rect = lerp_rect(cell.source, cell.target, t);
        // While typing a hint, only prefix matches highlight; otherwise the
        // arrow-key / mouse selection does.
        let active = if ov.typed.is_empty() {
            ov.hovered == Some(i) || ov.selected == Some(i)
        } else {
            matches_prefix(cell)
        };

        // A ring around every thumbnail: the bright highlight for the active
        // cell, a thinner border for the rest. Rounded to match the thumbnail
        // when a corner shader is available, else flat strips.
        let (ring_color, ring_px) = if active {
            (highlight, highlight_px)
        } else {
            (border_color, border_px)
        };
        if ring_px > 0 && ring_color[3] > 0.001 {
            #[cfg(feature = "rounded-corners")]
            {
                if let Some(r) = round.as_ref() {
                    if let Some(elem) = crate::render::rounded_thumb_ring(renderer, &cell.ring_ids[0], rect, ring_color, ring_px, r, output_h_phys, scale, ov.frame_counter) {
                        elems.push(RwlRenderElement::RoundedBorder(elem));
                    }
                } else {
                    elems.extend(ring_elements(cell, rect, ring_color, ring_px, scale, ov.frame_counter));
                }
            }
            #[cfg(not(feature = "rounded-corners"))]
            elems.extend(ring_elements(cell, rect, ring_color, ring_px, scale, ov.frame_counter));
        }

        // Hint badge in the cell's top-left corner. Dim non-matching hints.
        if !cell.hint.is_empty() && ov.closing.is_none() {
            let faded = !ov.typed.is_empty() && !matches_prefix(cell);
            let bg = if faded { [hint_bg[0], hint_bg[1], hint_bg[2], hint_bg[3] * 0.4] } else { hint_bg };
            let fg = if faded { [hint_fg[0], hint_fg[1], hint_fg[2], hint_fg[3] * 0.4] } else { hint_fg };
            let badge_loc: Point<i32, Physical> = rect.loc.to_physical_precise_round(scale);
            if let Some(elem) = font::hint_badge(renderer, &cell.hint, badge_loc, bg, fg, scale) {
                elems.push(RwlRenderElement::Memory(elem));
            }
        }

        // The live thumbnail, constrained into `rect` — rounded when a corner
        // shader is available, else the plain cropped element.
        let thumbs = constrain_space_element(
            renderer,
            &cell.window,
            rect.loc,
            1.0,
            scale,
            rect,
            behavior,
        );
        #[cfg(feature = "rounded-corners")]
        {
            if let Some(r) = round.as_ref() {
                let (cx, cy, cw, ch) = crate::render::thumb_corners(rect, r.y_inverted, output_h_phys, scale);
                elems.extend(thumbs.map(|inner| {
                    RwlRenderElement::RoundedOverview(crate::render::RoundedThumbElem {
                        inner,
                        corner_x: cx,
                        corner_y: cy,
                        corner_w: cw,
                        corner_h: ch,
                        corner_r: r.radius_px,
                        program: r.corner_shader.clone(),
                    })
                }));
            } else {
                elems.extend(thumbs.map(RwlRenderElement::Overview));
            }
        }
        #[cfg(not(feature = "rounded-corners"))]
        elems.extend(thumbs.map(RwlRenderElement::Overview));
    }

    // Full-output dim backdrop (bottommost of the overview block).
    if let Some(out_geo) = space.output_geometry(output) {
        let phys_loc: Point<i32, Physical> = out_geo.loc.to_physical_precise_round(scale);
        let phys_size: Size<i32, Physical> = out_geo.size.to_physical_precise_round(scale);
        // Fade the dim in/out with the animation so close doesn't pop.
        let dim = [dim[0], dim[1], dim[2], dim[3] * t];
        elems.push(RwlRenderElement::Border(SolidColorRenderElement::new(
            ov.backdrop_id.clone(),
            Rectangle::new(phys_loc, phys_size),
            ov.frame_counter,
            dim,
            Kind::Unspecified,
        )));
    }

    elems
}

/// Build four solid strips forming a `bw`-thick ring around `rect`.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn ring_elements(
    cell: &OverviewCell,
    rect: Rectangle<i32, Logical>,
    color: Color,
    bw: i32,
    scale: f64,
    counter: CommitCounter,
) -> Vec<RwlRenderElement> {
    let ids = &cell.ring_ids;

    let strips: [Rectangle<i32, Logical>; 4] = [
        Rectangle::new(Point::from((rect.loc.x - bw, rect.loc.y - bw)), Size::from((rect.size.w + bw * 2, bw))),
        Rectangle::new(Point::from((rect.loc.x - bw, rect.loc.y + rect.size.h)), Size::from((rect.size.w + bw * 2, bw))),
        Rectangle::new(Point::from((rect.loc.x - bw, rect.loc.y)), Size::from((bw, rect.size.h))),
        Rectangle::new(Point::from((rect.loc.x + rect.size.w, rect.loc.y)), Size::from((bw, rect.size.h))),
    ];

    strips
        .into_iter()
        .enumerate()
        .filter_map(|(i, r)| {
            let phys_loc: Point<i32, Physical> = r.loc.to_physical_precise_round(scale);
            let phys_size: Size<i32, Physical> = r.size.to_physical_precise_round(scale);
            (phys_size.w > 0 && phys_size.h > 0).then(|| {
                RwlRenderElement::Border(SolidColorRenderElement::new(
                    ids[i].clone(),
                    Rectangle::new(phys_loc, phys_size),
                    counter,
                    color,
                    Kind::Unspecified,
                ))
            })
        })
        .collect()
}

