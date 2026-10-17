//! Hooks on the window library's functions, and export lookup.
//!
//! LWJGL is handed a hook in place of a function when it looks the function up
//! ([`crate::jvmti`]); nothing is patched. The hook runs the real function, which LWJGL
//! looked up, through its [`Original`].

use std::ffi::{CStr, c_void};
use std::marker::PhantomData;
use std::sync::atomic::{AtomicUsize, Ordering};

use windows_sys::Win32::Foundation::HMODULE;

use crate::ffi;

/// The real function behind a hook, once LWJGL has looked it up.
pub struct Original<F> {
    looked_up: AtomicUsize,
    function: PhantomData<F>,
}

impl<F: Copy> Original<F> {
    pub const fn new() -> Self {
        Self {
            looked_up: AtomicUsize::new(0),
            function: PhantomData,
        }
    }

    pub fn get(&self) -> Option<F> {
        const {
            assert!(
                size_of::<F>() == size_of::<usize>(),
                "F must be a fn pointer"
            )
        };
        let address = self.looked_up.load(Ordering::Acquire);
        // SAFETY: the real function, whose type F is (see `Slot::set_looked_up`).
        (address != 0).then(|| unsafe { std::mem::transmute_copy::<usize, F>(&address) })
    }
}

/// An [`Original`] of any function type, for the tables of [`Hook`]s.
pub trait Slot: Sync {
    /// # Safety
    /// `address` is a function of the slot's type.
    unsafe fn set_looked_up(&self, address: *const c_void);
    /// Whether the game's calls reach the hook.
    fn hooked(&self) -> bool;

    /// Whether the game's calls reach the hook, or will once LWJGL looks the function up.
    fn ready(&self) -> bool {
        self.hooked() || crate::jvmti::handing_out()
    }
}

impl<F: Copy + Send + Sync> Slot for Original<F> {
    unsafe fn set_looked_up(&self, address: *const c_void) {
        self.looked_up.store(address as usize, Ordering::Release);
    }

    fn hooked(&self) -> bool {
        self.looked_up.load(Ordering::Acquire) != 0
    }
}

/// A hooked function of the window library.
pub struct Hook {
    pub name: &'static CStr,
    /// Has the function's signature.
    pub hook: *const c_void,
    pub original: &'static dyn Slot,
}

// SAFETY: `hook` is a function's address.
unsafe impl Send for Hook {}
unsafe impl Sync for Hook {}

impl Hook {
    pub fn new(name: &'static CStr, hook: *const c_void, original: &'static dyn Slot) -> Self {
        Self {
            name,
            hook,
            original,
        }
    }
}

/// The address of an exported function of `module`, a loaded module (read from its PE
/// headers).
pub fn export(module: HMODULE, name: &CStr) -> Option<*const c_void> {
    // SAFETY: every caller passes a module LWJGL loaded and looks functions up in.
    unsafe { ffi::export_address(module, name) }
}
