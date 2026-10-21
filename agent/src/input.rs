//! GLFW と SDL3 のフックとフレームの処理が共有する入力の振り分け（router）。

use std::sync::{Mutex, MutexGuard};

use reminedog_render::{InputRouter, Modifiers};

static ROUTER: Mutex<InputRouter> = Mutex::new(InputRouter::new());

/// router。持っている間は、ゲームのコードを呼ばないこと。
pub fn router() -> MutexGuard<'static, InputRouter> {
    ROUTER.lock().unwrap_or_else(|e| e.into_inner())
}

/// どの OS でも報告される 3 つのフラグから作る egui の修飾キー。
pub fn modifiers(shift: bool, ctrl: bool, alt: bool) -> Modifiers {
    Modifiers {
        alt,
        ctrl,
        shift,
        mac_cmd: false,
        command: ctrl,
    }
}

/// F1〜F25（各 OS のキーの表のため）。
pub const FUNCTION_KEYS: [reminedog_render::Key; 25] = {
    use reminedog_render::Key::*;
    [
        F1, F2, F3, F4, F5, F6, F7, F8, F9, F10, F11, F12, F13, F14, F15, F16, F17, F18, F19, F20,
        F21, F22, F23, F24, F25,
    ]
};
