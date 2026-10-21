//! 高精細のズーム：ゲームに縦に長いフレームを描かせ、その中央でウィンドウを埋める。
//!
//! Minecraft の視野は縦で決まるので、同じ幅で `k` 倍高いフレームは、1 度あたり `k` 倍の
//! ピクセルを詰め込む。その中央をウィンドウの大きさで見せると、本物の細かさのまま `k` 倍に
//! 拡大した眺めになる（スピードランナーがエンダーアイを測るのに使う "tall" の解像度）。
//! ズームのキーを押している間：
//!
//! 1. ゲームに、フレームバッファがその縦長の大きさだと伝える（GLFW：フレームバッファの大きさの
//!    コールバックと `glfwGetFramebufferSize`。SDL3：ピクセルの大きさのイベントと
//!    `SDL_GetWindowSizeInPixels`）。ゲームは、ほかの大きさの変更と同じく描画先を作り直す。
//! 2. ゲーム自身のウィンドウへの出力（フレームバッファ 0）は、代わりにその大きさの自前の
//!    フレームバッファに行く：LWJGL の `glBindFramebuffer(EXT)` と `glBlitNamedFramebuffer`
//!    のネイティブを、JVM が結び付けるときに差し替え（[`wrap_native`]、[`crate::jvmti`] から
//!    呼ぶ）、0 を自前のものに置き換える。変わるのは、LWJGL を通るゲームの呼び出しだけ。
//! 3. バッファのスワップのときに、自前のフレームバッファの中央の行をウィンドウに写し、
//!    本物のスワップの間はウィンドウのフレームバッファを結び付ける（[`Swapping`]）。
//!
//! `glViewport` も包むが、ゲームが本当に縦長の大きさで描いているかを確かめるためだけ
//! （ズームを始めてから縦長の viewport を一度設定したか）。描いていなければ、ズームは
//! ピクセルを引き伸ばす方式に落ちる。

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use glow::HasContext as _;
use reminedog_render::{ZoomView, middle_row, tall_size};

use crate::gl;

type BindFramebufferFn = unsafe extern "system" fn(target: u32, framebuffer: u32);
/// LWJGL の JNI のネイティブ：GL の関数の引数の前に `(JNIEnv*, jclass, ...)`。
type JniBindFramebufferFn =
    unsafe extern "system" fn(env: *mut c_void, class: *mut c_void, target: u32, framebuffer: u32);
type JniBlitNamedFramebufferFn = unsafe extern "system" fn(
    env: *mut c_void,
    class: *mut c_void,
    read: u32,
    draw: u32,
    src_x0: i32,
    src_y0: i32,
    src_x1: i32,
    src_y1: i32,
    dst_x0: i32,
    dst_y0: i32,
    dst_x1: i32,
    dst_y1: i32,
    mask: u32,
    filter: u32,
);
type JniViewportFn = unsafe extern "system" fn(
    env: *mut c_void,
    class: *mut c_void,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
);

const GL_FRAMEBUFFER: u32 = 0x8D40;
const GL_READ_FRAMEBUFFER: u32 = 0x8CA8;
const GL_DRAW_FRAMEBUFFER: u32 = 0x8CA9;
const GL_DRAW_FRAMEBUFFER_BINDING: u32 = 0x8CA6;
const GL_READ_FRAMEBUFFER_BINDING: u32 = 0x8CAA;

/// LWJGL が 2 つ以上のクラスに持ちうる、1 つの GL の関数のネイティブ（3.1：`GL30` と
/// `ARBFramebufferObject` がそれぞれ `glBindFramebuffer` を、`GL45` と `ARBDirectStateAccess`
/// が `glBlitNamedFramebuffer` を持つ。3.2 以降はそれぞれ 1 つのクラス）。
const NATIVES: usize = 2;

/// 自前のものに差し替えた、LWJGL の GL の関数の JVM 自身のネイティブ（それぞれ、同じ番号の
/// 自前の包む関数で）。
static JNI_BIND_FRAMEBUFFER: [OnceLock<JniBindFramebufferFn>; NATIVES] =
    [const { OnceLock::new() }; NATIVES];
static JNI_BIND_FRAMEBUFFER_EXT: [OnceLock<JniBindFramebufferFn>; NATIVES] =
    [const { OnceLock::new() }; NATIVES];
static JNI_BLIT_NAMED_FRAMEBUFFER: [OnceLock<JniBlitNamedFramebufferFn>; NATIVES] =
    [const { OnceLock::new() }; NATIVES];
static JNI_VIEWPORT: [OnceLock<JniViewportFn>; NATIVES] = [const { OnceLock::new() }; NATIVES];
/// ゲームが自前の `glViewport` を受け取ったので、縦長の大きさを確かめられる。
static VIEWPORT_WRAPPED: AtomicBool = AtomicBool::new(false);

/// ゲームのコンテキストで 0 の代わりになる自前のフレームバッファ。ズーム中でなければ 0。
static REDIRECT: AtomicU32 = AtomicU32::new(0);
static GAME_CONTEXT: AtomicUsize = AtomicUsize::new(0);
/// 前のフレームから、ゲームがフレームバッファ 0 を結び付けた（か blit した）ので、自前のものを
/// 渡した。
static REDIRECTED: AtomicBool = AtomicBool::new(false);
/// 本物の `glBindFramebuffer`（ゲームのコンテキストでウィンドウのフレームバッファを結び付けるため）。
static BIND_FRAMEBUFFER: OnceLock<Option<BindFramebufferFn>> = OnceLock::new();
static WINDOW_BOUND_LOGGED: AtomicBool = AtomicBool::new(false);

/// ズーム中は、ウィンドウのシステムの本物のスワップのために、ゲームのコンテキストでウィンドウの
/// フレームバッファ（0）を結び付け、捨てるときにゲームの結び付け（自前の縦長のフレームバッファ）を
/// 戻す。スワップをフックする画面の録画（OBS のゲームキャプチャ）は、読み取り用に結び付いている
/// フレームバッファから写す：自前のものだと、ウィンドウではなく縦長のフレームの下の行を録って
/// しまう。ゲームは結び付けを覚えている（Minecraft は結び付いていると思うものは結び付けない）
/// ので、戻す。
pub struct Swapping {
    restore: Option<(BindFramebufferFn, u32, u32)>,
}

impl Swapping {
    pub fn begin() -> Self {
        Swapping {
            restore: crate::catch("zoom: the window's framebuffer for the swap", bind_window)
                .flatten(),
        }
    }
}

impl Drop for Swapping {
    fn drop(&mut self) {
        if let Some((bind, read, draw)) = self.restore {
            // SAFETY: ゲームのコンテキストがまだこのスレッドで今のもの。ゲームが持っていた結び付け。
            unsafe {
                bind(GL_READ_FRAMEBUFFER, read);
                bind(GL_DRAW_FRAMEBUFFER, draw);
            }
        }
    }
}

/// ゲームのコンテキストで自前のものが結び付いていれば、ウィンドウのフレームバッファを結び付ける。
/// 戻すものを返す。
fn bind_window() -> Option<(BindFramebufferFn, u32, u32)> {
    let ours = REDIRECT.load(Ordering::Relaxed);
    if ours == 0 {
        return None;
    }
    let current = gl::current_context();
    if current == 0 || current != GAME_CONTEXT.load(Ordering::Relaxed) {
        return None;
    }
    let read = gl::integer(GL_READ_FRAMEBUFFER_BINDING) as u32;
    let draw = gl::integer(GL_DRAW_FRAMEBUFFER_BINDING) as u32;
    if read != ours && draw != ours {
        return None;
    }
    let bind = (*BIND_FRAMEBUFFER.get_or_init(|| {
        // SAFETY: glBindFramebuffer の型。アドレスはドライバのどのコンテキストでも同じ。
        unsafe { gl::function::<BindFramebufferFn>(c"glBindFramebuffer") }
    }))?;
    // SAFETY: ゲームのコンテキストが今のもの。フレームバッファ 0 は必ずある。
    unsafe {
        bind(GL_READ_FRAMEBUFFER, 0);
        bind(GL_DRAW_FRAMEBUFFER, 0);
    }
    if !WINDOW_BOUND_LOGGED.swap(true, Ordering::Relaxed) {
        log::debug!(
            "zoom: the window's framebuffer is bound for the swap (the game had read {read}, draw {draw}; ours is {ours})"
        );
    }
    Some((bind, read, draw))
}
/// ゲームが viewport に設定すべき縦長の大きさ（width << 32 | height）と、ズームを始めてから
/// ゲームがそれを設定したか。比べるのは高さだけ：ゲームは自分の幅を使うことがある（26.3 は、
/// SDL が 2561 ピクセル幅と報告するウィンドウで 2560 幅で描く）。一度設定すると viewport は
/// 縦長のままで、Sodium は直前と同じ `glViewport` を省くので、ほとんどのフレームは何も設定しない：
/// 「見た」はズームが終わるまで残し、フレームごとには確かめない。
static TALL_VIEWPORT: AtomicU64 = AtomicU64::new(0);
static TALL_VIEWPORT_SEEN: AtomicBool = AtomicBool::new(false);
/// ズーム中のログのため：前のフレームからのゲームの `glViewport` の呼び出しの数、その中で
/// いちばん高いもの（height << 32 | width）、ウィンドウの大きさを聞いた回数。
static VIEWPORT_CALLS: AtomicU32 = AtomicU32::new(0);
static VIEWPORT_TALLEST: AtomicU64 = AtomicU64::new(0);
static SIZE_QUERIES: AtomicU32 = AtomicU32::new(0);

/// ズーム中にゲームに伝える大きさと、それが当てはまるウィンドウ。
static SIZE_OVERRIDE: Mutex<Option<(usize, [i32; 2])>> = Mutex::new(None);
/// まだゲームに伝えていない大きさ（GLFW は次のスワップの後、SDL3 は次の `SDL_PollEvent` で）。
static PENDING: Mutex<Option<(usize, [i32; 2])>> = Mutex::new(None);
/// ゲームが縦長の大きさを信じている間に、本当のウィンドウの大きさが変わった。
static REAL_RESIZED: AtomicBool = AtomicBool::new(false);

/// LWJGL の `name`（この JNI のシグネチャ）の代わりの自前のネイティブ。呼ぶために JVM 自身の
/// `address` を覚える。ズームが見ないネイティブなら `None`。
pub fn wrap_native(name: &str, signature: &str, address: *const c_void) -> Option<*const c_void> {
    let wrapper = match (name, signature) {
        ("glBindFramebuffer", "(II)V") => keep(&JNI_BIND_FRAMEBUFFER, name, address).map(|i| {
            [
                jni_bind_framebuffer::<0> as *const c_void,
                jni_bind_framebuffer::<1> as *const c_void,
            ][i]
        }),
        ("glBindFramebufferEXT", "(II)V") => {
            keep(&JNI_BIND_FRAMEBUFFER_EXT, name, address).map(|i| {
                [
                    jni_bind_framebuffer_ext::<0> as *const c_void,
                    jni_bind_framebuffer_ext::<1> as *const c_void,
                ][i]
            })
        }
        ("glBlitNamedFramebuffer", "(IIIIIIIIIIII)V") => {
            keep(&JNI_BLIT_NAMED_FRAMEBUFFER, name, address).map(|i| {
                [
                    jni_blit_named_framebuffer::<0> as *const c_void,
                    jni_blit_named_framebuffer::<1> as *const c_void,
                ][i]
            })
        }
        ("glViewport", "(IIII)V") => {
            let kept = keep(&JNI_VIEWPORT, name, address);
            if kept.is_some() {
                VIEWPORT_WRAPPED.store(true, Ordering::Relaxed);
            }
            kept.map(|i| {
                [
                    jni_viewport::<0> as *const c_void,
                    jni_viewport::<1> as *const c_void,
                ][i]
            })
        }
        _ => None,
    }?;
    log::info!("zoom: LWJGL's {name} native goes through the agent");
    Some(wrapper)
}

/// `name` の JVM のネイティブ `address` を最初の空いた置き場所に覚えるか、もう覚えているのを
/// 見つける（2 つのスレッドが同時にメソッドを結び付けると、JVM はもう一度結び付ける）：
/// 置き場所の番号（同じ番号の包む関数のため）。置き場所がほかのネイティブで埋まっていれば `None`。
fn keep<F: Copy>(
    slots: &[OnceLock<F>; NATIVES],
    name: &str,
    address: *const c_void,
) -> Option<usize> {
    // SAFETY: `F` は `name` の JNI の関数の型で、`address` はそのネイティブ。
    let native = unsafe { std::mem::transmute_copy::<*const c_void, F>(&address) };
    for (i, slot) in slots.iter().enumerate() {
        // 最初に来たものが空いた置き場所を取る。取られた置き場所には、このネイティブか別のものがある。
        let kept = slot.get_or_init(|| native);
        // SAFETY: 関数ポインタを、アドレスで比べる。
        if unsafe { std::mem::transmute_copy::<F, *const c_void>(kept) } == address {
            return Some(i);
        }
    }
    log::warn!("zoom: a further native of {name} is bound; it is left alone");
    None
}

/// ゲームがこれらのネイティブのどれかを呼んだか（JVM はネイティブを最初の呼び出しで結び付ける）。
fn bound<F>(slots: &[OnceLock<F>; NATIVES]) -> bool {
    slots.iter().any(|slot| slot.get().is_some())
}

/// ゲームの `glBindFramebuffer(target, framebuffer)` で結び付けるフレームバッファ。
fn bind_target(target: u32, framebuffer: u32) -> u32 {
    if matches!(
        target,
        GL_FRAMEBUFFER | GL_DRAW_FRAMEBUFFER | GL_READ_FRAMEBUFFER
    ) {
        redirected(framebuffer)
    } else {
        framebuffer
    }
}

/// `glBindFramebufferEXT` についての同じもの（GL_FRAMEBUFFER しか知らない）。
fn bind_target_ext(target: u32, framebuffer: u32) -> u32 {
    if target == GL_FRAMEBUFFER {
        redirected(framebuffer)
    } else {
        framebuffer
    }
}

/// ズーム中のゲームの `glViewport` を書き留める。
fn note_viewport(width: i32, height: i32) {
    let tall = TALL_VIEWPORT.load(Ordering::Relaxed);
    if tall != 0 {
        if tall & 0xffff_ffff == pack([0, height]) {
            TALL_VIEWPORT_SEEN.store(true, Ordering::Relaxed);
        }
        VIEWPORT_CALLS.fetch_add(1, Ordering::Relaxed);
        VIEWPORT_TALLEST.fetch_max(pack([height, width]), Ordering::Relaxed);
    }
}

unsafe extern "system" fn jni_bind_framebuffer<const I: usize>(
    env: *mut c_void,
    class: *mut c_void,
    target: u32,
    framebuffer: u32,
) {
    let framebuffer = bind_target(target, framebuffer);
    if let Some(native) = JNI_BIND_FRAMEBUFFER[I].get() {
        // SAFETY: JVM の引数。0 の代わりに自前のフレームバッファ。
        unsafe { native(env, class, target, framebuffer) }
    }
}

unsafe extern "system" fn jni_bind_framebuffer_ext<const I: usize>(
    env: *mut c_void,
    class: *mut c_void,
    target: u32,
    framebuffer: u32,
) {
    let framebuffer = bind_target_ext(target, framebuffer);
    if let Some(native) = JNI_BIND_FRAMEBUFFER_EXT[I].get() {
        // SAFETY: 上と同じ。
        unsafe { native(env, class, target, framebuffer) }
    }
}

#[allow(clippy::too_many_arguments)]
unsafe extern "system" fn jni_blit_named_framebuffer<const I: usize>(
    env: *mut c_void,
    class: *mut c_void,
    read: u32,
    draw: u32,
    src_x0: i32,
    src_y0: i32,
    src_x1: i32,
    src_y1: i32,
    dst_x0: i32,
    dst_y0: i32,
    dst_x1: i32,
    dst_y1: i32,
    mask: u32,
    filter: u32,
) {
    if let Some(native) = JNI_BLIT_NAMED_FRAMEBUFFER[I].get() {
        // SAFETY: 上と同じ。
        unsafe {
            native(
                env,
                class,
                redirected(read),
                redirected(draw),
                src_x0,
                src_y0,
                src_x1,
                src_y1,
                dst_x0,
                dst_y0,
                dst_x1,
                dst_y1,
                mask,
                filter,
            )
        }
    }
}

unsafe extern "system" fn jni_viewport<const I: usize>(
    env: *mut c_void,
    class: *mut c_void,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
) {
    note_viewport(width, height);
    if let Some(native) = JNI_VIEWPORT[I].get() {
        // SAFETY: JVM の引数。
        unsafe { native(env, class, x, y, width, height) }
    }
}

/// ズーム中、ゲームのコンテキストでは、0 の代わりに自前のフレームバッファ。
fn redirected(framebuffer: u32) -> u32 {
    if framebuffer != 0 {
        return framebuffer;
    }
    let ours = REDIRECT.load(Ordering::Relaxed);
    if ours == 0 {
        return 0;
    }
    let current = gl::current_context();
    if current == 0 || current != GAME_CONTEXT.load(Ordering::Relaxed) {
        return 0;
    }
    REDIRECTED.store(true, Ordering::Relaxed);
    ours
}

fn pack([w, h]: [i32; 2]) -> u64 {
    (u64::from(w as u32) << 32) | u64::from(h as u32)
}

thread_local! {
    /// ウィンドウの大きさを（SDL を通して）聞いているのは、ゲームではなくエージェント自身。
    static OWN_QUERY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// `f` を動かす。その中のウィンドウの大きさの問い合わせはエージェント自身のもの：ズームの
/// 診断はゲームのものだけを数える。
pub fn own_query<R>(f: impl FnOnce() -> R) -> R {
    OWN_QUERY.set(true);
    let result = f();
    OWN_QUERY.set(false);
    result
}

/// ズーム中に、本当の大きさの代わりに `window` についてゲームに答える大きさ。
pub fn size_override(window: *mut c_void) -> Option<[i32; 2]> {
    let current = *SIZE_OVERRIDE.lock().unwrap_or_else(|e| e.into_inner());
    let size = current.and_then(|(w, size)| (w == window as usize).then_some(size));
    if size.is_some() && !OWN_QUERY.get() {
        SIZE_QUERIES.fetch_add(1, Ordering::Relaxed);
    }
    size
}

/// ウィンドウのシステムが、`window` の本当の大きさの変更を知らせる。ゲームに見せてはいけないなら
/// true：ゲームはズームが終わる（次のスワップ）まで縦長の大きさを信じ、その後に新しい本当の
/// 大きさを伝えられる。
pub fn real_resize(window: *mut c_void) -> bool {
    if size_override(window).is_some() {
        REAL_RESIZED.store(true, Ordering::Relaxed);
        true
    } else {
        false
    }
}

/// いまゲームに伝えるべき大きさ（あれば）。
pub fn take_pending() -> Option<(*mut c_void, [i32; 2])> {
    PENDING
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
        .map(|(window, size)| (window as *mut c_void, size))
}

fn set_size(window: *mut c_void, size: Option<[i32; 2]>, tell: [i32; 2]) {
    *SIZE_OVERRIDE.lock().unwrap_or_else(|e| e.into_inner()) =
        size.map(|size| (window as usize, size));
    *PENDING.lock().unwrap_or_else(|e| e.into_inner()) = Some((window as usize, tell));
}

/// 高精細のズームをあきらめるまでに、ゲームが縦長の大きさで描きそこねてよい連続のフレームの数
/// （大きさの変更は、道によっては 1 フレーム遅れて伝わる）。
const MAX_MISSES: u32 = 3;
/// ……と、その時間。縦長の大きさで描画先を作り直すと、ゲームが数フレーム止まることがあるので、
/// フレームの数だけでは本物の機械で早くあきらめすぎる。
const MISS_TIMEOUT: Duration = Duration::from_secs(1);

/// ウィンドウごとの状態。ゲームのコンテキストが今のものの、スワップのフックの中で使う。
#[derive(Default)]
pub struct TallZoom {
    game: Option<GameGl>,
    active: Option<Active>,
    misses: u32,
    /// 今の描きそこねの連続が始まった時刻。
    first_miss: Option<Instant>,
    /// 高精細のズームが使えない理由（メニューのため）と、ズームの設定を変えたら試し直すか
    /// （[`retry`](TallZoom::retry)）。
    failure: Option<(String, bool)>,
}

/// ズームの途中でオーバーレイがなくなる（描画先が変わったか、フレームがパニックした）：
/// 差し替えをやめ、ゲームに本当の大きさを返す。ゲームのコンテキストが今のものでなければ、ここで
/// 結び付けを直せない。ゲームは毎フレーム自分のフレームバッファを結び付け直す。
impl Drop for TallZoom {
    fn drop(&mut self) {
        if let Some(active) = self.active.take() {
            REDIRECT.store(0, Ordering::Relaxed);
            TALL_VIEWPORT.store(0, Ordering::Relaxed);
            set_size(active.window as *mut c_void, None, active.real);
            log::debug!("zoom: ended with the overlay");
        }
    }
}

/// ゲームのコンテキストの GL の物。
struct GameGl {
    context: usize,
    gl: glow::Context,
    max_dim: u32,
    target: Option<Target>,
}

struct Target {
    framebuffer: glow::Framebuffer,
    color: glow::Renderbuffer,
    depth: glow::Renderbuffer,
    size: [i32; 2],
}

struct Active {
    window: usize,
    real: [i32; 2],
    tall: [i32; 2],
    factor: f32,
}

impl TallZoom {
    pub fn failure(&self) -> Option<&str> {
        self.failure.as_ref().map(|(text, _)| text.as_str())
    }

    /// ズームの設定が変わった：それに左右されうる失敗（大きさ、その大きさでのゲームの描画）を
    /// 試し直す。
    pub fn retry(&mut self) {
        if self.failure.as_ref().is_some_and(|&(_, retry)| retry) {
            log::debug!("zoom: trying the high resolution again");
            self.failure = None;
        }
    }

    /// ズームを終え、縦長のフレームバッファを解放する（ゲームのコンテキストが今のものなら。そうで
    /// なければズームを終えるだけ）：オーバーレイがなくなる前や、ウィンドウが最小化されている間。
    pub fn release(&mut self) {
        let current = gl::current_context();
        let in_game_context = current != 0
            && self
                .game
                .as_ref()
                .is_some_and(|game| game.context == current);
        if let Some(active) = &self.active {
            let real = active.real;
            if in_game_context {
                self.stop(real);
            } else {
                REDIRECT.store(0, Ordering::Relaxed);
                TALL_VIEWPORT.store(0, Ordering::Relaxed);
                let active = self.active.take().expect("checked");
                set_size(active.window as *mut c_void, None, real);
            }
        }
        if in_game_context && let Some(game) = &mut self.game {
            game.delete_target();
        }
    }

    /// オーバーレイが描く前に、ゲームのコンテキストが今のもので、スワップのフックの中で呼ぶ。
    /// ゲームが拡大したフレームを描いていればそれを見せ、ズームを始めるか終え、このフレームで
    /// オーバーレイがズームをどう見せるかを返す。
    pub fn frame(
        &mut self,
        window: *mut c_void,
        real: [i32; 2],
        want: bool,
        factor: f32,
    ) -> ZoomView {
        let fallback = if want {
            ZoomView::Magnify
        } else {
            ZoomView::Off
        };
        let context = gl::current_context();
        if let Some(active) = &self.active {
            if context == 0 || context != GAME_CONTEXT.load(Ordering::Relaxed) {
                // 別のコンテキストがスワップしている。ズームはそのままにする。
                return fallback;
            }
            let redirected = REDIRECTED.swap(false, Ordering::Relaxed);
            let tall = TALL_VIEWPORT_SEEN.load(Ordering::Relaxed)
                || !VIEWPORT_WRAPPED.load(Ordering::Relaxed);
            let viewport_calls = VIEWPORT_CALLS.swap(0, Ordering::Relaxed);
            let tallest = VIEWPORT_TALLEST.swap(0, Ordering::Relaxed);
            let size_queries = SIZE_QUERIES.swap(0, Ordering::Relaxed);
            let seen = || {
                let (h, w) = (tallest >> 32, tallest & 0xffff_ffff);
                let mut current = [0; 4];
                if let Some(game) = &self.game {
                    // SAFETY: ゲームのコンテキストが今のもの。問い合わせは状態を変えない。
                    unsafe {
                        game.gl
                            .get_parameter_i32_slice(glow::VIEWPORT, &mut current)
                    };
                }
                format!(
                    "glViewport calls: {viewport_calls}, tallest {w}x{h}, viewport now {}x{}, size queries: {size_queries}",
                    current[2], current[3]
                )
            };
            let factor = active.factor;
            let view = if redirected && tall {
                if self.misses > 0 {
                    log::debug!(
                        "zoom: tall frame after {} frames not rendered at the tall size",
                        self.misses
                    );
                }
                self.misses = 0;
                self.first_miss = None;
                self.compose();
                ZoomView::HighRes { factor }
            } else {
                if redirected {
                    // ゲームは、ウィンドウではなく、前の大きさのまま自前のフレームバッファに描いた：
                    // 引き伸ばす方式のために、それを写す（ウィンドウのバッファには古いフレームか何もない）。
                    self.copy_rows(0);
                }
                self.misses += 1;
                let first_miss = *self.first_miss.get_or_insert_with(Instant::now);
                if log::log_enabled!(log::Level::Debug) {
                    log::debug!(
                        "zoom: frame not rendered at the tall size (output redirected: {redirected}, tall viewport: {tall}, {} frames in {:.0?}; {})",
                        self.misses,
                        first_miss.elapsed(),
                        seen()
                    );
                }
                fallback
            };
            let resized = REAL_RESIZED.swap(false, Ordering::Relaxed) || real != active.real;
            if self.misses >= MAX_MISSES
                && self
                    .first_miss
                    .is_some_and(|first| first.elapsed() >= MISS_TIMEOUT)
            {
                log::warn!(
                    "zoom: the game does not render at the tall size; enlarging pixels instead (last frame: output redirected: {redirected}, tall viewport: {tall}; {} frames; {})",
                    self.misses,
                    seen()
                );
                self.failure = Some((
                    "ゲームが縦長の解像度で描かなかった（倍率か「高精細にする」を変えると試し直す）"
                        .into(),
                    true,
                ));
            }
            if !want || resized || self.failure.is_some() {
                self.stop(real);
            }
            return view;
        }
        if !want || self.failure.is_some() {
            return fallback;
        }
        if context == 0 {
            return fallback;
        }
        if !bound(&JNI_BIND_FRAMEBUFFER) && !bound(&JNI_BIND_FRAMEBUFFER_EXT) {
            self.failure = Some((
                "ゲームの OpenGL の関数をフックできなかった（ゲームを再起動すると試し直す）".into(),
                false,
            ));
            log::warn!("zoom: the game has not called LWJGL's glBindFramebuffer");
            return fallback;
        }
        if let Err(e) = self.start(window, context, real, factor) {
            log::warn!("zoom: no high resolution: {e}");
            self.failure = Some((e, true));
        }
        // このフレームはまだふつうのもの。縦長のフレームは次から始まる。
        fallback
    }

    fn start(
        &mut self,
        window: *mut c_void,
        context: usize,
        real: [i32; 2],
        factor: f32,
    ) -> Result<(), String> {
        let game = self.game_gl(context)?;
        let size = [real[0].max(0) as u32, real[1].max(0) as u32];
        let Some(([w, h], factor)) = tall_size(size, factor, game.max_dim) else {
            // 得るものがない（倍率が 1 に近いか、ウィンドウがもう GPU の許す高さまである）。
            // 今回はピクセルを引き伸ばす。
            return Ok(());
        };
        let tall = [w as i32, h as i32];
        let framebuffer = game.ensure_target(tall)?;
        let id = framebuffer.0.get();
        GAME_CONTEXT.store(context, Ordering::Relaxed);
        REDIRECTED.store(false, Ordering::Relaxed);
        // ズームしていない間に知らされた（か、もうないオーバーレイに知らされた）大きさの変更は、
        // このズームのものではない。
        REAL_RESIZED.store(false, Ordering::Relaxed);
        TALL_VIEWPORT.store(pack(tall), Ordering::Relaxed);
        TALL_VIEWPORT_SEEN.store(false, Ordering::Relaxed);
        VIEWPORT_CALLS.store(0, Ordering::Relaxed);
        VIEWPORT_TALLEST.store(0, Ordering::Relaxed);
        SIZE_QUERIES.store(0, Ordering::Relaxed);
        REDIRECT.store(id, Ordering::Relaxed);
        // これからは「0」が自前のもの。いま 0 が結び付いていれば（ゲームはそれを覚えている）、
        // その代わりに自前のものを結び付ける。
        game.swap_binding(None, Some(framebuffer));
        set_size(window, Some(tall), tall);
        self.misses = 0;
        self.first_miss = None;
        self.active = Some(Active {
            window: window as usize,
            real,
            tall,
            factor,
        });
        log::debug!(
            "zoom: game renders at {}x{} (×{factor:.2})",
            tall[0],
            tall[1]
        );
        Ok(())
    }

    fn stop(&mut self, real: [i32; 2]) {
        let Some(active) = self.active.take() else {
            return;
        };
        REDIRECT.store(0, Ordering::Relaxed);
        TALL_VIEWPORT.store(0, Ordering::Relaxed);
        TALL_VIEWPORT_SEEN.store(false, Ordering::Relaxed);
        if let Some(game) = &mut self.game
            && let Some(target) = &game.target
        {
            game.swap_binding(Some(target.framebuffer), None);
            // 大きな倍率では数百 MB。次のズームでまた作る。
            game.delete_target();
        }
        set_size(active.window as *mut c_void, None, real);
        log::debug!("zoom: game back at {}x{}", real[0], real[1]);
    }

    /// 自前のフレームバッファの中央を、等倍でウィンドウに。
    fn compose(&self) {
        if let Some(active) = &self.active {
            self.copy_rows(middle_row(active.tall[1] as u32, active.real[1] as u32) as i32);
        }
    }

    /// 自前のフレームバッファの、行 `y0` から上にウィンドウの高さ分の行を、等倍でウィンドウに。
    fn copy_rows(&self, y0: i32) {
        let (Some(game), Some(active)) = (&self.game, &self.active) else {
            return;
        };
        let Some(target) = &game.target else {
            return;
        };
        let gl = &game.gl;
        let [w, h] = active.real;
        // SAFETY: ゲームのコンテキストが今のもの（呼び出し側が確かめた）。触れた状態はすべて戻す。
        unsafe {
            let saved = SavedState::save(gl);
            gl.disable(glow::SCISSOR_TEST);
            gl.color_mask(true, true, true, true);
            gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(target.framebuffer));
            gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, None);
            gl.blit_framebuffer(
                0,
                y0,
                w,
                y0 + h,
                0,
                0,
                w,
                h,
                glow::COLOR_BUFFER_BIT,
                glow::NEAREST,
            );
            saved.restore(gl);
        }
    }

    fn game_gl(&mut self, context: usize) -> Result<&mut GameGl, String> {
        if self.game.as_ref().is_some_and(|g| g.context != context) {
            // 新しいコンテキスト（古いものの物は、それと一緒になくなった）。
            self.game = None;
        }
        if self.game.is_none() {
            // SAFETY: ゲームのコンテキストが今のもので、glow のコンテキストはその中でだけ使う。
            let gl = unsafe { gl::glow_context() };
            let version = gl.version();
            if version.major < 3 {
                return Err(format!(
                    "OpenGL {}.{} では使えない（3.0 以降が必要）",
                    version.major, version.minor
                ));
            }
            // SAFETY: ただの問い合わせ。
            let max_dim = unsafe {
                let mut viewport = [0; 2];
                gl.get_parameter_i32_slice(glow::MAX_VIEWPORT_DIMS, &mut viewport);
                [
                    gl.get_parameter_i32(glow::MAX_TEXTURE_SIZE),
                    gl.get_parameter_i32(glow::MAX_RENDERBUFFER_SIZE),
                    viewport[0].min(viewport[1]),
                ]
                .into_iter()
                .min()
                .unwrap_or(0)
                .max(0) as u32
            };
            log::info!("zoom: largest tall size {max_dim} px");
            self.game = Some(GameGl {
                context,
                gl,
                max_dim,
                target: None,
            });
        }
        Ok(self.game.as_mut().expect("just set"))
    }
}

impl GameGl {
    /// 自前のフレームバッファがあれば消す。ゲームのコンテキストが今のものであること。
    fn delete_target(&mut self) {
        if let Some(target) = self.target.take() {
            // SAFETY: ゲームのコンテキスト（今のもの）の中の、自前の物。
            unsafe {
                self.gl.delete_framebuffer(target.framebuffer);
                self.gl.delete_renderbuffer(target.color);
                self.gl.delete_renderbuffer(target.depth);
            }
        }
    }

    /// `size` の自前のフレームバッファ（色と深度・ステンシルのレンダーバッファ）。
    fn ensure_target(&mut self, size: [i32; 2]) -> Result<glow::Framebuffer, String> {
        if let Some(target) = &self.target {
            if target.size == size {
                return Ok(target.framebuffer);
            }
            self.delete_target();
        }
        let gl = &self.gl;
        // SAFETY: ゲームのコンテキストが今のもの。その結び付けは戻す。
        unsafe {
            let saved = SavedState::save(gl);
            let saved_renderbuffer = gl.get_parameter_i32(glow::RENDERBUFFER_BINDING);
            let result = (|| {
                let color = gl.create_renderbuffer()?;
                gl.bind_renderbuffer(glow::RENDERBUFFER, Some(color));
                gl.renderbuffer_storage(glow::RENDERBUFFER, glow::RGBA8, size[0], size[1]);
                let depth = gl.create_renderbuffer()?;
                gl.bind_renderbuffer(glow::RENDERBUFFER, Some(depth));
                gl.renderbuffer_storage(
                    glow::RENDERBUFFER,
                    glow::DEPTH24_STENCIL8,
                    size[0],
                    size[1],
                );
                let framebuffer = gl.create_framebuffer()?;
                gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, Some(framebuffer));
                gl.framebuffer_renderbuffer(
                    glow::DRAW_FRAMEBUFFER,
                    glow::COLOR_ATTACHMENT0,
                    glow::RENDERBUFFER,
                    Some(color),
                );
                gl.framebuffer_renderbuffer(
                    glow::DRAW_FRAMEBUFFER,
                    glow::DEPTH_STENCIL_ATTACHMENT,
                    glow::RENDERBUFFER,
                    Some(depth),
                );
                let status = gl.check_framebuffer_status(glow::DRAW_FRAMEBUFFER);
                Ok::<_, String>((framebuffer, color, depth, status))
            })();
            gl.bind_renderbuffer(
                glow::RENDERBUFFER,
                std::num::NonZeroU32::new(saved_renderbuffer as u32).map(glow::NativeRenderbuffer),
            );
            saved.restore(gl);
            let (framebuffer, color, depth, status) =
                result.map_err(|e| format!("フレームバッファを作れなかった（{e}）"))?;
            if status != glow::FRAMEBUFFER_COMPLETE {
                gl.delete_framebuffer(framebuffer);
                gl.delete_renderbuffer(color);
                gl.delete_renderbuffer(depth);
                return Err(format!(
                    "{}×{} のフレームバッファを作れなかった（0x{status:X}）",
                    size[0], size[1]
                ));
            }
            self.target = Some(Target {
                framebuffer,
                color,
                depth,
                size,
            });
            Ok(framebuffer)
        }
    }

    /// `from` が描画用か読み取り用に結び付いているところに、代わりに `to` を結び付ける（ゲームの
    /// 包みではなく本物の関数で）。
    fn swap_binding(&self, from: Option<glow::Framebuffer>, to: Option<glow::Framebuffer>) {
        let from = from.map_or(0, |f| f.0.get() as i32);
        let gl = &self.gl;
        // SAFETY: ゲームのコンテキストが今のもの。
        unsafe {
            for (binding, target) in [
                (glow::DRAW_FRAMEBUFFER_BINDING, glow::DRAW_FRAMEBUFFER),
                (glow::READ_FRAMEBUFFER_BINDING, glow::READ_FRAMEBUFFER),
            ] {
                if gl.get_parameter_i32(binding) == from {
                    gl.bind_framebuffer(target, to);
                }
            }
        }
    }
}

/// blit が変えるゲームの状態。
struct SavedState {
    draw: i32,
    read: i32,
    scissor: bool,
    color_mask: [bool; 4],
}

impl SavedState {
    unsafe fn save(gl: &glow::Context) -> Self {
        // SAFETY: 今のコンテキストへのただの問い合わせ。
        unsafe {
            Self {
                draw: gl.get_parameter_i32(glow::DRAW_FRAMEBUFFER_BINDING),
                read: gl.get_parameter_i32(glow::READ_FRAMEBUFFER_BINDING),
                scissor: gl.is_enabled(glow::SCISSOR_TEST),
                color_mask: gl.get_parameter_bool_array::<4>(glow::COLOR_WRITEMASK),
            }
        }
    }

    unsafe fn restore(&self, gl: &glow::Context) {
        let framebuffer =
            |id: i32| std::num::NonZeroU32::new(id as u32).map(glow::NativeFramebuffer);
        // SAFETY: `save` が読んだものを、同じコンテキストで戻す。
        unsafe {
            gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, framebuffer(self.draw));
            gl.bind_framebuffer(glow::READ_FRAMEBUFFER, framebuffer(self.read));
            if self.scissor {
                gl.enable(glow::SCISSOR_TEST);
            }
            let [r, g, b, a] = self.color_mask;
            gl.color_mask(r, g, b, a);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 3 つの別々のネイティブ（中身が違うので、1 つのアドレスにまとめられることはない）。
    unsafe extern "system" fn native_a(_: *mut c_void, _: *mut c_void, _: u32, _: u32) {
        std::hint::black_box(1);
    }
    unsafe extern "system" fn native_b(_: *mut c_void, _: *mut c_void, _: u32, _: u32) {
        std::hint::black_box(2);
    }
    unsafe extern "system" fn native_c(_: *mut c_void, _: *mut c_void, _: u32, _: u32) {
        std::hint::black_box(3);
    }

    #[test]
    fn each_class_native_gets_a_wrapper_of_its_own() {
        let slots: [OnceLock<JniBindFramebufferFn>; NATIVES] = [const { OnceLock::new() }; NATIVES];
        let (a, b, c) = (
            native_a as *const c_void,
            native_b as *const c_void,
            native_c as *const c_void,
        );
        assert!(!bound(&slots));
        assert_eq!(keep(&slots, "glBindFramebuffer", a), Some(0));
        // LWJGL 3.1：同じ関数が 2 つ目のクラスに。
        assert_eq!(keep(&slots, "glBindFramebuffer", b), Some(1));
        // 最初のものがもう一度結び付く：また同じ包む関数。
        assert_eq!(keep(&slots, "glBindFramebuffer", a), Some(0));
        assert_eq!(keep(&slots, "glBindFramebuffer", c), None);
        assert!(bound(&slots));
    }
}
