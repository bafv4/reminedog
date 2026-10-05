//! SDL3 input (Minecraft 26.x): events are filtered in `SDL_PollEvent`.
//!
//! SDL3 queues input and the game drains the queue with `SDL_PollEvent`; the detour hands
//! each event to the router and drops the ones the overlay takes, so the game never sees
//! them. The game might read input some other way (event watchers, `SDL_PeepEvents`); those
//! functions are only logged the first time they are used, so a log shows whether the filter
//! covers the game.
//!
//! The key rebinding rewrites the caller's event in place into the router's output, key or
//! mouse button alike; `SDL_GetKeyboardState` and the modifier bits of key events report what
//! the game got ([`rebind_state`]).
//!
//! F3+C for waypoints ([`f3c`]) is made-up key events handed out before SDL's own; the
//! game handles each one before it polls again, so a `SDL_SetClipboardText` between two
//! polls belongs to the event handed out in between.

use std::collections::VecDeque;
use std::ffi::{CStr, c_char, c_int, c_void};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

use reminedog_core::{InputId, SCANCODE_COUNT, sdl_keycode, sdl_keycode_of, sdl_scancode};
use reminedog_render::{Delivery, Key, Modifiers, Output, Phase, PointerButton, Route};
use windows_sys::Win32::Foundation::HMODULE;

use crate::f3c::{self, Failure, Injected, Job, Purpose};
use crate::ffi;
use crate::hook::{self, export};
use crate::input::{self, FUNCTION_KEYS, router};
use crate::rebind_state;

type PollEventFn = unsafe extern "C" fn(event: *mut u8) -> bool;
type GetWindowFromIdFn = unsafe extern "C" fn(id: u32) -> *mut c_void;
type WindowBoolFn = unsafe extern "C" fn(window: *mut c_void) -> bool;
type GetWindowPixelDensityFn = unsafe extern "C" fn(window: *mut c_void) -> f32;
type GetWindowIdFn = unsafe extern "C" fn(window: *mut c_void) -> u32;
type SetClipboardTextFn = unsafe extern "C" fn(text: *const c_char) -> bool;
type GetModStateFn = unsafe extern "C" fn() -> u16;

// Probes.
type PeepEventsFn = unsafe extern "C" fn(*mut u8, c_int, c_int, u32, u32) -> c_int;
type WaitEventTimeoutFn = unsafe extern "C" fn(*mut u8, i32) -> bool;
type WaitEventFn = unsafe extern "C" fn(*mut u8) -> bool;
type SetRelativeMouseModeFn = unsafe extern "C" fn(*mut c_void, bool) -> bool;
type GetKeyboardStateFn = unsafe extern "C" fn(*mut c_int) -> *const bool;
type GetMouseStateFn = unsafe extern "C" fn(*mut f32, *mut f32) -> u32;
type AddEventWatchFn = unsafe extern "C" fn(*const c_void, *mut c_void) -> bool;
type SetEventFilterFn = unsafe extern "C" fn(*const c_void, *mut c_void);

// Event types and SDL_Event field offsets (SDL 3.4, checked against LWJGL 3.4.3's layouts).
const EVENT_WINDOW_RESIZED: u32 = 0x206;
const EVENT_WINDOW_PIXEL_SIZE_CHANGED: u32 = 0x207;
const EVENT_WINDOW_FOCUS_LOST: u32 = 0x20F;
const EVENT_KEY_DOWN: u32 = 0x300;
const EVENT_KEY_UP: u32 = 0x301;
const EVENT_TEXT_EDITING: u32 = 0x302;
const EVENT_TEXT_INPUT: u32 = 0x303;
const EVENT_MOUSE_MOTION: u32 = 0x400;
const EVENT_MOUSE_BUTTON_DOWN: u32 = 0x401;
const EVENT_MOUSE_BUTTON_UP: u32 = 0x402;
const EVENT_MOUSE_WHEEL: u32 = 0x403;
const OFF_WINDOW_ID: usize = 16;
const OFF_WINDOW_DATA1: usize = 20;
const OFF_WINDOW_DATA2: usize = 24;
/// The keyboard or mouse of key and button events.
const OFF_WHICH: usize = 20;
/// sizeof(SDL_Event).
const EVENT_SIZE: usize = 128;
const OFF_KEY_SCANCODE: usize = 24;
const OFF_KEY: usize = 28;
const OFF_KEY_MOD: usize = 32;
const OFF_KEY_DOWN: usize = 36;
const OFF_KEY_REPEAT: usize = 37;
const OFF_TEXT: usize = 24;
const OFF_MOTION_X: usize = 28;
const OFF_MOTION_Y: usize = 32;
const OFF_MOTION_XREL: usize = 36;
const OFF_MOTION_YREL: usize = 40;
const OFF_BUTTON: usize = 24;
const OFF_BUTTON_DOWN: usize = 25;
const OFF_BUTTON_CLICKS: usize = 26;
const OFF_WHEEL_X: usize = 24;
const OFF_WHEEL_Y: usize = 28;
const OFF_WHEEL_DIRECTION: usize = 32;

const KMOD_SHIFT: u16 = 0x0003;
const KMOD_CTRL: u16 = 0x00C0;
const KMOD_ALT: u16 = 0x0300;
/// The modifier keys' scancodes and bits (SDL3 has one bit per key).
const MODIFIER_BITS: [(u16, u16); 8] = [
    (224, 0x0040), // left Ctrl
    (225, 0x0001), // left Shift
    (226, 0x0100), // left Alt
    (227, 0x0400), // left GUI
    (228, 0x0080), // right Ctrl
    (229, 0x0002), // right Shift
    (230, 0x0200), // right Alt
    (231, 0x0800), // right GUI
];

const SCANCODE_LCTRL: u32 = 224;
const SCANCODE_RCTRL: u32 = 228;

type Event = [u8; EVENT_SIZE];

/// What handing out a made-up event means for F3+C.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tag {
    Plain,
    /// The copy key's press: the game copies the location while handling it.
    CopyDown(Purpose),
    /// The modifier's release: whether the game copied is known by then.
    ModifierUp(Purpose),
}

struct Api {
    get_window_from_id: GetWindowFromIdFn,
    get_relative_mouse_mode: WindowBoolFn,
    get_pixel_density: GetWindowPixelDensityFn,
    start_text_input: WindowBoolFn,
    stop_text_input: WindowBoolFn,
    text_input_active: WindowBoolFn,
    get_window_id: Option<GetWindowIdFn>,
    /// The real modifier state, for mouse buttons rebound to keys (not detoured: Minecraft reads
    /// a click's modifiers with it, which stay physical).
    get_mod_state: Option<GetModStateFn>,
}

static API: OnceLock<Api> = OnceLock::new();
static POLL_EVENT: OnceLock<PollEventFn> = OnceLock::new();
/// The window of the last input event, for switching text input when the UI toggles.
static LAST_WINDOW: AtomicUsize = AtomicUsize::new(0);
/// We switched SDL text input on for the UI and must switch it off again.
static TEXT_INPUT_OURS: AtomicBool = AtomicBool::new(false);
/// Events made up by the agent (the zoom's resizes, F3+C), handed out before SDL's own.
static INJECTED: Mutex<VecDeque<(Event, Tag)>> = Mutex::new(VecDeque::new());
static SET_CLIPBOARD_TEXT: OnceLock<SetClipboardTextFn> = OnceLock::new();

static PEEP_EVENTS: OnceLock<PeepEventsFn> = OnceLock::new();
static WAIT_EVENT: OnceLock<WaitEventFn> = OnceLock::new();
static WAIT_EVENT_TIMEOUT: OnceLock<WaitEventTimeoutFn> = OnceLock::new();
static SET_RELATIVE_MOUSE_MODE: OnceLock<SetRelativeMouseModeFn> = OnceLock::new();
/// Trampoline of SDL_GetKeyboardState; also called directly for the real key state (F3+C, the
/// rebinding), which made-up events never change.
static GET_KEYBOARD_STATE: OnceLock<GetKeyboardStateFn> = OnceLock::new();
/// SDL_GetKeyboardState itself, for the real key state if it could not be detoured.
static GET_KEYBOARD_STATE_EXPORT: OnceLock<GetKeyboardStateFn> = OnceLock::new();
/// SDL_GetKeyboardState is detoured: the game reads the key state the rebinding gives it.
static KEY_STATE_SPOOFED: AtomicBool = AtomicBool::new(false);
/// What SDL_GetKeyboardState returns while the rebinding has a say on some key: SDL's array with
/// that applied (a C bool per scancode, as SDL's).
static SPOOFED_KEYS: [AtomicU8; SCANCODE_COUNT] = [const { AtomicU8::new(0) }; SCANCODE_COUNT];
static GET_MOUSE_STATE: OnceLock<GetMouseStateFn> = OnceLock::new();
static GET_RELATIVE_MOUSE_STATE: OnceLock<GetMouseStateFn> = OnceLock::new();
static ADD_EVENT_WATCH: OnceLock<AddEventWatchFn> = OnceLock::new();
static SET_EVENT_FILTER: OnceLock<SetEventFilterFn> = OnceLock::new();

pub fn install(module: HMODULE) {
    let api = (|| {
        let get = |name: &CStr| export(module, name);
        // SAFETY: SDL3 exports with these C signatures.
        unsafe {
            Some(Api {
                get_window_from_id: std::mem::transmute::<*const c_void, GetWindowFromIdFn>(get(
                    c"SDL_GetWindowFromID",
                )?),
                get_relative_mouse_mode: std::mem::transmute::<*const c_void, WindowBoolFn>(get(
                    c"SDL_GetWindowRelativeMouseMode",
                )?),
                get_pixel_density: std::mem::transmute::<*const c_void, GetWindowPixelDensityFn>(
                    get(c"SDL_GetWindowPixelDensity")?,
                ),
                start_text_input: std::mem::transmute::<*const c_void, WindowBoolFn>(get(
                    c"SDL_StartTextInput",
                )?),
                stop_text_input: std::mem::transmute::<*const c_void, WindowBoolFn>(get(
                    c"SDL_StopTextInput",
                )?),
                text_input_active: std::mem::transmute::<*const c_void, WindowBoolFn>(get(
                    c"SDL_TextInputActive",
                )?),
                get_window_id: get(c"SDL_GetWindowID")
                    .map(|f| std::mem::transmute::<*const c_void, GetWindowIdFn>(f)),
                get_mod_state: get(c"SDL_GetModState")
                    .map(|f| std::mem::transmute::<*const c_void, GetModStateFn>(f)),
            })
        }
    })();
    let Some(api) = api else {
        log::warn!("SDL3 lacks functions the overlay's input needs; no overlay input");
        f3c::set_hooks_ready(false);
        return;
    };
    let has_window_id = api.get_window_id.is_some();
    let _ = API.set(api);

    // True when the detour is in.
    macro_rules! hook {
        ($name:literal, $detour:ident, $original:ident) => {
            match export(module, $name) {
                // SAFETY: the export, its detour and the slot share one signature; nothing
                // runs SDL3 while it is being loaded.
                Some(target) => {
                    let name = $name.to_string_lossy();
                    match unsafe {
                        hook::install(&name, target, $detour as *const c_void, &$original)
                    } {
                        Ok(()) => true,
                        Err(e) => {
                            log::warn!("SDL3 input: {e}");
                            false
                        }
                    }
                }
                None => {
                    log::warn!("SDL3 does not export {}", $name.to_string_lossy());
                    false
                }
            }
        };
    }
    let poll_hooked = hook!(c"SDL_PollEvent", poll_event_detour, POLL_EVENT);
    let clipboard_hooked = hook!(
        c"SDL_SetClipboardText",
        set_clipboard_text_detour,
        SET_CLIPBOARD_TEXT
    );
    if let Some(f) = export(module, c"SDL_GetKeyboardState") {
        // SAFETY: SDL_GetKeyboardState has this signature.
        let _ = GET_KEYBOARD_STATE_EXPORT
            .set(unsafe { std::mem::transmute::<*const c_void, GetKeyboardStateFn>(f) });
    }
    hook!(c"SDL_PeepEvents", peep_events_probe, PEEP_EVENTS);
    hook!(c"SDL_WaitEvent", wait_event_probe, WAIT_EVENT);
    hook!(
        c"SDL_WaitEventTimeout",
        wait_event_timeout_probe,
        WAIT_EVENT_TIMEOUT
    );
    hook!(
        c"SDL_SetWindowRelativeMouseMode",
        set_relative_mouse_mode_probe,
        SET_RELATIVE_MOUSE_MODE
    );
    let keyboard_state_hooked = hook!(
        c"SDL_GetKeyboardState",
        get_keyboard_state_detour,
        GET_KEYBOARD_STATE
    );
    KEY_STATE_SPOOFED.store(keyboard_state_hooked, Ordering::Relaxed);
    hook!(c"SDL_GetMouseState", get_mouse_state_probe, GET_MOUSE_STATE);
    hook!(
        c"SDL_GetRelativeMouseState",
        get_relative_mouse_state_probe,
        GET_RELATIVE_MOUSE_STATE
    );
    hook!(c"SDL_AddEventWatch", add_event_watch_probe, ADD_EVENT_WATCH);
    hook!(
        c"SDL_SetEventFilter",
        set_event_filter_probe,
        SET_EVENT_FILTER
    );
    // F3+C needs the game window's id for its key events and the real key state.
    if !has_window_id {
        log::warn!("SDL3 does not export SDL_GetWindowID");
    }
    f3c::set_hooks_ready(
        poll_hooked && clipboard_hooked && has_window_id && real_keyboard_state().is_some(),
    );
}

unsafe extern "C" fn poll_event_detour(event: *mut u8) -> bool {
    let Some(original) = POLL_EVENT.get() else {
        return false;
    };
    ffi::catch("SDL resize for the zoom", queue_zoom_resize);
    ffi::catch("SDL F3+C", start_f3c);
    {
        let mut injected = INJECTED.lock().unwrap_or_else(|e| e.into_inner());
        if !injected.is_empty() {
            if !event.is_null() {
                let (made_up, tag) = injected.pop_front().expect("checked");
                if tag != Tag::Plain {
                    ffi::catch("SDL F3+C", || on_injected(&mut injected, &made_up, tag));
                }
                // SAFETY: the caller's buffer holds a whole SDL_Event.
                unsafe { std::ptr::copy_nonoverlapping(made_up.as_ptr(), event, EVENT_SIZE) };
            }
            return true;
        }
    }
    loop {
        // SAFETY: the caller's event buffer (or null to only peek).
        let has_event = unsafe { original(event) };
        if !has_event || event.is_null() {
            return has_event;
        }
        // SAFETY: SDL just filled a whole SDL_Event.
        let route = ffi::catch("SDL_PollEvent detour", || unsafe { route_event(event) });
        if log::log_enabled!(log::Level::Trace) {
            // SAFETY: as above.
            let kind = unsafe { event.cast::<u32>().read_unaligned() };
            log::trace!("SDL event 0x{kind:X} -> {route:?}");
        }
        ffi::catch("SDL text input", update_text_input);
        if route != Some(Route::Consume) {
            // SAFETY: as above.
            ffi::catch("SDL F3+C", || unsafe { mark_passive(event) });
            return true;
        }
    }
}

/// The user's own F3+C: when their copy key's press goes to the game, a location it copies
/// before the next poll is theirs.
///
/// # Safety
/// `event` must point to a complete SDL_Event.
unsafe fn mark_passive(event: *const u8) {
    // SAFETY: fields inside the 128-byte SDL_Event.
    let (kind, repeat, scancode) = unsafe {
        (
            event.cast::<u32>().read_unaligned(),
            *event.add(OFF_KEY_REPEAT) != 0,
            event.add(OFF_KEY_SCANCODE).cast::<u32>().read_unaligned(),
        )
    };
    if kind == EVENT_KEY_DOWN && !repeat && f3c::passive_key_sdl(scancode) {
        f3c::begin_passive();
    }
}

/// At every SDL_PollEvent: the game handled the previous event, which ends the user's own
/// F3+C; a waypoint request becomes key events handed out from now on.
fn start_f3c() {
    f3c::end_passive();
    // Before INJECTED is locked (lock order).
    let Some(job) = f3c::take_job(None) else {
        return;
    };
    match f3c_events(&job) {
        Ok(events) => {
            log::debug!("SDL3: sending F3+C to the game");
            INJECTED
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .extend(events);
        }
        Err(reason) => f3c::fail(job.purpose, reason),
    }
}

/// F3+C for `job` as SDL key events, if it can be sent now.
fn f3c_events(job: &Job) -> Result<[(Event, Tag); 4], Failure> {
    let codes = f3c::key_codes(&job.keys, |name| sdl_scancode(name).zip(sdl_keycode(name)))?;
    let window = job.window as *mut c_void;
    if !captured(window) {
        return Err(Failure::NotPlaying);
    }
    // A held crash key would arm Minecraft's debug crash once the modifier is down, the real
    // release of a held modifier would come after ours (and toggle the debug overlay), and,
    // where the copy key also drops items, Ctrl would make a refused C drop a whole stack
    // (only then: Ctrl is often held to sprint). Held as the game reads them: a key rebound
    // to one of them counts, one rebound to another key does not.
    let ctrl = job.keys.copy_drops_items();
    let watched = [
        Some(codes.modifier.0),
        Some(codes.copy.0),
        codes.crash.map(|(scancode, _)| scancode),
        ctrl.then_some(SCANCODE_LCTRL),
        ctrl.then_some(SCANCODE_RCTRL),
    ];
    let real = real_keys().ok_or(Failure::NoHooks)?;
    let held: Vec<u32> = watched
        .into_iter()
        .flatten()
        .filter(|&scancode| {
            let physically = || real.get(scancode as usize).is_some_and(|&down| down != 0);
            match input_key(scancode) {
                Some(id) => rebind_state::held(id, physically),
                None => physically(),
            }
        })
        .collect();
    if !held.is_empty() {
        let ids: Vec<InputId> = held.into_iter().filter_map(input_key).collect();
        return Err(Failure::KeysHeld(rebind_state::rule_holding(&ids)));
    }
    let id = API
        .get()
        .and_then(|api| api.get_window_id)
        // SAFETY: the window SDL passed to SDL_GL_SwapWindow, on its thread.
        .map_or(0, |get| unsafe { get(window) });
    if id == 0 {
        return Err(Failure::NoCallback);
    }
    Ok(burst_events(id, codes.modifier, codes.copy, job.purpose))
}

fn real_keyboard_state() -> Option<GetKeyboardStateFn> {
    GET_KEYBOARD_STATE
        .get()
        .or(GET_KEYBOARD_STATE_EXPORT.get())
        .copied()
}

/// SDL's own key state, one C bool per scancode (what is physically held); `None` if SDL cannot
/// tell. Read it right away: SDL updates it as it pumps events.
fn real_keys() -> Option<&'static [u8]> {
    let get = real_keyboard_state()?;
    let mut count: c_int = 0;
    // SAFETY: SDL writes the array's length and returns its own array. Called through the
    // trampoline, the rebinding's detour does not see it.
    let state = unsafe { get(&mut count) };
    if state.is_null() {
        return None;
    }
    let count = usize::try_from(count).unwrap_or(0);
    // SAFETY: SDL's array of `count` one-byte bools, which lives as long as SDL.
    Some(unsafe { std::slice::from_raw_parts(state.cast::<u8>(), count) })
}

/// Whether the game reads the key state the rebinding gives it (without that, keys cannot be
/// rebound: a held source would read as held).
pub fn key_state_spoofed() -> bool {
    KEY_STATE_SPOOFED.load(Ordering::Relaxed)
}

/// The game's view of the key state: SDL's array, with the rebinding's say on the keys it
/// holds down or hides ([`rebind_state`]). Minecraft asks for the array afresh every time it
/// reads a key, so a copy patched now is what it reads.
unsafe extern "C" fn get_keyboard_state_detour(numkeys: *mut c_int) -> *const bool {
    static USED: AtomicBool = AtomicBool::new(false);
    first_use(&USED, "SDL_GetKeyboardState");
    let Some(original) = GET_KEYBOARD_STATE.get() else {
        return std::ptr::null();
    };
    let mut count: c_int = 0;
    // SAFETY: SDL writes the length to our local and returns its own array.
    let state = unsafe { original(&mut count) };
    if !numkeys.is_null() {
        // SAFETY: the caller's pointer, which SDL allows to be null.
        unsafe { *numkeys = count };
    }
    if state.is_null() || !rebind_state::any_forced() {
        return state;
    }
    // A longer array than ours is left alone (the game must not read past what it is told).
    let Some(count) = usize::try_from(count)
        .ok()
        .filter(|&n| n > 0 && n <= SCANCODE_COUNT)
    else {
        return state;
    };
    // SAFETY: SDL's array of `count` one-byte bools.
    let real = unsafe { std::slice::from_raw_parts(state.cast::<u8>(), count) };
    spoof_keys(real, &SPOOFED_KEYS[..count], rebind_state::forced);
    SPOOFED_KEYS.as_ptr().cast()
}

/// Copies the key state `real` into `out`, with what `forced` says about each key.
fn spoof_keys(real: &[u8], out: &[AtomicU8], forced: impl Fn(InputId) -> Option<bool>) {
    for (scancode, (entry, &down)) in out.iter().zip(real).enumerate() {
        let down = match u16::try_from(scancode)
            .ok()
            .and_then(|sc| forced(InputId::Key(sc)))
        {
            Some(held) => u8::from(held),
            None => down,
        };
        entry.store(down, Ordering::Relaxed);
    }
}

/// Modifier down, copy down, copy up, modifier up, for the window with `window_id`. Keys are
/// (scancode, keycode); Minecraft matches its bindings on the scancode.
fn burst_events(
    window_id: u32,
    modifier: (u32, u32),
    copy: (u32, u32),
    purpose: Purpose,
) -> [(Event, Tag); 4] {
    [
        (key_event(true, window_id, modifier), Tag::Plain),
        (key_event(true, window_id, copy), Tag::CopyDown(purpose)),
        (key_event(false, window_id, copy), Tag::Plain),
        (
            key_event(false, window_id, modifier),
            Tag::ModifierUp(purpose),
        ),
    ]
}

/// A key event as SDL reports one, without modifiers (a held Ctrl must not turn it into
/// Ctrl+C), not repeated, from no particular keyboard (`which` 0) and with no timestamp
/// (Minecraft reads neither).
fn key_event(down: bool, window_id: u32, (scancode, keycode): (u32, u32)) -> Event {
    let mut e = [0u8; EVENT_SIZE];
    let kind = if down { EVENT_KEY_DOWN } else { EVENT_KEY_UP };
    e[..4].copy_from_slice(&kind.to_ne_bytes());
    e[OFF_WINDOW_ID..OFF_WINDOW_ID + 4].copy_from_slice(&window_id.to_ne_bytes());
    e[OFF_KEY_SCANCODE..OFF_KEY_SCANCODE + 4].copy_from_slice(&scancode.to_ne_bytes());
    e[OFF_KEY..OFF_KEY + 4].copy_from_slice(&keycode.to_ne_bytes());
    e[OFF_KEY_DOWN] = u8::from(down);
    e
}

/// Handing out a tagged event: the copy key's press opens the window in which the game's
/// clipboard write is ours; the modifier's release closes it. Runs with INJECTED locked.
fn on_injected(queue: &mut VecDeque<(Event, Tag)>, event: &Event, tag: Tag) {
    match tag {
        Tag::Plain => {}
        Tag::CopyDown(purpose) => f3c::begin_injected(purpose),
        Tag::ModifierUp(purpose) => {
            if let Some(reason) = after_modifier_up(queue, event, f3c::end_injected()) {
                f3c::fail(purpose, reason);
            }
        }
    }
}

/// What the game copied settles the burst as its modifier release (`up`) goes out. Refused,
/// that release toggles the debug overlay: the modifier is pressed and released once more
/// right after it, in the same poll loop, so no frame shows the toggle.
fn after_modifier_up(
    queue: &mut VecDeque<(Event, Tag)>,
    up: &Event,
    copied: Injected,
) -> Option<Failure> {
    match copied {
        Injected::Captured => None,
        // The game handled the keys; only its text was not a location.
        Injected::Written => Some(Failure::Unreadable),
        Injected::Nothing => {
            let mut down = *up;
            down[..4].copy_from_slice(&EVENT_KEY_DOWN.to_ne_bytes());
            down[OFF_KEY_DOWN] = 1;
            // In reverse, so they come out DOWN, then UP.
            queue.push_front((*up, Tag::Plain));
            queue.push_front((down, Tag::Plain));
            Some(Failure::Refused)
        }
    }
}

/// Keeps the location Minecraft copies for our F3+C off the clipboard (reporting success, or
/// the game logs an error), and sees the user's own F3+C.
unsafe extern "C" fn set_clipboard_text_detour(text: *const c_char) -> bool {
    let Some(original) = SET_CLIPBOARD_TEXT.get() else {
        return false;
    };
    let ours = !text.is_null()
        && ffi::catch("SDL_SetClipboardText detour", || {
            // SAFETY: NUL-terminated UTF-8, valid only during this call (LWJGL passes a stack
            // buffer); it is read here, before returning.
            f3c::on_clipboard(&unsafe { CStr::from_ptr(text) }.to_string_lossy())
        })
        .unwrap_or(false);
    if ours {
        return true;
    }
    // SAFETY: the caller's argument.
    unsafe { original(text) }
}

/// Hands the event to the router; the key rebinding rewrites it in place.
///
/// # Safety
/// `event` must point to a complete SDL_Event the caller lets us write.
unsafe fn route_event(event: *mut u8) -> Route {
    let Some(api) = API.get() else {
        return Route::Forward;
    };
    // SAFETY: every field read lies inside the 128-byte SDL_Event.
    unsafe {
        let read_u32 = |off: usize| event.add(off).cast::<u32>().read_unaligned();
        let read_f32 = |off: usize| event.add(off).cast::<f32>().read_unaligned();
        let kind = read_u32(0);
        if kind == EVENT_WINDOW_PIXEL_SIZE_CHANGED {
            let window = (api.get_window_from_id)(read_u32(OFF_WINDOW_ID));
            return if crate::tall::real_resize(window) {
                Route::Consume
            } else {
                Route::Forward
            };
        }
        if !matches!(
            kind,
            EVENT_KEY_DOWN
                | EVENT_KEY_UP
                | EVENT_TEXT_EDITING
                | EVENT_TEXT_INPUT
                | EVENT_MOUSE_MOTION
                | EVENT_MOUSE_BUTTON_DOWN
                | EVENT_MOUSE_BUTTON_UP
                | EVENT_MOUSE_WHEEL
                | EVENT_WINDOW_FOCUS_LOST
        ) {
            return Route::Forward;
        }
        let window = (api.get_window_from_id)(read_u32(OFF_WINDOW_ID));
        if !window.is_null() {
            LAST_WINDOW.store(window as usize, Ordering::Relaxed);
        }
        let captured = !window.is_null() && (api.get_relative_mouse_mode)(window);
        let density = if window.is_null() {
            1.0
        } else {
            let d = (api.get_pixel_density)(window);
            if d.is_finite() && d > 0.0 { d } else { 1.0 }
        };
        match kind {
            EVENT_KEY_DOWN | EVENT_KEY_UP => {
                let key = read_u32(OFF_KEY);
                let mods = event.add(OFF_KEY_MOD).cast::<u16>().read_unaligned();
                let down = *event.add(OFF_KEY_DOWN) != 0;
                let repeat = *event.add(OFF_KEY_REPEAT) != 0;
                let raw = input_key(read_u32(OFF_KEY_SCANCODE));
                let delivery = {
                    let mut router = router();
                    let delivery = router.key(
                        egui_key(key),
                        raw,
                        down,
                        repeat,
                        modifiers(mods),
                        captured,
                        false,
                    );
                    // Before the game gets the event (and may read the key state).
                    rebind_state::apply(window as usize, &router.rebind_state());
                    delivery
                };
                deliver(&mut *event.cast::<Event>(), delivery, || adjust_mod(mods))
            }
            EVENT_TEXT_INPUT => {
                let text = event.add(OFF_TEXT).cast::<*const c_char>().read_unaligned();
                if text.is_null() {
                    return Route::Forward;
                }
                router().text(&CStr::from_ptr(text).to_string_lossy())
            }
            // IME composition is not shown yet; keep it away from the game while typing.
            EVENT_TEXT_EDITING => {
                if router().ui_open() {
                    Route::Consume
                } else {
                    Route::Forward
                }
            }
            EVENT_MOUSE_MOTION => router().cursor_motion(
                read_f32(OFF_MOTION_XREL) * density,
                read_f32(OFF_MOTION_YREL) * density,
                read_f32(OFF_MOTION_X) * density,
                read_f32(OFF_MOTION_Y) * density,
                captured,
            ),
            EVENT_MOUSE_BUTTON_DOWN | EVENT_MOUSE_BUTTON_UP => {
                let number = *event.add(OFF_BUTTON);
                let button = match number {
                    1 => PointerButton::Primary,
                    2 => PointerButton::Middle,
                    3 => PointerButton::Secondary,
                    4 => PointerButton::Extra1,
                    5 => PointerButton::Extra2,
                    _ => return Route::Forward,
                };
                let down = *event.add(OFF_BUTTON_DOWN) != 0;
                let delivery = {
                    let mut router = router();
                    let delivery =
                        router.button(button, Some(InputId::Mouse(number)), down, captured);
                    rebind_state::apply(window as usize, &router.rebind_state());
                    delivery
                };
                // A key made of a click gets the modifiers held now, as the click would (SAFETY:
                // SDL_GetModState takes no arguments).
                let real_mods = || api.get_mod_state.map_or(0, |get| get());
                deliver(&mut *event.cast::<Event>(), delivery, || {
                    adjust_mod(real_mods())
                })
            }
            EVENT_MOUSE_WHEEL => {
                // Direction 1 = flipped (natural scrolling).
                let sign = if read_u32(OFF_WHEEL_DIRECTION) == 1 {
                    -1.0
                } else {
                    1.0
                };
                router().scroll(read_f32(OFF_WHEEL_X) * sign, read_f32(OFF_WHEEL_Y) * sign)
            }
            EVENT_WINDOW_FOCUS_LOST => {
                // SDL3 sends the releases of held keys after this event, not of buttons, and the
                // router drops those of rebound sources: their outputs are released first, at
                // the next poll.
                let releases = {
                    let mut router = router();
                    let releases = router.focus_lost(|id| matches!(id, InputId::Key(_)));
                    rebind_state::apply(window as usize, &router.rebind_state());
                    releases
                };
                let window_id = read_u32(OFF_WINDOW_ID);
                inject_first(
                    releases
                        .into_iter()
                        .filter_map(|output| output_event(window_id, output))
                        .collect(),
                );
                Route::Forward
            }
            _ => Route::Forward,
        }
    }
}

/// Acts on the router's decision about the key or button event the game is about to get:
/// `Send` rewrites it into the output. Key events get `key_mod()` as their modifier bits.
fn deliver(event: &mut Event, delivery: Delivery, key_mod: impl FnOnce() -> u16) -> Route {
    match delivery {
        Delivery::Consume => Route::Consume,
        Delivery::Forward => {
            if matches!(u32_at(event, 0), EVENT_KEY_DOWN | EVENT_KEY_UP) {
                put(event, OFF_KEY_MOD, &key_mod().to_ne_bytes());
            }
            Route::Forward
        }
        Delivery::Send(output) => {
            if rewrite(event, output, key_mod) {
                Route::Forward
            } else {
                Route::Consume
            }
        }
    }
}

/// Rewrites a key or mouse button event into `output` in place, keeping its window and
/// timestamp; false if the game must not get it (a mouse button never repeats: the game would
/// take a repeat for a press). A key gets `key_mod()`; a key made of a key keeps its keyboard
/// and raw code, a button made of a button its mouse and clicks.
fn rewrite(event: &mut Event, output: Output, key_mod: impl FnOnce() -> u16) -> bool {
    let from_key = matches!(u32_at(event, 0), EVENT_KEY_DOWN | EVENT_KEY_UP);
    let down = output.phase != Phase::Release;
    match output.id {
        InputId::Key(scancode) => {
            if !from_key {
                event[OFF_WHICH..].fill(0);
            }
            let kind = if down { EVENT_KEY_DOWN } else { EVENT_KEY_UP };
            put(event, 0, &kind.to_ne_bytes());
            put(event, OFF_KEY_SCANCODE, &u32::from(scancode).to_ne_bytes());
            put(event, OFF_KEY, &sdl_keycode_of(output.id).to_ne_bytes());
            put(event, OFF_KEY_MOD, &key_mod().to_ne_bytes());
            event[OFF_KEY_DOWN] = u8::from(down);
            event[OFF_KEY_REPEAT] = u8::from(output.phase == Phase::Repeat);
        }
        InputId::Mouse(button) => {
            if output.phase == Phase::Repeat {
                return false;
            }
            if from_key {
                event[OFF_WHICH..].fill(0);
                event[OFF_BUTTON_CLICKS] = 1;
            }
            let kind = if down {
                EVENT_MOUSE_BUTTON_DOWN
            } else {
                EVENT_MOUSE_BUTTON_UP
            };
            put(event, 0, &kind.to_ne_bytes());
            event[OFF_BUTTON] = button;
            event[OFF_BUTTON_DOWN] = u8::from(down);
        }
    }
    true
}

fn u32_at(event: &Event, offset: usize) -> u32 {
    let mut bytes = [0; 4];
    bytes.copy_from_slice(&event[offset..offset + 4]);
    u32::from_ne_bytes(bytes)
}

fn put(event: &mut Event, offset: usize, bytes: &[u8]) {
    event[offset..offset + bytes.len()].copy_from_slice(bytes);
}

/// The modifier bits of a key event for the game: while the rebinding has a say on a modifier
/// key, its bit follows the key state the game reads (a key rebound to Ctrl sets Ctrl's bit,
/// Ctrl rebound to F3 clears it, so F3+B does not turn into Ctrl+B).
fn adjust_mod(mods: u16) -> u16 {
    if !rebind_state::modifiers_forced() {
        return mods;
    }
    patch_mod(mods, rebind_state::forced)
}

/// `mods` with each modifier key's bit set or cleared as `forced` says.
fn patch_mod(mods: u16, forced: impl Fn(InputId) -> Option<bool>) -> u16 {
    MODIFIER_BITS.iter().fold(mods, |mods, &(scancode, bit)| {
        match forced(InputId::Key(scancode)) {
            Some(true) => mods | bit,
            Some(false) => mods & !bit,
            None => mods,
        }
    })
}

/// An event that gives the game `output` on its own (a release on focus loss or from the
/// safety net), for the window with `window_id`; `None` for a mouse button's repeat.
fn output_event(window_id: u32, output: Output) -> Option<Event> {
    let down = output.phase != Phase::Release;
    match output.id {
        InputId::Key(scancode) => {
            let keycode = sdl_keycode_of(output.id);
            let mut e = key_event(down, window_id, (u32::from(scancode), keycode));
            e[OFF_KEY_REPEAT] = u8::from(output.phase == Phase::Repeat);
            Some(e)
        }
        InputId::Mouse(_) if output.phase == Phase::Repeat => None,
        InputId::Mouse(button) => Some(button_event(down, window_id, button)),
    }
}

/// A mouse button event as SDL reports one, from no particular mouse (`which` 0), with no
/// timestamp or position (Minecraft reads none of them).
fn button_event(down: bool, window_id: u32, button: u8) -> Event {
    let mut e = [0u8; EVENT_SIZE];
    let kind = if down {
        EVENT_MOUSE_BUTTON_DOWN
    } else {
        EVENT_MOUSE_BUTTON_UP
    };
    put(&mut e, 0, &kind.to_ne_bytes());
    put(&mut e, OFF_WINDOW_ID, &window_id.to_ne_bytes());
    e[OFF_BUTTON] = button;
    e[OFF_BUTTON_DOWN] = u8::from(down);
    e[OFF_BUTTON_CLICKS] = 1;
    e
}

/// Hands out `events` before anything queued. Takes INJECTED: call without the router.
fn inject_first(events: Vec<Event>) {
    if events.is_empty() {
        return;
    }
    let mut injected = INJECTED.lock().unwrap_or_else(|e| e.into_inner());
    for event in events.into_iter().rev() {
        injected.push_front((event, Tag::Plain));
    }
}

/// The safety net's releases ([`rebind_state::lost_releases`]), after a swap of `window`: the
/// game gets them at its next poll.
pub fn release_lost_keys(window: *mut c_void) {
    let Some(get_id) = API.get().and_then(|api| api.get_window_id) else {
        return;
    };
    let releases = rebind_state::lost_releases(window as usize);
    if releases.is_empty() {
        return;
    }
    // SAFETY: the window SDL passed to SDL_GL_SwapWindow, on its thread.
    let window_id = unsafe { get_id(window) };
    let events = releases
        .into_iter()
        .filter_map(|output| output_event(window_id, output))
        .map(|event| (event, Tag::Plain));
    INJECTED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .extend(events);
}

/// The key of an SDL3 scancode; `None` outside the key ids (SDL3 has no key below 4).
fn input_key(scancode: u32) -> Option<InputId> {
    u16::try_from(scancode)
        .ok()
        .filter(|&sc| (4..SCANCODE_COUNT as u16).contains(&sc))
        .map(InputId::Key)
}

/// Tells the game the size the zoom wants, the way SDL reports a resize: the new size in
/// pixels, then the window size (the same on displays without scaling; Minecraft may read
/// either).
fn queue_zoom_resize() {
    let Some((window, [w, h])) = crate::tall::take_pending() else {
        return;
    };
    let Some(api) = API.get() else {
        return;
    };
    let Some(get_id) = api.get_window_id else {
        return;
    };
    // SAFETY: a window SDL passed to SDL_GL_SwapWindow.
    let id = unsafe { get_id(window) };
    let (window_w, window_h) = window_size_for(api, window, [w, h]);
    let event = |kind: u32, w: i32, h: i32| {
        let mut e = [0u8; EVENT_SIZE];
        e[..4].copy_from_slice(&kind.to_ne_bytes());
        e[OFF_WINDOW_ID..OFF_WINDOW_ID + 4].copy_from_slice(&id.to_ne_bytes());
        e[OFF_WINDOW_DATA1..OFF_WINDOW_DATA1 + 4].copy_from_slice(&w.to_ne_bytes());
        e[OFF_WINDOW_DATA2..OFF_WINDOW_DATA2 + 4].copy_from_slice(&h.to_ne_bytes());
        e
    };
    log::debug!("SDL3: telling the game the window is {w}x{h} pixels");
    let mut injected = INJECTED.lock().unwrap_or_else(|e| e.into_inner());
    injected.push_back((event(EVENT_WINDOW_PIXEL_SIZE_CHANGED, w, h), Tag::Plain));
    injected.push_back((event(EVENT_WINDOW_RESIZED, window_w, window_h), Tag::Plain));
}

/// The window size (in SDL's window coordinates) matching `pixels`, scaled like the real
/// window's size is to its pixel size.
fn window_size_for(api: &Api, window: *mut c_void, [w, h]: [i32; 2]) -> (i32, i32) {
    // SAFETY: as above.
    let density = unsafe { (api.get_pixel_density)(window) };
    let density = if density.is_finite() && density > 0.0 {
        density
    } else {
        1.0
    };
    (
        (w as f32 / density).round() as i32,
        (h as f32 / density).round() as i32,
    )
}

/// Whether the game has the mouse in relative mode on `window` (it is being played).
pub fn captured(window: *mut c_void) -> bool {
    API.get().is_some_and(|api| {
        // SAFETY: a window SDL passed us, on its thread.
        unsafe { (api.get_relative_mouse_mode)(window) }
    })
}

/// SDL3 only sends text (and runs the IME) while text input is on; switch it on for the
/// UI and back off afterwards if the game had it off.
fn update_text_input() {
    let Some(open) = router().take_ui_changed() else {
        return;
    };
    let (Some(api), window) = (
        API.get(),
        LAST_WINDOW.load(Ordering::Relaxed) as *mut c_void,
    ) else {
        return;
    };
    if window.is_null() {
        return;
    }
    // SAFETY: a window SDL reported for an event on this (the event) thread.
    unsafe {
        if open {
            if !(api.text_input_active)(window) && (api.start_text_input)(window) {
                TEXT_INPUT_OURS.store(true, Ordering::Relaxed);
            }
        } else if TEXT_INPUT_OURS.swap(false, Ordering::Relaxed) {
            (api.stop_text_input)(window);
        }
    }
}

fn modifiers(mods: u16) -> Modifiers {
    input::modifiers(
        mods & KMOD_SHIFT != 0,
        mods & KMOD_CTRL != 0,
        mods & KMOD_ALT != 0,
    )
}

/// egui's name for an SDL3 keycode (the keys a text field or a hotkey can use). Keycodes
/// follow the keyboard layout.
fn egui_key(key: u32) -> Option<Key> {
    const LETTERS: [Key; 26] = [
        Key::A,
        Key::B,
        Key::C,
        Key::D,
        Key::E,
        Key::F,
        Key::G,
        Key::H,
        Key::I,
        Key::J,
        Key::K,
        Key::L,
        Key::M,
        Key::N,
        Key::O,
        Key::P,
        Key::Q,
        Key::R,
        Key::S,
        Key::T,
        Key::U,
        Key::V,
        Key::W,
        Key::X,
        Key::Y,
        Key::Z,
    ];
    const DIGITS: [Key; 10] = [
        Key::Num0,
        Key::Num1,
        Key::Num2,
        Key::Num3,
        Key::Num4,
        Key::Num5,
        Key::Num6,
        Key::Num7,
        Key::Num8,
        Key::Num9,
    ];
    Some(match key {
        0x61..=0x7A => LETTERS[(key - 0x61) as usize],
        0x30..=0x39 => DIGITS[(key - 0x30) as usize],
        0x20 => Key::Space,
        0x1B => Key::Escape,
        0x0D | 0x4000_0058 => Key::Enter, // Return, keypad Enter
        0x09 => Key::Tab,
        0x08 => Key::Backspace,
        0x7F => Key::Delete,
        0x4000_0049 => Key::Insert,
        0x4000_004A => Key::Home,
        0x4000_004B => Key::PageUp,
        0x4000_004D => Key::End,
        0x4000_004E => Key::PageDown,
        0x4000_004F => Key::ArrowRight,
        0x4000_0050 => Key::ArrowLeft,
        0x4000_0051 => Key::ArrowDown,
        0x4000_0052 => Key::ArrowUp,
        0x4000_003A..=0x4000_0045 => FUNCTION_KEYS[(key - 0x4000_003A) as usize], // F1-F12
        0x4000_0068..=0x4000_0073 => FUNCTION_KEYS[(key - 0x4000_0068 + 12) as usize], // F13-F24
        0x27 => Key::Quote,
        0x2C => Key::Comma,
        0x2D | 0x4000_0056 => Key::Minus,  // keypad -
        0x2E | 0x4000_0063 => Key::Period, // keypad .
        0x2F | 0x4000_0054 => Key::Slash,  // keypad /
        0x3A => Key::Colon,
        0x3B => Key::Semicolon,
        0x3D => Key::Equals,
        0x5B => Key::OpenBracket,
        0x5C => Key::Backslash,
        0x5D => Key::CloseBracket,
        0x60 => Key::Backtick,
        0x4000_0057 => Key::Plus, // keypad +
        0x4000_0059..=0x4000_0061 => DIGITS[(key - 0x4000_0059 + 1) as usize], // keypad 1-9
        0x4000_0062 => Key::Num0, // keypad 0
        _ => return None,
    })
}

/// Logs the first use of an input function the filter above does not cover.
fn first_use(flag: &AtomicBool, name: &str) {
    if !flag.swap(true, Ordering::Relaxed) {
        log::info!("SDL3 input: the game uses {name}");
    }
}

macro_rules! probe {
    ($detour:ident, $original:ident, $name:literal, ($($arg:ident: $ty:ty),*) -> $ret:ty, $default:expr) => {
        unsafe extern "C" fn $detour($($arg: $ty),*) -> $ret {
            static USED: AtomicBool = AtomicBool::new(false);
            first_use(&USED, $name);
            match $original.get() {
                // SAFETY: the caller's arguments, unchanged.
                Some(original) => unsafe { original($($arg),*) },
                None => $default,
            }
        }
    };
}

probe!(peep_events_probe, PEEP_EVENTS, "SDL_PeepEvents",
    (events: *mut u8, count: c_int, action: c_int, min: u32, max: u32) -> c_int, -1);
probe!(wait_event_probe, WAIT_EVENT, "SDL_WaitEvent", (event: *mut u8) -> bool, false);
probe!(wait_event_timeout_probe, WAIT_EVENT_TIMEOUT, "SDL_WaitEventTimeout",
    (event: *mut u8, timeout: i32) -> bool, false);
probe!(get_mouse_state_probe, GET_MOUSE_STATE, "SDL_GetMouseState",
    (x: *mut f32, y: *mut f32) -> u32, 0);
probe!(get_relative_mouse_state_probe, GET_RELATIVE_MOUSE_STATE, "SDL_GetRelativeMouseState",
    (x: *mut f32, y: *mut f32) -> u32, 0);
probe!(add_event_watch_probe, ADD_EVENT_WATCH, "SDL_AddEventWatch",
    (filter: *const c_void, data: *mut c_void) -> bool, false);
probe!(set_event_filter_probe, SET_EVENT_FILTER, "SDL_SetEventFilter",
    (filter: *const c_void, data: *mut c_void) -> (), ());

/// Logs every change, since this is how the game grabs the mouse.
unsafe extern "C" fn set_relative_mouse_mode_probe(window: *mut c_void, enabled: bool) -> bool {
    static COUNT: AtomicUsize = AtomicUsize::new(0);
    if COUNT.fetch_add(1, Ordering::Relaxed) < 20 {
        log::info!("SDL3 input: SDL_SetWindowRelativeMouseMode({enabled})");
    }
    match SET_RELATIVE_MOUSE_MODE.get() {
        // SAFETY: the caller's arguments, unchanged.
        Some(original) => unsafe { original(window, enabled) },
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const F3: (u32, u32) = (60, 0x4000_003C);
    const C: (u32, u32) = (6, 0x63);
    const ID: u32 = 3;

    /// (type, scancode, down) of each event, for comparing sequences.
    fn keys(events: impl IntoIterator<Item = Event>) -> Vec<(u32, u32, u8)> {
        events
            .into_iter()
            .map(|e| (u32_at(&e, 0), u32_at(&e, OFF_KEY_SCANCODE), e[OFF_KEY_DOWN]))
            .collect()
    }

    #[test]
    fn key_event_layout() {
        let e = key_event(true, ID, F3);
        assert_eq!(u32_at(&e, 0), EVENT_KEY_DOWN);
        assert_eq!(u32_at(&e, OFF_WINDOW_ID), ID);
        assert_eq!(u32_at(&e, OFF_KEY_SCANCODE), 60);
        assert_eq!(u32_at(&e, OFF_KEY), 0x4000_003C);
        assert_eq!(e[OFF_KEY_DOWN], 1);
        // No timestamp, keyboard, modifiers or raw code, and not a repeat.
        assert!(e[4..OFF_WINDOW_ID].iter().all(|&b| b == 0));
        assert_eq!(u32_at(&e, 20), 0);
        assert_eq!(e[OFF_KEY_MOD..OFF_KEY_DOWN], [0; 4]);
        assert_eq!(e[OFF_KEY_REPEAT], 0);
        assert!(e[OFF_KEY_REPEAT + 1..].iter().all(|&b| b == 0));

        let e = key_event(false, ID, C);
        assert_eq!(u32_at(&e, 0), EVENT_KEY_UP);
        assert_eq!(u32_at(&e, OFF_KEY_SCANCODE), 6);
        assert_eq!(u32_at(&e, OFF_KEY), 0x63);
        assert_eq!(e[OFF_KEY_DOWN], 0);
    }

    #[test]
    fn burst_order_and_tags() {
        let burst = burst_events(ID, F3, C, Purpose::Record);
        assert_eq!(
            keys(burst.map(|(e, _)| e)),
            [
                (EVENT_KEY_DOWN, 60, 1),
                (EVENT_KEY_DOWN, 6, 1),
                (EVENT_KEY_UP, 6, 0),
                (EVENT_KEY_UP, 60, 0),
            ]
        );
        assert_eq!(
            burst.map(|(_, tag)| tag),
            [
                Tag::Plain,
                Tag::CopyDown(Purpose::Record),
                Tag::Plain,
                Tag::ModifierUp(Purpose::Record),
            ]
        );
        assert!(burst.iter().all(|(e, _)| u32_at(e, OFF_WINDOW_ID) == ID));
    }

    /// Hands out the queue as the poll detour does, settling the burst with `copied`.
    fn hand_out(mut queue: VecDeque<(Event, Tag)>, copied: Injected) -> (Vec<Event>, Vec<Failure>) {
        let (mut out, mut failures) = (Vec::new(), Vec::new());
        while let Some((event, tag)) = queue.pop_front() {
            if let Tag::ModifierUp(_) = tag {
                failures.extend(after_modifier_up(&mut queue, &event, copied));
            }
            out.push(event);
        }
        (out, failures)
    }

    #[test]
    fn refusal_is_undone_right_after_the_modifier_release() {
        let resize = resize_event();
        let mut queue: VecDeque<_> = burst_events(ID, F3, C, Purpose::Refresh).into();
        queue.push_back((resize, Tag::Plain));
        let (out, failures) = hand_out(queue, Injected::Nothing);
        assert_eq!(failures, [Failure::Refused]);
        assert_eq!(
            keys(out),
            [
                (EVENT_KEY_DOWN, 60, 1),
                (EVENT_KEY_DOWN, 6, 1),
                (EVENT_KEY_UP, 6, 0),
                (EVENT_KEY_UP, 60, 0),
                // The undo: DOWN before UP, both before anything queued later.
                (EVENT_KEY_DOWN, 60, 1),
                (EVENT_KEY_UP, 60, 0),
                (EVENT_WINDOW_PIXEL_SIZE_CHANGED, 0, 0),
            ]
        );
    }

    #[test]
    fn a_copy_needs_no_undo() {
        for (copied, failures) in [
            (Injected::Captured, vec![]),
            (Injected::Written, vec![Failure::Unreadable]),
        ] {
            let queue: VecDeque<_> = burst_events(ID, F3, C, Purpose::Record).into();
            let (out, got) = hand_out(queue, copied);
            assert_eq!(got, failures);
            assert_eq!(out.len(), 4);
        }
    }

    fn resize_event() -> Event {
        let mut e = [0u8; EVENT_SIZE];
        e[..4].copy_from_slice(&EVENT_WINDOW_PIXEL_SIZE_CHANGED.to_ne_bytes());
        e
    }

    const W: InputId = InputId::Key(26);
    const LCTRL: InputId = InputId::Key(224);
    const MOUSE4: InputId = InputId::Mouse(4);
    const TIMESTAMP: u64 = 0x1234_5678_9ABC;

    fn send(id: InputId, phase: Phase) -> Output {
        Output { id, phase }
    }

    /// A key event as SDL fills one: with a timestamp, keyboard, modifiers and raw code.
    fn physical_key(down: bool, repeat: bool, (scancode, keycode): (u32, u32)) -> Event {
        let mut e = key_event(down, ID, (scancode, keycode));
        put(&mut e, 8, &TIMESTAMP.to_ne_bytes());
        put(&mut e, OFF_WHICH, &7u32.to_ne_bytes());
        put(&mut e, OFF_KEY_MOD, &0x1000u16.to_ne_bytes()); // Num Lock
        put(&mut e, 34, &0x2Eu16.to_ne_bytes()); // raw
        e[OFF_KEY_REPEAT] = u8::from(repeat);
        e
    }

    /// A button event as SDL fills one: with a timestamp, mouse, clicks and position.
    fn physical_button(down: bool, button: u8) -> Event {
        let mut e = button_event(down, ID, button);
        put(&mut e, 8, &TIMESTAMP.to_ne_bytes());
        put(&mut e, OFF_WHICH, &9u32.to_ne_bytes());
        e[OFF_BUTTON_CLICKS] = 2;
        put(&mut e, 28, &100.5f32.to_ne_bytes());
        e
    }

    fn u16_at(e: &Event, off: usize) -> u16 {
        u16::from_ne_bytes([e[off], e[off + 1]])
    }

    #[test]
    fn a_key_rebound_to_a_key() {
        let mut e = physical_key(true, false, C);
        assert!(rewrite(&mut e, send(W, Phase::Press), || 0x1040));
        assert_eq!(u32_at(&e, 0), EVENT_KEY_DOWN);
        assert_eq!(u32_at(&e, OFF_KEY_SCANCODE), 26);
        assert_eq!(u32_at(&e, OFF_KEY), u32::from(b'w'));
        assert_eq!(u16_at(&e, OFF_KEY_MOD), 0x1040);
        assert_eq!((e[OFF_KEY_DOWN], e[OFF_KEY_REPEAT]), (1, 0));
        // The rest stays: window, timestamp, keyboard, raw code.
        assert_eq!(u32_at(&e, OFF_WINDOW_ID), ID);
        assert_eq!(e[8..16], TIMESTAMP.to_ne_bytes());
        assert_eq!(u32_at(&e, OFF_WHICH), 7);
        assert_eq!(u16_at(&e, 34), 0x2E);

        let mut e = physical_key(true, true, C);
        assert!(rewrite(&mut e, send(W, Phase::Repeat), || 0));
        assert_eq!((e[OFF_KEY_DOWN], e[OFF_KEY_REPEAT]), (1, 1));
        let mut e = physical_key(false, false, C);
        assert!(rewrite(&mut e, send(W, Phase::Release), || 0));
        assert_eq!(u32_at(&e, 0), EVENT_KEY_UP);
        assert_eq!((e[OFF_KEY_DOWN], e[OFF_KEY_REPEAT]), (0, 0));
    }

    #[test]
    fn a_key_rebound_to_a_button() {
        let mut e = physical_key(true, false, C);
        assert!(rewrite(
            &mut e,
            send(MOUSE4, Phase::Press),
            || unreachable!()
        ));
        let mut expected = button_event(true, ID, 4);
        put(&mut expected, 8, &TIMESTAMP.to_ne_bytes());
        assert_eq!(e, expected);
        // A button never repeats: the repeat is dropped.
        let mut e = physical_key(true, true, C);
        assert!(!rewrite(&mut e, send(MOUSE4, Phase::Repeat), || 0));
        let mut e = physical_key(false, false, C);
        assert!(rewrite(&mut e, send(MOUSE4, Phase::Release), || 0));
        assert_eq!(u32_at(&e, 0), EVENT_MOUSE_BUTTON_UP);
        assert_eq!(
            (e[OFF_BUTTON], e[OFF_BUTTON_DOWN], e[OFF_BUTTON_CLICKS]),
            (4, 0, 1)
        );
    }

    #[test]
    fn a_button_rebound_to_a_key() {
        let mut e = physical_button(true, 5);
        assert!(rewrite(
            &mut e,
            send(InputId::Key(60), Phase::Press),
            || 0x40
        ));
        let mut expected = key_event(true, ID, F3);
        put(&mut expected, 8, &TIMESTAMP.to_ne_bytes());
        put(&mut expected, OFF_KEY_MOD, &0x40u16.to_ne_bytes());
        assert_eq!(e, expected);
        let mut e = physical_button(false, 5);
        assert!(rewrite(
            &mut e,
            send(InputId::Key(60), Phase::Release),
            || 0
        ));
        assert_eq!(u32_at(&e, 0), EVENT_KEY_UP);
        assert_eq!(e[OFF_KEY_DOWN], 0);
    }

    #[test]
    fn a_button_rebound_to_a_button() {
        let mut e = physical_button(true, 5);
        assert!(rewrite(
            &mut e,
            send(InputId::Mouse(1), Phase::Press),
            || unreachable!()
        ));
        let mut expected = physical_button(true, 1);
        expected[OFF_BUTTON] = 1;
        assert_eq!(e, expected);
    }

    #[test]
    fn delivery_of_key_events() {
        // Forwarded keys get the modifiers as the game reads them; buttons are left alone.
        let mut e = physical_key(true, false, C);
        assert_eq!(
            deliver(&mut e, Delivery::Forward, || 0x1001),
            Route::Forward
        );
        assert_eq!(u16_at(&e, OFF_KEY_MOD), 0x1001);
        let before = physical_button(true, 4);
        let mut e = before;
        assert_eq!(
            deliver(&mut e, Delivery::Forward, || 0x1001),
            Route::Forward
        );
        assert_eq!(e, before);
        assert_eq!(deliver(&mut e, Delivery::Consume, || 0), Route::Consume);
        assert_eq!(e, before);
        let repeat = Delivery::Send(send(MOUSE4, Phase::Repeat));
        assert_eq!(
            deliver(&mut physical_key(true, true, C), repeat, || 0),
            Route::Consume
        );
    }

    #[test]
    fn modifier_bits_follow_the_rebinding() {
        let forced = |id: InputId| match id {
            InputId::Key(224) => Some(true),  // left Ctrl held by a rule
            InputId::Key(225) => Some(false), // left Shift taken by a rule
            _ => None,
        };
        // Right Shift and Num Lock stay as they are.
        assert_eq!(patch_mod(0x1003, forced), 0x1042);
        assert_eq!(patch_mod(0, forced), 0x40);
        assert_eq!(patch_mod(0x1003, |_| None), 0x1003);
    }

    #[test]
    fn events_for_releases() {
        let e = output_event(ID, send(LCTRL, Phase::Release)).unwrap();
        assert_eq!(e, key_event(false, ID, (224, 0x4000_00E0)));
        let e = output_event(ID, send(MOUSE4, Phase::Release)).unwrap();
        assert_eq!(e, button_event(false, ID, 4));
        assert_eq!(u32_at(&e, 0), EVENT_MOUSE_BUTTON_UP);
        assert_eq!(output_event(ID, send(MOUSE4, Phase::Repeat)), None);
    }

    #[test]
    fn the_game_reads_the_spoofed_key_state() {
        let mut real = [0u8; 300];
        real[26] = 1; // W, physically held and taken
        real[6] = 1; // C, physically held
        let out: Vec<AtomicU8> = (0..300).map(|_| AtomicU8::new(9)).collect();
        let forced = |id: InputId| match id {
            InputId::Key(26) => Some(false),
            InputId::Key(60) => Some(true),
            _ => None,
        };
        spoof_keys(&real, &out, forced);
        let got: Vec<u8> = out.iter().map(|b| b.load(Ordering::Relaxed)).collect();
        let mut expected = real;
        expected[26] = 0;
        expected[60] = 1;
        assert_eq!(got, expected);
    }
}
