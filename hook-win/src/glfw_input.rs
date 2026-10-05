//! GLFW input: the game's callbacks are wrapped so the overlay sees every event first.
//!
//! The `glfwSet*Callback` functions are detoured. GLFW gets our wrapper instead of the
//! game's callback, and the game's pointer is kept here. Each setter hands back the
//! game's previous pointer, never the wrapper: LWJGL frees whatever the setters return
//! (`Callbacks.glfwFreeCallbacks` at shutdown), so returning a wrapper would crash on exit.
//! Wrappers never call game code while holding a lock.
//!
//! The key rebinding gives the game's callbacks the router's output in place of the event, key
//! or mouse button alike; `glfwGetKey` and the modifier bits of key events report what the
//! game got ([`rebind_state`]).
//!
//! F3+C for waypoints ([`f3c`]) goes straight to the game's key callback after a swap
//! ([`run_f3c`]); `glfwSetClipboardString` is detoured to catch the location, and
//! `glfwGetKey` to report the modifier as held meanwhile (Minecraft 1.16 reads F3 that way).

use std::collections::HashMap;
use std::ffi::{CStr, c_char, c_int, c_uint, c_void};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

use reminedog_core::{
    DebugKeys, InputId, glfw_button_of, glfw_key, glfw_key_of, input_from_glfw_button,
    input_from_glfw_key,
};
use reminedog_render::{Delivery, Key, Output, Phase, PointerButton, Route};
use windows_sys::Win32::Foundation::HMODULE;

use crate::f3c::{self, Failure, Injected, Purpose};
use crate::ffi;
use crate::hook::{self, export};
use crate::input::{self, FUNCTION_KEYS, router};
use crate::rebind_state;

type KeyFn = unsafe extern "C" fn(*mut c_void, c_int, c_int, c_int, c_int);
type CharFn = unsafe extern "C" fn(*mut c_void, c_uint);
type CharModsFn = unsafe extern "C" fn(*mut c_void, c_uint, c_int);
type MouseButtonFn = unsafe extern "C" fn(*mut c_void, c_int, c_int, c_int);
type CursorPosFn = unsafe extern "C" fn(*mut c_void, f64, f64);
type ScrollFn = unsafe extern "C" fn(*mut c_void, f64, f64);
type FocusFn = unsafe extern "C" fn(*mut c_void, c_int);
type FramebufferSizeFn = unsafe extern "C" fn(*mut c_void, c_int, c_int);
type GetInputModeFn = unsafe extern "C" fn(*mut c_void, c_int) -> c_int;
type SetClipboardStringFn = unsafe extern "C" fn(*mut c_void, *const c_char);
type GetKeyFn = unsafe extern "C" fn(*mut c_void, c_int) -> c_int;
type GetKeyScancodeFn = unsafe extern "C" fn(c_int) -> c_int;

const RELEASE: c_int = 0;
const PRESS: c_int = 1;
const REPEAT: c_int = 2;
const KEY_LEFT_CONTROL: c_int = 341;
const KEY_RIGHT_CONTROL: c_int = 345;
const MOD_SHIFT: c_int = 0x1;
const MOD_CONTROL: c_int = 0x2;
const MOD_ALT: c_int = 0x4;
const MOD_SUPER: c_int = 0x8;
/// The modifier bits the rebinding may change; Caps Lock's and Num Lock's stay.
const MOD_KEYS: c_int = MOD_SHIFT | MOD_CONTROL | MOD_ALT | MOD_SUPER;
/// The modifier keys and their bits: the left Shift, Control, Alt and Super, then the right
/// ones. A bit is set while either of its keys is held.
const MODIFIER_KEYS: [(c_int, c_int); 8] = [
    (340, MOD_SHIFT),
    (341, MOD_CONTROL),
    (342, MOD_ALT),
    (343, MOD_SUPER),
    (344, MOD_SHIFT),
    (345, MOD_CONTROL),
    (346, MOD_ALT),
    (347, MOD_SUPER),
];
/// GLFW key codes are below this (`GLFW_KEY_LAST` + 1).
const KEY_COUNT: usize = 349;
/// The rebinding's key id (SDL scancode) of each GLFW key code; 0 for none.
const KEY_IDS: [u16; KEY_COUNT] = {
    let mut ids = [0; KEY_COUNT];
    let mut key = 0;
    while key < KEY_COUNT {
        if let Some(InputId::Key(sc)) = input_from_glfw_key(key as c_int, 0) {
            ids[key] = sc;
        }
        key += 1;
    }
    ids
};
const CURSOR: c_int = 0x0003_3001;
const CURSOR_DISABLED: c_int = 0x0003_4003;
const RAW_MOUSE_MOTION: c_int = 0x0003_3005;

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
    framebuffer_size: Option<FramebufferSizeFn>,
}

// SAFETY: plain function pointers.
unsafe impl Send for GameCallbacks {}

static GAME: Mutex<Option<HashMap<usize, GameCallbacks>>> = Mutex::new(None);
static GET_INPUT_MODE: OnceLock<GetInputModeFn> = OnceLock::new();
/// GLFW 3.3 and later: raw input is the GLFW_RAW_MOUSE_MOTION input mode, which Minecraft's
/// "Raw Input" setting switches.
static RAW_MOTION_OPTIONAL: AtomicBool = AtomicBool::new(false);
static SET_CLIPBOARD_STRING: OnceLock<SetClipboardStringFn> = OnceLock::new();
/// Trampoline to the original glfwGetKey: the real key state, never spoofed.
static GET_KEY: OnceLock<GetKeyFn> = OnceLock::new();
/// glfwGetKey itself, for the real key state if it could not be detoured.
static GET_KEY_EXPORT: OnceLock<GetKeyFn> = OnceLock::new();
static GET_KEY_SCANCODE: OnceLock<GetKeyScancodeFn> = OnceLock::new();
/// While F3+C is being sent, glfwGetKey reports `SPOOF_KEY` of this window as pressed
/// (0 = off).
static SPOOF_WINDOW: AtomicUsize = AtomicUsize::new(0);
static SPOOF_KEY: AtomicI32 = AtomicI32::new(0);
/// glfwGetKey is detoured: the game reads the key state the rebinding gives it.
static KEY_STATE_SPOOFED: AtomicBool = AtomicBool::new(false);

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
pub fn captured(window: *mut c_void) -> bool {
    GET_INPUT_MODE.get().is_some_and(|get| {
        // SAFETY: a window GLFW passed us, on its event thread.
        unsafe { get(window, CURSOR) == CURSOR_DISABLED }
    })
}

/// Whether a captured cursor reports raw mouse counts rather than the system pointer's
/// motion. GLFW builds without `glfwRawMouseMotionSupported` (the 3.3 pre-release in
/// LWJGL 3.1.6, as Minecraft 1.13 ships) always use raw input for a disabled cursor.
pub fn raw_motion(window: *mut c_void) -> bool {
    if !RAW_MOTION_OPTIONAL.load(Ordering::Relaxed) {
        return true;
    }
    GET_INPUT_MODE.get().is_none_or(|get| {
        // SAFETY: a window GLFW passed us; the mode exists in this GLFW version.
        unsafe { get(window, RAW_MOUSE_MOTION) != 0 }
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
setter!(
    set_framebuffer_size_detour,
    SET_FRAMEBUFFER_SIZE,
    framebuffer_size,
    FramebufferSizeFn,
    framebuffer_size_wrapper
);

/// Detours the callback setters. Failures only cost the overlay its input.
pub fn install(module: HMODULE) {
    if let Some(f) = export(module, c"glfwGetInputMode") {
        // SAFETY: glfwGetInputMode has this signature.
        let _ =
            GET_INPUT_MODE.set(unsafe { std::mem::transmute::<*const c_void, GetInputModeFn>(f) });
    }
    RAW_MOTION_OPTIONAL.store(
        export(module, c"glfwRawMouseMotionSupported").is_some(),
        Ordering::Relaxed,
    );
    // True when the detour is in.
    macro_rules! hook {
        ($name:literal, $detour:ident, $original:ident) => {
            hook!($name, $detour, $original, "no overlay input")
        };
        ($name:literal, $detour:ident, $original:ident, $lost:literal) => {
            match export(module, $name) {
                // SAFETY: the function and its detour share the slot's signature; nothing
                // runs GLFW while it is being loaded.
                Some(target) => {
                    let name = $name.to_string_lossy();
                    match unsafe {
                        hook::install(&name, target, $detour as *const c_void, &$original)
                    } {
                        Ok(()) => true,
                        Err(e) => {
                            log::warn!("{}: {e}", $lost);
                            false
                        }
                    }
                }
                None => {
                    log::warn!("GLFW does not export {}", $name.to_string_lossy());
                    false
                }
            }
        };
    }
    let key_hooked = hook!(c"glfwSetKeyCallback", set_key_detour, SET_KEY);
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
    hook!(
        c"glfwSetFramebufferSizeCallback",
        set_framebuffer_size_detour,
        SET_FRAMEBUFFER_SIZE
    );

    // F3+C: the game's clipboard writes, the real key state, and F3 as 1.16 reads it.
    let clipboard_hooked = hook!(
        c"glfwSetClipboardString",
        set_clipboard_string_detour,
        SET_CLIPBOARD_STRING,
        "no waypoints"
    );
    if let Some(f) = export(module, c"glfwGetKey") {
        // SAFETY: glfwGetKey has this signature.
        let _ = GET_KEY_EXPORT.set(unsafe { std::mem::transmute::<*const c_void, GetKeyFn>(f) });
    }
    let get_key_hooked = hook!(
        c"glfwGetKey",
        get_key_detour,
        GET_KEY,
        "older Minecraft versions may refuse F3+C, and keys cannot be rebound"
    );
    KEY_STATE_SPOOFED.store(get_key_hooked, Ordering::Relaxed);
    if let Some(f) = export(module, c"glfwGetKeyScancode") {
        // SAFETY: glfwGetKeyScancode has this signature (GLFW 3.3+; optional).
        let _ = GET_KEY_SCANCODE
            .set(unsafe { std::mem::transmute::<*const c_void, GetKeyScancodeFn>(f) });
    }
    f3c::set_hooks_ready(key_hooked && clipboard_hooked && real_get_key().is_some());
}

fn modifiers(mods: c_int) -> reminedog_render::Modifiers {
    input::modifiers(
        mods & MOD_SHIFT != 0,
        mods & MOD_CONTROL != 0,
        mods & MOD_ALT != 0,
    )
}

/// Whether the game reads the key state the rebinding gives it (without that, keys cannot be
/// rebound: a held source would read as held).
pub fn key_state_spoofed() -> bool {
    KEY_STATE_SPOOFED.load(Ordering::Relaxed)
}

/// The rebinding's key id of a GLFW key code (key -1 has none: GLFW keeps no state for it).
fn key_id(key: c_int) -> Option<InputId> {
    let id = *KEY_IDS.get(usize::try_from(key).ok()?)?;
    (id != 0).then_some(InputId::Key(id))
}

unsafe extern "C" fn key_wrapper(
    window: *mut c_void,
    key: c_int,
    scancode: c_int,
    action: c_int,
    mods: c_int,
) {
    let delivery = ffi::catch("GLFW key", || {
        let pressed = action == PRESS || action == REPEAT;
        let mut router = router();
        let delivery = router.key(
            egui_key(key),
            input_from_glfw_key(key, scancode),
            pressed,
            action == REPEAT,
            modifiers(mods),
            captured(window),
            key == -1,
        );
        // GLFW updated its key state before calling us; the game reads ours from now on.
        rebind_state::apply(window as usize, &router.rebind_state());
        delivery
    });
    match delivery.unwrap_or(Delivery::Forward) {
        Delivery::Forward => key_to_game(window, key, scancode, action, adjust_mods(window, mods)),
        Delivery::Consume => {}
        Delivery::Send(output) => send_output(window, output, adjust_mods(window, mods), mods),
    }
}

/// Calls the game's key callback. The user's own F3+C: a location the game copies while
/// handling the copy key's press is theirs.
fn key_to_game(window: *mut c_void, key: c_int, scancode: c_int, action: c_int, mods: c_int) {
    let Some(game) = game_callbacks(window).key else {
        return;
    };
    let passive = action == PRESS && f3c::passive_key_glfw(key);
    if passive {
        ffi::catch("GLFW F3+C", f3c::begin_passive);
    }
    // SAFETY: a window GLFW passed us, on the thread that handles its events.
    unsafe { game(window, key, scancode, action, mods) };
    if passive {
        ffi::catch("GLFW F3+C", f3c::end_passive);
    }
}

/// Gives the game the rebinding's `output` as GLFW would report it: a key with `key_mods`, a
/// mouse button with `button_mods` (Minecraft takes a click's modifiers as they physically are).
fn send_output(window: *mut c_void, output: Output, key_mods: c_int, button_mods: c_int) {
    match output.id {
        InputId::Key(_) => {
            let Some((key, scancode)) = glfw_key_of(output.id) else {
                return;
            };
            // What GLFW reports for the key, else the table's. Not asked for key -1 (GLFW would
            // report an invalid key to the game's error callback).
            let scancode = match key {
                -1 => scancode,
                _ => match key_scancode(key) {
                    0 => scancode,
                    reported => reported,
                },
            };
            let action = match output.phase {
                Phase::Press => PRESS,
                Phase::Repeat => REPEAT,
                Phase::Release => RELEASE,
            };
            key_to_game(window, key, scancode, action, key_mods);
        }
        InputId::Mouse(_) => {
            // A mouse button never repeats; the game would take a repeat for a release.
            let action = match output.phase {
                Phase::Press => PRESS,
                Phase::Release => RELEASE,
                Phase::Repeat => return,
            };
            if let Some(button) = glfw_button_of(output.id)
                && let Some(game) = game_callbacks(window).mouse_button
            {
                // SAFETY: as above.
                unsafe { game(window, button, action, button_mods) };
            }
        }
    }
}

/// The modifier bits of a key event for the game. While the rebinding has a say on a modifier
/// key, Shift, Control, Alt and Super follow the key state the game reads (GLFW sets them from
/// the physical keys, so a key rebound to Ctrl would not set Control, and Ctrl rebound to F3
/// would turn F3+B into Ctrl+B).
fn adjust_mods(window: *mut c_void, mods: c_int) -> c_int {
    if !rebind_state::modifiers_forced() {
        return mods;
    }
    let real = real_get_key();
    rebuild_mods(mods, |key| {
        let physically = || {
            // SAFETY: a window GLFW passed us, on its event thread; a valid key code.
            real.is_some_and(|get| unsafe { get(window, key) } == PRESS)
        };
        match key_id(key) {
            Some(id) => rebind_state::held(id, physically),
            None => physically(),
        }
    })
}

/// `mods` with the Shift, Control, Alt and Super bits set by which modifier keys are `held`.
fn rebuild_mods(mods: c_int, held: impl Fn(c_int) -> bool) -> c_int {
    MODIFIER_KEYS.iter().fold(
        mods & !MOD_KEYS,
        |bits, &(key, bit)| {
            if held(key) { bits | bit } else { bits }
        },
    )
}

/// The safety net's releases ([`rebind_state::lost_releases`]), after a swap of `window`.
pub fn release_lost_keys(window: *mut c_void) {
    for output in rebind_state::lost_releases(window as usize) {
        send_output(window, output, 0, 0);
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
    let delivery = ffi::catch("GLFW mouse button", || {
        let raw = input_from_glfw_button(button);
        let pointer = match button {
            0 => PointerButton::Primary,
            1 => PointerButton::Secondary,
            2 => PointerButton::Middle,
            3 => PointerButton::Extra1,
            4 => PointerButton::Extra2,
            _ => return Delivery::Forward,
        };
        let mut router = router();
        let delivery = router.button(pointer, raw, action == PRESS, captured(window));
        rebind_state::apply(window as usize, &router.rebind_state());
        delivery
    });
    match delivery.unwrap_or(Delivery::Forward) {
        Delivery::Forward => {
            if let Some(game) = game_callbacks(window).mouse_button {
                // SAFETY: as above.
                unsafe { game(window, button, action, mods) };
            }
        }
        Delivery::Consume => {}
        Delivery::Send(output) => send_output(window, output, adjust_mods(window, mods), mods),
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
        // GLFW sends the releases of held keys (but key -1) and buttons after this callback,
        // which the router drops for rebound sources: their outputs are released here, first.
        let releases = ffi::catch("GLFW focus", || {
            let mut router = router();
            let releases = router.focus_lost(|id| !matches!(glfw_key_of(id), Some((-1, _))));
            rebind_state::apply(window as usize, &router.rebind_state());
            releases
        })
        .unwrap_or_default();
        for output in releases {
            // Without modifiers, as GLFW's own releases on focus loss.
            send_output(window, output, 0, 0);
        }
    }
    if let Some(game) = game_callbacks(window).focus {
        // SAFETY: as above.
        unsafe { game(window, focused) };
    }
}

/// While the game renders at the zoom's tall size, a real resize is held back until the
/// zoom ends (the game is then told the new size).
unsafe extern "C" fn framebuffer_size_wrapper(window: *mut c_void, width: c_int, height: c_int) {
    let held = ffi::catch("GLFW framebuffer size", || crate::tall::real_resize(window));
    if held != Some(true) {
        send_framebuffer_size(window, width, height);
    }
}

/// Calls the game's framebuffer size callback, as GLFW does on a resize.
pub fn send_framebuffer_size(window: *mut c_void, width: c_int, height: c_int) {
    if let Some(game) = game_callbacks(window).framebuffer_size {
        log::debug!("framebuffer size {width}x{height} to the game");
        // SAFETY: a window GLFW passed us, on the thread that handles its events.
        unsafe { game(window, width, height) };
    }
}

/// Keeps the location Minecraft copies for our F3+C off the clipboard, and sees the user's
/// own F3+C.
unsafe extern "C" fn set_clipboard_string_detour(window: *mut c_void, text: *const c_char) {
    let Some(original) = SET_CLIPBOARD_STRING.get() else {
        return;
    };
    let ours = !text.is_null()
        && ffi::catch("glfwSetClipboardString detour", || {
            // SAFETY: NUL-terminated UTF-8, valid only during this call (LWJGL frees it right
            // after); it is read here, before returning.
            f3c::on_clipboard(&unsafe { CStr::from_ptr(text) }.to_string_lossy())
        })
        .unwrap_or(false);
    if !ours {
        // SAFETY: the caller's arguments.
        unsafe { original(window, text) };
    }
}

/// The key state the game should see: the modifier while F3+C is being sent (Minecraft 1.16
/// asks glfwGetKey whether F3 is held when C's key event arrives), else the rebinding's say
/// ([`rebind_state`]), else the real state. Lock-free: the game calls it often.
unsafe extern "C" fn get_key_detour(window: *mut c_void, key: c_int) -> c_int {
    let state = match GET_KEY.get() {
        // SAFETY: the caller's arguments; GLFW reports its own errors.
        Some(original) => unsafe { original(window, key) },
        None => RELEASE,
    };
    let spoofed = SPOOF_WINDOW.load(Ordering::Relaxed);
    if spoofed != 0 && spoofed == window as usize && SPOOF_KEY.load(Ordering::Relaxed) == key {
        return PRESS;
    }
    match key_id(key).and_then(|id| rebind_state::forced_in(window as usize, id)) {
        Some(true) => PRESS,
        Some(false) => RELEASE,
        None => state,
    }
}

/// glfwGetKey without the spoof.
fn real_get_key() -> Option<GetKeyFn> {
    GET_KEY.get().or(GET_KEY_EXPORT.get()).copied()
}

/// The scancode GLFW would report with `key`; 0 if it cannot tell (Minecraft only looks at
/// scancodes of keys without a key code). Asked when needed: GLFW must be initialized.
fn key_scancode(key: c_int) -> c_int {
    GET_KEY_SCANCODE.get().map_or(0, |get| {
        // SAFETY: glfwGetKeyScancode takes any key code.
        match unsafe { get(key) } {
            -1 => 0,
            scancode => scancode,
        }
    })
}

/// Calls the game's current key callback as GLFW would, without modifiers; false if the game
/// has none.
fn send_key(window: *mut c_void, key: c_int, action: c_int) -> bool {
    let Some(game) = game_callbacks(window).key else {
        return false;
    };
    // SAFETY: a window GLFW passed us, on the thread that handles its events.
    unsafe { game(window, key, key_scancode(key), action, 0) };
    true
}

/// Sends a waypoint request's F3+C to the game, if one waits for `window`. Called after the
/// swap, outside every lock: the game handles the keys (and copies the location) right away.
pub fn run_f3c(window: *mut c_void) {
    let Some(job) = f3c::take_job(Some(window as usize)) else {
        return;
    };
    match check_f3c(window, &job.keys) {
        Ok((modifier, copy)) => send_f3c(window, job.purpose, modifier, copy),
        Err(reason) => f3c::fail(job.purpose, reason),
    }
}

/// The modifier's and copy key's codes, if F3+C can be sent now.
fn check_f3c(window: *mut c_void, keys: &DebugKeys) -> Result<(c_int, c_int), Failure> {
    let codes = f3c::key_codes(keys, glfw_key)?;
    if !captured(window) {
        return Err(Failure::NotPlaying);
    }
    let get_key = real_get_key().ok_or(Failure::NoHooks)?;
    // A held crash key would arm Minecraft's debug crash once the modifier is down, the real
    // release of a held modifier would come after ours (and toggle the debug overlay), and,
    // where the copy key also drops items, Ctrl would make a refused C drop a whole stack
    // (only then: Ctrl is often held to sprint). Held as the game reads them: a key rebound
    // to one of them counts, one rebound to another key does not.
    let ctrl = keys.copy_drops_items();
    let watched = [
        Some(codes.modifier),
        Some(codes.copy),
        codes.crash,
        ctrl.then_some(KEY_LEFT_CONTROL),
        ctrl.then_some(KEY_RIGHT_CONTROL),
    ];
    let held: Vec<c_int> = watched
        .into_iter()
        .flatten()
        .filter(|&key| {
            // SAFETY: a window GLFW passed us, on its event thread; valid key codes.
            let physically = || unsafe { get_key(window, key) } == PRESS;
            match key_id(key) {
                Some(id) => rebind_state::held(id, physically),
                None => physically(),
            }
        })
        .collect();
    if !held.is_empty() {
        let ids: Vec<InputId> = held.into_iter().filter_map(key_id).collect();
        return Err(Failure::KeysHeld(rebind_state::rule_holding(&ids)));
    }
    if game_callbacks(window).key.is_none() {
        return Err(Failure::NoCallback);
    }
    Ok((codes.modifier, codes.copy))
}

/// One F3+C on the game's key callback. However it ends, a panic included, dropping it clears
/// the spoof, releases a modifier it left pressed and settles the job: the game is never left
/// with the modifier held.
struct Burst {
    window: *mut c_void,
    purpose: Purpose,
    modifier: c_int,
    /// A modifier PRESS reached the game, and its RELEASE did not yet.
    modifier_down: bool,
    /// `end_injected` ran (any failure is reported right after).
    settled: bool,
}

impl Burst {
    fn begin(window: *mut c_void, purpose: Purpose, modifier: c_int) -> Self {
        // Made first, so it undoes the spoof whatever happens next.
        let burst = Self {
            window,
            purpose,
            modifier,
            modifier_down: false,
            settled: false,
        };
        SPOOF_KEY.store(modifier, Ordering::Relaxed);
        SPOOF_WINDOW.store(window as usize, Ordering::Relaxed);
        f3c::begin_injected(purpose);
        burst
    }

    fn press_modifier(&mut self) -> bool {
        self.modifier_down = send_key(self.window, self.modifier, PRESS);
        self.modifier_down
    }

    fn release_modifier(&mut self) {
        if std::mem::take(&mut self.modifier_down) {
            send_key(self.window, self.modifier, RELEASE);
        }
    }
}

impl Drop for Burst {
    fn drop(&mut self) {
        stop_spoof();
        self.release_modifier();
        if !self.settled {
            let _ = f3c::end_injected();
            f3c::fail(self.purpose, Failure::NoCallback);
        }
    }
}

fn stop_spoof() {
    SPOOF_WINDOW.store(0, Ordering::Relaxed);
}

/// Modifier PRESS, copy PRESS, copy RELEASE, modifier RELEASE (the spoof ends before it).
/// Refused, that RELEASE toggled the debug overlay; one more PRESS and RELEASE toggles it back.
fn send_f3c(window: *mut c_void, purpose: Purpose, modifier: c_int, copy: c_int) {
    let mut burst = Burst::begin(window, purpose, modifier);
    // The callback disappears only at shutdown; the guard reports it.
    if !burst.press_modifier() || !send_key(window, copy, PRESS) {
        return;
    }
    send_key(window, copy, RELEASE);
    stop_spoof();
    burst.release_modifier();
    let copied = f3c::end_injected();
    burst.settled = true;
    match copied {
        Injected::Captured => {}
        // The game handled the keys; only its text was not a location.
        Injected::Written => f3c::fail(purpose, Failure::Unreadable),
        Injected::Nothing => {
            if burst.press_modifier() {
                burst.release_modifier();
            }
            f3c::fail(purpose, Failure::Refused);
        }
    }
}

/// egui's name for a GLFW key code (the keys a text field or a hotkey can use). GLFW key
/// codes name positions on a US keyboard.
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
        290..=314 => FUNCTION_KEYS[(key - 290) as usize],
        39 => Key::Quote,
        44 => Key::Comma,
        45 | 333 => Key::Minus,  // keypad -
        46 | 330 => Key::Period, // keypad .
        47 | 331 => Key::Slash,  // keypad /
        59 => Key::Semicolon,
        61 | 336 => Key::Equals, // keypad =
        91 => Key::OpenBracket,
        92 => Key::Backslash,
        93 => Key::CloseBracket,
        96 => Key::Backtick,
        320..=329 => DIGITS[(key - 320) as usize], // keypad digits
        334 => Key::Plus,                          // keypad +
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_ids_of_glfw_key_codes() {
        assert_eq!(key_id(65), Some(InputId::Key(4))); // A
        assert_eq!(key_id(292), Some(InputId::Key(60))); // F3
        assert_eq!(key_id(340), Some(InputId::Key(225))); // left Shift
        assert_eq!(key_id(347), Some(InputId::Key(231))); // right Super
        assert_eq!(key_id(348), Some(InputId::Key(101))); // Menu
        // Both of GLFW's "world" keys are the ISO key.
        assert_eq!(key_id(161), Some(InputId::Key(100)));
        assert_eq!(key_id(162), Some(InputId::Key(100)));
        // Key -1, codes GLFW does not use, F25 (SDL3 has none), out of range.
        for key in [-1, i32::MIN, 0, 31, 64, 314, 349, i32::MAX] {
            assert_eq!(key_id(key), None, "{key}");
        }
        // The modifier keys are the rebinding's modifier keys.
        for (key, _) in MODIFIER_KEYS {
            let Some(InputId::Key(id)) = key_id(key) else {
                panic!("{key}");
            };
            assert!(
                rebind_state::MODIFIER_SCANCODES.contains(&usize::from(id)),
                "{key}"
            );
        }
    }

    #[test]
    fn modifier_bits_follow_the_held_keys() {
        const CAPS_LOCK: c_int = 0x10;
        // Nothing held: only Caps Lock's bit stays.
        assert_eq!(rebuild_mods(MOD_KEYS | CAPS_LOCK, |_| false), CAPS_LOCK);
        // Either side sets the bit.
        assert_eq!(rebuild_mods(0, |key| key == 345), MOD_CONTROL);
        assert_eq!(rebuild_mods(0, |key| key == 340), MOD_SHIFT);
        assert_eq!(
            rebuild_mods(MOD_CONTROL, |key| matches!(key, 342 | 347)),
            MOD_ALT | MOD_SUPER
        );
    }
}
