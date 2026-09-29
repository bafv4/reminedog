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

/// F1 to F25, for the platforms' key tables.
pub const FUNCTION_KEYS: [reminedog_render::Key; 25] = {
    use reminedog_render::Key::*;
    [
        F1, F2, F3, F4, F5, F6, F7, F8, F9, F10, F11, F12, F13, F14, F15, F16, F17, F18, F19, F20,
        F21, F22, F23, F24, F25,
    ]
};
