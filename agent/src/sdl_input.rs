//! SDL3 の入力（Minecraft 26.x）：`SDL_PollEvent` でイベントを選り分ける。
//!
//! SDL3 は入力をキューに溜め、ゲームは `SDL_PollEvent` でキューを空にする。フックは各イベントを
//! router に渡し、オーバーレイが取るものは捨てるので、ゲームには見えない。ゲームがほかの方法
//! （イベントの監視、`SDL_PeepEvents`）で入力を読むかもしれない。それらの関数は最初に使われた
//! ときにログに出すだけなので、選り分けがゲームを覆っているかはログで分かる。
//!
//! キーの置き換えは、キーでもマウスのボタンでも、呼び出し側のイベントをその場で router の出力に
//! 書き換える。`SDL_GetKeyboardState` とキーのイベントの修飾キーのビットは、ゲームが受け取った
//! ものを答える（[`rebind_state`]）。
//!
//! ウェイポイントのための F3+C（[`f3c`]）は、SDL 自身のイベントより先に渡す作ったキーのイベント。
//! ゲームは次にポーリングする前に 1 つずつ処理するので、2 回のポーリングの間の
//! `SDL_SetClipboardText` は、その間に渡したイベントによるもの。

use std::collections::VecDeque;
use std::ffi::{CStr, c_char, c_int, c_void};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicUsize, Ordering};
use std::sync::{LazyLock, Mutex, OnceLock};

use reminedog_core::{InputId, Naming, SCANCODE_COUNT, sdl_keycode, sdl_keycode_of, sdl_scancode};
use reminedog_render::{Delivery, Key, Modifiers, Output, Phase, PointerButton, Route};

use crate::f3c::{self, Failure, Injected, Job, Purpose};
use crate::hook::{Hook, Original};
use crate::input::{self, FUNCTION_KEYS, router};
use crate::library::export;
use crate::rebind_state;

type PollEventFn = unsafe extern "C" fn(event: *mut u8) -> bool;
type GetWindowFromIdFn = unsafe extern "C" fn(id: u32) -> *mut c_void;
type WindowBoolFn = unsafe extern "C" fn(window: *mut c_void) -> bool;
type GetWindowPixelDensityFn = unsafe extern "C" fn(window: *mut c_void) -> f32;
type GetWindowIdFn = unsafe extern "C" fn(window: *mut c_void) -> u32;
type SetClipboardTextFn = unsafe extern "C" fn(text: *const c_char) -> bool;
type GetModStateFn = unsafe extern "C" fn() -> u16;

// 様子を見るためのフック。
type PeepEventsFn = unsafe extern "C" fn(*mut u8, c_int, c_int, u32, u32) -> c_int;
type WaitEventTimeoutFn = unsafe extern "C" fn(*mut u8, i32) -> bool;
type WaitEventFn = unsafe extern "C" fn(*mut u8) -> bool;
type SetRelativeMouseModeFn = unsafe extern "C" fn(*mut c_void, bool) -> bool;
type GetKeyboardStateFn = unsafe extern "C" fn(*mut c_int) -> *const bool;
type GetMouseStateFn = unsafe extern "C" fn(*mut f32, *mut f32) -> u32;
type AddEventWatchFn = unsafe extern "C" fn(*const c_void, *mut c_void) -> bool;
type SetEventFilterFn = unsafe extern "C" fn(*const c_void, *mut c_void);

// イベントの種類と SDL_Event のフィールドの位置（SDL 3.4。LWJGL 3.4.3 の並びと照らし合わせた）。
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
/// キーとボタンのイベントのキーボードかマウス。
const OFF_WHICH: usize = 20;
/// sizeof(SDL_Event)。
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
/// 修飾キーのスキャンコードとビット（SDL3 はキーごとに 1 ビット）。
const MODIFIER_BITS: [(u16, u16); 8] = [
    (224, 0x0040), // 左の Ctrl
    (225, 0x0001), // 左の Shift
    (226, 0x0100), // 左の Alt
    (227, 0x0400), // 左の GUI
    (228, 0x0080), // 右の Ctrl
    (229, 0x0002), // 右の Shift
    (230, 0x0200), // 右の Alt
    (231, 0x0800), // 右の GUI
];

const SCANCODE_LCTRL: u32 = 224;
const SCANCODE_RCTRL: u32 = 228;

type Event = [u8; EVENT_SIZE];

/// 作ったイベントを渡すことが、F3+C にとって何を意味するか。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tag {
    Plain,
    /// コピーのキーの押下：ゲームはこれを処理する間に座標をコピーする。
    CopyDown(Purpose),
    /// 修飾キーの解放：そのときには、ゲームがコピーしたかが分かっている。
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
    /// キーに置き換えたマウスのボタンのための、本当の修飾キーの状態（フックしない：Minecraft は
    /// これでクリックの修飾キーを読み、それは物理のまま）。
    get_mod_state: Option<GetModStateFn>,
}

static API: OnceLock<Api> = OnceLock::new();
static POLL_EVENT: Original<PollEventFn> = Original::new();
/// 最後の入力のイベントのウィンドウ（UI の開閉で文字の入力を切り替えるため）。
static LAST_WINDOW: AtomicUsize = AtomicUsize::new(0);
/// UI のために SDL の文字の入力をオンにしたので、またオフに戻す必要がある（ゲーム自身の
/// 切り替えを追えていないとき）。
static TEXT_INPUT_OURS: AtomicBool = AtomicBool::new(false);
/// 本物の SDL_StartTextInput と SDL_StopTextInput：ゲーム自身の切り替えを追うので、UI を
/// 閉じると、文字の入力はゲームが最後に望んだ状態になる。
static START_TEXT_INPUT: Original<WindowBoolFn> = Original::new();
static STOP_TEXT_INPUT: Original<WindowBoolFn> = Original::new();
/// ゲーム（か SDL）が最後に切り替えた文字の入力の状態。
static GAME_TEXT_INPUT: AtomicBool = AtomicBool::new(false);
/// UI が開いていて、文字の入力をオンにしている。
static UI_TEXT_INPUT: AtomicBool = AtomicBool::new(false);
/// ゲームのウィンドウのピクセルの密度（ウィンドウの単位あたりのピクセル）。フレームごとに求める。
static DENSITY: AtomicU32 = AtomicU32::new(1.0f32.to_bits());
/// エージェントが作ったイベント（ズームの大きさの変更、F3+C）。SDL 自身のものより先に渡す。
static INJECTED: Mutex<VecDeque<(Event, Tag)>> = Mutex::new(VecDeque::new());
static SET_CLIPBOARD_TEXT: Original<SetClipboardTextFn> = Original::new();

static PEEP_EVENTS: Original<PeepEventsFn> = Original::new();
static WAIT_EVENT: Original<WaitEventFn> = Original::new();
static WAIT_EVENT_TIMEOUT: Original<WaitEventTimeoutFn> = Original::new();
static SET_RELATIVE_MOUSE_MODE: Original<SetRelativeMouseModeFn> = Original::new();
/// ゲームの SDL_GetKeyboardState（置き換えのキーの状態を渡すフックの裏の本物）。
static GET_KEYBOARD_STATE: Original<GetKeyboardStateFn> = Original::new();
/// SDL_GetKeyboardState そのもの：エージェント（F3+C、置き換え）のための本当のキーの状態。
/// 作ったイベントでは変わらない。
static GET_KEYBOARD_STATE_EXPORT: OnceLock<GetKeyboardStateFn> = OnceLock::new();
/// 置き換えがどれかのキーについて判断を持つ間に SDL_GetKeyboardState が返すもの：それを
/// 当てはめた SDL の配列（SDL と同じく、スキャンコードごとの C の bool）。
static SPOOFED_KEYS: [AtomicU8; SCANCODE_COUNT] = [const { AtomicU8::new(0) }; SCANCODE_COUNT];
static GET_MOUSE_STATE: Original<GetMouseStateFn> = Original::new();
static GET_RELATIVE_MOUSE_STATE: Original<GetMouseStateFn> = Original::new();
static ADD_EVENT_WATCH: Original<AddEventWatchFn> = Original::new();
static SET_EVENT_FILTER: Original<SetEventFilterFn> = Original::new();

static HOOKS: LazyLock<[Hook; 13]> = LazyLock::new(|| {
    [
        Hook::new(
            c"SDL_PollEvent",
            poll_event_hook as *const c_void,
            &POLL_EVENT,
        ),
        Hook::new(
            c"SDL_SetClipboardText",
            set_clipboard_text_hook as *const c_void,
            &SET_CLIPBOARD_TEXT,
        ),
        Hook::new(
            c"SDL_StartTextInput",
            start_text_input_hook as *const c_void,
            &START_TEXT_INPUT,
        ),
        Hook::new(
            c"SDL_StopTextInput",
            stop_text_input_hook as *const c_void,
            &STOP_TEXT_INPUT,
        ),
        Hook::new(
            c"SDL_GetKeyboardState",
            get_keyboard_state_hook as *const c_void,
            &GET_KEYBOARD_STATE,
        ),
        Hook::new(
            c"SDL_PeepEvents",
            peep_events_probe as *const c_void,
            &PEEP_EVENTS,
        ),
        Hook::new(
            c"SDL_WaitEvent",
            wait_event_probe as *const c_void,
            &WAIT_EVENT,
        ),
        Hook::new(
            c"SDL_WaitEventTimeout",
            wait_event_timeout_probe as *const c_void,
            &WAIT_EVENT_TIMEOUT,
        ),
        Hook::new(
            c"SDL_SetWindowRelativeMouseMode",
            set_relative_mouse_mode_probe as *const c_void,
            &SET_RELATIVE_MOUSE_MODE,
        ),
        Hook::new(
            c"SDL_GetMouseState",
            get_mouse_state_probe as *const c_void,
            &GET_MOUSE_STATE,
        ),
        Hook::new(
            c"SDL_GetRelativeMouseState",
            get_relative_mouse_state_probe as *const c_void,
            &GET_RELATIVE_MOUSE_STATE,
        ),
        Hook::new(
            c"SDL_AddEventWatch",
            add_event_watch_probe as *const c_void,
            &ADD_EVENT_WATCH,
        ),
        Hook::new(
            c"SDL_SetEventFilter",
            set_event_filter_probe as *const c_void,
            &SET_EVENT_FILTER,
        ),
    ]
});

/// SDL3 の入力の関数のフック。
pub fn hooks() -> &'static [Hook] {
    &*HOOKS
}

pub fn install(module: usize) {
    let api = (|| {
        let get = |name: &CStr| export(module, name);
        // SAFETY: この C のシグネチャの SDL3 の export。
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
        return;
    };
    let has_window_id = api.get_window_id.is_some();
    let _ = API.set(api);

    if let Some(f) = export(module, c"SDL_GetKeyboardState") {
        // SAFETY: SDL_GetKeyboardState はこのシグネチャ。
        let _ = GET_KEYBOARD_STATE_EXPORT
            .set(unsafe { std::mem::transmute::<*const c_void, GetKeyboardStateFn>(f) });
    }
    // F3+C は、キーのイベントのためのゲームのウィンドウの番号と、本当のキーの状態が要る。
    if !has_window_id {
        log::warn!("SDL3 does not export SDL_GetWindowID");
    }
    f3c::set_hooks(f3c_hooks_ready);
}

/// F3+C が要るもの：ゲームのウィンドウの番号、本当のキーの状態、フックを通るイベント。
/// クリップボードのフックは入れない：LWJGL は SDL_SetClipboardText を、ゲームが最初に
/// コピーするとき（そのクラスが初期化されるとき）に引く。それは自前の F3+C そのものかもしれない。
fn f3c_hooks_ready() -> bool {
    API.get().is_some_and(|api| api.get_window_id.is_some())
        && real_keyboard_state().is_some()
        && POLL_EVENT.get().is_some()
}

unsafe extern "C" fn poll_event_hook(event: *mut u8) -> bool {
    let Some(original) = POLL_EVENT.get() else {
        return false;
    };
    crate::catch("SDL resize for the zoom", queue_zoom_resize);
    crate::catch("SDL F3+C", start_f3c);
    {
        let mut injected = INJECTED.lock().unwrap_or_else(|e| e.into_inner());
        if !injected.is_empty() {
            if !event.is_null() {
                let (made_up, tag) = injected.pop_front().expect("checked");
                if tag != Tag::Plain {
                    crate::catch("SDL F3+C", || on_injected(&mut injected, &made_up, tag));
                }
                // SAFETY: 呼び出し側のバッファは SDL_Event 1 つ分を持つ。
                unsafe { std::ptr::copy_nonoverlapping(made_up.as_ptr(), event, EVENT_SIZE) };
            }
            return true;
        }
    }
    loop {
        // SAFETY: 呼び出し側のイベントのバッファ（覗くだけなら null）。
        let has_event = unsafe { original(event) };
        if !has_event || event.is_null() {
            return has_event;
        }
        // SAFETY: SDL がいま SDL_Event 1 つ分を埋めた。
        let route = crate::catch("SDL_PollEvent hook", || unsafe { route_event(event) });
        if log::log_enabled!(log::Level::Trace) {
            // SAFETY: 上と同じ。
            let kind = unsafe { event.cast::<u32>().read_unaligned() };
            log::trace!("SDL event 0x{kind:X} -> {route:?}");
        }
        // UI はキーで開閉する（そうでなければ、フレームが変化に気づく）。
        // SAFETY: 上と同じ。
        if matches!(
            unsafe { event.cast::<u32>().read_unaligned() },
            EVENT_KEY_DOWN | EVENT_KEY_UP
        ) {
            crate::catch("SDL text input", update_text_input);
        }
        if route != Some(Route::Consume) {
            // SAFETY: 上と同じ。
            crate::catch("SDL F3+C", || unsafe { mark_passive(event) });
            return true;
        }
    }
}

/// 利用者自身の F3+C：利用者のコピーのキーの押下がゲームに行ったら、次のポーリングまでに
/// ゲームがコピーする座標は利用者のもの。
///
/// # Safety
/// `event` は、完全な SDL_Event を指していなければならない。
unsafe fn mark_passive(event: *const u8) {
    // SAFETY: 128 バイトの SDL_Event の中のフィールド。
    let (kind, repeat, scancode) = unsafe {
        (
            event.cast::<u32>().read_unaligned(),
            *event.add(OFF_KEY_REPEAT) != 0,
            event.add(OFF_KEY_SCANCODE).cast::<u32>().read_unaligned(),
        )
    };
    // ゲームから見てデバッグの修飾キーを押しているときだけ：入力欄の Ctrl+C は、座標に読める
    // 文字（/tp のコマンド）をコピーするかもしれない。
    if kind == EVENT_KEY_DOWN
        && !repeat
        && f3c::passive_key_sdl(scancode)
        && input::router().debug_held()
    {
        f3c::begin_passive();
    }
}

/// SDL_PollEvent のたびに：ゲームは前のイベントを処理したので、利用者自身の F3+C は終わる。
/// ウェイポイントの要求は、ここから渡すキーのイベントになる。
fn start_f3c() {
    f3c::end_passive();
    // INJECTED をロックする前に（ロックの順序）。
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

/// いま送れるなら、`job` の F3+C を SDL のキーのイベントとして。
fn f3c_events(job: &Job) -> Result<[(Event, Tag); 4], Failure> {
    let codes = f3c::key_codes(&job.keys, |name| sdl_scancode(name).zip(sdl_keycode(name)))?;
    let window = job.window as *mut c_void;
    if !captured(window) {
        return Err(Failure::NotPlaying);
    }
    // 押しているクラッシュのキーは、修飾キーが下りると Minecraft のデバッグのクラッシュを始めて
    // しまう。押している修飾キーの本当の解放は自分のものの後に来る（デバッグの画面を切り替える）。
    // コピーのキーがアイテムを捨てるキーでもあるときは、Ctrl があると断られた C がスタックごと
    // 捨ててしまう（そのときだけ：Ctrl はダッシュでよく押している）。ゲームが読むとおりに押して
    // いるか：それらに置き換えたキーは数え、別のキーに置き換えたものは数えない。
    let ctrl = job.keys.copy_drops_items();
    let crash = match f3c::crash_key(&job.keys, Naming::Modern) {
        Some(InputId::Key(scancode)) => Some(u32::from(scancode)),
        _ => None,
    };
    let watched = [
        Some(codes.modifier.0),
        Some(codes.copy.0),
        crash,
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
        // SAFETY: SDL が SDL_GL_SwapWindow に渡したウィンドウを、そのスレッドで。
        .map_or(0, |get| unsafe { get(window) });
    if id == 0 {
        return Err(Failure::NoCallback);
    }
    Ok(burst_events(id, codes.modifier, codes.copy, job.purpose))
}

fn real_keyboard_state() -> Option<GetKeyboardStateFn> {
    GET_KEYBOARD_STATE_EXPORT.get().copied()
}

/// SDL 自身のキーの状態（スキャンコードごとに C の bool 1 つ。物理的に押しているもの）。SDL が
/// 答えられなければ `None`。すぐに読むこと：SDL はイベントを汲み上げるたびに更新する。
fn real_keys() -> Option<&'static [u8]> {
    let get = real_keyboard_state()?;
    let mut count: c_int = 0;
    // SAFETY: SDL は配列の長さを書き、自分の配列を返す。直接呼ぶので、置き換えのフックには見えない。
    let state = unsafe { get(&mut count) };
    if state.is_null() {
        return None;
    }
    let count = usize::try_from(count).unwrap_or(0);
    // SAFETY: `count` 個の 1 バイトの bool の SDL の配列。SDL と同じだけ生きる。
    Some(unsafe { std::slice::from_raw_parts(state.cast::<u8>(), count) })
}

/// ゲームから見たキーの状態：SDL の配列に、置き換えが押さえているか隠しているキーの判断を
/// 当てはめたもの（[`rebind_state`]）。Minecraft はキーを読むたびに配列を聞き直すので、
/// いま直した写しがゲームの読むもの。
unsafe extern "C" fn get_keyboard_state_hook(numkeys: *mut c_int) -> *const bool {
    static USED: AtomicBool = AtomicBool::new(false);
    first_use(&USED, "SDL_GetKeyboardState");
    let Some(original) = GET_KEYBOARD_STATE.get() else {
        return std::ptr::null();
    };
    let mut count: c_int = 0;
    // SAFETY: SDL は長さをこちらの変数に書き、自分の配列を返す。
    let state = unsafe { original(&mut count) };
    if !numkeys.is_null() {
        // SAFETY: 呼び出し側のポインタ（SDL は null でもよいとする）。
        unsafe { *numkeys = count };
    }
    if state.is_null() || !rebind_state::any_forced() {
        return state;
    }
    // こちらより長い配列には触れない（ゲームは伝えられた長さを超えて読んではならない）。
    let Some(count) = usize::try_from(count)
        .ok()
        .filter(|&n| n > 0 && n <= SCANCODE_COUNT)
    else {
        return state;
    };
    // SAFETY: `count` 個の 1 バイトの bool の SDL の配列。
    let real = unsafe { std::slice::from_raw_parts(state.cast::<u8>(), count) };
    let out = &SPOOFED_KEYS[..count];
    if !rebind_state::patch_forced(real, out) {
        spoof_keys(real, out, rebind_state::forced);
    }
    SPOOFED_KEYS.as_ptr().cast()
}

/// キーの状態 `real` を `out` に写し、各キーについて `forced` の判断を当てはめる。
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

/// `window_id` のウィンドウへの、修飾キーを下ろす、コピーを下ろす、コピーを上げる、修飾キーを
/// 上げる。キーは (スキャンコード, キーコード)。Minecraft はスキャンコードで割り当てを照らし合わせる。
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

/// SDL が報告するのと同じキーのイベント。修飾キーなし（押している Ctrl で Ctrl+C にしては
/// いけない）、リピートでない、特定のキーボードからでない（`which` 0）、時刻なし（Minecraft は
/// どちらも読まない）。
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

/// 印の付いたイベントを渡す：コピーのキーの押下で、ゲームのクリップボードへの書き込みが
/// 自分のものになる窓を開き、修飾キーの解放で閉じる。INJECTED をロックしたまま動く。
fn on_injected(queue: &mut VecDeque<(Event, Tag)>, event: &Event, tag: Tag) {
    match tag {
        Tag::Plain => {}
        Tag::CopyDown(purpose) => f3c::begin_injected(purpose),
        Tag::ModifierUp(purpose) => {
            let hooked = SET_CLIPBOARD_TEXT.get().is_some();
            if let Some(reason) = after_modifier_up(queue, event, f3c::end_injected(), hooked) {
                f3c::fail(purpose, reason);
            }
        }
    }
}

/// 修飾キーの解放（`up`）を出すときに、ゲームがコピーしたもので一連の送信を終わらせる。
/// 断られた場合、その解放がデバッグの画面を切り替える：同じポーリングのループの中で、すぐ後に
/// 修飾キーをもう 1 回押して離すので、切り替えが見えるフレームはない。LWJGL がまだ受け取って
/// いないクリップボードのフック（`clipboard_hooked` が false）を通って何も来なかったのは、
/// 断られたのではない。
fn after_modifier_up(
    queue: &mut VecDeque<(Event, Tag)>,
    up: &Event,
    copied: Injected,
    clipboard_hooked: bool,
) -> Option<Failure> {
    match copied {
        Injected::Captured => None,
        // ゲームはキーを処理した。その文字が座標でなかっただけ。
        Injected::Written => Some(Failure::Unreadable),
        Injected::Nothing if !clipboard_hooked => Some(Failure::NoHooks),
        Injected::Nothing => {
            let mut down = *up;
            down[..4].copy_from_slice(&EVENT_KEY_DOWN.to_ne_bytes());
            down[OFF_KEY_DOWN] = 1;
            // 逆の順に入れるので、DOWN、UP の順に出る。
            queue.push_front((*up, Tag::Plain));
            queue.push_front((down, Tag::Plain));
            Some(Failure::Refused)
        }
    }
}

/// 自前の F3+C で Minecraft がコピーする座標をクリップボードに入れず（成功と答える。そうしないと
/// ゲームがエラーをログに出す）、利用者自身の F3+C を見る。
unsafe extern "C" fn set_clipboard_text_hook(text: *const c_char) -> bool {
    let Some(original) = SET_CLIPBOARD_TEXT.get() else {
        return false;
    };
    // ほかの書き込み（ゲーム自身のコピー）は読まずに通す。
    let ours = !text.is_null()
        && f3c::watching()
        && crate::catch("SDL_SetClipboardText hook", || {
            // SAFETY: NUL で終わる UTF-8 で、この呼び出しの間だけ有効（LWJGL はスタックのバッファを
            // 渡す）。戻る前にここで読む。
            f3c::on_clipboard(&unsafe { CStr::from_ptr(text) }.to_string_lossy())
        })
        .unwrap_or(false);
    if ours {
        return true;
    }
    // SAFETY: 呼び出し側の引数。
    unsafe { original(text) }
}

/// イベントを router に渡す。キーの置き換えはそれをその場で書き換える。
///
/// # Safety
/// `event` は、呼び出し側が書いてよい完全な SDL_Event を指していなければならない。
unsafe fn route_event(event: *mut u8) -> Route {
    let Some(api) = API.get() else {
        return Route::Forward;
    };
    // SAFETY: 読むフィールドはどれも 128 バイトの SDL_Event の中。
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
        let density = f32::from_bits(DENSITY.load(Ordering::Relaxed));
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
                    // ゲームがイベントを受け取る（キーの状態を読むかもしれない）前に。
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
            // IME の変換中の文字はまだ見せない。入力している間はゲームに渡さない。
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
                // クリックから作ったキーは、クリックと同じく今押している修飾キーを受け取る（SAFETY:
                // SDL_GetModState は引数を取らない）。
                let real_mods = || api.get_mod_state.map_or(0, |get| get());
                deliver(&mut *event.cast::<Event>(), delivery, || {
                    adjust_mod(real_mods())
                })
            }
            EVENT_MOUSE_WHEEL => {
                // direction 1 = 反転（ナチュラルスクロール）。
                let sign = if read_u32(OFF_WHEEL_DIRECTION) == 1 {
                    -1.0
                } else {
                    1.0
                };
                router().scroll(read_f32(OFF_WHEEL_X) * sign, read_f32(OFF_WHEEL_Y) * sign)
            }
            EVENT_WINDOW_FOCUS_LOST => {
                // SDL3 は、押しているキーの解放をこのイベントの前後に送り（スモークではその前に見えた）、
                // ボタンのものは送らない。router はこの後で、置き換えた元のキーのものを捨てる：その出力は
                // ここで、次のポーリングで離す。どちらの順でも、最後には全部離れる。
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

/// ゲームがこれから受け取るキーかボタンのイベントについての router の判断を行う：`Send` なら
/// 出力に書き換える。キーのイベントは、修飾キーのビットとして `key_mod()` を受け取る。
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

/// キーかマウスのボタンのイベントを、ウィンドウと時刻を残したまま、その場で `output` に
/// 書き換える。ゲームに渡してはいけなければ false（マウスのボタンはリピートしない：ゲームは
/// リピートを押下と受け取ってしまう）。キーは `key_mod()` を受け取る。キーから作ったキーは
/// キーボードと生のコードを、ボタンから作ったボタンはマウスとクリックの数を残す。
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

/// ゲームに渡すキーのイベントの修飾キーのビット：置き換えが修飾キーについて判断を持つ間は、
/// そのビットはゲームが読むキーの状態に従う（Ctrl に置き換えたキーは Ctrl のビットを立て、
/// F3 に置き換えた Ctrl はそれを消すので、F3+B が Ctrl+B にならない）。
fn adjust_mod(mods: u16) -> u16 {
    if !rebind_state::modifiers_forced() {
        return mods;
    }
    patch_mod(mods, rebind_state::forced)
}

/// `mods` の各修飾キーのビットを、`forced` の判断どおりに立てるか消したもの。
fn patch_mod(mods: u16, forced: impl Fn(InputId) -> Option<bool>) -> u16 {
    MODIFIER_BITS.iter().fold(mods, |mods, &(scancode, bit)| {
        match forced(InputId::Key(scancode)) {
            Some(true) => mods | bit,
            Some(false) => mods & !bit,
            None => mods,
        }
    })
}

/// `window_id` のウィンドウで、それだけでゲームに `output` を渡すイベント（フォーカスを
/// 失ったときか安全網からの解放）。マウスのボタンのリピートなら `None`。
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

/// SDL が報告するのと同じマウスのボタンのイベント。特定のマウスからでなく（`which` 0）、
/// 時刻も位置もない（Minecraft はどれも読まない）。
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

/// `events` を、キューにあるものより先に渡す。INJECTED を取る：router を持たずに呼ぶこと。
fn inject_first(events: Vec<Event>) {
    if events.is_empty() {
        return;
    }
    let mut injected = INJECTED.lock().unwrap_or_else(|e| e.into_inner());
    for event in events.into_iter().rev() {
        injected.push_front((event, Tag::Plain));
    }
}

/// `window` のスワップの後の、安全網の解放（[`rebind_state::lost_releases`]）：ゲームは
/// 次のポーリングでそれを受け取る。
pub fn release_lost_keys(window: *mut c_void) {
    let Some(get_id) = API.get().and_then(|api| api.get_window_id) else {
        return;
    };
    let releases = rebind_state::lost_releases(window as usize);
    if releases.is_empty() {
        return;
    }
    // SAFETY: SDL が SDL_GL_SwapWindow に渡したウィンドウを、そのスレッドで。
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

/// SDL3 のスキャンコードのキー。キーの id の外なら `None`（SDL3 に 4 より下のキーはない）。
fn input_key(scancode: u32) -> Option<InputId> {
    u16::try_from(scancode)
        .ok()
        .filter(|&sc| (4..SCANCODE_COUNT as u16).contains(&sc))
        .map(InputId::Key)
}

/// SDL が大きさの変更を報告するのと同じ形で、ズームが望む大きさをゲームに伝える：新しい
/// ピクセルの大きさ、次にウィンドウの大きさ（拡大のない画面では同じ。Minecraft はどちらを
/// 読むこともある）。
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
    // SAFETY: SDL が SDL_GL_SwapWindow に渡したウィンドウ。
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

/// `pixels` に合うウィンドウの大きさ（SDL のウィンドウの座標で）。本当のウィンドウの大きさと
/// ピクセルの大きさの比と同じ割合で縮める。
fn window_size_for(api: &Api, window: *mut c_void, [w, h]: [i32; 2]) -> (i32, i32) {
    // SAFETY: 上と同じ。
    let density = crate::tall::own_query(|| unsafe { (api.get_pixel_density)(window) });
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

/// ゲームが `window` でマウスを相対モードにしているか（プレイ中か）。
pub fn captured(window: *mut c_void) -> bool {
    API.get().is_some_and(|api| {
        // SAFETY: SDL が渡したウィンドウを、そのスレッドで。
        unsafe { (api.get_relative_mouse_mode)(window) }
    })
}

/// フレームに 1 回、`window` のスワップの後に（そのイベントをポーリングするスレッドで）：
/// ピクセルの密度と、UI が開いたか閉じたなら文字の入力。
pub fn after_swap(window: *mut c_void) {
    if let Some(api) = API.get()
        && !window.is_null()
    {
        // SAFETY: SDL が SDL_GL_SwapWindow に渡したウィンドウを、そのスレッドで。
        let density = crate::tall::own_query(|| unsafe { (api.get_pixel_density)(window) });
        let density = if density.is_finite() && density > 0.0 {
            density
        } else {
            1.0
        };
        DENSITY.store(density.to_bits(), Ordering::Relaxed);
    }
    update_text_input();
}

/// SDL3 は、文字の入力がオンの間だけ文字を送る（IME も動かす）。UI のためにオンにし、UI を
/// 閉じたらゲームが最後に頼んだ状態に戻す（追えていなければ、ゲームがオフにしていたならオフ）。
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
    UI_TEXT_INPUT.store(open, Ordering::Relaxed);
    if window.is_null() {
        return;
    }
    let followed = START_TEXT_INPUT.get().zip(STOP_TEXT_INPUT.get());
    // SAFETY: このスレッド（イベントのスレッド）のイベントで SDL が報告したウィンドウ。本物の
    // 関数は export のシグネチャを持つ。
    unsafe {
        match followed {
            Some((start, stop)) => {
                let active = (api.text_input_active)(window);
                if open || GAME_TEXT_INPUT.load(Ordering::Relaxed) {
                    if !active {
                        start(window);
                    }
                } else if active {
                    stop(window);
                }
            }
            None if open => {
                if !(api.text_input_active)(window) && (api.start_text_input)(window) {
                    TEXT_INPUT_OURS.store(true, Ordering::Relaxed);
                }
            }
            None => {
                if TEXT_INPUT_OURS.swap(false, Ordering::Relaxed) {
                    (api.stop_text_input)(window);
                }
            }
        }
    }
}

/// ゲーム（か SDL）が文字の入力をオンにする：UI を閉じたときのために覚える。
unsafe extern "C" fn start_text_input_hook(window: *mut c_void) -> bool {
    GAME_TEXT_INPUT.store(true, Ordering::Relaxed);
    let Some(original) = START_TEXT_INPUT.get() else {
        return false;
    };
    // SAFETY: 呼び出し側の引数。
    unsafe { original(window) }
}

/// ゲームが文字の入力をオフにする：覚えておき、UI が開いている間は閉じるまで延ばす（UI の
/// 入力欄にはそれが要る）。
unsafe extern "C" fn stop_text_input_hook(window: *mut c_void) -> bool {
    GAME_TEXT_INPUT.store(false, Ordering::Relaxed);
    if UI_TEXT_INPUT.load(Ordering::Relaxed) {
        return true;
    }
    let Some(original) = STOP_TEXT_INPUT.get() else {
        return false;
    };
    // SAFETY: 呼び出し側の引数。
    unsafe { original(window) }
}

fn modifiers(mods: u16) -> Modifiers {
    input::modifiers(
        mods & KMOD_SHIFT != 0,
        mods & KMOD_CTRL != 0,
        mods & KMOD_ALT != 0,
    )
}

/// SDL3 のキーコードの egui の名前（入力欄とホットキーが使えるキー）。キーコードはキーボードの
/// 配列に従う。
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
        0x0D | 0x4000_0058 => Key::Enter, // Return、テンキーの Enter
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
        0x2D | 0x4000_0056 => Key::Minus,  // テンキーの -
        0x2E | 0x4000_0063 => Key::Period, // テンキーの .
        0x2F | 0x4000_0054 => Key::Slash,  // テンキーの /
        0x3A => Key::Colon,
        0x3B => Key::Semicolon,
        0x3D => Key::Equals,
        0x5B => Key::OpenBracket,
        0x5C => Key::Backslash,
        0x5D => Key::CloseBracket,
        0x60 => Key::Backtick,
        0x4000_0057 => Key::Plus, // テンキーの +
        0x4000_0059..=0x4000_0061 => DIGITS[(key - 0x4000_0059 + 1) as usize], // テンキーの 1〜9
        0x4000_0062 => Key::Num0, // テンキーの 0
        _ => return None,
    })
}

/// 上の選り分けが覆わない入力の関数が最初に使われたら、ログに出す。
fn first_use(flag: &AtomicBool, name: &str) {
    if !flag.swap(true, Ordering::Relaxed) {
        log::info!("SDL3 input: the game uses {name}");
    }
}

macro_rules! probe {
    ($hook:ident, $original:ident, $name:literal, ($($arg:ident: $ty:ty),*) -> $ret:ty, $default:expr) => {
        unsafe extern "C" fn $hook($($arg: $ty),*) -> $ret {
            static USED: AtomicBool = AtomicBool::new(false);
            first_use(&USED, $name);
            match $original.get() {
                // SAFETY: 呼び出し側の引数をそのまま。
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

/// ゲームがマウスを捕まえる方法なので、変わるたびにログに出す。
unsafe extern "C" fn set_relative_mouse_mode_probe(window: *mut c_void, enabled: bool) -> bool {
    static COUNT: AtomicUsize = AtomicUsize::new(0);
    if COUNT.fetch_add(1, Ordering::Relaxed) < 20 {
        log::info!("SDL3 input: SDL_SetWindowRelativeMouseMode({enabled})");
    }
    match SET_RELATIVE_MOUSE_MODE.get() {
        // SAFETY: 呼び出し側の引数をそのまま。
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

    /// 各イベントの (種類, スキャンコード, 押している)（並びを比べるため）。
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
        // 時刻、キーボード、修飾キー、生のコードなし。リピートでない。
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

    /// ポーリングのフックと同じようにキューを渡し、`copied` で一連の送信を終わらせる。
    fn hand_out(mut queue: VecDeque<(Event, Tag)>, copied: Injected) -> (Vec<Event>, Vec<Failure>) {
        let (mut out, mut failures) = (Vec::new(), Vec::new());
        while let Some((event, tag)) = queue.pop_front() {
            if let Tag::ModifierUp(_) = tag {
                failures.extend(after_modifier_up(&mut queue, &event, copied, true));
            }
            out.push(event);
        }
        (out, failures)
    }

    #[test]
    fn nothing_copied_without_the_clipboard_hook_is_no_refusal() {
        let mut queue: VecDeque<_> = burst_events(ID, F3, C, Purpose::Refresh).into();
        let (mut out, mut failures) = (Vec::new(), Vec::new());
        while let Some((event, tag)) = queue.pop_front() {
            if let Tag::ModifierUp(_) = tag {
                failures.extend(after_modifier_up(
                    &mut queue,
                    &event,
                    Injected::Nothing,
                    false,
                ));
            }
            out.push(event);
        }
        assert_eq!(failures, [Failure::NoHooks]);
        // 戻さない：デバッグの画面は切り替わっていない。
        assert_eq!(out.len(), 4);
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
                // 戻し：DOWN が UP より先で、どちらも後でキューに入ったものより先。
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

    /// SDL が埋めるのと同じキーのイベント：時刻、キーボード、修飾キー、生のコード付き。
    fn physical_key(down: bool, repeat: bool, (scancode, keycode): (u32, u32)) -> Event {
        let mut e = key_event(down, ID, (scancode, keycode));
        put(&mut e, 8, &TIMESTAMP.to_ne_bytes());
        put(&mut e, OFF_WHICH, &7u32.to_ne_bytes());
        put(&mut e, OFF_KEY_MOD, &0x1000u16.to_ne_bytes()); // Num Lock
        put(&mut e, 34, &0x2Eu16.to_ne_bytes()); // 生のコード
        e[OFF_KEY_REPEAT] = u8::from(repeat);
        e
    }

    /// SDL が埋めるのと同じボタンのイベント：時刻、マウス、クリックの数、位置付き。
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
        // 残り（ウィンドウ、時刻、キーボード、生のコード）はそのまま。
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
        // ボタンはリピートしない：リピートは捨てる。
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
        // そのまま渡すキーは、ゲームが読む修飾キーを受け取る。ボタンには触れない。
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
            InputId::Key(224) => Some(true),  // ルールが押さえている左の Ctrl
            InputId::Key(225) => Some(false), // ルールが取った左の Shift
            _ => None,
        };
        // 右の Shift と Num Lock はそのまま。
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
        real[26] = 1; // W：物理的に押していて、取られている
        real[6] = 1; // C：物理的に押している
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
