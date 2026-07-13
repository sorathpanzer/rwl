# Architecture

This document describes how rwl is put together at runtime: the central state,
the event loop, the backend abstraction, the render path, and the protocol
handlers. It complements the per-module `//!` doc comments (`cargo doc --open`).

## High-level model

rwl is a single-threaded, event-loop-driven Wayland compositor built on
[Smithay](https://github.com/Smithay/smithay). One large struct — `Rwl` (in
[`src/state.rs`](../src/state.rs)) — holds *all* compositor state and is threaded
through a [`calloop`](https://docs.rs/calloop) event loop as the loop's shared
data. Every Wayland protocol Smithay exposes is implemented as a trait `impl`
on `Rwl` (mostly under [`src/handlers/`](../src/handlers/)).

Two auxiliary threads exist, both optional:

- the **embedded bar** (`bar` feature) runs as a Wayland *client* thread that
  connects back to the compositor's own socket;
- the **PAM worker** (`lock` feature) runs authentication off the event loop so
  its deliberate failure delays never stall rendering.

Everything else — input, layout, rendering, IPC dispatch — happens on the main
event-loop thread.

## Startup sequence

[`src/main.rs`](../src/main.rs) `main()` → `run()`:

1. **`rwl msg` fast-path.** If `argv[1] == "msg"`, dispatch as an IPC *client*
   and exit before any compositor state is created (`ipc` feature).
2. Parse CLI args (`-s`, `-v`, `-d`), init `tracing`, load the Lua config
   (`config::init()`).
3. Require `XDG_RUNTIME_DIR`; create the `calloop` `EventLoop` and the Wayland
   `Display`.
4. Construct `Rwl::new()` — this creates every protocol global (see below).
5. **Select the backend:** winit if nested (`WAYLAND_DISPLAY`/`DISPLAY` set and
   the `winit` feature is on), otherwise udev/DRM.
6. `init_input()`, announce `WAYLAND_DISPLAY`, register the `rwl.sock` IPC server.
7. Spawn the startup command (`-s`), `bar_cmd`, `startup_cmds`, and/or the
   embedded bar — each wired to the IPC status pipe on stdin as appropriate.
8. Insert a 250 ms timer that polls toplevel titles (catches title/app-id
   changes that arrive without a `wl_surface.commit`, e.g. Firefox tab switches).
9. Run the event loop. Its per-iteration callback drains queued Lua hook
   commands (`hooks` feature) and flushes Wayland clients.
10. On exit: pause DRM devices (so Smithay skips the restore-state atomic commit
    that would fail without DRM master) and remove the IPC socket.

## The `Rwl` state

`Rwl` groups its fields into labelled sections (see the struct in
[`src/state.rs`](../src/state.rs)):

- **Event-loop plumbing** — `loop_handle`, `loop_signal`, monotonic `clock`.
- **Wayland display** — `display`, `display_handle`.
- **Protocol states** — `compositor_state`, `shm_state`, `xdg_shell_state`,
  `layer_shell_state`, `seat_state`, selection/data-device state, dmabuf,
  idle-notify/inhibit, and a `ProtocolGlobals` bag that merely keeps
  write-only globals alive for the process lifetime.
- **Desktop / windows** — Smithay's `Space<Window>`, `PopupManager`, plus rwl's
  own `windows` / `focus_stack` vectors and a `window_map` (`WlSurface → Window`)
  for O(1) lookup.
- **Input** — `seat`, `keyboard`, `pointer`, cursor status/mode, cursor-hide and
  key-repeat timers, and the action being repeated.
- **Monitors** — `monitors`, the selected monitor index `sel_mon`, per-monitor
  tag history, and a cached bounding box of all outputs (so pointer clamping
  never iterates monitors on the hot path).
- **Grab** — move/resize grab bookkeeping (see [`src/grab/`](../src/grab/)).
- **Feature state** — session lock, native lock (`lock`), overview (`overview`),
  mod-tap gesture (`mod-tap`), PiP (`pip`), tag-transition animation state
  (`tag-transition`), warp (`warp`) — each `#[cfg]`-gated.
- **IPC** — `socket_name`, `rwl_sock`, the `ipc_out` status pipe, and persistent
  `ipc_subscribers`.
- **Backend** — `backend` (udev or winit), dmabuf global, gamma control state,
  pending screencopy frames.
- **`arrange()` scratch buffers** — reused `Vec`s so the layout pass allocates
  nothing per call.

### Configuration access

The config lives in a process-global `OnceLock<RwLock<Config>>`
([`src/config/mod.rs`](../src/config/mod.rs)). `config::get()` returns a read
guard; `config::reload()` swaps the whole `Config` under the write lock. Because
reads take the lock, code must avoid holding a read guard while doing anything
that re-enters `config::get()` (nested reads on one thread are not
reentrant-safe) — hence the "snapshot and drop the guard before spawning"
pattern in `main.rs`.

## Windows, monitors, tags

- **Window metadata** ([`src/window.rs`](../src/window.rs)) is stored in
  Smithay's `Window::user_data()`: tag bitmask, floating/fullscreen flags,
  geometry, and feature-specific bits. Helpers like `window_tags`,
  `window_is_floating`, `with_state` / `with_state_mut` read and mutate it.
- **Monitor state** ([`src/monitor.rs`](../src/monitor.rs)) tracks the current
  layout index, `mfact` (master area fraction), `nmaster` (master window count),
  the selected/occupied/urgent tag masks, and per-tag layout overrides.
- **Tags** are a bitmask (default 6 tags → bits `1<<0 .. 1<<5`). Actions like
  `View`, `Tag`, `ToggleView`, `ToggleTag` operate on masks. The all-tags view
  is the full mask (`(1 << tag_count) - 1`).

## Layout / `arrange`

Layout is a pure function of "how many tiled windows are visible on this
monitor." Each layout in [`src/features/layout/`](../src/features/layout/)
exposes an `arrange` function that receives the tiled-window count and the work
area and returns a parallel `Vec` of geometries in tiling order (see
`features/layout/mod.rs`). `Rwl::arrange_all()` classifies windows (hidden /
tiled / floating / fullscreen / scratch) into its reusable scratch buffers,
calls the active layout, and applies the resulting geometries. Per-tag layout
selection (`pertag-layouts`) can override the monitor's layout per tag, either
fixed or chosen by window count.

## Backends

[`src/backend/mod.rs`](../src/backend/mod.rs) defines `BackendData`, an enum over
the two backends. Backend choice is made once, at startup, from the environment.

### udev/DRM ([`src/backend/udev.rs`](../src/backend/udev.rs))

The production backend for bare-metal / TTY use. It:

- opens a session via **libseat**,
- discovers GPUs via **udev**,
- creates DRM outputs and a **GLES renderer** per GPU (via EGL/GBM),
- drives the render loop (frame callbacks, damage tracking),
- reads **libinput** events and forwards them to the compositor.

DRM devices discovered at init are opened lazily once `SessionEvent::
ActivateSession` fires on the first loop tick, which is also what makes VT
switching and suspend/resume work.

### winit ([`src/features/winit.rs`](../src/features/winit.rs), `winit` feature)

Runs rwl nested as an EGL-backed window inside an existing Wayland/X11
compositor — the counterpart used for development. Selected automatically when
`WAYLAND_DISPLAY`/`DISPLAY` is present. Note the `Flipped180` transform applied
in `main.rs`: the GLES renderer's built-in flip would otherwise render the EGL
surface upside-down under the parent compositor.

## Render path

[`src/render.rs`](../src/render.rs) defines the compositor's render element
types and border drawing. Rendering composites, per output: layer-shell
backgrounds, tiled/floating window surfaces (with borders and optional rounded
corners), fullscreen surfaces, overview/PiP thumbnails, animation elements
(fade, tag-transition), the top layer, and the cursor.

Two feature integrations touch the render path directly:

- **Rounded corners** ([`src/features/rounded_corners.rs`](../src/features/rounded_corners.rs))
  contributes shader-based render elements, re-exported through `render.rs`.
- **Overview / PiP** reuse Smithay's `constrain_space_element` to scale a
  window's *live* render elements into a target rectangle, producing thumbnails
  that stay animated.

### Security hardening in the render path

When the session is locked (`ext-session-lock-v1` client *or* the native `lock`
feature), the render path composites **only** cursor + black backdrop
(+ the native locker's password-progress bar), never client or layer content,
and screencopy requests are refused (see
[`src/handlers/screencopy.rs`](../src/handlers/screencopy.rs) and
[`src/features/lock.rs`](../src/features/lock.rs)).

## Protocol handlers

[`src/handlers/`](../src/handlers/) implements the Smithay handler traits on
`Rwl`. Notable ones:

| Handler | Responsibility |
|---------|----------------|
| `compositor.rs` | Surface commits, buffer management, the commit hook that drives re-arrange/render |
| `xdg_shell.rs` | Toplevel/popup lifecycle (map, configure, destroy) |
| `xdg_decoration.rs` | Server-side decoration negotiation |
| `layer_shell.rs` | `wlr-layer-shell` surfaces (bars, backgrounds, notifications) |
| `input.rs` | Keyboard/pointer event routing, keybinding matching, key repeat, lock/passthrough gating |
| `seat.rs` | Seat, focus, cursor-image status |
| `data_device.rs` | Clipboard / primary selection / drag-and-drop |
| `output.rs` | Output add/remove, mode/transform changes |
| `session_lock.rs` | `ext-session-lock-v1` (external lockers) |
| `screencopy.rs` | `wlr-screencopy` (screenshots/recording), refused while locked |
| `gamma_control.rs` | `wlr-gamma-control` (night-light style clients) |
| `idle.rs` | `ext-idle-notify` / idle-inhibit |

## IPC and status

Two related but distinct mechanisms:

- **Status output** ([`src/ipc.rs`](../src/ipc.rs)) formats per-monitor state in
  the dwl `printstatus()` format (seven lines per monitor: title, appid,
  fullscreen, floating, selmon, tags, layout) and writes it to the `ipc_out`
  pipe — consumed by an external bar or the embedded bar thread.
- **Command socket** ([`src/features/ipc/`](../src/features/ipc/), `ipc`
  feature) — `server.rs` binds `rwl.sock` inside the compositor and dispatches
  text commands to `Rwl`; `mod.rs` is the `rwl msg` client. Subscribers get
  status pushed on every state change. See [`IPC.md`](IPC.md).

## Lua hooks (`hooks` feature)

[`src/features/hooks.rs`](../src/features/hooks.rs) keeps the config Lua VM alive
after loading so user callbacks (`on_window_open`, `on_window_close`,
`on_tag_switch`, `on_focus`, `on_title_change`, `on_fullscreen`,
`on_layout_change`, `on_monitor_add`, `on_monitor_remove`, `on_startup`) can fire
on compositor events. Because a callback runs *inside* the `&mut Rwl` borrow that
raised the event, the `rwl.*` / `win:*` action helpers do not mutate state
directly — they enqueue a `HookCmd`, and `drain()` applies the queue at a safe
point (right after firing, and once per loop iteration from `main.rs`). Read-only
helpers (`rwl.count`, `rwl.clients`, `rwl.focused`, `win:tags`, …) instead answer
from a `Snapshot` of the selected monitor that `refresh()` captures just before
each callback fires, so no live `&Rwl` is needed inside the closure. See
[`CONFIGURATION.md`](CONFIGURATION.md#lua-hooks) for the API.

## Wayland protocols

Beyond the core protocols Smithay provides, the [`protocols/`](../protocols/)
directory ships XML the embedded **bar client** binds against:

- `dwl-ipc-unstable-v2.xml` — the dwl status/IPC protocol (tags, layout, title).
- `wlr-layer-shell-unstable-v1.xml` — layer-shell, for the bar surface.
- `wlr-output-power-management-unstable-v1.xml` — output power management.

## The `azoth-render` subcrate

[`src/features/bar/azoth-render/`](../src/features/bar/azoth-render/) is a small
in-tree crate providing a *safe* API over the C libraries **fcft** (font
rendering) and **pixman** (software compositing) for the bar. All `unsafe` code
is contained there; its `build.rs` links `fcft` and `pixman-1` via `pkg-config`.
Note that the top-level crate denies `unsafe_code` (`Cargo.toml` lints), so this
subcrate is where the FFI boundary lives.

## Coding standards

`Cargo.toml` sets a strict lint posture worth knowing when contributing:

- `unsafe_code = "deny"` (top-level crate), `unused_must_use = "forbid"`.
- Clippy `pedantic` + `nursery` at warn; `panic`, `unwrap_used`, `expect_used`,
  `implicit_clone`, `redundant_clone`, `enum_glob_use` are **forbidden**.
- `missing_docs` warns — hence the pervasive `//!` / `///` comments.

Prefer fallible handling over `unwrap`/`expect`, and match the surrounding
module's style and comment density.
