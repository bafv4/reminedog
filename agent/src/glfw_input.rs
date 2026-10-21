//! GLFW の入力：オーバーレイがどのイベントも先に見られるように、ゲームのコールバックを包む。
//!
//! `glfwSet*Callback` の関数をフックする（[`crate::hook`]）。GLFW にはゲームのコールバックの
//! 代わりに自前の包みを渡し、ゲームのポインタはここに覚える。各設定の関数は、包みではなく
//! ゲームの前のポインタを返す：LWJGL は設定の関数が返したものを解放する（終了時の
//! `Callbacks.glfwFreeCallbacks`）ので、包みを返すと終了時に落ちる。
//! 包みは、ロックを持ったままゲームのコードを呼ばない。
//!
//! キーの置き換えは、キーでもマウスのボタンでも、イベントの代わりに router の出力をゲームの
//! コールバックに渡す。`glfwGetKey` とキーのイベントの修飾キーのビットは、ゲームが受け取った
//! ものを答える（[`rebind_state`]）。
//!
//! ウェイポイントのための F3+C（[`f3c`]）は、スワップの後にゲームのキーのコールバックへ直接
//! 送る（[`run_f3c`]）。座標を捕まえるために `glfwSetClipboardString` を、その間は修飾キーを
//! 押していると答えるために `glfwGetKey` をフックする（Minecraft 1.16 は F3 をそう読む）。

use std::collections::HashMap;
use std::ffi::{CStr, c_char, c_int, c_uint, c_void};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering};
use std::sync::{LazyLock, Mutex, OnceLock};

use reminedog_core::{
    DebugKeys, InputId, Naming, glfw_button_of, glfw_key, glfw_key_of, input_from_glfw_button,
    input_from_glfw_key,
};
use reminedog_render::{Delivery, Key, Output, Phase, PointerButton, Route};

use crate::f3c::{self, Failure, Injected, Purpose};
use crate::hook::{Hook, Original};
use crate::input::{self, FUNCTION_KEYS, router};
use crate::library::export;
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
/// 置き換えが変えてよい修飾キーのビット。Caps Lock と Num Lock のものは残す。
const MOD_KEYS: c_int = MOD_SHIFT | MOD_CONTROL | MOD_ALT | MOD_SUPER;
/// 修飾キーとそのビット：左の Shift、Control、Alt、Super、その後に右のもの。ビットは、
/// どちらかのキーを押している間立つ。
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
/// GLFW のキーコードはこれより小さい（`GLFW_KEY_LAST` + 1）。
const KEY_COUNT: usize = 349;
/// 各 GLFW のキーコードの、置き換えのキーの id（SDL のスキャンコード）。なければ 0。
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

/// 1 つのウィンドウのゲームのコールバック。
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

// SAFETY: ただの関数ポインタ。
unsafe impl Send for GameCallbacks {}

static GAME: Mutex<Option<HashMap<usize, GameCallbacks>>> = Mutex::new(None);
static GET_INPUT_MODE: OnceLock<GetInputModeFn> = OnceLock::new();
/// GLFW 3.3 以降：生の入力は GLFW_RAW_MOUSE_MOTION の入力のモードで、Minecraft の
/// 「Raw Input」の設定がこれを切り替える。
static RAW_MOTION_OPTIONAL: AtomicBool = AtomicBool::new(false);
static SET_CLIPBOARD_STRING: Original<SetClipboardStringFn> = Original::new();
/// 本物の glfwGetKey：本当のキーの状態。偽装しない。
static GET_KEY: Original<GetKeyFn> = Original::new();
/// glfwGetKey そのもの：エージェントのための本当のキーの状態（ゲームの呼び出しはフックを通る）。
static GET_KEY_EXPORT: OnceLock<GetKeyFn> = OnceLock::new();
static GET_KEY_SCANCODE: OnceLock<GetKeyScancodeFn> = OnceLock::new();
/// F3+C を送っている間、glfwGetKey はこのウィンドウの `SPOOF_KEY` を押していると答える
/// （0 = 偽装しない）。
static SPOOF_WINDOW: AtomicUsize = AtomicUsize::new(0);
static SPOOF_KEY: AtomicI32 = AtomicI32::new(0);

fn game_callbacks(window: *mut c_void) -> GameCallbacks {
    let game = GAME.lock().unwrap_or_else(|e| e.into_inner());
    game.as_ref()
        .and_then(|map| map.get(&(window as usize)).copied())
        .unwrap_or_default()
}

/// ゲームの新しいコールバックに入れ替え、前のものを返す。
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

/// ゲームがカーソルを捕まえているか（プレイ中で、メニューを出していない）。
pub fn captured(window: *mut c_void) -> bool {
    GET_INPUT_MODE.get().is_some_and(|get| {
        // SAFETY: GLFW が渡したウィンドウを、そのイベントのスレッドで。
        unsafe { get(window, CURSOR) == CURSOR_DISABLED }
    })
}

/// 捕まえたカーソルが、システムのポインターの動きではなく生のマウスの数を報告するか。
/// `glfwRawMouseMotionSupported` のない GLFW のビルド（Minecraft 1.13 が使う LWJGL 3.1.6 の
/// 3.3 の先行版）は、無効にしたカーソルでは必ず生の入力を使う。
pub fn raw_motion(window: *mut c_void) -> bool {
    if !RAW_MOTION_OPTIONAL.load(Ordering::Relaxed) {
        return true;
    }
    GET_INPUT_MODE.get().is_none_or(|get| {
        // SAFETY: GLFW が渡したウィンドウ。この GLFW の版にはこのモードがある。
        unsafe { get(window, RAW_MOUSE_MOTION) != 0 }
    })
}

/// 1 つの `glfwSet*Callback` の関数のフックを定義する。
macro_rules! setter {
    ($hook:ident, $original:ident, $field:ident, $fn:ty, $wrapper:ident) => {
        static $original: Original<unsafe extern "C" fn(*mut c_void, Option<$fn>) -> Option<$fn>> =
            Original::new();

        unsafe extern "C" fn $hook(window: *mut c_void, callback: Option<$fn>) -> Option<$fn> {
            let original = $original.get()?;
            let previous = crate::catch(stringify!($hook), || {
                replace_game_callback(window, |c| &mut c.$field, callback)
            })
            .flatten();
            let ours: Option<$fn> = callback.map(|_| $wrapper as $fn);
            // SAFETY: 同じウィンドウ。自前の包みはコールバックのシグネチャを持つ。
            unsafe { original(window, ours) };
            previous
        }
    };
}

setter!(set_key_hook, SET_KEY, key, KeyFn, key_wrapper);
setter!(set_char_hook, SET_CHAR, char, CharFn, char_wrapper);
setter!(
    set_char_mods_hook,
    SET_CHAR_MODS,
    char_mods,
    CharModsFn,
    char_mods_wrapper
);
setter!(
    set_mouse_button_hook,
    SET_MOUSE_BUTTON,
    mouse_button,
    MouseButtonFn,
    mouse_button_wrapper
);
setter!(
    set_cursor_pos_hook,
    SET_CURSOR_POS,
    cursor_pos,
    CursorPosFn,
    cursor_pos_wrapper
);
setter!(
    set_scroll_hook,
    SET_SCROLL,
    scroll,
    ScrollFn,
    scroll_wrapper
);
setter!(set_focus_hook, SET_FOCUS, focus, FocusFn, focus_wrapper);
setter!(
    set_framebuffer_size_hook,
    SET_FRAMEBUFFER_SIZE,
    framebuffer_size,
    FramebufferSizeFn,
    framebuffer_size_wrapper
);

static HOOKS: LazyLock<[Hook; 10]> = LazyLock::new(|| {
    [
        Hook::new(
            c"glfwSetKeyCallback",
            set_key_hook as *const c_void,
            &SET_KEY,
        ),
        Hook::new(
            c"glfwSetCharCallback",
            set_char_hook as *const c_void,
            &SET_CHAR,
        ),
        Hook::new(
            c"glfwSetCharModsCallback",
            set_char_mods_hook as *const c_void,
            &SET_CHAR_MODS,
        ),
        Hook::new(
            c"glfwSetMouseButtonCallback",
            set_mouse_button_hook as *const c_void,
            &SET_MOUSE_BUTTON,
        ),
        Hook::new(
            c"glfwSetCursorPosCallback",
            set_cursor_pos_hook as *const c_void,
            &SET_CURSOR_POS,
        ),
        Hook::new(
            c"glfwSetScrollCallback",
            set_scroll_hook as *const c_void,
            &SET_SCROLL,
        ),
        Hook::new(
            c"glfwSetWindowFocusCallback",
            set_focus_hook as *const c_void,
            &SET_FOCUS,
        ),
        Hook::new(
            c"glfwSetFramebufferSizeCallback",
            set_framebuffer_size_hook as *const c_void,
            &SET_FRAMEBUFFER_SIZE,
        ),
        // F3+C：ゲームのクリップボードへの書き込み、本当のキーの状態、1.16 が読む F3。
        Hook::new(
            c"glfwSetClipboardString",
            set_clipboard_string_hook as *const c_void,
            &SET_CLIPBOARD_STRING,
        ),
        Hook::new(c"glfwGetKey", get_key_hook as *const c_void, &GET_KEY),
    ]
});

/// GLFW の入力の関数のフック。
pub fn hooks() -> &'static [Hook] {
    &*HOOKS
}

/// コールバックの設定の関数をフックする。失敗しても、オーバーレイが入力を失うだけ。
pub fn install(module: usize) {
    if let Some(f) = export(module, c"glfwGetInputMode") {
        // SAFETY: glfwGetInputMode はこのシグネチャ。
        let _ =
            GET_INPUT_MODE.set(unsafe { std::mem::transmute::<*const c_void, GetInputModeFn>(f) });
    }
    RAW_MOTION_OPTIONAL.store(
        export(module, c"glfwRawMouseMotionSupported").is_some(),
        Ordering::Relaxed,
    );
    if let Some(f) = export(module, c"glfwGetKey") {
        // SAFETY: glfwGetKey はこのシグネチャ。
        let _ = GET_KEY_EXPORT.set(unsafe { std::mem::transmute::<*const c_void, GetKeyFn>(f) });
    }
    if let Some(f) = export(module, c"glfwGetKeyScancode") {
        // SAFETY: glfwGetKeyScancode はこのシグネチャ（GLFW 3.3 以降。なくてもよい）。
        let _ = GET_KEY_SCANCODE
            .set(unsafe { std::mem::transmute::<*const c_void, GetKeyScancodeFn>(f) });
    }
    f3c::set_hooks(f3c_hooks_ready);
}

/// F3+C が要るもの：フックを通るゲームのキーのコールバックとクリップボードへの書き込み
/// （LWJGL は GLFW の関数をまとめて引く）と、本当のキーの状態。
fn f3c_hooks_ready() -> bool {
    SET_KEY.get().is_some()
        && SET_CLIPBOARD_STRING.get().is_some()
        && GET_KEY.get().is_some()
        && real_get_key().is_some()
}

fn modifiers(mods: c_int) -> reminedog_render::Modifiers {
    input::modifiers(
        mods & MOD_SHIFT != 0,
        mods & MOD_CONTROL != 0,
        mods & MOD_ALT != 0,
    )
}

/// GLFW のキーコードの、置き換えのキーの id（key -1 にはない：GLFW はその状態を持たない）。
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
    let delivery = crate::catch("GLFW key", || {
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
        // GLFW は私たちを呼ぶ前に自分のキーの状態を更新した。ここからゲームはこちらのものを読む。
        rebind_state::apply(window as usize, &router.rebind_state());
        delivery
    });
    match delivery.unwrap_or(Delivery::Forward) {
        Delivery::Forward => key_to_game(window, key, scancode, action, adjust_mods(window, mods)),
        Delivery::Consume => {}
        Delivery::Send(output) => send_output(window, output, adjust_mods(window, mods), mods),
    }
}

/// ゲームのキーのコールバックを呼ぶ。利用者自身の F3+C：コピーのキーの押下を処理する間に
/// ゲームがコピーする座標は、利用者のもの。
fn key_to_game(window: *mut c_void, key: c_int, scancode: c_int, action: c_int, mods: c_int) {
    let Some(game) = game_callbacks(window).key else {
        return;
    };
    // ゲームから見てデバッグの修飾キーを押しているときだけ：入力欄の Ctrl+C は、座標に読める
    // 文字（/tp のコマンド）をコピーするかもしれない。
    let passive = action == PRESS && f3c::passive_key_glfw(key) && input::router().debug_held();
    if passive {
        crate::catch("GLFW F3+C", f3c::begin_passive);
    }
    // SAFETY: GLFW が渡したウィンドウを、そのイベントを処理するスレッドで。
    unsafe { game(window, key, scancode, action, mods) };
    if passive {
        crate::catch("GLFW F3+C", f3c::end_passive);
    }
}

/// 置き換えの `output` を、GLFW が報告するようにゲームに渡す：キーなら `key_mods` で、マウスの
/// ボタンなら `button_mods` で（Minecraft はクリックの修飾キーを物理のままで受け取る）。
fn send_output(window: *mut c_void, output: Output, key_mods: c_int, button_mods: c_int) {
    match output.id {
        InputId::Key(_) => {
            let Some((key, scancode)) = glfw_key_of(output.id) else {
                return;
            };
            // キーについて GLFW が答えるもの、なければ表のもの。key -1 については聞かない（GLFW は
            // 無効なキーとしてゲームのエラーのコールバックに報告してしまう）。
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
            // マウスのボタンはリピートしない。ゲームはリピートを解放と受け取ってしまう。
            let action = match output.phase {
                Phase::Press => PRESS,
                Phase::Release => RELEASE,
                Phase::Repeat => return,
            };
            if let Some(button) = glfw_button_of(output.id)
                && let Some(game) = game_callbacks(window).mouse_button
            {
                // SAFETY: 上と同じ。
                unsafe { game(window, button, action, button_mods) };
            }
        }
    }
}

/// ゲームに渡すキーのイベントの修飾キーのビット。置き換えが修飾キーについて判断を持つ間は、
/// Shift、Control、Alt、Super は、ゲームが読むキーの状態に従う（GLFW は物理のキーから立てる
/// ので、Ctrl に置き換えたキーでは Control が立たず、F3 に置き換えた Ctrl は F3+B を Ctrl+B に
/// してしまう）。
fn adjust_mods(window: *mut c_void, mods: c_int) -> c_int {
    if !rebind_state::modifiers_forced() {
        return mods;
    }
    let real = real_get_key();
    rebuild_mods(mods, |key| {
        let physically = || {
            // SAFETY: GLFW が渡したウィンドウを、そのイベントのスレッドで。有効なキーコード。
            real.is_some_and(|get| unsafe { get(window, key) } == PRESS)
        };
        match key_id(key) {
            Some(id) => rebind_state::held(id, physically),
            None => physically(),
        }
    })
}

/// `mods` の Shift、Control、Alt、Super のビットを、`held` の修飾キーで立てたもの。
fn rebuild_mods(mods: c_int, held: impl Fn(c_int) -> bool) -> c_int {
    MODIFIER_KEYS.iter().fold(
        mods & !MOD_KEYS,
        |bits, &(key, bit)| {
            if held(key) { bits | bit } else { bits }
        },
    )
}

/// `window` のスワップの後の、安全網の解放（[`rebind_state::lost_releases`]）。
pub fn release_lost_keys(window: *mut c_void) {
    for output in rebind_state::lost_releases(window as usize) {
        send_output(window, output, 0, 0);
    }
}

fn text_route(codepoint: c_uint) -> Option<Route> {
    crate::catch("GLFW char", || {
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
        // SAFETY: 上と同じ。
        unsafe { game(window, codepoint) };
    }
}

unsafe extern "C" fn char_mods_wrapper(window: *mut c_void, codepoint: c_uint, mods: c_int) {
    if text_route(codepoint) != Some(Route::Consume)
        && let Some(game) = game_callbacks(window).char_mods
    {
        // SAFETY: 上と同じ。
        unsafe { game(window, codepoint, mods) };
    }
}

unsafe extern "C" fn mouse_button_wrapper(
    window: *mut c_void,
    button: c_int,
    action: c_int,
    mods: c_int,
) {
    let delivery = crate::catch("GLFW mouse button", || {
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
                // SAFETY: 上と同じ。
                unsafe { game(window, button, action, mods) };
            }
        }
        Delivery::Consume => {}
        Delivery::Send(output) => send_output(window, output, adjust_mods(window, mods), mods),
    }
}

unsafe extern "C" fn cursor_pos_wrapper(window: *mut c_void, x: f64, y: f64) {
    let forward = crate::catch("GLFW cursor", || {
        router().cursor_position(x, y, captured(window))
    })
    .unwrap_or(Some((x, y)));
    if let Some((x, y)) = forward
        && let Some(game) = game_callbacks(window).cursor_pos
    {
        // SAFETY: 上と同じ（オーバーレイが取った動きの分だけ位置を直して）。
        unsafe { game(window, x, y) };
    }
}

unsafe extern "C" fn scroll_wrapper(window: *mut c_void, dx: f64, dy: f64) {
    let route = crate::catch("GLFW scroll", || router().scroll(dx as f32, dy as f32));
    if route != Some(Route::Consume)
        && let Some(game) = game_callbacks(window).scroll
    {
        // SAFETY: 上と同じ。
        unsafe { game(window, dx, dy) };
    }
}

unsafe extern "C" fn focus_wrapper(window: *mut c_void, focused: c_int) {
    if focused == 0 {
        // GLFW はこのコールバックの後に、押しているキー（key -1 を除く）とボタンの解放を送り、
        // router は置き換えた元のキーのものを捨てる：その出力は、ここで先に離す。
        let releases = crate::catch("GLFW focus", || {
            let mut router = router();
            let releases = router.focus_lost(|id| !matches!(glfw_key_of(id), Some((-1, _))));
            rebind_state::apply(window as usize, &router.rebind_state());
            releases
        })
        .unwrap_or_default();
        for output in releases {
            // GLFW 自身のフォーカスを失ったときの解放と同じく、修飾キーなしで。
            send_output(window, output, 0, 0);
        }
    }
    if let Some(game) = game_callbacks(window).focus {
        // SAFETY: 上と同じ。
        unsafe { game(window, focused) };
    }
}

/// ゲームがズームの縦長の大きさで描いている間は、本当の大きさの変更をズームが終わるまで
/// 止めておく（そのときに新しい大きさを伝える）。
unsafe extern "C" fn framebuffer_size_wrapper(window: *mut c_void, width: c_int, height: c_int) {
    let held = crate::catch("GLFW framebuffer size", || crate::tall::real_resize(window));
    if held != Some(true) {
        send_framebuffer_size(window, width, height);
    }
}

/// 大きさが変わったときの GLFW と同じく、ゲームのフレームバッファの大きさのコールバックを呼ぶ。
pub fn send_framebuffer_size(window: *mut c_void, width: c_int, height: c_int) {
    if let Some(game) = game_callbacks(window).framebuffer_size {
        log::debug!("framebuffer size {width}x{height} to the game");
        // SAFETY: GLFW が渡したウィンドウを、そのイベントを処理するスレッドで。
        unsafe { game(window, width, height) };
    }
}

/// 自前の F3+C で Minecraft がコピーする座標をクリップボードに入れず、利用者自身の F3+C を見る。
unsafe extern "C" fn set_clipboard_string_hook(window: *mut c_void, text: *const c_char) {
    let Some(original) = SET_CLIPBOARD_STRING.get() else {
        return;
    };
    // ほかの書き込み（ゲーム自身のコピー）は読まずに通す。
    let ours = !text.is_null()
        && f3c::watching()
        && crate::catch("glfwSetClipboardString hook", || {
            // SAFETY: NUL で終わる UTF-8 で、この呼び出しの間だけ有効（LWJGL はすぐ後に解放する）。
            // 戻る前にここで読む。
            f3c::on_clipboard(&unsafe { CStr::from_ptr(text) }.to_string_lossy())
        })
        .unwrap_or(false);
    if !ours {
        // SAFETY: 呼び出し側の引数。
        unsafe { original(window, text) };
    }
}

/// ゲームが見るべきキーの状態：F3+C を送っている間は修飾キー（Minecraft 1.16 は、C のキーの
/// イベントが来たときに F3 を押しているかを glfwGetKey に聞く）、そうでなければ置き換えの判断
/// （[`rebind_state`]）、それもなければ本当の状態。ロックなし：ゲームは頻繁に呼ぶ。
unsafe extern "C" fn get_key_hook(window: *mut c_void, key: c_int) -> c_int {
    let state = match GET_KEY.get() {
        // SAFETY: 呼び出し側の引数。エラーは GLFW が自分で報告する。
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

/// 偽装なしの glfwGetKey。
fn real_get_key() -> Option<GetKeyFn> {
    GET_KEY_EXPORT.get().copied()
}

/// `key` について GLFW が報告するスキャンコード。分からなければ 0（Minecraft がスキャンコードを
/// 見るのは、キーコードのないキーだけ）。要るときに聞く：GLFW が初期化されていなければならない。
fn key_scancode(key: c_int) -> c_int {
    GET_KEY_SCANCODE.get().map_or(0, |get| {
        // SAFETY: glfwGetKeyScancode はどのキーコードでも受け取る。
        match unsafe { get(key) } {
            -1 => 0,
            scancode => scancode,
        }
    })
}

/// GLFW がするように、修飾キーなしでゲームの今のキーのコールバックを呼ぶ。ゲームがコールバックを
/// 持っていなければ false。
fn send_key(window: *mut c_void, key: c_int, action: c_int) -> bool {
    let Some(game) = game_callbacks(window).key else {
        return false;
    };
    // SAFETY: GLFW が渡したウィンドウを、そのイベントを処理するスレッドで。
    unsafe { game(window, key, key_scancode(key), action, 0) };
    true
}

/// `window` へのウェイポイントの要求が待っていれば、その F3+C をゲームに送る。スワップの後、
/// どのロックも持たずに呼ぶ：ゲームはすぐにキーを処理する（そして座標をコピーする）。
pub fn run_f3c(window: *mut c_void) {
    let Some(job) = f3c::take_job(Some(window as usize)) else {
        return;
    };
    match check_f3c(window, &job.keys) {
        Ok((modifier, copy)) => send_f3c(window, job.purpose, modifier, copy),
        Err(reason) => f3c::fail(job.purpose, reason),
    }
}

/// いま F3+C を送れるなら、修飾キーとコピーのキーのコード。
fn check_f3c(window: *mut c_void, keys: &DebugKeys) -> Result<(c_int, c_int), Failure> {
    let codes = f3c::key_codes(keys, glfw_key)?;
    if !captured(window) {
        return Err(Failure::NotPlaying);
    }
    let get_key = real_get_key().ok_or(Failure::NoHooks)?;
    // 押しているクラッシュのキーは、修飾キーが下りると Minecraft のデバッグのクラッシュを始めて
    // しまう。押している修飾キーの本当の解放は自分のものの後に来る（デバッグの画面を切り替える）。
    // コピーのキーがアイテムを捨てるキーでもあるときは、Ctrl があると断られた C がスタックごと
    // 捨ててしまう（そのときだけ：Ctrl はダッシュでよく押している）。ゲームが読むとおりに押して
    // いるか：それらに置き換えたキーは数え、別のキーに置き換えたものは数えない。
    let ctrl = keys.copy_drops_items();
    let crash = f3c::crash_key(keys, Naming::Glfw)
        .and_then(glfw_key_of)
        .map(|(key, _)| key)
        .filter(|&key| key != -1);
    let watched = [
        Some(codes.modifier),
        Some(codes.copy),
        crash,
        ctrl.then_some(KEY_LEFT_CONTROL),
        ctrl.then_some(KEY_RIGHT_CONTROL),
    ];
    let held: Vec<c_int> = watched
        .into_iter()
        .flatten()
        .filter(|&key| {
            // SAFETY: GLFW が渡したウィンドウを、そのイベントのスレッドで。有効なキーコード。
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

/// ゲームのキーのコールバックへの 1 回の F3+C。どう終わっても（パニックも含めて）、捨てると
/// 偽装を消し、押したまま残った修飾キーを離し、仕事を終わらせる：ゲームに修飾キーを押した
/// まま残さない。
struct Burst {
    window: *mut c_void,
    purpose: Purpose,
    modifier: c_int,
    /// 修飾キーの PRESS はゲームに届き、RELEASE はまだ届いていない。
    modifier_down: bool,
    /// `end_injected` が動いた（失敗があれば、すぐ後に報告する）。
    settled: bool,
}

impl Burst {
    fn begin(window: *mut c_void, purpose: Purpose, modifier: c_int) -> Self {
        // 最初に作るので、この後に何が起きても偽装を戻す。
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

/// 修飾キーの PRESS、コピーの PRESS、コピーの RELEASE、修飾キーの RELEASE（偽装はその前に
/// 終わる）。断られた場合、その RELEASE がデバッグの画面を切り替えた。もう 1 回の PRESS と
/// RELEASE で戻す。
fn send_f3c(window: *mut c_void, purpose: Purpose, modifier: c_int, copy: c_int) {
    let mut burst = Burst::begin(window, purpose, modifier);
    // コールバックがなくなるのは終了時だけ。ガードが報告する。
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
        // ゲームはキーを処理した。その文字が座標でなかっただけ。
        Injected::Written => f3c::fail(purpose, Failure::Unreadable),
        // ゲームのコピーはフックを通らなかった：何を書いたにせよ、断ってはいない（デバッグの画面も
        // 切り替わっていない）。
        Injected::Nothing if SET_CLIPBOARD_STRING.get().is_none() => {
            f3c::fail(purpose, Failure::NoHooks);
        }
        Injected::Nothing => {
            if burst.press_modifier() {
                burst.release_modifier();
            }
            f3c::fail(purpose, Failure::Refused);
        }
    }
}

/// GLFW のキーコードの egui の名前（入力欄とホットキーが使えるキー）。GLFW のキーコードは
/// US 配列のキーボードの位置を名付ける。
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
        257 | 335 => Key::Enter, // Enter、テンキーの Enter
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
        45 | 333 => Key::Minus,  // テンキーの -
        46 | 330 => Key::Period, // テンキーの .
        47 | 331 => Key::Slash,  // テンキーの /
        59 => Key::Semicolon,
        61 | 336 => Key::Equals, // テンキーの =
        91 => Key::OpenBracket,
        92 => Key::Backslash,
        93 => Key::CloseBracket,
        96 => Key::Backtick,
        320..=329 => DIGITS[(key - 320) as usize], // テンキーの数字
        334 => Key::Plus,                          // テンキーの +
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
        assert_eq!(key_id(340), Some(InputId::Key(225))); // 左の Shift
        assert_eq!(key_id(347), Some(InputId::Key(231))); // 右の Super
        assert_eq!(key_id(348), Some(InputId::Key(101))); // Menu
        // GLFW の 2 つの "world" のキーは、どちらも ISO 配列のキー。
        assert_eq!(key_id(161), Some(InputId::Key(100)));
        assert_eq!(key_id(162), Some(InputId::Key(100)));
        // key -1、GLFW が使わないコード、F25（SDL3 にはない）、範囲外。
        for key in [-1, i32::MIN, 0, 31, 64, 314, 349, i32::MAX] {
            assert_eq!(key_id(key), None, "{key}");
        }
        // 修飾キーは、置き換えの修飾キー。
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
        // 何も押していない：Caps Lock のビットだけ残る。
        assert_eq!(rebuild_mods(MOD_KEYS | CAPS_LOCK, |_| false), CAPS_LOCK);
        // どちら側でもビットが立つ。
        assert_eq!(rebuild_mods(0, |key| key == 345), MOD_CONTROL);
        assert_eq!(rebuild_mods(0, |key| key == 340), MOD_SHIFT);
        assert_eq!(
            rebuild_mods(MOD_CONTROL, |key| matches!(key, 342 | 347)),
            MOD_ALT | MOD_SUPER
        );
    }
}
