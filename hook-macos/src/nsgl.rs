//! macOS の `NSOpenGLContext` の OpenGL のコンテキスト。[`crate::macos`] のため（エージェントは
//! これで、ゲームのビューの上に自前のコンテキストを持つ）。
//!
//! GLFW と SDL3 は `makeCurrentContext` でコンテキストを今のものにするので、今の
//! `NSOpenGLContext` はゲームのもので、そのビューが描画先（「サーフェス」）。エージェントの
//! コンテキストはゲームのコンテキストのピクセルフォーマットで作り、同じビューに付ける：1 つの
//! ビューに付けたコンテキストは同じサーフェスに描くので、オーバーレイはゲームが見せるバック
//! バッファに載る。AppKit はビューをメインスレッドで触ることを求め、macOS の GLFW と SDL3 は
//! そこで描く（Minecraft は JVM を `-XstartOnFirstThread` で起動する）。
//!
//! ハンドルは `NSOpenGLContext` と `NSView` のポインタ。GL の関数は OpenGL.framework の export
//! から引く（CGL には `GetProcAddress` がない）。

use std::ffi::{CStr, c_int, c_void};
use std::sync::{Mutex, OnceLock};

use crate::dl;
use crate::objc::{self, Id, NIL, Rect, Selector};

const NS_OPENGL_PFA_DOUBLE_BUFFER: u32 = 5;
const NS_OPENGL_PFA_COLOR_SIZE: u32 = 8;
const NS_OPENGL_PFA_ALPHA_SIZE: u32 = 11;
const NS_OPENGL_PFA_DEPTH_SIZE: u32 = 12;
const NS_OPENGL_PFA_STENCIL_SIZE: u32 = 13;
const NS_OPENGL_PFA_SAMPLES: u32 = 56;
const NS_OPENGL_PFA_ACCELERATED: u32 = 73;
const NS_OPENGL_PFA_OPENGL_PROFILE: u32 = 99;

static CURRENT_CONTEXT: Selector = Selector::new(c"currentContext");
static CLEAR_CURRENT_CONTEXT: Selector = Selector::new(c"clearCurrentContext");
static MAKE_CURRENT_CONTEXT: Selector = Selector::new(c"makeCurrentContext");
static VIEW: Selector = Selector::new(c"view");
static SET_VIEW: Selector = Selector::new(c"setView:");
static CONTENT_VIEW: Selector = Selector::new(c"contentView");
static PIXEL_FORMAT: Selector = Selector::new(c"pixelFormat");
static ALLOC: Selector = Selector::new(c"alloc");
static INIT_WITH_FORMAT: Selector = Selector::new(c"initWithFormat:shareContext:");
static UPDATE: Selector = Selector::new(c"update");
static CLEAR_DRAWABLE: Selector = Selector::new(c"clearDrawable");
static RELEASE: Selector = Selector::new(c"release");
static BOUNDS: Selector = Selector::new(c"bounds");
static WINDOW: Selector = Selector::new(c"window");
static BACKING_SCALE_FACTOR: Selector = Selector::new(c"backingScaleFactor");
static GET_VALUES: Selector = Selector::new(c"getValues:forAttribute:forVirtualScreen:");
static WANTS_LAYER: Selector = Selector::new(c"wantsLayer");

unsafe extern "C" {
    fn pthread_main_np() -> c_int;
}

/// エージェントのコンテキストの 1 つと、最後にビューに合わせたときのビューの大きさ。
struct Own {
    context: usize,
    view: usize,
    size: Option<(f64, f64)>,
}

/// エージェントのコンテキスト（描くウィンドウごとに 1 つ）。
static OWN: Mutex<Vec<Own>> = Mutex::new(Vec::new());

fn opengl() -> Option<usize> {
    static OPENGL: OnceLock<Option<usize>> = OnceLock::new();
    *OPENGL.get_or_init(|| {
        dl::loaded(c"/System/Library/Frameworks/OpenGL.framework/OpenGL").or_else(|| {
            dl::loaded(c"/System/Library/Frameworks/OpenGL.framework/Versions/A/OpenGL")
        })
    })
}

fn context_class() -> Result<Id, String> {
    let class = objc::class(c"NSOpenGLContext");
    if class.is_null() {
        Err("AppKit is not loaded".into())
    } else {
        Ok(class)
    }
}

/// 今の `NSOpenGLContext`（なければ 0）。
pub fn current_context() -> usize {
    let Ok(class) = context_class() else {
        return 0;
    };
    // SAFETY: オブジェクトか nil を答えるクラスメソッド。
    objc::autoreleased(|| unsafe { objc::send::<Id>(class, &CURRENT_CONTEXT) as usize })
}

/// このスレッドで今のビューとコンテキスト（なければ 0）。
pub fn current() -> (usize, usize) {
    let context = current_context();
    if context == 0 {
        return (0, 0);
    }
    // SAFETY: NSOpenGLContext。そのビューは nil のことがある。
    let view = objc::autoreleased(|| unsafe { objc::send::<Id>(context as Id, &VIEW) as usize });
    (view, context)
}

/// ゲームが描くビュー：今のコンテキストのもの、なければ `window`（`NSWindow`）のコンテンツ
/// ビュー。
pub fn window_view(window: Option<usize>) -> usize {
    match current() {
        (0, _) => window.map_or(0, |window| {
            // SAFETY: GLFW と SDL3 の NSWindow。
            objc::autoreleased(|| unsafe { objc::send::<Id>(window as Id, &CONTENT_VIEW) as usize })
        }),
        (view, _) => view,
    }
}

/// GL の関数。OpenGL.framework が export していなければ null。
pub fn function(name: &CStr) -> *const c_void {
    opengl()
        .and_then(|gl| dl::symbol(gl, name))
        .unwrap_or(std::ptr::null())
}

/// 別のコンテキストが同じビューに描く前に、今のコンテキストのコマンドを送り出す。
fn flush() {
    static GL_FLUSH: OnceLock<Option<unsafe extern "C" fn()>> = OnceLock::new();
    let flush = GL_FLUSH.get_or_init(|| {
        let p = function(c"glFlush");
        // SAFETY: glFlush の型。
        (!p.is_null())
            .then(|| unsafe { std::mem::transmute::<*const c_void, unsafe extern "C" fn()>(p) })
    });
    if let Some(flush) = flush {
        // SAFETY: コンテキストが今のものになっている。
        unsafe { flush() };
    }
}

/// メインスレッド（AppKit のビューを触ってよい唯一のスレッド）か。
pub fn on_main_thread() -> bool {
    // SAFETY: 前提はない。
    unsafe { pthread_main_np() != 0 }
}

/// `view` のための、ゲームの今のコンテキストのピクセルフォーマットのコンテキスト。
pub fn create_context(view: usize) -> Result<usize, String> {
    if !on_main_thread() {
        return Err("not on the main thread (AppKit needs it to attach to a view)".into());
    }
    let class = context_class()?;
    let game = current_context();
    if game == 0 {
        return Err("no NSOpenGLContext is current".into());
    }
    objc::autoreleased(|| {
        // SAFETY: NSOpenGLContext のメソッド。フォーマットはゲームのコンテキストのもので、それが
        // 生きている間は生きている。
        unsafe {
            let format = objc::send::<Id>(game as Id, &PIXEL_FORMAT);
            if format.is_null() {
                return Err("the game's context has no pixel format".into());
            }
            let allocated = objc::send::<Id>(class, &ALLOC);
            let context = objc::send2::<Id, Id, Id>(allocated, &INIT_WITH_FORMAT, format, NIL);
            if context.is_null() {
                return Err("initWithFormat:shareContext: failed".into());
            }
            objc::send1::<Id, ()>(context, &SET_VIEW, view as Id);
            let attached = objc::send::<Id>(context, &VIEW) as usize;
            if attached != view {
                objc::send::<()>(context, &RELEASE);
                return Err(format!("cannot attach to the view 0x{view:x}"));
            }
            // コンテキストがレイヤーで裏打ちしたビューのサーフェスを共有するかは、この試作で確かめる
            // こと。
            log::info!(
                "nsgl: overlay context 0x{:x} on view 0x{view:x} (layer-backed {})",
                context as usize,
                objc::send::<u8>(view as Id, &WANTS_LAYER) != 0
            );
            OWN.lock().unwrap_or_else(|e| e.into_inner()).push(Own {
                context: context as usize,
                view,
                size: None,
            });
            Ok(context as usize)
        }
    })
}

/// ビューのピクセルでの大きさ（境界にウィンドウのバッキングの倍率を掛けたもの）。
fn view_size(view: usize) -> (f64, f64) {
    objc::autoreleased(|| {
        // SAFETY: NSView。そのウィンドウは nil のことがある（そのとき倍率は 0）。
        unsafe {
            let bounds: Rect = objc::send_rect(view as Id, &BOUNDS);
            let window = objc::send::<Id>(view as Id, &WINDOW);
            let scale = if window.is_null() {
                0.0
            } else {
                objc::send::<f64>(window, &BACKING_SCALE_FACTOR)
            };
            (bounds.width * scale, bounds.height * scale)
        }
    })
}

/// `context` を今のものにする（0：なし）。エージェント自前のコンテキストは、GLFW と SDL3 が
/// 自分のものにするように（`update`）、先にビューの大きさに合わせる。
pub fn make_current(_view: usize, context: usize) -> Result<(), String> {
    let class = context_class()?;
    let current = current_context();
    if current != 0 && current != context {
        flush();
    }
    // SAFETY: ゲームかエージェントのコンテキストへの NSOpenGLContext のメソッド。
    unsafe {
        if context == 0 {
            objc::send::<()>(class, &CLEAR_CURRENT_CONTEXT);
            return Ok(());
        }
        let mut own = OWN.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(own) = own.iter_mut().find(|own| own.context == context) {
            // ビューに合わせるのは AppKit の仕事。ほかのスレッドのスワップではオーバーレイを省く。
            if !on_main_thread() {
                return Err("not on the main thread".into());
            }
            let size = view_size(own.view);
            if own.size != Some(size) {
                objc::send::<()>(context as Id, &UPDATE);
                own.size = Some(size);
            }
        }
        drop(own);
        objc::send::<()>(context as Id, &MAKE_CURRENT_CONTEXT);
    }
    if current_context() != context {
        return Err("makeCurrentContext did not take".into());
    }
    Ok(())
}

/// エージェントのコンテキストの 1 つ（今のものでない）を消す。メインスレッドの外では残す
/// （ビューから外すのは AppKit の仕事）。
pub fn delete_context(context: usize) -> Result<(), String> {
    if !on_main_thread() {
        return Err("not on the main thread; the context is left".into());
    }
    OWN.lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|own| own.context != context);
    // SAFETY: エージェントのコンテキスト。1 回だけ解放する。
    unsafe {
        objc::send::<()>(context as Id, &CLEAR_DRAWABLE);
        objc::send::<()>(context as Id, &RELEASE);
    }
    Ok(())
}

/// 今のコンテキストのピクセルフォーマット（ログ用）。
pub fn describe(view: usize) -> String {
    let context = current_context();
    if context == 0 {
        return format!("view 0x{view:x}");
    }
    objc::autoreleased(|| {
        // SAFETY: 今の NSOpenGLContext とそのピクセルフォーマット。
        unsafe {
            let format = objc::send::<Id>(context as Id, &PIXEL_FORMAT);
            if format.is_null() {
                return format!("view 0x{view:x}");
            }
            let value = |attribute: u32| {
                let mut value: i32 = 0;
                objc::send3::<*mut i32, u32, i32, ()>(
                    format,
                    &GET_VALUES,
                    &mut value,
                    attribute,
                    0,
                );
                value
            };
            let profile = match value(NS_OPENGL_PFA_OPENGL_PROFILE) {
                0x1000 => "legacy".to_owned(),
                0x3200 => "3.2 core".to_owned(),
                0x4100 => "4.1 core".to_owned(),
                other => format!("0x{other:x}"),
            };
            format!(
                "NSOpenGL view 0x{view:x}: color {} alpha {} depth {} stencil {}, samples {}, \
                 profile {profile}, accelerated {}, double-buffered {}",
                value(NS_OPENGL_PFA_COLOR_SIZE),
                value(NS_OPENGL_PFA_ALPHA_SIZE),
                value(NS_OPENGL_PFA_DEPTH_SIZE),
                value(NS_OPENGL_PFA_STENCIL_SIZE),
                value(NS_OPENGL_PFA_SAMPLES),
                value(NS_OPENGL_PFA_ACCELERATED) != 0,
                value(NS_OPENGL_PFA_DOUBLE_BUFFER) != 0
            )
        }
    })
}
