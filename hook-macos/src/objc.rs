//! Objective-C のランタイム（エージェントが AppKit（`NSOpenGLContext`、`NSPasteboard`）と
//! 話すところまで）。
//!
//! libobjc はリンクする（小さく、ウィンドウを持つ macOS のプロセスにはどれにもある）。AppKit は
//! リンクしない：そのクラスは、ゲーム（GLFW、SDL3）が読み込んでから名前で引き、それまでは null。

use std::ffi::{CStr, c_char, c_void};
use std::sync::atomic::{AtomicUsize, Ordering};

/// オブジェクト（`id`）。null は `nil`。
pub type Id = *mut c_void;
/// セレクタ（`SEL`）。
pub type Sel = *const c_void;

pub const NIL: Id = std::ptr::null_mut();

#[link(name = "objc")]
unsafe extern "C" {
    fn objc_getClass(name: *const c_char) -> Id;
    fn sel_registerName(name: *const c_char) -> Sel;
    fn objc_msgSend();
    #[cfg(target_arch = "x86_64")]
    fn objc_msgSend_stret();
    fn objc_autoreleasePoolPush() -> *mut c_void;
    fn objc_autoreleasePoolPop(pool: *mut c_void);
}

/// 名前で引いたクラス。読み込まれていなければ null。
pub fn class(name: &CStr) -> Id {
    // SAFETY: NUL で終わる名前。
    unsafe { objc_getClass(name.as_ptr()) }
}

/// セレクタ。初めて使うときに登録する。
pub struct Selector {
    name: &'static CStr,
    sel: AtomicUsize,
}

impl Selector {
    pub const fn new(name: &'static CStr) -> Self {
        Self {
            name,
            sel: AtomicUsize::new(0),
        }
    }

    fn get(&self) -> Sel {
        match self.sel.load(Ordering::Relaxed) {
            0 => {
                // SAFETY: NUL で終わる名前。セレクタはプロセスの間生きる。
                let sel = unsafe { sel_registerName(self.name.as_ptr()) };
                self.sel.store(sel as usize, Ordering::Relaxed);
                sel
            }
            sel => sel as Sel,
        }
    }
}

/// メソッドの関数の型としての `objc_msgSend`。
///
/// # Safety
/// `F` は、構造体を返さない `extern "C"` の関数ポインタの型。
unsafe fn msg_send<F: Copy>() -> F {
    let raw = objc_msgSend as unsafe extern "C" fn();
    // SAFETY: 関数ポインタはどれも同じ大きさ。メソッドの型は呼び出し側が与える。
    unsafe { std::mem::transmute_copy(&raw) }
}

/// 引数なしでメッセージを送る。
///
/// # Safety
/// `receiver` は、`sel` に `R` で答えるオブジェクト（か nil）。
pub unsafe fn send<R>(receiver: Id, sel: &Selector) -> R {
    // SAFETY: 呼び出し側が保証する。
    unsafe { msg_send::<unsafe extern "C" fn(Id, Sel) -> R>()(receiver, sel.get()) }
}

/// 引数 1 つでメッセージを送る。
///
/// # Safety
/// `receiver` は、`A` を取る `sel` に `R` で答えるオブジェクト（か nil）。
pub unsafe fn send1<A, R>(receiver: Id, sel: &Selector, a: A) -> R {
    // SAFETY: 呼び出し側が保証する。
    unsafe { msg_send::<unsafe extern "C" fn(Id, Sel, A) -> R>()(receiver, sel.get(), a) }
}

/// 引数 2 つでメッセージを送る。
///
/// # Safety
/// [`send1`] と同じ。
pub unsafe fn send2<A, B, R>(receiver: Id, sel: &Selector, a: A, b: B) -> R {
    // SAFETY: 呼び出し側が保証する。
    unsafe { msg_send::<unsafe extern "C" fn(Id, Sel, A, B) -> R>()(receiver, sel.get(), a, b) }
}

/// 引数 3 つでメッセージを送る。
///
/// # Safety
/// [`send1`] と同じ。
pub unsafe fn send3<A, B, C, R>(receiver: Id, sel: &Selector, a: A, b: B, c: C) -> R {
    // SAFETY: 呼び出し側が保証する。
    unsafe {
        msg_send::<unsafe extern "C" fn(Id, Sel, A, B, C) -> R>()(receiver, sel.get(), a, b, c)
    }
}

/// `NSRect`（`CGRect`）：ポイントでの原点と大きさ。
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// 引数なしで、`NSRect` で答えるメッセージを送る（x86_64 では構造体をメモリで返すので、
/// `objc_msgSend_stret` を通す）。
///
/// # Safety
/// `receiver` は、`sel` に `NSRect` で答えるオブジェクト。
pub unsafe fn send_rect(receiver: Id, sel: &Selector) -> Rect {
    #[cfg(target_arch = "x86_64")]
    let raw = objc_msgSend_stret as unsafe extern "C" fn();
    #[cfg(not(target_arch = "x86_64"))]
    let raw = objc_msgSend as unsafe extern "C" fn();
    // SAFETY: このアーキテクチャで構造体を返すときの objc_msgSend の変種を、メソッドの型として。
    let f: unsafe extern "C" fn(Id, Sel) -> Rect = unsafe { std::mem::transmute(raw) };
    // SAFETY: 呼び出し側が保証する。
    unsafe { f(receiver, sel.get()) }
}

/// `f` の周りの autorelease pool：AppKit の getter は答えるものを autorelease することがあり、
/// エージェントはゲームのプールの外で動く。
pub fn autoreleased<R>(f: impl FnOnce() -> R) -> R {
    struct Pool(*mut c_void);
    impl Drop for Pool {
        fn drop(&mut self) {
            // SAFETY: 下で積んだプール。このスレッドで 1 回だけ降ろす。
            unsafe { objc_autoreleasePoolPop(self.0) };
        }
    }
    // SAFETY: 前提はない。
    let _pool = Pool(unsafe { objc_autoreleasePoolPush() });
    f()
}
