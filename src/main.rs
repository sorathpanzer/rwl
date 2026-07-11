//! rwl — a dynamic Wayland window manager, rewritten in Rust using Smithay.
//!
//! Entry point: parses CLI arguments, sets up tracing, creates the compositor
//! state and the udev/DRM backend, then runs the calloop event loop.

mod actions;
mod backend;
mod config;
mod error;
mod grab;
mod handlers;
mod ipc;
mod monitor;
mod render;
mod features;
mod state;
mod window;

#[cfg(feature = "bar")]
use std::sync::Arc;

use smithay::reexports::calloop::EventLoop;
use smithay::reexports::wayland_server::Display;

use error::Result;
use state::Rwl;

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

fn main() {
    // Handle `rwl msg …` before any compositor initialisation.
    #[cfg(feature = "ipc")]
    {
        let argv: Vec<String> = std::env::args().collect();
        if argv.get(1).map(String::as_str) == Some("msg") {
            features::ipc::run(&argv[2..]);
            return;
        }
    }

    let args = parse_args();
    init_tracing(args.log_level);
    config::init();

    if let Err(e) = run(args.startup_cmd) {
        tracing::error!("Fatal error: {}", e);
        std::process::exit(1);
    }
}

// ---------------------------------------------------------------------------
// Compositor main loop
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_lines)]
fn run(startup_cmd: Option<String>) -> Result<()> {
    // XDG_RUNTIME_DIR is required to create the Wayland socket.
    if std::env::var_os("XDG_RUNTIME_DIR").is_none() {
        return Err(error::RwlError::Backend(
            "XDG_RUNTIME_DIR is not set — cannot create Wayland socket".into(),
        ));
    }

    let mut event_loop: EventLoop<'static, Rwl> =
        EventLoop::try_new().map_err(|e| error::RwlError::EventLoop(e.to_string()))?;
    let loop_handle = event_loop.handle();
    let loop_signal = event_loop.get_signal();

    let display: Display<Rwl> =
        Display::new().map_err(|e| error::RwlError::Display(e.to_string()))?;

    let mut state = Rwl::new(display, loop_handle.clone(), loop_signal)?;

    // Select backend: when the `winit` feature is enabled and we are running
    // nested inside an existing compositor, use winit; otherwise use udev/DRM.
    #[cfg(feature = "winit")]
    {
        let is_nested = std::env::var_os("WAYLAND_DISPLAY").is_some()
            || std::env::var_os("DISPLAY").is_some();
        if is_nested {
            tracing::info!("Nested environment detected — using winit backend");
            let winit_data = features::winit::init(&loop_handle, &state.display_handle)?;
            let output = winit_data.output.clone();
            state.backend = Some(backend::BackendData::Winit(Box::new(winit_data)));
            state.output_added(&output);
            // output_added() applies the MonitorRule transform (defaults to Normal).
            // For the EGL/winit backend, Flipped180 is required: the GlesRenderer's
            // built-in flip180 would otherwise produce an upside-down image when the
            // parent compositor composites the EGL surface without any additional flip.
            output.change_current_state(
                None,
                Some(smithay::utils::Transform::Flipped180),
                None,
                None,
            );
            // Kick off the first frame so WinitEvent::Redraw starts firing.
            if let Some(backend::BackendData::Winit(ref data)) = state.backend {
                data.gfx.window().request_redraw();
            }
        } else {
            tracing::info!("No nested compositor detected — using udev/DRM backend");
            let udev_data = backend::udev::init(&loop_handle, &state.display_handle)?;
            state.backend = Some(backend::BackendData::Udev(Box::new(udev_data)));
        }
    }
    #[cfg(not(feature = "winit"))]
    {
        let is_nested = std::env::var_os("WAYLAND_DISPLAY").is_some()
            || std::env::var_os("DISPLAY").is_some();
        if is_nested {
            eprintln!(
                "rwl: a Wayland compositor is already running, \
                 support for nested (winit) mode is disabled in this build."
            );
            std::process::exit(1);
        }
        tracing::info!("Using udev/DRM backend");
        // init() stores existing DRM devices in pending_devices; they will be
        // opened once SessionEvent::ActivateSession fires (first event loop tick).
        let udev_data = backend::udev::init(&loop_handle, &state.display_handle)?;
        state.backend = Some(backend::BackendData::Udev(Box::new(udev_data)));
    }

    state.init_input();

    // The child-process environment — Wayland socket, desktop identity, cursor
    // theme/size, RWL_SOCK, and DISPLAY stripping — is applied per-spawn via
    // `Rwl::configure_child` instead of mutating this process's environment.
    // Global `set_var`/`remove_var` are unsound off the main thread (they race
    // with any thread reading the environment, e.g. the bar thread) and are
    // `unsafe` under edition 2024.
    tracing::info!("WAYLAND_DISPLAY={}", state.socket_name);

    // WM IPC socket — exported to children as RWL_SOCK so `rwl msg` can reach it.
    #[cfg(feature = "ipc")]
    {
        state.rwl_sock = features::ipc::server::register(&loop_handle);
        if let Some(ref path) = state.rwl_sock {
            tracing::info!("RWL_SOCK={}", path.display());
        }
    }

    if let Some(cmd) = startup_cmd {
        tracing::info!("Spawning startup command: {}", cmd);

        // Create a pipe: child reads status from its stdin; compositor writes
        // to ipc_out (the write end) instead of stdout.
        if let Ok((reader, writer)) = std::io::pipe() {
            let mut command = std::process::Command::new("/bin/sh");
            command.arg("-c").arg(&cmd).stdin(reader);
            state.configure_child(&mut command);
            match command.spawn() {
                Ok(_) => {
                    state.ipc_out = Some(writer);
                }
                Err(e) => {
                    tracing::warn!("Failed to spawn startup command: {}", e);
                }
            }
        } else {
            // pipe() failed — spawn without the pipe.
            let mut command = std::process::Command::new("/bin/sh");
            command.arg("-c").arg(&cmd);
            state.configure_child(&mut command);
            match command.spawn() {
                Ok(_) => {}
                Err(e) => tracing::warn!("Failed to spawn startup command: {}", e),
            }
        }
    }

    // bar_cmd from config — spawned with the IPC pipe on stdin so it receives
    // tag/layout/title updates, just like the CLI `-s` flag.  Only used when
    // `-s` was not already provided (to avoid creating two ipc_out writers).
    {
        // Snapshot config and drop the read guard before spawning: `spawn` /
        // `configure_child` acquire their own config read lock, and nested
        // `RwLock` reads on one thread are not reentrant-safe.
        let bar_cmd = crate::config::get().bar_cmd.clone();
        #[cfg(feature = "startup-cmds")]
        let startup_cmds = crate::config::get().startup_cmds.clone();

        if state.ipc_out.is_none()
            && let Some((prog, args)) = bar_cmd.as_ref().and_then(|argv| argv.split_first())
        {
            match std::io::pipe() {
                Ok((reader, writer)) => {
                    let mut command = std::process::Command::new(prog);
                    command.args(args).stdin(reader);
                    state.configure_child(&mut command);
                    match command.spawn() {
                        Ok(_) => { state.ipc_out = Some(writer); }
                        Err(e) => tracing::warn!("Failed to spawn bar_cmd: {e}"),
                    }
                }
                Err(e) => tracing::warn!("pipe() failed for bar_cmd: {e}"),
            }
        }

        // Startup commands from config (run once, independent of any tag).
        #[cfg(feature = "startup-cmds")]
        for cmd in &startup_cmds {
            state.spawn(cmd);
        }
    }

    // Embedded bar — starts in a background thread, connects back to the
    // compositor's Wayland socket as a client.  Only active when WAYLAND_DISPLAY
    // is already set (done above) and no external bar_cmd was launched.
    // A pipe is created so the compositor's IPC status lines reach the bar thread.
    #[cfg(feature = "bar")]
    if state.ipc_out.is_none() {
        match std::io::pipe() {
            Ok((reader, writer)) => {
                state.ipc_out = Some(writer);
                features::bar::start(reader, crate::config::get().bar.clone(), state.socket_name.clone(), Arc::clone(&state.bar_has_notification));
            }
            Err(e) => tracing::warn!("[bar] failed to create IPC pipe: {e}"),
        }
    }

    // Poll for title/app-id changes that arrive without a wl_surface.commit
    // (Firefox tab-switch pattern). 250 ms is imperceptible to users and avoids
    // the 60 wakeups/sec that a 16 ms event-loop timeout would cause when idle.
    loop_handle
        .insert_source(
            smithay::reexports::calloop::timer::Timer::from_duration(
                std::time::Duration::from_millis(250),
            ),
            |_, (), state: &mut Rwl| {
                state.check_toplevel_titles();
                smithay::reexports::calloop::timer::TimeoutAction::ToDuration(
                    std::time::Duration::from_millis(250),
                )
            },
        )
        .map_err(|e| error::RwlError::EventLoop(e.to_string()))?;

    event_loop
        .run(
            None,
            &mut state,
            |state| {
                // Apply any commands enqueued by Lua hooks during this iteration
                // (on_focus / on_tag_switch / on_title_change callbacks).
                #[cfg(feature = "hooks")]
                crate::features::hooks::drain(state);
                state.display_handle.flush_clients().ok();
            },
        )
        .map_err(|e| error::RwlError::EventLoop(e.to_string()))?;

    // Pause DRM devices before state is dropped so Smithay's DrmAtomicSurface::drop()
    // sees the device as inactive and skips the restore-previous-state atomic commit
    // (which fails with EPERM when running without DRM master).
    if let Some(backend::BackendData::Udev(ref mut udev)) = state.backend {
        for dev in udev.devices.values_mut() {
            dev.drm.pause();
        }
    }

    #[cfg(feature = "ipc")]
    if let Some(path) = state.rwl_sock.take() {
        let _ = std::fs::remove_file(path);
    }

    tracing::info!("rwl exiting");
    Ok(())
}

// ---------------------------------------------------------------------------
// CLI argument parsing
// ---------------------------------------------------------------------------

struct Args {
    startup_cmd: Option<String>,
    log_level: tracing::Level,
}

fn parse_args() -> Args {
    let mut args_iter = std::env::args().skip(1);
    let mut startup_cmd: Option<String> = None;
    let mut log_level = tracing::Level::INFO;

    while let Some(arg) = args_iter.next() {
        match arg.as_str() {
            "-s" => {
                startup_cmd = args_iter.next();
            }
            "-v" | "--verbose" => {
                log_level = tracing::Level::INFO;
            }
            "-d" | "--debug" => {
                log_level = tracing::Level::DEBUG;
            }
            other => {
                eprintln!("Unknown argument: {other}");
                print_usage();
                std::process::exit(1);
            }
        }
    }

    Args { startup_cmd, log_level }
}

fn print_usage() {
    eprintln!("Usage: rwl [-s <startup-cmd>] [-v] [-d]");
    eprintln!("  -s <cmd>   Run <cmd> after compositor starts");
    eprintln!("  -v         Verbose logging");
    eprintln!("  -d         Debug logging");
}

// ---------------------------------------------------------------------------
// Tracing initialisation
// ---------------------------------------------------------------------------

fn init_tracing(level: tracing::Level) {
    use tracing_subscriber::EnvFilter;

    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(level.as_str()));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_writer(std::io::stderr)
        .init();
}
