# Running rwl on NetBSD

NetBSD (tested against 11.0, `x86_64-unknown-netbsd`) is closer to a normal Linux
build than the OpenBSD port is: NetBSD has PAM, and pkgsrc ships native
`libudev`, `libinput`, and `libseat` — so Smithay's `backend_udev`,
`backend_libinput`, and `backend_session_libseat` link against real libraries
rather than shims. In practice the build reaches the final link with only one
class of problem: pkgsrc puts libraries in `/usr/pkg/lib`, which is not on the
linker's default search path.

> **Status: partially verified.** The build compiles all crates (including the
> local patched Smithay) and reaches the final link. The steps below are derived
> from the observed link failure and the repo layout; items marked *(verify on
> target)* still need confirmation on real hardware — DRM-master permissions,
> seat setup, and runtime startup in particular.

## 1. Packages

```
pkgin install rust MesaLib libdrm wayland wayland-protocols libxkbcommon \
              pixman fcft seatd libudev libinput
```

- `MesaLib` provides the GBM/EGL/GL runtime (the `libgbm` that `-lgbm` needs).
- `pixman` also ships in the base X11R7 set (`/usr/X11R7/lib`), which is why
  `-lpixman-1` links even before you fix the search path below.
- Confirm the gbm library exists after install: `ls /usr/pkg/lib/libgbm.*`
  (or wherever your Mesa package lands it — `pkg_info -L MesaLib | grep gbm`).

The link error you'll see if any of these are **missing** is:

```
ld: cannot find -lfcft:      No such file or directory
ld: cannot find -lxkbcommon: No such file or directory
ld: cannot find -lgbm:       No such file or directory
```

That is almost always "package not installed" — note that pkgsrc libs which
*are* installed (`libseat`, `libudev`, `libinput`, `libpam`) link fine, because
their `*-sys` crates already add `/usr/pkg/lib` to the search path via
`pkg-config`. The three above fail only when the library file itself is absent.
(The `warning: reference to compatibility __kevent50()` line in the same output
is a harmless NetBSD compat-symbol note, not the error.)

## 2. Library search path

pkgsrc's `/usr/pkg/lib` is not searched by the linker by default. The repo now
carries a NetBSD-scoped [`.cargo/config.toml`](../.cargo/config.toml) that adds
it (and sets the runtime rpath) for the `x86_64-unknown-netbsd` target only —
Linux/Nix and OpenBSD builds are untouched:

```toml
[target.x86_64-unknown-netbsd]
rustflags = ["-L", "/usr/pkg/lib", "-C", "link-args=-Wl,-R/usr/pkg/lib"]
```

The `-Wl,-R/usr/pkg/lib` rpath means the resulting binary also *runs* without
setting `LD_LIBRARY_PATH`.

Also make sure `pkg-config` can find the `.pc` files (this is what lets
`azoth-render/build.rs` resolve `fcft` and `pixman`):

```
export PKG_CONFIG_PATH=/usr/pkg/lib/pkgconfig:/usr/X11R7/lib/pkgconfig
pkg-config --exists fcft && echo fcft-ok
```

## 3. Smithay

rwl pins a patched local Smithay (`smithay/` in-tree; the OpenBSD `eventfd`
patch is `cfg(target_os = "openbsd")`-gated). At **build** time NetBSD needs no
patch — the OpenBSD gates don't apply, and `rustix`'s `eventfd` resolved for the
NetBSD target.

At **runtime** one Smithay fix was required and is committed in-tree:

- **Sealed keymap file** (`smithay/src/utils/sealed_file.rs`). On non-Linux
  platforms Smithay creates the keymap via `shm_open`. The randomized name was
  built without a leading `/`; NetBSD's `shm_open(2)` requires the name to begin
  with a single slash and contain no others, so it failed with
  `EINVAL (os error 22)` — logged as `Error when creating sealed keymap file`,
  which left the compositor with no keyboard keymap. The fix prepends `/` to the
  shm name; it's POSIX-correct and harmless on every platform that reaches that
  branch (Linux uses a sealed memfd instead and is unaffected).

If a future NetBSD build trips on `eventfd` or another Linux-ism in the
DRM-syncobj path, mirror the OpenBSD approach: add a `cfg(target_os = "netbsd")`
gate alongside the existing OpenBSD one in `smithay/src/wayland/drm_syncobj/`.

## 4. seatd and session activation

`backend_session_libseat` needs the seat daemon running and your user in the
seat group:

```
# enable seatd at boot (rc.conf) and start it now
echo 'seatd=YES' >> /etc/rc.conf
/etc/rc.d/seatd start
usermod -G wheel <your-user>   # plus whatever group seatd/DRM access needs
```

**The session must become *active* or nothing displays.** rwl opens the libseat
session, finds the DRM devices, and defers opening them until the session is
active (logging `Found N DRM device(s), deferring until session active`). It
then opens them when it sees the session is active — either synchronously at
init (`Session was already active at init`) or on a `SessionEvent::ActivateSession`
(`Session activated — resuming DRM devices`). **If neither of those two log
lines appears, the session never activated and the screen stays black** even
though the build and seat setup are fine.

On NetBSD the usual cause is VT activation: seatd's VT-tracking is limited there,
so it never tells libseat the seat is active. Fixes, in order of preference:

1. **Launch from the active wscons console (`ttyE0`)** — not over SSH and not
   from inside X. Over SSH `seat0` is never active, which reproduces this exactly.
2. **Run seatd with `SEATD_VTBOUND=0`** so it marks the session active
   immediately instead of waiting on a VT-active signal it can't obtain on
   NetBSD:
   ```
   SEATD_VTBOUND=0 seatd -g seatd      # or set it in seatd's rc environment
   ```
3. **Bypass seatd to confirm the diagnosis** — run rwl as root so libseat uses
   the builtin backend, which marks the session active directly:
   ```
   LIBSEAT_BACKEND=builtin doas rwl -s ~/.config/rwl/startup.sh
   ```
   If rwl displays this way, the problem is seatd VT-activation (use fix 1/2 for
   the non-root path), not DRM/KMS.

Exact group/permission model for DRM master on NetBSD is still unconfirmed.

## 4a. XWayland socket directory

XWayland binds `/tmp/.X11-unix/X<n>` but Smithay does not create that directory.
NetBSD has no tmpfiles mechanism to pre-create it, so every display from 0..32
fails with `Failed to create sockets: No such file or directory` and XWayland
reports `Could not find a free socket for the XServer`. Create it once (survives
until reboot; add to a startup script to make it persistent):

```
mkdir -p /tmp/.X11-unix && chmod 1777 /tmp/.X11-unix
```

This is non-fatal — the Wayland compositor runs without it; only X11 apps need
it. (Trailing `spawn failed: No such file or directory` lines are autostart
commands from your `startup.sh` not found on `PATH` — unrelated.)

## 5. Build

```
cargo build --release
```

If `fcft` is uninstallable on your pkgsrc branch, build everything **minus the
`bar`** feature (only `bar` → `azoth-render` needs `fcft`/`pixman`):

```
cargo build --release --no-default-features --features \
"tile,monocle,scratchpad,col,scroll,dwindle,bstack,centeredmaster,ipc,winit,\
rounded-corners,gaps,warp,fade,tag-transition,startup-zoom,pertag-layouts,\
startup-cmds,auto-back-empty-tag,overview,hooks,pip,mod-tap,lock,wallpaper,\
swallow,xwayland"
```

(This is the `default` feature list from `Cargo.toml` with `bar` removed.)

## 6. Screen lock

Unlike OpenBSD (which uses `bsd_auth`), NetBSD has PAM — the build links
`-lpam` and the `pam` crate compiles, so the standard PAM lock path is used, as
on Linux. No `auth`-group dance is required. *(Verify the PAM service
configuration resolves at runtime on target.)*

## Open items to confirm on hardware

- Whether `SEATD_VTBOUND=0` (or a VT-aware seatd build) gives a working non-root
  session on the active console, vs. the builtin-backend-as-root fallback (§4).
- DRM-master / seatd permissions and the correct user groups (§4).
- `libgbm` runtime name and location from your Mesa package.
- Window swallowing: OpenBSD uses `sysctl(KERN_PROC_PID)` instead of `/proc`.
  NetBSD also lacks a Linux `/proc` by default and has `sysctl KERN_PROC2` — if
  swallowing misbehaves, that code path likely needs a `netbsd` cfg branch.
- Suspend/resume behavior (no OpenBSD-style apm wall-clock recovery is wired for
  NetBSD).
