//! ウィンドウのライブラリの関数のフック。
//!
//! LWJGL が関数を引くときに、本物の代わりにフックを渡す（[`crate::jvmti`]）。何も書き換えない。
//! フックは、LWJGL が引いた本物を [`Original`] から呼ぶ。

use std::ffi::{CStr, c_void};
use std::marker::PhantomData;
use std::sync::atomic::{AtomicUsize, Ordering};

/// フックの裏の本物の関数（LWJGL が引いた後）。
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
        // SAFETY: 型が F の本物の関数（`Slot::set_looked_up` を参照）。
        (address != 0).then(|| unsafe { std::mem::transmute_copy::<usize, F>(&address) })
    }
}

impl<F: Copy> Default for Original<F> {
    fn default() -> Self {
        Self::new()
    }
}

/// どの関数の型でもよい [`Original`]（[`Hook`] の表のため）。
pub trait Slot: Sync {
    /// # Safety
    /// `address` はこの置き場所の型の関数。
    unsafe fn set_looked_up(&self, address: *const c_void);
}

impl<F: Copy + Send + Sync> Slot for Original<F> {
    unsafe fn set_looked_up(&self, address: *const c_void) {
        self.looked_up.store(address as usize, Ordering::Release);
    }
}

/// ウィンドウのライブラリのフックした関数。
pub struct Hook {
    pub name: &'static CStr,
    /// 関数のシグネチャを持つ。
    pub hook: *const c_void,
    pub original: &'static dyn Slot,
}

// SAFETY: `hook` は関数のアドレス。
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
