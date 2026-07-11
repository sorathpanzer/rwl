//! Compositor render element types and border-drawing helpers.

// The render_elements! macro generates code with patterns that may trigger
// some pedantic/nursery lints; suppress them for this module.
#![allow(
    clippy::wildcard_imports,
    clippy::missing_panics_doc,
    clippy::exhaustive_enums,
)]

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::memory::{MemoryRenderBuffer, MemoryRenderBufferRenderElement};
use smithay::backend::renderer::element::solid::SolidColorRenderElement;
use smithay::input::pointer::CursorIcon;
use smithay::backend::renderer::element::surface::{
    WaylandSurfaceRenderElement, render_elements_from_surface_tree,
};
use smithay::backend::renderer::element::{Id, Kind};
#[cfg(not(any(feature = "fade", feature = "tag-transition")))]
use smithay::backend::renderer::element::Wrap;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::utils::CommitCounter;
use smithay::desktop::space::{Space, SpaceRenderElements};
#[cfg(any(feature = "fade", feature = "tag-transition", feature = "rounded-corners"))]
use smithay::desktop::PopupManager;
use smithay::desktop::{LayerSurface, Window, layer_map_for_output};
use smithay::output::Output;
use smithay::wayland::session_lock::LockSurface;
use smithay::wayland::shell::wlr_layer::Layer as WlrLayer;
use smithay::input::pointer::{CursorImageStatus, CursorImageSurfaceData};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{IsAlive, Logical, Physical, Point, Rectangle, Size, Transform};
#[cfg(any(feature = "fade", feature = "tag-transition", feature = "rounded-corners"))]
use smithay::wayland::seat::WaylandFocus;
use smithay::wayland::compositor;

// Re-export all rounded-corner types so backends only need `crate::render::*`.
#[cfg(feature = "rounded-corners")]
pub use crate::features::rounded_corners::*;


use crate::config::Color;
use crate::window::{with_state, window_is_floating};

// ---------------------------------------------------------------------------
// Unified render element type (concrete GlesRenderer variant)
// ---------------------------------------------------------------------------

// The `<=Type>` form generates a non-generic enum + From impls, avoiding the
// unsatisfied-trait-bound issues of the generic form.
#[cfg(feature = "rounded-corners")]
smithay::backend::renderer::element::render_elements! {
    /// Render element covering window surfaces, border decorations, and the
    /// software cursor, concrete over `GlesRenderer`.
    pub RwlRenderElement<=GlesRenderer>;
    Space=SpaceRenderElements<GlesRenderer, WaylandSurfaceRenderElement<GlesRenderer>>,
    Border=SolidColorRenderElement,
    Cursor=WaylandSurfaceRenderElement<GlesRenderer>,
    Memory=MemoryRenderBufferRenderElement<GlesRenderer>,
    Rounded=RoundedWindowElem,
    RoundedBorder=RoundedBorderElem,
    Overview=OverviewThumb,
    RoundedOverview=RoundedThumbElem,
}

#[cfg(not(feature = "rounded-corners"))]
smithay::backend::renderer::element::render_elements! {
    /// Render element covering window surfaces, border decorations, and the
    /// software cursor, concrete over `GlesRenderer`.
    pub RwlRenderElement<=GlesRenderer>;
    Space=SpaceRenderElements<GlesRenderer, WaylandSurfaceRenderElement<GlesRenderer>>,
    Border=SolidColorRenderElement,
    Cursor=WaylandSurfaceRenderElement<GlesRenderer>,
    Memory=MemoryRenderBufferRenderElement<GlesRenderer>,
    Overview=OverviewThumb,
}

/// A scaled-down, repositioned, clipped live window thumbnail used by the
/// workspace overview (Exposé). Produced by `constrain_space_element`.
///
/// Defined unconditionally (not gated on `overview`) because the
/// `render_elements!` macro above always references it; only the overview
/// feature actually constructs values of this type.
pub type OverviewThumb = smithay::backend::renderer::element::utils::CropRenderElement<
    smithay::backend::renderer::element::utils::RelocateRenderElement<
        smithay::backend::renderer::element::utils::RescaleRenderElement<
            WaylandSurfaceRenderElement<GlesRenderer>,
        >,
    >,
>;

/// Corner-rounding resources shared by the overview and PiP thumbnails, passed
/// in from the backend when the `rounded-corners` feature is active and its
/// shaders compiled. Only referenced by the `overview_elements` /
/// `pip_elements` signatures and the `rounded-corners` builder, so it is
/// compiled only when one of those features is enabled.
#[cfg(any(feature = "overview", feature = "pip", feature = "rounded-corners"))]
#[derive(Clone, Copy)]
pub struct ThumbRound<'a> {
    /// Texture shader that clips a thumbnail to a rounded rectangle.
    pub corner_shader: &'a smithay::backend::renderer::gles::GlesTexProgram,
    /// Shader that draws a rounded border ring.
    pub border_shader: &'a smithay::backend::renderer::gles::GlesTexProgram,
    /// 1×1 dummy buffer that carries the border shader.
    pub dummy_buffer: &'a smithay::backend::renderer::element::memory::MemoryRenderBuffer,
    /// Corner radius in physical pixels.
    pub radius_px: f32,
    /// Whether the framebuffer Y-axis is inverted (EGL convention).
    pub y_inverted: bool,
}

#[cfg(any(feature = "overview", feature = "pip", feature = "rounded-corners"))]
impl std::fmt::Debug for ThumbRound<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ThumbRound").field("radius_px", &self.radius_px).finish_non_exhaustive()
    }
}

impl std::fmt::Debug for RwlRenderElement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Space(e) => f.debug_tuple("Space").field(e).finish(),
            Self::Border(e) => f.debug_tuple("Border").field(e).finish(),
            Self::Cursor(e) => f.debug_tuple("Cursor").field(e).finish(),
            Self::Memory(_) => f.debug_tuple("Memory").finish(),
            #[cfg(feature = "rounded-corners")]
            Self::Rounded(_) => f.debug_tuple("Rounded").finish(),
            #[cfg(feature = "rounded-corners")]
            Self::RoundedBorder(_) => f.debug_tuple("RoundedBorder").finish(),
            Self::Overview(_) => f.debug_tuple("Overview").finish(),
            #[cfg(feature = "rounded-corners")]
            Self::RoundedOverview(_) => f.debug_tuple("RoundedOverview").finish(),
            Self::_GenericCatcher(_) => std::process::abort(),
        }
    }
}

// ---------------------------------------------------------------------------
// Border element generation
// ---------------------------------------------------------------------------

/// Build `SolidColorRenderElement`s for all window borders in `space`.
///
/// Each window gets 4 thin rectangles drawn just *outside* its content area
/// (top, bottom, left, right). Colour is taken from the window's stored
/// `border_color` field (maintained by `Rwl::update_borders`), falling back
/// to `FOCUS_COLOR` / `BORDER_COLOR` based on whether the window is focused.
///
/// `scale` is the output's fractional scale factor (logical → physical pixels).
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss
)]
pub fn border_elements(
    space: &Space<Window>,
    focused: Option<&Window>,
    scale: f64,
    visible_count: usize,
) -> Vec<SolidColorRenderElement> {
    let (bw, focus_color, border_color) = {
        let cfg = crate::config::get();
        (cfg.border_px as i32, cfg.focus_color, cfg.border_color)
    };
    // No borders when only one tiled window is visible and nothing is floating.
    let any_floating = space.elements().any(window_is_floating);
    if bw == 0 || (visible_count <= 1 && !any_floating) {
        return Vec::new();
    }

    let make_strips = |w: &Window, color: Color| -> Vec<SolidColorRenderElement> {
        let Some(geom) = space.element_geometry(w)
            .or_else(|| with_state(w, |s| s.pending_geom).flatten()) else {
            return Vec::new();
        };
        let (ids, counter) = with_state(w, |s| {
            (s.border_ids.clone(), s.border_counter)
        }).unwrap_or_else(|| {
            (std::array::from_fn(|_| Id::new()), CommitCounter::default())
        });

        #[cfg(feature = "tag-transition")]
        let slide_x_px: i32 =
            with_state(w, |s| (s.slide_offset_x * scale).round() as i32).unwrap_or(0);

        // Four thin strips in logical coordinates placed just outside the
        // window's content geometry.
        let logical_rects: [Rectangle<i32, Logical>; 4] = [
            Rectangle::new(
                // top strip
                Point::from((geom.loc.x - bw, geom.loc.y - bw)),
                Size::from((geom.size.w + bw * 2, bw)),
            ),
            Rectangle::new(
                // bottom strip
                Point::from((geom.loc.x - bw, geom.loc.y + geom.size.h)),
                Size::from((geom.size.w + bw * 2, bw)),
            ),
            Rectangle::new(
                // left strip
                Point::from((geom.loc.x - bw, geom.loc.y)),
                Size::from((bw, geom.size.h)),
            ),
            Rectangle::new(
                // right strip
                Point::from((geom.loc.x + geom.size.w, geom.loc.y)),
                Size::from((bw, geom.size.h)),
            ),
        ];

        logical_rects.into_iter().enumerate().filter_map(|(i, rect)| {
            if rect.size.w <= 0 || rect.size.h <= 0 { return None; }
            let phys_loc: Point<i32, Physical> = rect.loc.to_physical_precise_round(scale);
            #[cfg(feature = "tag-transition")]
            let phys_loc: Point<i32, Physical> = Point::from((phys_loc.x + slide_x_px, phys_loc.y));
            let phys_size: Size<i32, Physical> = rect.size.to_physical_precise_round(scale);
            (phys_size.w > 0 && phys_size.h > 0).then(|| SolidColorRenderElement::new(
                ids[i].clone(),
                Rectangle::new(phys_loc, phys_size),
                counter,
                color,
                Kind::Unspecified,
            ))
        }).collect()
    };

    // Non-focused windows first, focused window last — ensures focus_color is
    // always drawn on top when stacked windows share the same geometry (monocle).
    let mut out: Vec<SolidColorRenderElement> = space.elements()
        .filter(|w| !with_state(w, |s| s.is_fullscreen).unwrap_or(false))
        .filter(|w| Some(*w) != focused)
        .filter_map(|w| {
            // Fully transparent border: omit the element entirely.
            // The damage tracker's "element gone" path will clear any
            // stale focus-ring pixels from the previous frame.
            let color = (border_color[3] >= 0.001).then_some(border_color)?;
            Some(make_strips(w, color))
        })
        .flatten()
        .collect();

    if let Some(w) = focused
        && !with_state(w, |s| s.is_fullscreen).unwrap_or(false)
    {
        out.extend(make_strips(w, focus_color));
    }

    out
}

// ---------------------------------------------------------------------------
// Cursor element generation
// ---------------------------------------------------------------------------

/// Build software cursor render elements from a client-provided cursor surface.
///
/// When `cursor_status` is `Surface(surf)` and `cursor_hidden` is false,
/// renders `surf` at `pointer_loc - hotspot`, scaled by `scale`.
///
/// When `cursor_status` is `Named` or `Hidden` (e.g. after a video player
/// hid the cursor and the pointer moved to empty space), `fallback` is used
/// instead if it is alive — this ensures a visible software cursor even when
/// the active client hid theirs and there is no host-cursor fallback (TTY).
/// Pass `None` for `fallback` to suppress this behaviour (e.g. when the
/// pointer is over a window that explicitly requested no cursor).
pub fn cursor_elements(
    renderer: &mut GlesRenderer,
    cursor_status: &CursorImageStatus,
    cursor_hidden: bool,
    pointer_loc: Point<f64, Logical>,
    scale: f64,
    fallback: Option<&WlSurface>,
) -> Vec<WaylandSurfaceRenderElement<GlesRenderer>> {
    if cursor_hidden {
        return Vec::new();
    }
    let wl_surface = if let CursorImageStatus::Surface(surf) = cursor_status {
        surf
    } else if matches!(cursor_status, CursorImageStatus::Hidden) {
        // Client explicitly requested no cursor — honour it unconditionally.
        return Vec::new();
    } else {
        // cursor_status is Named (no client cursor set yet) — try the fallback.
        match fallback {
            Some(s) if s.alive() => s,
            _ => return Vec::new(),
        }
    };

    let hotspot = compositor::with_states(wl_surface, |states| {
        states
            .data_map
            .get::<CursorImageSurfaceData>()
            .and_then(|m| m.lock().ok())
            .map(|a| a.hotspot)
            .unwrap_or_default()
    });

    let cursor_loc = (pointer_loc - hotspot.to_f64())
        .to_physical(scale)
        .to_i32_round();

    render_elements_from_surface_tree(renderer, wl_surface, cursor_loc, scale, 1.0, Kind::Cursor)
}

/// Render the drag-and-drop icon surface at the pointer location.
///
/// Called each frame while a Wayland `DnD` operation is active.  The icon is
/// rendered on top of everything else (inserted before cursor elements in the
/// final element list) so it stays visually attached to the pointer.
pub fn dnd_icon_elements(
    renderer: &mut GlesRenderer,
    dnd_icon: &WlSurface,
    pointer_loc: Point<f64, Logical>,
    scale: f64,
) -> Vec<WaylandSurfaceRenderElement<GlesRenderer>> {
    if !dnd_icon.alive() {
        return Vec::new();
    }
    let loc = pointer_loc.to_physical(scale).to_i32_round();
    render_elements_from_surface_tree(renderer, dnd_icon, loc, scale, 1.0, Kind::Unspecified)
}

// ---------------------------------------------------------------------------
// Named (xcursor) cursor
// ---------------------------------------------------------------------------

/// Pre-loaded xcursor image kept in [`crate::state::Rwl`] for use during TTY
/// rendering.
#[derive(Clone)]
pub struct NamedCursorBuffer {
    /// CPU-side render buffer containing the cursor ARGB pixels.
    pub buffer: MemoryRenderBuffer,
    /// Hotspot in physical pixels (origin = top-left of cursor image).
    pub hotspot: Point<i32, Physical>,
}

/// Load the default arrow cursor.  Calls `load_cursor_icon` with `Default` and
/// falls back to a built-in bitmap when no theme is found.
pub fn load_default_cursor(theme: Option<&str>, size: u32) -> NamedCursorBuffer {
    let cursor_size = if size > 0 {
        size
    } else {
        std::env::var("XCURSOR_SIZE").ok().and_then(|s| s.parse().ok()).unwrap_or(24)
    };
    load_cursor_icon(theme, cursor_size, CursorIcon::Default).unwrap_or_else(|| {
        tracing::warn!(
            "xcursor theme not found (set XCURSOR_THEME / XCURSOR_PATH or cursor_theme in config); \
             using built-in fallback arrow cursor"
        );
        builtin_arrow_cursor()
    })
}

/// Try to load the xcursor image for `icon` from the configured theme.
///
/// Priority: explicit `theme` arg → `XCURSOR_THEME` env → common system themes.
/// Returns `None` if the icon is not found in any theme.
#[allow(
    clippy::cast_possible_wrap,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
)]
pub fn load_cursor_icon(
    explicit_theme: Option<&str>,
    cursor_size: u32,
    icon: CursorIcon,
) -> Option<NamedCursorBuffer> {
    let names = cursor_icon_xcursor_names(icon);
    let env_theme = std::env::var("XCURSOR_THEME").ok();
    let fallbacks = ["default", "Adwaita", "breeze_cursors", "Breeze",
                     "DMZ-White", "DMZ-Black", "Bibata-Modern-Classic",
                     "capitaine-cursors", "Vanilla-DMZ"];
    for theme_name in explicit_theme.into_iter().chain(env_theme.as_deref()).chain(fallbacks) {
        if let Some(buf) = xcursor_from_theme(theme_name, names, cursor_size) {
            return Some(buf);
        }
    }
    None
}

/// Try loading one of `icon_names` from a single named theme.
#[allow(
    clippy::cast_possible_wrap,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
)]
fn xcursor_from_theme(theme_name: &str, icon_names: &[&str], cursor_size: u32) -> Option<NamedCursorBuffer> {
    let theme = xcursor::CursorTheme::load(theme_name);
    for icon_name in icon_names {
        let Some(path) = theme.load_icon(icon_name) else { continue };
        let Ok(data) = std::fs::read(&path) else { continue };
        let Some(images) = xcursor::parser::parse_xcursor(&data) else { continue };
        let Some(image) = images.iter()
            .min_by_key(|img| (img.size as i32 - cursor_size as i32).unsigned_abs())
        else { continue };

        // xcursor pixels are 0xAARRGGBB (u32, native endian).
        // On little-endian this becomes [BB, GG, RR, AA] = Fourcc::Argb8888.
        let mut rgba_bytes = Vec::with_capacity(image.pixels_rgba.len() * 4);
        for &p in &image.pixels_rgba {
            rgba_bytes.extend_from_slice(&p.to_ne_bytes());
        }
        let buffer = MemoryRenderBuffer::from_slice(
            &rgba_bytes, Fourcc::Argb8888,
            (image.width as i32, image.height as i32),
            1, Transform::Normal, None,
        );
        tracing::debug!("Loaded cursor '{icon_name}' from theme '{theme_name}'");
        return Some(NamedCursorBuffer {
            buffer,
            hotspot: Point::from((image.xhot as i32, image.yhot as i32)),
        });
    }
    None
}

/// Map a [`CursorIcon`] to the xcursor icon names to try, in priority order.
pub const fn cursor_icon_xcursor_names(icon: CursorIcon) -> &'static [&'static str] {
    match icon {
        CursorIcon::Default      => &["left_ptr", "default", "arrow"],
        CursorIcon::Pointer      => &["hand2", "pointer", "pointing_hand"],
        CursorIcon::Text         => &["xterm", "text", "ibeam"],
        CursorIcon::Wait         => &["watch", "wait"],
        CursorIcon::Progress     => &["left_ptr_watch", "progress"],
        CursorIcon::Help         => &["question_arrow", "help"],
        CursorIcon::Move         => &["fleur", "move"],
        CursorIcon::Crosshair    => &["crosshair", "cross"],
        CursorIcon::Grab         => &["hand1", "grab", "openhand"],
        CursorIcon::Grabbing     => &["grabbing", "closedhand"],
        CursorIcon::NotAllowed   => &["crossed_circle", "not-allowed", "forbidden"],
        CursorIcon::NoDrop       => &["no-drop"],
        CursorIcon::Copy         => &["copy"],
        CursorIcon::Alias        => &["dnd-link", "link"],
        CursorIcon::EResize      => &["right_side", "e-resize"],
        CursorIcon::WResize      => &["left_side", "w-resize"],
        CursorIcon::NResize      => &["top_side", "n-resize"],
        CursorIcon::SResize      => &["bottom_side", "s-resize"],
        CursorIcon::NeResize     => &["top_right_corner", "ne-resize"],
        CursorIcon::NwResize     => &["top_left_corner", "nw-resize"],
        CursorIcon::SeResize     => &["bottom_right_corner", "se-resize"],
        CursorIcon::SwResize     => &["bottom_left_corner", "sw-resize"],
        CursorIcon::EwResize     => &["h_double_arrow", "ew-resize", "size_hor"],
        CursorIcon::NsResize     => &["v_double_arrow", "ns-resize", "size_ver"],
        CursorIcon::NeswResize   => &["fd_double_arrow", "nesw-resize", "size_bdiag"],
        CursorIcon::NwseResize   => &["bd_double_arrow", "nwse-resize", "size_fdiag"],
        CursorIcon::ColResize    => &["col-resize", "split_h"],
        CursorIcon::RowResize    => &["row-resize", "split_v"],
        CursorIcon::AllScroll | CursorIcon::AllResize => &["all-scroll", "fleur"],
        CursorIcon::ZoomIn       => &["zoom-in"],
        CursorIcon::ZoomOut      => &["zoom-out"],
        CursorIcon::Cell         => &["cell", "plus"],
        CursorIcon::VerticalText => &["vertical-text"],
        CursorIcon::ContextMenu  => &["context-menu"],
        CursorIcon::DndAsk       => &["dnd-ask"],
        _                        => &["left_ptr"],
    }
}

/// Generate a minimal built-in left-pointer arrow cursor (16×16 ARGB8888).
///
/// Used when no xcursor theme is available on the system. The hotspot is at
/// the tip of the arrow (top-left corner, pixel 0,0).
fn builtin_arrow_cursor() -> NamedCursorBuffer {
    // B = opaque black, W = opaque white, _ = transparent
    const B: u32 = 0xFF00_0000;
    const W: u32 = 0xFFFF_FFFF;
    const T: u32 = 0x0000_0000;

    #[rustfmt::skip]
    const PIXELS: [u32; 16 * 16] = [
        B,T,T,T,T,T,T,T,T,T,T,T,T,T,T,T,
        B,B,T,T,T,T,T,T,T,T,T,T,T,T,T,T,
        B,W,B,T,T,T,T,T,T,T,T,T,T,T,T,T,
        B,W,W,B,T,T,T,T,T,T,T,T,T,T,T,T,
        B,W,W,W,B,T,T,T,T,T,T,T,T,T,T,T,
        B,W,W,W,W,B,T,T,T,T,T,T,T,T,T,T,
        B,W,W,W,W,W,B,T,T,T,T,T,T,T,T,T,
        B,W,W,W,W,W,W,B,T,T,T,T,T,T,T,T,
        B,W,W,W,W,W,W,W,B,T,T,T,T,T,T,T,
        B,W,W,W,W,W,B,B,B,T,T,T,T,T,T,T,
        B,W,W,B,W,W,B,T,T,T,T,T,T,T,T,T,
        B,W,B,T,B,W,W,B,T,T,T,T,T,T,T,T,
        B,B,T,T,B,W,W,B,T,T,T,T,T,T,T,T,
        T,T,T,T,T,B,W,B,T,T,T,T,T,T,T,T,
        T,T,T,T,T,B,B,T,T,T,T,T,T,T,T,T,
        T,T,T,T,T,T,T,T,T,T,T,T,T,T,T,T,
    ];

    let mut bytes = Vec::with_capacity(PIXELS.len() * 4);
    for &p in &PIXELS {
        bytes.extend_from_slice(&p.to_ne_bytes());
    }
    let buffer = MemoryRenderBuffer::from_slice(
        &bytes,
        Fourcc::Argb8888,
        (16, 16),
        1,
        Transform::Normal,
        None,
    );

    NamedCursorBuffer {
        buffer,
        hotspot: Point::from((0, 0)),
    }
}

/// Build a render element for the xcursor image stored in `named_cursor`.
///
/// Returns an empty vec if the cursor is hidden or no xcursor was loaded.
/// Only called from the TTY/udev backend; the winit backend relies on the
/// host compositor's cursor for `CursorImageStatus::Named`.
pub fn named_cursor_elements(
    renderer: &mut GlesRenderer,
    named_cursor: Option<&NamedCursorBuffer>,
    pointer_loc: Point<f64, Logical>,
    scale: f64,
) -> Vec<MemoryRenderBufferRenderElement<GlesRenderer>> {
    let Some(named) = named_cursor else {
        return Vec::new();
    };

    // Place the cursor image so its hotspot lands on the pointer location.
    let loc: Point<f64, Physical> =
        pointer_loc.to_physical(scale) - named.hotspot.to_f64();

    match MemoryRenderBufferRenderElement::from_buffer(
        renderer,
        loc,
        &named.buffer,
        None,
        None,
        None,
        Kind::Cursor,
    ) {
        Ok(elem) => vec![elem],
        Err(e) => {
            tracing::warn!("xcursor render element failed: {e}");
            Vec::new()
        }
    }
}

// ---------------------------------------------------------------------------
// Layer-shell element generation
// ---------------------------------------------------------------------------

/// Build render elements for all layer-shell surfaces on `output` that belong
/// to `layer`.
///
/// Surfaces are yielded in front-to-back order (newest mapping = topmost =
/// first in the returned vec) so callers can concatenate directly into a
/// front-to-back element list.
///
/// `scale` is the output's fractional scale factor.
#[allow(clippy::needless_collect)] // collect is required to drop the MutexGuard before re-entering the map
pub fn layer_elements(
    renderer: &mut GlesRenderer,
    output: &Output,
    layer: WlrLayer,
    scale: f64,
) -> Vec<WaylandSurfaceRenderElement<GlesRenderer>> {
    // Collect (surface, geometry) while the map guard is held, then release it
    // before calling render_elements_from_surface_tree (which may re-enter the map).
    // layers_on iterates oldest→newest; .rev() gives newest first = topmost first.
    // Both layers_on and layer_geometry take &self, so they can share the borrow.
    let map = layer_map_for_output(output);
    let layer_data: Vec<(LayerSurface, Rectangle<i32, Logical>)> = map
        .layers_on(layer)
        .rev()
        .filter_map(|s| map.layer_geometry(s).map(|g| (s.clone(), g)))
        .collect();
    drop(map); // release the map MutexGuard before rendering

    layer_data
        .into_iter()
        .flat_map(|(layer_surf, geo)| {
            let physical_loc = geo.loc.to_physical_precise_round(scale);
            render_elements_from_surface_tree(
                renderer,
                layer_surf.wl_surface(),
                physical_loc,
                scale,
                1.0,
                Kind::Unspecified,
            )
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Session-lock surface element generation
// ---------------------------------------------------------------------------

/// Build render elements for the lock surface assigned to `output`.
///
/// Returns an empty vec when no lock surface is found for this output or when
/// the surface has not yet committed a buffer.  Called from both backends
/// during each render pass; lock surfaces sit just below the cursor in the
/// front-to-back element list.
pub fn lock_surface_elements(
    renderer: &mut GlesRenderer,
    lock_surfaces: &[(LockSurface, Output)],
    output: &Output,
    scale: f64,
) -> Vec<WaylandSurfaceRenderElement<GlesRenderer>> {
    lock_surfaces
        .iter()
        .filter(|(surf, surf_output)| surf_output == output && surf.alive())
        .flat_map(|(surf, _)| {
            // Lock surface is positioned at the output's top-left, which is (0, 0)
            // in output-local physical coordinates (each output renders independently).
            render_elements_from_surface_tree(
                renderer,
                surf.wl_surface(),
                Point::from((0, 0)),
                scale,
                1.0,
                Kind::Unspecified,
            )
        })
        .collect()
}

/// Stable render-element id for the lock backdrop, reused across frames so the
/// damage tracker treats the black fill as unchanged (no per-frame full repaint
/// while the session sits locked).
fn lock_backdrop_id() -> Id {
    static ID: std::sync::OnceLock<Id> = std::sync::OnceLock::new();
    ID.get_or_init(Id::new).clone()
}

/// Build a full-output opaque black backdrop element for use while the session
/// is locked.
///
/// This guarantees that no client content is ever visible behind the lock
/// surfaces — including on outputs that have **no** live lock surface (e.g. a
/// monitor hotplugged after locking, or a locker that only covered a subset of
/// outputs) and in the interval before the locker commits an opaque buffer.
#[must_use]
pub fn lock_backdrop_element(output: &Output) -> Option<SolidColorRenderElement> {
    let mode = output.current_mode()?;
    let size: Size<i32, Physical> = Size::from((mode.size.w, mode.size.h));
    if size.w <= 0 || size.h <= 0 {
        return None;
    }
    Some(SolidColorRenderElement::new(
        lock_backdrop_id(),
        Rectangle::new(Point::from((0, 0)), size),
        CommitCounter::default(),
        [0.0, 0.0, 0.0, 1.0],
        Kind::Unspecified,
    ))
}

// ---------------------------------------------------------------------------
// window_surface_elements — per-window alpha (fade) path
// ---------------------------------------------------------------------------

/// Build render elements for all windows mapped to `output`, applying
/// per-window `fade_alpha` from [`crate::window::WindowState`].
///
/// This replaces `Space::render_elements_for_region` for the non-rounded
/// path so that entering/exiting windows can be faded independently.
/// A transparent `SolidColorRenderElement` sentinel is emitted for each
/// window that is currently fading so the damage tracker marks the region
/// dirty even when the Wayland surface itself has no new buffer commit.
///
/// Returned elements are in front-to-back order (same convention as
/// `Space::render_elements_for_region`).
#[cfg(any(feature = "fade", feature = "tag-transition"))]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
)]
pub fn window_surface_elements(
    renderer: &mut GlesRenderer,
    space: &Space<Window>,
    output: &Output,
    scale: f64,
) -> Vec<RwlRenderElement> {
    let output_geo = space.output_geometry(output).unwrap_or_default();
    let mut elems: Vec<RwlRenderElement> = Vec::new();

    // Space::elements() yields back-to-front; reverse for front-to-back.
    for w in space.elements().rev() {
        let Some(geom_logical) = space.element_geometry(w) else { continue };
        if !output_geo.overlaps(geom_logical) {
            continue;
        }

        #[cfg(feature = "fade")]
        let (alpha, fade_damage_id, fade_damage_counter, fading) =
            with_state(w, |s| (s.fade_alpha, s.fade_damage_id.clone(), s.fade_damage_counter, s.fade_start.is_some()))
                .unwrap_or_else(|| (1.0, Id::new(), CommitCounter::default(), false));
        #[cfg(not(feature = "fade"))]
        let alpha = 1.0_f32;

        #[cfg(feature = "tag-transition")]
        let (sliding, slide_dmg_id, slide_dmg_counter, slide_x_px) = with_state(w, |s| (
            s.slide_start.is_some(),
            s.slide_damage_id.clone(),
            s.slide_damage_counter,
            (s.slide_offset_x * scale).round() as i32,
        )).unwrap_or_else(|| (false, Id::new(), CommitCounter::default(), 0));

        let render_loc: Point<i32, Physical> = {
            let base = (geom_logical.loc - w.geometry().loc).to_physical_precise_round(scale);
            #[cfg(feature = "tag-transition")]
            { Point::from((base.x + slide_x_px, base.y)) }
            #[cfg(not(feature = "tag-transition"))]
            { base }
        };

        let Some(wl_surf_cow) = w.wl_surface() else { continue };
        let wl_surf = wl_surf_cow.into_owned();

        // Fade sentinel: forces damage-tracker repaint each frame during alpha changes.
        #[cfg(feature = "fade")]
        if fading {
            let phys_loc: Point<i32, Physical> =
                geom_logical.loc.to_physical_precise_round(scale);
            let phys_size: Size<i32, Physical> =
                geom_logical.size.to_physical_precise_round(scale);
            if phys_size.w > 0 && phys_size.h > 0 {
                elems.push(RwlRenderElement::Border(SolidColorRenderElement::new(
                    fade_damage_id,
                    Rectangle::new(phys_loc, phys_size),
                    fade_damage_counter,
                    [0.0, 0.0, 0.0, 0.0],
                    Kind::Unspecified,
                )));
            }
        }

        // Slide sentinel: forces repaint of the layout-position area while the
        // window slides in from off-screen, so the background is revealed there.
        #[cfg(feature = "tag-transition")]
        if sliding {
            let phys_loc: Point<i32, Physical> =
                geom_logical.loc.to_physical_precise_round(scale);
            let phys_size: Size<i32, Physical> =
                geom_logical.size.to_physical_precise_round(scale);
            if phys_size.w > 0 && phys_size.h > 0 {
                elems.push(RwlRenderElement::Border(SolidColorRenderElement::new(
                    slide_dmg_id,
                    Rectangle::new(phys_loc, phys_size),
                    slide_dmg_counter,
                    [0.0, 0.0, 0.0, 0.0],
                    Kind::Unspecified,
                )));
            }
        }

        // Popups rendered at the shifted location (they move with the window).
        for (popup, popup_offset) in PopupManager::popups_for_surface(&wl_surf) {
            let popup_render_loc = render_loc
                + (w.geometry().loc + popup_offset - popup.geometry().loc)
                    .to_physical_precise_round(scale);
            let popup_elems: Vec<WaylandSurfaceRenderElement<GlesRenderer>> =
                render_elements_from_surface_tree(
                    renderer,
                    popup.wl_surface(),
                    popup_render_loc,
                    scale,
                    alpha,
                    Kind::Unspecified,
                );
            for e in popup_elems {
                elems.push(RwlRenderElement::Cursor(e));
            }
        }

        let surface_elems: Vec<WaylandSurfaceRenderElement<GlesRenderer>> =
            render_elements_from_surface_tree(
                renderer,
                &wl_surf,
                render_loc,
                scale,
                alpha,
                Kind::Unspecified,
            );
        for e in surface_elems {
            elems.push(RwlRenderElement::Cursor(e));
        }
    }

    elems
}

// ---------------------------------------------------------------------------
// rounded_window_elements — public builder
// ---------------------------------------------------------------------------

/// Build render elements for all non-fullscreen windows, applying rounded
/// corners via `program` on main surfaces while leaving popup surfaces
/// unclipped (they extend beyond the window boundary).
///
/// Returned elements replace the full `space_elems` list from
/// `render_elements_for_region`; callers must NOT add plain `space_elems` on
/// top of these or windows will be double-drawn.
///
/// When `corner_radius == 0` the function returns `None`, signalling the
/// caller to fall back to the plain `space_elems` path.
#[cfg(feature = "rounded-corners")]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::too_many_lines,
)]
pub fn rounded_window_elements(
    renderer: &mut GlesRenderer,
    space: &Space<Window>,
    output: &Output,
    scale: f64,
    ctx: &RoundedWindowCtx<'_>,
) -> Option<Vec<RwlRenderElement>> {
    let (program, corner_radius, y_inverted) = (ctx.program, ctx.corner_radius, ctx.y_inverted);
    if corner_radius == 0 {
        return None;
    }

    let output_h = output.current_mode().map_or(0, |m| m.size.h);
    let corner_r = corner_radius as f32 * scale as f32;

    let mut elems: Vec<RwlRenderElement> = Vec::new();

    // Space::elements() yields windows back-to-front; reverse for front-to-back.
    for w in space.elements().rev() {
        let is_fullscreen = with_state(w, |s| s.is_fullscreen).unwrap_or(false);
        let Some(geom_logical) = space.element_geometry(w) else { continue };

        #[cfg(feature = "fade")]
        let (alpha, fade_damage_id, fade_damage_counter, fading) =
            with_state(w, |s| (s.fade_alpha, s.fade_damage_id.clone(), s.fade_damage_counter, s.fade_start.is_some()))
                .unwrap_or_else(|| (1.0, Id::new(), CommitCounter::default(), false));
        #[cfg(not(feature = "fade"))]
        let alpha = 1.0_f32;

        #[cfg(feature = "tag-transition")]
        let (sliding, slide_dmg_id, slide_dmg_counter, slide_x_px) = with_state(w, |s| (
            s.slide_start.is_some(),
            s.slide_damage_id.clone(),
            s.slide_damage_counter,
            (s.slide_offset_x * scale).round() as i32,
        )).unwrap_or_else(|| (false, Id::new(), CommitCounter::default(), 0));

        // render_location = element position minus XDG geometry offset + slide.
        let render_loc: Point<i32, Physical> = {
            let base = (geom_logical.loc - w.geometry().loc).to_physical_precise_round(scale);
            #[cfg(feature = "tag-transition")]
            { Point::from((base.x + slide_x_px, base.y)) }
            #[cfg(not(feature = "tag-transition"))]
            { base }
        };

        let Some(wl_surf_cow) = w.wl_surface() else { continue };
        let wl_surf = wl_surf_cow.into_owned();

        // Fade sentinel: drives per-frame damage when no buffer commit.
        #[cfg(feature = "fade")]
        if fading {
            let phys_loc: Point<i32, Physical> =
                geom_logical.loc.to_physical_precise_round(scale);
            let phys_size: Size<i32, Physical> =
                geom_logical.size.to_physical_precise_round(scale);
            if phys_size.w > 0 && phys_size.h > 0 {
                elems.push(RwlRenderElement::Border(SolidColorRenderElement::new(
                    fade_damage_id,
                    Rectangle::new(phys_loc, phys_size),
                    fade_damage_counter,
                    [0.0, 0.0, 0.0, 0.0],
                    Kind::Unspecified,
                )));
            }
        }

        // Slide sentinel: forces repaint of the layout-position area.
        #[cfg(feature = "tag-transition")]
        if sliding {
            let phys_loc: Point<i32, Physical> =
                geom_logical.loc.to_physical_precise_round(scale);
            let phys_size: Size<i32, Physical> =
                geom_logical.size.to_physical_precise_round(scale);
            if phys_size.w > 0 && phys_size.h > 0 {
                elems.push(RwlRenderElement::Border(SolidColorRenderElement::new(
                    slide_dmg_id,
                    Rectangle::new(phys_loc, phys_size),
                    slide_dmg_counter,
                    [0.0, 0.0, 0.0, 0.0],
                    Kind::Unspecified,
                )));
            }
        }

        // Emit popup surface elements FIRST (they are frontmost).
        // Popups are rendered without the rounded-corner clip — they can
        // visually extend beyond the window's rectangular boundary.
        for (popup, popup_offset) in PopupManager::popups_for_surface(&wl_surf) {
            let popup_render_loc = render_loc
                + (w.geometry().loc + popup_offset - popup.geometry().loc)
                    .to_physical_precise_round(scale);
            let popup_elems: Vec<WaylandSurfaceRenderElement<GlesRenderer>> =
                render_elements_from_surface_tree(
                    renderer,
                    popup.wl_surface(),
                    popup_render_loc,
                    scale,
                    alpha,
                    Kind::Unspecified,
                );
            for e in popup_elems {
                elems.push(RwlRenderElement::Cursor(e));
            }
        }

        if is_fullscreen {
            // Fullscreen windows are not rounded but still need to be drawn.
            let surface_elems: Vec<WaylandSurfaceRenderElement<GlesRenderer>> =
                render_elements_from_surface_tree(
                    renderer,
                    &wl_surf,
                    render_loc,
                    scale,
                    alpha,
                    Kind::Unspecified,
                );
            for e in surface_elems {
                elems.push(RwlRenderElement::Cursor(e));
            }
            continue;
        }

        // Use pending_geom for shader uniforms: the committed buffer size from
        // space.element_geometry can lag behind during resize, producing an
        // off-screen corner_y that clips the entire window transparent.
        let geom_for_shader = with_state(w, |s| s.pending_geom)
            .flatten()
            .unwrap_or(geom_logical);
        let win_phys_loc: Point<i32, Physical> = geom_for_shader.loc.to_physical_precise_round(scale);
        let win_phys_size: Size<i32, Physical> = geom_for_shader.size.to_physical_precise_round(scale);

        // Both TTY and winit EGL use Transform::Normal + flip180: EGL surfaces on all
        // tested platforms have y_inverted=true behaviour (GL NDC y=+1 = visual bottom),
        // so flip180 maps phys_y=0 to NDC y=-1 = visual top.  gl_FragCoord.y therefore
        // equals phys_y numerically → y_inverted=true for both backends.
        // y_inverted=true → corner_y = phys_loc.y
        // y_inverted=false → corner_y = output_h - phys_loc.y - phys_h (unused currently)
        let corner_x = {
            let base = win_phys_loc.x as f32;
            #[cfg(feature = "tag-transition")]
            { base + slide_x_px as f32 }
            #[cfg(not(feature = "tag-transition"))]
            { base }
        };
        let corner_y = if y_inverted {
            win_phys_loc.y as f32
        } else {
            output_h as f32 - win_phys_loc.y as f32 - win_phys_size.h as f32
        };

        let corner_w = win_phys_size.w as f32;
        let corner_h = win_phys_size.h as f32;

        let surface_elems: Vec<WaylandSurfaceRenderElement<GlesRenderer>> =
            render_elements_from_surface_tree(
                renderer,
                &wl_surf,
                render_loc,
                scale,
                alpha,
                Kind::Unspecified,
            );
        for inner in surface_elems {
            elems.push(RwlRenderElement::Rounded(RoundedWindowElem {
                inner,
                corner_x,
                corner_y,
                corner_w,
                corner_h,
                corner_r,
                program: program.clone(),
            }));
        }
    }

    Some(elems)
}

// ---------------------------------------------------------------------------
// Element-building helpers — shared between udev and winit backends
// ---------------------------------------------------------------------------

/// Build window render elements using the plain (no-rounded-corners) path.
///
/// When the `fade` feature is enabled this calls [`window_surface_elements`]
/// so per-window alpha is applied; otherwise it uses
/// `Space::render_elements_for_region` which is slightly cheaper.
pub fn build_window_elements_plain(
    renderer: &mut GlesRenderer,
    space: &Space<Window>,
    output: &Output,
    scale: f64,
) -> Vec<RwlRenderElement> {
    #[cfg(any(feature = "fade", feature = "tag-transition"))]
    { window_surface_elements(renderer, space, output, scale) }
    #[cfg(not(any(feature = "fade", feature = "tag-transition")))]
    {
        let output_geo = space.output_geometry(output).unwrap_or_default();
        space.render_elements_for_region(renderer, &output_geo, scale, 1.0)
            .into_iter()
            .map(|e| RwlRenderElement::from(SpaceRenderElements::Element(Wrap::from(e))))
            .collect()
    }
}

/// Build window render elements using the rounded-corners path, falling back
/// to the plain path when `rounded` is `None` or the corner shader is absent.
#[cfg(feature = "rounded-corners")]
pub fn build_window_elements_rounded(
    renderer: &mut GlesRenderer,
    space: &Space<Window>,
    output: &Output,
    scale: f64,
    rounded: Option<&RoundedCornerState>,
    y_inverted: bool,
) -> Vec<RwlRenderElement> {
    let corner_radius = crate::config::get().corner_radius;
    if let Some(state) = rounded
        && let Some(prog) = state.corner_shader.as_ref()
        && let Some(elems) = rounded_window_elements(
            renderer, space, output, scale,
            &RoundedWindowCtx { program: prog, corner_radius, y_inverted },
        )
    {
        return elems;
    }
    build_window_elements_plain(renderer, space, output, scale)
}

/// Build border render elements using the plain (flat solid-colour) path.
pub fn build_border_elements_plain(
    space: &Space<Window>,
    focused: Option<&Window>,
    scale: f64,
    visible_count: usize,
    has_fullscreen: bool,
) -> Vec<RwlRenderElement> {
    if has_fullscreen {
        return Vec::new();
    }
    border_elements(space, focused, scale, visible_count)
        .into_iter()
        .map(RwlRenderElement::from)
        .collect()
}

/// Build border render elements using the rounded-border-ring path, falling
/// back to flat strips when `rounded` is `None` or the border shader is absent.
#[cfg(feature = "rounded-corners")]
#[allow(clippy::too_many_arguments)]
pub fn build_border_elements_rounded(
    renderer: &mut GlesRenderer,
    space: &Space<Window>,
    focused: Option<&Window>,
    output: &Output,
    scale: f64,
    rounded: Option<&RoundedCornerState>,
    y_inverted: bool,
    visible_count: usize,
    has_fullscreen: bool,
) -> Vec<RwlRenderElement> {
    if has_fullscreen {
        return Vec::new();
    }
    let corner_radius = crate::config::get().corner_radius;
    if corner_radius > 0
        && let Some(state) = rounded
        && let Some(prog) = state.border_shader.as_ref()
    {
        let ctx = RoundedBorderCtx {
            program: prog,
            dummy_buffer: &state.dummy_border_buffer,
            corner_radius,
            y_inverted,
        };
        return rounded_border_elements(renderer, space, focused, output, scale, &ctx, visible_count)
            .into_iter()
            .map(RwlRenderElement::RoundedBorder)
            .collect();
    }
    // Shader unavailable or corner_radius == 0: fall back to flat strips.
    build_border_elements_plain(space, focused, scale, visible_count, false)
}
