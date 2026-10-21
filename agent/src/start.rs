//! オプションを読んだ後の `Agent_OnLoad` の流れ：ゲームフォルダ、ログ、パニックフック、
//! 全体の状態と OS、JVMTI。

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

/// 起きた場所（[`Os::panic_details`]）と一緒にログに書くパニックの数。それより後はメッセージ
/// だけを書き、[`MAX_PANICS_LOGGED`] を超えたら書かない（パニックする入力のフックは、
/// イベントのたびにパニックする）。
const MAX_PANIC_DETAILS: u32 = 3;
const MAX_PANICS_LOGGED: u32 = 50;

static PANICS: AtomicU32 = AtomicU32::new(0);

/// `os` でエージェントを始める。
///
/// # Safety
/// `vm` は、JVM が `Agent_OnLoad` に渡した `JavaVM*`。
pub unsafe fn start(raw_options: &str, vm: *mut std::ffi::c_void, os: &'static dyn Os) {
    if globals::globals().is_some() {
        // 同じライブラリが JVM の引数に 2 回ある：JVM が Agent_OnLoad をもう一度呼ぶ。
        log::warn!(
            "the agent was loaded again (given twice in the JVM arguments?); the options {raw_options:?} are ignored"
        );
        return;
    }
    let start = Instant::now();
    let (options, warnings) = AgentOptions::parse(raw_options);
    let cwd = std::env::current_dir().unwrap_or_default();
    // --gameDir を探すためだけに使う。セッションのトークンが入っているので、ログには出さない。
    let args: Vec<String> = std::env::args_os()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let game_dir = options
        .game_dir
        .clone()
        .unwrap_or_else(|| detect_game_dir(&args, &cwd));
    let log_path = data_dir(&game_dir).join("reminedog.log");
    if os.another_copy_runs() {
        // 別の場所のエージェントがこのプロセスで動いている。こちらがログを回すとあちらのログが
        // 消えるので、代わりにあちらのログに 1 行を足す。
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
    // SAFETY: 呼び出し側が保証する。
    if let Err(e) = unsafe { crate::jvmti::start(vm) } {
        log::error!("jvmti: not started, the overlay will not work: {e}");
    }
}

/// `path` のログに `line` を足す（ログを奪わない）。
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
