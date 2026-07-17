//! `wl_compositor` / `wl_subcompositor` handler.

use smithay::backend::renderer::utils::on_commit_buffer_handler;
use smithay::desktop::layer_map_for_output;
use smithay::reexports::calloop::Interest;
use smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::Client;
use smithay::reexports::wayland_server::Resource;
use smithay::wayland::buffer::BufferHandler;
use smithay::wayland::compositor::{
    add_blocker, add_destruction_hook, add_pre_commit_hook, BufferAssignment, CompositorClientState,
    CompositorHandler, CompositorState, SurfaceAttributes, get_parent, is_sync_subsurface,
};
use smithay::wayland::dmabuf::get_dmabuf;
use smithay::wayland::drm_syncobj::DrmSyncobjCachedState;
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::wayland::shell::xdg::XdgToplevelSurfaceData;
use smithay::wayland::shm::{ShmHandler, ShmState};

use crate::state::{ClientState, Rwl};
use crate::window::{
    window_is_floating, window_is_fullscreen, with_state, with_state_mut,
};


impl CompositorHandler for Rwl {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        // XWayland's client carries `XWaylandClientData`, not rwl's `ClientState`,
        // so check it first — otherwise the abort below fires the moment XWayland
        // binds its first global (e.g. wl_output) and takes the whole session down.
        #[cfg(feature = "xwayland")]
        if let Some(state) = client.get_data::<smithay::xwayland::XWaylandClientData>() {
            return &state.compositor_state;
        }
        let Some(state) = client.get_data::<ClientState>() else {
            std::process::abort()
        };
        &state.compositor_state
    }

    /// Register a pre-commit hook on every new surface that delays the commit
    /// until any DMA-BUF fence attached to the buffer has been signalled.
    ///
    /// Without this, a GPU-rendered DMA-BUF (e.g. from Electron/Chromium) can
    /// be scanned out before the GPU has finished writing to it, causing the
    /// flickering observed with `--enable-gpu` apps.  With explicit sync
    /// (`wp_linux_drm_syncobj_manager_v1`) the acquire point is waited on
    /// directly; otherwise the DMA-BUF implicit `IN_FENCE` is used as a fallback.
    fn new_surface(&mut self, surface: &WlSurface) {
        // Schedule a repaint when this surface is destroyed.  A surface can
        // disappear without any other surface committing — e.g. an xdg_popup
        // context menu, a tooltip, an autocomplete list, or an IME popup being
        // dismissed.  When that happens the render element for the popup is gone
        // from the next elements list, and the OutputDamageTracker will damage
        // the region it occupied and repaint the parent content underneath — but
        // only if a render actually runs.  Without this hook nothing kicks the
        // render loop, so the area under the just-closed surface stays stale
        // (its content appears to "vanish") until some unrelated event (opening
        // another window, moving the pointer over a client that redraws, …)
        // forces a full repaint.  Covers popups, subsurfaces and layer surfaces
        // alike, since destruction is the one lifecycle event with no commit.
        add_destruction_hook::<Self, _>(surface, |state, _surface| {
            state.schedule_render();
        });

        add_pre_commit_hook::<Self, _>(surface, |state, _dh, surface| {
            // Retrieve the pending DMA-BUF and any explicit-sync acquire point.
            let (maybe_dmabuf, maybe_acquire) =
                smithay::wayland::compositor::with_states(surface, |states| {
                    let dmabuf = {
                        let mut guard = states.cached_state.get::<SurfaceAttributes>();
                        guard.pending().buffer.as_ref().and_then(|a| match a {
                            BufferAssignment::NewBuffer(buf) => get_dmabuf(buf).cloned().ok(),
                            BufferAssignment::Removed => None,
                        })
                    };
                    let acquire = {
                        let mut guard = states.cached_state.get::<DrmSyncobjCachedState>();
                        guard.pending().acquire_point.clone()
                    };
                    (dmabuf, acquire)
                });

            let Some(dmabuf) = maybe_dmabuf else { return };
            let Some(client) = surface.client() else { return };

            // Explicit sync: DRM syncobj timeline fence provided by the client.
            if let Some(acquire) = maybe_acquire
                && let Ok((blocker, source)) = acquire.generate_blocker()
            {
                    let client2 = client.clone();
                    let res = state.loop_handle.insert_source(source, move |(), (), state| {
                        let dh = state.display_handle.clone();
                        let cc = state.client_compositor_state(&client2);
                        cc.blocker_cleared(state, &dh);
                        // blocker_cleared → apply() → commit() → schedule_render() normally,
                        // but if take_ready() skips this surface (another transaction for the
                        // same surface is still pending), commit() is never called and the
                        // render loop goes idle.  Kick it explicitly to cover that case.
                        state.schedule_render();
                        Ok(())
                    });
                    if res.is_ok() {
                        add_blocker(surface, blocker);
                        return;
                    }
            }

            // Implicit sync: wait for the DMA-BUF IN_FENCE (GPU write completion).
            if let Ok((blocker, source)) = dmabuf.generate_blocker(Interest::READ) {
                let res = state.loop_handle.insert_source(source, move |_dmabuf, _metadata, state| {
                    let dh = state.display_handle.clone();
                    let cc = state.client_compositor_state(&client);
                    cc.blocker_cleared(state, &dh);
                    // Same reasoning as the explicit-sync path above.
                    state.schedule_render();
                    Ok(())
                });
                if res.is_ok() {
                    add_blocker(surface, blocker);
                }
            }
        });
    }

    #[allow(clippy::too_many_lines)]
    fn commit(&mut self, surface: &WlSurface) {
        on_commit_buffer_handler::<Self>(surface);
        self.popup_manager.commit(surface);

        // Update the Window's bounding box so that space.element_geometry()
        // reflects the current surface size.  This is required for correct
        // border placement.  Walk up to the root in case this is a subsurface.
        if !is_sync_subsurface(surface) {
            let mut root = surface.clone();
            while let Some(parent) = get_parent(&root) {
                root = parent;
            }
            if let Some(w) = self.window_for_surface(&root).cloned() {
                w.on_commit();
            } else {
                // XWayland override-redirect surface (menu/tooltip): update its
                // bbox too, but it is otherwise unmanaged (no rules/focus/tiling).
                #[cfg(feature = "xwayland")]
                if let Some(w) = self.x11_or_window_for_surface(&root) {
                    w.on_commit();
                }
            }
        }

        let window = self.window_for_surface(surface).cloned();
        if let Some(ref w) = window {
            // When a tiled window finishes resizing (committed size now matches
            // pending_geom.size), bump border_counter so the damage tracker redraws
            // the border strips at the correct new geometry.  arrange() already
            // updated the space location via relocate_element, so no relocation
            // is needed here — only the size has changed.
            {
                let pending = with_state(w, |s| s.pending_geom).flatten();
                if let Some(pending) = pending {
                    let committed_size = w.geometry().size;
                    if committed_size == pending.size && committed_size.w > 0 {
                        with_state_mut(w, |s| s.border_counter.increment());
                    }
                }
            }

            let needs_rules = with_state(w, |s| !s.rules_applied).unwrap_or(false);
            if needs_rules {
                // First commit: app_id / title are now set.  Apply window
                // rules (tag assignment, floating, etc.) and then send the
                // mandatory initial configure with the correct geometry and
                // xdg states (Activated, Tiled, …) in one shot.
                with_state_mut(w, |s| s.rules_applied = true);
                self.apply_rules(w);
                // If this window was spawned by a terminal, hide that terminal and
                // let this window take its slot (before the first arrange).
                #[cfg(feature = "swallow")]
                crate::features::swallow::try_swallow(self, w);
                // Fire on_window_open now — after static rules, before the first
                // arrange — so a callback's set_tags/set_floating behave like a
                // rule (no visible re-tile).
                #[cfg(feature = "hooks")]
                {
                    crate::features::hooks::window_open(self, w);
                    crate::features::hooks::drain(self);
                }
                self.arrange_all();
                // Mirror C dwl mapnotify: send wl_keyboard.enter AFTER the
                // initial configure so clients (e.g. mpv with
                // profile-cond=not focused) see Activated before the enter.
                let top = self.focused_window().cloned();
                self.focus_window(top);
                #[cfg(feature = "ipc")]
                {
                    let cw = w.clone();
                    crate::features::ipc::event::window(self, "open", &cw);
                }
            } else {
                // Subsequent commits: check whether the initial configure has
                // been sent.  This is a safety net for the rare case where
                // arrange_all() didn't run on the very first commit.
                //
                // `is_none_or`: XWayland windows have no xdg toplevel and no
                // "initial configure" concept, so treat them as already sent —
                // otherwise every buffer commit from an X11 client (e.g. Steam
                // rendering) would re-run arrange_all(), thrashing focus/titles
                // and pegging the CPU.
                let initial_sent = w.toplevel().is_none_or(|tl| {
                    smithay::wayland::compositor::with_states(tl.wl_surface(), |states| {
                        states
                            .data_map
                            .get::<XdgToplevelSurfaceData>()
                            .is_some_and(|d| d.lock().is_ok_and(|g| g.initial_configure_sent))
                    })
                });
                if !initial_sent {
                    self.arrange_all();
                }
            }
            // Re-send a configure on the first commit that actually attaches a
            // buffer.  mpv's handle_toplevel_config returns early (discarding
            // the state array) while wl->geometry_configured is false; that
            // flag is only set inside vo_wayland_reconfig, which runs *after*
            // our initial Activated configures have already been delivered.
            // Mirror wlroots' map event: once the client has a real surface
            // size we re-emit the current Activated/Tiled/Fullscreen states so
            // the post-init parser picks them up.
            let win_size = w.geometry().size;
            let already_mapped = with_state(w, |s| s.buffer_mapped).unwrap_or(true);
            if !already_mapped && win_size.w > 0 && win_size.h > 0 {
                with_state_mut(w, |s| {
                    s.buffer_mapped = true;
                    #[cfg(feature = "fade")]
                    {
                        let fade_in_ms = crate::config::get().fade_in_ms;
                        if fade_in_ms > 0 {
                            s.fade_alpha = 0.0;
                            s.fade_start = Some(std::time::Instant::now());
                            s.fading_out = false;
                        }
                    }
                });
                let is_focused = self.focus_stack.first() == Some(w);
                let is_fs = window_is_fullscreen(w);
                let is_tiled = !window_is_floating(w) && !is_fs;
                if let Some(tl) = w.toplevel() {
                    tl.with_pending_state(|state| {
                        if is_focused {
                            state.states.set(xdg_toplevel::State::Activated);
                        } else {
                            state.states.unset(xdg_toplevel::State::Activated);
                        }
                        if is_tiled {
                            state.states.set(xdg_toplevel::State::TiledLeft);
                            state.states.set(xdg_toplevel::State::TiledRight);
                            state.states.set(xdg_toplevel::State::TiledTop);
                            state.states.set(xdg_toplevel::State::TiledBottom);
                        } else {
                            state.states.unset(xdg_toplevel::State::TiledLeft);
                            state.states.unset(xdg_toplevel::State::TiledRight);
                            state.states.unset(xdg_toplevel::State::TiledTop);
                            state.states.unset(xdg_toplevel::State::TiledBottom);
                        }
                        if is_fs {
                            state.states.set(xdg_toplevel::State::Fullscreen);
                        } else {
                            state.states.unset(xdg_toplevel::State::Fullscreen);
                        }
                    });
                    // send_configure (not send_pending_configure) forces a new
                    // event even if smithay considers nothing changed.
                    tl.send_configure();
                }
                #[cfg(feature = "warp")]
                if is_focused {
                    crate::features::warp::warp_cursor_to_focused(self);
                }

                // Mirror C dwl mapnotify: decide floating/centring at *map*
                // time, not at the initial pre-buffer commit.  A fixed-size
                // window (e.g. GIMP's splash) only commits its min==max size
                // when it attaches its first buffer — after apply_rules() has
                // already run and (correctly, for that moment) classified it as
                // tiled.  dwl side-steps this by deciding floating in mapnotify;
                // rwl re-checks here and promotes + centres if it now qualifies.
                // Wayland toplevels only — X11 fixed-size/typed windows are
                // handled in the XWM property_notify path.
                if w.toplevel().is_some()
                    && !window_is_floating(w)
                    && !window_is_fullscreen(w)
                    && Self::window_is_float_type(w)
                {
                    with_state_mut(w, |s| {
                        s.is_floating = true;
                        s.needs_centering = true;
                    });
                    // Reflow the tiled peers now that this window has left the
                    // tiling layout; the block below positions it at centre.
                    self.arrange_all();
                }
            }

            // Centre newly-floating windows on their first buffer commit.
            // apply_rules() sets needs_centering when is_floating becomes true.
            // The actual surface size is only known after the client commits a
            // buffer in response to the initial configure, so we defer here.
            let needs_centering = with_state(w, |s| s.needs_centering).unwrap_or(false);
            if needs_centering {
                let win_size = w.geometry().size;
                if win_size.w > 0 && win_size.h > 0 {
                    let mon_idx = with_state(w, |s| s.mon_idx).unwrap_or(self.sel_mon);
                    // Mirror C dwl applyrules: center using work-area dimensions
                    // offset by the full monitor origin (mon->w.w/h + mon->m.x/y).
                    let loc = self.monitors.get(mon_idx).map(|mon| {
                        smithay::utils::Point::<i32, smithay::utils::Logical>::from((
                            mon.m.loc.x + (mon.w.size.w - win_size.w) / 2,
                            mon.m.loc.y + (mon.w.size.h - win_size.h) / 2,
                        ))
                    });
                    if let Some(loc) = loc {
                        with_state_mut(w, |s| {
                            s.needs_centering = false;
                        });
                        // Only map the window if it is actually visible on some
                        // monitor.  Scratchpads have tags=0 (hidden) when they
                        // first commit a buffer, so mapping them here would make
                        // them briefly visible.  Instead, stash the position in
                        // saved_loc so arrange() uses it when toggled on-screen.
                        let win_tags = crate::window::with_state(w, |s| s.tags).unwrap_or(0);
                        let visible_on_any =
                            self.monitors.iter().any(|m| m.tags() & win_tags != 0);
                        if visible_on_any {
                            self.space.map_element(w.clone(), loc, false);
                        } else {
                            with_state_mut(w, |s| s.saved_loc = Some(loc));
                        }
                    }
                }
            }

            // Detect title/appid changes and push an updated status line to bars
            // (dwlb, sombar, …) immediately.  In C dwl this is done via an
            // explicit set_title event handler; in Smithay there is no such
            // callback, so we compare against the last-seen values each commit.
            // Both fields are non-double-buffered (take effect before commit),
            // so XdgToplevelSurfaceData already has the new values by commit time.
            let (title_now, appid_now) = w.toplevel().and_then(|t| {
                smithay::wayland::compositor::with_states(t.wl_surface(), |states| {
                    states
                        .data_map
                        .get::<smithay::wayland::shell::xdg::XdgToplevelSurfaceData>()
                        .and_then(|d| d.lock().ok().map(|g| (g.title.clone(), g.app_id.clone())))
                })
            }).or_else(|| {
                // XWayland: read WM_NAME/WM_CLASS.  Without this, `w.toplevel()` is
                // None for X11 windows so title_now becomes None every commit,
                // resetting last_title and firing print_status on every frame a
                // continuously-rendering X11 app (e.g. Steam) draws — the flashing
                // title.  Mirrors the same fallback in check_toplevel_titles.
                #[cfg(feature = "xwayland")]
                { w.x11_surface().map(|x| (Some(x.title()), Some(x.class()))) }
                #[cfg(not(feature = "xwayland"))]
                { None }
            }).unwrap_or_default();
            let (title_changed, appid_changed) = with_state(w, |s| (
                s.last_title.as_deref() != title_now.as_deref(),
                s.last_appid.as_deref() != appid_now.as_deref(),
            )).unwrap_or((false, false));
            if title_changed || appid_changed {
                with_state_mut(w, |s| {
                    if title_changed { s.last_title = title_now; }
                    if appid_changed { s.last_appid = appid_now; }
                });
                crate::ipc::print_status(self);
            }
        } else {
            // Not a toplevel — check whether this is a layer-shell surface.
            let mon_idx = self.monitors.iter().position(|mon| {
                layer_map_for_output(&mon.output).layers().any(|l| l.wl_surface() == surface)
            });
            if let Some(idx) = mon_idx {
                // Check whether this is the very first commit (initial configure
                // not yet sent).  Must be checked before acquiring the map guard
                // to keep lock ordering consistent.
                let needs_configure =
                    smithay::wayland::compositor::with_states(surface, |states| {
                        states
                            .data_map
                            .get::<smithay::wayland::shell::wlr_layer::LayerSurfaceData>()
                            .is_some_and(|d| {
                                d.lock().is_ok_and(|g| !g.initial_configure_sent)
                            })
                    });

                // Clone the Output (Arc) so the map guard does not borrow self.
                let output = self.monitors[idx].output.clone();

                let zone = {
                    let mut map = layer_map_for_output(&output);

                    // Always call map.arrange() on every commit so that the
                    // surface position is updated whenever the client commits a
                    // new buffer size or anchor.  Layer surfaces like Wofi may
                    // commit several times during setup (defaults → configure →
                    // actual size → …), and skipping arrange on any of those
                    // leaves the surface at a stale position.
                    map.arrange();

                    if needs_configure {
                        for layer in map.layers() {
                            if layer.wl_surface() == surface {
                                layer.layer_surface().send_configure();
                                break;
                            }
                        }
                    }

                    map.non_exclusive_zone()
                };

                // Update the work area and re-tile only when it actually changes,
                // preventing a feedback loop on bar frame commits.
                let new_w = if zone.size.w > 0 && zone.size.h > 0 {
                    smithay::utils::Rectangle::new(
                        (
                            self.monitors[idx].m.loc.x + zone.loc.x,
                            self.monitors[idx].m.loc.y + zone.loc.y,
                        )
                            .into(),
                        zone.size,
                    )
                } else {
                    self.monitors[idx].m
                };
                if new_w != self.monitors[idx].w {
                    self.monitors[idx].w = new_w;
                    self.arrange(idx);
                }

                // C dwl: after arrangelayers, focus the topmost
                // keyboard-interactive Overlay/Top layer if it is now mapped.
                self.update_layer_focus();
            }
        }

        // Restart the udev render loop for any output that may have gone idle.
        self.schedule_render();
    }
}

impl BufferHandler for Rwl {
    fn buffer_destroyed(&mut self, _buffer: &WlBuffer) {}
}

impl ShmHandler for Rwl {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}
