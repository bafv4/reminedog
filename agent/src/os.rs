//! What the agent needs from the operating system, implemented by the platform's agent library
//! (`hook-win` for Windows) and set once in `Agent_OnLoad` ([`set`]).
//!
//! Handles are opaque `usize`s, whatever the OS calls them: a library is the handle LWJGL
//! looked functions up in (an `HMODULE`, a `dlopen` handle), a native window the window
//! library's (an `HWND`, an X11 `Window`), a surface what a GL context draws on (an `HDC`, a
//! GLX drawable or EGL surface) and a context a GL context (an `HGLRC`, a `GLXContext`, an
//! `EGLContext`).

use std::ffi::{CStr, c_void};
use std::path::Path;
use std::sync::OnceLock;

use reminedog_core::{InputId, Settings};
use reminedog_render::{
    BrowserAction, BrowserCommand, BrowserPixels, BrowserView, Notice, PointerSpeed,
};

pub trait Os: Sync {
    /// The address of the exported function `name` of `library`.
    fn symbol(&self, library: usize, name: &CStr) -> Option<*const c_void>;
    /// Where `library` was loaded from, for the log.
    fn library_path(&self, library: usize) -> String;

    /// GLFW's function giving a window's native window (`glfwGetWin32Window`), if it has one.
    fn glfw_native_window(&self) -> Option<&'static CStr>;
    /// The SDL3 window property holding the native window.
    fn sdl_native_window(&self) -> Option<SdlProperty>;

    /// The surface the game's context draws `native_window` on, for the overlay's context
    /// (else the surface current on this thread); 0 if there is none.
    fn window_surface(&self, native_window: Option<usize>) -> usize;
    /// The surface's pixel format, for the log.
    fn describe_surface(&self, surface: usize) -> String;
    /// The surface and context current on this thread (0 if none).
    fn current_gl(&self) -> (usize, usize);
    /// Only the context current on this thread (0 if none): the zoom asks at the game's
    /// framebuffer binds.
    fn current_context(&self) -> usize {
        self.current_gl().1
    }
    /// A GL function for the current context, null if there is none.
    fn gl_function(&self, name: &CStr) -> *const c_void;
    /// A new context for `surface`, with its pixel format.
    fn create_context(&self, surface: usize) -> Result<usize, String>;
    /// Makes `context` current on `surface` (0 and 0: none).
    fn make_current(&self, surface: usize, context: usize) -> Result<(), String>;
    /// Deletes a context that is not current.
    fn delete_context(&self, context: usize) -> Result<(), String>;
    /// Whether the overlay's context can be made on this thread; if not, the overlay waits for
    /// a swap on one that can (macOS: AppKit attaches it to the view on the main thread only,
    /// and Forge's early loading screen swaps from a thread of its own).
    fn can_create_context_here(&self) -> bool {
        true
    }
    /// Whether the OS shows `native_window` minimized, where it can tell cheaply; `None` asks
    /// the window library (Windows answers: SDL3's flag changes only as the game polls).
    fn window_minimized(&self, _native_window: Option<usize>) -> Option<bool> {
        None
    }

    /// Whether the system reports the key or mouse button as up now; false when it cannot
    /// tell (the safety net for releases the game never got, [`crate::rebind_state`]).
    fn input_up(&self, id: InputId) -> bool;
    /// How the system's pointer moves for relative motion (the overlay's own cursor).
    fn pointer_speed(&self) -> PointerSpeed;
    /// The clipboard's text, for the menu (`native_window` owns the clipboard while read).
    fn read_clipboard(&self, native_window: Option<usize>) -> Option<String>;
    /// Puts the menu's copied text on the clipboard.
    fn write_clipboard(&self, native_window: Option<usize>, text: &str) -> bool;
    /// Reads the font for Japanese text (on a thread of its own; [`crate::fonts`] keeps it).
    fn japanese_font(&self) -> Option<FontFile>;
    /// The in-game browser, where there is one.
    fn browser(&self) -> Option<&dyn Browser>;

    /// Whether another copy of the agent (from another folder, with statics of its own) runs in
    /// this process; this one then stays off.
    fn another_copy_runs(&self) -> bool {
        false
    }
    /// Tells a problem where no log can be written.
    fn report(&self, message: &str) {
        eprintln!("reminedog: {message}");
    }
    /// Where a panic happened, for the log (called from the panic hook).
    fn panic_details(&self) -> Option<String> {
        None
    }
}

/// A font file read for the overlay.
pub struct FontFile {
    pub name: String,
    pub data: Vec<u8>,
    /// The face in a collection (`.ttc`); 0 for a plain font.
    pub index: u32,
}

/// How SDL3 gives a window's native window.
#[derive(Clone, Copy)]
pub enum SdlProperty {
    /// A pointer property (`SDL.window.win32.hwnd`).
    Pointer(&'static CStr),
    /// A number property (`SDL.window.x11.window`).
    Number(&'static CStr),
}

/// The in-game browser, from the overlay's side ([`crate::frame`]).
pub trait Browser: Sync {
    /// What the overlay shows of the browser this frame, and the browser's new notices.
    fn before_frame(&self) -> (BrowserView, Vec<Notice>);
    /// Calls `upload` with the newest picture of the page if it changed since `version`
    /// (which it updates).
    fn with_new_picture(&self, version: &mut u64, upload: &mut dyn FnMut(BrowserPixels<'_>));
    /// Carries out what the overlay and the browser's hotkeys asked for, in order.
    fn after_frame(
        &self,
        commands: Vec<BrowserCommand>,
        actions: Vec<BrowserAction>,
        settings: &Settings,
        game_dir: &Path,
    );
}

/// No picture taken yet, for [`Browser::with_new_picture`].
pub const NO_PICTURE: u64 = u64::MAX;

static OS: OnceLock<&'static dyn Os> = OnceLock::new();

/// Sets the OS for the process; false if one was set already.
pub fn set(os: &'static dyn Os) -> bool {
    OS.set(os).is_ok()
}

/// The OS. Panics before [`set`]: the agent's entry point sets it before any hook can run.
pub fn get() -> &'static dyn Os {
    *OS.get().expect("the OS is set in Agent_OnLoad")
}
