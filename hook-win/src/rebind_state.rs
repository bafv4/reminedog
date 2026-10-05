//! The keys the game reads as held while the key rebinding is at work: one lock-free table by
//! SDL scancode, and the safety net for releases that never come ([`lost_releases`]).
//!
//! Minecraft keeps its own idea of which keys are down from the events it gets, but also asks
//! the window library for the key state: when a screen closes (`KeyMapping.setAll` re-reads
//! every keyboard mapping), for Ctrl (dropping a stack, picking a block with its data), for the
//! debug crash key on every key event, and 1.16 for F3. A source whose press went to the game
//! as another key must read as up there and the output as held, or closing a screen leaves the
//! source's mappings held and lets go of the output's.
//!
//! The platform rewrites the table from the router's [`RebindState`] right before it hands the
//! game an event (GLFW updates its own key state before the callback, too), after focus loss
//! and after the safety net. `glfwGetKey`, `SDL_GetKeyboardState`, the modifier bits of key
//! events and F3+C's checks read it. Mouse buttons are not in it: the game never asks for their
//! state.
//!
//! Lock order: `router -> SUSPECTS`. The table takes no lock; only the router's holder writes it.

use std::ops::RangeInclusive;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use reminedog_core::{InputId, SCANCODE_COUNT, input_name, win_scancode_of};
use reminedog_render::{Output, RebindState};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, MAPVK_VSC_TO_VK_EX, MapVirtualKeyW, VK_DECIMAL, VK_LBUTTON, VK_MBUTTON,
    VK_NUMLOCK, VK_NUMPAD0, VK_NUMPAD1, VK_PAUSE, VK_RBUTTON, VK_XBUTTON1, VK_XBUTTON2,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_SWAPBUTTON};

use crate::input;

/// The key's real state.
const REAL: u8 = 0;
/// The game must read the key as held: a rule holds it down.
const PRESSED: u8 = 1;
/// The game must read the key as up: a rule took its press.
const RELEASED: u8 = 2;

/// SDL scancodes of the modifier keys: the left Ctrl, Shift, Alt and Windows keys, then the
/// right ones.
pub const MODIFIER_SCANCODES: RangeInclusive<usize> = 224..=231;

/// How long a held source must read as up before its release counts as lost. A release on its
/// way (the key went up after the game's last event poll) reaches the game before that.
const LOST_RELEASE_DELAY: Duration = Duration::from_millis(100);

static TABLE: [AtomicU8; SCANCODE_COUNT] = [const { AtomicU8::new(REAL) }; SCANCODE_COUNT];
/// The window (`GLFWwindow*` or `SDL_Window*`) of the events the table was last written for.
static WINDOW: AtomicUsize = AtomicUsize::new(0);
/// Some entry is not `REAL`.
static FORCED: AtomicBool = AtomicBool::new(false);
/// Some modifier key's entry is not `REAL`.
static MODIFIERS_FORCED: AtomicBool = AtomicBool::new(false);
/// A rule holds its output down (a key or a mouse button).
static REBOUND: AtomicBool = AtomicBool::new(false);
/// A press the game got as it was holds a rule's output ([`RebindState::passthrough`]).
static PASSED: AtomicBool = AtomicBool::new(false);
/// Ids the safety net watches that Windows reported as up at the last checks, and since when.
static SUSPECTS: Mutex<Vec<(InputId, Instant)>> = Mutex::new(Vec::new());

fn key_index(id: InputId) -> Option<usize> {
    match id {
        InputId::Key(sc) => Some(usize::from(sc)).filter(|&i| i < SCANCODE_COUNT),
        InputId::Mouse(_) => None,
    }
}

/// The table for `state`. Where a key is both taken and held (A and B swapped, both held), the
/// game has it down.
fn table_of(state: &RebindState) -> [u8; SCANCODE_COUNT] {
    let mut table = [REAL; SCANCODE_COUNT];
    for &id in &state.taken {
        if let Some(i) = key_index(id) {
            table[i] = RELEASED;
        }
    }
    for &(_, id) in &state.held {
        if let Some(i) = key_index(id) {
            table[i] = PRESSED;
        }
    }
    table
}

/// Rewrites the table from the router's state, for the events of `window`. Call with the router
/// locked, right after the call that changed the state and before the game gets the event.
pub fn apply(window: usize, state: &RebindState) {
    let rebound = !state.held.is_empty();
    WINDOW.store(window, Ordering::Relaxed);
    PASSED.store(!state.passthrough.is_empty(), Ordering::Release);
    if !rebound && state.taken.is_empty() && !FORCED.load(Ordering::Acquire) {
        REBOUND.store(false, Ordering::Release);
        return;
    }
    let table = table_of(state);
    for (entry, &value) in TABLE.iter().zip(&table) {
        if entry.load(Ordering::Relaxed) != value {
            entry.store(value, Ordering::Relaxed);
        }
    }
    MODIFIERS_FORCED.store(
        table[MODIFIER_SCANCODES].iter().any(|&v| v != REAL),
        Ordering::Release,
    );
    FORCED.store(table.iter().any(|&v| v != REAL), Ordering::Release);
    REBOUND.store(rebound, Ordering::Release);
}

/// The rebinding's say on a key: `Some(true)` the game must read it as held, `Some(false)` as
/// up, `None` (also for mouse buttons) its real state.
pub fn forced(id: InputId) -> Option<bool> {
    if !FORCED.load(Ordering::Acquire) {
        return None;
    }
    match TABLE.get(key_index(id)?)?.load(Ordering::Relaxed) {
        PRESSED => Some(true),
        RELEASED => Some(false),
        _ => None,
    }
}

/// [`forced`], for a query about `window` (GLFW's key state is per window).
pub fn forced_in(window: usize, id: InputId) -> Option<bool> {
    if !FORCED.load(Ordering::Acquire) || WINDOW.load(Ordering::Relaxed) != window {
        return None;
    }
    forced(id)
}

/// Whether the game reads `id` as held: the rebinding's say, else `physically()`.
pub fn held(id: InputId, physically: impl FnOnce() -> bool) -> bool {
    forced(id).unwrap_or_else(physically)
}

/// Whether any key's state is the rebinding's.
pub fn any_forced() -> bool {
    FORCED.load(Ordering::Acquire)
}

/// Whether a modifier key's state is the rebinding's (the modifier bits of key events must
/// follow it then).
pub fn modifiers_forced() -> bool {
    MODIFIERS_FORCED.load(Ordering::Acquire)
}

/// The rule (source, output) that holds one of `ids` down in the game, for telling the user
/// which key to let go of.
pub fn rule_holding(ids: &[InputId]) -> Option<(InputId, InputId)> {
    if !REBOUND.load(Ordering::Acquire) {
        return None;
    }
    input::router()
        .rebind_state()
        .held
        .into_iter()
        .find(|(_, output)| ids.contains(output))
}

/// The safety net for releases that never come (an IME took them, the window lost focus,
/// 26.x dropped them while loading a world, anything else). A source a rule holds whose key or
/// button Windows reports as up for [`LOST_RELEASE_DELAY`] is let go, as its release would. A
/// rule's output the game got pressed as it was ([`RebindState::passthrough`]) is forgotten
/// then, with nothing for the game: it let go of it by itself (26.x resets its key state when
/// the screen changes), and the press would swallow the rule's own presses. Called every frame
/// after the swap of `window`; only the window the table was written for is checked. Returns
/// the releases to give the game right away (the table is up to date by then).
pub fn lost_releases(window: usize) -> Vec<Output> {
    if !REBOUND.load(Ordering::Acquire) && !PASSED.load(Ordering::Acquire) {
        SUSPECTS.lock().unwrap_or_else(|e| e.into_inner()).clear();
        return Vec::new();
    }
    if WINDOW.load(Ordering::Relaxed) != window {
        return Vec::new();
    }
    let mut router = input::router();
    let state = router.rebind_state();
    let watched: Vec<InputId> = state
        .held
        .iter()
        .map(|&(source, _)| source)
        .chain(state.passthrough.iter().copied())
        .collect();
    let lost = {
        let mut suspects = SUSPECTS.lock().unwrap_or_else(|e| e.into_inner());
        let (lost, still) = check_suspects(&watched, &suspects, Instant::now(), is_up);
        *suspects = still;
        lost
    };
    if lost.is_empty() {
        return Vec::new();
    }
    let mut releases = Vec::new();
    for source in lost {
        if state.passthrough.contains(&source) {
            if router.forget_passthrough(source) {
                log::debug!(
                    "rebinds: {} is up, but its release never came; forgetting its press",
                    input_name(source)
                );
            }
            continue;
        }
        let release = router.release_source(source);
        match release {
            Some(output) => log::debug!(
                "rebinds: {} is up, but its release never came; releasing {}",
                input_name(source),
                input_name(output.id)
            ),
            None => log::debug!(
                "rebinds: {} is up, but its release never came; its output stays held by another key",
                input_name(source)
            ),
        }
        releases.extend(release);
    }
    apply(window, &router.rebind_state());
    releases
}

/// Of the `watched` ids (held rules' sources, rule outputs pressed as they were), those up (by
/// `up`) since [`LOST_RELEASE_DELAY`] (lost), and those up for less (the suspects to keep, with
/// when they were first seen up). An id seen down, or not watched any more, is no suspect.
fn check_suspects(
    watched: &[InputId],
    suspects: &[(InputId, Instant)],
    now: Instant,
    up: impl Fn(InputId) -> bool,
) -> (Vec<InputId>, Vec<(InputId, Instant)>) {
    let (mut lost, mut still) = (Vec::new(), Vec::new());
    for &source in watched {
        if !up(source) {
            continue;
        }
        let since = suspects
            .iter()
            .find(|&&(id, _)| id == source)
            .map_or(now, |&(_, at)| at);
        if now.saturating_duration_since(since) >= LOST_RELEASE_DELAY {
            lost.push(source);
        } else {
            still.push((source, since));
        }
    }
    (lost, still)
}

/// Whether Windows reports the key or mouse button as up now; false when it cannot tell.
fn is_up(id: InputId) -> bool {
    let keys = virtual_keys(id);
    !keys.is_empty()
        && keys.iter().all(|&vk| {
            // SAFETY: plain query; the high bit is set while the key is down.
            let state = unsafe { GetAsyncKeyState(i32::from(vk)) };
            state >= 0
        })
}

/// The virtual keys that report `id` held (any of them down: held). Empty when it cannot be
/// told: keys without a Win32 scancode.
fn virtual_keys(id: InputId) -> Vec<u16> {
    match id {
        InputId::Mouse(button) => button_virtual_key(button, buttons_swapped())
            .into_iter()
            .collect(),
        // GLFW 3.4 reports Pause as 0x45, which is Num Lock's scancode to Windows.
        InputId::Key(72) => vec![VK_PAUSE],
        InputId::Key(83) => vec![VK_NUMLOCK],
        // The keypad's digits and point are the navigation keys while Num Lock is off.
        InputId::Key(sc @ 89..=97) => [VK_NUMPAD1 + (sc - 89)]
            .into_iter()
            .chain(scancode_virtual_key(id))
            .collect(),
        InputId::Key(98) => [VK_NUMPAD0]
            .into_iter()
            .chain(scancode_virtual_key(id))
            .collect(),
        InputId::Key(99) => [VK_DECIMAL]
            .into_iter()
            .chain(scancode_virtual_key(id))
            .collect(),
        InputId::Key(_) => scancode_virtual_key(id).into_iter().collect(),
    }
}

/// The virtual key of a mouse button (SDL's numbering, which follows the buttons' logical
/// roles). GetAsyncKeyState reads the physical buttons: with the buttons `swapped`, the
/// logical left button is the physical right one.
fn button_virtual_key(button: u8, swapped: bool) -> Option<u16> {
    match (button, swapped) {
        (1, false) | (3, true) => Some(VK_LBUTTON),
        (1, true) | (3, false) => Some(VK_RBUTTON),
        (2, _) => Some(VK_MBUTTON),
        (4, _) => Some(VK_XBUTTON1),
        (5, _) => Some(VK_XBUTTON2),
        _ => None,
    }
}

/// Whether the user swapped the left and right mouse buttons in Windows' settings.
fn buttons_swapped() -> bool {
    // SAFETY: plain query.
    unsafe { GetSystemMetrics(SM_SWAPBUTTON) != 0 }
}

/// The virtual key Windows' keyboard layout gives the key's scancode (the left and right
/// modifiers apart).
fn scancode_virtual_key(id: InputId) -> Option<u16> {
    let scancode = u32::from(win_scancode_of(id)?);
    // GLFW's 0x100 for the 0xE0 prefix.
    let code = if scancode & 0x100 != 0 {
        0xE000 | (scancode & 0xFF)
    } else {
        scancode
    };
    // SAFETY: plain query.
    let vk = unsafe { MapVirtualKeyW(code, MAPVK_VSC_TO_VK_EX) };
    u16::try_from(vk).ok().filter(|&vk| vk != 0)
}

#[cfg(test)]
mod tests {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        VK_F3, VK_LSHIFT, VK_NUMPAD8, VK_RCONTROL, VK_RMENU,
    };

    use super::*;

    const A: InputId = InputId::Key(4);
    const B: InputId = InputId::Key(5);
    const F3: InputId = InputId::Key(60);
    const MOUSE4: InputId = InputId::Mouse(4);

    #[test]
    fn table_takes_sources_and_holds_outputs() {
        let state = RebindState {
            held: vec![(MOUSE4, F3), (A, InputId::Mouse(1))],
            taken: vec![MOUSE4, A],
            passthrough: vec![InputId::Key(6)],
        };
        let table = table_of(&state);
        assert_eq!(table[60], PRESSED);
        assert_eq!(table[4], RELEASED);
        // Mouse buttons are not in it (Mouse(4) is not the key with scancode 4).
        assert_eq!(table.iter().filter(|&&v| v != REAL).count(), 2);
        assert_eq!(table_of(&RebindState::default()), [REAL; SCANCODE_COUNT]);
    }

    #[test]
    fn a_held_output_wins_over_a_taken_source() {
        // A and B swapped, both held: the game has both down.
        let state = RebindState {
            held: vec![(A, B), (B, A)],
            taken: vec![A, B],
            ..RebindState::default()
        };
        let table = table_of(&state);
        assert_eq!((table[4], table[5]), (PRESSED, PRESSED));
        // Only B held (as A): B reads as up, A as held.
        let state = RebindState {
            held: vec![(B, A)],
            taken: vec![B],
            ..RebindState::default()
        };
        let table = table_of(&state);
        assert_eq!((table[4], table[5]), (PRESSED, RELEASED));
    }

    /// The only test that writes the shared table.
    #[test]
    fn the_shared_table() {
        const WINDOW_A: usize = 0x1000;
        const LCTRL: InputId = InputId::Key(224);
        let state = RebindState {
            held: vec![(MOUSE4, F3), (A, LCTRL)],
            taken: vec![MOUSE4, A],
            ..RebindState::default()
        };
        apply(WINDOW_A, &state);
        assert!(any_forced() && modifiers_forced());
        assert_eq!(forced(F3), Some(true));
        assert_eq!(forced(LCTRL), Some(true));
        assert_eq!(forced(A), Some(false));
        assert_eq!((forced(B), forced(MOUSE4)), (None, None));
        assert_eq!(forced_in(WINDOW_A, F3), Some(true));
        assert_eq!(forced_in(WINDOW_A + 1, F3), None, "another window");
        assert!(held(F3, || false));
        assert!(!held(A, || true));
        assert!(held(B, || true) && !held(B, || false));
        // Mouse-only rules force no key, but are known to be held.
        let state = RebindState {
            held: vec![(MOUSE4, InputId::Mouse(1))],
            taken: vec![MOUSE4],
            ..RebindState::default()
        };
        apply(WINDOW_A, &state);
        assert!(!any_forced() && !modifiers_forced());
        assert!(REBOUND.load(Ordering::Acquire));
        assert_eq!(forced(F3), None);
        // A rule's output pressed as it was: watched by the safety net, nothing forced.
        let state = RebindState {
            passthrough: vec![InputId::Mouse(1)],
            ..RebindState::default()
        };
        apply(WINDOW_A + 1, &state);
        assert!(PASSED.load(Ordering::Acquire) && !REBOUND.load(Ordering::Acquire));
        assert!(!any_forced());
        assert_eq!(WINDOW.load(Ordering::Relaxed), WINDOW_A + 1);
        apply(WINDOW_A, &RebindState::default());
        assert!(!REBOUND.load(Ordering::Acquire) && !PASSED.load(Ordering::Acquire));
        assert!(TABLE.iter().all(|e| e.load(Ordering::Relaxed) == REAL));
        assert!(lost_releases(WINDOW_A).is_empty());
    }

    #[test]
    fn ids_out_of_range_are_left_out() {
        assert_eq!(key_index(InputId::Key(511)), Some(511));
        assert_eq!(key_index(InputId::Key(512)), None);
        assert_eq!(key_index(InputId::Key(u16::MAX)), None);
        let state = RebindState {
            held: vec![(A, InputId::Key(600))],
            taken: vec![InputId::Key(512)],
            ..RebindState::default()
        };
        assert_eq!(table_of(&state), [REAL; SCANCODE_COUNT]);
    }

    #[test]
    fn a_release_counts_as_lost_once_it_is_late() {
        let start = Instant::now();
        // A held rule's source, and a rule's output pressed as it was.
        let held = [MOUSE4, A];
        let up = |id: InputId| id == MOUSE4;
        // First seen up: a suspect.
        let (lost, suspects) = check_suspects(&held, &[], start, up);
        assert!(lost.is_empty());
        assert_eq!(suspects, [(MOUSE4, start)]);
        // Still up, not for long enough.
        let soon = start + LOST_RELEASE_DELAY / 2;
        let (lost, suspects) = check_suspects(&held, &suspects, soon, up);
        assert!(lost.is_empty());
        assert_eq!(suspects, [(MOUSE4, start)]);
        let late = start + LOST_RELEASE_DELAY;
        let (lost, suspects) = check_suspects(&held, &suspects, late, up);
        assert_eq!(lost, [MOUSE4]);
        assert!(suspects.is_empty());
    }

    #[test]
    fn a_key_seen_down_again_is_no_suspect() {
        let start = Instant::now();
        let held = [MOUSE4];
        let (_, suspects) = check_suspects(&held, &[], start, |_| true);
        let later = start + LOST_RELEASE_DELAY;
        let (lost, suspects) = check_suspects(&held, &suspects, later, |_| false);
        assert!(lost.is_empty() && suspects.is_empty());
        // Up again: the wait starts over.
        let (lost, suspects) = check_suspects(&held, &suspects, later, |_| true);
        assert!(lost.is_empty());
        assert_eq!(suspects, [(MOUSE4, later)]);
        // Released meanwhile (not held any more): forgotten.
        let (lost, suspects) = check_suspects(&[], &suspects, later + LOST_RELEASE_DELAY, |_| true);
        assert!(lost.is_empty() && suspects.is_empty());
    }

    #[test]
    fn virtual_keys_of_keys_and_buttons() {
        // Keys whose virtual key does not depend on the keyboard layout.
        assert_eq!(virtual_keys(InputId::Key(225)), [VK_LSHIFT]);
        assert_eq!(virtual_keys(InputId::Key(228)), [VK_RCONTROL]);
        assert_eq!(virtual_keys(InputId::Key(230)), [VK_RMENU]);
        assert_eq!(virtual_keys(F3), [VK_F3]);
        assert_eq!(virtual_keys(InputId::Key(72)), [VK_PAUSE]);
        assert_eq!(virtual_keys(InputId::Key(83)), [VK_NUMLOCK]);
        // Keypad 8: also ↑ while Num Lock is off.
        assert_eq!(virtual_keys(InputId::Key(96))[0], VK_NUMPAD8);
        assert_eq!(virtual_keys(InputId::Key(96)).len(), 2);
        assert_eq!(virtual_keys(InputId::Mouse(2)), [VK_MBUTTON]);
        assert_eq!(virtual_keys(MOUSE4), [VK_XBUTTON1]);
        assert_eq!(virtual_keys(InputId::Mouse(5)), [VK_XBUTTON2]);
        // The left and right buttons by their roles: Windows reads the physical ones.
        assert_eq!(button_virtual_key(1, false), Some(VK_LBUTTON));
        assert_eq!(button_virtual_key(3, false), Some(VK_RBUTTON));
        assert_eq!(button_virtual_key(1, true), Some(VK_RBUTTON));
        assert_eq!(button_virtual_key(3, true), Some(VK_LBUTTON));
        assert_eq!(button_virtual_key(2, true), Some(VK_MBUTTON));
        assert_eq!(button_virtual_key(6, false), None);
        assert_eq!(virtual_keys(InputId::Mouse(1)).len(), 1);
        // Cannot tell: never reported as up.
        assert!(virtual_keys(InputId::Mouse(0)).is_empty());
        assert!(!is_up(InputId::Mouse(0)));
        // SDL-only keys have no Win32 scancode.
        assert!(virtual_keys(InputId::Key(300)).is_empty());
    }
}
