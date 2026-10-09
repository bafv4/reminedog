//! Process-wide agent state and the `Agent_OnLoad` sequence.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Instant;

use reminedog_core::gamedir::{data_dir, detect_game_dir};
use reminedog_core::logfile::FileLogger;
use reminedog_core::options::AgentOptions;
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

/// Panics logged with where they happened; more are logged by their message only, and after
/// [`MAX_PANICS_LOGGED`] not at all (an input hook that panics would do so on every event).
const MAX_PANIC_TRACES: u32 = 3;
const MAX_PANICS_LOGGED: u32 = 50;
/// Return addresses written for a panic.
const MAX_FRAMES: usize = 32;

static PANICS: AtomicU32 = AtomicU32::new(0);

pub struct Globals {
    pub options: AgentOptions,
    pub game_dir: PathBuf,
    pub start: Instant,
}

static GLOBALS: OnceLock<Globals> = OnceLock::new();

/// `None` until `Agent_OnLoad` has run.
pub fn globals() -> Option<&'static Globals> {
    GLOBALS.get()
}

/// The version: the release's (the release workflow sets REMINEDOG_VERSION), otherwise the
/// crate's; as `build.rs` put it in the version resource.
pub const VERSION: &str = env!("REMINEDOG_VERSION");

/// Identifies the build in logs; CI sets it to the commit hash ("local" otherwise).
pub const BUILD_ID: &str = env!("REMINEDOG_BUILD_ID");

pub fn on_load(raw_options: &str) {
    if GLOBALS.get().is_some() {
        // The same DLL twice in the JVM's arguments: the JVM calls Agent_OnLoad again.
        log::warn!(
            "the agent was loaded again (given twice in the JVM arguments?); the options {raw_options:?} are ignored"
        );
        return;
    }
    let start = Instant::now();
    let (options, warnings) = AgentOptions::parse(raw_options);
    let cwd = std::env::current_dir().unwrap_or_default();
    // Only used to find --gameDir; never logged, since it carries the session token.
    let args: Vec<String> = std::env::args_os()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let game_dir = options
        .game_dir
        .clone()
        .unwrap_or_else(|| detect_game_dir(&args, &cwd));
    let log_path = data_dir(&game_dir).join("reminedog.log");
    if another_copy_runs() {
        // Another reminedog DLL (from another folder) is at work in this process; its log
        // would be rotated away by this one's, so a line is added to it instead.
        note_in_log(
            &log_path,
            "another reminedog DLL was loaded into this process too; it stays off (remove one of the -agentpath arguments)",
        );
        return;
    }
    let level = options.log_level.unwrap_or(log::LevelFilter::Info);
    if let Err(e) = FileLogger::install(&log_path, level) {
        let msg = format!("reminedog: cannot open {}: {e}\n", log_path.display());
        // SAFETY: NUL-terminated wide string.
        unsafe { OutputDebugStringW(ffi::wide(&msg).as_ptr()) };
    }
    std::panic::set_hook(Box::new(|info| {
        let n = PANICS.fetch_add(1, Ordering::Relaxed);
        if n < MAX_PANIC_TRACES {
            log::error!("panic: {info}\n{}", return_addresses());
        } else if n < MAX_PANICS_LOGGED {
            log::error!("panic: {info}");
        } else if n == MAX_PANICS_LOGGED {
            log::error!("panic: {info} (further panics are not logged)");
        }
    }));

    log::info!("reminedog {VERSION} ({BUILD_ID}) agent loaded");
    log::info!(
        "java: {}",
        ffi::module_path(None).map_or_else(|| "?".into(), |p| p.display().to_string())
    );
    log::info!("cwd: {}", cwd.display());
    log::info!("game dir: {}", game_dir.display());
    log::info!("options: {raw_options:?} -> {options:?}");
    for warning in &warnings {
        log::warn!("option: {warning}");
    }

    let _ = GLOBALS.set(Globals {
        options,
        game_dir,
        start,
    });

    if globals().is_some_and(|globals| globals.options.overlay) {
        crate::fonts::preload();
    }
    if let Err(e) = crate::loader::watch() {
        log::error!("cannot watch DLL loads, the overlay will not work: {e}");
    }
}

/// Whether another copy of the agent (a reminedog DLL from another folder, with statics of
/// its own) runs in this process: a mutex named after the process, kept open by the first.
fn another_copy_runs() -> bool {
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

/// Adds `line` to the log at `path` without taking it over.
fn note_in_log(path: &Path, line: &str) {
    let written = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)
        .and_then(|mut file| writeln!(file, "WARN  {line}"));
    if written.is_err() {
        // SAFETY: NUL-terminated wide string.
        unsafe { OutputDebugStringW(ffi::wide(&format!("reminedog: {line}\n")).as_ptr()) };
    }
}

/// Where a panic happened: the return addresses on the stack as `module+0xoffset`, which the
/// release's `.pdb` resolves. No symbols are looked up (they are not on the user's PC, and
/// that would load dbghelp, maybe under the loader lock).
fn return_addresses() -> String {
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
