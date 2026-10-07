//! Process-wide agent state and the `Agent_OnLoad` sequence.

use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Instant;

use reminedog_core::gamedir::{data_dir, detect_game_dir};
use reminedog_core::logfile::FileLogger;
use reminedog_core::options::AgentOptions;
use windows_sys::Win32::System::Diagnostics::Debug::OutputDebugStringW;

use crate::ffi;

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
    let level = options.log_level.unwrap_or(log::LevelFilter::Info);
    if let Err(e) = FileLogger::install(&log_path, level) {
        let msg = format!("reminedog: cannot open {}: {e}\n", log_path.display());
        // SAFETY: NUL-terminated wide string.
        unsafe { OutputDebugStringW(ffi::wide(&msg).as_ptr()) };
    }
    std::panic::set_hook(Box::new(|info| {
        log::error!(
            "panic: {info}\n{}",
            std::backtrace::Backtrace::force_capture()
        );
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

    if let Err(e) = crate::loader::watch() {
        log::error!("cannot watch DLL loads, the overlay will not work: {e}");
    }
}
