# Features

Every optional capability in rwl is gated behind a Cargo feature flag (see
`[features]` in [`Cargo.toml`](../Cargo.toml)). This keeps the compositor core
small and lets you compile only what you use. This document describes each
feature and points at its implementation.

## Feature matrix

The `default` feature set enables all of the following:

```
tile monocle scratchpad col scroll dwindle bstack centeredmaster
bar ipc winit rounded-corners gaps warp fade tag-transition
pertag-layouts startup-cmds auto-back-empty-tag overview hooks pip
mod-tap lock wallpaper
```

Build a subset with, e.g.:

```sh
cargo build --release --no-default-features --features "tile,monocle,ipc,bar"
```

### Dependency-carrying features

Most features are pure code toggles (`feature = []`). A few pull in crates or
Smithay features:

| Feature | Extra dependencies |
|---------|--------------------|
| `bar` | `azoth-render` (in-tree), `wayland-client`/`-backend`/`-protocols`/`-protocols-wlr`/`-scanner`, `bytemuck`, `signal-hook` |
| `lock` | `pam`, `zeroize` |
| `wallpaper` | `image` (PNG/JPEG decoding) |
| `winit` | `smithay/backend_winit` |

---

## Tiling layouts

Each layout is a feature contributing a `LayoutKind` variant and an `arrange`
function ([`src/features/layout/`](../src/features/layout/)). A layout receives
the count of tiled (non-floating, non-fullscreen) windows on the monitor and
returns a parallel `Vec` of geometries in tiling order.

| Feature | Symbol | Layout |
|---------|--------|--------|
| `tile` | `[]=` | Master/stack: `nmaster` windows in a master column, the rest stacked beside them. The classic dwm tile. |
| `monocle` | `[M]` | All windows fullscreen-stacked; only the focused one is visible. |
| `col` | `\|\|` | Windows arranged in resizable columns. |
| `scroll` | `>>>` | Horizontally scrollable strip of windows. |
| `dwindle` | `[@]` | Fibonacci/spiral split. |
| `bstack` | `(B)` | Bottom-stack: master row on top, stack row below. |
| `centeredmaster` | `\|-\|` | Master centred with stacks on both sides. |

When *no* layout feature is enabled, a built-in `Fallback` gives every window
the full work area so the compositor still tiles. `mfact` (master fraction) and
`nmaster` (master count) are per-monitor and adjustable at runtime
(`set_mfact`, `inc_nmaster`).

Related: **`pertag-layouts`** assigns layouts per tag — fixed, or chosen by the
live tiled-window count on that tag — see
[`src/features/pertag_layouts.rs`](../src/features/pertag_layouts.rs) and
[CONFIGURATION.md](CONFIGURATION.md#pertag_layouts-pertag-layouts-feature).

---

## Window & workspace features

### `scratchpad`
[`src/features/scratchpad.rs`](../src/features/scratchpad.rs) — dropdown-style
windows bound to a key. Spawn/hide/show a floating window on demand; actions
`toggle_scratch`, `focus_or_toggle_scratch`,
`focus_or_toggle_matching_scratch`. All scratchpad state is contained here so
the rest of the compositor is oblivious when the feature is off.

### `overview` (Exposé / mission control)
[`src/features/overview.rs`](../src/features/overview.rs) — shrinks every window
into a live, animated grid of thumbnails; jump to one by typing a letter hint or
clicking. Two modes: current-tag (`toggle_overview`) and all-tags
(`toggle_overview_all`, which switches tag on selection). Rendering reuses
Smithay's `constrain_space_element` to scale live render elements into each grid
cell; the zoom animation interpolates between real geometry and grid cell.

### `pip` (picture-in-picture)
[`src/features/pip.rs`](../src/features/pip.rs) — mirrors the focused window into
a small always-on-top thumbnail pinned to a corner, kept live even when the
source window is on a hidden tag (a frame callback is delivered every frame).
Actions `toggle_pip`, `move_pip`.

### `auto-back-empty-tag`
[`src/features/auto_back_empty_tag.rs`](../src/features/auto_back_empty_tag.rs) —
when the last window on a tag closes, automatically navigate back to the
previously viewed tag.

### `startup-cmds`
[`src/features/startup_cmds.rs`](../src/features/startup_cmds.rs) — commands in
`startup_cmds` are spawned once, unconditionally, after the compositor is ready.

---

## Input & focus features

### `warp`
[`src/features/warp.rs`](../src/features/warp.rs) — warp the pointer to the newly
focused window after keyboard focus changes (and once at startup).

### `mod-tap`
Tapping the mod key alone (press + release with no other key) fires a bound
action — by default toggling the overview. Configured via an empty `key = ""`
binding; the gesture state lives in `Rwl::mod_tap`.

---

## Visual features

### `rounded-corners`
[`src/features/rounded_corners.rs`](../src/features/rounded_corners.rs) —
shader-based rounded-corner render elements, re-exported through
[`src/render.rs`](../src/render.rs). Radius set by `windows.corner_radius`.

### `gaps`
[`src/features/gaps.rs`](../src/features/gaps.rs) — configurable gaps between
tiled windows (`windows.gaps_px`), togglable at runtime (`toggle_gaps`).

### `fade`
[`src/features/fade.rs`](../src/features/fade.rs) — per-window fade-in/fade-out
animations (`effects.fade_in_ms` / `fade_out_ms`; `0` disables).

### `tag-transition`
[`src/features/tag_transition.rs`](../src/features/tag_transition.rs) — smooth
horizontal slide between tags: old-tag windows slide out in the direction of
travel while new-tag windows slide in from the opposite side. Duration
`effects.tag_transition_ms`. The compositor keeps windows mapped through the
animation and re-arranges afterwards so late-committing clients (e.g. video
players) don't show a blank first frame.

### `wallpaper`
[`src/features/wallpaper.rs`](../src/features/wallpaper.rs) — decodes PNG/JPEG
images (via the `image` crate) and composites them behind windows. Supports a
`default` image and per-tag wallpapers (`wallpaper.tags`), with fit modes
`fill` / `fit` / `stretch` / `center`. Decoding runs on a background thread and
results are cached, so a tag revisit never re-decodes; `preload_configured()`
warms every configured image at startup. Set at runtime with `rwl msg wallpaper`
(see [IPC.md](IPC.md#wm-commands)) or from a hook via `rwl.set_wallpaper` /
`rwl.preload` (see [CONFIGURATION.md](CONFIGURATION.md#lua-hooks)).

---

## System features

### `bar` (embedded status bar)
[`src/features/bar/`](../src/features/bar/) — a status bar that runs as a
background thread inside the compositor and connects back to the compositor's own
Wayland socket as a *client*, using the `wlr-layer-shell` and `zdwl-ipc`
protocols. It therefore receives tag/layout/title state without an external
pipe. Text is rendered by the in-tree
[`azoth-render`](../src/features/bar/azoth-render/) crate — a safe wrapper over
the C libraries **fcft** (fonts) and **pixman** (software compositing); it is the
one place FFI (`unsafe`) lives, since the top-level crate denies `unsafe_code`.
Configured via the `bar` table; controlled at runtime with the `-`-prefixed
`rwl msg` commands (see [IPC.md](IPC.md#bar-commands-bar-feature)).

### `ipc` (command socket)
[`src/features/ipc/`](../src/features/ipc/) — binds `$XDG_RUNTIME_DIR/rwl.sock`,
dispatches text commands to the compositor, and supports status subscribers. The
`rwl msg` client lives in the same module. Full reference in [IPC.md](IPC.md).

### `hooks` (Lua event bus)
[`src/features/hooks.rs`](../src/features/hooks.rs) — keeps the config Lua VM
alive so user callbacks fire on compositor events, turning the static config into
a programmable one. Events: `on_window_open`, `on_window_close`, `on_tag_switch`,
`on_focus`, `on_title_change`, `on_fullscreen`, `on_layout_change`,
`on_monitor_add`, `on_monitor_remove`, `on_startup`. Callbacks read live state
through snapshot helpers (`rwl.clients()`, `rwl.focused()`, `rwl.count()`, …) and
mutate it through a deferred command queue (`win:set_tags`, `rwl.zoom`, …) to
avoid re-entering the compositor borrow. Full API in
[CONFIGURATION.md](CONFIGURATION.md#lua-hooks).

### `lock` (native screen locker)
[`src/features/lock.rs`](../src/features/lock.rs) — a built-in PAM locker (as
opposed to the `ext-session-lock-v1` path that lets an external client like
swaylock draw the lock screen). Security model:

- While locked, the render path composites only cursor + black backdrop + a
  minimal password-progress bar — never client/layer content — and screencopy
  is refused.
- Keyboard focus is cleared and every key is intercepted before reaching a
  client; there is no unlock keybind — the only exit is successful PAM auth.
- The typed password lives in a pre-allocated `Zeroizing` buffer (no
  reallocation → no stale copies in freed pages) and is zeroed on drop; the bar
  renders only its length.
- PAM runs on a worker thread (it blocks and deliberately delays on failure);
  the result returns via a calloop channel so the event loop never stalls.

### `winit` (nested backend)
[`src/features/winit.rs`](../src/features/winit.rs) — runs rwl as an EGL-backed
window inside an existing Wayland/X11 compositor, selected automatically when
`WAYLAND_DISPLAY`/`DISPLAY` is set. The development counterpart to the udev/DRM
backend. Without this feature, rwl refuses to start in a nested environment.

---

## Choosing features

- **Minimal tiling WM:** `tile ipc` (add `monocle`, `col`, … for more layouts).
- **Development / nested:** add `winit`.
- **Daily driver:** the full `default` set.
- Drop `bar` to avoid the fcft/pixman C dependencies; drop `lock` to avoid the
  PAM dependency; drop `winit` if you never run nested.

See [ARCHITECTURE.md](ARCHITECTURE.md) for how these features integrate with the
core compositor.
