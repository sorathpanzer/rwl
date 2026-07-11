//! Rounded-corner shader elements.
//!
//! Compiled only when the `rounded-corners` Cargo feature is enabled.
//! All public types are re-exported from `crate::render` so backends only
//! need one import path.

#![allow(
    clippy::wildcard_imports,
    clippy::missing_panics_doc,
    clippy::exhaustive_enums,
)]

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::memory::{
    MemoryRenderBuffer, MemoryRenderBufferRenderElement,
};
use smithay::backend::renderer::element::surface::WaylandSurfaceRenderElement;
use smithay::backend::renderer::element::{Element, Id, Kind, RenderElement, UnderlyingStorage};
use smithay::backend::renderer::gles::{
    GlesError, GlesFrame, GlesRenderer, GlesTexProgram, Uniform, UniformName, UniformType,
};
use smithay::backend::renderer::utils::{CommitCounter, DamageSet, OpaqueRegions};
use smithay::desktop::space::Space;
use smithay::desktop::Window;
use smithay::output::Output;
use smithay::utils::{
    Buffer as BufferCoords, Logical, Physical, Point, Rectangle, Scale, Transform,
};
use smithay::utils::user_data::UserDataMap;

use crate::window::{with_state, window_is_floating};

// ─── GLSL shader sources ──────────────────────────────────────────────────────

const ROUNDED_CORNER_FRAG: &str = r"
//_DEFINES_
#if defined(EXTERNAL)
#extension GL_OES_EGL_image_external : require
#endif
precision highp float;
#if defined(EXTERNAL)
uniform samplerExternalOES tex;
#else
uniform sampler2D tex;
#endif
uniform float alpha;
varying vec2 v_coords;
uniform float u_corner_x;
uniform float u_corner_y;
uniform float u_corner_w;
uniform float u_corner_h;
uniform float u_corner_r;
#if defined(DEBUG_FLAGS)
uniform float tint;
#endif
float roundedBoxSDF(vec2 p, vec2 b, float r) {
    vec2 q = abs(p) - b + r;
    return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - r;
}
void main() {
    vec4 color = texture2D(tex, v_coords);
    float lx = gl_FragCoord.x - u_corner_x;
    float ly = gl_FragCoord.y - u_corner_y;
    vec2 half_size = vec2(u_corner_w, u_corner_h) * 0.5;
    float dist = roundedBoxSDF(vec2(lx, ly) - half_size, half_size, u_corner_r);
    float mask = 1.0 - smoothstep(-0.5, 0.5, dist);
#if defined(NO_ALPHA)
    color = vec4(color.rgb, mask) * alpha;
#else
    color *= mask * alpha;
#endif
#if defined(DEBUG_FLAGS)
    if (tint == 1.0) color = vec4(0.0, 0.2, 0.0, 0.2) + color * 0.8;
#endif
    gl_FragColor = color;
}
";

/// GLSL fragment shader that draws a solid-colour rounded rectangle ring —
/// the area between an outer rounded rect and an inner rounded rect.
const ROUNDED_BORDER_FRAG: &str = r"
//_DEFINES_
#if defined(EXTERNAL)
#extension GL_OES_EGL_image_external : require
#endif
precision highp float;
#if defined(EXTERNAL)
uniform samplerExternalOES tex;
#else
uniform sampler2D tex;
#endif
uniform float alpha;
varying vec2 v_coords;
uniform float u_outer_x;
uniform float u_outer_y;
uniform float u_outer_w;
uniform float u_outer_h;
uniform float u_bw;
uniform float u_radius_outer;
uniform float u_radius_inner;
uniform float u_color_r;
uniform float u_color_g;
uniform float u_color_b;
uniform float u_color_a;
#if defined(DEBUG_FLAGS)
uniform float tint;
#endif
float roundedBoxSDF(vec2 p, vec2 b, float r) {
    vec2 q = abs(p) - b + r;
    return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - r;
}
void main() {
    float lx = gl_FragCoord.x - u_outer_x;
    float ly = gl_FragCoord.y - u_outer_y;
    vec2 outer_half = vec2(u_outer_w, u_outer_h) * 0.5;
    vec2 inner_half = outer_half - vec2(u_bw);
    float outer_dist = roundedBoxSDF(vec2(lx, ly) - outer_half, outer_half, u_radius_outer);
    float inner_dist = roundedBoxSDF(vec2(lx, ly) - outer_half, inner_half, u_radius_inner);
    float in_outer = 1.0 - smoothstep(-0.5, 0.5, outer_dist);
    float out_inner = smoothstep(-0.5, 0.5, inner_dist);
    float mask = in_outer * out_inner;
    // Premultiply alpha: GLES uses premultiplied blending (GL_ONE, GL_ONE_MINUS_SRC_ALPHA).
    vec4 premult = vec4(u_color_r * u_color_a, u_color_g * u_color_a, u_color_b * u_color_a, u_color_a);
    vec4 color = premult * mask;
    // Anchor tex so strict Mesa GLSL linkers keep the sampler uniform live.
    color += texture2D(tex, v_coords) * 0.0;
#if defined(DEBUG_FLAGS)
    if (tint == 1.0) color = vec4(0.0, 0.0, 0.2, 0.2) + color * 0.8;
#endif
    gl_FragColor = color * alpha;
}
";

// ─── Shader compilation ───────────────────────────────────────────────────────

fn compile_rounded_corner_shader(renderer: &mut GlesRenderer) -> Option<GlesTexProgram> {
    let uniforms = [
        UniformName::new("u_corner_x", UniformType::_1f),
        UniformName::new("u_corner_y", UniformType::_1f),
        UniformName::new("u_corner_w", UniformType::_1f),
        UniformName::new("u_corner_h", UniformType::_1f),
        UniformName::new("u_corner_r", UniformType::_1f),
    ];
    match renderer.compile_custom_texture_shader(ROUNDED_CORNER_FRAG, &uniforms) {
        Ok(p) => Some(p),
        Err(e) => {
            tracing::warn!("rounded corner shader compile failed: {e}");
            None
        }
    }
}

fn compile_rounded_border_shader(renderer: &mut GlesRenderer) -> Option<GlesTexProgram> {
    let uniforms = [
        UniformName::new("u_outer_x",      UniformType::_1f),
        UniformName::new("u_outer_y",      UniformType::_1f),
        UniformName::new("u_outer_w",      UniformType::_1f),
        UniformName::new("u_outer_h",      UniformType::_1f),
        UniformName::new("u_bw",           UniformType::_1f),
        UniformName::new("u_radius_outer", UniformType::_1f),
        UniformName::new("u_radius_inner", UniformType::_1f),
        UniformName::new("u_color_r",      UniformType::_1f),
        UniformName::new("u_color_g",      UniformType::_1f),
        UniformName::new("u_color_b",      UniformType::_1f),
        UniformName::new("u_color_a",      UniformType::_1f),
    ];
    match renderer.compile_custom_texture_shader(ROUNDED_BORDER_FRAG, &uniforms) {
        Ok(p) => Some(p),
        Err(e) => {
            tracing::warn!("rounded border shader compile failed: {e}");
            None
        }
    }
}

fn make_border_dummy_buffer() -> MemoryRenderBuffer {
    MemoryRenderBuffer::from_slice(
        &[255u8, 255, 255, 255],
        Fourcc::Argb8888,
        (1, 1),
        1,
        Transform::Normal,
        None,
    )
}

// ─── Grouped backend state ────────────────────────────────────────────────────

/// All GPU-side state for rounded-corner rendering, held per backend device.
///
/// Backends store a single `rounded: RoundedCornerState` field instead of
/// three separate shader/buffer fields.
pub struct RoundedCornerState {
    /// Pre-compiled window-corner clipping shader (`None` = compile failed).
    pub corner_shader:       Option<GlesTexProgram>,
    /// Pre-compiled border-ring shader (`None` = compile failed).
    pub border_shader:       Option<GlesTexProgram>,
    /// 1×1 white dummy buffer used as the texture carrier for border elements.
    pub dummy_border_buffer: MemoryRenderBuffer,
}

impl RoundedCornerState {
    /// Compile shaders and create the dummy buffer on `renderer`.
    /// Called once per GPU device at backend initialisation time.
    pub fn init(renderer: &mut GlesRenderer) -> Self {
        Self {
            corner_shader:       compile_rounded_corner_shader(renderer),
            border_shader:       compile_rounded_border_shader(renderer),
            dummy_border_buffer: make_border_dummy_buffer(),
        }
    }
}

impl std::fmt::Debug for RoundedCornerState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RoundedCornerState")
            .field("corner_shader", &self.corner_shader.is_some())
            .field("border_shader", &self.border_shader.is_some())
            .finish_non_exhaustive()
    }
}

// ─── Context structs ──────────────────────────────────────────────────────────

/// Per-frame context for `rounded_window_elements`.
pub struct RoundedWindowCtx<'a> {
    /// Compiled corner-rounding texture shader.
    pub program:       &'a GlesTexProgram,
    /// Corner radius in logical pixels (from config).
    pub corner_radius: u32,
    /// Whether the framebuffer Y-axis is inverted (EGL convention).
    pub y_inverted:    bool,
}

/// Per-frame context for `rounded_border_elements`.
pub struct RoundedBorderCtx<'a> {
    /// Compiled border SDF shader.
    pub program:       &'a GlesTexProgram,
    /// 1×1 dummy buffer used to drive the shader element.
    pub dummy_buffer:  &'a MemoryRenderBuffer,
    /// Corner radius in logical pixels (from config).
    pub corner_radius: u32,
    /// Whether the framebuffer Y-axis is inverted (EGL convention).
    pub y_inverted:    bool,
}

// ─── RoundedWindowElem ────────────────────────────────────────────────────────

/// A texture render element decorated with per-frame corner-rounding uniforms.
/// During rendering it overrides the default texture program with the
/// pre-compiled rounded-corner shader. Generic over the wrapped element so it
/// can round both live window surfaces and scaled overview thumbnails.
pub struct RoundedElem<E> {
    pub(crate) inner:    E,
    /// Window bottom-left X in physical pixels (`gl_FragCoord` origin).
    pub(crate) corner_x: f32,
    /// Window bottom-left Y in physical pixels (`gl_FragCoord` origin).
    pub(crate) corner_y: f32,
    /// Window width in physical pixels.
    pub(crate) corner_w: f32,
    /// Window height in physical pixels.
    pub(crate) corner_h: f32,
    /// Corner radius in physical pixels.
    pub(crate) corner_r: f32,
    pub(crate) program:  GlesTexProgram,
}

/// Rounded live window surface (the common case).
pub type RoundedWindowElem = RoundedElem<WaylandSurfaceRenderElement<GlesRenderer>>;
/// Rounded overview thumbnail (a scaled/relocated/cropped window surface).
pub type RoundedThumbElem = RoundedElem<crate::render::OverviewThumb>;

impl<E> std::fmt::Debug for RoundedElem<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RoundedElem")
            .field("corner_x", &self.corner_x)
            .field("corner_y", &self.corner_y)
            .field("corner_w", &self.corner_w)
            .field("corner_h", &self.corner_h)
            .finish_non_exhaustive()
    }
}

impl<E: Element> Element for RoundedElem<E> {
    fn id(&self) -> &Id { self.inner.id() }
    fn current_commit(&self) -> CommitCounter { self.inner.current_commit() }
    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> { self.inner.geometry(scale) }
    fn src(&self) -> Rectangle<f64, BufferCoords> { self.inner.src() }
    fn damage_since(
        &self,
        scale: Scale<f64>,
        commit: Option<CommitCounter>,
    ) -> DamageSet<i32, Physical> {
        self.inner.damage_since(scale, commit)
    }
    fn opaque_regions(&self, _scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        // Rounded corners have transparent pixels — report no opaque region.
        OpaqueRegions::default()
    }
    fn alpha(&self) -> f32 { self.inner.alpha() }
    fn kind(&self) -> Kind { self.inner.kind() }
}

impl<E: RenderElement<GlesRenderer>> RenderElement<GlesRenderer> for RoundedElem<E> {
    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, BufferCoords>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
        cache: Option<&UserDataMap>,
    ) -> Result<(), GlesError> {
        let uniforms: Vec<Uniform<'static>> = vec![
            Uniform::new("u_corner_x", self.corner_x),
            Uniform::new("u_corner_y", self.corner_y),
            Uniform::new("u_corner_w", self.corner_w),
            Uniform::new("u_corner_h", self.corner_h),
            Uniform::new("u_corner_r", self.corner_r),
        ];
        frame.override_default_tex_program(self.program.clone(), uniforms);
        let result = RenderElement::<GlesRenderer>::draw(
            &self.inner, frame, src, dst, damage, opaque_regions, cache,
        );
        frame.clear_tex_program_override();
        result
    }

    fn underlying_storage(&self, _renderer: &mut GlesRenderer) -> Option<UnderlyingStorage<'_>> {
        // Prevent DRM hardware-plane direct scanout — corners must go through the shader.
        None
    }
}

// ─── RoundedBorderElem ────────────────────────────────────────────────────────

/// A render element that draws a solid-colour rounded rectangle ring around
/// one window, replacing the four flat `SolidColorRenderElement` strips.
pub struct RoundedBorderElem {
    pub(crate) id:           Id,
    pub(crate) commit:       CommitCounter,
    /// 1×1 dummy texture stretched to the outer border rect.
    pub(crate) inner:        MemoryRenderBufferRenderElement<GlesRenderer>,
    /// `gl_FragCoord` X of the outer rect's left edge.
    pub(crate) outer_x:      f32,
    /// `gl_FragCoord` Y of the outer rect's bottom edge (OpenGL Y-up).
    pub(crate) outer_y:      f32,
    pub(crate) outer_w:      f32,
    pub(crate) outer_h:      f32,
    /// Border width in physical pixels.
    pub(crate) bw:           f32,
    pub(crate) radius_outer: f32,
    pub(crate) radius_inner: f32,
    pub(crate) color:        [f32; 4],
    pub(crate) program:      GlesTexProgram,
}

impl std::fmt::Debug for RoundedBorderElem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RoundedBorderElem")
            .field("outer_x", &self.outer_x)
            .field("outer_y", &self.outer_y)
            .finish_non_exhaustive()
    }
}

impl Element for RoundedBorderElem {
    fn id(&self) -> &Id { &self.id }
    fn current_commit(&self) -> CommitCounter { self.commit }
    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> { self.inner.geometry(scale) }
    fn src(&self) -> Rectangle<f64, BufferCoords> { self.inner.src() }
    fn damage_since(
        &self,
        scale: Scale<f64>,
        _commit: Option<CommitCounter>,
    ) -> DamageSet<i32, Physical> {
        // Always report the full ring as damaged in *element-local* coordinates
        // (origin = (0, 0), the top-left of the element's bounding rect).
        // The damage tracker adds the element's output-space location on top of
        // whatever we return here, so returning self.inner.geometry() — which
        // already contains the absolute position — would double-count it and
        // shift the damage rectangle off the actual ring, causing the top-left
        // borders to be skipped by the scissor test.  Border rings are thin;
        // repainting them unconditionally on every frame is negligible overhead.
        DamageSet::from_slice(&[Rectangle::from_size(self.inner.geometry(scale).size)])
    }
    fn opaque_regions(&self, _scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        OpaqueRegions::default()
    }
    fn alpha(&self) -> f32 { 1.0 }
    fn kind(&self) -> Kind { Kind::Unspecified }
}

impl RenderElement<GlesRenderer> for RoundedBorderElem {
    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, BufferCoords>,
        dst: Rectangle<i32, Physical>,
        _damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
        cache: Option<&UserDataMap>,
    ) -> Result<(), GlesError> {
        let uniforms: Vec<Uniform<'static>> = vec![
            Uniform::new("u_outer_x",      self.outer_x),
            Uniform::new("u_outer_y",      self.outer_y),
            Uniform::new("u_outer_w",      self.outer_w),
            Uniform::new("u_outer_h",      self.outer_h),
            Uniform::new("u_bw",           self.bw),
            Uniform::new("u_radius_outer", self.radius_outer),
            Uniform::new("u_radius_inner", self.radius_inner),
            Uniform::new("u_color_r",      self.color[0]),
            Uniform::new("u_color_g",      self.color[1]),
            Uniform::new("u_color_b",      self.color[2]),
            Uniform::new("u_color_a",      self.color[3]),
        ];
        frame.override_default_tex_program(self.program.clone(), uniforms);
        // Always repaint the full ring bounding rect rather than the clipped
        // damage rects from the tracker.  The tracker subtracts opaque regions
        // of elements in front (bar, cursor), which can incorrectly exclude
        // the ring's top/left borders when area_x/area_y > 0.  Rings are thin
        // strips with negligible GPU cost, so full repaints are fine.
        let full_rect = [Rectangle::from_size(dst.size)];
        let result = RenderElement::<GlesRenderer>::draw(
            &self.inner, frame, src, dst, &full_rect, opaque_regions, cache,
        );
        frame.clear_tex_program_override();
        result
    }

    fn underlying_storage(&self, _renderer: &mut GlesRenderer) -> Option<UnderlyingStorage<'_>> {
        None
    }
}

// ─── rounded_border_elements — public builder ─────────────────────────────────

/// Build [`RoundedBorderElem`]s for all visible non-fullscreen windows,
/// replacing the flat `SolidColorRenderElement` border strips.
///
/// Returns an empty vec when `visible_count <= 1` or `corner_radius == 0`.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::too_many_lines,
)]
pub fn rounded_border_elements(
    renderer:      &mut GlesRenderer,
    space:         &Space<Window>,
    focused:       Option<&Window>,
    output:        &Output,
    scale:         f64,
    ctx:           &RoundedBorderCtx<'_>,
    visible_count: usize,
) -> Vec<RoundedBorderElem> {
    let (program, dummy_buffer, corner_radius, y_inverted) =
        (ctx.program, ctx.dummy_buffer, ctx.corner_radius, ctx.y_inverted);
    #[allow(clippy::cast_possible_wrap)]
    let (bw, focus_color, border_color) = {
        let cfg = crate::config::get();
        (cfg.border_px as i32, cfg.focus_color, cfg.border_color)
    };
    tracing::trace!(
        "rounded_border_elements: bw={bw} visible_count={visible_count} corner_radius={corner_radius}"
    );
    // No borders when only one tiled window is visible and nothing is floating.
    let any_floating = space.elements().any(window_is_floating);
    if bw == 0 || corner_radius == 0 || (visible_count <= 1 && !any_floating) {
        return Vec::new();
    }

    let output_h = output.current_mode().map_or(0, |m| m.size.h);
    let bw_phys = bw as f32 * scale as f32;
    let corner_r = corner_radius as f32 * scale as f32;
    let mut elems = Vec::new();

    // Helper: build a RoundedBorderElem for one window with the given color.
    // Returns None if the window has no geometry or the buffer import fails.
    let mut make_elem = |w: &Window, color: [f32; 4]| -> Option<RoundedBorderElem> {
        let space_geom = space.element_geometry(w);
        let pending = with_state(w, |s| s.pending_geom).flatten();
        tracing::trace!(
            "border_elem: appid={:?} space_geom={:?} pending={:?}",
            with_state(w, |s| s.last_appid.clone()),
            space_geom,
            pending,
        );
        let geom = space_geom.or(pending)?;

        let (id, counter) = with_state(w, |s| {
            (s.border_ids[0].clone(), s.border_counter)
        }).unwrap_or_else(|| (Id::new(), CommitCounter::default()));

        #[cfg(feature = "tag-transition")]
        let slide_x_px: i32 =
            crate::window::with_state(w, |s| (s.slide_offset_x * scale).round() as i32).unwrap_or(0);

        let outer_logical: Rectangle<i32, Logical> = Rectangle::new(
            Point::from((geom.loc.x - bw, geom.loc.y - bw)),
            smithay::utils::Size::from((geom.size.w + bw * 2, geom.size.h + bw * 2)),
        );
        let outer_phys_loc: Point<i32, Physical> =
            outer_logical.loc.to_physical_precise_round(scale);
        #[cfg(feature = "tag-transition")]
        let outer_phys_loc: Point<i32, Physical> =
            Point::from((outer_phys_loc.x + slide_x_px, outer_phys_loc.y));
        let outer_phys_size: smithay::utils::Size<i32, Physical> =
            outer_logical.size.to_physical_precise_round(scale);

        if outer_phys_size.w <= 0 || outer_phys_size.h <= 0 { return None; }

        let outer_gl_x = outer_phys_loc.x as f32;
        let outer_gl_y = if y_inverted {
            outer_phys_loc.y as f32
        } else {
            output_h as f32 - outer_phys_loc.y as f32 - outer_phys_size.h as f32
        };
        tracing::trace!(
            "border_elem: outer_logical={:?} outer_phys=({},{})+({}×{}) gl_x={} gl_y={} bw={} r={}",
            outer_logical,
            outer_phys_loc.x, outer_phys_loc.y,
            outer_phys_size.w, outer_phys_size.h,
            outer_gl_x, outer_gl_y,
            bw_phys,
            corner_r,
        );

        let inner_elem = match MemoryRenderBufferRenderElement::from_buffer(
            renderer,
            outer_phys_loc.to_f64(),
            dummy_buffer,
            None,
            None,
            Some(outer_logical.size),
            Kind::Unspecified,
        ) {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!("border dummy buffer import failed: {e}");
                return None;
            }
        };

        tracing::trace!(
            "border_elem: pushing element at phys_loc=({},{}) size={}×{}",
            outer_phys_loc.x, outer_phys_loc.y,
            outer_phys_size.w, outer_phys_size.h,
        );
        Some(RoundedBorderElem {
            id,
            commit: counter,
            inner: inner_elem,
            outer_x: outer_gl_x,
            outer_y: outer_gl_y,
            outer_w: outer_phys_size.w as f32,
            outer_h: outer_phys_size.h as f32,
            bw: bw_phys,
            radius_outer: corner_r + bw_phys,
            radius_inner: corner_r,
            color,
            program: program.clone(),
        })
    };

    // Non-focused windows first, focused window last — ensures focus_color ring
    // is always rendered on top when adjacent windows' rings overlap at shared edges.
    for w in space.elements() {
        if with_state(w, |s| s.is_fullscreen).unwrap_or(false) { continue; }
        if Some(w) == focused { continue; }
        if border_color[3] < 0.001 { continue; }
        if let Some(elem) = make_elem(w, border_color) {
            elems.push(elem);
        }
    }

    if let Some(w) = focused
        && !with_state(w, |s| s.is_fullscreen).unwrap_or(false)
        && let Some(elem) = make_elem(w, focus_color)
    {
        elems.push(elem);
    }

    elems
}

// ─── Shared thumbnail rounding (overview + PiP) ───────────────────────────────

/// Build a [`crate::render::ThumbRound`] from the backend's compiled shaders
/// (`None` when a shader failed to compile or `corner_radius == 0`).
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
pub fn thumb_round(
    rounded: &RoundedCornerState,
    scale: f64,
    y_inverted: bool,
) -> Option<crate::render::ThumbRound<'_>> {
    let radius = crate::config::get().corner_radius;
    if radius == 0 {
        return None;
    }
    Some(crate::render::ThumbRound {
        corner_shader: rounded.corner_shader.as_ref()?,
        border_shader: rounded.border_shader.as_ref()?,
        dummy_buffer: &rounded.dummy_border_buffer,
        radius_px: radius as f32 * scale as f32,
        y_inverted,
    })
}

/// Corner-shader uniforms (`corner_x/y/w/h`, physical px) for a thumbnail at
/// `rect`. `gl_FragCoord`'s origin is the framebuffer bottom-left, so Y is
/// flipped unless the framebuffer is already Y-inverted.
#[allow(clippy::cast_precision_loss)]
pub fn thumb_corners(
    rect: Rectangle<i32, Logical>,
    y_inverted: bool,
    output_h_phys: i32,
    scale: f64,
) -> (f32, f32, f32, f32) {
    let loc: Point<i32, Physical> = rect.loc.to_physical_precise_round(scale);
    let size: smithay::utils::Size<i32, Physical> = rect.size.to_physical_precise_round(scale);
    let cx = loc.x as f32;
    let cy = if y_inverted {
        loc.y as f32
    } else {
        (output_h_phys - loc.y - size.h) as f32
    };
    (cx, cy, size.w as f32, size.h as f32)
}

/// Build a rounded border ring around a thumbnail `rect` (matching the
/// thumbnail's corner radius), reusing the rounded-border shader. `id` is the
/// stable damage id to attach to the ring.
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::too_many_arguments)]
pub fn rounded_thumb_ring(
    renderer: &mut GlesRenderer,
    id: &Id,
    rect: Rectangle<i32, Logical>,
    color: [f32; 4],
    bw: i32,
    round: &crate::render::ThumbRound<'_>,
    output_h_phys: i32,
    scale: f64,
    counter: CommitCounter,
) -> Option<RoundedBorderElem> {
    let outer = Rectangle::new(
        Point::from((rect.loc.x - bw, rect.loc.y - bw)),
        smithay::utils::Size::from((rect.size.w + bw * 2, rect.size.h + bw * 2)),
    );
    let outer_loc: Point<i32, Physical> = outer.loc.to_physical_precise_round(scale);
    let outer_size: smithay::utils::Size<i32, Physical> = outer.size.to_physical_precise_round(scale);
    if outer_size.w <= 0 || outer_size.h <= 0 {
        return None;
    }
    let bw_phys = bw as f32 * scale as f32;
    let outer_x = outer_loc.x as f32;
    let outer_y = if round.y_inverted {
        outer_loc.y as f32
    } else {
        (output_h_phys - outer_loc.y - outer_size.h) as f32
    };

    let inner = MemoryRenderBufferRenderElement::from_buffer(
        renderer,
        outer_loc.to_f64(),
        round.dummy_buffer,
        None,
        None,
        Some(outer.size),
        Kind::Unspecified,
    )
    .ok()?;

    Some(RoundedBorderElem {
        id: id.clone(),
        commit: counter,
        inner,
        outer_x,
        outer_y,
        outer_w: outer_size.w as f32,
        outer_h: outer_size.h as f32,
        bw: bw_phys,
        radius_outer: round.radius_px + bw_phys,
        radius_inner: round.radius_px,
        color,
        program: round.border_shader.clone(),
    })
}
