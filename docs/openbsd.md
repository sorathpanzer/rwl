# Running rwl on OpenBSD

rwl runs natively (DRM/KMS) on OpenBSD -current, the same way the `wayland/niri`
port does: OpenBSD's shim packages present the libudev / libinput / libseat APIs
that Smithay expects on top of the native device model (`wscons(4)`, `kqueue(2)`,
DRM). This guide covers the extra steps beyond a normal `cargo build`.

rwl's own code is already portable (screen lock uses `bsd_auth` instead of PAM,
window-swallowing uses `sysctl(KERN_PROC_PID)` instead of `/proc`, the bar maps
its status "signals" onto `SIGUSR1`/`SIGUSR2` since OpenBSD has no `SIGRTMIN`
range). The remaining work is at the dependency layer, described below.

## 1. Packages

```
pkg_add rust mesa libdrm wayland wayland-protocols libxkbcommon pixman fcft \
        seatd libudev-openbsd libinput-openbsd
```

- `libudev-openbsd` — libudev shim, satisfies Smithay's `backend_udev`.
- `libinput-openbsd` — libinput over `wscons`, satisfies `backend_libinput`.
- `seatd` — provides libseat for `backend_session_libseat`.
- `mesa` provides the GBM/EGL/GL runtime.

## 2. seatd

```
rcctl enable seatd
rcctl start seatd
usermod -G seatd,video,wheel <your-user>   # then re-login
```

## 3. Patch Smithay (required — the one hard blocker)

rwl pins Smithay `5fb12b87…` (v0.7.0). That revision calls `rustix::event::eventfd`
in its DRM-syncobj code, and rustix does not expose `eventfd` on OpenBSD (the OS
has no `eventfd(2)`). The fix is three small, `cfg`-gated changes — they compile
out on Linux, so a single Smithay checkout serves both platforms. The exact patch
is committed here: [`openbsd/smithay-0.7-openbsd.patch`](../openbsd/smithay-0.7-openbsd.patch).

**Do NOT adopt tobhe's `openbsd` Smithay branch** — it is Smithay 0.4.0, far older
than rwl's 0.7.0, and switching to it would remove APIs rwl relies on.

Create a fork of Smithay at rwl's exact revision and apply the patch:

```
git clone https://github.com/Smithay/smithay
cd smithay
git checkout 5fb12b87407b3680135c45d94214c5f1b1d0fbea
git switch -c openbsd-0.7
git apply /path/to/rwl/openbsd/smithay-0.7-openbsd.patch
git commit -am "OpenBSD: gate DRM-syncobj eventfd; OpenBSD libEGL name"
# push to your own GitHub fork, note the commit hash
```

Then point rwl at your patched fork. Because the patch is `cfg`-gated, this line
is safe to keep for Linux/Nix builds too:

```toml
# rwl/Cargo.toml — replace the upstream smithay line's git URL + rev
smithay = { git = "https://github.com/<you>/smithay", rev = "<your patched commit>", default-features = false, features = [ … ] }
```

(Alternatively, for a local-only OpenBSD build, skip the fork and use a path patch:
`[patch."https://github.com/Smithay/smithay"] smithay = { path = "../smithay" }`.)

## 4. Dependency patches (add as the build/run requires them)

These come from Tobias Heider's OpenBSD forks (the same ones the niri port uses).
Add to `rwl/Cargo.toml`. `[patch]` is global but the fork branches are
Linux-compatible; keep them only on your OpenBSD build if you prefer.

```toml
[patch.crates-io]
# DRM node-type detection fix for OpenBSD (matches rwl's drm 0.14 line).
# If keyboard init fails to dlopen libxkbcommon, add tobhe's xkbcommon-dl fork.
# If a libc symbol/struct is missing at build time, add tobhe's libc fork.
```

Verify branch/rev names against https://github.com/tobhe (branches drift). The
`drm-rs` `openbsd-0.14` branch is the one matching rwl's `drm 0.14.1`.

## 5. Build

```
cargo build --release
```

### Building without the status bar (if `fcft` won't install)

The `bar` feature is the only thing that needs `fcft` and `pixman` (via the
`azoth-render` sub-crate). If `fcft` is currently uninstallable on your OpenBSD
snapshot (broken dependency chain), build the full default set **minus `bar`** —
everything else works, and you can add `bar` back once `fcft` installs:

```
cargo build --release --no-default-features --features \
"tile,monocle,scratchpad,col,scroll,dwindle,bstack,centeredmaster,ipc,winit,\
rounded-corners,gaps,warp,fade,tag-transition,startup-zoom,pertag-layouts,\
startup-cmds,auto-back-empty-tag,overview,hooks,pip,mod-tap,lock,wallpaper,\
swallow,xwayland"
```

(This is the `default` feature list from `Cargo.toml` with `bar` removed.)

If you hit an EGL load error at startup, check the runtime lib name
(`ls /usr/X11R6/lib/libEGL.so*` or `/usr/local/lib`) and adjust `LIBEGL` in the
Smithay patch (`src/backend/egl/ffi.rs`) to match your Mesa version.

## 6. Launch

Run from a text VT (stop `xenodm` first), with seatd running and your user in the
`seatd`/`video` groups — mirroring the niri port's `niri-session`:

```
rwl -s ~/.config/rwl/startup.sh
```

## Screen lock (`rwl msg lock`)

The native lock authenticates with `bsd_auth` (`auth_userokay`), which execs
`/usr/libexec/auth/login_passwd` — runnable only by root and members of the
**`auth`** group. Add your user to it (and re-login):

```
doas usermod -G auth,wheel <youruser>   # include every group you already had —
                                        # OpenBSD's usermod -G REPLACES the set
id                                       # confirm: auth + wheel present
```

If your user is *not* in `auth`, rwl refuses to lock (it would trap the session
with no way to unlock) and shows a red 5-second notice on the bar telling you to
add the group, rather than locking.

## Suspend / resume (`zzz`, `ZZZ`)

OpenBSD's `apm` suspend does **not** go through seatd, so the compositor never
receives a libseat session pause/resume event. After waking, libinput's device
fds are stale and pointer/keyboard input is dead (the mouse cursor disappears).

rwl recovers **automatically** — no apmd hook required. Since apm gives the
compositor no session event (and OpenBSD's monotonic clock counts suspended time),
rwl detects the wake from a wall-clock jump between its 250 ms internal ticks (the
timer can't fire while suspended). On detecting it, rwl cycles libinput (re-grabs
`wsmouse`/`wskbd`), resets DRM, releases any stuck keys, and shows the cursor —
within ~250 ms of waking.

If you ever need to trigger the same recovery by hand:

```
rwl msg resume
```

(Bindable from your config, e.g. spawn `rwl msg resume`, if you want a manual
fallback.)

## Status / verify-on-target

- **Verified on Linux:** the Smithay patch compiles and rwl builds against it with
  no change to Linux behavior (all edits are `cfg(target_os = "openbsd")`).
- **To confirm on OpenBSD:** the `drm`/`xkbcommon-dl`/`libc` fork patches (add
  incrementally as each surfaces), the exact `libEGL` version string, DRM-master
  permissions, and `bsd_auth` login for the screen lock.
