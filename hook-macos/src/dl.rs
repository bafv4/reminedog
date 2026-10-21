//! 動的リンカ：LWJGL が読み込んだライブラリのシンボルと、ゲームが読み込んだ後にエージェント自身が
//! 使うライブラリ（OpenGL.framework）。

use std::ffi::{CStr, c_char, c_void};

// dyld のイメージの一覧（libSystem。libc の束縛は別の crate に移るため非推奨）。
unsafe extern "C" {
    fn _dyld_image_count() -> u32;
    fn _dyld_get_image_name(index: u32) -> *const c_char;
}

/// `dlopen` のハンドル `library` の中の `name` のアドレス。
pub fn symbol(library: usize, name: &CStr) -> Option<*const c_void> {
    // SAFETY: LWJGL が dlopen で得たハンドル。NUL で終わる名前。
    let p = unsafe { libc::dlsym(library as *mut c_void, name.as_ptr()) };
    (!p.is_null()).then_some(p.cast_const())
}

/// `dlopen` のハンドルのライブラリを読み込んだ場所：そのハンドルを持つ読み込み済みのイメージ
/// （macOS には `dlinfo` がない）。
pub fn path(library: usize) -> Option<String> {
    // SAFETY: dyld のイメージの一覧。RTLD_NOLOAD はイメージを探すだけで、取った参照は
    // また手放す。
    unsafe {
        for i in 0.._dyld_image_count() {
            let name = _dyld_get_image_name(i);
            if name.is_null() {
                continue;
            }
            let handle = libc::dlopen(name, libc::RTLD_LAZY | libc::RTLD_NOLOAD);
            if handle.is_null() {
                continue;
            }
            let same = handle as usize == library;
            libc::dlclose(handle);
            if same {
                return Some(CStr::from_ptr(name).to_string_lossy().into_owned());
            }
        }
    }
    None
}

/// このライブラリ以外の読み込み済みのイメージが `symbol` を export しているか（別のフォルダの
/// エージェントの別のコピー：dyld はパスごとに別のイメージとして読み込む）。
pub fn loaded_elsewhere(symbol: &CStr, ours: *const c_void) -> bool {
    // SAFETY: dyld のイメージの一覧。RTLD_NOLOAD はイメージを探すだけで、取った参照は
    // また手放す。
    unsafe {
        for i in 0.._dyld_image_count() {
            let name = _dyld_get_image_name(i);
            if name.is_null() {
                continue;
            }
            let handle = libc::dlopen(name, libc::RTLD_LAZY | libc::RTLD_NOLOAD);
            if handle.is_null() {
                continue;
            }
            let found = libc::dlsym(handle, symbol.as_ptr());
            libc::dlclose(handle);
            if !found.is_null() && found.cast_const() != ours {
                return true;
            }
        }
    }
    false
}

/// プロセスがもう読み込んだライブラリ（読み込みはしない）のハンドル。
pub fn loaded(name: &CStr) -> Option<usize> {
    // SAFETY: RTLD_NOLOAD はライブラリを探すだけ。ハンドルはプロセスの間有効
    // （取った参照は手放さない）。
    let handle = unsafe { libc::dlopen(name.as_ptr(), libc::RTLD_LAZY | libc::RTLD_NOLOAD) };
    (!handle.is_null()).then_some(handle as usize)
}
