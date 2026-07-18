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
mod-tap lock wallpaper swallow
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

With the `hooks` feature, a `lua` layout kind lets you write the tiling algorithm
yourself in Lua via the global `on_arrange` callback — see
[CONFIGURATION.md](CONFIGURATION.md#scriptable-layouts-on_arrange).

Related: **`pertag-layouts`** assigns layouts per tag — fixed, or chosen by the
live tiled-window count on that tag — see
[`src/features/windows/pertag_layouts.rs`](../src/features/windows/pertag_layouts.rs) and
[CONFIGURATION.md](CONFIGURATION.md#pertag_layouts-pertag-layouts-feature).

---

## Window & workspace features

### `scratchpad`
[`src/features/windows/scratchpad.rs`](../src/features/windows/scratchpad.rs) — dropdown-style
windows bound to a key. Spawn/hide/show a floating window on demand; actions
`toggle_scratch`, `focus_or_toggle_scratch`,
`focus_or_toggle_matching_scratch`. All scratchpad state is contained here so
the rest of the compositor is oblivious when the feature is off.

### `overview` (Exposé / mission control)
[`src/features/windows/overview/`](../src/features/windows/overview/) — shrinks every window
into a live, animated grid of thumbnails; jump to one by typing a letter hint or
clicking. Two modes: current-tag (`toggle_overview`) and all-tags
(`toggle_overview_all`, which switches tag on selection). Rendering reuses
Smithay's `constrain_space_element` to scale live render elements into each grid
cell; the zoom animation interpolates between real geometry and grid cell.

### `pip` (picture-in-picture)
[`src/features/windows/pip.rs`](../src/features/windows/pip.rs) — mirrors the focused window into
a small always-on-top thumbnail pinned to a corner, kept live even when the
source window is on a hidden tag (a frame callback is delivered every frame).
Actions `toggle_pip`, `move_pip`.

### `auto-back-empty-tag`
[`src/features/windows/auto_back_empty_tag.rs`](../src/features/windows/auto_back_empty_tag.rs) —
when the last window on a tag closes, automatically navigate back to the
previously viewed tag.

### `startup-cmds`
[`src/features/session/startup_cmds.rs`](../src/features/session/startup_cmds.rs) — commands in
`startup_cmds` are spawned once, unconditionally, after the compositor is ready.

### `swallow` (terminal window swallowing)
[`src/features/windows/swallow.rs`](../src/features/windows/swallow.rs) — dwm's swallow patch:
when a terminal spawns a graphical child (`mpv video.mp4`, `zathura doc.pdf`, …
run from a shell), the terminal is hidden and the child takes its slot in the
layout; closing the child restores the terminal. Matching is by **process
ancestry** — the child's PID must descend from the terminal's PID (walked via
`/proc/<pid>/stat`) and the terminal's `app_id` must be in the configured
`swallow.terminals` list. A terminal never swallows another terminal. Because
rwl inserts new windows at the front of the stack, the just-spawned child already
sits above its terminal in tiling order, so no reordering is needed. When the
child closes, the restored terminal takes focus. A per-rule `no_swallow` flag
exempts specific apps (see [CONFIGURATION.md](CONFIGURATION.md#rules)). A swallowed
window inherits the terminal's tag and monitor, so a `switch_to_tag` rule on the
child is suppressed — the view stays on the terminal's tag rather than jumping
away. Linux-only (reads `/proc`), matching rwl's udev/DRM target.

---

## Input & focus features

### `warp`
[`src/features/integration/warp.rs`](../src/features/integration/warp.rs) — warp the pointer to the newly
focused window after keyboard focus changes (and once at startup).

### `mod-tap`
Tapping the mod key alone (press + release with no other key) fires a bound
action — by default toggling the overview. Configured via an empty `key = ""`
binding; the gesture state lives in `Rwl::mod_tap`.

---

## Visual features

### `rounded-corners`
[`src/features/effects/rounded_corners.rs`](../src/features/effects/rounded_corners.rs) —
shader-based rounded-corner render elements, re-exported through
[`src/render.rs`](../src/render.rs). Radius set by `windows.corner_radius`.

### `gaps`
[`src/features/effects/gaps.rs`](../src/features/effects/gaps.rs) — configurable gaps between
tiled windows (`windows.gaps_px`), togglable at runtime (`toggle_gaps`). With
`windows.smart_gaps` (default on, dwm's smartgaps) a lone tiled window fills the
work area with no gaps; set it `false` to gap a single window too.

### `fade`
[`src/features/effects/fade.rs`](../src/features/effects/fade.rs) — per-window fade-in/fade-out
animations (`effects.fade_in_ms` / `fade_out_ms`; `0` disables).

### `tag-transition`
[`src/features/effects/tag_transition.rs`](../src/features/effects/tag_transition.rs) — smooth
horizontal slide between tags: old-tag windows slide out in the direction of
travel while new-tag windows slide in from the opposite side. Duration
`effects.tag_transition_ms`. The compositor keeps windows mapped through the
animation and re-arranges afterwards so late-committing clients (e.g. video
players) don't show a blank first frame.

### `wallpaper`
[`src/features/shell/wallpaper.rs`](../src/features/shell/wallpaper.rs) — decodes PNG/JPEG
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
[`src/features/shell/bar/`](../src/features/shell/bar/) — a status bar that runs as a
background thread inside the compositor and connects back to the compositor's own
Wayland socket as a *client*, using the `wlr-layer-shell` and `zdwl-ipc`
protocols. It therefore receives tag/layout/title state without an external
pipe. Text is rendered by the in-tree
[`azoth-render`](../src/features/shell/bar/azoth-render/) crate — a safe wrapper over
the C libraries **fcft** (fonts) and **pixman** (software compositing); it is the
one place FFI (`unsafe`) lives, since the top-level crate denies `unsafe_code`.
Configured via the `bar` table; controlled at runtime with the `-`-prefixed
`rwl msg` commands (see [IPC.md](IPC.md#bar-commands-bar-feature)).

### `ipc` (command socket)
[`src/features/integration/ipc/`](../src/features/integration/ipc/) — binds `$XDG_RUNTIME_DIR/rwl.sock`,
dispatches text commands to the compositor, and supports status subscribers. The
`rwl msg` client lives in the same module. It also exposes a **structured JSON
surface** ([`event.rs`](../src/features/integration/ipc/event.rs)): `rwl msg clients` dumps
the window tree and `rwl msg watch` streams `window`/`focus`/`tag`/`title` events
— enough to build external window switchers or activity loggers without any
in-process UI. Full reference in [IPC.md](IPC.md).

### `hooks` (Lua event bus)
[`src/features/integration/hooks.rs`](../src/features/integration/hooks.rs) — keeps the config Lua VM
alive so user callbacks fire on compositor events, turning the static config into
a programmable one. Events range from window lifecycle (`on_window_open`,
`on_window_close`, `on_window_rule`) through focus/tag/layout changes
(`on_focus`, `on_tag_switch`, `on_layout_change`, …) to monitor hotplug, urgency,
session lock/unlock, and config errors — see
[CONFIGURATION.md](CONFIGURATION.md#lua-hooks) for the complete list. Callbacks
read live state
through snapshot helpers (`rwl.clients()`, `rwl.focused()`, `rwl.count()`, …) and
mutate it through a deferred command queue (`win:set_tags`, `rwl.zoom`, …) to
avoid re-entering the compositor borrow. A `lua` layout kind additionally routes
tiling to an `on_arrange` callback. Full API in
[CONFIGURATION.md](CONFIGURATION.md#lua-hooks).

### `lock` (native screen locker)
[`src/features/session/lock.rs`](../src/features/session/lock.rs) — a built-in PAM locker (as
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
[`src/features/integration/winit.rs`](../src/features/integration/winit.rs) — runs rwl as an EGL-backed
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
