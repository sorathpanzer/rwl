# Contributing to rwl

Thanks for your interest in improving rwl. This is a small, opinionated dwl/dwm-style
Wayland compositor; contributions that keep the core minimal and match the existing
style are the easiest to merge.

Please also read the [Code of Conduct](.github/CODE_OF_CONDUCT.md).

## Getting set up

The supported toolchain and native libraries are pinned by the Nix flake:

```sh
nix develop            # Rust stable + clippy + rustfmt + all native libs
cargo build            # or `cargo build --release`
```

Without Nix you need the native development libraries listed in the
[README](README.md#with-cargo-directly) on your `PKG_CONFIG_PATH`, and the dlopen'd
runtime libraries (EGL / GBM / wayland / …) on your loader path at run time.

To try a change nested inside your current session (no TTY switch needed):

```sh
cargo run                       # winit backend, auto-selected when nested
```

## Before you open a PR

There is no CI yet, so run these locally — they mirror the crate's lint posture:

```sh
cargo fmt --all
cargo clippy --all-targets      # then again with --no-default-features for a minimal build
cargo build
```

The crate sets a strict lint bar in `Cargo.toml` (`[lints]`) that clippy will enforce:

- `unsafe_code = "deny"` — the only FFI lives in the `azoth-render` subcrate
  (`src/features/shell/bar/azoth-render/`), which is where `unsafe` is allowed.
- `panic`, `unwrap_used`, `expect_used`, `implicit_clone`, `redundant_clone`,
  `enum_glob_use`, `mut_mut` are **forbidden** — prefer fallible handling over
  `unwrap`/`expect`.
- `unused_must_use = "forbid"`; clippy `pedantic` + `nursery` at warn.
- `missing_docs` warns — add `//!` / `///` docs to new modules and public items.

Match the surrounding module's style and comment density. See
[`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md#coding-standards) for the full rationale.

## Where things live

- New optional capabilities are **feature-gated** and grouped under
  `src/features/` (`effects/`, `integration/`, `session/`, `shell/`, `windows/`,
  `layout/`). Add a Cargo feature so a minimal build can drop it.
- Prefer growing the Lua hook / IPC surface over adding built-in UI, and keep
  the dwl philosophy: a small keyboard-driven core with minimal chrome.
- [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) explains the `Rwl` state, the
  event loop, backends, and the render path — read it before larger changes.

## Documentation

User-facing behaviour changes should update the relevant doc under `docs/`
(and the [example config](examples/config/) if you add or rename a config key).
The four docs are cross-linked; the `#lua-hooks` section of
[`docs/CONFIGURATION.md`](docs/CONFIGURATION.md) is the single source of truth for
the hook list.
