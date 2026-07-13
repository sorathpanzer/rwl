//! Embedded status bar.
//!
//! Runs as a background thread within the compositor.  Connects back to the
//! compositor's own Wayland socket as a client using the layer-shell and
//! zdwl-ipc protocols, so it receives tag/layout/title state without an
//! external pipe.

mod blocks;
mod color;
mod config;
mod draw;
mod dwl_ipc;
mod ffi;
mod parse;
mod text;

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------
// User-tunable bar configuration passed to the embedded bar thread. The Lua
// parser lives in `config::settings`; these own the shape + defaults.

#[derive(Clone, Debug)]
pub struct BlockSettings {
    pub icon:     String,
    pub command:  String,
    pub interval: u32,
    pub signal:   u8,
    /// Shell command run on click; `$BLOCK_BUTTON` holds the mouse button number.
    pub command_click: String,
    /// Shell command whose stdout is shown in a floating widget while the pointer
    /// hovers the block (e.g. `cal` under a clock). Empty disables the widget.
    pub hover_command: String,
}

#[derive(Clone, Debug)]
#[allow(clippy::struct_excessive_bools)]
pub struct BarSettings {
    pub font:               String,
    pub vertical_padding:   u32,
    pub buffer_scale:       u32,
    pub hidden:             bool,
    pub bottom:             bool,
    pub hide_vacant:        bool,
    pub status_commands:    bool,
    pub center_title:       bool,
    pub active_color_title: bool,
    // 0xRRGGBBAA
    pub active_fg:          u32,
    pub active_bg:          u32,
    pub occupied_fg:        u32,
    pub occupied_bg:        u32,
    pub inactive_fg:        u32,
    pub inactive_bg:        u32,
    pub urgent_fg:          u32,
    pub urgent_bg:          u32,
    pub middle_bg:          u32,
    pub middle_bg_selected: u32,
    pub blocks:             Vec<BlockSettings>,
    pub blocks_delim:       String,
    pub tag_names:          Vec<String>,
}

impl Default for BarSettings {
    fn default() -> Self {
        Self {
            font:               "UbuntuMono Nerd Font:style=Bold:size=14".into(),
            vertical_padding:   4,
            buffer_scale:       1,
            hidden:             false,
            bottom:             false,
            hide_vacant:        true,
            status_commands:    true,
            center_title:       true,
            active_color_title: true,
            active_fg:          0xdf73_ffff,
            active_bg:          0x0000_00ff,
            occupied_fg:        0x9999_99ff,
            occupied_bg:        0x0000_00ff,
            inactive_fg:        0x9999_99ff,
            inactive_bg:        0x0000_00ff,
            urgent_fg:          0x0000_00ff,
            urgent_bg:          0xeeee_eeff,
            middle_bg:          0x0000_00ff,
            middle_bg_selected: 0x0000_00ff,
            blocks:             default_block_settings(),
            blocks_delim:       "  ".into(),
            tag_names:          vec![
                " ".into(), " ".into(), " ".into(),
                " ".into(), " ".into(), " ".into(),
            ],
        }
    }
}

fn default_block_settings() -> Vec<BlockSettings> {
    vec![
        BlockSettings {
            icon:     "🗄️".into(),
            command:  "df -h | rg /dev/dm-0 | awk '{print $5}'".into(),
            interval: 3600,
            signal:   0,
            command_click: String::new(),
            hover_command: String::new(),
        },
        BlockSettings {
            icon:     "💻".into(),
            command:  "free -h | awk '/^Mem/ { print $3 }' | sed s/i//g".into(),
            interval: 30,
            signal:   0,
            command_click: String::new(),
            hover_command: String::new(),
        },
        BlockSettings {
            icon:     String::new(),
            command:  "lister".into(),
            interval: 0,
            signal:   1,
            command_click: String::new(),
            hover_command: String::new(),
        },
        BlockSettings {
            icon:     "🕓".into(),
            command:  "nu -c 'date now | format date %H:%M'".into(),
            interval: 1,
            signal:   0,
            command_click: String::new(),
            hover_command: "cal --color=always".into(),
        },
    ]
}

use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::{self, ZwlrLayerShellV1},
    zwlr_layer_surface_v1::{self, Anchor, ZwlrLayerSurfaceV1},
};
use wayland_protocols::xdg::xdg_output::zv1::client::{
    zxdg_output_manager_v1::ZxdgOutputManagerV1,
    zxdg_output_v1::{self, ZxdgOutputV1},
};

use std::{
    env,
    fs,
    io::{self, PipeReader, Read},
    mem,
    sync::{Arc, Mutex},
    thread,
    os::unix::{
        fs::PermissionsExt,
        io::{AsFd, OwnedFd},
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

use rustix::event::{poll, PollFd, PollFlags, Timespec};
use rustix::fs::{fcntl_getfl, fcntl_setfl, OFlags};
use rustix::io::Errno;

use wayland_client::{
    protocol::{
        wl_buffer, wl_compositor, wl_output, wl_registry, wl_shm, wl_shm_pool, wl_surface, wl_seat, wl_keyboard, wl_pointer,
    },
    Connection, Dispatch, EventQueue, QueueHandle,
};

use config::{Config, DEFAULT_TAG_NAMES, blocks_from_settings};
use draw::{allocate_shm_file, render_frame, BarDrawParams};
use parse::parse_into_customtext;
use azoth_render::Font;
use text::CustomText;

// ── Bar ───────────────────────────────────────────────────────────────────────

#[allow(clippy::struct_excessive_bools)]
struct Bar {
    wl_output: wl_output::WlOutput,
    name: Option<String>,

    surface: Option<wl_surface::WlSurface>,
    layer_surface: Option<ZwlrLayerSurfaceV1>,
    xdg_output: Option<ZxdgOutputV1>,

    mtags: u32,
    ctags: u32,
    urg: u32,
    sel: u32,
    layout: Option<Arc<str>>,
    window_title: Option<String>,
    layout_idx: u32,

    title_override: Option<(String, std::time::Instant, Option<azoth_render::RustColor>)>,
    title_is_error: bool,
    status: Arc<CustomText>,
    /// Cumulative pixel x-offset at which each block ends, relative to the start
    /// of the status text.  Empty when block geometry is unknown (e.g. status set
    /// via the socket/stdin paths rather than the blocks thread).
    block_pix_ends: Vec<u32>,

    dwl_output: Option<dwl_ipc::zdwl_ipc_output_v2::ZdwlIpcOutputV2>,
    pending_ctags: u32,
    pending_mtags: u32,
    pending_urg: u32,

    hidden: bool,
    bottom: bool,
    redraw: bool,
    dead: bool,

    configured: bool,
    width: u32,
    height: u32,
    stride: u32,
    bufsize: usize,
    textpadding: u32,
    global_name: u32,

    is_prompt_active: bool,
    prompt_buffer: String,
    was_hidden_before_prompt: bool,
    was_hidden_before_title: bool,

    autocomplete_query: Option<String>,
    autocomplete_index: usize,

    prompt_history: Vec<String>,
    history_index: Option<usize>,
    history_temp_buffer: String,
}

impl Bar {
    fn new(wl_output: wl_output::WlOutput, cfg: &Config, height: u32, textpadding: u32, global_name: u32) -> Self {
        Self {
            wl_output,
            name: None,
            surface: None, layer_surface: None, xdg_output: None,
            mtags: 0, ctags: 0, urg: 0, sel: 0,
            layout: None, window_title: None, layout_idx: 0,
            title_override: None, title_is_error: false, status: Arc::new(CustomText::default()),
            block_pix_ends: Vec::new(),
            dwl_output: None,
            pending_ctags: 0, pending_mtags: 0, pending_urg: 0,
            hidden: cfg.hidden, bottom: cfg.bottom, redraw: false, dead: false,
            configured: false, width: 0, height,
            stride: 0, bufsize: 0, textpadding, global_name,
            is_prompt_active: false,
            prompt_buffer: String::new(),
            was_hidden_before_prompt: false,
            was_hidden_before_title: false,
            autocomplete_query: None,
            autocomplete_index: 0,
            prompt_history: Vec::new(),
            history_index: None,
            history_temp_buffer: String::new(),
        }
    }
}

// ── Application state ─────────────────────────────────────────────────────────

/// Shared state between the Wayland event-loop thread and the blocks thread.
/// Tuple: (block list, delimiter, `reload_pending` flag).
type SharedBlocks = Arc<Mutex<(Vec<config::Block>, String, bool)>>;

/// Joined status string plus the byte offset at which each block ends, sent from
/// the blocks thread to the event loop.
type SharedStatus = Arc<Mutex<(String, Vec<u32>)>>;

/// Click/hover behaviour for one status block.
#[derive(Clone, Default)]
struct BlockAction {
    click: String,
    hover_command: String,
}

/// Marker user-data for the hover widget's layer surface, so its configure
/// events dispatch separately from the bars' layer surfaces (keyed by `usize`).
struct WidgetTag;

/// A floating hover widget: its own small layer surface anchored under the block,
/// rendering the block's `hover_command` output.
struct Widget {
    surface:       wl_surface::WlSurface,
    layer_surface: ZwlrLayerSurfaceV1,
    /// Rendered command output, one entry per line.
    lines:  Vec<String>,
    width:  u32,
    height: u32,
    stride: u32,
    bufsize: usize,
    configured: bool,
}

struct State {
    cfg: Config,
    font: Font,
    bar_height: u32,
    textpadding: u32,
    tags: Vec<String>,
    layouts: Vec<Arc<str>>,
    bars: Vec<Bar>,

    compositor: Option<wl_compositor::WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    layer_shell: Option<ZwlrLayerShellV1>,
    output_manager: Option<ZxdgOutputManagerV1>,

    dwl_manager: Option<dwl_ipc::zdwl_ipc_manager_v2::ZdwlIpcManagerV2>,

    seat: Option<wl_seat::WlSeat>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    pointer: Option<wl_pointer::WlPointer>,
    /// Bar index the pointer is currently over, plus its last surface-local x.
    pointer_focus: Option<usize>,
    pointer_x: f64,

    /// Per-block click command + hover command, indexed like the block list.
    /// Kept on the event-loop side so clicks/hovers need no cross-thread lock.
    block_actions: Vec<BlockAction>,

    /// (bar index, block index) the pointer currently hovers, if any.  Tracked so
    /// the hover widget is only rebuilt when the pointer crosses a block boundary.
    hover_target: Option<(usize, usize)>,
    /// The floating hover widget (a separate small layer surface), if shown.
    widget: Option<Widget>,

    shared_blocks: SharedBlocks,

    /// The compositor's Wayland socket name (value of `WAYLAND_DISPLAY`), passed
    /// in explicitly so the bar and its prompt-launched children need not rely on
    /// the process environment.
    wayland_socket: String,

    stdin_buf: String,
    ready: bool,
}

// Font is Send+Sync (declared in azoth-render), so State is Send.
// No unsafe impl needed.

impl State {
    const fn new(cfg: Config, font: Font, bar_height: u32, textpadding: u32, wayland_socket: String, shared_blocks: SharedBlocks) -> Self {
        Self {
            cfg, font, bar_height, textpadding,
            tags: vec![], layouts: vec![], bars: vec![],
            compositor: None, shm: None, layer_shell: None,
            output_manager: None, dwl_manager: None,
            seat: None, keyboard: None, pointer: None,
            pointer_focus: None, pointer_x: 0.0,
            block_actions: Vec::new(),
            hover_target: None, widget: None,
            shared_blocks,
            wayland_socket,
            stdin_buf: String::new(), ready: false,
        }
    }

    fn show_bar(&mut self, idx: usize, qh: &QueueHandle<Self>) {
        let bar = &mut self.bars[idx];
        if bar.surface.is_some() { return; }
        let Some(compositor)  = &self.compositor  else { return };
        let Some(layer_shell) = &self.layer_shell else { return };
        let surface = compositor.create_surface(qh, ());
        let anchor = Anchor::Left | Anchor::Right
            | if bar.bottom { Anchor::Bottom } else { Anchor::Top };
        let ls = layer_shell.get_layer_surface(
            &surface, Some(&bar.wl_output),
            zwlr_layer_shell_v1::Layer::Top, "dwlb".to_string(), qh, idx,
        );
        let ls_height = bar.height;
        ls.set_size(0, ls_height);
        ls.set_anchor(anchor);
        ls.set_exclusive_zone(ls_height.cast_signed());
        surface.commit();
        bar.surface = Some(surface);
        bar.layer_surface = Some(ls);
    }

    fn hide_bar(&mut self, idx: usize) {
        let bar = &mut self.bars[idx];
        if bar.surface.is_none() { return; }
        if let Some(ls) = bar.layer_surface.take() { ls.destroy(); }
        if let Some(s)  = bar.surface.take()       { s.destroy(); }
        bar.configured = false;
    }

    fn reanchor(&mut self, idx: usize) {
        if self.bars[idx].surface.is_none() { return; }
        if let Some(ls) = &self.bars[idx].layer_surface {
            let anchor = Anchor::Left | Anchor::Right
                | if self.bars[idx].bottom { Anchor::Bottom } else { Anchor::Top };
            ls.set_anchor(anchor);
            self.bars[idx].redraw = true;
        }
    }

    fn set_top(&mut self, idx: usize)    { self.bars[idx].bottom = false; self.reanchor(idx); }
    fn set_bottom(&mut self, idx: usize) { self.bars[idx].bottom = true;  self.reanchor(idx); }

    fn draw_frame_with_qh(&self, idx: usize, qh: &QueueHandle<Self>) {
        let bar = &self.bars[idx];
        if bar.surface.is_none() || !bar.configured { return; }

        let num_pixels = bar.bufsize / 4;
        let mut pixels = vec![0u32; num_pixels];

        {
            let bar = &self.bars[idx];
            let now = std::time::Instant::now();
            let (override_title, title_fg_override) = if bar.is_prompt_active {
                (Some(format!("Run: {}|", bar.prompt_buffer)), None)
            } else {
                match &bar.title_override {
                    Some((text, until, color)) if now < *until => (Some(text.clone()), *color),
                    _ => {
                        let fg = if bar.title_is_error {
                            Some(azoth_render::RustColor::opaque(0xff, 0x33, 0x33))
                        } else {
                            None
                        };
                        (None, fg)
                    }
                }
            };
            let window_title = override_title.as_deref().or(bar.window_title.as_deref());
            render_frame(&mut pixels, &BarDrawParams {
                width: bar.width, height: bar.height,
                stride: bar.stride,
                textpadding: bar.textpadding,
                mtags: bar.mtags, ctags: bar.ctags, urg: bar.urg, sel: bar.sel,
                layout: bar.layout.as_deref(),
                window_title,
                title_fg_override,
                status: &bar.status,
                font: &self.font, config: &self.cfg, tags: &self.tags,
            });
        }

        let fd: OwnedFd = match allocate_shm_file(self.bars[idx].bufsize as u64) {
            Ok(fd) => fd,
            Err(e) => { tracing::warn!("[bar] shm alloc: {e}"); return; }
        };

        if let Err(e) = write_all_fd(&fd, bytemuck::cast_slice(&pixels)) {
            tracing::warn!("[bar] write pixels: {e}"); return;
        }

        let Some(shm) = &self.shm else { return; };
        let bar = &self.bars[idx];
        #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
        let bufsize_i32 = bar.bufsize as i32;
        let pool = shm.create_pool(
            fd.as_fd(),
            bufsize_i32,
            qh, (),
        );
        let buffer = pool.create_buffer(
            0,
            bar.width.cast_signed(),
            bar.height.cast_signed(),
            bar.stride.cast_signed(),
            wl_shm::Format::Argb8888, qh, (),
        );
        pool.destroy();

        if let Some(surface) = &self.bars[idx].surface {
            surface.set_buffer_scale(self.cfg.buffer_scale.cast_signed());
            surface.attach(Some(&buffer), 0, 0);
            surface.damage_buffer(
                0, 0,
                self.bars[idx].width.cast_signed(),
                self.bars[idx].height.cast_signed(),
            );
            surface.commit();
        }
    }

    fn setup_bar(&mut self, idx: usize, qh: &QueueHandle<Self>) {
        if let Some(om) = &self.output_manager {
            let xo = om.get_xdg_output(&self.bars[idx].wl_output, qh, idx);
            self.bars[idx].xdg_output = Some(xo);
        }
        if self.cfg.ipc
            && let Some(mgr) = &self.dwl_manager
        {
            let ipc_out = mgr.get_output(&self.bars[idx].wl_output, qh, idx);
            self.bars[idx].dwl_output = Some(ipc_out);
        }
        if !self.bars[idx].hidden { self.show_bar(idx, qh); }
    }
}

// ── Dispatch impls ────────────────────────────────────────────────────────────

impl Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(state: &mut Self, registry: &wl_registry::WlRegistry, event: wl_registry::Event,
             &(): &(), _: &Connection, qh: &QueueHandle<Self>) {
        if let wl_registry::Event::Global { name, interface, version } = event {
            match interface.as_str() {
                "wl_compositor" => {
                    state.compositor = Some(registry.bind(name, version.min(4), qh, ()));
                }
                "wl_shm" => {
                    state.shm = Some(registry.bind(name, 1, qh, ()));
                }
                "zwlr_layer_shell_v1" => {
                    state.layer_shell = Some(registry.bind(name, version.min(4), qh, ()));
                }
                "zxdg_output_manager_v1" => {
                    state.output_manager = Some(registry.bind(name, version.min(3), qh, ()));
                }
                "wl_seat" => {
                    state.seat = Some(registry.bind(name, version.min(7), qh, ()));
                }
                "zdwl_ipc_manager_v2" if state.cfg.ipc => {
                    let mgr: dwl_ipc::zdwl_ipc_manager_v2::ZdwlIpcManagerV2 =
                        registry.bind(name, version.min(2), qh, ());
                    state.dwl_manager = Some(mgr);
                }
                "wl_output" => {
                    let wl_out: wl_output::WlOutput = registry.bind(name, 1, qh, ());
                    let bar = Bar::new(wl_out, &state.cfg, state.bar_height, state.textpadding, name);
                    let idx = state.bars.len();
                    state.bars.push(bar);
                    if state.ready { state.setup_bar(idx, qh); }
                }
                _ => {}
            }
        } else if let wl_registry::Event::GlobalRemove { name } = event
            && let Some(idx) = state.bars.iter().position(|b| !b.dead && b.global_name == name)
        {
            state.hide_bar(idx);
            if let Some(xo) = state.bars[idx].xdg_output.take() { xo.destroy(); }
            if let Some(dwl_o) = state.bars[idx].dwl_output.take() { dwl_o.release(); }
            state.bars[idx].dead = true;
        }
    }
}

impl Dispatch<wl_compositor::WlCompositor, ()> for State {
    fn event(_: &mut Self, _: &wl_compositor::WlCompositor, _: wl_compositor::Event, &(): &(), _: &Connection, _: &QueueHandle<Self>) {}
}
impl Dispatch<wl_shm::WlShm, ()> for State {
    fn event(_: &mut Self, _: &wl_shm::WlShm, _: wl_shm::Event, &(): &(), _: &Connection, _: &QueueHandle<Self>) {}
}
impl Dispatch<wl_shm_pool::WlShmPool, ()> for State {
    fn event(_: &mut Self, _: &wl_shm_pool::WlShmPool, _: wl_shm_pool::Event, &(): &(), _: &Connection, _: &QueueHandle<Self>) {}
}
impl Dispatch<wl_surface::WlSurface, ()> for State {
    fn event(_: &mut Self, _: &wl_surface::WlSurface, _: wl_surface::Event, &(): &(), _: &Connection, _: &QueueHandle<Self>) {}
}
impl Dispatch<wl_output::WlOutput, ()> for State {
    fn event(_: &mut Self, _: &wl_output::WlOutput, _: wl_output::Event, &(): &(), _: &Connection, _: &QueueHandle<Self>) {}
}
impl Dispatch<wl_buffer::WlBuffer, ()> for State {
    fn event(_: &mut Self, buf: &wl_buffer::WlBuffer, event: wl_buffer::Event, &(): &(), _: &Connection, _: &QueueHandle<Self>) {
        if matches!(event, wl_buffer::Event::Release) { buf.destroy(); }
    }
}
impl Dispatch<ZwlrLayerShellV1, ()> for State {
    fn event(_: &mut Self, _: &ZwlrLayerShellV1, _: zwlr_layer_shell_v1::Event, &(): &(), _: &Connection, _: &QueueHandle<Self>) {}
}
impl Dispatch<ZwlrLayerSurfaceV1, usize> for State {
    fn event(state: &mut Self, surface: &ZwlrLayerSurfaceV1, event: zwlr_layer_surface_v1::Event,
             &idx: &usize, _: &Connection, _: &QueueHandle<Self>) {
        match event {
            zwlr_layer_surface_v1::Event::Configure { serial, width, height } => {
                let prev_w = state.bars[idx].width;
                let prev_h = state.bars[idx].height;
                let w = if width  > 0 { width  * state.cfg.buffer_scale } else { prev_w };
                let h = if height > 0 { height * state.cfg.buffer_scale } else { prev_h };
                surface.ack_configure(serial);
                if state.bars[idx].layer_surface.as_ref() != Some(surface) { return; }
                let bar = &mut state.bars[idx];
                if bar.configured && w == bar.width && h == bar.height { return; }
                bar.width = w; bar.height = h;
                bar.stride = w * 4; bar.bufsize = bar.stride as usize * h as usize;
                bar.configured = true; bar.redraw = true;
            }
            zwlr_layer_surface_v1::Event::Closed
                if state.bars[idx].layer_surface.as_ref() == Some(surface) => {
                state.hide_bar(idx);
            }
            _ => {}
        }
    }
}
impl Dispatch<ZxdgOutputManagerV1, ()> for State {
    fn event(_: &mut Self, _: &ZxdgOutputManagerV1,
             _: wayland_protocols::xdg::xdg_output::zv1::client::zxdg_output_manager_v1::Event,
             &(): &(), _: &Connection, _: &QueueHandle<Self>) {}
}
impl Dispatch<ZxdgOutputV1, usize> for State {
    fn event(state: &mut Self, _: &ZxdgOutputV1, event: zxdg_output_v1::Event,
             &idx: &usize, _: &Connection, _: &QueueHandle<Self>) {
        if let zxdg_output_v1::Event::Name { name } = event {
            state.bars[idx].name = Some(name);
        }
    }
}

// ── Seat / Keyboard Dispatch ──────────────────────────────────────────────────

impl Dispatch<wl_seat::WlSeat, ()> for State {
    fn event(
        state: &mut Self,
        seat: &wl_seat::WlSeat,
        event: wl_seat::Event,
        _data: &(),
        _conn: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities { capabilities } = event {
            use wayland_client::protocol::wl_seat::Capability;
            let caps = match capabilities {
                wayland_client::WEnum::Value(caps) => caps,
                wayland_client::WEnum::Unknown(_) => Capability::empty(),
            };
            let has_keyboard = caps.contains(Capability::Keyboard);
            if has_keyboard && state.keyboard.is_none() {
                state.keyboard = Some(seat.get_keyboard(qh, ()));
            } else if !has_keyboard
                && let Some(kb) = state.keyboard.take()
            {
                kb.release();
            }

            let has_pointer = caps.contains(Capability::Pointer);
            if has_pointer && state.pointer.is_none() {
                state.pointer = Some(seat.get_pointer(qh, ()));
            } else if !has_pointer
                && let Some(ptr) = state.pointer.take()
            {
                ptr.release();
            }
        }
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, ()> for State {
    fn event(
        state: &mut Self,
        _keyboard: &wl_keyboard::WlKeyboard,
        event: wl_keyboard::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let wl_keyboard::Event::Key { key, state: key_state, .. } = event {
            use wayland_client::protocol::wl_keyboard::KeyState;
            if !matches!(key_state, wayland_client::WEnum::Value(KeyState::Pressed)) {
                return;
            }
            if let Some(idx) = state.bars.iter().position(|b| b.is_prompt_active) {
                Self::handle_key_input(state, idx, key);
            }
        }
    }
}

// ── Pointer Dispatch (block hover + click) ────────────────────────────────────

impl Dispatch<wl_pointer::WlPointer, ()> for State {
    fn event(
        state: &mut Self,
        _pointer: &wl_pointer::WlPointer,
        event: wl_pointer::Event,
        _data: &(),
        _conn: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        use wl_pointer::Event;
        match event {
            Event::Enter { surface, surface_x, .. } => {
                if let Some(idx) = state.bars.iter().position(|b| b.surface.as_ref() == Some(&surface)) {
                    state.pointer_focus = Some(idx);
                    state.pointer_x = surface_x;
                    state.update_hover(idx, qh);
                }
            }
            Event::Leave { surface, .. } => {
                if let Some(idx) = state.bars.iter().position(|b| b.surface.as_ref() == Some(&surface)) {
                    if state.pointer_focus == Some(idx) { state.pointer_focus = None; }
                    state.clear_hover();
                }
            }
            Event::Motion { surface_x, .. } => {
                if let Some(idx) = state.pointer_focus {
                    state.pointer_x = surface_x;
                    state.update_hover(idx, qh);
                }
            }
            Event::Button { button, state: btn_state, .. } => {
                use wayland_client::protocol::wl_pointer::ButtonState;
                if !matches!(btn_state, wayland_client::WEnum::Value(ButtonState::Pressed)) {
                    return;
                }
                let Some(idx) = state.pointer_focus else { return; };
                // evdev button codes → 1=left, 2=middle, 3=right.
                let n = match button { 0x110 => 1, 0x112 => 2, 0x111 => 3, _ => return };
                if let Some(k) = state.block_at(idx, state.pointer_x) {
                    state.spawn_click(k, n);
                }
            }
            _ => {}
        }
    }
}

// The hover widget's own layer surface dispatches here (keyed by `WidgetTag`),
// separate from the bars' layer surfaces (keyed by `usize`).
impl Dispatch<ZwlrLayerSurfaceV1, WidgetTag> for State {
    fn event(state: &mut Self, surface: &ZwlrLayerSurfaceV1, event: zwlr_layer_surface_v1::Event,
             _tag: &WidgetTag, _: &Connection, qh: &QueueHandle<Self>) {
        let is_current = matches!(&state.widget, Some(w) if &w.layer_surface == surface);
        match event {
            zwlr_layer_surface_v1::Event::Configure { serial, .. } => {
                surface.ack_configure(serial);
                if is_current {
                    if let Some(w) = &mut state.widget { w.configured = true; }
                    state.draw_widget(qh);
                }
            }
            zwlr_layer_surface_v1::Event::Closed if is_current => {
                state.clear_hover();
            }
            _ => {}
        }
    }
}

impl State {
    /// Block index under a surface-local x-position on bar `idx`, if any.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    fn block_at(&self, idx: usize, surface_x: f64) -> Option<usize> {
        let bar = self.bars.get(idx)?;
        let full_w = *bar.block_pix_ends.last()?;
        if full_w == 0 { return None; }
        // Surface-local (logical) → buffer pixels.
        let scale = self.cfg.buffer_scale.max(1);
        let x = (surface_x.max(0.0) as u32).saturating_mul(scale);
        // Status is right-aligned: it begins one text-padding in from its
        // right edge, which sits at the bar's right edge.
        let start = bar.width.saturating_sub(bar.textpadding).saturating_sub(full_w);
        let rel = x.checked_sub(start)?;
        bar.block_pix_ends.iter().position(|&end| rel < end)
    }

    /// Recompute which block the pointer hovers on bar `idx`.  When it crosses
    /// into a block with a `hover_command`, (re)build the floating widget.
    fn update_hover(&mut self, idx: usize, qh: &QueueHandle<Self>) {
        let target = self.block_at(idx, self.pointer_x).map(|k| (idx, k));
        if target == self.hover_target { return; }
        self.hover_target = target;
        self.close_widget();
        if let Some((bar_idx, block_idx)) = target {
            let cmd = self.block_actions.get(block_idx)
                .map(|a| a.hover_command.clone()).unwrap_or_default();
            if !cmd.is_empty() {
                self.open_widget(bar_idx, block_idx, &cmd, qh);
            }
        }
    }

    /// Forget the hover target and tear down any open widget.
    fn clear_hover(&mut self) {
        self.hover_target = None;
        self.close_widget();
    }

    /// Destroy the floating widget's surfaces, if any.
    fn close_widget(&mut self) {
        if let Some(w) = self.widget.take() {
            w.layer_surface.destroy();
            w.surface.destroy();
        }
    }

    /// Run a block's `hover_command` and pop up a widget showing its output,
    /// anchored just below (or above) the block on the bar's output.
    #[allow(clippy::cast_possible_truncation)]
    fn open_widget(&mut self, bar_idx: usize, block_idx: usize, cmd: &str, qh: &QueueHandle<Self>) {
        let (Some(compositor), Some(layer_shell)) = (self.compositor.clone(), self.layer_shell.clone())
        else { return };

        // Run the command (fast utilities like `cal`; a short blocking call).
        let text = std::process::Command::new("sh")
            .arg("-c").arg(cmd)
            .env("WAYLAND_DISPLAY", &self.wayland_socket)
            .output().ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default();
        let mut lines: Vec<String> = text.trim_end_matches('\n').lines().map(str::to_owned).collect();
        while lines.last().is_some_and(|l| l.trim().is_empty()) { lines.pop(); }
        if lines.is_empty() { return; }

        // Widget geometry in buffer pixels (same coordinate space as the bar).
        let pad = self.textpadding;
        let line_h = self.font.height().max(1);
        // Measure the *visible* text: ANSI escapes (from e.g. `cal --color=always`)
        // are stripped before drawing, so they must not count toward the width.
        let text_w = lines.iter()
            .map(|l| self.font.measure(&azoth_render::strip_ansi(l), 0))
            .max().unwrap_or(0);
        if text_w == 0 { return; }
        let width  = text_w + pad * 2;
        let height = line_h * lines.len() as u32 + pad * 2;

        // Align the widget's right edge to the block's right edge on the bar.
        let bar = &self.bars[bar_idx];
        let Some(&full_w) = bar.block_pix_ends.last() else { return };
        if full_w == 0 { return; }
        let text_start  = bar.width.saturating_sub(bar.textpadding).saturating_sub(full_w);
        let block_right = text_start + bar.block_pix_ends.get(block_idx).copied().unwrap_or(full_w);
        let margin_right_buf = bar.width.saturating_sub(block_right);
        let bottom = bar.bottom;
        let bar_h  = bar.height;
        let wl_output = bar.wl_output.clone();

        // Layer-shell size/margins are logical; the buffer is in physical pixels.
        let scale = self.cfg.buffer_scale.max(1);
        let w_logical = width.div_ceil(scale);
        let h_logical = height.div_ceil(scale);
        let margin_right = (margin_right_buf / scale).cast_signed();
        let margin_v     = (bar_h / scale).cast_signed();

        let surface = compositor.create_surface(qh, ());
        let anchor = Anchor::Right | if bottom { Anchor::Bottom } else { Anchor::Top };
        let ls = layer_shell.get_layer_surface(
            &surface, Some(&wl_output),
            zwlr_layer_shell_v1::Layer::Top, "dwlb-widget".to_string(), qh, WidgetTag,
        );
        ls.set_size(w_logical, h_logical);
        ls.set_anchor(anchor);
        // set_margin(top, right, bottom, left): push the widget clear of the bar.
        if bottom {
            ls.set_margin(0, margin_right, margin_v, 0);
        } else {
            ls.set_margin(margin_v, margin_right, 0, 0);
        }
        // -1: anchor to the real output edge, ignoring the bar's exclusive zone,
        // so `margin_v` alone positions the widget flush against the bar.
        ls.set_exclusive_zone(-1);
        ls.set_keyboard_interactivity(zwlr_layer_surface_v1::KeyboardInteractivity::None);
        surface.commit();

        let stride = width * 4;
        self.widget = Some(Widget {
            surface, layer_surface: ls, lines,
            width, height, stride,
            bufsize: stride as usize * height as usize,
            configured: false,
        });
    }

    /// Render the current widget's text into an SHM buffer and attach it.
    fn draw_widget(&self, qh: &QueueHandle<Self>) {
        let Some(w) = &self.widget else { return };
        if !w.configured { return; }

        // Round the widget's corners to match windows when the feature is on.
        // corner_radius is logical; scale it to the widget's buffer pixels.
        #[cfg(feature = "rounded-corners")]
        let corner_radius = crate::config::get().corner_radius * self.cfg.buffer_scale.max(1);
        #[cfg(not(feature = "rounded-corners"))]
        let corner_radius = 0;

        let mut pixels = vec![0u32; w.bufsize / 4];
        azoth_render::render_text_widget(&mut pixels, &azoth_render::TextWidgetParams {
            width: w.width, height: w.height, stride: w.stride,
            padding: self.textpadding,
            line_height: self.font.height().max(1),
            corner_radius,
            fg:     &self.cfg.inactive_fg_color,
            bg:     &self.cfg.inactive_bg_color,
            border: &self.cfg.active_fg_color,
            lines:  &w.lines,
            font:   &self.font,
        });

        let fd: OwnedFd = match allocate_shm_file(w.bufsize as u64) {
            Ok(fd) => fd,
            Err(e) => { tracing::warn!("[bar] widget shm alloc: {e}"); return; }
        };
        if let Err(e) = write_all_fd(&fd, bytemuck::cast_slice(&pixels)) {
            tracing::warn!("[bar] widget write pixels: {e}"); return;
        }
        let Some(shm) = &self.shm else { return; };
        #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
        let pool = shm.create_pool(fd.as_fd(), w.bufsize as i32, qh, ());
        let buffer = pool.create_buffer(
            0, w.width.cast_signed(), w.height.cast_signed(),
            w.stride.cast_signed(), wl_shm::Format::Argb8888, qh, (),
        );
        pool.destroy();
        w.surface.set_buffer_scale(self.cfg.buffer_scale.cast_signed());
        w.surface.attach(Some(&buffer), 0, 0);
        w.surface.damage_buffer(0, 0, w.width.cast_signed(), w.height.cast_signed());
        w.surface.commit();
    }

    /// Spawn a block's click command, exposing the mouse button as `$BLOCK_BUTTON`.
    fn spawn_click(&self, k: usize, button: u32) {
        let Some(action) = self.block_actions.get(k) else { return };
        if action.click.is_empty() { return; }
        // Same environment fix-ups as the prompt launcher so Chromium/Electron
        // apps find the Wayland session.
        let mut command = std::process::Command::new("sh");
        command
            .arg("-c")
            .arg(&action.click)
            .env_remove("DISPLAY")
            .env("WAYLAND_DISPLAY", &self.wayland_socket)
            .env("XDG_SESSION_TYPE", "wayland")
            .env("XDG_CURRENT_DESKTOP", "rwl")
            .env("BLOCK_BUTTON", button.to_string());
        {
            let cfg = crate::config::get();
            if let Some(ref theme) = cfg.cursor_theme {
                command.env("XCURSOR_THEME", theme);
            }
            if cfg.cursor_size > 0 {
                command.env("XCURSOR_SIZE", cfg.cursor_size.to_string());
            }
        }
        command.spawn().ok();
    }
}

/// Extract per-block click/hover behaviour from the bar settings.
fn block_actions_from_settings(s: &BarSettings) -> Vec<BlockAction> {
    s.blocks.iter().map(|b| BlockAction {
        click: b.command_click.clone(),
        hover_command: b.hover_command.clone(),
    }).collect()
}

/// Pixel x-offset (relative to status text start) at which each block ends.
fn compute_pix_ends(font: &Font, raw: &str, bounds: &[u32]) -> Vec<u32> {
    bounds.iter().map(|&b| {
        let mut end = (b as usize).min(raw.len());
        while end > 0 && !raw.is_char_boundary(end) { end -= 1; }
        font.measure(&raw[..end], 0)
    }).collect()
}

impl State {
    #[allow(clippy::too_many_lines)]
    fn handle_key_input(state: &mut Self, idx: usize, key: u32) {
        match key {
            1 => {
                let was_hidden = state.bars[idx].was_hidden_before_prompt;
                let bar = &mut state.bars[idx];
                bar.is_prompt_active = false;
                bar.prompt_buffer.clear();
                bar.history_index = None;
                if let Some(ls) = &bar.layer_surface {
                    ls.set_keyboard_interactivity(zwlr_layer_surface_v1::KeyboardInteractivity::None);
                    if let Some(surf) = &bar.surface { surf.commit(); }
                }
                bar.redraw = true;
                if was_hidden {
                    state.hide_bar(idx);
                }
            }
            28 => {
                let was_hidden = state.bars[idx].was_hidden_before_prompt;
                let bar = &mut state.bars[idx];
                bar.is_prompt_active = false;
                let cmd = bar.prompt_buffer.trim().to_string();
                bar.history_index = None;
                bar.autocomplete_query = None;
                if !cmd.is_empty() {
                    // Mirror `Rwl::configure_child`: without these, Chromium/
                    // Electron apps (chromium, signal-desktop, whatsapp-electron)
                    // default to the X11 ozone backend, fail to find an X server
                    // (DISPLAY is stripped, no XWayland) and exit. XDG_SESSION_TYPE
                    // tells them to use Wayland instead. Wayland-native apps (foot,
                    // firefox) work off WAYLAND_DISPLAY alone, which is why only the
                    // Chromium-family launches were failing.
                    let mut command = std::process::Command::new("sh");
                    command
                        .arg("-c")
                        .arg(&cmd)
                        .env_remove("DISPLAY")
                        .env("WAYLAND_DISPLAY", &state.wayland_socket)
                        .env("XDG_SESSION_TYPE", "wayland")
                        .env("XDG_CURRENT_DESKTOP", "rwl");
                    {
                        let cfg = crate::config::get();
                        if let Some(ref theme) = cfg.cursor_theme {
                            command.env("XCURSOR_THEME", theme);
                        }
                        if cfg.cursor_size > 0 {
                            command.env("XCURSOR_SIZE", cfg.cursor_size.to_string());
                        }
                    }
                    command.spawn().ok();
                    if bar.prompt_history.last() != Some(&cmd) {
                        bar.prompt_history.push(cmd);
                    }
                }
                bar.prompt_buffer.clear();
                if let Some(ls) = &bar.layer_surface {
                    ls.set_keyboard_interactivity(zwlr_layer_surface_v1::KeyboardInteractivity::None);
                    if let Some(surf) = &bar.surface { surf.commit(); }
                }
                bar.redraw = true;
                if was_hidden {
                    state.hide_bar(idx);
                }
            }
            14 => {
                let bar = &mut state.bars[idx];
                bar.prompt_buffer.pop();
                bar.history_index = None;
                bar.autocomplete_query = None;
                bar.redraw = true;
            }
            15 => {
                let bar = &mut state.bars[idx];
                if !bar.prompt_buffer.is_empty() {
                    // On the first Tab press, anchor the query on the current input.
                    // On subsequent Tab presses, cycle through the same candidates.
                    if bar.autocomplete_query.is_none() {
                        bar.autocomplete_query = Some(bar.prompt_buffer.clone());
                        bar.autocomplete_index = 0;
                    } else {
                        bar.autocomplete_index += 1;
                    }
                    if let Some(q) = &bar.autocomplete_query
                        && let Some(completed) = attempt_autocomplete(q, bar.autocomplete_index)
                    {
                        bar.prompt_buffer = completed;
                        bar.redraw = true;
                    }
                }
            }
            103 => {
                let bar = &mut state.bars[idx];
                if !bar.prompt_history.is_empty() {
                    match bar.history_index {
                        None => {
                            bar.history_temp_buffer = bar.prompt_buffer.clone();
                            let last_idx = bar.prompt_history.len() - 1;
                            bar.history_index = Some(last_idx);
                            bar.prompt_buffer = bar.prompt_history[last_idx].clone();
                        }
                        Some(h_idx) if h_idx > 0 => {
                            bar.history_index = Some(h_idx - 1);
                            bar.prompt_buffer = bar.prompt_history[h_idx - 1].clone();
                        }
                        _ => {}
                    }
                    bar.redraw = true;
                }
            }
            108 => {
                let bar = &mut state.bars[idx];
                if let Some(h_idx) = bar.history_index {
                    if h_idx + 1 < bar.prompt_history.len() {
                        let next_idx = h_idx + 1;
                        bar.history_index = Some(next_idx);
                        bar.prompt_buffer = bar.prompt_history[next_idx].clone();
                    } else {
                        bar.history_index = None;
                        bar.prompt_buffer = bar.history_temp_buffer.clone();
                    }
                    bar.redraw = true;
                }
            }
            _ => {
                let bar = &mut state.bars[idx];
                bar.autocomplete_query = None;
                let ch = match key {
                    2..=11 => char::from_digit(if key == 11 { 0 } else { key - 1 }, 10),
                    16 => Some('q'), 17 => Some('w'), 18 => Some('e'), 19 => Some('r'),
                    20 => Some('t'), 21 => Some('y'), 22 => Some('u'), 23 => Some('i'),
                    24 => Some('o'), 25 => Some('p'), 30 => Some('a'), 31 => Some('s'),
                    32 => Some('d'), 33 => Some('f'), 34 => Some('g'), 35 => Some('h'),
                    36 => Some('j'), 37 => Some('k'), 38 => Some('l'), 44 => Some('z'),
                    45 => Some('x'), 46 => Some('c'), 47 => Some('v'), 48 => Some('b'),
                    49 => Some('n'), 50 => Some('m'), 57 => Some(' '),
                    _ => None,
                };
                if let Some(c) = ch {
                    bar.history_index = None;
                    bar.prompt_buffer.push(c);
                    bar.redraw = true;
                }
            }
        }
    }
}

// ── dwl-ipc Dispatch impls ────────────────────────────────────────────────────

impl Dispatch<dwl_ipc::zdwl_ipc_manager_v2::ZdwlIpcManagerV2, ()> for State {
    fn event(state: &mut Self, _: &dwl_ipc::zdwl_ipc_manager_v2::ZdwlIpcManagerV2,
             event: dwl_ipc::zdwl_ipc_manager_v2::Event, &(): &(), _: &Connection,
             _: &QueueHandle<Self>) {
        use dwl_ipc::zdwl_ipc_manager_v2::Event;
        match event {
            Event::Tags { amount } => {
                let amt = amount as usize;
                let start = state.tags.len();
                state.tags.extend((start + 1..=amt).map(|n| n.to_string()));
                state.tags.truncate(amt);
            }
            Event::Layout { name } => {
                state.layouts.push(Arc::from(name));
            }
        }
    }
}

impl Dispatch<dwl_ipc::zdwl_ipc_output_v2::ZdwlIpcOutputV2, usize> for State {
    fn event(state: &mut Self, _: &dwl_ipc::zdwl_ipc_output_v2::ZdwlIpcOutputV2,
             event: dwl_ipc::zdwl_ipc_output_v2::Event, &idx: &usize,
             _: &Connection, qh: &QueueHandle<Self>) {
        use dwl_ipc::zdwl_ipc_output_v2::Event;
        match event {
            Event::ToggleVisibility => {
                if state.bars[idx].surface.is_none() {
                    state.show_bar(idx, qh);
                } else {
                    state.hide_bar(idx);
                }
            }
            Event::Active { active } => {
                state.bars[idx].sel = active;
            }
            Event::Tag { tag, state: tag_state, clients, focused: _ } => {
                use dwl_ipc::zdwl_ipc_output_v2::TagState;
                use wayland_client::WEnum;
                let mask = 1u32 << tag;
                match tag_state {
                    WEnum::Value(TagState::Active) => { state.bars[idx].pending_ctags |= mask; }
                    WEnum::Value(TagState::Urgent) => { state.bars[idx].pending_urg   |= mask; }
                    _ => {}
                }
                if clients > 0 { state.bars[idx].pending_mtags |= mask; }
            }
            Event::Layout { layout } => {
                state.bars[idx].layout_idx = layout;
                state.bars[idx].layout = state.layouts.get(layout as usize).map(Arc::clone);
            }
            Event::Title { title } => {
                state.bars[idx].window_title = Some(title);
            }
            Event::LayoutSymbol { layout } => {
                state.bars[idx].layout = Some(Arc::from(layout));
            }
            Event::Frame => {
                let bar = &mut state.bars[idx];
                bar.ctags = bar.pending_ctags;
                bar.mtags = bar.pending_mtags;
                bar.urg   = bar.pending_urg;
                bar.pending_ctags = 0;
                bar.pending_mtags = 0;
                bar.pending_urg   = 0;
                bar.redraw = true;
            }
            _ => {}
        }
    }
}

// ── Low-level I/O helpers ─────────────────────────────────────────────────────

/// Scan `$PATH` for executables whose name starts with `query`; return the
/// entry at `index % matches.len()` for tab-cycling autocomplete.
#[must_use]
pub fn attempt_autocomplete(query: &str, index: usize) -> Option<String> {
    use std::collections::HashSet;
    let mut commands = HashSet::new();
    let mut paths_to_search = Vec::new();

    if let Some(path_var) = env::var_os("PATH") {
        paths_to_search.extend(env::split_paths(&path_var));
    }

    let home = env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    let extra_paths = vec![
        format!("{home}/.local/bin"),
        format!("{home}/.nix-profile/bin"),
        "/run/current-system/sw/bin".to_string(),
        "/usr/bin".to_string(),
        "/usr/local/bin".to_string(),
    ];

    for path in extra_paths {
        let p = PathBuf::from(path);
        if p.exists() && !paths_to_search.contains(&p) {
            paths_to_search.push(p);
        }
    }

    for path_dir in paths_to_search {
        if let Ok(entries) = fs::read_dir(path_dir) {
            for entry in entries.flatten() {
                if let Ok(metadata) = entry.metadata() {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::MetadataExt;
                        if (metadata.is_file() || metadata.is_symlink()) && (metadata.mode() & 0o111) != 0
                            && let Some(name) = entry.file_name().to_str()
                        {
                            commands.insert(name.to_string());
                        }
                    }
                    #[cfg(not(unix))]
                    if metadata.is_file() {
                        if let Some(name) = entry.file_name().to_str() {
                            commands.insert(name.to_string());
                        }
                    }
                }
            }
        }
    }

    let mut matches: Vec<String> = commands
        .into_iter()
        .filter(|cmd| cmd.starts_with(query))
        .collect();
    matches.sort();

    if matches.is_empty() {
        return None;
    }
    let target_idx = index % matches.len();
    matches.into_iter().nth(target_idx)
}

fn parse_hex_color(s: &str) -> Option<azoth_render::RustColor> {
    let hex = s.strip_prefix('#')?;
    let n = u32::from_str_radix(hex, 16).ok()?;
    Some(match hex.len() {
        6 => azoth_render::RustColor::opaque(
            ((n >> 16) & 0xff) as u8,
            ((n >>  8) & 0xff) as u8,
            (n & 0xff) as u8,
        ),
        8 => azoth_render::RustColor {
            red:   (((n >> 24) & 0xff) as u16) * 0x101,
            green: (((n >> 16) & 0xff) as u16) * 0x101,
            blue:  (((n >>  8) & 0xff) as u16) * 0x101,
            alpha: ((n & 0xff) as u16) * 0x101,
        },
        _ => return None,
    })
}

fn write_all_fd(fd: &OwnedFd, mut data: &[u8]) -> Result<(), rustix::io::Errno> {
    while !data.is_empty() {
        match rustix::io::write(fd, data) {
            Ok(n)            => data = &data[n..],
            Err(Errno::INTR) => {}
            Err(e)           => return Err(e),
        }
    }
    Ok(())
}

// ── Socket helpers ────────────────────────────────────────────────────────────

/// Directory holding the bar control sockets: `$XDG_RUNTIME_DIR/dwlb`.
///
/// Returns `None` when `XDG_RUNTIME_DIR` is unset — we refuse to fall back to a
/// world-accessible location such as `/tmp`, where another local user could
/// pre-create the directory or connect to the control sockets.
fn socket_dir() -> Option<PathBuf> {
    let dir = env::var_os("XDG_RUNTIME_DIR")?;
    if dir.is_empty() {
        return None;
    }
    Some(PathBuf::from(dir).join("dwlb"))
}

fn find_socket_path(dir: &Path) -> Option<PathBuf> {
    (0..50u32)
        .map(|i| dir.join(format!("dwlb-{i}")))
        .find(|p| UnixStream::connect(p).is_err())
}

#[allow(clippy::too_many_lines)]
fn handle_socket_command(buf: &str, state: &mut State, qh: &QueueHandle<State>, notification_flag: &AtomicBool) {
    let mut words = buf.split_whitespace();
    let Some(target) = words.next() else { return };
    let indices: Vec<usize> = match target {
        "all"      => state.bars.iter().enumerate().filter(|(_, b)| !b.dead).map(|(i,_)| i).collect(),
        "selected" => state.bars.iter().enumerate().filter(|(_, b)| !b.dead && b.sel != 0).map(|(i,_)| i).collect(),
        name       => state.bars.iter().enumerate().filter(|(_, b)| !b.dead && b.name.as_deref() == Some(name)).map(|(i,_)| i).collect(),
    };
    if indices.is_empty() { return; }
    let Some(cmd) = words.next() else { return };
    let skip = buf.splitn(3, char::is_whitespace).take(2).map(|s| s.len() + 1).sum::<usize>();
    let rest = if skip < buf.len() { buf[skip..].trim_end_matches('\n') } else { "" };

    match cmd {
        "prompt" => {
            for &i in &indices {
                let is_hidden = state.bars[i].surface.is_none();
                state.bars[i].was_hidden_before_prompt = is_hidden;
                if is_hidden {
                    state.show_bar(i, qh);
                }
                let bar = &mut state.bars[i];
                bar.is_prompt_active = true;
                bar.prompt_buffer.clear();
                bar.autocomplete_query = None;
                bar.autocomplete_index = 0;
                bar.history_index = None;
                if let Some(ls) = &bar.layer_surface {
                    ls.set_keyboard_interactivity(zwlr_layer_surface_v1::KeyboardInteractivity::OnDemand);
                }
                if let Some(surface) = &bar.surface {
                    surface.commit();
                }
                bar.redraw = true;
            }
        }
        "status" => {
            if rest.is_empty() { return; }
            let parsed = Arc::new(parse_into_customtext(rest, &state.cfg));
            for &i in &indices {
                let bar = &mut state.bars[i];
                bar.status = Arc::clone(&parsed);
                // Externally-fed status has no per-block geometry.
                bar.block_pix_ends.clear();
                bar.redraw = true;
            }
            state.clear_hover();
        }
        "title" => {
            if rest.is_empty() { return; }
            let mut remaining = rest.trim_end_matches('\n');
            let mut secs = 3u64;
            let mut color: Option<azoth_render::RustColor> = None;
            // optional duration
            if let Some((tok, after)) = remaining.split_once(' ')
                && let Ok(n) = tok.parse::<u64>()
            {
                secs = n.clamp(1, 3600);
                remaining = after;
            }
            // optional #rrggbb[aa] color
            if remaining.starts_with('#')
                && let Some((tok, after)) = remaining.split_once(' ')
                && let Some(c) = parse_hex_color(tok)
            {
                color = Some(c);
                remaining = after;
            }
            if remaining.is_empty() { return; }
            let text = remaining.to_owned();
            let expire = std::time::Instant::now() + std::time::Duration::from_secs(secs);
            for &i in &indices {
                if state.bars[i].surface.is_none() {
                    state.bars[i].was_hidden_before_title = true;
                    state.show_bar(i, qh);
                }
                state.bars[i].title_override = Some((text.clone(), expire, color));
                state.bars[i].redraw = true;
            }
            notification_flag.store(true, Ordering::Relaxed);
        }
        "show"              => { for &i in &indices { if  state.bars[i].surface.is_none() { state.show_bar(i, qh); } } }
        "hide"              => { for &i in &indices { if  state.bars[i].surface.is_some() { state.hide_bar(i); } } }
        "toggle-visibility" => { for &i in &indices { if state.bars[i].surface.is_none() { state.show_bar(i, qh); } else { state.hide_bar(i); } } }
        "set-top"           => { for &i in &indices { state.set_top(i); } }
        "set-bottom"        => { for &i in &indices { state.set_bottom(i); } }
        "toggle-location"   => { for &i in &indices { if state.bars[i].bottom { state.set_top(i); } else { state.set_bottom(i); } } }
        "reload" => {
            let settings = &crate::config::get().bar;
            state.cfg = Config::from_settings(settings);
            if !settings.tag_names.is_empty() {
                state.tags.clone_from(&settings.tag_names);
            }
            if let Ok(mut g) = state.shared_blocks.lock() {
                let new_blocks = config::blocks_from_settings(settings);
                let new_delim  = settings.blocks_delim.clone();
                *g = (new_blocks, new_delim, true);
            }
            state.block_actions = block_actions_from_settings(settings);

            // Reload font and recompute bar geometry if font or vertical_padding changed.
            let dpi = 96 * state.cfg.buffer_scale;
            if let Some(new_font) = Font::load(state.cfg.font_str.split(',').map(str::trim), dpi) {
                let font_height = new_font.height();
                if font_height > 0 && font_height <= 500 {
                    let new_textpadding = font_height / 2;
                    let new_bar_height  = font_height / state.cfg.buffer_scale + state.cfg.vertical_padding * 2;
                    state.font         = new_font;
                    state.bar_height   = new_bar_height;
                    state.textpadding  = new_textpadding;
                    for bar in &mut state.bars {
                        bar.textpadding = new_textpadding;
                        bar.height      = new_bar_height;
                    }
                    // Resize visible layer surfaces so the compositor sends a configure.
                    for idx in 0..state.bars.len() {
                        if state.bars[idx].surface.is_some() {
                            if let Some(ls) = &state.bars[idx].layer_surface {
                                ls.set_size(0, new_bar_height);
                                ls.set_exclusive_zone(new_bar_height.cast_signed());
                            }
                            if let Some(surface) = &state.bars[idx].surface {
                                surface.commit();
                            }
                        }
                    }
                }
            }

            // Apply bottom/top change.
            let want_bottom = state.cfg.bottom;
            for i in 0..state.bars.len() {
                if want_bottom && !state.bars[i].bottom {
                    state.set_bottom(i);
                } else if !want_bottom && state.bars[i].bottom {
                    state.set_top(i);
                }
            }

            for bar in &mut state.bars { bar.redraw = true; }
            // Block geometry/actions changed; drop any open hover widget.
            state.clear_hover();
        }
        _ => {}
    }
}

fn process_stdin_buf(buf: &str, bars: &mut [Bar]) {
    for line in buf.lines() {
        let Some((output, rest)) = line.split_once(' ') else { continue };
        let Some(bar) = bars.iter_mut().find(|b| !b.dead && b.name.as_deref() == Some(output)) else { continue };
        let (cmd, arg) = rest.split_once(' ').unwrap_or((rest, ""));
        match cmd {
            "tags"   => {
                let ns: Vec<u32> = arg.split_whitespace().filter_map(|s| s.parse().ok()).collect();
                if ns.len() >= 4 {
                    if ns[0] != bar.ctags { bar.ctags = ns[0]; bar.redraw = true; }
                    if ns[1] != bar.mtags { bar.mtags = ns[1]; bar.redraw = true; }
                    if ns[3] != bar.urg   { bar.urg   = ns[3]; bar.redraw = true; }
                }
            }
            "layout"      => { bar.layout       = Some(Arc::from(arg)); bar.redraw = true; }
            "title"       => { bar.window_title = Some(arg.to_owned()); bar.title_is_error = false; bar.redraw = true; }
            "title_error" => { bar.window_title = Some(arg.to_owned()); bar.title_is_error = true;  bar.redraw = true; }
            "selmon"  => { let v = arg.trim().parse().unwrap_or(0); if v != bar.sel { bar.sel = v; bar.redraw = true; } }
            _ => {}
        }
    }
}

// ── Socket cleanup guard ──────────────────────────────────────────────────────

struct SocketCleanup(PathBuf);
impl Drop for SocketCleanup {
    fn drop(&mut self) { let _ = fs::remove_file(&self.0); }
}

// ── Event loop helpers ────────────────────────────────────────────────────────

fn wait_for_configure(state: &mut State, qh: &QueueHandle<State>, eq: &mut EventQueue<State>) {
    state.ready = true;
    for idx in 0..state.bars.len() {
        if !state.bars[idx].dead { state.setup_bar(idx, qh); }
    }
    eq.flush().ok();
    if eq.roundtrip(state).is_err() { return; }
    for _attempt in 0..10 {
        let unconfigured = state.bars.iter().filter(|b| b.surface.is_some() && !b.configured).count();
        if unconfigured == 0 { break; }
        if eq.roundtrip(state).is_err() { break; }
    }
    for idx in 0..state.bars.len() {
        if state.bars[idx].configured && state.bars[idx].surface.is_some() {
            state.draw_frame_with_qh(idx, qh);
            state.bars[idx].redraw = false;
        }
    }
    eq.flush().ok();
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines, clippy::needless_pass_by_value)]
fn run_event_loop(
    conn: &Connection,
    mut event_queue: EventQueue<State>,
    qh: &QueueHandle<State>,
    mut state: State,
    listener: &UnixListener,
    stop: &Arc<AtomicBool>,
    blocks_pipe: &OwnedFd,
    latest_status: &SharedStatus,
    ipc_fd: OwnedFd,
    notification_flag: &AtomicBool,
) {
    match fcntl_getfl(&ipc_fd) {
        Ok(flags) => { fcntl_setfl(&ipc_fd, flags | OFlags::NONBLOCK).ok(); }
        Err(e) => { tracing::warn!("[bar] fcntl ipc_fd: {e}"); }
    }

    let mut to_redraw: Vec<usize> = Vec::new();
    while !stop.load(Ordering::Relaxed) {
        if let Err(e) = event_queue.flush() {
            tracing::warn!("[bar] Wayland flush: {e}"); break;
        }

        let read_guard = event_queue.prepare_read();

        let mut fds = [
            PollFd::new(conn,          PollFlags::IN),
            PollFd::new(listener,      PollFlags::IN),
            PollFd::new(&ipc_fd,       PollFlags::IN),
            PollFd::new(blocks_pipe,   PollFlags::IN),
        ];
        let now = std::time::Instant::now();
        let min_override = state.bars.iter()
            .filter_map(|b| b.title_override.as_ref())
            .filter_map(|(_, until, _)| until.checked_duration_since(now))
            .min();
        let timeout_ts = min_override.map(|d| Timespec {
            tv_sec: d.as_secs().cast_signed(),
            tv_nsec: i64::from(d.subsec_nanos()),
        });
        match poll(&mut fds, timeout_ts.as_ref()) {
            Ok(_) => {}
            Err(Errno::INTR) => { drop(read_guard); continue; }
            Err(e) => { tracing::warn!("[bar] poll: {e}"); drop(read_guard); break; }
        }

        let wl_ready     = fds[0].revents().contains(PollFlags::IN);
        let sock_ready   = fds[1].revents().contains(PollFlags::IN);
        let ipc_ready    = fds[2].revents().contains(PollFlags::IN);
        let blocks_ready = fds[3].revents().contains(PollFlags::IN);

        if wl_ready {
            if let Some(guard) = read_guard
                && let Err(e) = guard.read()
            {
                match &e {
                    // Nothing to read yet — normal, keep looping.
                    wayland_client::backend::WaylandError::Io(io_e)
                        if io_e.kind() == io::ErrorKind::WouldBlock => {}
                    // Compositor closed the socket (it's shutting down).
                    // This is an expected exit, not an error — leave quietly.
                    wayland_client::backend::WaylandError::Io(io_e)
                        if matches!(
                            io_e.kind(),
                            io::ErrorKind::BrokenPipe
                                | io::ErrorKind::ConnectionReset
                                | io::ErrorKind::UnexpectedEof
                        ) => break,
                    _ => { tracing::warn!("[bar] Wayland read: {e}"); break; }
                }
            }
        } else {
            drop(read_guard);
        }

        if let Err(e) = event_queue.dispatch_pending(&mut state) {
            tracing::warn!("[bar] Wayland dispatch: {e}"); break;
        }

        if sock_ready {
            let mut buf = String::new();
            loop {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        buf.clear();
                        // Bound the read in time and size so a stalled or
                        // flooding client can't hang the bar thread or exhaust
                        // memory (defence in depth; the socket dir is 0700).
                        let _ = stream.set_read_timeout(Some(std::time::Duration::from_millis(200)));
                        let _ = io::Read::by_ref(&mut stream).take(64 * 1024).read_to_string(&mut buf);
                        if !buf.is_empty() { handle_socket_command(&buf, &mut state, qh, notification_flag); }
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                    Err(_) => break,
                }
            }
        }

        if ipc_ready {
            let mut tmp = [0u8; 4096];
            loop {
                match rustix::io::read(&ipc_fd, &mut tmp) {
                    Ok(0) => { stop.store(true, Ordering::Relaxed); break; }
                    Ok(n) => {
                        if let Ok(s) = std::str::from_utf8(&tmp[..n]) {
                            state.stdin_buf.push_str(s);
                        }
                    }
                    Err(Errno::AGAIN | _) => break,
                }
            }
            if !state.stdin_buf.is_empty() {
                let buf = mem::take(&mut state.stdin_buf);
                if let Some(nl) = buf.rfind('\n') {
                    process_stdin_buf(&buf[..=nl], &mut state.bars);
                    buf[nl+1..].clone_into(&mut state.stdin_buf);
                } else {
                    state.stdin_buf = buf;
                }
            }
        }

        if blocks_ready {
            let mut drain = [0u8; 64];
            loop {
                match rustix::io::read(blocks_pipe, &mut drain) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
            }
            let (new_status, new_bounds) = std::mem::take(&mut *latest_status.lock().unwrap_or_else(std::sync::PoisonError::into_inner));
            if !new_status.is_empty() {
                let parsed = Arc::new(parse_into_customtext(&new_status, &state.cfg));
                let pix_ends = compute_pix_ends(&state.font, &new_status, &new_bounds);
                for bar in &mut state.bars {
                    bar.status = Arc::clone(&parsed);
                    bar.block_pix_ends.clone_from(&pix_ends);
                    bar.redraw = true;
                }
                // The status width changed, so the block under the pointer may have too.
                if let Some(idx) = state.pointer_focus {
                    state.update_hover(idx, qh);
                }
            }
        }

        let now = std::time::Instant::now();
        let mut title_expired: Vec<usize> = Vec::new();
        for (idx, bar) in state.bars.iter_mut().enumerate() {
            if matches!(&bar.title_override, Some((_, until, _)) if now >= *until) {
                bar.title_override = None;
                bar.redraw = true;
                if bar.was_hidden_before_title {
                    bar.was_hidden_before_title = false;
                    title_expired.push(idx);
                }
            }
        }
        if !state.bars.iter().any(|b| b.title_override.is_some()) {
            notification_flag.store(false, Ordering::Relaxed);
        }
        for idx in title_expired {
            state.hide_bar(idx);
        }

        to_redraw.clear();
        to_redraw.extend(
            state.bars.iter().enumerate()
                .filter(|(_, b)| b.redraw && b.surface.is_some())
                .map(|(i, _)| i)
        );
        #[allow(clippy::iter_with_drain)]
        for idx in to_redraw.drain(..) {
            state.draw_frame_with_qh(idx, qh);
            state.bars[idx].redraw = false;
        }
    }

    drop(state);
    azoth_render::fini_fcft();
}

// ── IPC helpers callable from the WM main thread ─────────────────────────────

/// Holds the Unix socket path once the bar thread has bound its listener.
static BAR_SOCKET: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();

/// Send a socket command to the bar from any thread (e.g. from a keybind handler).
///
/// `cmd` must be a complete command string such as `"all toggle-visibility"` or
/// `"selected prompt"`.  Silently does nothing if the bar isn't running yet.
pub fn send_bar_command(cmd: &str) {
    use std::io::Write as _;
    use std::os::unix::net::UnixStream;
    if let Some(path) = BAR_SOCKET.get()
        && let Ok(mut s) = UnixStream::connect(path)
    {
        let _ = s.write_all(cmd.as_bytes());
    }
}

// ── Public entry point ────────────────────────────────────────────────────────

/// Spawn the bar as a background thread.  Must be called after
/// `WAYLAND_DISPLAY` is set in the environment.  The caller must pass the
/// read end of the IPC pipe; the compositor writes status lines to the write
/// end via `state.ipc_out`.
pub fn start(reader: PipeReader, settings: BarSettings, wayland_socket: String, notification_flag: Arc<AtomicBool>) {
    let ipc_fd: OwnedFd = reader.into();
    if let Err(e) = thread::Builder::new().name("bar".into()).spawn(move || run_bar(ipc_fd, settings, wayland_socket, &notification_flag)) {
        tracing::warn!("[bar] failed to spawn bar thread: {e}");
    }
}

// ── Bar thread main ───────────────────────────────────────────────────────────

#[allow(clippy::too_many_lines)]
fn run_bar(ipc_fd: OwnedFd, settings: BarSettings, wayland_socket: String, notification_flag: &Arc<AtomicBool>) {
    let cfg = Config::from_settings(&settings);
    let blocks = blocks_from_settings(&settings);
    let block_actions = block_actions_from_settings(&settings);
    let delim  = settings.blocks_delim;
    let initial_tags: Vec<String> = if settings.tag_names.is_empty() {
        DEFAULT_TAG_NAMES.iter().map(|&s| s.to_owned()).collect()
    } else {
        settings.tag_names
    };

    azoth_render::init_fcft();

    let dpi = 96 * cfg.buffer_scale;
    let Some(font) = Font::load(cfg.font_str.split(',').map(str::trim), dpi) else {
        tracing::error!("[bar] Could not load font '{}'", cfg.font_str);
        azoth_render::fini_fcft();
        return;
    };
    let font_height = font.height();
    if font_height == 0 || font_height > 500 {
        tracing::error!("[bar] Bad font height from fcft: {font_height}px");
        drop(font);
        azoth_render::fini_fcft();
        return;
    }
    let textpadding = font_height / 2;
    let bar_height  = font_height / cfg.buffer_scale + cfg.vertical_padding * 2;

    let stop = Arc::new(AtomicBool::new(false));

    // Wake pipe: SIGRTMIN+N handlers write one byte here to wake the blocks poll.
    let (wake_pipe_r, wake_pipe_w) = match rustix::pipe::pipe() {
        Ok(pair) => pair,
        Err(e) => {
            tracing::error!("[bar] wake pipe: {e}");
            drop(font);
            azoth_render::fini_fcft();
            return;
        }
    };
    {
        match fcntl_getfl(&wake_pipe_r) {
            Ok(flags) => { fcntl_setfl(&wake_pipe_r, flags | OFlags::NONBLOCK).ok(); }
            Err(e) => { tracing::warn!("[bar] fcntl wake pipe: {e}"); }
        }
    }

    // Register SIGRTMIN+N handlers for blocks that have a signal number.
    let mut rt_signal_flags: Vec<(u8, Arc<AtomicBool>)> = {
        let mut seen = std::collections::HashSet::new();
        blocks.iter()
            .filter(|b| b.signal > 0)
            .filter_map(|b| seen.insert(b.signal).then_some(b.signal))
            .filter_map(|sig_n| {
                let flag = Arc::new(AtomicBool::new(false));
                let signum = libc::SIGRTMIN() + i32::from(sig_n);
                let ok1 = signal_hook::flag::register(signum, Arc::clone(&flag)).is_ok();
                let wake_dup = rustix::io::dup(&wake_pipe_w).ok()?;
                let ok2 = signal_hook::low_level::pipe::register(signum, wake_dup).is_ok();
                if ok1 && ok2 { Some((sig_n, flag)) } else { None }
            })
            .collect()
    };
    // Keep wake_pipe_w alive in the blocks thread so reload can dup it for new signals.

    // Unix socket for external control (e.g. `azoth -prompt all`).
    let Some(dir) = socket_dir() else {
        tracing::error!("[bar] XDG_RUNTIME_DIR is unset; refusing to create control sockets in a world-accessible location");
        drop(font);
        azoth_render::fini_fcft();
        return;
    };
    fs::create_dir_all(&dir).ok();
    let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o700));

    let Some(socket_path) = find_socket_path(&dir) else {
        tracing::error!("[bar] Could not secure a socket path in {:?}", dir);
        drop(font);
        azoth_render::fini_fcft();
        return;
    };
    let _ = fs::remove_file(&socket_path);
    let listener = match UnixListener::bind(&socket_path) {
        Ok(l) => l,
        Err(e) => {
            tracing::error!("[bar] bind socket: {e}");
            drop(font);
            azoth_render::fini_fcft();
            return;
        }
    };
    if let Err(e) = listener.set_nonblocking(true) {
        tracing::warn!("[bar] set_nonblocking: {e}");
    }
    BAR_SOCKET.set(socket_path.clone()).ok();
    let _cleanup = SocketCleanup(socket_path);

    // Connect to the compositor's socket explicitly rather than via
    // `connect_to_env()`, so the bar does not depend on `WAYLAND_DISPLAY` being
    // present in the (unmutated) process environment.
    let conn = {
        let sock_path = if wayland_socket.starts_with('/') {
            PathBuf::from(&wayland_socket)
        } else if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR") {
            Path::new(&dir).join(&wayland_socket)
        } else {
            tracing::error!("[bar] XDG_RUNTIME_DIR unset; cannot connect to compositor");
            drop(font);
            azoth_render::fini_fcft();
            return;
        };
        match std::os::unix::net::UnixStream::connect(&sock_path)
            .map_err(|e| e.to_string())
            .and_then(|s| Connection::from_socket(s).map_err(|e| e.to_string()))
        {
            Ok(c) => c,
            Err(e) => {
                tracing::error!("[bar] Wayland connect ({}): {e}", sock_path.display());
                drop(font);
                azoth_render::fini_fcft();
                return;
            }
        }
    };
    let mut event_queue: EventQueue<State> = conn.new_event_queue();
    let qh = event_queue.handle();
    conn.display().get_registry(&qh, ());

    let shared_blocks: SharedBlocks = Arc::new(Mutex::new((blocks, delim, false)));

    let mut state = State::new(cfg, font, bar_height, textpadding, wayland_socket, Arc::clone(&shared_blocks));
    state.tags = initial_tags;
    state.block_actions = block_actions;
    if event_queue.roundtrip(&mut state).is_err() {
        tracing::error!("[bar] initial Wayland roundtrip failed");
        azoth_render::fini_fcft();
        return;
    }

    if state.compositor.is_none() || state.shm.is_none()
        || state.layer_shell.is_none() || state.output_manager.is_none()
    {
        tracing::error!("[bar] Compositor is missing required Wayland protocols");
        azoth_render::fini_fcft();
        return;
    }

    // Pipe for blocks thread → event loop notifications.
    let (blocks_pipe_r, blocks_pipe_w) = match rustix::pipe::pipe() {
        Ok(pair) => pair,
        Err(e) => {
            tracing::error!("[bar] blocks pipe: {e}");
            azoth_render::fini_fcft();
            return;
        }
    };
    {
        match fcntl_getfl(&blocks_pipe_r) {
            Ok(flags) => { fcntl_setfl(&blocks_pipe_r, flags | OFlags::NONBLOCK).ok(); }
            Err(e) => { tracing::warn!("[bar] fcntl blocks pipe: {e}"); }
        }
    }

    let latest_status: SharedStatus = Arc::new(Mutex::new((String::new(), Vec::new())));
    let status_for_blocks = Arc::clone(&latest_status);

    let thread_shared = Arc::clone(&shared_blocks);

    let _blocks_thread = thread::spawn(move || {
        // Take a local snapshot so we don't hold the lock during command execution.
        let (mut local_blocks, mut local_delim) = {
            let g = thread_shared.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            (g.0.clone(), g.1.clone())
        };
        let mut cache = blocks::initial_cache(&local_blocks);
        let mut prev_hash = 0u64;
        let mut time = 0u32;
        let one_sec = Timespec { tv_sec: 1, tv_nsec: 0 };
        loop {
            // Check for a config reload: rebuild blocks/delim snapshot and cache.
            {
                let mut g = thread_shared.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                if g.2 {
                    g.2 = false;
                    local_blocks.clone_from(&g.0);
                    local_delim.clone_from(&g.1);
                    drop(g);
                    // Register signal handlers for any new signal numbers not seen at startup.
                    let registered: u32 = rt_signal_flags.iter()
                        .fold(0u32, |acc, (n, _)| acc | (1u32 << u32::from(*n)));
                    let mut new_sigs: u32 = 0;
                    for block in &local_blocks {
                        let sig = block.signal;
                        let bit = 1u32 << u32::from(sig);
                        if sig > 0 && registered & bit == 0 && new_sigs & bit == 0 {
                            new_sigs |= bit;
                            let flag = Arc::new(AtomicBool::new(false));
                            let signum = libc::SIGRTMIN() + i32::from(sig);
                            let ok1 = signal_hook::flag::register(signum, Arc::clone(&flag)).is_ok();
                            let ok2 = rustix::io::dup(&wake_pipe_w)
                                .ok()
                                .and_then(|dup| signal_hook::low_level::pipe::register(signum, dup).ok())
                                .is_some();
                            if ok1 && ok2 {
                                rt_signal_flags.push((sig, flag));
                            }
                        }
                    }
                    cache = blocks::initial_cache(&local_blocks);
                    prev_hash = 0; // force status push on next tick
                }
            }
            let signal_mask: u32 = rt_signal_flags.iter()
                .filter_map(|(n, flag)| flag.swap(false, Ordering::Relaxed).then_some(1u32 << u32::from(*n)))
                .fold(0u32, |acc, bit| acc | bit);
            let new_cache = blocks::update_cache(time, signal_mask, cache, &local_blocks);
            let (new_status, new_bounds) = blocks::build_status_bounds(&new_cache, &local_delim);
            let new_hash = blocks::fast_hash(&new_status);
            if new_hash != prev_hash {
                prev_hash = new_hash;
                *status_for_blocks.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = (new_status, new_bounds);
                let _ = rustix::io::write(&blocks_pipe_w, b"x");
            }
            cache = new_cache;
            time = time.wrapping_add(1);
            let mut wake_fds = [PollFd::new(&wake_pipe_r, PollFlags::IN)];
            match poll(&mut wake_fds, Some(&one_sec)) {
                Ok(_) | Err(Errno::INTR) => {}
                Err(_) => break,
            }
            let mut drain = [0u8; 64];
            loop {
                match rustix::io::read(&wake_pipe_r, &mut drain) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
            }
        }
    });

    wait_for_configure(&mut state, &qh, &mut event_queue);
    run_event_loop(&conn, event_queue, &qh, state, &listener, &stop, &blocks_pipe_r, &latest_status, ipc_fd, notification_flag);
}
