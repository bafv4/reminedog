//! F3+C: how the agent learns the player's position without game code. The platform hooks
//! send the game its debug modifier and copy-location key events (as options.txt binds
//! them) and catch the location Minecraft then writes to the clipboard. This module is the
//! platform-neutral half: the request from the frame, the windows in which a clipboard write
//! is ours (swallowed) or the user's own F3+C (passed on), and the outcomes the frame turns
//! into notices.
//!
//! A request is taken at the platform's next chance (GLFW: after the next swap of its
//! window; SDL3: at the next `SDL_PollEvent`) and finishes right there, within that swap or
//! that poll loop, since the game handles each key event synchronously. A job still in
//! flight at the next frame was abandoned (a panic, a missed path), and [`expire`] fails it.
//!
//! Lock order: `router -> f3c` and `INJECTED -> f3c` are allowed. Nothing is locked while the
//! f3c lock is held, and no game code runs under it. Call [`take_job`] before locking
//! `INJECTED`.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering};
use std::time::{Duration, Instant};

use reminedog_core::{
    DebugKeys, InputId, Location, Naming, UNBOUND, input_by_name, key_label, parse_f3c,
};

/// Longest clipboard text looked at; F3+C writes well under 200 bytes.
const MAX_TEXT: usize = 512;
/// Outcomes kept for the frame (it drains them every frame; this only bounds a stall).
const MAX_OUTCOMES: usize = 16;
/// "No passive key" in the atomics below; neither is a real key code.
const NO_GLFW_KEY: i32 = i32::MIN;
const NO_SDL_KEY: u32 = u32::MAX;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Purpose {
    /// Record a waypoint at the position.
    Record,
    /// Update the known position (and show the way to the selected waypoint).
    Refresh,
}

impl Purpose {
    fn name(self) -> &'static str {
        match self {
            Purpose::Record => "record",
            Purpose::Refresh => "refresh",
        }
    }
}

/// A request for F3+C.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Job {
    pub purpose: Purpose,
    /// The window (`GLFWwindow*` or `SDL_Window*`) whose game gets the key events.
    pub window: usize,
    /// The game's bindings, from options.txt.
    pub keys: DebugKeys,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    /// A position from F3+C; `purpose` is `None` for the user's own F3+C.
    Located {
        location: Location,
        purpose: Option<Purpose>,
    },
    Failed {
        purpose: Purpose,
        reason: Failure,
    },
}

/// Why a request gave no position.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Failure {
    /// The game copied nothing: no player, or the world limits debug info.
    Refused,
    /// The game copied text that is not a location (it was swallowed all the same).
    Unreadable,
    /// The game did not have the cursor (a Minecraft screen was open).
    NotPlaying,
    /// The modifier, copy or crash key, or a Ctrl key, was held as the game reads it: physically,
    /// or as the output of a rebinding rule, (source, output), held by its source.
    KeysHeld(Option<(InputId, InputId)>),
    /// The debug modifier or the copy-location mapping is bound to a key the agent cannot
    /// send (the key's label).
    Unsupported(DebugKey, String),
    /// The modifier or copy-location mapping has no key.
    Unbound,
    /// The game had no key callback (GLFW) or window id (SDL3) to send the keys to.
    NoCallback,
    /// The clipboard or input hooks are not in.
    NoHooks,
    /// No answer in time.
    Timeout,
}

/// The debug mappings F3+C sends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DebugKey {
    Modifier,
    Copy,
}

/// What the clipboard saw while the key events were with the game.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Injected {
    /// A location: the job is done.
    Captured,
    /// A write that did not parse (swallowed): the game did handle the keys.
    Written,
    /// No write: the game refused.
    Nothing,
}

/// The game's codes of the keys F3+C sends (GLFW key codes, or SDL scancodes and keycodes).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyCodes<T> {
    pub modifier: T,
    pub copy: T,
}

/// Maps the debug modifier and copy keys with `code`. An unbound one makes F3+C impossible;
/// a key `code` does not know cannot be sent.
pub fn key_codes<T>(
    keys: &DebugKeys,
    code: impl Fn(&str) -> Option<T>,
) -> Result<KeyCodes<T>, Failure> {
    let map = |name: &str, which| {
        if name == UNBOUND {
            return Err(Failure::Unbound);
        }
        code(name).ok_or_else(|| Failure::Unsupported(which, key_label(name)))
    };
    Ok(KeyCodes {
        modifier: map(&keys.modifier, DebugKey::Modifier)?,
        copy: map(&keys.copy_location, DebugKey::Copy)?,
    })
}

/// The crash key, as a keyboard key to check (it is never sent, so any key will do): `None`
/// when it is unbound (it cannot arm then) or a mouse button (not checked: the hooks read
/// the keyboard's state only).
pub fn crash_key(keys: &DebugKeys, naming: Naming) -> Option<InputId> {
    input_by_name(&keys.crash, naming).filter(|id| matches!(id, InputId::Key(_)))
}

/// The clipboard as seen while a job's key events are with the game.
#[derive(Debug)]
struct Window {
    purpose: Purpose,
    written: bool,
    captured: bool,
}

#[derive(Debug)]
struct State {
    /// A request no platform took yet, and when it was made.
    pending: Option<(Job, Instant)>,
    /// A taken job that has no outcome yet.
    in_flight: Option<Purpose>,
    injected: Option<Window>,
    /// The user's own copy key went to the game.
    passive: bool,
    outcomes: Vec<Outcome>,
}

impl State {
    const fn new() -> Self {
        Self {
            pending: None,
            in_flight: None,
            injected: None,
            passive: false,
            outcomes: Vec::new(),
        }
    }

    fn busy(&self) -> bool {
        self.pending.is_some() || self.in_flight.is_some()
    }

    fn watching(&self) -> bool {
        self.injected.is_some() || self.passive
    }

    fn push(&mut self, outcome: Outcome) {
        if self.outcomes.len() >= MAX_OUTCOMES {
            self.outcomes.remove(0);
        }
        self.outcomes.push(outcome);
    }

    /// False (and nothing changes) while another job is pending or in flight.
    fn request(&mut self, job: Job, now: Instant) -> bool {
        if self.busy() {
            return false;
        }
        self.pending = Some((job, now));
        true
    }

    fn take_job(&mut self, window: Option<usize>) -> Option<Job> {
        let (job, _) = self
            .pending
            .take_if(|(job, _)| window.is_none_or(|w| w == job.window))?;
        self.in_flight = Some(job.purpose);
        Some(job)
    }

    fn begin_injected(&mut self, purpose: Purpose) {
        self.injected = Some(Window {
            purpose,
            written: false,
            captured: false,
        });
    }

    fn end_injected(&mut self) -> Injected {
        let copied = match self.injected.take() {
            Some(Window { captured: true, .. }) => Injected::Captured,
            Some(Window { written: true, .. }) => Injected::Written,
            _ => Injected::Nothing,
        };
        if copied == Injected::Captured {
            self.in_flight = None;
        }
        copied
    }

    fn fail(&mut self, purpose: Purpose, reason: Failure) {
        self.injected = None;
        // Already failed by `expire`: one notice is enough.
        if self.in_flight.take().is_none() {
            log::debug!(
                "F3+C: {} already settled; {reason:?} dropped",
                purpose.name()
            );
            return;
        }
        match reason {
            Failure::Refused => log::info!("F3+C: refused ({})", purpose.name()),
            _ => log::info!("F3+C: {} failed: {reason:?}", purpose.name()),
        }
        self.push(Outcome::Failed { purpose, reason });
    }

    fn on_clipboard(&mut self, text: &str) -> bool {
        if let Some(window) = &mut self.injected {
            // Our key events made this write: it never reaches the clipboard.
            window.written = true;
            if window.captured {
                return true;
            }
            if text.len() > MAX_TEXT {
                log::warn!(
                    "F3+C: the game copied {} bytes, too long for a location",
                    text.len()
                );
                return true;
            }
            let purpose = window.purpose;
            match parse_f3c(text) {
                Ok(location) => {
                    window.captured = true;
                    log::info!("F3+C: location captured ({})", purpose.name());
                    self.push(Outcome::Located {
                        location,
                        purpose: Some(purpose),
                    });
                }
                Err(e) => log::warn!("F3+C: the game copied something else: {e}"),
            }
            return true;
        }
        if self.passive
            && text.len() <= MAX_TEXT
            && let Ok(location) = parse_f3c(text)
        {
            log::info!("F3+C: location captured (manual)");
            self.push(Outcome::Located {
                location,
                purpose: None,
            });
        }
        false
    }

    fn expire(&mut self, now: Instant, timeout: Duration) {
        if let Some((job, _)) = self
            .pending
            .take_if(|(_, at)| now.saturating_duration_since(*at) >= timeout)
        {
            log::warn!(
                "F3+C: the {} request was not taken in time",
                job.purpose.name()
            );
            self.push(Outcome::Failed {
                purpose: job.purpose,
                reason: Failure::Timeout,
            });
        }
        if let Some(purpose) = self.in_flight.take() {
            log::warn!("F3+C: the {} request did not finish", purpose.name());
            self.push(Outcome::Failed {
                purpose,
                reason: Failure::Timeout,
            });
        }
        if self.watching() {
            log::debug!("F3+C: closing a clipboard window left open");
            self.injected = None;
            self.passive = false;
        }
    }
}

static STATE: Mutex<State> = Mutex::new(State::new());
static HOOKS_READY: AtomicBool = AtomicBool::new(false);
/// Mirrors `State::watching`, so most clipboard writes skip the lock.
static WATCHING: AtomicBool = AtomicBool::new(false);
/// Mirrors `State.pending.is_some()`, for the platforms' check on every swap or poll.
static PENDING: AtomicBool = AtomicBool::new(false);
static PASSIVE_GLFW: AtomicI32 = AtomicI32::new(NO_GLFW_KEY);
static PASSIVE_SDL: AtomicU32 = AtomicU32::new(NO_SDL_KEY);

/// Runs `f` on the state, then updates the flags read without the lock.
fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> R {
    let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
    let result = f(&mut state);
    WATCHING.store(state.watching(), Ordering::Release);
    PENDING.store(state.pending.is_some(), Ordering::Release);
    result
}

/// Set by the platform once its clipboard detour and the detour it sends keys through are in.
pub fn set_hooks_ready(ready: bool) {
    HOOKS_READY.store(ready, Ordering::Release);
    if ready {
        log::info!("F3+C: clipboard hooks ready");
    } else {
        log::warn!("F3+C: hooks missing; no waypoints");
    }
}

/// Asks the platform to send F3+C. While another job is pending or in flight this does
/// nothing (the UI disables its buttons meanwhile).
pub fn request(job: Job) -> Result<(), Failure> {
    if !HOOKS_READY.load(Ordering::Acquire) {
        return Err(Failure::NoHooks);
    }
    let purpose = job.purpose;
    if with_state(|s| s.request(job, Instant::now())) {
        log::info!("F3+C: requested ({})", purpose.name());
    } else {
        log::debug!("F3+C: busy; {} request ignored", purpose.name());
    }
    Ok(())
}

pub fn busy() -> bool {
    with_state(|s| s.busy())
}

/// The pending job, for the platform to send now. GLFW passes the window it just swapped
/// (a job only goes to its own window); SDL3 passes `None`.
pub fn take_job(window: Option<usize>) -> Option<Job> {
    if !PENDING.load(Ordering::Acquire) {
        return None;
    }
    with_state(|s| s.take_job(window))
}

/// The copy key's event is about to reach the game: a clipboard write until
/// [`end_injected`] is ours.
pub fn begin_injected(purpose: Purpose) {
    with_state(|s| s.begin_injected(purpose));
}

/// Ends the injected window: Captured (location parsed), Written (something was written but
/// did not parse), or Nothing (no write: the game refused). After Captured the job is done;
/// otherwise the platform reports it with [`fail`].
pub fn end_injected() -> Injected {
    with_state(State::end_injected)
}

/// Settles the job in flight with `reason` (ignored if it is settled already).
pub fn fail(purpose: Purpose, reason: Failure) {
    with_state(|s| s.fail(purpose, reason));
}

/// The copy-location key's codes, for spotting the user's own F3+C.
pub fn set_passive_keys(glfw_copy: Option<i32>, sdl_copy: Option<u32>) {
    PASSIVE_GLFW.store(glfw_copy.unwrap_or(NO_GLFW_KEY), Ordering::Relaxed);
    PASSIVE_SDL.store(sdl_copy.unwrap_or(NO_SDL_KEY), Ordering::Relaxed);
}

pub fn passive_key_glfw(key: i32) -> bool {
    key != NO_GLFW_KEY && PASSIVE_GLFW.load(Ordering::Relaxed) == key
}

pub fn passive_key_sdl(scancode: u32) -> bool {
    scancode != NO_SDL_KEY && PASSIVE_SDL.load(Ordering::Relaxed) == scancode
}

/// The user's copy key is going to the game: a location it copies until [`end_passive`] is
/// theirs (it still reaches the clipboard).
pub fn begin_passive() {
    with_state(|s| s.passive = true);
}

pub fn end_passive() {
    if WATCHING.load(Ordering::Acquire) {
        with_state(|s| s.passive = false);
    }
}

/// Whether a clipboard write now may be F3+C's (ours or the user's): the detours read the
/// text only then.
pub fn watching() -> bool {
    WATCHING.load(Ordering::Acquire)
}

/// From the clipboard detours. true = swallow the write (it was ours).
pub fn on_clipboard(text: &str) -> bool {
    if !WATCHING.load(Ordering::Acquire) {
        return false;
    }
    with_state(|s| s.on_clipboard(text))
}

pub fn take_outcomes() -> Vec<Outcome> {
    with_state(|s| std::mem::take(&mut s.outcomes))
}

/// Called every frame: a job not taken within `timeout`, or taken but still in flight at the
/// next frame (both platforms finish a job within one swap / one poll loop), fails with
/// Timeout; clipboard windows left open are closed.
pub fn expire(timeout: Duration) {
    with_state(|s| s.expire(Instant::now(), timeout));
}

#[cfg(test)]
mod tests {
    use reminedog_core::{glfw_key, sdl_keycode, sdl_scancode};

    use super::*;

    const COPIED: &str =
        "/execute in minecraft:the_nether run tp @s 12.50 64.00 -7.25 -179.90 12.30";
    const WINDOW: usize = 0x1000;
    const TIMEOUT: Duration = Duration::from_secs(2);

    fn job(purpose: Purpose) -> Job {
        Job {
            purpose,
            window: WINDOW,
            keys: DebugKeys::default(),
        }
    }

    fn located(purpose: Option<Purpose>) -> Outcome {
        Outcome::Located {
            location: parse_f3c(COPIED).unwrap(),
            purpose,
        }
    }

    fn failed(purpose: Purpose, reason: Failure) -> Outcome {
        Outcome::Failed { purpose, reason }
    }

    /// A state with `purpose`'s job taken by the platform.
    fn in_flight(purpose: Purpose) -> State {
        let mut s = State::new();
        assert!(s.request(job(purpose), Instant::now()));
        assert_eq!(s.take_job(Some(WINDOW)), Some(job(purpose)));
        s
    }

    #[test]
    fn captured_location_is_swallowed() {
        let mut s = State::new();
        let now = Instant::now();
        assert!(!s.busy());
        assert!(s.request(job(Purpose::Record), now));
        assert!(s.busy());
        // Busy: a second request changes nothing.
        assert!(!s.request(job(Purpose::Refresh), now));
        // GLFW: only the job's own window takes it.
        assert_eq!(s.take_job(Some(WINDOW + 1)), None);
        assert_eq!(s.take_job(Some(WINDOW)), Some(job(Purpose::Record)));
        assert_eq!(s.take_job(Some(WINDOW)), None);
        assert!(s.busy());

        s.begin_injected(Purpose::Record);
        assert!(s.watching());
        assert!(s.on_clipboard(COPIED));
        // A second write in the same window is ours too.
        assert!(s.on_clipboard("anything"));
        assert_eq!(s.end_injected(), Injected::Captured);
        assert!(!s.busy());
        assert!(!s.watching());
        assert_eq!(s.outcomes, [located(Some(Purpose::Record))]);
        // Outside any window, writes are left alone.
        assert!(!s.on_clipboard(COPIED));
        assert_eq!(s.outcomes.len(), 1);
    }

    #[test]
    fn sdl_takes_the_job_for_any_window() {
        let mut s = State::new();
        assert!(s.request(job(Purpose::Refresh), Instant::now()));
        assert_eq!(s.take_job(None), Some(job(Purpose::Refresh)));
        assert_eq!(s.in_flight, Some(Purpose::Refresh));
    }

    #[test]
    fn refused_without_a_write() {
        let mut s = in_flight(Purpose::Refresh);
        s.begin_injected(Purpose::Refresh);
        assert_eq!(s.end_injected(), Injected::Nothing);
        // Busy until the platform has undone the overlay toggle and reported it.
        assert!(s.busy());
        s.fail(Purpose::Refresh, Failure::Refused);
        assert!(!s.busy());
        assert_eq!(s.outcomes, [failed(Purpose::Refresh, Failure::Refused)]);
    }

    #[test]
    fn unreadable_write_is_swallowed() {
        let mut s = in_flight(Purpose::Record);
        s.begin_injected(Purpose::Record);
        assert!(s.on_clipboard("/execute in bad:DIM run tp @s 1 2 3 4 5"));
        assert_eq!(s.end_injected(), Injected::Written);
        s.fail(Purpose::Record, Failure::Unreadable);
        assert_eq!(s.outcomes, [failed(Purpose::Record, Failure::Unreadable)]);

        // Too long to parse, but still the game's answer to our keys.
        let mut s = in_flight(Purpose::Record);
        s.begin_injected(Purpose::Record);
        assert!(s.on_clipboard(&"x".repeat(MAX_TEXT + 1)));
        assert_eq!(s.end_injected(), Injected::Written);
    }

    #[test]
    fn validation_failure() {
        let mut s = in_flight(Purpose::Record);
        s.fail(Purpose::Record, Failure::KeysHeld(None));
        assert!(!s.busy());
        assert_eq!(
            s.outcomes,
            [failed(Purpose::Record, Failure::KeysHeld(None))]
        );
    }

    #[test]
    fn users_own_f3c_is_passed_on() {
        let mut s = State::new();
        s.passive = true;
        assert!(s.watching());
        assert!(!s.on_clipboard("some chat text"));
        assert!(!s.on_clipboard(&format!("{COPIED}{}", " ".repeat(MAX_TEXT))));
        assert!(s.outcomes.is_empty());
        assert!(!s.on_clipboard(COPIED));
        assert_eq!(s.outcomes, [located(None)]);
        s.passive = false;
        assert!(!s.watching());
        assert!(!s.on_clipboard(COPIED));
        assert_eq!(s.outcomes.len(), 1);
    }

    #[test]
    fn unanswered_requests_time_out() {
        let start = Instant::now();
        let mut s = State::new();
        assert!(s.request(job(Purpose::Record), start));
        s.expire(start + Duration::from_millis(500), TIMEOUT);
        assert!(s.busy());
        assert!(s.outcomes.is_empty());
        s.expire(start + TIMEOUT, TIMEOUT);
        assert!(!s.busy());
        assert_eq!(s.outcomes, [failed(Purpose::Record, Failure::Timeout)]);
    }

    #[test]
    fn abandoned_job_times_out_at_the_next_frame() {
        let mut s = in_flight(Purpose::Refresh);
        s.begin_injected(Purpose::Refresh);
        s.passive = true;
        s.expire(Instant::now(), TIMEOUT);
        assert!(!s.busy());
        assert!(!s.watching());
        assert_eq!(s.outcomes, [failed(Purpose::Refresh, Failure::Timeout)]);
        // A late report of the same job adds no second notice.
        s.fail(Purpose::Refresh, Failure::Refused);
        assert_eq!(s.outcomes.len(), 1);
        // And the next request goes through.
        assert!(s.request(job(Purpose::Record), Instant::now()));
    }

    #[test]
    fn outcomes_are_bounded() {
        let mut s = State::new();
        s.passive = true;
        for _ in 0..MAX_OUTCOMES + 5 {
            s.on_clipboard(COPIED);
        }
        assert_eq!(s.outcomes.len(), MAX_OUTCOMES);
    }

    #[test]
    fn debug_key_codes() {
        let keys = DebugKeys::default();
        assert_eq!(
            key_codes(&keys, glfw_key),
            Ok(KeyCodes {
                modifier: 292,
                copy: 67,
            })
        );
        let sdl = |name: &str| sdl_scancode(name).zip(sdl_keycode(name));
        assert_eq!(
            key_codes(&keys, sdl),
            Ok(KeyCodes {
                modifier: (60, 0x4000_003C),
                copy: (6, 0x63),
            })
        );
        assert_eq!(crash_key(&keys, Naming::Modern), Some(InputId::Key(6)));

        let with = |modifier: &str, copy: &str, crash: &str| DebugKeys {
            modifier: modifier.into(),
            copy_location: copy.into(),
            crash: crash.into(),
            shared_with_copy: Vec::new(),
        };
        let f3 = "key.keyboard.f3";
        let c = "key.keyboard.c";
        // An unbound crash key needs no check, nor a mouse button (not checked); any key is
        // checked.
        assert_eq!(crash_key(&with(f3, c, UNBOUND), Naming::Glfw), None);
        assert_eq!(
            crash_key(&with(f3, c, "key.mouse.left"), Naming::Glfw),
            None
        );
        assert_eq!(
            crash_key(&with(f3, c, "key.keyboard.pause"), Naming::Modern),
            input_by_name("key.keyboard.pause", Naming::Modern)
        );
        assert!(key_codes(&with(f3, c, "key.keyboard.pause"), glfw_key).is_ok());
        assert_eq!(
            key_codes(&with(UNBOUND, c, c), glfw_key),
            Err(Failure::Unbound)
        );
        assert_eq!(
            key_codes(&with(f3, UNBOUND, c), glfw_key),
            Err(Failure::Unbound)
        );
        assert_eq!(
            key_codes(&with(f3, "key.keyboard.left.shift", c), glfw_key),
            Err(Failure::Unsupported(
                DebugKey::Copy,
                key_label("key.keyboard.left.shift")
            ))
        );
        // SDL3 has no F25.
        assert_eq!(
            key_codes(&with("key.keyboard.f25", c, c), sdl),
            Err(Failure::Unsupported(DebugKey::Modifier, "F25".into()))
        );
    }
}
