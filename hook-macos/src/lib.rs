//! `-agentpath:<path>/libreminedog_macos.dylib[=<options>]` で読み込む macOS のエージェント。
//!
//! エージェント本体は OS に依存しない（[`reminedog_agent`]）。このライブラリは、それが macOS に
//! 頼ること（[`macos`]：`dlsym` での関数の引き当て、`NSOpenGLContext` の OpenGL のコンテキスト
//! （[`nsgl`]）、ペーストボード（[`clipboard`]）、日本語フォント（[`fonts`]））と、JVMTI の入口を
//! 足す。
//!
//! 試作：キーの状態とポインターの速度はまだ読まず、ゲーム内ブラウザはない。
//!
//! すべて macOS だけのもの。ほかの OS では、ワークスペースがビルドできるように、この crate は
//! 空のライブラリとしてビルドする。
#![cfg(target_os = "macos")]

mod clipboard;
mod dl;
mod fonts;
mod macos;
mod nsgl;
mod objc;

use std::ffi::{CStr, c_char, c_void};

const JNI_OK: i32 = 0;

/// プロセスの読み込み済みのライブラリの中でこのライブラリに印を付ける。別のコピーが見つけるため
/// （[`dl::loaded_elsewhere`]）。
#[unsafe(no_mangle)]
pub static REMINEDOG_AGENT_MARKER: u8 = 0;

/// JVMTI の入口。JVM は起動のとても早い時期、Java のコードが動く前、LWJGL が GLFW を読み込む前に
/// これを呼ぶ。
///
/// # Safety
/// JVM が自分の `JavaVM*` で呼ぶ。`options` は null か NUL で終わる文字列。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Agent_OnLoad(
    vm: *mut c_void,
    options: *const c_char,
    _reserved: *mut c_void,
) -> i32 {
    reminedog_agent::catch("Agent_OnLoad", || {
        let options = if options.is_null() {
            String::new()
        } else {
            // SAFETY: JVM は NUL で終わる文字列を渡す。
            unsafe { CStr::from_ptr(options) }
                .to_string_lossy()
                .into_owned()
        };
        // SAFETY: JVM の JavaVM。
        unsafe { reminedog_agent::start(&options, vm, &macos::MACOS) };
    });
    // 何が起きても、ゲームの起動を止めない。
    JNI_OK
}

/// 終了時に JVM が呼ぶ。
///
/// # Safety
/// JVM が呼ぶ。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Agent_OnUnload(_vm: *mut c_void) {
    reminedog_agent::catch("Agent_OnUnload", || log::info!("JVM shutting down"));
}
