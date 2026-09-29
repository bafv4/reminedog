//! The input router shared by the GLFW and SDL3 hooks and the frame code.

use std::sync::{Mutex, MutexGuard};

use reminedog_render::{InputRouter, Modifiers};

static ROUTER: Mutex<InputRouter> = Mutex::new(InputRouter::new());

/// The router. Callers must not call game code while holding it.
pub fn router() -> MutexGuard<'static, InputRouter> {
    ROUTER.lock().unwrap_or_else(|e| e.into_inner())
}

/// egui modifiers from the three flags every platform reports.
pub fn modifiers(shift: bool, ctrl: bool, alt: bool) -> Modifiers {
    Modifiers {
        alt,
        ctrl,
        shift,
        mac_cmd: false,
        command: ctrl,
    }
}
