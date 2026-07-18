//! Thin native screen locker (`lock` feature).
//!
//! Unlike the `ext-session-lock-v1` path in [`crate::handlers::session_lock`]
//! (which lets an external client such as swaylock draw the lock screen), this
//! is a *built-in* locker: the compositor itself owns the locked session, draws
//! a minimal password-progress bar, and authenticates against PAM.
//!
//! Security model (paranoid by design):
//! * Reuses the existing `Rwl::locked` gate, so while a native lock is active
//!   the render path composites **only** cursor + black backdrop + this bar
//!   (never client/layer content), and screencopy is refused — see
//!   [`crate::handlers::screencopy`] and the security-hardening invariants.
//! * Keyboard focus is cleared to `None` on lock and every key is intercepted
//!   before it can reach a client (see [`crate::handlers::input`]); there is no
//!   unlock keybind — the *only* way out is a successful PAM authentication.
//! * The typed password never leaves this module as plaintext: it lives in a
//!   pre-allocated [`Zeroizing`] buffer (no reallocation ⇒ no stale copies in
//!   freed pages) and is zeroed on drop. The bar renders only its *length*.
//! * PAM runs on a worker thread — it blocks and deliberately delays on
//!   failure, which must never stall the calloop event loop. The result is fed
//!   back through a calloop channel.

use smithay::backend::renderer::element::solid::SolidColorRenderElement;
use smithay::backend::renderer::element::Id;
use smithay::backend::renderer::utils::CommitCounter;
use smithay::input::keyboard::keysyms as xkb;
use smithay::input::pointer::MotionEvent;
use smithay::output::Output;
use smithay::reexports::calloop::channel::{channel, Channel, Event as ChannelEvent, Sender};
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::reexports::calloop::RegistrationToken;
use smithay::utils::{Physical, Point, Rectangle, Size, SERIAL_COUNTER};
use smithay::wayland::seat::WaylandFocus;
use zeroize::{Zeroize, Zeroizing};

use crate::state::Rwl;

/// Maximum password reservation. The buffer is allocated once at this capacity
/// and never grows, so `push` never reallocates and leaves no plaintext copy in
/// freed heap pages. Typed characters beyond this are ignored.
const PASSWORD_CAP: usize = 1024;

/// Number of typed characters that fill the progress bar completely. Chosen so
/// the bar keeps growing for realistic passwords without revealing the exact
/// length once it saturates.
const FILL_CHARS: usize = 24;

/// How long the red failure flash stays on screen before resetting to the
/// neutral typing state.
const FLASH_MS: u64 = 600;

/// Redraw interval for the auth "working" animation (~60 fps).
const ANIM_FRAME_MS: u64 = 16;

/// How long the auth bar takes to grow from empty to full while the PAM check
/// runs. Tuned to roughly track `pam_unix`'s default failure delay so the bar
/// completes about when a wrong password would turn it red.
const ANIM_FILL_MS: f64 = 2000.0;

/// Fill colour while typing.
const BLUE: [f32; 4] = [0.30, 0.52, 0.90, 1.0];

/// Fill colour while authenticating — a plain grey with no blue tint.
const NEUTRAL: [f32; 4] = [0.72, 0.72, 0.72, 1.0];

/// Fill colour for a rejected attempt.
const RED: [f32; 4] = [0.90, 0.16, 0.16, 1.0];

/// PAM service name. On NixOS this requires `security.pam.services.rwl-lock = {};`
/// (which wires up the standard `pam_unix` user-auth stack); without it every
/// authentication fails closed and the screen stays locked.
const PAM_SERVICE: &str = "rwl-lock";

/// Phase of the locked session, driving the bar colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Accepting keystrokes.
    Typing,
    /// A PAM authentication is in flight on the worker thread.
    Authenticating,
    /// The last attempt was rejected; the bar flashes red until the timer fires.
    Failed,
}

/// Live state for a native lock session. Held in `Rwl::native_lock` only while
/// locked; dropping it zeroes the password.
pub struct LockState {
    /// Login name to authenticate against, captured once at lock time.
    user: String,
    /// Typed password. Never rendered; only its length feeds the bar.
    password: Zeroizing<String>,
    /// Current phase.
    phase: Phase,
    /// Monotonic generation, bumped on every visible change so the damage
    /// tracker sees a new commit for the (stable-id) bar elements only when the
    /// bar actually changed.
    generation: usize,
    /// When the in-flight PAM check started, driving the left→right "working"
    /// animation. `None` unless a check is running.
    auth_started: Option<std::time::Instant>,
    /// Sender handed to PAM worker threads; the paired receiver is registered in
    /// the event loop (see `auth_source`).
    tx: Sender<bool>,
    /// Loop registration for the auth-result channel; removed on unlock.
    auth_source: RegistrationToken,
    /// One-shot timer that clears the red failure flash, if pending.
    flash_timer: Option<RegistrationToken>,
    /// Repeating timer that redraws the auth "working" animation, if running.
    anim_timer: Option<RegistrationToken>,
}

// Manual Debug so the password is never printed, even accidentally, via a
// `{:?}` on `Rwl`.
impl std::fmt::Debug for LockState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LockState")
            .field("password_len", &self.password.chars().count())
            .field("phase", &self.phase)
            .finish_non_exhaustive()
    }
}

impl LockState {
    /// Append a printable character, ignoring input once the buffer is at
    /// capacity or a PAM check is already running.
    fn push(&mut self, c: char) {
        if self.phase == Phase::Authenticating || self.password.len() >= PASSWORD_CAP {
            return;
        }
        self.password.push(c);
        self.generation = self.generation.wrapping_add(1);
    }

    /// Remove the last character.
    fn backspace(&mut self) {
        if self.phase == Phase::Authenticating {
            return;
        }
        self.password.pop();
        self.generation = self.generation.wrapping_add(1);
    }

    /// Discard the whole buffer (Escape) without unlocking, wiping the bytes
    /// (including spare capacity) rather than merely truncating. No reallocation
    /// ever happens (buffer is pre-sized to `PASSWORD_CAP`), so this leaves no
    /// plaintext copy behind.
    fn clear(&mut self) {
        if self.phase == Phase::Authenticating {
            return;
        }
        self.password.zeroize();
        self.generation = self.generation.wrapping_add(1);
    }

    /// Hand the current password to a PAM worker thread. The buffer is *moved*
    /// out (replaced by a fresh pre-sized one) so this state never holds the
    /// plaintext during the check, and the worker zeroes it on drop.
    fn submit(&mut self) {
        if self.phase == Phase::Authenticating || self.password.is_empty() {
            return;
        }
        self.phase = Phase::Authenticating;
        self.auth_started = Some(std::time::Instant::now());
        self.generation = self.generation.wrapping_add(1);

        let password =
            std::mem::replace(&mut self.password, Zeroizing::new(String::with_capacity(PASSWORD_CAP)));
        let tx = self.tx.clone();
        let user = self.user.clone();
        std::thread::spawn(move || {
            let ok = authenticate(&user, &password);
            // `password` (Zeroizing) is dropped and wiped here.
            let _ = tx.send(ok);
        });
    }
}

/// Engage the native lock. No-op if the session is already locked (by either a
/// native lock or an external `ext-session-lock-v1` client), so the two schemes
/// can never coexist.
pub fn lock(state: &mut Rwl) {
    if state.locked || state.native_lock.is_some() {
        return;
    }

    let (tx, channel): (Sender<bool>, Channel<bool>) = channel();
    let Ok(auth_source) = state.loop_handle.insert_source(channel, |event, (), state| {
        if let ChannelEvent::Msg(ok) = event {
            on_auth_result(state, ok);
        }
    }) else {
        tracing::warn!("[lock] failed to register auth channel; not locking");
        return;
    };

    state.native_lock = Some(LockState {
        user: current_user().unwrap_or_default(),
        password: Zeroizing::new(String::with_capacity(PASSWORD_CAP)),
        phase: Phase::Typing,
        generation: 0,
        auth_started: None,
        tx,
        auth_source,
        flash_timer: None,
        anim_timer: None,
    });
    state.locked = true;
    #[cfg(feature = "hooks")]
    crate::features::hooks::lock(state);

    // Abandon any in-progress pointer grab (e.g. locking mid-drag) so it cannot
    // resume against a client window after unlock. Input handlers ignore grabs
    // while locked anyway; this just clears the leftover mode.
    state.cursor_mode = crate::state::CursorMode::Normal;

    // Cancel any key-repeat left over from a held keybind so a repeating action
    // (e.g. a held spawn/focus binding) cannot keep firing behind the lock.
    if let Some(token) = state.key_repeat_timer.take() {
        state.loop_handle.remove(token);
    }
    state.key_repeat_action = None;

    // Strip keyboard focus from every client so no keystroke can reach one even
    // if the input interception were somehow bypassed.
    if let Some(kb) = state.keyboard.clone() {
        kb.set_focus(state, None, SERIAL_COUNTER.next_serial());
    }
    reevaluate_pointer(state);
    state.schedule_render();
}

/// Feed a key press into the locker. Called from the input handler while a
/// native lock is active; returns having fully consumed the key.
pub fn feed_key(state: &mut Rwl, keysym: u32, ch: Option<char>) {
    let Some(lock) = state.native_lock.as_mut() else {
        return;
    };
    match keysym {
        xkb::KEY_Return | xkb::KEY_KP_Enter => lock.submit(),
        xkb::KEY_Escape => lock.clear(),
        xkb::KEY_BackSpace => lock.backspace(),
        _ => {
            if let Some(c) = ch
                && !c.is_control()
            {
                lock.push(c);
            }
        }
    }
    // A fresh submit just entered the Authenticating phase — start the left→right
    // "working" animation that runs until the PAM result arrives.
    let start_anim = state
        .native_lock
        .as_ref()
        .is_some_and(|l| l.phase == Phase::Authenticating && l.anim_timer.is_none());
    if start_anim {
        start_auth_animation(state);
    }
    state.schedule_render();
}

/// Register a repeating timer that redraws the auth "working" animation while a
/// PAM check is in flight, dropping itself once the phase leaves Authenticating.
fn start_auth_animation(state: &mut Rwl) {
    let frame = std::time::Duration::from_millis(ANIM_FRAME_MS);
    let token = state
        .loop_handle
        .insert_source(Timer::from_duration(frame), move |_, (), state| {
            let running = state.native_lock.as_mut().is_some_and(|lock| {
                if lock.phase == Phase::Authenticating {
                    lock.generation = lock.generation.wrapping_add(1);
                    true
                } else {
                    lock.anim_timer = None;
                    false
                }
            });
            state.schedule_render();
            if running {
                TimeoutAction::ToDuration(frame)
            } else {
                TimeoutAction::Drop
            }
        })
        .ok();
    if let Some(lock) = state.native_lock.as_mut() {
        lock.anim_timer = token;
    }
}

/// Handle a PAM result delivered from the worker thread.
fn on_auth_result(state: &mut Rwl, ok: bool) {
    if ok {
        unlock(state);
        return;
    }

    // Rejected: clear the buffer and flash red until the one-shot timer resets
    // the phase.
    let has_lock = state
        .native_lock
        .as_mut()
        .map(|lock| {
            lock.password.clear();
            lock.phase = Phase::Failed;
            lock.auth_started = None;
            lock.generation = lock.generation.wrapping_add(1);
        })
        .is_some();
    if !has_lock {
        return;
    }

    let timer = state
        .loop_handle
        .insert_source(
            Timer::from_duration(std::time::Duration::from_millis(FLASH_MS)),
            |_, (), state| {
                let reset = state.native_lock.as_mut().is_some_and(|lock| {
                    lock.flash_timer = None;
                    if lock.phase == Phase::Failed {
                        lock.phase = Phase::Typing;
                        lock.generation = lock.generation.wrapping_add(1);
                        true
                    } else {
                        false
                    }
                });
                if reset {
                    state.schedule_render();
                }
                TimeoutAction::Drop
            },
        )
        .ok();
    if let Some(lock) = state.native_lock.as_mut() {
        lock.flash_timer = timer;
    }
    state.schedule_render();
}

/// Tear down the native lock after a successful authentication and restore
/// input focus to the previously focused window.
fn unlock(state: &mut Rwl) {
    if let Some(lock) = state.native_lock.take() {
        state.loop_handle.remove(lock.auth_source);
        if let Some(timer) = lock.flash_timer {
            state.loop_handle.remove(timer);
        }
        if let Some(timer) = lock.anim_timer {
            state.loop_handle.remove(timer);
        }
        // `lock` (and its Zeroizing password) is wiped as it drops here.
    }
    state.locked = false;
    #[cfg(feature = "hooks")]
    crate::features::hooks::unlock(state);

    if let Some(kb) = state.keyboard.clone() {
        let surface = state
            .focused_window()
            .and_then(|w| w.wl_surface().map(std::borrow::Cow::into_owned));
        kb.set_focus(state, surface, SERIAL_COUNTER.next_serial());
    }
    reevaluate_pointer(state);
    state.schedule_render();
}

/// Re-run pointer focus so it routes correctly for the new locked/unlocked
/// state (mirrors [`crate::handlers::session_lock`]).
fn reevaluate_pointer(state: &mut Rwl) {
    if let Some(ptr) = state.pointer.clone() {
        let loc = ptr.current_location();
        let under = state.pointer_focus_under(loc);
        let serial = SERIAL_COUNTER.next_serial();
        #[allow(clippy::cast_possible_truncation)]
        let time_ms = std::time::Duration::from(state.clock.now()).as_millis() as u32;
        ptr.motion(state, under, &MotionEvent { location: loc, serial, time: time_ms });
        ptr.frame(state);
    }
}

/// Resolve the login name to authenticate against, from the compositor's own
/// environment (the logged-in user's session).
fn current_user() -> Option<String> {
    std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .ok()
        .filter(|u| !u.is_empty())
}

/// Blocking PAM password check. Runs on a worker thread. Fails closed on any
/// error (missing service, wrong password, PAM misconfiguration).
fn authenticate(user: &str, password: &str) -> bool {
    match pam::Authenticator::with_password(PAM_SERVICE) {
        Ok(mut auth) => {
            auth.get_handler().set_credentials(user, password);
            auth.authenticate().is_ok()
        }
        Err(e) => {
            tracing::warn!("[lock] PAM init failed for service {PAM_SERVICE:?}: {e}");
            false
        }
    }
}

// ---------------------------------------------------------------------------
// Rendering — a pure function of the lock state (no mutation, no allocation of
// the password).
// ---------------------------------------------------------------------------

/// Stable render-element ids for the two bar quads, reused across frames so the
/// damage tracker only repaints when `generation` (the commit) advances.
fn bar_ids() -> &'static [Id; 2] {
    static IDS: std::sync::OnceLock<[Id; 2]> = std::sync::OnceLock::new();
    IDS.get_or_init(|| [Id::new(), Id::new()])
}

/// Build the progress-bar elements for `output`: a dark track plus a fill.
/// While typing the fill grows from the centre outward with the number of typed
/// characters; while authenticating it sweeps left→right as a "working"
/// indicator. It stays a neutral grey throughout, turning red only when an
/// attempt is rejected.
#[must_use]
pub fn bar_elements(lock: &LockState, output: &Output) -> Vec<SolidColorRenderElement> {
    let Some(mode) = output.current_mode() else {
        return Vec::new();
    };
    let (ow, oh) = (mode.size.w, mode.size.h);
    if ow <= 0 || oh <= 0 {
        return Vec::new();
    }
    let scale = output.current_scale().fractional_scale().max(1.0);

    // Track geometry: 60% of the output width, a few px tall, centred.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let track_w = (f64::from(ow) * 0.6) as i32;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let height = (8.0 * scale).round() as i32;
    let x = (ow - track_w) / 2;
    let y = oh / 2 - height / 2;

    let commit = CommitCounter::from(lock.generation);

    let track = SolidColorRenderElement::new(
        bar_ids()[0].clone(),
        Rectangle::new(Point::<i32, Physical>::from((x, y)), Size::from((track_w, height))),
        commit,
        [0.14, 0.14, 0.17, 1.0],
        smithay::backend::renderer::element::Kind::Unspecified,
    );

    // Blue while typing, grey while the PAM check is in flight; the bar only
    // turns red once an attempt is actually rejected.
    // Anchor: while typing the fill grows from the centre outward; while
    // authenticating it sweeps left→right as a "working" indicator.
    let (fill_frac, color, from_left) = match lock.phase {
        Phase::Typing => (fill_fraction(lock), BLUE, false),
        Phase::Authenticating => (auth_fraction(lock), NEUTRAL, true),
        Phase::Failed => (1.0, RED, false),
    };
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let fill_w = (f64::from(track_w) * fill_frac) as i32;
    if fill_w <= 0 {
        return vec![track];
    }
    let fill_x = if from_left { x } else { x + (track_w - fill_w) / 2 };
    let fill = SolidColorRenderElement::new(
        bar_ids()[1].clone(),
        Rectangle::new(Point::<i32, Physical>::from((fill_x, y)), Size::from((fill_w, height))),
        commit,
        color,
        smithay::backend::renderer::element::Kind::Unspecified,
    );
    // Fill in front of the track.
    vec![fill, track]
}

/// Fraction of the bar filled by the current password length, saturating at 1.0.
fn fill_fraction(lock: &LockState) -> f64 {
    let chars = lock.password.chars().count().min(FILL_CHARS);
    #[allow(clippy::cast_precision_loss)]
    let frac = chars as f64 / FILL_CHARS as f64;
    frac
}

/// Fraction of the auth "working" sweep, from the time the PAM check started,
/// saturating at 1.0.
fn auth_fraction(lock: &LockState) -> f64 {
    lock.auth_started
        .map_or(0.0, |started| (started.elapsed().as_secs_f64() * 1000.0 / ANIM_FILL_MS).min(1.0))
}
