//! The agent's parts that do not depend on the OS, used by the platform's agent library
//! (`hook-win` builds `reminedog.dll`).
//!
//! - [`jvmti`]: the JVM reports LWJGL's natives as it binds them; LWJGL's symbol lookups are
//!   replaced so it is handed the platform's hooks ([`hook`])
//! - [`f3c`], [`waypoints`]: F3+C requests and their results, the waypoints of the current world
//! - [`input`]: the input router shared by the window library's hooks and the overlay
//! - [`saver`]: the thread that writes files

pub mod f3c;
pub mod hook;
pub mod input;
pub mod jvmti;
pub mod saver;
pub mod waypoints;

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
