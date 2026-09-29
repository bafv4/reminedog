//! A small `log` backend that writes to a file, shared by the platform hooks.
//!
//! Each record is written and flushed as one line, since the host process may be killed at
//! any moment:
//! `[<secs since start>] [<LEVEL>] [<thread>] <target>: <message>`.

use std::fmt::Write as _;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use log::{LevelFilter, Log, Metadata, Record};

/// The process-wide file logger. Obtain it through [`FileLogger::install`].
pub struct FileLogger {
    sink: OnceLock<Sink>,
}

struct Sink {
    file: Mutex<File>,
    start: Instant,
    level: LevelFilter,
}

static LOGGER: FileLogger = FileLogger {
    sink: OnceLock::new(),
};

/// Serializes installs; `true` once `LOGGER` holds the global logger slot.
static CLAIMED: Mutex<bool> = Mutex::new(false);

impl FileLogger {
    /// Starts logging to `path` at `level`.
    ///
    /// Creates the parent directory and keeps the previous log as `<stem>.prev.log`. If a
    /// logger is already installed (this one or another), returns `Ok` without touching any
    /// file. Logging never panics; I/O errors after installation are ignored.
    pub fn install(path: &Path, level: LevelFilter) -> io::Result<()> {
        let mut claimed = CLAIMED.lock().unwrap_or_else(PoisonError::into_inner);
        if LOGGER.sink.get().is_some() {
            return Ok(());
        }
        // Take the slot before touching files, so losing to another logger changes nothing.
        if !*claimed {
            if log::set_logger(&LOGGER).is_err() {
                return Ok(());
            }
            *claimed = true;
        }
        let file = open_fresh(path)?;
        let sink = Sink {
            file: Mutex::new(file),
            start: Instant::now(),
            level,
        };
        if level > LevelFilter::Off {
            let unix = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_secs());
            sink.write(&format!(
                "[0.000] [INFO] [{}] {}: log started at unix time {unix}\n",
                thread_label(),
                module_path!()
            ));
        }
        // Cannot fail: `sink` is only set here, under the `CLAIMED` lock.
        let _ = LOGGER.sink.set(sink);
        log::set_max_level(level);
        Ok(())
    }
}

impl Log for FileLogger {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        self.sink.get().is_some_and(|s| metadata.level() <= s.level)
    }

    fn log(&self, record: &Record<'_>) {
        let Some(sink) = self.sink.get() else {
            return;
        };
        if record.level() > sink.level {
            return;
        }
        // A panicking `Display` impl in the arguments must not take the caller down.
        let _ = panic::catch_unwind(AssertUnwindSafe(|| {
            // Format before locking so that logging from inside a `Display` impl cannot deadlock.
            let line = format_line(sink.start.elapsed(), &thread_label(), record);
            sink.write(&line);
        }));
    }

    fn flush(&self) {
        if let Some(sink) = self.sink.get() {
            let _ = sink
                .file
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .flush();
        }
    }
}

impl Sink {
    fn write(&self, line: &str) {
        let mut file = self.file.lock().unwrap_or_else(PoisonError::into_inner);
        // A single write keeps concurrent lines whole; File has no user-space buffer.
        let _ = file.write_all(line.as_bytes());
        let _ = file.flush();
    }
}

/// Creates the parent directory, moves an existing log to `<stem>.prev.log` and opens `path`
/// empty.
fn open_fresh(path: &Path) -> io::Result<File> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        fs::create_dir_all(dir)?;
    }
    // Best effort: if the old log cannot be moved, it is truncated instead.
    let _ = fs::rename(path, prev_path(path));
    OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
}

fn prev_path(path: &Path) -> PathBuf {
    let mut name = path.file_stem().unwrap_or_default().to_owned();
    name.push(".prev.log");
    path.with_file_name(name)
}

fn thread_label() -> String {
    let thread = std::thread::current();
    match thread.name() {
        Some(name) => name.to_owned(),
        None => format!("{:?}", thread.id()),
    }
}

fn format_line(elapsed: Duration, thread: &str, record: &Record<'_>) -> String {
    let mut line = String::with_capacity(128);
    let _ = writeln!(
        line,
        "[{:.3}] [{}] [{thread}] {}: {}",
        elapsed.as_secs_f64(),
        record.level(),
        record.target(),
        record.args()
    );
    line
}

#[cfg(test)]
mod tests {
    use super::*;
    use log::Level;

    #[test]
    fn line_format() {
        let line = format_line(
            Duration::from_millis(12_345),
            "Render thread",
            &Record::builder()
                .level(Level::Warn)
                .target("reminedog::hooks")
                .args(format_args!("hooked {} functions", 7))
                .build(),
        );
        assert_eq!(
            line,
            "[12.345] [WARN] [Render thread] reminedog::hooks: hooked 7 functions\n"
        );
        let line = format_line(
            Duration::ZERO,
            "ThreadId(3)",
            &Record::builder()
                .level(Level::Info)
                .target("t")
                .args(format_args!("x"))
                .build(),
        );
        assert_eq!(line, "[0.000] [INFO] [ThreadId(3)] t: x\n");
    }

    #[test]
    fn prev_path_names() {
        assert_eq!(
            prev_path(Path::new("dir/reminedog.log")),
            Path::new("dir/reminedog.prev.log")
        );
        assert_eq!(
            prev_path(Path::new("dir/log")),
            Path::new("dir/log.prev.log")
        );
    }

    #[test]
    fn open_fresh_rotates_previous_log() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("reminedog.log");
        let mut file = open_fresh(&path).unwrap();
        file.write_all(b"first run\n").unwrap();
        drop(file);
        fs::write(
            dir.path().join("nested").join("reminedog.prev.log"),
            "older",
        )
        .unwrap();

        let mut file = open_fresh(&path).unwrap();
        file.write_all(b"second run\n").unwrap();
        drop(file);
        assert_eq!(fs::read_to_string(&path).unwrap(), "second run\n");
        assert_eq!(
            fs::read_to_string(dir.path().join("nested").join("reminedog.prev.log")).unwrap(),
            "first run\n"
        );
    }

    #[test]
    fn thread_labels() {
        let named = std::thread::Builder::new()
            .name("worker".into())
            .spawn(thread_label)
            .unwrap()
            .join()
            .unwrap();
        assert_eq!(named, "worker");
        let unnamed = std::thread::Builder::new()
            .spawn(thread_label)
            .unwrap()
            .join()
            .unwrap();
        assert!(unnamed.starts_with("ThreadId("), "{unnamed}");
    }

    struct Panics;

    impl std::fmt::Display for Panics {
        fn fmt(&self, _: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            panic!("display failed")
        }
    }

    /// The only test that installs the global logger (it can be set once per process).
    #[test]
    fn install_writes_lines_and_second_install_is_a_no_op() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("logs").join("reminedog.log");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "previous session\n").unwrap();

        FileLogger::install(&path, LevelFilter::Debug).unwrap();
        assert_eq!(log::max_level(), LevelFilter::Debug);
        log::info!(target: "reminedog_test", "hello {}", "world");
        log::trace!(target: "reminedog_test", "filtered out");
        log::debug!(target: "reminedog_test", "{}", Panics);
        log::warn!(target: "reminedog_test", "still alive");
        log::logger().flush();

        let text = fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text
            .lines()
            .filter(|l| l.contains("reminedog_test") || l.contains("log started"))
            .collect();
        assert_eq!(lines.len(), 3, "{text}");
        assert!(lines[0].starts_with("[0.000] [INFO] ["), "{}", lines[0]);
        assert!(
            lines[0].contains("log started at unix time"),
            "{}",
            lines[0]
        );
        assert!(
            lines[1].starts_with('[') && lines[1].ends_with("] reminedog_test: hello world"),
            "{}",
            lines[1]
        );
        assert!(lines[1].contains("] [INFO] ["), "{}", lines[1]);
        assert!(lines[2].contains("] [WARN] [") && lines[2].ends_with("still alive"));
        assert_eq!(
            fs::read_to_string(dir.path().join("logs").join("reminedog.prev.log")).unwrap(),
            "previous session\n"
        );

        // A second install neither fails nor touches the new path.
        let other = dir.path().join("other").join("second.log");
        FileLogger::install(&other, LevelFilter::Trace).unwrap();
        assert!(!other.exists());
        assert!(!other.parent().unwrap().exists());
        assert_eq!(log::max_level(), LevelFilter::Debug);
    }
}
