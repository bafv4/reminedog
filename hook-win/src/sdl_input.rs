//! SDL3 input (Minecraft 26.x): events are filtered in `SDL_PollEvent`.
//!
//! SDL3 queues input and the game drains the queue with `SDL_PollEvent`; the detour hands
//! each event to the router and drops the ones the overlay takes, so the game never sees
//! them. The game might read input some other way (event watchers, `SDL_PeepEvents`,
//! keyboard-state polling); those functions are only logged the first time they are
//! used, so a log shows whether the filter covers the game.

use std::ffi::{CStr, c_char, c_int, c_void};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use reminedog_render::{Key, Modifiers, PointerButton, Route};
use windows_sys::Win32::Foundation::HMODULE;

use crate::ffi;
use crate::hook::{self, export};
use crate::input::{self, router};

type PollEventFn = unsafe extern "C" fn(event: *mut u8) -> bool;
type GetWindowFromIdFn = unsafe extern "C" fn(id: u32) -> *mut c_void;
type WindowBoolFn = unsafe extern "C" fn(window: *mut c_void) -> bool;
type GetWindowPixelDensityFn = unsafe extern "C" fn(window: *mut c_void) -> f32;

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
const OFF_WHEEL_X: usize = 24;
const OFF_WHEEL_Y: usize = 28;
const OFF_WHEEL_DIRECTION: usize = 32;

const KMOD_SHIFT: u16 = 0x0003;
const KMOD_CTRL: u16 = 0x00C0;
const KMOD_ALT: u16 = 0x0300;

struct Api {
    get_window_from_id: GetWindowFromIdFn,
    get_relative_mouse_mode: WindowBoolFn,
    get_pixel_density: GetWindowPixelDensityFn,
    start_text_input: WindowBoolFn,
    stop_text_input: WindowBoolFn,
    text_input_active: WindowBoolFn,
}

static API: OnceLock<Api> = OnceLock::new();
static POLL_EVENT: OnceLock<PollEventFn> = OnceLock::new();
/// The window of the last input event, for switching text input when the UI toggles.
static LAST_WINDOW: AtomicUsize = AtomicUsize::new(0);
/// We switched SDL text input on for the UI and must switch it off again.
static TEXT_INPUT_OURS: AtomicBool = AtomicBool::new(false);

static PEEP_EVENTS: OnceLock<PeepEventsFn> = OnceLock::new();
static WAIT_EVENT: OnceLock<WaitEventFn> = OnceLock::new();
static WAIT_EVENT_TIMEOUT: OnceLock<WaitEventTimeoutFn> = OnceLock::new();
static SET_RELATIVE_MOUSE_MODE: OnceLock<SetRelativeMouseModeFn> = OnceLock::new();
static GET_KEYBOARD_STATE: OnceLock<GetKeyboardStateFn> = OnceLock::new();
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
            })
        }
    })();
    let Some(api) = api else {
        log::warn!("SDL3 lacks functions the overlay's input needs; no overlay input");
        return;
    };
    let _ = API.set(api);

    macro_rules! hook {
        ($name:literal, $detour:ident, $original:ident) => {
            match export(module, $name) {
                // SAFETY: the export, its detour and the slot share one signature; nothing
                // runs SDL3 while it is being loaded.
                Some(target) => {
                    let name = $name.to_string_lossy();
                    if let Err(e) = unsafe {
                        hook::install(&name, target, $detour as *const c_void, &$original)
                    } {
                        log::warn!("SDL3 input: {e}");
                    }
                }
                None => log::warn!("SDL3 does not export {}", $name.to_string_lossy()),
            }
        };
    }
    hook!(c"SDL_PollEvent", poll_event_detour, POLL_EVENT);
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
    hook!(
        c"SDL_GetKeyboardState",
        get_keyboard_state_probe,
        GET_KEYBOARD_STATE
    );
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
}

unsafe extern "C" fn poll_event_detour(event: *mut u8) -> bool {
    let Some(original) = POLL_EVENT.get() else {
        return false;
    };
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
            return true;
        }
    }
}

/// # Safety
/// `event` must point to a complete SDL_Event.
unsafe fn route_event(event: *const u8) -> Route {
    let Some(api) = API.get() else {
        return Route::Forward;
    };
    // SAFETY: every field read lies inside the 128-byte SDL_Event.
    unsafe {
        let read_u32 = |off: usize| event.add(off).cast::<u32>().read_unaligned();
        let read_f32 = |off: usize| event.add(off).cast::<f32>().read_unaligned();
        let kind = read_u32(0);
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
                router().key(egui_key(key), down, repeat, modifiers(mods), captured)
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
                let button = match *event.add(OFF_BUTTON) {
                    1 => PointerButton::Primary,
                    2 => PointerButton::Middle,
                    3 => PointerButton::Secondary,
                    4 => PointerButton::Extra1,
                    5 => PointerButton::Extra2,
                    _ => return Route::Forward,
                };
                router().button(button, *event.add(OFF_BUTTON_DOWN) != 0)
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
                router().focus_lost();
                Route::Forward
            }
            _ => Route::Forward,
        }
    }
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

/// egui's name for an SDL3 keycode (the keys a text field or the hotkeys need).
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
probe!(get_keyboard_state_probe, GET_KEYBOARD_STATE, "SDL_GetKeyboardState",
    (count: *mut c_int) -> *const bool, std::ptr::null());
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
