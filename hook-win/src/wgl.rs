//! WGL：Windows の OpenGL のコンテキスト。[`crate::windows`] のため（エージェントはこれで、
//! ゲームのデバイスコンテキストの上に自前のコンテキストを持つ）。
//!
//! opengl32.dll は import せずに動いている間に引く（GLFW が読み込む）ので、エージェントを
//! 読み込んでも、それがいつどこから読み込まれるかは変わらない。

use std::ffi::{CStr, c_char, c_void};
use std::sync::OnceLock;

use windows_sys::Win32::Foundation::{GetLastError, HMODULE};
use windows_sys::Win32::Graphics::Gdi::HDC;
use windows_sys::Win32::Graphics::OpenGL::{
    DescribePixelFormat, GetPixelFormat, HGLRC, PFD_DOUBLEBUFFER, PFD_GENERIC_ACCELERATED,
    PFD_GENERIC_FORMAT, PIXELFORMATDESCRIPTOR,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;

use crate::ffi;

type Bool = i32;

type CreateContextFn = unsafe extern "system" fn(HDC) -> HGLRC;
type DeleteContextFn = unsafe extern "system" fn(HGLRC) -> Bool;
type MakeCurrentFn = unsafe extern "system" fn(HDC, HGLRC) -> Bool;
type GetCurrentContextFn = unsafe extern "system" fn() -> HGLRC;
type GetCurrentDcFn = unsafe extern "system" fn() -> HDC;
type GetProcAddressFn = unsafe extern "system" fn(*const c_char) -> *const c_void;

pub struct Wgl {
    module: HMODULE,
    create_context: CreateContextFn,
    delete_context: DeleteContextFn,
    make_current: MakeCurrentFn,
    get_current_context: GetCurrentContextFn,
    get_current_dc: GetCurrentDcFn,
    get_proc_address: GetProcAddressFn,
}

// SAFETY: ただの関数ポインタとモジュールのハンドル。
unsafe impl Send for Wgl {}
// SAFETY: 上と同じ。作った後は何も変えない。
unsafe impl Sync for Wgl {}

static WGL: OnceLock<Result<Wgl, String>> = OnceLock::new();

/// opengl32.dll の入口。プロセスがそれを読み込んでいなければ失敗する。
pub fn get() -> Result<&'static Wgl, String> {
    WGL.get_or_init(Wgl::resolve).as_ref().map_err(Clone::clone)
}

impl Wgl {
    fn resolve() -> Result<Wgl, String> {
        let name = ffi::wide("opengl32.dll");
        // SAFETY: NUL で終わるワイド文字列。
        let module = unsafe { GetModuleHandleW(name.as_ptr()) };
        if module.is_null() {
            return Err("opengl32.dll is not loaded".into());
        }
        let proc = |name: &CStr| {
            ffi::proc_address(module, name)
                .ok_or_else(|| format!("opengl32.dll does not export {}", name.to_string_lossy()))
        };
        // SAFETY: これらの opengl32 の export の、文書どおりのシグネチャ。
        unsafe {
            Ok(Wgl {
                module,
                create_context: std::mem::transmute::<*const c_void, CreateContextFn>(proc(
                    c"wglCreateContext",
                )?),
                delete_context: std::mem::transmute::<*const c_void, DeleteContextFn>(proc(
                    c"wglDeleteContext",
                )?),
                make_current: std::mem::transmute::<*const c_void, MakeCurrentFn>(proc(
                    c"wglMakeCurrent",
                )?),
                get_current_context: std::mem::transmute::<*const c_void, GetCurrentContextFn>(
                    proc(c"wglGetCurrentContext")?,
                ),
                get_current_dc: std::mem::transmute::<*const c_void, GetCurrentDcFn>(proc(
                    c"wglGetCurrentDC",
                )?),
                get_proc_address: std::mem::transmute::<*const c_void, GetProcAddressFn>(proc(
                    c"wglGetProcAddress",
                )?),
            })
        }
    }

    /// このスレッドで今のコンテキストとデバイスコンテキスト（なければ null）。
    pub fn current(&self) -> (HDC, HGLRC) {
        // SAFETY: 前提はない。
        unsafe { ((self.get_current_dc)(), (self.get_current_context)()) }
    }

    /// このスレッドで今のコンテキスト（なければ null）。
    pub fn current_context(&self) -> HGLRC {
        // SAFETY: 前提はない。
        unsafe { (self.get_current_context)() }
    }

    /// 今のコンテキストの GL の関数を引く：拡張と GL 1.1 より後のものはドライバから
    /// wglGetProcAddress で、GL 1.1 のものは opengl32 の export からだけ。
    pub fn load(&self, name: &CStr) -> *const c_void {
        // SAFETY: NUL で終わる名前。
        let p = unsafe { (self.get_proc_address)(name.as_ptr()) };
        // 失敗したときに、null の代わりに小さな番兵の値を返すドライバがある。
        if matches!(p as isize, -1..=3) {
            ffi::proc_address(self.module, name).unwrap_or(std::ptr::null())
        } else {
            p
        }
    }

    /// ウィンドウのライブラリが `hdc` にもう設定したピクセルフォーマットのコンテキスト。
    pub fn create_context(&self, hdc: HDC) -> Result<HGLRC, String> {
        // SAFETY: `hdc` は、ゲームの今のコンテキストの DC。
        let hglrc = unsafe { (self.create_context)(hdc) };
        if hglrc.is_null() {
            // SAFETY: 前提はない。
            let error = unsafe { GetLastError() };
            return Err(format!("wglCreateContext failed (error {error})"));
        }
        Ok(hglrc)
    }

    /// `hglrc` を `hdc` で今のものにする（null と null：なし）。
    pub fn make_current(&self, hdc: HDC, hglrc: HGLRC) -> Result<(), String> {
        // SAFETY: この DC のために作ったコンテキスト、またはなし。
        if unsafe { (self.make_current)(hdc, hglrc) } == 0 {
            // SAFETY: 前提はない。
            let error = unsafe { GetLastError() };
            return Err(format!("wglMakeCurrent failed (error {error})"));
        }
        Ok(())
    }

    /// 今のものでないコンテキストを消す。
    pub fn delete_context(&self, hglrc: HGLRC) -> Result<(), String> {
        // SAFETY: 呼び出し側の、今のものでないコンテキスト。
        if unsafe { (self.delete_context)(hglrc) } == 0 {
            return Err("wglDeleteContext failed".into());
        }
        Ok(())
    }
}

/// ウィンドウの DC のピクセルフォーマット（ドライバやコンテキストの問題を調べるため）。
pub fn describe_pixel_format(hdc: HDC) -> String {
    // SAFETY: 有効な DC への、ただの GDI の問い合わせ。
    unsafe {
        let index = GetPixelFormat(hdc);
        if index == 0 {
            return "none".into();
        }
        let mut pfd: PIXELFORMATDESCRIPTOR = std::mem::zeroed();
        if DescribePixelFormat(
            hdc,
            index,
            size_of::<PIXELFORMATDESCRIPTOR>() as u32,
            &mut pfd,
        ) == 0
        {
            return format!("#{index} (DescribePixelFormat failed)");
        }
        let acceleration = if pfd.dwFlags & PFD_GENERIC_FORMAT == 0 {
            "ICD"
        } else if pfd.dwFlags & PFD_GENERIC_ACCELERATED != 0 {
            "MCD"
        } else {
            "software (GDI generic)"
        };
        format!(
            "#{index}: color {} alpha {} depth {} stencil {}, double-buffered {}, {acceleration}",
            pfd.cColorBits,
            pfd.cAlphaBits,
            pfd.cDepthBits,
            pfd.cStencilBits,
            pfd.dwFlags & PFD_DOUBLEBUFFER != 0
        )
    }
}
