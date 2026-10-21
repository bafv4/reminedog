//! Windows だけが起動時とパニックのときにすること（[`crate::windows`]）。

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

/// パニックのときに書く戻り先のアドレスの数。
const MAX_FRAMES: usize = 32;

/// ログを書けないときに、`message` をデバッガーの出力（DebugView）に書く。
pub fn debug_output(message: &str) {
    // SAFETY: NUL で終わるワイド文字列。
    unsafe { OutputDebugStringW(ffi::wide(&format!("reminedog: {message}\n")).as_ptr()) };
}

/// 別の場所のエージェント（自分の static を持つ、別のフォルダの reminedog の DLL）がこの
/// プロセスで動いているか：最初のものが開いたままにする、プロセスにちなんで名付けたミューテックス。
pub fn another_copy_runs() -> bool {
    let name = ffi::wide(&format!("Local\\reminedog-agent-{}", std::process::id()));
    // SAFETY: NUL で終わる名前。最初のコピーのハンドルはプロセスの間開いたまま。
    let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
    // SAFETY: ただの呼び出し。エラーを読む呼び出しのすぐ後。
    let exists = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
    if exists && !handle.is_null() {
        // SAFETY: いま開いたハンドル。
        unsafe { CloseHandle(handle) };
    }
    exists
}

/// パニックが起きた場所：スタックの戻り先のアドレスを `module+0xoffset` として。リリースの
/// `.pdb` で解決できる。シンボルは引かない（利用者の PC にはなく、引くと dbghelp を読み込み、
/// ローダーロックの中かもしれない）。
pub fn return_addresses() -> String {
    let mut frames = [std::ptr::null_mut(); MAX_FRAMES];
    // SAFETY: バッファは MAX_FRAMES 個のポインタを持つ。
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
        // SAFETY: 読み込んだモジュールの中でアドレスを引く。参照は取らない。
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
