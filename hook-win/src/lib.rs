//! `-agentpath:<path>\reminedog.dll[=<options>]` で読み込む Windows のエージェント。
//!
//! エージェント本体は OS に依存しない（[`reminedog_agent`]）。このライブラリは、それが Windows に
//! 頼ること（[`windows`]：WGL のコンテキスト、物理のキーの状態、クリップボード、ポインターの速度、
//! 日本語フォント、WebView2 のブラウザ）と、JVMTI の入口を足す。
//!
//! 流れ：`Agent_OnLoad` がログを用意し、JVM に、LWJGL のネイティブを結び付けるたびに知らせて
//! もらう（[`reminedog_agent::jvmti`]）。LWJGL が GLFW（か SDL3）の関数を引くと、エージェントの
//! フックを渡す。それからは、どのフレームもエージェントのスワップのフックを通り、フックは
//! エージェント自前の GL のコンテキストでオーバーレイを描き、スワップを GLFW に返す。
//!
//! すべて Windows だけのもの。ほかの OS では、ワークスペースがビルドできるように、この crate は
//! 空のライブラリとしてビルドする。
#![cfg(windows)]

mod browser;
mod clipboard;
mod ffi;
mod fonts;
mod keys;
mod pointer;
mod startup;
mod wgl;
mod windows;

use std::ffi::{CStr, c_char, c_void};

const JNI_OK: i32 = 0;

/// JVMTI の入口。JVM は起動のとても早い時期、Java のコードが動く前、LWJGL が GLFW を読み込む前に
/// これを呼ぶ。
///
/// # Safety
/// JVM が呼ぶ。`options` は null か NUL で終わる文字列。
#[unsafe(no_mangle)]
pub unsafe extern "system" fn Agent_OnLoad(
    vm: *mut c_void,
    options: *const c_char,
    _reserved: *mut c_void,
) -> i32 {
    ffi::catch("Agent_OnLoad", || {
        let options = if options.is_null() {
            String::new()
        } else {
            // SAFETY: JVM は NUL で終わる文字列を渡す。
            ffi::decode_command_line_text(unsafe { CStr::from_ptr(options) }.to_bytes())
        };
        // SAFETY: JVM の JavaVM。
        unsafe { reminedog_agent::start(&options, vm, &windows::WINDOWS) };
    });
    // 何が起きても、ゲームの起動を止めない。
    JNI_OK
}

/// 終了時に JVM が呼ぶ。
///
/// # Safety
/// JVM が呼ぶ。
#[unsafe(no_mangle)]
pub unsafe extern "system" fn Agent_OnUnload(_vm: *mut c_void) {
    ffi::catch("Agent_OnUnload", || log::info!("JVM shutting down"));
}
