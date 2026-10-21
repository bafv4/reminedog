//! LWJGL に渡すもの（[`crate::jvmti`]）：ゲームのウィンドウのライブラリのフック（LWJGL が
//! 最初に関数を引いた GLFW か SDL3 だけをフックする。GLFW のゲームでコントローラーのために
//! Mod が読み込む SDL には触れない）と、LWJGL の GL のネイティブのズーム用の差し替え。

use std::ffi::{CStr, c_void};
use std::sync::Mutex;

use crate::hook::Hook;
use crate::os;

/// LWJGL が関数を引いた、フックしないライブラリ（それぞれ 1 回だけ調べる）。ライブラリを
/// 調べる間は持ったままにし、2 つのスレッドが同時に取り付けないようにする。
static PASSED: Mutex<Vec<usize>> = Mutex::new(Vec::new());

/// `library` で引かれた `name` の代わりに LWJGL に渡すフック。
pub fn hook(library: usize, name: &CStr) -> Option<&'static Hook> {
    identify(library);
    let hooks: &[&[Hook]] = if crate::glfw::is_hooked(library) {
        &[crate::glfw::hooks(), crate::glfw_input::hooks()]
    } else if crate::sdl::is_hooked(library) {
        &[crate::sdl::hooks(), crate::sdl_input::hooks()]
    } else {
        return None;
    };
    hooks
        .iter()
        .flat_map(|hooks| hooks.iter())
        .find(|hook| hook.name == name)
}

/// `class` の LWJGL の `name` の代わりのネイティブ：ズームの GL の関数。
pub fn native(
    class: &str,
    name: &str,
    signature: &str,
    address: *const c_void,
) -> Option<*const c_void> {
    if class.starts_with("Lorg/lwjgl/opengl/") {
        crate::tall::wrap_native(name, signature, address)
    } else {
        None
    }
}

/// `library`（LWJGL が読み込んだライブラリ）の export の関数のアドレス。
pub fn export(library: usize, name: &CStr) -> Option<*const c_void> {
    os::get().symbol(library, name)
}

/// `library` が、LWJGL が関数を引いた最初の GLFW か SDL3 ならフックする。
fn identify(library: usize) {
    if library == 0 || crate::glfw::is_hooked(library) || crate::sdl::is_hooked(library) {
        return;
    }
    let mut passed = PASSED.lock().unwrap_or_else(|e| e.into_inner());
    // このスレッドがロックを待つ間に、別のスレッドがフックしたかもしれない。
    if passed.contains(&library)
        || crate::glfw::is_hooked(library)
        || crate::sdl::is_hooked(library)
    {
        return;
    }
    let path = || os::get().library_path(library);
    if crate::glfw::is_glfw(library) {
        if crate::sdl::attached() {
            log::warn!("a GLFW library used after SDL3 is ignored: {}", path());
        } else {
            crate::glfw::attach(library, &path());
        }
    } else if crate::sdl::is_sdl3(library) {
        if crate::glfw::attached() {
            log::warn!(
                "an SDL3 library used after GLFW (a mod's) is ignored: {}",
                path()
            );
        } else {
            crate::sdl::attach(library, &path());
        }
    }
    if !crate::glfw::is_hooked(library) && !crate::sdl::is_hooked(library) {
        passed.push(library);
    }
}
