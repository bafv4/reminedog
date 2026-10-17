//! The `Agent_OnLoad` sequence: options, logging, the agent's globals and OS, JVMTI.

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Instant;

use reminedog_agent::globals::{self, Globals};
use reminedog_agent::{jvmti, os};
use reminedog_core::gamedir::{data_dir, detect_game_dir};
use reminedog_core::logfile::FileLogger;
use reminedog_core::options::AgentOptions;

/// After this many panics they are not logged any more (an input hook that panics would do
/// so on every event).
const MAX_PANICS_LOGGED: u32 = 50;

static PANICS: AtomicU32 = AtomicU32::new(0);

/// The version: the release's (the release workflow sets REMINEDOG_VERSION), otherwise the
/// crate's.
pub const VERSION: &str = env!("REMINEDOG_VERSION");

/// Identifies the build in logs; CI sets it to the commit hash ("local" otherwise).
pub const BUILD_ID: &str = env!("REMINEDOG_BUILD_ID");

pub fn on_load(raw_options: &str, vm: *mut std::ffi::c_void) {
    if globals::globals().is_some() {
        // The same library twice in the JVM's arguments: the JVM calls Agent_OnLoad again.
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
    let level = options.log_level.unwrap_or(log::LevelFilter::Info);
    if let Err(e) = FileLogger::install(&log_path, level) {
        eprintln!("reminedog: cannot open {}: {e}", log_path.display());
    }
    std::panic::set_hook(Box::new(|info| {
        let n = PANICS.fetch_add(1, Ordering::Relaxed);
        if n < MAX_PANICS_LOGGED {
            log::error!("panic: {info}");
        } else if n == MAX_PANICS_LOGGED {
            log::error!("panic: {info} (further panics are not logged)");
        }
    }));

    log::info!("reminedog {VERSION} ({BUILD_ID}) agent loaded (Linux)");
    log::info!(
        "java: {}",
        std::env::current_exe().map_or_else(|_| "?".into(), |p| p.display().to_string())
    );
    log::info!("cwd: {}", cwd.display());
    log::info!("game dir: {}", game_dir.display());
    log::info!("options: {raw_options:?} -> {options:?}");
    for warning in &warnings {
        log::warn!("option: {warning}");
    }

    let overlay = options.overlay;
    globals::init(Globals {
        options,
        game_dir,
        start,
        version: VERSION,
        build_id: BUILD_ID,
    });
    os::set(&crate::linux::LINUX);

    if overlay {
        crate::fonts::preload();
    }
    // SAFETY: the JavaVM the JVM passed Agent_OnLoad.
    if let Err(e) = unsafe { jvmti::start(vm) } {
        log::error!("jvmti: not started, the overlay will not work: {e}");
    }
}
