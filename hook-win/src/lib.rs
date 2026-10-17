//! Windows agent loaded with `-agentpath:<path>\reminedog.dll[=<options>]`.
//!
//! Flow: `Agent_OnLoad` sets up logging and has the JVM report LWJGL's natives as it binds
//! them ([`jvmti`]). When LWJGL looks up a function of GLFW (or SDL3, [`library`]), it is
//! handed the agent's hook ([`hook`], [`glfw`]). From then on every frame goes through
//! [`frame::before_swap`], which draws the overlay with the agent's own GL context ([`wgl`])
//! and hands the swap back to GLFW. Waypoints
//! ([`waypoints`]) get the player's position by sending the game F3+C ([`f3c`]).
//!
//! Everything is Windows-only; on other platforms this crate builds as an empty library
//! so the workspace still builds.
#![cfg(windows)]

mod agent;
mod browser;
mod clipboard;
mod ffi;
mod fonts;
mod frame;
mod glfw;
mod glfw_input;
mod library;
mod pointer;
mod rebind_state;
mod sdl;
mod sdl_input;
mod tall;
mod wgl;

use std::ffi::{CStr, c_char, c_void};

// The OS-independent parts, at the paths the modules here use.
use reminedog_agent::{f3c, hook, input, jvmti, saver, waypoints};

const JNI_OK: i32 = 0;

/// JVMTI entry point. The JVM calls it very early during startup, before any Java code
/// runs and before LWJGL loads GLFW.
///
/// # Safety
/// Called by the JVM; `options` is null or a NUL-terminated string.
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
            // SAFETY: the JVM passes a NUL-terminated string.
            ffi::decode_command_line_text(unsafe { CStr::from_ptr(options) }.to_bytes())
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
pub unsafe extern "system" fn Agent_OnUnload(_vm: *mut c_void) {
    ffi::catch("Agent_OnUnload", || log::info!("JVM shutting down"));
}
