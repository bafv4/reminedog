//! エージェントが Linux に頼ること（[`reminedog_agent::os::Os`]）。今は GLX の X11。

use std::ffi::{CStr, c_void};

use reminedog_agent::os::{Browser, FontFile, Os, SdlProperty};
use reminedog_core::InputId;
use reminedog_render::PointerSpeed;

use crate::{dl, fonts, glx};

pub struct Linux;

pub static LINUX: Linux = Linux;

impl Os for Linux {
    fn symbol(&self, library: usize, name: &CStr) -> Option<*const c_void> {
        dl::symbol(library, name)
    }

    fn library_path(&self, library: usize) -> String {
        dl::path(library).unwrap_or_else(|| format!("0x{library:x}"))
    }

    fn glfw_native_window(&self) -> Option<&'static CStr> {
        Some(c"glfwGetX11Window")
    }

    fn sdl_native_window(&self) -> Option<SdlProperty> {
        Some(SdlProperty::Number(c"SDL.window.x11.window"))
    }

    /// スワップのときに今の GLX の描画先：GLFW は X のウィンドウそのものではなく、自分の GLX の
    /// ウィンドウ（`glXCreateWindow`）に描く。
    fn window_surface(&self, native_window: Option<usize>) -> usize {
        match glx::current() {
            (0, _) => native_window.unwrap_or(0),
            (drawable, _) => drawable,
        }
    }

    fn describe_surface(&self, surface: usize) -> String {
        glx::describe(surface)
    }

    fn current_gl(&self) -> (usize, usize) {
        glx::current()
    }

    fn current_context(&self) -> usize {
        glx::current_context()
    }

    fn gl_function(&self, name: &CStr) -> *const c_void {
        glx::function(name)
    }

    fn create_context(&self, surface: usize) -> Result<usize, String> {
        glx::create_context(surface)
    }

    fn make_current(&self, surface: usize, context: usize) -> Result<(), String> {
        glx::make_current(surface, context)
    }

    fn delete_context(&self, context: usize) -> Result<(), String> {
        glx::delete_context(context)
    }

    /// まだ読まない：安全網はキーを離さない。
    fn input_up(&self, _id: InputId) -> bool {
        false
    }

    /// まだ読まない：相対移動はそのままオーバーレイのカーソルを動かす。
    fn pointer_speed(&self) -> PointerSpeed {
        PointerSpeed::RAW
    }

    fn read_clipboard(&self, _native_window: Option<usize>) -> Option<String> {
        None
    }

    fn write_clipboard(&self, _native_window: Option<usize>, _text: &str) -> bool {
        false
    }

    fn japanese_font(&self) -> Option<FontFile> {
        fonts::read()
    }

    fn browser(&self) -> Option<&dyn Browser> {
        None
    }

    fn another_copy_runs(&self) -> bool {
        dl::loaded_elsewhere(
            c"REMINEDOG_AGENT_MARKER",
            (&raw const crate::REMINEDOG_AGENT_MARKER).cast(),
        )
    }
}
