//! The agent, apart from what depends on the operating system, which the platform's agent
//! library supplies ([`os::Os`]; `hook-win` builds `reminedog.dll` for Windows) along with
//! the JVMTI entry point.
//!
//! - [`jvmti`]: the JVM reports LWJGL's natives as it binds them; LWJGL's symbol lookups are
//!   replaced so it is handed the hooks ([`hook`]) of the game's window library ([`library`])
//! - [`glfw`], [`glfw_input`] (Minecraft up to 1.21), [`sdl`], [`sdl_input`] (26.x): the hooks,
//!   input and the key rebinding's view of the key state ([`rebind_state`])
//! - [`frame`]: the overlay drawn at each buffer swap with the agent's own context ([`gl`]),
//!   and the high-resolution zoom ([`tall`])
//! - [`f3c`], [`waypoints`]: F3+C requests and their results, the waypoints of the current world
//! - [`input`]: the input router shared by the window library's hooks and the overlay
//! - [`saver`]: the thread that writes files; [`globals`]: the options and the game folder
//! - [`start`]: the `Agent_OnLoad` sequence, which the platform's library calls

mod f3c;
mod fonts;
mod frame;
mod gl;
mod glfw;
mod glfw_input;
pub mod globals;
mod hook;
mod input;
pub mod jvmti;
mod library;
pub mod os;
mod rebind_state;
mod saver;
mod sdl;
mod sdl_input;
mod start;
mod tall;
mod waypoints;

pub use start::start;

/// The version: the release's (the release workflow sets REMINEDOG_VERSION), otherwise the
/// crate's (`build.rs`).
pub const VERSION: &str = env!("REMINEDOG_VERSION");

/// Identifies the build in logs; CI sets it to the commit hash ("local" otherwise).
pub const BUILD_ID: &str = env!("REMINEDOG_BUILD_ID");

use std::panic::{AssertUnwindSafe, catch_unwind};

/// Runs `f`, turning a panic into a log entry. Every function the JVM, LWJGL or a library
/// calls into must go through this: a panic unwinding into foreign frames aborts the process,
/// which here means crashing the game.
pub fn catch<R>(what: &str, f: impl FnOnce() -> R) -> Option<R> {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(value) => Some(value),
        Err(_) => {
            // The panic hook already logged the message and location.
            log::error!("recovered from a panic in {what}");
            None
        }
    }
}
