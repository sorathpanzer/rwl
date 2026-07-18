//! Winit backend — runs rwl nested inside an existing Wayland or X11 compositor.
//!
//! This is the counterpart to the udev/DRM backend: instead of taking over a
//! GPU via libseat/DRM it opens an EGL-backed window in the parent compositor.
//! It is selected automatically when `WAYLAND_DISPLAY` or `DISPLAY` is set.

use std::time::Duration;

use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::winit::{self, WinitEvent, WinitGraphicsBackend};
use smithay::input::pointer::CursorImageStatus;
use smithay::output::{Mode, Output, PhysicalProperties, Subpixel};
use smithay::reexports::calloop::LoopHandle;
use smithay::reexports::wayland_server::backend::GlobalId;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::DisplayHandle;
use smithay::utils::{IsAlive, Transform};
use smithay::wayland::seat::WaylandFocus;

use crate::error::{RwlError, Result};
use crate::state::Rwl;

// ---------------------------------------------------------------------------
// Data structure
// ---------------------------------------------------------------------------

/// State for the winit (nested) backend.
pub struct WinitData {
    /// EGL-backed graphics backend (renderer + swap chain).
    pub gfx: WinitGraphicsBackend<GlesRenderer>,
    /// The single virtual output that maps to the host window.
    pub output: Output,
    /// Keeps the `wl_output` global alive for the duration of the session.
    _output_global: GlobalId,
    /// Per-output damage tracker used for partial redraws.
    damage_tracker: OutputDamageTracker,
    /// GPU-side state for the rounded-corner feature.
    #[cfg(feature = "rounded-corners")]
    pub rounded: crate::features::rounded_corners::RoundedCornerState,
}

// ---------------------------------------------------------------------------
// Initialisation
// ---------------------------------------------------------------------------

/// Initialise the winit backend and register it with the calloop event loop.
///
/// Returns [`WinitData`] which must be stored in `state.backend`.  After
/// storing it, call `state.output_added(&winit_data.output)` to register the
/// virtual monitor.
#[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
pub fn init(
    loop_handle: &LoopHandle<'static, Rwl>,
    display_handle: &DisplayHandle,
) -> Result<WinitData> {
    let (mut gfx, winit_event_loop) = winit::init::<GlesRenderer>()
        .map_err(|e| RwlError::Backend(format!("{e:?}")))?;

    let size = gfx.window_size();
    let mode = Mode { size, refresh: 60_000 };

    let output = Output::new(
        "winit".to_owned(),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "Smithay".into(),
            model: "Winit".into(),
            serial_number: "Unknown".into(),
        },
    );
    let output_global = output.create_global::<Rwl>(display_handle);
    // EGL window surfaces on Wayland do not have the DRM y-flip that KMS
    // scanout applies, so the flip180 baked into GlesRenderer's projection
    // matrix over-compensates and produces an upside-down image with Normal.
    // Flipped180 has matrix diag(1,−1,1), identical to flip180, so the two
    // cancel out and the projection becomes the bare ortho matrix — pixel y=0
    // maps to NDC +1 (visual top).  This matches anvil's reference winit setup.
    output.change_current_state(
        Some(mode),
        Some(Transform::Flipped180),
        None,
        Some((0, 0).into()),
    );
    output.set_preferred(mode);

    let damage_tracker = OutputDamageTracker::from_output(&output);

    loop_handle
        .insert_source(winit_event_loop, |event, (), state| {
            match event {
                WinitEvent::Resized { size, scale_factor } => {
                    let new_mode = Mode { size, refresh: 60_000 };
                    // Update output mode so clients see the new resolution.
                    // Rebuild damage_tracker so it tracks the new output dimensions;
                    // without this, partial redraws use stale damage rectangles after resize.
                    if let Some(crate::backend::BackendData::Winit(ref mut data)) = state.backend {
                        data.output.change_current_state(
                            Some(new_mode),
                            None,
                            Some(smithay::output::Scale::Fractional(scale_factor)),
                            None,
                        );
                        data.damage_tracker = OutputDamageTracker::from_output(&data.output);
                    }
                    // Update monitor work area (logical pixels = physical / scale).
                    let lw = (f64::from(size.w) / scale_factor) as i32;
                    let lh = (f64::from(size.h) / scale_factor) as i32;
                    if let Some(mon) = state.monitors.first_mut() {
                        mon.m = smithay::utils::Rectangle::new(
                            (0, 0).into(),
                            (lw, lh).into(),
                        );
                        mon.w = mon.m;
                    }
                    state.arrange_all();
                }
                WinitEvent::Input(event) => {
                    // Update pointer/keyboard state, then request a redraw
                    // instead of rendering synchronously here.
                    //
                    // Rendering to a Wayland EGL surface blocks on the host's
                    // vsync (eglSwapBuffers waits for a free buffer, ~16 ms).
                    // Rendering a full frame for *every* pointer-motion event
                    // therefore serialises input behind the swap: a high-rate
                    // mouse (1000 Hz) produces events far faster than 16 ms and
                    // the queue backs up, so the cursor lags badly.
                    //
                    // request_redraw() is coalesced by winit — a whole burst of
                    // motion between frames collapses into a single Redraw that
                    // renders the latest pointer position.  Smithay's winit
                    // source drains pending events in before_sleep and keeps
                    // calloop awake while a RedrawRequested is queued, so this
                    // reliably drives WinitEvent::Redraw without stalling.
                    state.process_input_event(event);
                    state.schedule_render();
                }
                WinitEvent::Redraw => {
                    // Temporarily take the backend data so we can borrow both it
                    // and the rest of `state` mutably at the same time.
                    let Some(crate::backend::BackendData::Winit(mut data)) =
                        state.backend.take()
                    else {
                        return;
                    };
                    let any_fading = render_frame(&mut data, state);
                    state.backend = Some(crate::backend::BackendData::Winit(data));
                    if any_fading {
                        state.schedule_render();
                    }
                }
                WinitEvent::CloseRequested => {
                    state.loop_signal.stop();
                }
                WinitEvent::Focus(focused) => {
                    // Smithay's winit backend does not forward CursorLeft, so a
                    // window-focus change is our only signal that the pointer
                    // has left the nested window.  When it leaves, the host
                    // compositor draws its own cursor; hide our software cursor
                    // so a stale copy isn't left frozen inside the window (a
                    // double cursor).  It reappears on the next input event via
                    // handle_cursor_activity().
                    if !focused && !state.cursor_hidden {
                        state.cursor_hidden = true;
                        state.schedule_render();
                    }
                }
            }
        })
        .map_err(|e| RwlError::EventLoop(e.to_string()))?;

    #[cfg(feature = "rounded-corners")]
    let rounded = crate::features::rounded_corners::RoundedCornerState::init(gfx.renderer());

    Ok(WinitData {
        gfx,
        output,
        _output_global: output_global,
        damage_tracker,
        #[cfg(feature = "rounded-corners")]
        rounded,
    })
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_lines)]
fn render_frame(data: &mut WinitData, state: &mut Rwl) -> bool {
    // If the cursor surface was destroyed (e.g. the window that owned it
    // closed), reset to the named default so the host cursor becomes visible
    // again instead of disappearing.
    if let CursorImageStatus::Surface(ref surf) = state.cursor_status
        && !surf.alive()
    {
        state.cursor_status = CursorImageStatus::default_named();
    }

    // Render gate: hold the previous frame while tiled windows are mid-resize.
    // New windows (no buffer) get 100 ms; existing windows resizing after a
    // close only need ~32 ms (~2 frames).
    let new_pending = state.any_tiled_pending_resize(&data.output);
    let resize_pending = !new_pending && state.any_existing_tiled_pending_resize(&data.output);
    let gate_timeout_ms: u128 = if new_pending { 100 } else { 32 };
    if new_pending || resize_pending {
        let now = std::time::Instant::now();
        let since = state.layout_gate_since.get_or_insert(now);
        if now.saturating_duration_since(*since).as_millis() < gate_timeout_ms {
            return false;
        }
    }
    state.layout_gate_since = None;

    // Advance per-window fade animations before building elements so the
    // updated alpha is used in this frame's render_elements_from_surface_tree.
    #[cfg(feature = "fade")]
    let any_fading = crate::features::fade::advance_fades(state);
    #[cfg(not(feature = "fade"))]
    let any_fading = false;

    #[cfg(feature = "tag-transition")]
    let any_transitioning = crate::features::tag_transition::advance_tag_transitions(state);
    #[cfg(feature = "tag-transition")]
    state.finalize_slide_outs();
    #[cfg(feature = "tag-transition")]
    if state.tag_transition_was_active && !any_transitioning {
        // See udev.rs: re-arrange when the transition ends, mirroring the manual
        // mod+<tag> workaround, so switch_to_tag windows repaint instead of
        // staying grey.
        state.arrange_all();
        let top = state.focused_window().cloned();
        state.focus_window(top);
        state.post_transition_wakeup = 30;
    }
    #[cfg(feature = "tag-transition")]
    { state.tag_transition_was_active = any_transitioning; }
    #[cfg(feature = "tag-transition")]
    if state.post_transition_wakeup > 0 {
        state.post_transition_wakeup -= 1;
        if state.post_transition_wakeup == 15 {
            state.arrange_all();
            let top = state.focused_window().cloned();
            state.focus_window(top);
        }
    }
    #[cfg(not(feature = "tag-transition"))]
    let any_transitioning = false;

    // Advance the overview open/close zoom (may finalize a selection).
    #[cfg(feature = "overview")]
    let any_overview = crate::features::overview::advance(state);
    #[cfg(not(feature = "overview"))]
    let any_overview = false;

    // Build border decoration elements before binding the EGL surface so they
    // live for the duration of the render call.
    let scale = data.output.current_scale().fractional_scale(); // f64
    let visible_count = state
        .monitors
        .first()
        .map_or(0, |mon| {
            let tags = mon.tags();
            state
                .windows
                .iter()
                .filter(|w| {
                    crate::window::window_visible_on(w, tags)
                        && crate::window::with_state(w, |s| s.buffer_mapped).unwrap_or(false)
                })
                .count()
        });
    // Borders are placed BEFORE windows in the front-to-back list, so they
    // render on top.  When a fullscreen window is active, skip borders entirely
    // — they'd appear over the fullscreen surface otherwise.
    let has_fullscreen_pre = state.space.elements().any(crate::window::window_is_fullscreen);
    let top_has_kbd_focus = {
        let map = smithay::desktop::layer_map_for_output(&data.output);
        map.layers_on(smithay::wayland::shell::wlr_layer::Layer::Top)
            .any(smithay::desktop::LayerSurface::can_receive_keyboard_focus)
    };
    #[cfg(feature = "bar")]
    let bar_has_notification = state.bar_has_notification.load(std::sync::atomic::Ordering::Relaxed);
    #[cfg(not(feature = "bar"))]
    let bar_has_notification = false;
    let pointer_loc = state.pointer.as_ref().map(smithay::input::pointer::PointerHandle::current_location).unwrap_or_default();
    // When the pointer is over empty space (no window on the current tag), use
    // the last known cursor surface as a software fallback.  This covers the
    // case where a video player hid the cursor (cursor_status = Hidden/Named)
    // and the user switched to an empty tag: without the fallback neither the
    // software nor the host cursor would appear.
    let pointer_over_window = state.space.element_under(pointer_loc).is_some();
    let cursor_fallback: Option<WlSurface> = if pointer_over_window {
        None
    } else {
        state
            .last_cursor_surface
            .as_ref()
            .filter(IsAlive::alive)
            .cloned()
    };

    // Refresh the space before rendering so element-to-output associations
    // are current.  Without this, windows mapped or moved since the last
    // refresh() (e.g. after arrange_all()) may be missing from elements_for_output
    // and won't be included in this frame's render elements.
    state.space.refresh();

    // Bind the EGL surface and render the space into the framebuffer.
    // The inner block ensures `renderer` and `fb` are dropped (releasing the
    // borrow on `data.gfx`) before we call `submit`.
    //
    // We manually compose all elements in the correct front-to-back z-order and
    // pass them as the `custom_elements` argument to `space::render_output` with
    // an empty `spaces` list.  This lets Bottom and Background layer-shell surfaces
    // sit BELOW windows without the old limitation where every custom element was
    // forced above the space by the combined-element path inside render_output.
    // Resolve the cursor image for the current Named status before the render block.
    let named_cursor: Option<crate::render::NamedCursorBuffer> =
        if let CursorImageStatus::Named(icon) = state.cursor_status {
            state.cursor_cache.entry(icon).or_insert_with(|| {
                let cfg = crate::config::get();
                let size = if cfg.cursor_size > 0 { cfg.cursor_size } else {
                    std::env::var("XCURSOR_SIZE").ok().and_then(|s| s.parse().ok()).unwrap_or(24)
                };
                let buf = crate::render::load_cursor_icon(cfg.cursor_theme.as_deref(), size, icon);
                drop(cfg);
                buf
            });
            state.cursor_cache
                .get(&icon)
                .and_then(|b| b.as_ref())
                .or_else(|| state.cursor_cache.get(&smithay::input::pointer::CursorIcon::Default).and_then(|b| b.as_ref()))
                .cloned()
        } else {
            None
        };

    let render_ok = {
        match data.gfx.bind() {
            Ok((renderer, mut fb)) => {
                #[cfg(feature = "overview")]
                let overview_on = state.overview.as_ref().is_some_and(|o| o.on_output(&data.output));
                #[cfg(feature = "overview")]
                let (window_elems, border_elems): (Vec<crate::render::RwlRenderElement>, Vec<crate::render::RwlRenderElement>) =
                    if overview_on {
                        #[cfg(feature = "rounded-corners")]
                        let round = crate::render::thumb_round(&data.rounded, scale, false);
                        #[cfg(not(feature = "rounded-corners"))]
                        let round = None;
                        let ov = state.overview.as_ref().map_or_else(Vec::new, |ov| {
                            crate::features::overview::overview_elements(renderer, ov, &state.space, &data.output, scale, round)
                        });
                        (ov, Vec::new())
                    } else {
                        let win = {
                            #[cfg(feature = "rounded-corners")]
                            { crate::render::build_window_elements_rounded(renderer, &state.space, &data.output, scale, Some(&data.rounded), false) }
                            #[cfg(not(feature = "rounded-corners"))]
                            { crate::render::build_window_elements_plain(renderer, &state.space, &data.output, scale) }
                        };
                        let brd = {
                            #[cfg(feature = "rounded-corners")]
                            { crate::render::build_border_elements_rounded(renderer, &state.space, state.focused_window(), &data.output, scale, Some(&data.rounded), false, visible_count, has_fullscreen_pre) }
                            #[cfg(not(feature = "rounded-corners"))]
                            { crate::render::build_border_elements_plain(&state.space, state.focused_window(), scale, visible_count, has_fullscreen_pre) }
                        };
                        (win, brd)
                    };
                #[cfg(not(feature = "overview"))]
                let window_elems: Vec<crate::render::RwlRenderElement> = {
                    #[cfg(feature = "rounded-corners")]
                    { crate::render::build_window_elements_rounded(renderer, &state.space, &data.output, scale, Some(&data.rounded), false) }
                    #[cfg(not(feature = "rounded-corners"))]
                    { crate::render::build_window_elements_plain(renderer, &state.space, &data.output, scale) }
                };
                #[cfg(not(feature = "overview"))]
                let border_elems: Vec<crate::render::RwlRenderElement> = {
                    #[cfg(feature = "rounded-corners")]
                    { crate::render::build_border_elements_rounded(renderer, &state.space, state.focused_window(), &data.output, scale, Some(&data.rounded), false, visible_count, has_fullscreen_pre) }
                    #[cfg(not(feature = "rounded-corners"))]
                    { crate::render::build_border_elements_plain(&state.space, state.focused_window(), scale, visible_count, has_fullscreen_pre) }
                };

                let cursor_elems =
                    crate::render::cursor_elements(renderer, &state.cursor_status, state.cursor_hidden, pointer_loc, scale, cursor_fallback.as_ref());
                let dnd_elems = state.dnd_icon.as_ref().map_or_else(Vec::new, |icon| {
                    crate::render::dnd_icon_elements(renderer, icon, pointer_loc, scale)
                });
                // Render the xcursor arrow only when cursor_elements produced nothing
                // (Named status with no alive fallback surface).  If cursor_elements
                // already rendered a surface (the fallback from last_cursor_surface),
                // skip xcursor to avoid a double arrow on empty tags.
                let xcursor_elems =
                    if !state.cursor_hidden
                        && matches!(state.cursor_status, CursorImageStatus::Named(_))
                        && cursor_elems.is_empty()
                    {
                        crate::render::named_cursor_elements(
                            renderer,
                            named_cursor.as_ref(),
                            pointer_loc,
                            scale,
                        )
                    } else {
                        Vec::new()
                    };
                let overlay_elems = crate::render::layer_elements(
                    renderer, &data.output, smithay::wayland::shell::wlr_layer::Layer::Overlay, scale,
                );
                let top_elems = crate::render::layer_elements(
                    renderer, &data.output, smithay::wayland::shell::wlr_layer::Layer::Top, scale,
                );
                let bottom_elems = crate::render::layer_elements(
                    renderer, &data.output, smithay::wayland::shell::wlr_layer::Layer::Bottom, scale,
                );
                let bg_elems = crate::render::layer_elements(
                    renderer, &data.output, smithay::wayland::shell::wlr_layer::Layer::Background, scale,
                );
                // Native wallpaper — backmost, below the Background layer shell.
                #[cfg(feature = "wallpaper")]
                let wallpaper_tags = state
                    .monitors
                    .iter()
                    .find(|m| m.output == data.output)
                    .map_or(0, crate::monitor::Monitor::tags);
                #[cfg(feature = "wallpaper")]
                let wallpaper_elems: Vec<crate::render::RwlRenderElement> =
                    crate::features::wallpaper::elements(renderer, &data.output, scale, wallpaper_tags)
                        .into_iter()
                        .map(crate::render::RwlRenderElement::from)
                        .collect();
                #[cfg(not(feature = "wallpaper"))]
                let wallpaper_elems: Vec<crate::render::RwlRenderElement> = Vec::new();
                let lock_elems = crate::render::lock_surface_elements(
                    renderer, &state.lock_surfaces, &data.output, scale,
                );
                #[cfg(feature = "pip")]
                let pip_elems: Vec<crate::render::RwlRenderElement> = {
                    #[cfg(feature = "overview")]
                    let overview_shown = state.overview.as_ref().is_some_and(|o| o.on_output(&data.output));
                    #[cfg(not(feature = "overview"))]
                    let overview_shown = false;
                    if state.locked || overview_shown {
                        Vec::new()
                    } else {
                        #[cfg(feature = "rounded-corners")]
                        let round = crate::render::thumb_round(&data.rounded, scale, false);
                        #[cfg(not(feature = "rounded-corners"))]
                        let round = None;
                        state.pip.as_ref().map_or_else(Vec::new, |pip| {
                            crate::features::pip::pip_elements(renderer, pip, &state.space, &data.output, scale, round)
                        })
                    }
                };

                // Front-to-back order (no fullscreen):
                //   cursor → dnd → xcursor → lock → Overlay → Top
                //   → borders → windows (incl. popups) → Bottom → Background
                // When fullscreen, Top layer is omitted so bars don't overlay the window,
                // UNLESS Overlay has surfaces, a Top surface requests keyboard
                // focus (bar prompt), or the bar has an active title notification.
                let show_top = !has_fullscreen_pre || !overlay_elems.is_empty() || top_has_kbd_focus || bar_has_notification;
                let all: Vec<crate::render::RwlRenderElement> = if state.locked {
                    // Session locked: composite ONLY the cursor, lock surfaces,
                    // and an opaque black backdrop. Client windows and
                    // layer-shell surfaces are never drawn while locked, and the
                    // backdrop guarantees outputs without a live lock surface
                    // reveal nothing behind them.
                    // The native locker hides the pointer entirely; external
                    // `ext-session-lock` clients keep it.
                    #[cfg(feature = "lock")]
                    let hide_cursor = state.native_lock.is_some();
                    #[cfg(not(feature = "lock"))]
                    let hide_cursor = false;

                    let mut all = Vec::with_capacity(
                        cursor_elems.len() + xcursor_elems.len() + lock_elems.len() + 1,
                    );
                    if !hide_cursor {
                        all.extend(cursor_elems.into_iter().map(crate::render::RwlRenderElement::from));
                        all.extend(xcursor_elems.into_iter().map(crate::render::RwlRenderElement::from));
                    }
                    all.extend(lock_elems.into_iter().map(crate::render::RwlRenderElement::from));
                    // Native locker: password-progress bar above the backdrop.
                    #[cfg(feature = "lock")]
                    if let Some(lock) = state.native_lock.as_ref() {
                        all.extend(
                            crate::features::lock::bar_elements(lock, &data.output)
                                .into_iter()
                                .map(crate::render::RwlRenderElement::Border),
                        );
                    }
                    if let Some(backdrop) = crate::render::lock_backdrop_element(&data.output) {
                        all.push(crate::render::RwlRenderElement::Border(backdrop));
                    }
                    all
                } else {
                    let mut all: Vec<crate::render::RwlRenderElement> = Vec::with_capacity(
                        cursor_elems.len()
                            + dnd_elems.len()
                            + xcursor_elems.len()
                            + lock_elems.len()
                            + overlay_elems.len()
                            + if show_top { top_elems.len() } else { 0 }
                            + border_elems.len()
                            + window_elems.len()
                            + bottom_elems.len()
                            + bg_elems.len()
                            + wallpaper_elems.len(),
                    );
                    all.extend(cursor_elems.into_iter().map(crate::render::RwlRenderElement::from));
                    all.extend(dnd_elems.into_iter().map(crate::render::RwlRenderElement::from));
                    all.extend(xcursor_elems.into_iter().map(crate::render::RwlRenderElement::from));
                    all.extend(lock_elems.into_iter().map(crate::render::RwlRenderElement::from));
                    #[cfg(feature = "pip")]
                    all.extend(pip_elems);
                    all.extend(overlay_elems.into_iter().map(crate::render::RwlRenderElement::from));
                    if show_top {
                        all.extend(top_elems.into_iter().map(crate::render::RwlRenderElement::from));
                    }
                    all.extend(border_elems);
                    all.extend(window_elems);
                    all.extend(bottom_elems.into_iter().map(crate::render::RwlRenderElement::from));
                    all.extend(bg_elems.into_iter().map(crate::render::RwlRenderElement::from));
                    all.extend(wallpaper_elems);
                    all
                };

                // Pass empty `spaces` — window elements are already in `all`.
                // The damage tracker is still driven by render_output so partial
                // redraws (age > 0) work correctly.
                #[allow(clippy::cast_possible_truncation)]
                let render_result = smithay::desktop::space::render_output::<
                    _,
                    crate::render::RwlRenderElement,
                    _,
                    _,
                >(
                    &data.output,
                    renderer,
                    &mut fb,
                    scale as f32,
                    0,
                    // Typed empty array (not slice ref) so IntoIterator yields
                    // `&Space<Window>` directly; window elements are in `all` already.
                    {
                        let s: [&smithay::desktop::Space<smithay::desktop::Window>; 0] = [];
                        s
                    },
                    &all,   // complete element list in correct z-order
                    &mut data.damage_tracker,
                    [0.1, 0.1, 0.1, 1.0],
                );

                // While the EGL surface is still bound (before submit), copy
                // the rendered framebuffer into any pending screencopy frames.
                if render_result.is_ok() {
                    crate::handlers::screencopy::submit_screencopy_frames(
                        renderer,
                        &fb,
                        &data.output,
                        &mut state.pending_screencopy_frames,
                        state.clock.now(),
                    );
                }

                render_result.map_err(|e| e.to_string()).map(|_| ())
            }
            Err(e) => Err(e.to_string()),
        }
    };

    if let Err(e) = render_ok {
        tracing::error!("winit render: {e}");
        return false;
    }

    if let Err(e) = data.gfx.submit(None) {
        tracing::error!("winit submit: {e}");
        return false;
    }

    // Send wl_surface.frame callbacks so clients know when to produce the
    // next frame.  Also send to the cursor surface so animated cursors advance
    // and clients that gate set_cursor on frame callbacks are not stalled.
    let time = state.clock.now();
    state.space.elements().for_each(|w| {
        if let Some(wl_surf) = w.wl_surface() {
            let wl_surf = wl_surf.into_owned();
            smithay::desktop::utils::send_frames_surface_tree(
                &wl_surf,
                &data.output,
                time,
                Some(Duration::ZERO),
                |_, _| Some(data.output.clone()),
            );
            // Send frame callbacks to all popups attached to this window.
            smithay::desktop::PopupManager::popups_for_surface(&wl_surf)
                .for_each(|(popup, _)| {
                    smithay::desktop::utils::send_frames_surface_tree(
                        popup.wl_surface(),
                        &data.output,
                        time,
                        Some(Duration::ZERO),
                        |_, _| Some(data.output.clone()),
                    );
                });
        }
    });
    if let CursorImageStatus::Surface(ref cursor_surf) = state.cursor_status {
        let cursor_surf = cursor_surf.clone();
        smithay::desktop::utils::send_frames_surface_tree(
            &cursor_surf,
            &data.output,
            time,
            Some(Duration::ZERO),
            |_, _| Some(data.output.clone()),
        );
    }

    // Layer-shell surfaces (bars, wallpapers) also need frame callbacks so
    // they know when the compositor has presented their content and can
    // render their next frame.  Without this dwlb would never receive a
    // frame-done event and could show stale content indefinitely.
    {
        let layer_map = smithay::desktop::layer_map_for_output(&data.output);
        layer_map.layers().for_each(|layer| {
            smithay::desktop::utils::send_frames_surface_tree(
                layer.wl_surface(),
                &data.output,
                time,
                Some(Duration::ZERO),
                |_, _| Some(data.output.clone()),
            );
        });
    }

    // Lock surfaces need frame callbacks so the screen locker's animation
    // (e.g. swaylock's typing indicator) can advance and its event loop does
    // not block waiting indefinitely for a callback that never arrives.
    state.lock_surfaces
        .iter()
        .filter(|(_, o)| o == &data.output)
        .filter(|(s, _)| s.alive())
        .for_each(|(s, _)| {
            smithay::desktop::utils::send_frames_surface_tree(
                s.wl_surface(),
                &data.output,
                time,
                Some(Duration::ZERO),
                |_, _| Some(data.output.clone()),
            );
        });

    // Keep a PiP window on a hidden tag producing frames for its live thumbnail.
    #[cfg(feature = "pip")]
    crate::features::pip::send_frame(state, &data.output, time);

    state.space.refresh();
    state.popup_manager.cleanup();

    // Always hide the host compositor's cursor: we render our own overlay for
    // every cursor state (xcursor arrow for Named, client surface for Surface).
    // Showing the host cursor alongside our overlay would cause a double cursor.
    data.gfx.window().set_cursor_visible(false);

    any_fading || any_transitioning || any_overview
}
