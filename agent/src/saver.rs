//! ファイルを専用のスレッドで書く。ゲームのスレッドがディスクを待たないように：設定、
//! ウェイポイント、サーバーのラベルは、それぞれ一時ファイルに書いて同期してから、古いものの上に
//! 名前を変えて置く（[`write_file`]）。ウイルス対策や同期のアプリがフォルダを見ていると、
//! これに数十ミリ秒かかることがある。
//!
//! 渡した順に書く。呼び出し側は、それぞれの書き込みの結果を後のフレームで確かめる。
//! ゲームが終わる直前に渡した書き込みは、行われないことがある。

use std::io;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use reminedog_core::write_file;

struct Write {
    path: PathBuf,
    bytes: Vec<u8>,
    done: Sender<io::Result<()>>,
}

/// 保存のスレッドのキュー。スレッドを始められなかったときは `None`。
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
                            crate::catch("saving a file", || write_file(&write.path, &write.bytes))
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

/// [`write`] に渡した書き込みの結果。
pub struct Pending(Receiver<io::Result<()>>);

impl Pending {
    /// 書き終わっていれば、その結果。
    pub fn poll(&self) -> Option<io::Result<()>> {
        match self.0.try_recv() {
            Ok(result) => Some(result),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                Some(Err(io::Error::other("the saving thread stopped")))
            }
        }
    }

    /// 結果を最大 `timeout` まで待つ（呼び出し側が後で確かめられないとき）。
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

/// `bytes` を `path` に、保存のスレッドでアトミックに書く（スレッドを始められなかったときは
/// このスレッドで）。
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
        // ファイルの場所にフォルダがある。
        let blocked = write(dir.path().to_owned(), b"x".to_vec());
        assert!(blocked.wait(Duration::from_secs(10)).unwrap().is_err());
    }
}
