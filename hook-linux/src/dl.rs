//! 動的リンカ：LWJGL が読み込んだライブラリのシンボルと、ゲームが読み込んだ後にエージェント自身が
//! 使うライブラリ（libGL、libX11）。

use std::ffi::{CStr, CString, c_char, c_int, c_void};

/// `dlopen` のハンドル `library` の中の `name` のアドレス。
pub fn symbol(library: usize, name: &CStr) -> Option<*const c_void> {
    // SAFETY: LWJGL が dlopen で得たハンドル。NUL で終わる名前。
    let p = unsafe { libc::dlsym(library as *mut c_void, name.as_ptr()) };
    (!p.is_null()).then_some(p.cast_const())
}

/// glibc の `struct link_map`（読むところまで）。
#[repr(C)]
struct LinkMap {
    l_addr: usize,
    l_name: *const c_char,
}

/// `dlopen` のハンドルのライブラリを読み込んだ場所。
pub fn path(library: usize) -> Option<String> {
    let mut map: *mut LinkMap = std::ptr::null_mut();
    // SAFETY: RTLD_DI_LINKMAP はライブラリのリンクマップへのポインタを書く。
    let ok = unsafe {
        libc::dlinfo(
            library as *mut c_void,
            libc::RTLD_DI_LINKMAP,
            (&raw mut map).cast(),
        )
    } == 0;
    if !ok || map.is_null() {
        return None;
    }
    // SAFETY: 動的リンカのリンクマップ。ライブラリを読み込んでいる間は生きている。
    let name = unsafe { (*map).l_name };
    // SAFETY: 上と同じ。NUL で終わるパス。
    (!name.is_null()).then(|| {
        unsafe { CStr::from_ptr(name) }
            .to_string_lossy()
            .into_owned()
    })
}

/// このライブラリ以外の読み込み済みのライブラリが `symbol` を export しているか（別のフォルダの
/// エージェントの別のコピー：動的リンカはパスごとに別のライブラリとして読み込む）。
pub fn loaded_elsewhere(symbol: &CStr, ours: *const c_void) -> bool {
    unsafe extern "C" fn collect(
        info: *mut libc::dl_phdr_info,
        _size: usize,
        names: *mut c_void,
    ) -> c_int {
        // SAFETY: 呼び出しの間の、1 つのオブジェクトのリンカの項目。`names` は自分のもの。
        unsafe {
            let name = (*info).dlpi_name;
            if !name.is_null() && *name != 0 {
                (*names.cast::<Vec<CString>>()).push(CStr::from_ptr(name).to_owned());
            }
        }
        0
    }
    let mut names: Vec<CString> = Vec::new();
    // SAFETY: コールバックは各項目を読んで `names` に足すだけ。
    unsafe { libc::dl_iterate_phdr(Some(collect), (&raw mut names).cast()) };
    names.iter().any(|name| {
        // SAFETY: RTLD_NOLOAD はライブラリを探すだけ。参照はまた手放す。
        unsafe {
            let handle = libc::dlopen(name.as_ptr(), libc::RTLD_LAZY | libc::RTLD_NOLOAD);
            if handle.is_null() {
                return false;
            }
            let found = libc::dlsym(handle, symbol.as_ptr());
            libc::dlclose(handle);
            !found.is_null() && found.cast_const() != ours
        }
    })
}

/// プロセスがもう読み込んだライブラリ（読み込みはしない）のハンドル。
pub fn loaded(name: &CStr) -> Option<usize> {
    // SAFETY: RTLD_NOLOAD はライブラリを探すだけ。ハンドルはプロセスの間有効
    // （取った参照は手放さない）。
    let handle = unsafe { libc::dlopen(name.as_ptr(), libc::RTLD_LAZY | libc::RTLD_NOLOAD) };
    (!handle.is_null()).then_some(handle as usize)
}
