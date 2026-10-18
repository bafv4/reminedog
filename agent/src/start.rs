//! The `Agent_OnLoad` sequence after the options were decoded: the game folder, logging, the
//! panic hook, the globals and the OS, JVMTI.

use std::io::Write as _;
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Instant;

use reminedog_core::gamedir::{data_dir, detect_game_dir};
use reminedog_core::logfile::FileLogger;
use reminedog_core::options::AgentOptions;

use crate::globals::{self, Globals};
use crate::os::{self, Os};
use crate::{BUILD_ID, VERSION};

/// Panics logged with where they happened ([`Os::panic_details`]); more are logged by their
/// message only, and after [`MAX_PANICS_LOGGED`] not at all (an input hook that panics would
/// do so on every event).
const MAX_PANIC_DETAILS: u32 = 3;
const MAX_PANICS_LOGGED: u32 = 50;

static PANICS: AtomicU32 = AtomicU32::new(0);

/// Starts the agent with `os`.
///
/// # Safety
/// `vm` is the `JavaVM*` the JVM passed `Agent_OnLoad`.
pub unsafe fn start(raw_options: &str, vm: *mut std::ffi::c_void, os: &'static dyn Os) {
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
    if os.another_copy_runs() {
        // Another copy of the agent (from another folder) is at work in this process; its log
        // would be rotated away by this one's, so a line is added to it instead.
        note_in_log(
            os,
            &log_path,
            "another reminedog library was loaded into this process too; it stays off (remove one of the -agentpath arguments)",
        );
        return;
    }
    let level = options.log_level.unwrap_or(log::LevelFilter::Info);
    if let Err(e) = FileLogger::install(&log_path, level) {
        os.report(&format!("cannot open {}: {e}", log_path.display()));
    }
    std::panic::set_hook(Box::new(move |info| {
        let n = PANICS.fetch_add(1, Ordering::Relaxed);
        if n < MAX_PANIC_DETAILS
            && let Some(details) = os.panic_details()
        {
            log::error!("panic: {info}\n{details}");
        } else if n < MAX_PANICS_LOGGED {
            log::error!("panic: {info}");
        } else if n == MAX_PANICS_LOGGED {
            log::error!("panic: {info} (further panics are not logged)");
        }
    }));

    log::info!("reminedog {VERSION} ({BUILD_ID}) agent loaded");
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
    });
    os::set(os);

    if overlay {
        crate::fonts::preload();
    }
    // SAFETY: guaranteed by the caller.
    if let Err(e) = unsafe { crate::jvmti::start(vm) } {
        log::error!("jvmti: not started, the overlay will not work: {e}");
    }
}

/// Adds `line` to the log at `path` without taking it over.
fn note_in_log(os: &dyn Os, path: &Path, line: &str) {
    let written = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)
        .and_then(|mut file| writeln!(file, "WARN  {line}"));
    if written.is_err() {
        os.report(line);
    }
}
