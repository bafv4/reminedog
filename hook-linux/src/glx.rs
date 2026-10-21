//! GLX：X11 の OpenGL のコンテキスト。[`crate::linux`] のため（エージェントはこれで、ゲームの
//! 描画先の上に自前のコンテキストを持つ）。
//!
//! libGL と libX11 はゲームのもの：GLFW と SDL3 が読み込み、エージェントは読み込まれてから関数を
//! 引く。エージェント自前のコンテキストはゲームのコンテキストのフレームバッファの設定で作るので、
//! ゲームの描画先で今のものにできる。Xlib の既定のエラー処理はプロセスを終えるので、エージェントの
//! 失敗しうる GLX の呼び出しは自前のエラー処理の下で行う（[`trap_x_errors`]）。

use std::ffi::{CStr, c_int, c_ulong, c_void};
use std::sync::atomic::{AtomicI32, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

use crate::dl;

type Display = c_void;
type GlxContext = *mut c_void;
type GlxFbConfig = *mut c_void;
/// X のリソースの id（`XID`。ここでは GLX の描画先）。
type Xid = c_ulong;
type Bool = c_int;

const GLX_DOUBLEBUFFER: c_int = 5;
const GLX_BUFFER_SIZE: c_int = 2;
const GLX_ALPHA_SIZE: c_int = 11;
const GLX_DEPTH_SIZE: c_int = 12;
const GLX_STENCIL_SIZE: c_int = 13;
const GLX_SCREEN: c_int = 0x800C;
const GLX_FBCONFIG_ID: c_int = 0x8013;
const GLX_RGBA_TYPE: c_int = 0x8014;

/// Xlib の `XErrorEvent`（エラー処理が読むところまで）。
#[repr(C)]
struct XErrorEvent {
    kind: c_int,
    display: *mut Display,
    resource_id: Xid,
    serial: c_ulong,
    error_code: u8,
    request_code: u8,
    minor_code: u8,
}

type XErrorHandler = unsafe extern "C" fn(*mut Display, *mut XErrorEvent) -> c_int;

struct Glx {
    get_current_context: unsafe extern "C" fn() -> GlxContext,
    get_current_drawable: unsafe extern "C" fn() -> Xid,
    get_current_display: unsafe extern "C" fn() -> *mut Display,
    query_context: unsafe extern "C" fn(*mut Display, GlxContext, c_int, *mut c_int) -> c_int,
    choose_fb_config:
        unsafe extern "C" fn(*mut Display, c_int, *const c_int, *mut c_int) -> *mut GlxFbConfig,
    get_fb_config_attrib:
        unsafe extern "C" fn(*mut Display, GlxFbConfig, c_int, *mut c_int) -> c_int,
    create_new_context:
        unsafe extern "C" fn(*mut Display, GlxFbConfig, c_int, GlxContext, Bool) -> GlxContext,
    make_context_current: unsafe extern "C" fn(*mut Display, Xid, Xid, GlxContext) -> Bool,
    destroy_context: unsafe extern "C" fn(*mut Display, GlxContext),
    get_proc_address: unsafe extern "C" fn(*const u8) -> *const c_void,
    x_free: unsafe extern "C" fn(*mut c_void) -> c_int,
    x_sync: unsafe extern "C" fn(*mut Display, Bool) -> c_int,
    x_set_error_handler: unsafe extern "C" fn(Option<XErrorHandler>) -> Option<XErrorHandler>,
}

static GLX: OnceLock<Result<Glx, String>> = OnceLock::new();
/// ゲームのコンテキストが今のものだったときに見た、ゲームの X のディスプレイ。
static DISPLAY: AtomicUsize = AtomicUsize::new(0);
/// [`trap_x_errors`] が捕まえた最後の X のエラー（0：なし）。
static X_ERROR: AtomicI32 = AtomicI32::new(0);
/// [`trap_x_errors`] を 1 つずつにする（エラー処理はプロセス全体のもの）。
static TRAP: Mutex<()> = Mutex::new(());
/// X のエラーなしで一度今のものにできた（描画先、コンテキスト）の組：今の描画先でそれらを
/// もう一度切り替えるとき（1 フレームに 2 回）は、エラーの捕捉と X サーバーとの往復を省く。
static SWITCHED: Mutex<Vec<(usize, usize)>> = Mutex::new(Vec::new());

fn get() -> Result<&'static Glx, String> {
    GLX.get_or_init(resolve).as_ref().map_err(Clone::clone)
}

fn resolve() -> Result<Glx, String> {
    let gl = dl::loaded(c"libGL.so.1")
        .or_else(|| dl::loaded(c"libGLX.so.0"))
        .ok_or("libGL is not loaded")?;
    let x11 = dl::loaded(c"libX11.so.6").ok_or("libX11 is not loaded")?;
    /// `library` の関数 `name` を、それが埋めるフィールドの型 `F` として。
    ///
    /// # Safety
    /// `F` はその関数の型。
    unsafe fn get<F>(library: usize, name: &CStr) -> Result<F, String> {
        let p =
            dl::symbol(library, name).ok_or_else(|| format!("no {}", name.to_string_lossy()))?;
        // SAFETY: 呼び出し側が保証する。
        Ok(unsafe { std::mem::transmute_copy::<*const c_void, F>(&p) })
    }
    // SAFETY: GLX と Xlib の関数を C のシグネチャで。
    unsafe {
        Ok(Glx {
            get_current_context: get(gl, c"glXGetCurrentContext")?,
            get_current_drawable: get(gl, c"glXGetCurrentDrawable")?,
            get_current_display: get(gl, c"glXGetCurrentDisplay")?,
            query_context: get(gl, c"glXQueryContext")?,
            choose_fb_config: get(gl, c"glXChooseFBConfig")?,
            get_fb_config_attrib: get(gl, c"glXGetFBConfigAttrib")?,
            create_new_context: get(gl, c"glXCreateNewContext")?,
            make_context_current: get(gl, c"glXMakeContextCurrent")?,
            destroy_context: get(gl, c"glXDestroyContext")?,
            get_proc_address: get(gl, c"glXGetProcAddressARB")?,
            x_free: get(x11, c"XFree")?,
            x_sync: get(x11, c"XSync")?,
            x_set_error_handler: get(x11, c"XSetErrorHandler")?,
        })
    }
}

unsafe extern "C" fn on_x_error(_display: *mut Display, event: *mut XErrorEvent) -> c_int {
    // SAFETY: Xlib はエラーのイベントを渡す。
    let code = if event.is_null() {
        -1
    } else {
        i32::from(unsafe { (*event).error_code })
    };
    X_ERROR.store(code, Ordering::Relaxed);
    0
}

/// `f`（`display` への GLX の呼び出し）を、X のエラーでプロセスを終えずに捕まえて動かす。
fn trap_x_errors<R>(glx: &Glx, display: *mut Display, f: impl FnOnce() -> R) -> Result<R, String> {
    let _serial = TRAP.lock().unwrap_or_else(|e| e.into_inner());
    X_ERROR.store(0, Ordering::Relaxed);
    // SAFETY: Xlib の関数。前のエラー処理は戻す。
    unsafe {
        let previous = (glx.x_set_error_handler)(Some(on_x_error));
        let result = f();
        (glx.x_sync)(display, 0);
        (glx.x_set_error_handler)(previous);
        match X_ERROR.load(Ordering::Relaxed) {
            0 => Ok(result),
            code => Err(format!("X error {code}")),
        }
    }
}

fn display() -> Result<*mut Display, String> {
    let display = DISPLAY.load(Ordering::Acquire) as *mut Display;
    if display.is_null() {
        Err("the game's X display is not known yet".into())
    } else {
        Ok(display)
    }
}

/// このスレッドで今の描画先とコンテキスト（なければ 0）。
pub fn current() -> (usize, usize) {
    let Ok(glx) = get() else {
        return (0, 0);
    };
    // SAFETY: ただの問い合わせ。
    unsafe {
        let context = (glx.get_current_context)();
        if context.is_null() {
            return (0, 0);
        }
        let display = (glx.get_current_display)();
        if !display.is_null() {
            DISPLAY.store(display as usize, Ordering::Release);
        }
        ((glx.get_current_drawable)() as usize, context as usize)
    }
}

/// このスレッドで今のコンテキスト（なければ 0）。
pub fn current_context() -> usize {
    get().map_or(0, |glx| {
        // SAFETY: ただの問い合わせ。
        unsafe { (glx.get_current_context)() as usize }
    })
}

/// 今のコンテキストの GL の関数。コンテキストがなければ null。
pub fn function(name: &CStr) -> *const c_void {
    get().map_or(std::ptr::null(), |glx| {
        // SAFETY: NUL で終わる名前。
        unsafe { (glx.get_proc_address)(name.as_ptr().cast()) }
    })
}

/// ゲームの今のコンテキストのフレームバッファの設定と、そのスクリーン。
fn game_config(glx: &Glx, display: *mut Display) -> Result<GlxFbConfig, String> {
    // SAFETY: `display` の今のコンテキストへの問い合わせ。一覧は XFree で解放する。
    unsafe {
        let context = (glx.get_current_context)();
        if context.is_null() {
            return Err("no GLX context is current (EGL is not supported yet)".into());
        }
        let (mut id, mut screen) = (0, 0);
        (glx.query_context)(display, context, GLX_FBCONFIG_ID, &mut id);
        (glx.query_context)(display, context, GLX_SCREEN, &mut screen);
        let attributes = [GLX_FBCONFIG_ID, id, 0];
        let mut count = 0;
        let configs = (glx.choose_fb_config)(display, screen, attributes.as_ptr(), &mut count);
        if configs.is_null() || count < 1 {
            return Err(format!("no framebuffer configuration {id}"));
        }
        let config = *configs;
        (glx.x_free)(configs.cast());
        Ok(config)
    }
}

/// ゲームの描画先のための、ゲームのコンテキストの設定のコンテキスト。
pub fn create_context(_surface: usize) -> Result<usize, String> {
    let glx = get()?;
    let display = display()?;
    let config = game_config(glx, display)?;
    let context = trap_x_errors(glx, display, || {
        // SAFETY: `display` の設定。直接描画、共有なし。
        unsafe { (glx.create_new_context)(display, config, GLX_RGBA_TYPE, std::ptr::null_mut(), 1) }
    })?;
    if context.is_null() {
        return Err("glXCreateNewContext failed".into());
    }
    Ok(context as usize)
}

/// `context` を `drawable` で今のものにする（0 と 0：なし）。
pub fn make_current(drawable: usize, context: usize) -> Result<(), String> {
    let glx = get()?;
    let display = display()?;
    let switch = || {
        // SAFETY: `display` のコンテキストと、それが描ける描画先、またはなし。
        unsafe {
            (glx.make_context_current)(
                display,
                drawable as Xid,
                drawable as Xid,
                context as GlxContext,
            )
        }
    };
    let pair = (drawable, context);
    let mut switched = SWITCHED.lock().unwrap_or_else(|e| e.into_inner());
    // 捕捉を省くのは今の描画先（スワップのもので、存在する）での切り替えだけ：前に知った組は、
    // その後で消えた描画先を指すことがあり（ゲームの描画先が変わった後のオーバーレイの後片付け）、
    // 捕まえない X のエラーはプロセスを終える。
    // SAFETY: ただの問い合わせ。
    let on_current = drawable != 0 && unsafe { (glx.get_current_drawable)() } as usize == drawable;
    let made = if on_current && switched.contains(&pair) {
        switch()
    } else {
        let made = trap_x_errors(glx, display, switch)?;
        if made != 0 {
            switched.push(pair);
        }
        made
    };
    if made == 0 {
        return Err("glXMakeContextCurrent failed".into());
    }
    Ok(())
}

/// 今のものでないコンテキストを消す。
pub fn delete_context(context: usize) -> Result<(), String> {
    let glx = get()?;
    let display = display()?;
    SWITCHED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|&(_, c)| c != context);
    trap_x_errors(glx, display, || {
        // SAFETY: エージェントのコンテキスト。今のものではない。
        unsafe { (glx.destroy_context)(display, context as GlxContext) }
    })
}

/// ゲームのコンテキストのフレームバッファの設定（ログ用）。
pub fn describe(drawable: usize) -> String {
    let (Ok(glx), Ok(display)) = (get(), display()) else {
        return format!("drawable 0x{drawable:x}");
    };
    let Ok(config) = game_config(glx, display) else {
        return format!("drawable 0x{drawable:x}");
    };
    let attribute = |name| {
        let mut value = 0;
        // SAFETY: `display` の設定。
        unsafe { (glx.get_fb_config_attrib)(display, config, name, &mut value) };
        value
    };
    format!(
        "GLX drawable 0x{drawable:x}: color {} alpha {} depth {} stencil {}, double-buffered {}",
        attribute(GLX_BUFFER_SIZE),
        attribute(GLX_ALPHA_SIZE),
        attribute(GLX_DEPTH_SIZE),
        attribute(GLX_STENCIL_SIZE),
        attribute(GLX_DOUBLEBUFFER) != 0
    )
}
