//! エージェントが macOS に頼ること（[`reminedog_agent::os::Os`]）。NSOpenGLContext の OpenGL。

use std::ffi::{CStr, c_void};

use reminedog_agent::os::{Browser, FontFile, Os, SdlProperty};
use reminedog_core::InputId;
use reminedog_render::PointerSpeed;

use crate::{clipboard, dl, fonts, nsgl};

pub struct MacOs;

pub static MACOS: MacOs = MacOs;

impl Os for MacOs {
    fn symbol(&self, library: usize, name: &CStr) -> Option<*const c_void> {
        dl::symbol(library, name)
    }

    fn library_path(&self, library: usize) -> String {
        dl::path(library).unwrap_or_else(|| format!("0x{library:x}"))
    }

    /// `NSWindow`。
    fn glfw_native_window(&self) -> Option<&'static CStr> {
        Some(c"glfwGetCocoaWindow")
    }

    fn sdl_native_window(&self) -> Option<SdlProperty> {
        Some(SdlProperty::Pointer(c"SDL.window.cocoa.window"))
    }

    /// ゲームの今の `NSOpenGLContext` が描くビュー。
    fn window_surface(&self, native_window: Option<usize>) -> usize {
        nsgl::window_view(native_window)
    }

    fn describe_surface(&self, surface: usize) -> String {
        nsgl::describe(surface)
    }

    fn current_gl(&self) -> (usize, usize) {
        nsgl::current()
    }

    fn current_context(&self) -> usize {
        nsgl::current_context()
    }

    fn gl_function(&self, name: &CStr) -> *const c_void {
        nsgl::function(name)
    }

    fn create_context(&self, surface: usize) -> Result<usize, String> {
        nsgl::create_context(surface)
    }

    fn make_current(&self, surface: usize, context: usize) -> Result<(), String> {
        nsgl::make_current(surface, context)
    }

    fn delete_context(&self, context: usize) -> Result<(), String> {
        nsgl::delete_context(context)
    }

    /// AppKit はメインスレッドでだけコンテキストをビューに付ける。
    fn can_create_context_here(&self) -> bool {
        nsgl::on_main_thread()
    }

    /// まだ読まない：安全網はキーを離さない。（`CGEventSourceKeyState` には入力監視の許可が要る
    /// かもしれない。許可がないと、どのキーも離しているように見える。）
    fn input_up(&self, _id: InputId) -> bool {
        false
    }

    /// まだ読まない：相対移動はそのままオーバーレイのカーソルを動かす。
    fn pointer_speed(&self) -> PointerSpeed {
        PointerSpeed::RAW
    }

    fn read_clipboard(&self, _native_window: Option<usize>) -> Option<String> {
        clipboard::read()
    }

    fn write_clipboard(&self, _native_window: Option<usize>, text: &str) -> bool {
        clipboard::write(text)
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
