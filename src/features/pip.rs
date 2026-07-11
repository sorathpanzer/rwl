//! Picture-in-Picture.
//!
//! Pops the focused window into a small, always-on-top, live thumbnail pinned to
//! a corner of the screen — great for watching a build/log/video while working
//! in another window (or on another tag).
//!
//! Rendering reuses the overview's thumbnail machinery: smithay's
//! [`constrain_space_element`] scales the window's live render elements into the
//! corner box, yielding the same `RwlRenderElement::Overview` variant.
//!
//! Liveness across tags: windows on a hidden tag are `unmap_elem`'d from the
//! space and stop receiving frame callbacks, which would freeze the mirror. So
//! [`send_frame`] delivers a frame callback to the PiP window every frame when
//! it is not otherwise visible, and `commit()` already schedules a render on any
//! surface commit — together keeping the thumbnail live.
//!
//! Compiled only when the `pip` Cargo feature is enabled.

use smithay::backend::renderer::element::solid::SolidColorRenderElement;
use smithay::backend::renderer::element::utils::{ConstrainAlign, ConstrainScaleBehavior};
use smithay::backend::renderer::element::{Id, Kind};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::utils::CommitCounter;
use smithay::desktop::space::{
    constrain_space_element, ConstrainBehavior, ConstrainReference, Space,
};
use smithay::desktop::utils::send_frames_surface_tree;
use smithay::desktop::Window;
use smithay::output::Output;
use smithay::utils::{IsAlive, Logical, Physical, Point, Rectangle, Size};
use smithay::wayland::seat::WaylandFocus;

use crate::config::{hex_color, Color};
use crate::render::RwlRenderElement;
use crate::state::Rwl;

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

/// User-tunable Picture-in-Picture appearance, loaded from the `pip` Lua table.
/// The Lua parser lives in `config::settings`; this owns the shape + defaults.
#[derive(Debug, Clone)]
pub struct PipSettings {
    /// Thumbnail width in logical pixels (height follows the window's aspect).
    pub width: u32,
    /// Gap in logical pixels between the thumbnail and the screen edges.
    pub margin: u32,
    /// Border thickness around the thumbnail in logical pixels (0 = none).
    pub border_px: u32,
    /// Border colour.
    pub border_color: Color,
    /// Initial corner: `top-left` | `top-right` | `bottom-left` | `bottom-right`.
    pub corner: String,
}

impl Default for PipSettings {
    fn default() -> Self {
        Self {
            width:        480,
            margin:       24,
            border_px:    2,
            border_color: hex_color(0xffff_ffff),
            corner:       "bottom-right".into(),
        }
    }
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// Which corner the PiP thumbnail is pinned to.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Corner {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

impl Corner {
    /// Cycle to the next corner (clockwise from top-left).
    const fn next(self) -> Self {
        match self {
            Self::TopLeft => Self::TopRight,
            Self::TopRight => Self::BottomRight,
            Self::BottomRight => Self::BottomLeft,
            Self::BottomLeft => Self::TopLeft,
        }
    }

    fn from_config(s: &str) -> Self {
        match s {
            "top-left" | "top_left" => Self::TopLeft,
            "top-right" | "top_right" => Self::TopRight,
            "bottom-left" | "bottom_left" => Self::BottomLeft,
            _ => Self::BottomRight,
        }
    }
}

/// An active PiP session.
pub struct PipState {
    /// The mirrored window.
    pub window: Window,
    /// Output the thumbnail is drawn on (the focused monitor at toggle time).
    output: Output,
    corner: Corner,
    /// Stable damage ids for the opaque backdrop and 4 border strips.
    bg_id: Id,
    border_ids: [Id; 4],
    /// Bumped when the box geometry changes (corner move / aspect change) so the
    /// solid elements re-damage.
    counter: CommitCounter,
}

impl std::fmt::Debug for PipState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PipState").finish_non_exhaustive()
    }
}

// ---------------------------------------------------------------------------
// Toggle / move / lifecycle
// ---------------------------------------------------------------------------

/// Toggle PiP: enable it on the focused window, or disable it if already on.
pub fn toggle(state: &mut Rwl) {
    if state.pip.is_some() {
        state.pip = None;
        state.schedule_render();
        return;
    }
    let Some(window) = state.focused_window().cloned() else { return };
    let Some(output) = state.sel_monitor().map(|m| m.output.clone()) else { return };
    let corner = Corner::from_config(&crate::config::get().pip.corner);
    state.pip = Some(PipState {
        window,
        output,
        corner,
        bg_id: Id::new(),
        border_ids: std::array::from_fn(|_| Id::new()),
        counter: CommitCounter::default(),
    });
    state.schedule_render();
}

/// Cycle the PiP thumbnail to the next corner.
pub fn move_corner(state: &mut Rwl) {
    if let Some(pip) = state.pip.as_mut() {
        pip.corner = pip.corner.next();
        pip.counter.increment();
        state.schedule_render();
    }
}

/// Clear PiP if its window just closed.
pub fn on_window_closed(state: &mut Rwl, window: &Window) {
    if state.pip.as_ref().is_some_and(|p| &p.window == window) {
        state.pip = None;
        state.schedule_render();
    }
}

// ---------------------------------------------------------------------------
// Liveness — frame callbacks for a hidden PiP window
// ---------------------------------------------------------------------------

/// Deliver a frame callback to the PiP window so it keeps producing frames even
/// when it lives on a hidden tag (and is therefore unmapped from the space and
/// skipped by the normal frame-callback loop). No-op when the window is already
/// visible on this output or lives on a different output.
pub fn send_frame(state: &Rwl, output: &Output, time: impl Into<std::time::Duration>) {
    let Some(pip) = state.pip.as_ref() else { return };
    if &pip.output != output || !pip.window.alive() {
        return;
    }
    // Already covered by the normal space frame-callback loop.
    if state.space.elements().any(|w| w == &pip.window) {
        return;
    }
    if let Some(surf) = pip.window.wl_surface() {
        let surf = surf.into_owned();
        send_frames_surface_tree(&surf, output, time, None, |_, _| Some(output.clone()));
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// Build the PiP render elements for `output` (empty when PiP is off, the window
/// is dead, or this is not the PiP output). Front-to-back: border → thumbnail →
/// opaque backdrop.
#[cfg_attr(not(feature = "rounded-corners"), allow(unused_variables))]
#[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap, clippy::cast_sign_loss)]
pub fn pip_elements(
    renderer: &mut GlesRenderer,
    pip: &PipState,
    space: &Space<Window>,
    output: &Output,
    scale: f64,
    round: Option<crate::render::ThumbRound<'_>>,
) -> Vec<RwlRenderElement> {
    if &pip.output != output || !pip.window.alive() {
        return Vec::new();
    }
    #[cfg(feature = "rounded-corners")]
    let output_h_phys = output.current_mode().map_or(0, |m| m.size.h);
    let Some(out_geo) = space.output_geometry(output) else { return Vec::new() };
    let win_size = pip.window.geometry().size;
    if win_size.w <= 0 || win_size.h <= 0 {
        return Vec::new();
    }

    let (width, margin, border_px, border_color) = {
        let cfg = crate::config::get();
        (
            cfg.pip.width as i32,
            cfg.pip.margin as i32,
            cfg.pip.border_px as i32,
            cfg.pip.border_color,
        )
    };

    // Box preserves the window's aspect ratio, capped to the work area.
    let max_w = (out_geo.size.w - margin * 2).max(1);
    let max_h = (out_geo.size.h - margin * 2).max(1);
    let mut box_w = width.clamp(1, max_w);
    let mut box_h = (box_w * win_size.h / win_size.w).max(1);
    if box_h > max_h {
        box_h = max_h;
        box_w = (box_h * win_size.w / win_size.h).clamp(1, max_w);
    }

    let loc = corner_loc(pip.corner, out_geo, box_w, box_h, margin);
    let box_rect = Rectangle::new(loc, Size::from((box_w, box_h)));

    let mut elems: Vec<RwlRenderElement> = Vec::new();

    // Border ring around the box (topmost): rounded when a corner shader is
    // available, else flat strips.
    if border_px > 0 && border_color[3] > 0.001 {
        #[cfg(feature = "rounded-corners")]
        {
            if let Some(r) = round.as_ref() {
                if let Some(elem) = crate::render::rounded_thumb_ring(
                    renderer, &pip.border_ids[0], box_rect, border_color, border_px, r, output_h_phys, scale, pip.counter,
                ) {
                    elems.push(RwlRenderElement::RoundedBorder(elem));
                }
            } else {
                elems.extend(border_ring(&pip.border_ids, box_rect, border_px, border_color, pip.counter, scale));
            }
        }
        #[cfg(not(feature = "rounded-corners"))]
        elems.extend(border_ring(&pip.border_ids, box_rect, border_px, border_color, pip.counter, scale));
    }

    // Live thumbnail — rounded when a corner shader is available.
    let behavior = ConstrainBehavior {
        reference: ConstrainReference::BoundingBox,
        behavior: ConstrainScaleBehavior::Fit,
        align: ConstrainAlign::CENTER,
    };
    let thumbs = constrain_space_element(renderer, &pip.window, box_rect.loc, 1.0, scale, box_rect, behavior);
    #[cfg(feature = "rounded-corners")]
    {
        if let Some(r) = round.as_ref() {
            let (cx, cy, cw, ch) = crate::render::thumb_corners(box_rect, r.y_inverted, output_h_phys, scale);
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

    // Opaque backdrop so the box is never see-through (letterbox / no buffer).
    // Skipped when rounding is active — a square backdrop would show square
    // corners behind the rounded thumbnail.
    let rounded_active = { #[cfg(feature = "rounded-corners")] { round.is_some() } #[cfg(not(feature = "rounded-corners"))] { false } };
    if !rounded_active {
        let phys_loc: Point<i32, Physical> = box_rect.loc.to_physical_precise_round(scale);
        let phys_size: Size<i32, Physical> = box_rect.size.to_physical_precise_round(scale);
        elems.push(RwlRenderElement::Border(SolidColorRenderElement::new(
            pip.bg_id.clone(),
            Rectangle::new(phys_loc, phys_size),
            pip.counter,
            [0.0, 0.0, 0.0, 1.0],
            Kind::Unspecified,
        )));
    }

    elems
}

/// Top-left logical position of the box in the chosen corner of `out_geo`.
fn corner_loc(
    corner: Corner,
    out_geo: Rectangle<i32, Logical>,
    box_w: i32,
    box_h: i32,
    margin: i32,
) -> Point<i32, Logical> {
    let left = out_geo.loc.x + margin;
    let right = out_geo.loc.x + out_geo.size.w - margin - box_w;
    let top = out_geo.loc.y + margin;
    let bottom = out_geo.loc.y + out_geo.size.h - margin - box_h;
    let (x, y) = match corner {
        Corner::TopLeft => (left, top),
        Corner::TopRight => (right, top),
        Corner::BottomLeft => (left, bottom),
        Corner::BottomRight => (right, bottom),
    };
    Point::from((x, y))
}

/// Four solid strips forming a border just outside `rect`.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn border_ring(
    ids: &[Id; 4],
    rect: Rectangle<i32, Logical>,
    bw: i32,
    color: Color,
    counter: CommitCounter,
    scale: f64,
) -> Vec<RwlRenderElement> {
    if bw <= 0 {
        return Vec::new();
    }
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
