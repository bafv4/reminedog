//! GLFW input: the game's callbacks are wrapped so the overlay sees every event first.
//!
//! The `glfwSet*Callback` functions are detoured. GLFW gets our wrapper instead of the
//! game's callback, and the game's pointer is kept here. Each setter hands back the
//! game's previous pointer, never the wrapper: LWJGL frees whatever the setters return
//! (`Callbacks.glfwFreeCallbacks` at shutdown), so returning a wrapper would crash on exit.
//! Wrappers never call game code while holding a lock.

use std::collections::HashMap;
use std::ffi::{c_int, c_uint, c_void};
use std::sync::{Mutex, OnceLock};

use reminedog_render::{Key, PointerButton, Route};
use windows_sys::Win32::Foundation::HMODULE;

use crate::ffi;
use crate::hook::{self, export};
use crate::input::{self, router};

type KeyFn = unsafe extern "C" fn(*mut c_void, c_int, c_int, c_int, c_int);
type CharFn = unsafe extern "C" fn(*mut c_void, c_uint);
type CharModsFn = unsafe extern "C" fn(*mut c_void, c_uint, c_int);
type MouseButtonFn = unsafe extern "C" fn(*mut c_void, c_int, c_int, c_int);
type CursorPosFn = unsafe extern "C" fn(*mut c_void, f64, f64);
type ScrollFn = unsafe extern "C" fn(*mut c_void, f64, f64);
type FocusFn = unsafe extern "C" fn(*mut c_void, c_int);
type GetInputModeFn = unsafe extern "C" fn(*mut c_void, c_int) -> c_int;

const PRESS: c_int = 1;
const REPEAT: c_int = 2;
const MOD_SHIFT: c_int = 0x1;
const MOD_CONTROL: c_int = 0x2;
const MOD_ALT: c_int = 0x4;
const CURSOR: c_int = 0x0003_3001;
const CURSOR_DISABLED: c_int = 0x0003_4003;

/// The game's callbacks of one window.
#[derive(Default, Clone, Copy)]
struct GameCallbacks {
    key: Option<KeyFn>,
    char: Option<CharFn>,
    char_mods: Option<CharModsFn>,
    mouse_button: Option<MouseButtonFn>,
    cursor_pos: Option<CursorPosFn>,
    scroll: Option<ScrollFn>,
    focus: Option<FocusFn>,
}

// SAFETY: plain function pointers.
unsafe impl Send for GameCallbacks {}

static GAME: Mutex<Option<HashMap<usize, GameCallbacks>>> = Mutex::new(None);
static GET_INPUT_MODE: OnceLock<GetInputModeFn> = OnceLock::new();

fn game_callbacks(window: *mut c_void) -> GameCallbacks {
    let game = GAME.lock().unwrap_or_else(|e| e.into_inner());
    game.as_ref()
        .and_then(|map| map.get(&(window as usize)).copied())
        .unwrap_or_default()
}

/// Swaps in the game's new callback and returns its previous one.
fn replace_game_callback<T>(
    window: *mut c_void,
    field: impl FnOnce(&mut GameCallbacks) -> &mut Option<T>,
    callback: Option<T>,
) -> Option<T> {
    let mut game = GAME.lock().unwrap_or_else(|e| e.into_inner());
    let entry = game
        .get_or_insert_with(HashMap::new)
        .entry(window as usize)
        .or_default();
    std::mem::replace(field(entry), callback)
}

/// Whether the game has grabbed the cursor (it is being played, not showing a menu).
fn captured(window: *mut c_void) -> bool {
    GET_INPUT_MODE.get().is_some_and(|get| {
        // SAFETY: a window GLFW passed us, on its event thread.
        unsafe { get(window, CURSOR) == CURSOR_DISABLED }
    })
}

/// Defines the detour of one `glfwSet*Callback` function.
macro_rules! setter {
    ($detour:ident, $original:ident, $field:ident, $fn:ty, $wrapper:ident) => {
        static $original: OnceLock<unsafe extern "C" fn(*mut c_void, Option<$fn>) -> Option<$fn>> =
            OnceLock::new();

        unsafe extern "C" fn $detour(window: *mut c_void, callback: Option<$fn>) -> Option<$fn> {
            let original = $original.get()?;
            let previous = ffi::catch(stringify!($detour), || {
                replace_game_callback(window, |c| &mut c.$field, callback)
            })
            .flatten();
            let ours: Option<$fn> = callback.map(|_| $wrapper as $fn);
            // SAFETY: same window; our wrapper has the callback's signature.
            unsafe { original(window, ours) };
            previous
        }
    };
}

setter!(set_key_detour, SET_KEY, key, KeyFn, key_wrapper);
setter!(set_char_detour, SET_CHAR, char, CharFn, char_wrapper);
setter!(
    set_char_mods_detour,
    SET_CHAR_MODS,
    char_mods,
    CharModsFn,
    char_mods_wrapper
);
setter!(
    set_mouse_button_detour,
    SET_MOUSE_BUTTON,
    mouse_button,
    MouseButtonFn,
    mouse_button_wrapper
);
setter!(
    set_cursor_pos_detour,
    SET_CURSOR_POS,
    cursor_pos,
    CursorPosFn,
    cursor_pos_wrapper
);
setter!(
    set_scroll_detour,
    SET_SCROLL,
    scroll,
    ScrollFn,
    scroll_wrapper
);
setter!(set_focus_detour, SET_FOCUS, focus, FocusFn, focus_wrapper);

/// Detours the callback setters. Failures only cost the overlay its input.
pub fn install(module: HMODULE) {
    if let Some(f) = export(module, c"glfwGetInputMode") {
        // SAFETY: glfwGetInputMode has this signature.
        let _ =
            GET_INPUT_MODE.set(unsafe { std::mem::transmute::<*const c_void, GetInputModeFn>(f) });
    }
    macro_rules! hook {
        ($name:literal, $detour:ident, $original:ident) => {
            match export(module, $name) {
                // SAFETY: the setter and its detour share the slot's signature; nothing
                // runs GLFW while it is being loaded.
                Some(target) => {
                    let name = $name.to_string_lossy();
                    if let Err(e) = unsafe {
                        hook::install(&name, target, $detour as *const c_void, &$original)
                    } {
                        log::warn!("no overlay input: {e}");
                    }
                }
                None => log::warn!("GLFW does not export {}", $name.to_string_lossy()),
            }
        };
    }
    hook!(c"glfwSetKeyCallback", set_key_detour, SET_KEY);
    hook!(c"glfwSetCharCallback", set_char_detour, SET_CHAR);
    hook!(
        c"glfwSetCharModsCallback",
        set_char_mods_detour,
        SET_CHAR_MODS
    );
    hook!(
        c"glfwSetMouseButtonCallback",
        set_mouse_button_detour,
        SET_MOUSE_BUTTON
    );
    hook!(
        c"glfwSetCursorPosCallback",
        set_cursor_pos_detour,
        SET_CURSOR_POS
    );
    hook!(c"glfwSetScrollCallback", set_scroll_detour, SET_SCROLL);
    hook!(c"glfwSetWindowFocusCallback", set_focus_detour, SET_FOCUS);
}

fn modifiers(mods: c_int) -> reminedog_render::Modifiers {
    input::modifiers(
        mods & MOD_SHIFT != 0,
        mods & MOD_CONTROL != 0,
        mods & MOD_ALT != 0,
    )
}

unsafe extern "C" fn key_wrapper(
    window: *mut c_void,
    key: c_int,
    scancode: c_int,
    action: c_int,
    mods: c_int,
) {
    let route = ffi::catch("GLFW key", || {
        let pressed = action == PRESS || action == REPEAT;
        router().key(
            egui_key(key),
            pressed,
            action == REPEAT,
            modifiers(mods),
            captured(window),
        )
    });
    if route != Some(Route::Consume)
        && let Some(game) = game_callbacks(window).key
    {
        // SAFETY: forwarding GLFW's arguments to the game's callback.
        unsafe { game(window, key, scancode, action, mods) };
    }
}

fn text_route(codepoint: c_uint) -> Option<Route> {
    ffi::catch("GLFW char", || {
        let text = char::from_u32(codepoint)
            .map(String::from)
            .unwrap_or_default();
        router().text(&text)
    })
}

unsafe extern "C" fn char_wrapper(window: *mut c_void, codepoint: c_uint) {
    if text_route(codepoint) != Some(Route::Consume)
        && let Some(game) = game_callbacks(window).char
    {
        // SAFETY: as above.
        unsafe { game(window, codepoint) };
    }
}

unsafe extern "C" fn char_mods_wrapper(window: *mut c_void, codepoint: c_uint, mods: c_int) {
    if text_route(codepoint) != Some(Route::Consume)
        && let Some(game) = game_callbacks(window).char_mods
    {
        // SAFETY: as above.
        unsafe { game(window, codepoint, mods) };
    }
}

unsafe extern "C" fn mouse_button_wrapper(
    window: *mut c_void,
    button: c_int,
    action: c_int,
    mods: c_int,
) {
    let route = ffi::catch("GLFW mouse button", || {
        let button = match button {
            0 => PointerButton::Primary,
            1 => PointerButton::Secondary,
            2 => PointerButton::Middle,
            3 => PointerButton::Extra1,
            4 => PointerButton::Extra2,
            _ => return Route::Forward,
        };
        router().button(button, action == PRESS)
    });
    if route != Some(Route::Consume)
        && let Some(game) = game_callbacks(window).mouse_button
    {
        // SAFETY: as above.
        unsafe { game(window, button, action, mods) };
    }
}

unsafe extern "C" fn cursor_pos_wrapper(window: *mut c_void, x: f64, y: f64) {
    let forward = ffi::catch("GLFW cursor", || {
        router().cursor_position(x, y, captured(window))
    })
    .unwrap_or(Some((x, y)));
    if let Some((x, y)) = forward
        && let Some(game) = game_callbacks(window).cursor_pos
    {
        // SAFETY: as above (with the position corrected for motion the overlay took).
        unsafe { game(window, x, y) };
    }
}

unsafe extern "C" fn scroll_wrapper(window: *mut c_void, dx: f64, dy: f64) {
    let route = ffi::catch("GLFW scroll", || router().scroll(dx as f32, dy as f32));
    if route != Some(Route::Consume)
        && let Some(game) = game_callbacks(window).scroll
    {
        // SAFETY: as above.
        unsafe { game(window, dx, dy) };
    }
}

unsafe extern "C" fn focus_wrapper(window: *mut c_void, focused: c_int) {
    if focused == 0 {
        ffi::catch("GLFW focus", || router().focus_lost());
    }
    if let Some(game) = game_callbacks(window).focus {
        // SAFETY: as above.
        unsafe { game(window, focused) };
    }
}

/// egui's name for a GLFW key code (the keys a text field or the hotkeys need).
fn egui_key(key: c_int) -> Option<Key> {
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
        65..=90 => LETTERS[(key - 65) as usize],
        48..=57 => DIGITS[(key - 48) as usize],
        32 => Key::Space,
        256 => Key::Escape,
        257 | 335 => Key::Enter, // Enter, keypad Enter
        258 => Key::Tab,
        259 => Key::Backspace,
        260 => Key::Insert,
        261 => Key::Delete,
        262 => Key::ArrowRight,
        263 => Key::ArrowLeft,
        264 => Key::ArrowDown,
        265 => Key::ArrowUp,
        266 => Key::PageUp,
        267 => Key::PageDown,
        268 => Key::Home,
        269 => Key::End,
        _ => return None,
    })
}
