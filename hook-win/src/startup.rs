//! What only Windows does at startup and for panics ([`crate::windows`]).

use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HMODULE};
use windows_sys::Win32::System::Diagnostics::Debug::{
    OutputDebugStringW, RtlCaptureStackBackTrace,
};
use windows_sys::Win32::System::LibraryLoader::{
    GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
    GetModuleHandleExW,
};
use windows_sys::Win32::System::Threading::CreateMutexW;

use crate::ffi;

/// Return addresses written for a panic.
const MAX_FRAMES: usize = 32;

/// Writes `message` to the debugger's output (DebugView), where no log can be written.
pub fn debug_output(message: &str) {
    // SAFETY: NUL-terminated wide string.
    unsafe { OutputDebugStringW(ffi::wide(&format!("reminedog: {message}\n")).as_ptr()) };
}

/// Whether another copy of the agent (a reminedog DLL from another folder, with statics of
/// its own) runs in this process: a mutex named after the process, kept open by the first.
pub fn another_copy_runs() -> bool {
    let name = ffi::wide(&format!("Local\\reminedog-agent-{}", std::process::id()));
    // SAFETY: a NUL-terminated name. The first copy's handle stays open for the process.
    let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
    // SAFETY: plain call, right after the one whose error it reads.
    let exists = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
    if exists && !handle.is_null() {
        // SAFETY: the handle just opened.
        unsafe { CloseHandle(handle) };
    }
    exists
}

/// Where a panic happened: the return addresses on the stack as `module+0xoffset`, which the
/// release's `.pdb` resolves. No symbols are looked up (they are not on the user's PC, and
/// that would load dbghelp, maybe under the loader lock).
pub fn return_addresses() -> String {
    let mut frames = [std::ptr::null_mut(); MAX_FRAMES];
    // SAFETY: the buffer holds MAX_FRAMES pointers.
    let count = unsafe {
        RtlCaptureStackBackTrace(
            1,
            MAX_FRAMES as u32,
            frames.as_mut_ptr(),
            std::ptr::null_mut(),
        )
    };
    let mut text = String::from("return addresses:");
    for &address in &frames[..usize::from(count)] {
        let mut module: HMODULE = std::ptr::null_mut();
        // SAFETY: looks the address up among the loaded modules, taking no reference.
        let found = unsafe {
            GetModuleHandleExW(
                GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS
                    | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
                address.cast_const().cast(),
                &mut module,
            )
        } != 0;
        let name = found
            .then(|| ffi::module_path(Some(module)))
            .flatten()
            .and_then(|path| {
                path.file_name()
                    .map(|name| name.to_string_lossy().into_owned())
            });
        match name {
            Some(name) => text.push_str(&format!(
                "\n  {name}+0x{:x}",
                address as usize - module as usize
            )),
            None => text.push_str(&format!("\n  0x{:x}", address as usize)),
        }
    }
    text
}
