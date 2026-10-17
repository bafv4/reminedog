//! Linux agent loaded with `-agentpath:<path>/libreminedog_linux.so[=<options>]`.
//!
//! The agent itself is OS-independent ([`reminedog_agent`]); this library adds what it needs
//! from Linux ([`linux`]: symbol lookup with `dlsym`, GLX contexts ([`glx`]), Japanese fonts
//! from fontconfig ([`fonts`])) and the JVMTI entry point ([`agent`]).
//!
//! Only X11 with GLX so far; the key state, the clipboard and the pointer's speed are not read
//! yet, and there is no in-game browser.
//!
//! Everything is Linux-only; on other platforms this crate builds as an empty library so the
//! workspace still builds.
#![cfg(target_os = "linux")]

mod agent;
mod dl;
mod fonts;
mod glx;
mod linux;

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
        agent::on_load(&options, vm);
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
