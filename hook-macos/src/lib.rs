//! macOS agent loaded with `-agentpath:<path>/libreminedog_macos.dylib[=<options>]`.
//!
//! The agent itself is OS-independent ([`reminedog_agent`]); this library adds what it needs
//! from macOS ([`macos`]: symbol lookup with `dlsym`, OpenGL contexts with `NSOpenGLContext`
//! ([`nsgl`]), the pasteboard ([`clipboard`]), a Japanese font ([`fonts`])) and the JVMTI entry
//! point.
//!
//! A prototype: the key state and the pointer's speed are not read yet, and there is no
//! in-game browser.
//!
//! Everything is macOS-only; on other platforms this crate builds as an empty library so the
//! workspace still builds.
#![cfg(target_os = "macos")]

mod clipboard;
mod dl;
mod fonts;
mod macos;
mod nsgl;
mod objc;

use std::ffi::{CStr, c_char, c_void};

const JNI_OK: i32 = 0;

/// JVMTI entry point. The JVM calls it very early during startup, before any Java code
/// runs and before LWJGL loads GLFW.
///
/// # Safety
/// Called by the JVM with its `JavaVM*`; `options` is null or a NUL-terminated string.
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
            // SAFETY: the JVM passes a NUL-terminated string.
            unsafe { CStr::from_ptr(options) }
                .to_string_lossy()
                .into_owned()
        };
        // SAFETY: the JVM's JavaVM.
        unsafe { reminedog_agent::start(&options, vm, &macos::MACOS) };
    });
    // Whatever happened, never keep the game from starting.
    JNI_OK
}

/// Called by the JVM at shutdown.
///
/// # Safety
/// Called by the JVM.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Agent_OnUnload(_vm: *mut c_void) {
    reminedog_agent::catch("Agent_OnUnload", || log::info!("JVM shutting down"));
}
