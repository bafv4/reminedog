//! エージェントが Windows に頼ること（[`reminedog_agent::os::Os`]）。

use std::ffi::{CStr, c_void};

use reminedog_agent::os::{Browser, FontFile, Os, SdlProperty};
use reminedog_core::InputId;
use reminedog_render::PointerSpeed;
use windows_sys::Win32::Foundation::{HMODULE, HWND};
use windows_sys::Win32::Graphics::Gdi::{GetDC, HDC};
use windows_sys::Win32::Graphics::OpenGL::HGLRC;
use windows_sys::Win32::UI::WindowsAndMessaging::IsIconic;

use crate::{browser, clipboard, ffi, fonts, keys, pointer, startup, wgl};

pub struct Windows;

pub static WINDOWS: Windows = Windows;

impl Os for Windows {
    fn symbol(&self, library: usize, name: &CStr) -> Option<*const c_void> {
        ffi::proc_address(library as HMODULE, name)
    }

    fn library_path(&self, library: usize) -> String {
        ffi::module_path(Some(library as HMODULE))
            .map(|p| p.display().to_string())
            .unwrap_or_default()
    }

    fn glfw_native_window(&self) -> Option<&'static CStr> {
        Some(c"glfwGetWin32Window")
    }

    fn sdl_native_window(&self) -> Option<SdlProperty> {
        Some(SdlProperty::Pointer(c"SDL.window.win32.hwnd"))
    }

    /// ウィンドウのデバイスコンテキスト。GLFW と SDL のウィンドウのクラスは CS_OWNDC を持つので、
    /// 今どのコンテキストが今のものでも（どちらのスワップの関数も、ウィンドウ自身のコンテキストが
    /// 今のものであることを求めない）、これはゲームのコンテキストが描き、スワップが見せる DC そのもの。
    fn window_surface(&self, native_window: Option<usize>) -> usize {
        if let Some(hwnd) = native_window {
            // SAFETY: 有効なウィンドウのハンドル。CS_OWNDC のウィンドウの DC は解放しなくてよい。
            let dc = unsafe { GetDC(hwnd as HWND) };
            if !dc.is_null() {
                return dc as usize;
            }
        }
        self.current_gl().0
    }

    /// OS が今持っているウィンドウ：最小化したウィンドウの GetDC は呼ぶたびに新しい一時的な DC を
    /// 返し、SDL3 のフラグはゲームがポーリングするまで遅れる。
    fn window_minimized(&self, native_window: Option<usize>) -> Option<bool> {
        // SAFETY: ウィンドウのハンドル（古いものは false を答えるだけ）。
        native_window.map(|hwnd| unsafe { IsIconic(hwnd as HWND) } != 0)
    }

    fn describe_surface(&self, surface: usize) -> String {
        wgl::describe_pixel_format(surface as HDC)
    }

    fn current_gl(&self) -> (usize, usize) {
        wgl::get().map_or((0, 0), |wgl| {
            let (dc, context) = wgl.current();
            (dc as usize, context as usize)
        })
    }

    fn current_context(&self) -> usize {
        wgl::get().map_or(0, |wgl| wgl.current_context() as usize)
    }

    fn gl_function(&self, name: &CStr) -> *const c_void {
        wgl::get().map_or(std::ptr::null(), |wgl| wgl.load(name))
    }

    fn create_context(&self, surface: usize) -> Result<usize, String> {
        wgl::get()?
            .create_context(surface as HDC)
            .map(|context| context as usize)
    }

    fn make_current(&self, surface: usize, context: usize) -> Result<(), String> {
        wgl::get()?.make_current(surface as HDC, context as HGLRC)
    }

    fn delete_context(&self, context: usize) -> Result<(), String> {
        wgl::get()?.delete_context(context as HGLRC)
    }

    fn input_up(&self, id: InputId) -> bool {
        keys::is_up(id)
    }

    fn pointer_speed(&self) -> PointerSpeed {
        pointer::system_speed()
    }

    fn read_clipboard(&self, native_window: Option<usize>) -> Option<String> {
        clipboard::read_text(native_window? as HWND)
    }

    fn write_clipboard(&self, native_window: Option<usize>, text: &str) -> bool {
        native_window.is_some_and(|hwnd| clipboard::write_text(hwnd as HWND, text))
    }

    fn japanese_font(&self) -> Option<FontFile> {
        fonts::read()
    }

    fn browser(&self) -> Option<&dyn Browser> {
        Some(&browser::WEBVIEW2)
    }

    fn another_copy_runs(&self) -> bool {
        startup::another_copy_runs()
    }

    fn report(&self, message: &str) {
        startup::debug_output(message);
    }

    fn panic_details(&self) -> Option<String> {
        Some(startup::return_addresses())
    }
}
