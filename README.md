# rwl

**rwl** is a dynamic Wayland window manager written in Rust on top of
[Smithay](https://github.com/Smithay/smithay). It follows the design of the
suckless [dwl](https://codeberg.org/dwl/dwl) / dwm family — tag-based dynamic
tiling, a small keyboard-driven core, minimal chrome — and adds a Lua
configuration layer, an embedded status bar, a command IPC socket, and a set of
optional, feature-gated extras (overview, picture-in-picture, native screen
locker, animations, and more).

- **Language / edition:** Rust 2024 (MSRV 1.95)
- **Compositor toolkit:** Smithay (`desktop`, `wayland_frontend`, libinput,
  udev, DRM, EGL, GBM, libseat, GLES renderer)
- **Config language:** Lua 5.4 (via `mlua`, vendored)
- **License:** GPL-3.0-or-later

---

## Table of contents

- [Feature overview](#feature-overview)
- [Building & installing](#building--installing)
- [Running](#running)
- [Configuration](#configuration)
- [Command IPC (`rwl msg`)](#command-ipc-rwl-msg)
- [Documentation map](#documentation-map)
- [Repository layout](#repository-layout)

---

## Feature overview

rwl compiles a large default feature set (see `[features]` in
[`Cargo.toml`](Cargo.toml)). Every optional capability is gated behind a Cargo
feature so a minimal build is possible.

**Tiling layouts** (each a feature flag): `tile`, `monocle`, `col`, `scroll`,
`dwindle`, `bstack`, `centeredmaster`.

**Window management:** tags (default 6), per-monitor master/stack with
adjustable `nmaster` and `mfact`, floating windows, fullscreen, multi-monitor
focus/move, per-tag layouts (`pertag-layouts`), scratchpads.

**Extras:** embedded status bar (`bar`), command socket (`ipc`), workspace
overview / Exposé (`overview`), picture-in-picture (`pip`), native PAM screen
locker (`lock`), Lua event hooks (`hooks`), fade animations (`fade`), tag-switch
slide animations (`tag-transition`), rounded corners (`rounded-corners`), gaps
(`gaps`), cursor warp (`warp`), mod-key tap gesture (`mod-tap`), auto-return
from empty tags (`auto-back-empty-tag`), startup commands (`startup-cmds`),
per-tag wallpapers (`wallpaper`), nested winit backend (`winit`).

See [`docs/FEATURES.md`](docs/FEATURES.md) for a per-feature description.

## Building & installing

### With Nix (recommended)

A [`flake.nix`](flake.nix) provides both a package and a dev shell with the full
toolchain and native libraries pinned.

```sh
# Build the package
nix build

# Run it
./result/bin/rwl

# Or drop into a dev shell (Rust stable + clippy + rustfmt + native libs)
nix develop
cargo build --release
```

The flake wraps the installed binary with `LD_LIBRARY_PATH` so the dlopen'd
runtime libraries (EGL / GBM / wayland / …) resolve outside the dev shell.

### With Cargo directly

You need the native development libraries on your `PKG_CONFIG_PATH`:
`wayland`, `libseat`, `libudev`, `libdrm`, `libgbm`, `libglvnd` (EGL/GL),
`libinput`, `libxkbcommon`, `libxcb`, `pam` (for the `lock` feature), plus
`fcft` and `pixman` (for the `bar` feature).

```sh
cargo build --release
# Minimal build (disable default features, pick your own):
cargo build --release --no-default-features --features "tile,monocle,ipc"
```

The release profile uses fat LTO, a single codegen unit, `panic = "abort"`, and
strips symbols.

## Running

rwl selects its backend automatically at startup:

- **udev/DRM** on a bare TTY (opens a libseat session, drives the GPU directly).
- **winit** when `WAYLAND_DISPLAY` or `DISPLAY` is set — runs nested inside an
  existing compositor, useful for development. Requires the `winit` feature.

```sh
rwl                     # start on the current TTY
rwl -s ~/.config/rwl/startup.sh   # run a startup command (its stdin receives
                                  # the IPC status stream)
rwl -v                  # verbose (info) logging
rwl -d                  # debug logging
```

Logging honours `RUST_LOG` (via `tracing`'s `EnvFilter`); `-v` / `-d` set the
default level when `RUST_LOG` is unset.

`XDG_RUNTIME_DIR` must be set — it is where the Wayland socket and the `rwl.sock`
command socket are created.

## Configuration

rwl reads `~/.config/rwl/config.lua` at startup (and on `reload_config`). Every
setting falls back to a compiled-in default when the file, table, or key is
missing, so an empty or absent config yields a fully working compositor. Unknown
keys are reported (logged and surfaced through the config-error channel) but do
not stop loading.

Minimal example:

```lua
tag_count = 6

windows = {
  border_px    = 2,
  border_color = "#222222",
  focus_color  = "#ffffff",
  gaps_px      = 8,
}

keyboard = { xkb_layout = "us", repeat_rate = 25, repeat_delay = 600 }

keys = {
  { mods = "M",  key = "Return", action = "spawn", cmd = { "foot" } },
  { mods = "MS", key = "q",      action = "quit" },
}
```

The default modifier (`M`) is the **Super / logo** key. See
[`docs/CONFIGURATION.md`](docs/CONFIGURATION.md) for the full schema, the
default keybindings, and the Lua hooks API.

## Command IPC (`rwl msg`)

When the `ipc` feature is enabled, the compositor binds a Unix socket at
`$XDG_RUNTIME_DIR/rwl.sock` and exports its path to children as `RWL_SOCK`. The
same binary doubles as the client:

```sh
rwl msg view 3            # view tag 3 on the selected output
rwl msg tag 1             # move focused window to tag 1
rwl msg togglefloating
rwl msg status            # print current per-monitor status once
rwl msg subscribe         # stream status on every state change
rwl msg spawn foot        # spawn a client
```

With the `bar` feature, dash-prefixed commands drive the embedded status bar
(`rwl msg -status`, `-title`, `-toggle-visibility`, …). The full command
reference is in [`docs/IPC.md`](docs/IPC.md).

## Documentation map

| Document | Contents |
|----------|----------|
| [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) | Runtime model, `Rwl` state, event loop, backends, render path, protocols |
| [`docs/CONFIGURATION.md`](docs/CONFIGURATION.md) | Full `config.lua` schema, default keybindings, Lua hooks API |
| [`docs/IPC.md`](docs/IPC.md) | `rwl msg` WM and bar command reference, status format |
| [`docs/FEATURES.md`](docs/FEATURES.md) | Per-feature reference and Cargo feature matrix |

Every module also carries `//!` doc comments; run `cargo doc --open` for the
API-level view.

## Repository layout

```
Cargo.toml            Crate manifest, feature flags, lints, build profiles
flake.nix / flake.lock  Nix package + dev shell
clippy.toml           Clippy doc-ident allowances
protocols/            Wayland protocol XML (dwl-ipc, wlr-layer-shell,
                      wlr-output-power-management) used by the bar client
src/
  main.rs             Entry point, CLI parsing, event-loop setup
  state.rs            `Rwl` — central compositor state + Smithay handler glue
  actions.rs          Keybinding/IPC action dispatch (`Rwl::dispatch`)
  render.rs           Render element types, border drawing
  window.rs           Per-window metadata (tags, floating, fullscreen, …)
  monitor.rs          Per-output state (layout, mfact, nmaster, tags)
  ipc.rs              Status output formatting (dwl-compatible)
  error.rs            `RwlError` / `Result`
  config/             Lua config loading (types, keybinds, settings, lua)
  backend/            udev/DRM backend (+ winit lives under features/)
  handlers/           Smithay protocol handler impls
  grab/               Interactive move/resize grabs
  features/           Optional, feature-gated capabilities
    layout/           Tiling algorithms
    bar/              Embedded status bar (+ azoth-render subcrate)
    ipc/              Command socket server + `rwl msg` client
    ...               overview, pip, lock, hooks, fade, gaps, warp, ...
```

See [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) for how these fit together.
