//! Native desktop wallpaper.
//!
//! Decodes an image file (via the `image` crate) into a CPU-side
//! [`MemoryRenderBuffer`] and composites it behind every window and layer-shell
//! surface. Each decoded buffer is reused across frames so its render-element id
//! and commit counter stay stable — the damage tracker then treats the
//! wallpaper as unchanged and never repaints it on its own.
//!
//! The effective wallpaper is the runtime **override** ([`set_override`], set by
//! `rwl msg wallpaper` or the `rwl.set_wallpaper` Lua hook) if one exists, else
//! the static `wallpaper.default` config value. The override lives outside the
//! reloadable [`crate::config::Config`], so a hook-driven / per-tag wallpaper
//! survives a config reload instead of vanishing.
//!
//! Decoded images are kept in a small per-thread LRU ([`CACHE`]) keyed by path,
//! so revisiting a tag whose wallpaper was already shown does not pay the decode
//! cost again.

use std::borrow::Cow;
use std::cell::RefCell;
use std::sync::RwLock;

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::memory::{
    MemoryRenderBuffer, MemoryRenderBufferRenderElement,
};
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::output::Output;
use smithay::reexports::calloop::channel::{Event, Sender, channel};
use smithay::reexports::calloop::LoopHandle;
use smithay::utils::{Logical, Physical, Point, Rectangle, Size, Transform};

/// How the wallpaper image is fitted to each output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WallpaperMode {
    /// Scale to cover the whole output, cropping the overflow. Preserves aspect.
    #[default]
    Fill,
    /// Scale to fit inside the output, letterboxing the remainder. Preserves aspect.
    Fit,
    /// Stretch to exactly the output size, ignoring aspect ratio.
    Stretch,
    /// Draw at native size, centred, with no scaling.
    Center,
}

impl WallpaperMode {
    /// Parse a config string (`fill` / `fit` / `stretch` / `center`).
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "fill"    => Some(Self::Fill),
            "fit"     => Some(Self::Fit),
            "stretch" => Some(Self::Stretch),
            "center"  => Some(Self::Center),
            _         => None,
        }
    }
}

// ─── runtime override (survives config reload) ───────────────────────────────

/// A runtime wallpaper selection set outside the reloadable config.
#[derive(Clone)]
struct Setting {
    /// Image path (`None` = wallpaper explicitly cleared).
    path: Option<String>,
    /// Fit mode.
    mode: WallpaperMode,
}

/// The runtime override. `None` = no override (fall back to `wallpaper.default`).
/// Holds only plain data (`String` + enum), so it is safely `Send + Sync` for a
/// global `static`, unlike the [`MemoryRenderBuffer`] cache.
static OVERRIDE: RwLock<Option<Setting>> = RwLock::new(None);

/// Set the runtime wallpaper override (from `rwl msg wallpaper` or the
/// `rwl.set_wallpaper` Lua hook). Unlike the static `wallpaper.default` config
/// value this is **not** cleared by a config reload, so a hook-driven / per-tag
/// wallpaper stays put when the Lua config is re-read. A `None` path clears the
/// wallpaper; a `None` mode keeps the currently effective fit mode.
pub fn set_override(path: Option<String>, mode: Option<WallpaperMode>) {
    let Ok(mut guard) = OVERRIDE.write() else { return };
    let cur_mode = guard
        .as_ref()
        .map_or_else(|| crate::config::get().wallpaper_mode, |s| s.mode);
    *guard = Some(Setting { path, mode: mode.unwrap_or(cur_mode) });
}

/// The effective wallpaper for a monitor showing tag-mask `tags`, in order of
/// precedence:
/// 1. the runtime override (`rwl msg` / `rwl.set_wallpaper`), if set;
/// 2. the per-tag `wallpaper.tags[tag]` entry for the lowest visible tag
///    that has one;
/// 3. the single `wallpaper.default` fallback.
fn effective(tags: u32) -> (Option<String>, WallpaperMode) {
    if let Ok(guard) = OVERRIDE.read()
        && let Some(s) = guard.as_ref()
    {
        return (s.path.clone(), s.mode);
    }
    let cfg = crate::config::get();
    for (i, entry) in cfg.wallpapers.iter().enumerate().take(32) {
        if tags & (1u32 << i) != 0
            && let Some(path) = entry
        {
            return (Some(path.clone()), cfg.wallpaper_mode);
        }
    }
    (cfg.wallpaper.clone(), cfg.wallpaper_mode)
}

/// Every wallpaper path referenced by the config (per-tag entries plus the
/// single fallback), de-duplicated implicitly by [`request_decode`].
fn configured_paths() -> Vec<String> {
    let cfg = crate::config::get();
    cfg.wallpapers
        .iter()
        .flatten()
        .cloned()
        .chain(cfg.wallpaper.clone())
        .collect()
}

/// Decode and GPU-warm every wallpaper named in the config, on background
/// threads. Called at startup and after a config reload so switching to any tag
/// is instant — no per-config Lua `preload` calls required. Idempotent.
pub fn preload_configured() {
    for path in configured_paths() {
        request_decode(path);
    }
}

/// Called on config reload: forget which paths previously failed to decode so a
/// path the user has since fixed is retried, then preload the (possibly new) set
/// of configured wallpapers. Successfully-decoded images are kept so an unchanged
/// wallpaper is not needlessly re-decoded.
pub fn on_reload() {
    FAILED.with(|f| f.borrow_mut().clear());
    preload_configured();
}

// ─── background decoding (preload) ────────────────────────────────────────────

/// A decoded image handed from a background decode thread back to the render
/// thread. Carries raw RGBA bytes (`Send`) — the [`MemoryRenderBuffer`] itself
/// is built on the render thread, where it is used.
struct Decoded {
    path: String,
    rgba: Vec<u8>,
    size: (i32, i32),
}

thread_local! {
    /// Sender half of the decode-result channel, set by [`init_decoder`]. Cloned
    /// into each background decode thread.
    static RESULT_TX: RefCell<Option<Sender<Decoded>>> = const { RefCell::new(None) };
    /// Paths currently being decoded in the background, so [`request_decode`]
    /// does not spawn a duplicate thread for the same image.
    static PENDING: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

/// Wire up the background wallpaper decoder: a calloop channel whose decoded
/// results are turned into cached buffers on the render thread. Call once at
/// startup, before any wallpaper is requested.
pub fn init_decoder(handle: &LoopHandle<'static, crate::state::Rwl>) {
    let (tx, rx) = channel::<Decoded>();
    RESULT_TX.with(|c| *c.borrow_mut() = Some(tx));
    let inserted = handle.insert_source(rx, |event, (), state| {
        let Event::Msg(dec) = event else { return };
        PENDING.with(|p| p.borrow_mut().retain(|x| *x != dec.path));
        let buffer = MemoryRenderBuffer::from_slice(
            &dec.rgba, Fourcc::Abgr8888, dec.size, 1, Transform::Normal, None,
        );
        // Upload the GL texture now, off the tag-switch critical path. Smithay
        // otherwise imports the texture lazily on first render, so the large
        // upload would stall the frame that first shows the wallpaper.
        let t = std::time::Instant::now();
        warm_texture(state, &buffer);
        tracing::debug!("wallpaper: uploaded {} in {} ms", dec.path, t.elapsed().as_millis());
        CACHE.with(|cell| {
            let mut cache = cell.borrow_mut();
            if !cache.iter().any(|l| l.path == dec.path) {
                if cache.len() >= CACHE_CAP {
                    cache.remove(0);
                }
                cache.push(Loaded { path: dec.path.clone(), buffer, size: dec.size });
            }
        });
        // A newly-decoded wallpaper may be the one currently shown (or about to
        // be), so repaint. Cheap: one frame, and the damage tracker no-ops if the
        // shown wallpaper did not actually change.
        state.schedule_render();
    });
    if let Err(e) = inserted {
        tracing::warn!("wallpaper: failed to register decode channel: {e}");
    }
}

/// Force the GPU texture upload for `buffer` on every live `GlesRenderer` now, so
/// the (potentially large) upload is not paid on the first frame that shows the
/// wallpaper. Building a throwaway render element triggers the internal
/// `import_texture`, which uploads and caches the texture keyed by renderer.
fn warm_texture(state: &mut crate::state::Rwl, buffer: &MemoryRenderBuffer) {
    let warm = |renderer: &mut GlesRenderer| {
        let _ = MemoryRenderBufferRenderElement::from_buffer(
            renderer, (0.0, 0.0), buffer, None, None, None, Kind::Unspecified,
        );
    };
    match &mut state.backend {
        #[cfg(feature = "winit")]
        Some(crate::backend::BackendData::Winit(data)) => warm(data.gfx.renderer()),
        Some(crate::backend::BackendData::Udev(udev)) => {
            for dev in udev.devices.values_mut() {
                warm(&mut dev.renderer);
            }
        }
        _ => {}
    }
}

/// Warm the cache for `path` on a background thread (non-blocking). No-op if the
/// image is already cached, already being decoded, or previously failed, or if
/// [`init_decoder`] has not run yet. Lets `on_startup` preload every per-tag
/// wallpaper so the first switch to each tag is instant.
pub fn request_decode(path: String) {
    if CACHE.with(|c| c.borrow().iter().any(|l| l.path == path))
        || PENDING.with(|p| p.borrow().contains(&path))
        || FAILED.with(|f| f.borrow().contains(&path))
    {
        return;
    }
    let Some(tx) = RESULT_TX.with(|c| c.borrow().clone()) else { return };
    PENDING.with(|p| p.borrow_mut().push(path.clone()));
    std::thread::spawn(move || {
        let resolved = resolve_path(&path);
        let t = std::time::Instant::now();
        match image::open(resolved.as_ref()) {
            Ok(img) => {
                let img = img.into_rgba8();
                if let (Ok(w), Ok(h)) =
                    (i32::try_from(img.width()), i32::try_from(img.height()))
                {
                    tracing::debug!(
                        "wallpaper: decoded {path} in {} ms ({w}x{h})", t.elapsed().as_millis()
                    );
                    let _ = tx.send(Decoded { path, rgba: img.into_raw(), size: (w, h) });
                }
            }
            Err(e) => tracing::warn!("wallpaper: preload failed for {resolved}: {e}"),
        }
    });
}

/// Placement of the wallpaper buffer for one output: physical location, an
/// optional source crop (buffer-logical), and an optional logical destination
/// size (scaled by the render scale at draw time).
type Placement = (
    Point<f64, Physical>,
    Option<Rectangle<f64, Logical>>,
    Option<Size<i32, Logical>>,
);

/// A decoded wallpaper kept in the render thread's cache.
struct Loaded {
    /// The configured path (unexpanded) this buffer was decoded from.
    path:   String,
    /// CPU-side ARGB buffer; reused across frames for stable damage.
    buffer: MemoryRenderBuffer,
    /// Intrinsic image size in pixels.
    size:   (i32, i32),
}

/// How many decoded wallpapers to keep in memory at once. Each entry costs
/// `width × height × 4` bytes (e.g. ~33 MB for a 4K image), so this is a
/// deliberate cap: enough to cover a handful of per-tag wallpapers without the
/// working set growing without bound. Oldest entry is evicted first (FIFO).
const CACHE_CAP: usize = 6;

thread_local! {
    /// Per-render-thread decode cache, newest-pushed last. The compositor
    /// renders on a single thread, so a `thread_local` avoids the `Send + Sync`
    /// bound a global `static` would impose on [`MemoryRenderBuffer`].
    static CACHE: RefCell<Vec<Loaded>> = const { RefCell::new(Vec::new()) };
    /// Paths whose decode failed, so a broken/missing path is not retried (and
    /// re-logged) every frame. Cleared on config reload by [`on_reload`].
    static FAILED: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

/// Expand a leading `~/` to `$HOME`.
fn resolve_path(path: &str) -> Cow<'_, str> {
    if let Some(rest) = path.strip_prefix("~/")
        && let Ok(home) = std::env::var("HOME")
    {
        return Cow::Owned(format!("{home}/{rest}"));
    }
    Cow::Borrowed(path)
}

/// Decode `path` into an RGBA [`MemoryRenderBuffer`]. Returns `None` (after
/// logging) when the file cannot be read or decoded.
fn decode(path: &str) -> Option<Loaded> {
    let resolved = resolve_path(path);
    let img = match image::open(resolved.as_ref()) {
        Ok(img) => img.into_rgba8(),
        Err(e) => {
            tracing::warn!("wallpaper: failed to load {resolved}: {e}");
            return None;
        }
    };
    let iw = i32::try_from(img.width()).ok()?;
    let ih = i32::try_from(img.height()).ok()?;
    // `image` stores RGBA8 as bytes [R, G, B, A]; on little-endian that packs to
    // the u32 0xAABBGGRR, which is `Fourcc::Abgr8888`.
    let buffer = MemoryRenderBuffer::from_slice(
        img.as_raw(),
        Fourcc::Abgr8888,
        (iw, ih),
        1,
        Transform::Normal,
        None,
    );
    tracing::debug!("wallpaper: loaded {resolved} ({iw}x{ih})");
    Some(Loaded { path: path.to_owned(), buffer, size: (iw, ih) })
}

/// Build the wallpaper render element(s) for `output`, whose monitor is showing
/// tag-mask `tags`.
///
/// Uses the effective wallpaper for `tags` (runtime override → per-tag
/// `wallpaper.tags` → `wallpaper.default` fallback), decoding on first use
/// and reusing the cached buffer thereafter. Returns an empty vec when no
/// wallpaper applies or the image failed to decode. Elements are backmost (drawn
/// last in the front-to-back list).
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
)]
pub fn elements(
    renderer: &mut GlesRenderer,
    output: &Output,
    scale: f64,
    tags: u32,
) -> Vec<MemoryRenderBufferRenderElement<GlesRenderer>> {
    let (Some(path), mode) = effective(tags) else { return Vec::new() };

    let Some((ow, oh)) = output.current_mode().map(|m| (f64::from(m.size.w), f64::from(m.size.h)))
    else {
        return Vec::new();
    };
    if ow <= 0.0 || oh <= 0.0 || scale <= 0.0 {
        return Vec::new();
    }

    CACHE.with(|cell| {
        let mut cache = cell.borrow_mut();
        // Decode on first use; cached buffers are reused across frames and across
        // tag revisits so the decode cost is paid at most once per image.
        if !cache.iter().any(|l| l.path == path) {
            if FAILED.with(|f| f.borrow().contains(&path)) {
                return Vec::new();
            }
            let Some(loaded) = decode(&path) else {
                FAILED.with(|f| f.borrow_mut().push(path.clone()));
                return Vec::new();
            };
            if cache.len() >= CACHE_CAP {
                cache.remove(0);
            }
            cache.push(loaded);
        }
        let Some(loaded) = cache.iter().find(|l| l.path == path) else { return Vec::new() };
        let (iwi, ihi) = loaded.size;
        if iwi <= 0 || ihi <= 0 {
            return Vec::new();
        }
        let (iw, ih) = (f64::from(iwi), f64::from(ihi));

        // `location` is output-local physical pixels; `size` is *logical* and is
        // multiplied by the render `scale` at draw time, so a target physical
        // extent `p` is requested as a logical `p / scale`. `src` selects a
        // sub-rectangle of the buffer (buffer_scale 1 → its logical units are
        // pixels).
        let logical_size = |pw: f64, ph: f64| Size::<i32, Logical>::from((
            (pw / scale).round() as i32,
            (ph / scale).round() as i32,
        ));

        let (loc, src, dst): Placement = match mode {
            WallpaperMode::Stretch => (
                Point::from((0.0, 0.0)),
                None,
                Some(logical_size(ow, oh)),
            ),
            WallpaperMode::Fill => {
                // Sample the largest centred sub-rect matching the output aspect.
                let out_aspect = ow / oh;
                let (sx, sy, sw, sh) = if iw / ih > out_aspect {
                    let sw = ih * out_aspect;
                    ((iw - sw) / 2.0, 0.0, sw, ih)
                } else {
                    let sh = iw / out_aspect;
                    (0.0, (ih - sh) / 2.0, iw, sh)
                };
                (
                    Point::from((0.0, 0.0)),
                    Some(Rectangle::new(Point::from((sx, sy)), Size::from((sw, sh)))),
                    Some(logical_size(ow, oh)),
                )
            }
            WallpaperMode::Fit => {
                // Largest scale fitting inside the output, centred (letterboxed).
                let s = (ow / iw).min(oh / ih);
                let (dw, dh) = (iw * s, ih * s);
                (
                    Point::from(((ow - dw) / 2.0, (oh - dh) / 2.0)),
                    None,
                    Some(logical_size(dw, dh)),
                )
            }
            WallpaperMode::Center => (
                Point::from(((ow - iw) / 2.0, (oh - ih) / 2.0)),
                None,
                Some(logical_size(iw, ih)),
            ),
        };

        match MemoryRenderBufferRenderElement::from_buffer(
            renderer, loc, &loaded.buffer, None, src, dst, Kind::Unspecified,
        ) {
            Ok(elem) => vec![elem],
            Err(e) => {
                tracing::warn!("wallpaper: render element failed: {e}");
                Vec::new()
            }
        }
    })
}
