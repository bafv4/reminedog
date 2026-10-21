//! LWJGL が読み込む GLFW のライブラリと、そのフック。
//!
//! LWJGL は GLFW の export を 1 回だけ引き、覚えたポインタを通して呼ぶので、引くときに
//! フックを渡す（[`crate::jvmti`]）。

use std::ffi::{CStr, c_char, c_int, c_void};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{LazyLock, Mutex, OnceLock};
use std::time::{Duration, Instant};

use reminedog_core::{
    InputId, Naming, glfw_button_of, glfw_key_of, input_from_glfw_button, input_from_glfw_key,
};

use crate::frame::{self, WindowSystem};
use crate::hook::{Hook, Original};
use crate::library::export;

type SwapBuffersFn = unsafe extern "C" fn(window: *mut c_void);
type GetFramebufferSizeFn =
    unsafe extern "C" fn(window: *mut c_void, width: *mut c_int, height: *mut c_int);
type GetWindowContentScaleFn =
    unsafe extern "C" fn(window: *mut c_void, xscale: *mut f32, yscale: *mut f32);
type GetVersionStringFn = unsafe extern "C" fn() -> *const c_char;
type GetWindowAttribFn = unsafe extern "C" fn(window: *mut c_void, attrib: c_int) -> c_int;
/// `glfwGetWin32Window`、`glfwGetX11Window`（`HWND`、X11 の `Window`）。
type NativeWindowFn = unsafe extern "C" fn(window: *mut c_void) -> usize;

const GLFW_ICONIFIED: c_int = 0x0002_0002;
const GLFW_VISIBLE: c_int = 0x0002_0004;

/// エージェント自身が呼ぶ GLFW の関数。
pub struct Glfw {
    pub path: String,
    /// 本当の大きさ（ゲームの呼び出しはフックを通り、ズームの大きさを答える）。
    get_framebuffer_size: GetFramebufferSizeFn,
    /// GLFW 3.3 以降。
    get_window_content_scale: Option<GetWindowContentScaleFn>,
    get_version_string: Option<GetVersionStringFn>,
    get_window_attrib: GetWindowAttribFn,
    /// ネイティブへのアクセスの export（[`crate::os::Os::glfw_native_window`]）。LWJGL のビルドにはある。
    native_window: Option<NativeWindowFn>,
}

impl WindowSystem for Glfw {
    fn framebuffer_size(&self, window: *mut c_void) -> (i32, i32) {
        let (mut width, mut height) = (0, 0);
        // SAFETY: `window` は、GLFW 自身が glfwSwapBuffers に渡したハンドル。
        unsafe { (self.get_framebuffer_size)(window, &mut width, &mut height) };
        (width, height)
    }

    /// GLFW が古くて答えられなければ 1.0。
    fn content_scale(&self, window: *mut c_void) -> f32 {
        let Some(f) = self.get_window_content_scale else {
            return 1.0;
        };
        let (mut x, mut y) = (1.0, 1.0);
        // SAFETY: 上と同じ。
        unsafe { f(window, &mut x, &mut y) };
        if x.is_finite() && x > 0.0 { x } else { 1.0 }
    }

    /// この GLFW がネイティブへのアクセスを export していなければ `None`。
    fn native_window(&self, window: *mut c_void) -> Option<usize> {
        let f = self.native_window?;
        // SAFETY: 上と同じ。
        let native = unsafe { f(window) };
        (native != 0).then_some(native)
    }

    /// OS が答えられないときに、スワップのたびに聞く。X11 の GLFW は X サーバーに聞く（往復に
    /// なる）ので、答えをしばらく覚える。
    fn minimized(&self, window: *mut c_void) -> bool {
        static LAST: Mutex<Option<(usize, Instant, bool)>> = Mutex::new(None);
        const KEPT: Duration = Duration::from_millis(250);
        let now = Instant::now();
        let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((of, at, minimized)) = *last
            && of == window as usize
            && now.duration_since(at) < KEPT
        {
            return minimized;
        }
        // SAFETY: 上と同じ。
        let minimized = unsafe { (self.get_window_attrib)(window, GLFW_ICONIFIED) != 0 };
        *last = Some((window as usize, now, minimized));
        minimized
    }

    fn is_visible(&self, window: *mut c_void) -> bool {
        // SAFETY: 上と同じ。
        unsafe { (self.get_window_attrib)(window, GLFW_VISIBLE) != 0 }
    }

    fn raw_motion(&self, window: *mut c_void) -> bool {
        crate::glfw_input::raw_motion(window)
    }

    fn captured(&self, window: *mut c_void) -> bool {
        crate::glfw_input::captured(window)
    }

    fn naming(&self) -> Naming {
        Naming::Glfw
    }

    fn can_send(&self, id: InputId) -> bool {
        match id {
            InputId::Key(_) => glfw_key_of(id).is_some(),
            InputId::Mouse(_) => glfw_button_of(id).is_some(),
        }
    }

    /// GLFW のコードがそのまま返ってくるキーとボタン。
    fn can_receive(&self, id: InputId) -> bool {
        match id {
            InputId::Key(_) => glfw_key_of(id)
                .is_some_and(|(key, scancode)| input_from_glfw_key(key, scancode) == Some(id)),
            InputId::Mouse(_) => glfw_button_of(id).and_then(input_from_glfw_button) == Some(id),
        }
    }

    fn describe(&self) -> String {
        match self.get_version_string {
            // SAFETY: 静的な NUL で終わる文字列を返す。いつ呼んでもよい。
            Some(f) => format!("GLFW {}", unsafe { CStr::from_ptr(f()) }.to_string_lossy()),
            None => "GLFW".into(),
        }
    }
}

static GLFW: OnceLock<Glfw> = OnceLock::new();
/// フックした GLFW のライブラリ（その前は 0）。
static MODULE: AtomicUsize = AtomicUsize::new(0);
static SWAP_BUFFERS: Original<SwapBuffersFn> = Original::new();
static GET_FRAMEBUFFER_SIZE: Original<GetFramebufferSizeFn> = Original::new();

static HOOKS: LazyLock<[Hook; 2]> = LazyLock::new(|| {
    [
        Hook::new(
            c"glfwSwapBuffers",
            swap_buffers_hook as *const c_void,
            &SWAP_BUFFERS,
        ),
        Hook::new(
            c"glfwGetFramebufferSize",
            get_framebuffer_size_hook as *const c_void,
            &GET_FRAMEBUFFER_SIZE,
        ),
    ]
});

/// ウィンドウの GLFW の関数のフック（入力のものは [`crate::glfw_input`]）。
pub fn hooks() -> &'static [Hook] {
    &*HOOKS
}

/// `module` がフックした GLFW のライブラリか。
pub fn is_hooked(module: usize) -> bool {
    module != 0 && MODULE.load(Ordering::Acquire) == module
}

/// GLFW をファイルの名前ではなく export で見分ける。名前を変えた GLFW や、ランチャーが渡す GLFW
/// （Prism の "use system GLFW"）も見分けられる。
pub fn is_glfw(module: usize) -> bool {
    export(module, c"glfwInit").is_some() && export(module, c"glfwSwapBuffers").is_some()
}

/// GLFW のライブラリをフックしたか。
pub fn attached() -> bool {
    GLFW.get().is_some()
}

/// `module` をフックする（[`crate::library`] が、1 度に 1 つのライブラリについて呼ぶ）。
pub fn attach(module: usize, path: &str) {
    if let Some(existing) = GLFW.get() {
        log::warn!(
            "a second GLFW library is ignored: {path} (hooked: {})",
            existing.path
        );
        return;
    }
    match install(module, path) {
        Ok(glfw) => {
            let _ = GLFW.set(glfw);
            // 準備が全部終わったここで初めてフックしたことにする：ここから、ほかのスレッドもフックを渡す。
            MODULE.store(module, Ordering::Release);
            log::info!("GLFW hooks installed: {path}");
        }
        Err(e) => log::error!("cannot hook GLFW ({path}): {e}"),
    }
}

fn install(module: usize, path: &str) -> Result<Glfw, String> {
    let resolve = |name: &CStr| {
        export(module, name).ok_or_else(|| format!("{} not exported", name.to_string_lossy()))
    };
    // SAFETY: この C のシグネチャの、GLFW の export の関数。
    let glfw = unsafe {
        Glfw {
            path: path.to_owned(),
            get_framebuffer_size: std::mem::transmute::<*const c_void, GetFramebufferSizeFn>(
                resolve(c"glfwGetFramebufferSize")?,
            ),
            get_window_content_scale: export(module, c"glfwGetWindowContentScale")
                .map(|f| std::mem::transmute::<*const c_void, GetWindowContentScaleFn>(f)),
            get_version_string: export(module, c"glfwGetVersionString")
                .map(|f| std::mem::transmute::<*const c_void, GetVersionStringFn>(f)),
            get_window_attrib: std::mem::transmute::<*const c_void, GetWindowAttribFn>(resolve(
                c"glfwGetWindowAttrib",
            )?),
            native_window: crate::os::get()
                .glfw_native_window()
                .and_then(|name| export(module, name))
                .map(|f| std::mem::transmute::<*const c_void, NativeWindowFn>(f)),
        }
    };

    crate::glfw_input::install(module);
    Ok(glfw)
}

unsafe extern "C" fn swap_buffers_hook(window: *mut c_void) {
    crate::catch("glfwSwapBuffers hook", || {
        if let Some(glfw) = GLFW.get() {
            frame::before_swap(glfw, window);
        }
    });
    if let Some(original) = SWAP_BUFFERS.get() {
        let _swapping = crate::tall::Swapping::begin();
        // SAFETY: GLFW の呼び出し側が渡したのと同じ引数。
        unsafe { original(window) };
    }
    // ズームの開始・終了：ゲームはフレームとフレームの間で、すぐに大きさを変える。
    if let Some((window, [w, h])) = crate::tall::take_pending() {
        crate::glfw_input::send_framebuffer_size(window, w, h);
    }
    // ウェイポイントの要求：フレームを出し終わり、ロックを持っていない今、F3+C を送る。
    crate::catch("F3+C", || crate::glfw_input::run_f3c(window));
    crate::catch("rebinds", || crate::glfw_input::release_lost_keys(window));
    crate::catch("hotkeys", crate::rebind_state::forget_lost_presses);
}

/// ズーム中は、ゲームに縦長の大きさを答える。
unsafe extern "C" fn get_framebuffer_size_hook(
    window: *mut c_void,
    width: *mut c_int,
    height: *mut c_int,
) {
    if let Some(original) = GET_FRAMEBUFFER_SIZE.get() {
        // SAFETY: 呼び出し側の引数。
        unsafe { original(window, width, height) };
    }
    if let Some([w, h]) = crate::tall::size_override(window) {
        // SAFETY: GLFW は、どちらのポインタも null でよい。
        unsafe {
            if !width.is_null() {
                *width = w;
            }
            if !height.is_null() {
                *height = h;
            }
        }
    }
}
