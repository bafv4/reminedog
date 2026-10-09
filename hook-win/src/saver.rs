//! Writes files on a thread of its own, so the game's thread never waits for the disk: the
//! settings, the waypoints and the servers' labels are each written to a temporary file,
//! synced and renamed over the old one ([`write_file`]), which can take tens of
//! milliseconds while a virus scanner or a sync client looks at the folder.
//!
//! Writes run in the order they were handed over. The caller polls each write's outcome on
//! later frames. A write handed over just before the game exits may not happen.

use std::io;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use reminedog_core::write_file;

use crate::ffi;

struct Write {
    path: PathBuf,
    bytes: Vec<u8>,
    done: Sender<io::Result<()>>,
}

/// The saving thread's queue; `None` when the thread could not be started.
static QUEUE: OnceLock<Option<Mutex<Sender<Write>>>> = OnceLock::new();

fn queue() -> Option<&'static Mutex<Sender<Write>>> {
    QUEUE
        .get_or_init(|| {
            let (tx, rx) = mpsc::channel::<Write>();
            let spawned = std::thread::Builder::new()
                .name("reminedog-saver".into())
                .spawn(move || {
                    for write in rx {
                        let result =
                            ffi::catch("saving a file", || write_file(&write.path, &write.bytes))
                                .unwrap_or_else(|| Err(io::Error::other("panicked while saving")));
                        let _ = write.done.send(result);
                    }
                });
            match spawned {
                Ok(_) => Some(Mutex::new(tx)),
                Err(e) => {
                    log::warn!("cannot start the saving thread ({e}); saving on the game's thread");
                    None
                }
            }
        })
        .as_ref()
}

/// The outcome of a write handed to [`write`].
pub struct Pending(Receiver<io::Result<()>>);

impl Pending {
    /// The outcome, once the write is done.
    pub fn poll(&self) -> Option<io::Result<()>> {
        match self.0.try_recv() {
            Ok(result) => Some(result),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                Some(Err(io::Error::other("the saving thread stopped")))
            }
        }
    }

    /// Waits up to `timeout` for the outcome (when the caller cannot come back later).
    pub fn wait(&self, timeout: Duration) -> Option<io::Result<()>> {
        match self.0.recv_timeout(timeout) {
            Ok(result) => Some(result),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => {
                Some(Err(io::Error::other("the saving thread stopped")))
            }
        }
    }
}

/// Writes `bytes` to `path` atomically on the saving thread (on this one if it could not
/// be started).
pub fn write(path: PathBuf, bytes: Vec<u8>) -> Pending {
    let (done, outcome) = mpsc::channel();
    let write = Write { path, bytes, done };
    let rejected = match queue() {
        Some(queue) => queue
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .send(write)
            .err()
            .map(|e| e.0),
        None => Some(write),
    };
    if let Some(write) = rejected {
        let _ = write.done.send(write_file(&write.path, &write.bytes));
    }
    Pending(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_in_order_and_reports_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a").join("file.json");
        let first = write(path.clone(), b"1".to_vec());
        let second = write(path.clone(), b"2".to_vec());
        assert!(second.wait(Duration::from_secs(10)).unwrap().is_ok());
        assert!(first.poll().unwrap().is_ok());
        assert_eq!(std::fs::read(&path).unwrap(), b"2");
        // A folder in the way of the file.
        let blocked = write(dir.path().to_owned(), b"x".to_vec());
        assert!(blocked.wait(Duration::from_secs(10)).unwrap().is_err());
    }
}
