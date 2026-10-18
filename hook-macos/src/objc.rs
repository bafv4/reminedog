//! The Objective-C runtime, as far as the agent talks to AppKit (`NSOpenGLContext`,
//! `NSPasteboard`).
//!
//! libobjc is linked (it is small and every macOS process with a window has it); AppKit is not:
//! its classes are looked up by name once the game (GLFW, SDL3) has loaded it, and are null
//! before that.

use std::ffi::{CStr, c_char, c_void};
use std::sync::atomic::{AtomicUsize, Ordering};

/// An object (`id`); null is `nil`.
pub type Id = *mut c_void;
/// A selector (`SEL`).
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

/// A class by name, null if it is not loaded.
pub fn class(name: &CStr) -> Id {
    // SAFETY: a NUL-terminated name.
    unsafe { objc_getClass(name.as_ptr()) }
}

/// A selector, registered on first use.
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
                // SAFETY: a NUL-terminated name; selectors live for the process.
                let sel = unsafe { sel_registerName(self.name.as_ptr()) };
                self.sel.store(sel as usize, Ordering::Relaxed);
                sel
            }
            sel => sel as Sel,
        }
    }
}

/// `objc_msgSend` as the function type of a method.
///
/// # Safety
/// `F` is an `extern "C"` function pointer type whose return is not a struct.
unsafe fn msg_send<F: Copy>() -> F {
    let raw = objc_msgSend as unsafe extern "C" fn();
    // SAFETY: function pointers are all one size; the caller gives the method's type.
    unsafe { std::mem::transmute_copy(&raw) }
}

/// Sends a message without arguments.
///
/// # Safety
/// `receiver` is an object (or nil) that answers `sel` with an `R`.
pub unsafe fn send<R>(receiver: Id, sel: &Selector) -> R {
    // SAFETY: guaranteed by the caller.
    unsafe { msg_send::<unsafe extern "C" fn(Id, Sel) -> R>()(receiver, sel.get()) }
}

/// Sends a message with one argument.
///
/// # Safety
/// `receiver` is an object (or nil) that answers `sel` taking an `A` with an `R`.
pub unsafe fn send1<A, R>(receiver: Id, sel: &Selector, a: A) -> R {
    // SAFETY: guaranteed by the caller.
    unsafe { msg_send::<unsafe extern "C" fn(Id, Sel, A) -> R>()(receiver, sel.get(), a) }
}

/// Sends a message with two arguments.
///
/// # Safety
/// As [`send1`].
pub unsafe fn send2<A, B, R>(receiver: Id, sel: &Selector, a: A, b: B) -> R {
    // SAFETY: guaranteed by the caller.
    unsafe { msg_send::<unsafe extern "C" fn(Id, Sel, A, B) -> R>()(receiver, sel.get(), a, b) }
}

/// Sends a message with three arguments.
///
/// # Safety
/// As [`send1`].
pub unsafe fn send3<A, B, C, R>(receiver: Id, sel: &Selector, a: A, b: B, c: C) -> R {
    // SAFETY: guaranteed by the caller.
    unsafe {
        msg_send::<unsafe extern "C" fn(Id, Sel, A, B, C) -> R>()(receiver, sel.get(), a, b, c)
    }
}

/// `NSRect` (`CGRect`): origin and size in points.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// Sends a message without arguments that answers an `NSRect` (a struct returned in memory on
/// x86_64, through `objc_msgSend_stret`).
///
/// # Safety
/// `receiver` is an object that answers `sel` with an `NSRect`.
pub unsafe fn send_rect(receiver: Id, sel: &Selector) -> Rect {
    #[cfg(target_arch = "x86_64")]
    let raw = objc_msgSend_stret as unsafe extern "C" fn();
    #[cfg(not(target_arch = "x86_64"))]
    let raw = objc_msgSend as unsafe extern "C" fn();
    // SAFETY: the variant of objc_msgSend for a struct return on this architecture, as the
    // method's type.
    let f: unsafe extern "C" fn(Id, Sel) -> Rect = unsafe { std::mem::transmute(raw) };
    // SAFETY: guaranteed by the caller.
    unsafe { f(receiver, sel.get()) }
}

/// An autorelease pool around `f`: AppKit's getters may autorelease what they answer, and the
/// agent runs outside the game's pools.
pub fn autoreleased<R>(f: impl FnOnce() -> R) -> R {
    struct Pool(*mut c_void);
    impl Drop for Pool {
        fn drop(&mut self) {
            // SAFETY: the pool pushed below, popped once on this thread.
            unsafe { objc_autoreleasePoolPop(self.0) };
        }
    }
    // SAFETY: no preconditions.
    let _pool = Pool(unsafe { objc_autoreleasePoolPush() });
    f()
}
