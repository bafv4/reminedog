//! プレイヤーがいるワールドと、そのワールドのウェイポイント。
//!
//! [`WorldWatcher`] は `latest.log` と saves のフォルダを追う。[`WaypointBook`] は今のワールドの
//! [`WaypointStore`] を持ち、変更のたびに保存する（すぐに、または（deferred なら）別のスレッドで）。
//! [`ServerLabels`] は各サーバーで最後に使ったラベルを覚える。利用者に向けたエラーは日本語の
//! UI の文言で、詳しいことは英語でログに出す。

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use crate::gamedir::{data_dir, latest_log_path, saves_dir, waypoints_path};
use crate::location::Location;
use crate::waypoint::{StoreError, Waypoint, WaypointStore};
use crate::world::{LogEvent, LogTail, WorldId, WorldTracker, level_name, might_be_world_line};

/// 内蔵サーバーが始まるときに開くワールドは、始まりの行を読む前のせいぜいこの時間内にロック
/// された。それより古いロックなら、その行はワールドを開いたことから来たのではない（サーバーは、
/// ゲームがそのままログに出す文字を通して、ログに行を入れられる）。
const LOCK_WINDOW: Duration = Duration::from_secs(300);

/// ログと saves のフォルダから見た、プレイヤーのいる場所。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorldState {
    /// どのワールドより前、または内蔵サーバーが止まった後。
    NotInWorld,
    /// 内蔵サーバーは始まったが、そのワールドのフォルダが見つからなかった。
    Unknown,
    In(WorldId),
}

/// latest.log と saves のフォルダを追い、プレイヤーがどのワールドかサーバーにいるかを知る。
#[derive(Debug)]
pub struct WorldWatcher {
    tail: LogTail,
    tracker: WorldTracker,
    saves: PathBuf,
    /// 最後のイベントでワールドに入った（追跡の `resolve` は、ワールドを出た後も、シングルプレイの
    /// ワールドが見つからないときも `None` を返す）。
    entered: bool,
    /// 最後に内蔵サーバーの始まりを読んだ時刻。
    started_at: Option<SystemTime>,
    state: WorldState,
    /// 入ったときに読んだ、ゲームの中でのシングルプレイのワールドの名前。
    level_name: Option<String>,
}

impl WorldWatcher {
    pub fn new(game_dir: &Path) -> Self {
        Self {
            tail: LogTail::filtered(latest_log_path(game_dir), might_be_world_line),
            tracker: WorldTracker::new(),
            saves: saves_dir(game_dir),
            entered: false,
            started_at: None,
            state: WorldState::NotInWorld,
            level_name: None,
        }
    }

    /// 新しい行を読む。追跡のやり直し（latest.log が回された。例：深夜 0 時）では、追跡を戻さない：
    /// 起動時の回しはエージェントの最初のフレームより前に起きるので、それより後のやり直しは
    /// セッションの途中で、新しいファイルにはまだワールドの行がない。新しいワールドのイベントの後に
    /// 1 回解決し直す。`state()` が変わったら true。ログがなければ `Ok(false)`。
    pub fn poll(&mut self) -> io::Result<bool> {
        self.poll_lines(true)
    }

    /// 前に読んでからずっとプレイヤーがゲームの中（画面なし）にいたときの [`poll`](Self::poll)：
    /// ゲームは画面を通してしかワールドを変えないので、いま読むワールドの行はサーバーがログに入れた
    /// もの（ゲームがそのままログに出す文字で）で、無視する。
    pub fn poll_while_playing(&mut self) -> io::Result<bool> {
        self.poll_lines(false)
    }

    fn poll_lines(&mut self, switches: bool) -> io::Result<bool> {
        let lines = match self.tail.poll() {
            Ok(lines) => lines,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(e),
        };
        let mut relevant = false;
        for line in &lines {
            let Some(ev) = crate::world::parse_log_line(line) else {
                continue;
            };
            if !switches {
                log::warn!("world: ignored a world change logged during play ({ev:?})");
                continue;
            }
            if ev == LogEvent::IntegratedServerStarting {
                self.started_at = Some(SystemTime::now());
            }
            self.entered = ev != LogEvent::IntegratedServerStopping;
            self.tracker.on_log_event(&ev);
            relevant = true;
        }
        Ok(relevant && self.refresh())
    }

    /// いま解決し直す（シングルプレイ：サーバーが始まる少し前にロックされた、いちばん新しい
    /// session.lock）。`state()` が変わったら true。
    pub fn refresh(&mut self) -> bool {
        let not_before = self
            .started_at
            .and_then(|started| started.checked_sub(LOCK_WINDOW));
        let state = match self.tracker.resolve_since(&self.saves, not_before) {
            Some(world) => WorldState::In(world),
            None if self.entered => WorldState::Unknown,
            None => WorldState::NotInWorld,
        };
        if state == self.state {
            return false;
        }
        self.level_name = match &state {
            WorldState::In(WorldId::Singleplayer { folder }) => {
                level_name(&self.saves.join(folder))
            }
            _ => None,
        };
        self.state = state;
        true
    }

    pub fn state(&self) -> &WorldState {
        &self.state
    }

    /// ゲームの中でのシングルプレイのワールドの名前（その `level.dat`）。読めれば。
    pub fn level_name(&self) -> Option<&str> {
        self.level_name.as_deref()
    }

    pub fn current(&self) -> Option<&WorldId> {
        match &self.state {
            WorldState::In(world) => Some(world),
            WorldState::NotInWorld | WorldState::Unknown => None,
        }
    }
}

const NOT_IN_WORLD: &str = "ワールドに入っていない";
const NOT_FOUND: &str = "その地点は見つからない";

/// ワールドのウェイポイントの保存。別のスレッドで [`write_file`](crate::waypoint::write_file) で
/// 書き、[`WaypointBook::save_done`] で結果を返す。
#[derive(Debug)]
pub struct SaveJob {
    pub path: PathBuf,
    pub bytes: Vec<u8>,
    /// これが来た記録と、その変更。
    opened: u64,
    revision: u64,
    world: String,
}

/// 今のワールドのウェイポイント。正しいのは記憶の方：変更のたびに、すぐに、または（deferred
/// なら）別のスレッドで [`take_saves`](Self::take_saves) で保存する。保存に失敗しても変更は
/// 記憶に残り、`problem` がそう言い、次の変更（か `retry_save`）がもう一度保存する。
#[derive(Debug)]
pub struct WaypointBook {
    game_dir: PathBuf,
    world: Option<WorldId>,
    store: Option<WaypointStore>,
    /// 記録を開けなかった理由、または壊れたファイルを脇に移したこと。
    open_problem: Option<String>,
    /// 前の保存が失敗した。次に成功すると消える。
    save_problem: Option<String>,
    /// まだディスクにない変更。
    dirty: bool,
    /// 開くのに I/O のエラーで失敗した（直るかもしれない）。`add` がもう一度試す。
    reopen: bool,
    /// 保存は書かずに `take_saves` が渡す。
    deferred: bool,
    /// 渡す保存（ファイルごとにいちばん新しいもの）。
    queued: Vec<SaveJob>,
    /// 渡したが、まだ結果が返っていない保存。
    in_flight: usize,
    /// 開いた記録を数え、遅れた報告を今開いている記録のものと見分ける。
    opened: u64,
    /// [`waypoints`](Self::waypoints) が返すものの変更を数える（ワールドの切り替えも）。
    revision: u64,
}

impl WaypointBook {
    pub fn new(game_dir: &Path) -> Self {
        Self {
            game_dir: game_dir.to_owned(),
            world: None,
            store: None,
            open_problem: None,
            save_problem: None,
            dirty: false,
            reopen: false,
            deferred: false,
            queued: Vec::new(),
            in_flight: 0,
            opened: 0,
            revision: 0,
        }
    }

    /// 保存を別の場所で書く記録：変更は [`take_saves`](Self::take_saves) のために [`SaveJob`] を
    /// 並べ、結果は [`save_done`](Self::save_done) で返ってくる。後で保存に失敗しても、変更は成功する。
    pub fn deferred(game_dir: &Path) -> Self {
        Self {
            deferred: true,
            ..Self::new(game_dir)
        }
    }

    /// 書く保存（古い順）。
    pub fn take_saves(&mut self) -> Vec<SaveJob> {
        self.in_flight += self.queued.len();
        std::mem::take(&mut self.queued)
    }

    /// `job` を書いた結果。UI のための文言を返す：その後に出たワールドの変更が失われた、または新しい
    /// 失敗（続く間は `problem` がそう言う）。
    pub fn save_done(&mut self, job: &SaveJob, result: Result<(), String>) -> Option<String> {
        self.in_flight = self.in_flight.saturating_sub(1);
        if job.opened != self.opened {
            let Err(e) = result else {
                return None;
            };
            log::warn!(
                "waypoints: unsaved changes to {} are lost: {e}",
                job.path.display()
            );
            return Some(format!(
                "「{}」のウェイポイントの変更を保存できずに失った（ログを見る）",
                job.world
            ));
        }
        match result {
            Ok(()) => {
                if job.revision == self.revision {
                    self.dirty = false;
                }
                self.save_problem = None;
                None
            }
            Err(e) => self.save_failed(&e),
        }
    }

    /// ウェイポイントの変更とワールドの切り替えを数える。[`waypoints`](Self::waypoints) の見え方は、
    /// これが変わったときだけ作り直す。
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// `world` に切り替える（None = なし）：先に変更のある記録の保存を試し、それから
    /// `<game>/reminedog/waypoints/<stem>.json` を `open_or_backup` で開く。エラーなら記録は None で、
    /// `problem` が理由を言う。壊れていて控えを取ったファイルも報告する。もう開いているワールドは
    /// そのまま残す。古いワールドの変更を保存できずに失ったら、UI のための文言を返す。
    pub fn set_world(&mut self, world: Option<&WorldId>) -> Option<String> {
        if self.store.is_some() && self.world.as_ref() == world {
            return None;
        }
        let mut lost = None;
        if self.dirty
            && !self.deferred
            && self.save().is_err()
            && let Some(store) = &self.store
        {
            log::warn!(
                "waypoints: unsaved changes to {} are lost",
                store.path().display()
            );
            lost = Some(format!(
                "「{}」のウェイポイントの変更を保存できずに失った（ログを見る）",
                store.world()
            ));
        }
        // 最後の変更の保存はもう並んでいる。失敗していれば、もう 1 回。
        if self.dirty && self.deferred && !self.queued.iter().any(|job| job.opened == self.opened) {
            let _ = self.save();
        }
        self.world = world.cloned();
        self.store = None;
        self.open_problem = None;
        self.save_problem = None;
        self.dirty = false;
        self.reopen = false;
        self.opened += 1;
        self.revision += 1;
        self.open();
        lost
    }

    fn open(&mut self) {
        let Some(world) = &self.world else {
            return;
        };
        let path = waypoints_path(&self.game_dir, world);
        match WaypointStore::open_or_backup(&path, &world.display_name()) {
            Ok((store, backup)) => {
                log::debug!(
                    "waypoints: opened {} ({} waypoints)",
                    path.display(),
                    store.waypoints().len()
                );
                self.open_problem = backup.map(|backup| {
                    let name = backup
                        .file_name()
                        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
                    format!(
                        "ウェイポイントのファイルが壊れていたので reminedog/waypoints/{name} に移した"
                    )
                });
                self.store = Some(store);
                self.reopen = false;
            }
            Err(e) => {
                log::warn!("waypoints: cannot open: {e}");
                self.reopen = matches!(e, StoreError::Io { .. });
                self.open_problem = Some(match e {
                    StoreError::Io { source, .. } => {
                        format!("ウェイポイントのファイルを開けなかった：{source}")
                    }
                    StoreError::UnsupportedVersion { version, .. } => {
                        format!(
                            "ウェイポイントのファイルが新しい形式（版 {version}）で読み込めない（reminedog を新しい版に更新する）"
                        )
                    }
                    StoreError::Corrupt { .. } => "ウェイポイントのファイルが壊れている".into(),
                });
            }
        }
    }

    pub fn world(&self) -> Option<&WorldId> {
        self.world.as_ref()
    }

    /// 今のワールドのウェイポイント（足した順）。記録がなければ空。
    pub fn waypoints(&self) -> &[Waypoint] {
        self.store.as_ref().map_or(&[], |store| store.waypoints())
    }

    /// メニューに出す問題：失敗した保存、または記録のファイル。
    pub fn problem(&self) -> Option<&str> {
        self.save_problem
            .as_deref()
            .or(self.open_problem.as_deref())
    }

    /// `location` に "地点 {id}" という名前のウェイポイントを足す。I/O のエラーで保存先を開けて
    /// いなかったら、先に開き直す。保存の失敗はエラーだが、ウェイポイントは残る。
    pub fn add(&mut self, location: &Location, now_unix: i64) -> Result<Waypoint, String> {
        if self.store.is_none() && self.reopen {
            self.open();
        }
        let store = self.store_mut()?;
        // 記録は名前のないウェイポイントに英語の名前を付ける。代わりに日本語の名前を与える。
        let mut waypoint = store.add("", location, now_unix).clone();
        waypoint.name = format!("地点 {}", waypoint.id);
        store.rename(waypoint.id, &waypoint.name);
        self.changed();
        self.save()?;
        Ok(waypoint)
    }

    /// ウェイポイントの名前を変える。（前後の空白を取って）空なら古い名前のまま。知らない id なら Err。
    pub fn rename(&mut self, id: u64, name: &str) -> Result<(), String> {
        let store = self.store_mut()?;
        if store.get(id).is_none() {
            return Err(NOT_FOUND.into());
        }
        // 記録は空の名前を "Waypoint {id}" にしてしまう。
        if name.chars().all(|c| c.is_whitespace() || c.is_control()) {
            return Ok(());
        }
        store.rename(id, name);
        self.changed();
        self.save()
    }

    pub fn remove(&mut self, id: u64) -> Result<(), String> {
        let store = self.store_mut()?;
        if store.remove(id).is_none() {
            return Err(NOT_FOUND.into());
        }
        self.changed();
        self.save()
    }

    fn changed(&mut self) {
        self.dirty = true;
        self.revision += 1;
    }

    /// 前の保存が書けなかった変更が待っている。
    pub fn has_unsaved(&self) -> bool {
        self.dirty
    }

    /// 前の保存が書けなかった変更を保存する。なければ、または（deferred なら）保存が途中なら何もしない。
    pub fn retry_save(&mut self) -> Result<(), String> {
        if !self.dirty || self.in_flight > 0 || !self.queued.is_empty() {
            return Ok(());
        }
        self.save()
    }

    fn store_mut(&mut self) -> Result<&mut WaypointStore, String> {
        match &mut self.store {
            Some(store) => Ok(store),
            None if self.world.is_none() => Err(NOT_IN_WORLD.into()),
            None => Err(self
                .open_problem
                .clone()
                .unwrap_or_else(|| "ウェイポイントのファイルを開けていない".into())),
        }
    }

    fn save(&mut self) -> Result<(), String> {
        let Some(store) = &self.store else {
            return Ok(());
        };
        if self.deferred {
            let bytes = match store.to_json() {
                Ok(bytes) => bytes,
                Err(e) => {
                    let _ = self.save_failed(&store_error_text(&e));
                    return Ok(());
                }
            };
            let job = SaveJob {
                path: store.path().to_owned(),
                bytes,
                opened: self.opened,
                revision: self.revision,
                world: store.world().to_owned(),
            };
            self.queued.retain(|queued| queued.path != job.path);
            self.queued.push(job);
            return Ok(());
        }
        match store.save() {
            Ok(()) => {
                self.dirty = false;
                self.save_problem = None;
                Ok(())
            }
            Err(e) => {
                let text = store_error_text(&e);
                self.save_failed(&text);
                Err(format!("保存できなかった：{text}"))
            }
        }
    }

    /// 保存の失敗を書き留める（`detail`：理由）。新しい失敗なら文言を返す。
    fn save_failed(&mut self, detail: &str) -> Option<String> {
        let problem = format!("保存できなかった：{detail}（あとで保存し直す）");
        // 続くエラーは、試し直すたびではなく 1 回だけログに出して伝える。
        if self.save_problem.as_ref() == Some(&problem) {
            log::debug!("waypoints: still cannot save: {detail}");
            return None;
        }
        log::warn!("waypoints: cannot save: {detail}");
        self.save_problem = Some(problem.clone());
        Some(problem)
    }
}

/// UI のための、ウェイポイントのファイルの問題。
fn store_error_text(e: &StoreError) -> String {
    match e {
        StoreError::Io { source, .. } => source.to_string(),
        other => other.to_string(),
    }
}

/// 各サーバーのアドレスで最後に使ったラベル。次に来たときに同じウェイポイントを開くため：
/// `<game>/reminedog/waypoints/labels.json`、`{"version": 1, "labels": {"<host>:<port>":
/// "<label>"}}`。最初に要るときに 1 回読む。読めないファイルならラベルなし。
#[derive(Debug)]
pub struct ServerLabels {
    path: PathBuf,
    labels: Option<BTreeMap<String, String>>,
}

#[derive(Serialize, Deserialize)]
struct LabelsFile {
    version: u64,
    #[serde(default)]
    labels: BTreeMap<String, String>,
}

impl ServerLabels {
    pub fn new(game_dir: &Path) -> Self {
        Self {
            path: data_dir(game_dir).join("waypoints").join("labels.json"),
            labels: None,
        }
    }

    fn labels(&mut self) -> &mut BTreeMap<String, String> {
        let path = &self.path;
        self.labels.get_or_insert_with(|| {
            let bytes = match std::fs::read(path) {
                Ok(bytes) => bytes,
                Err(e) if e.kind() == io::ErrorKind::NotFound => return BTreeMap::new(),
                Err(e) => {
                    log::warn!("waypoints: cannot read {}: {e}", path.display());
                    return BTreeMap::new();
                }
            };
            let json = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes);
            match serde_json::from_slice::<LabelsFile>(json) {
                Ok(file) => file.labels,
                Err(e) => {
                    log::warn!("waypoints: ignoring {}: {e}", path.display());
                    BTreeMap::new()
                }
            }
        })
    }

    /// そのアドレスで最後に使ったラベルの下の `world`（シングルプレイのワールドはそのまま）。
    pub fn apply(&mut self, world: &WorldId) -> WorldId {
        let Some(address) = world.address_key() else {
            return world.clone();
        };
        let label = self.labels().get(&address).cloned();
        world.with_label(label.as_deref())
    }

    /// `world` のラベルをそのアドレスについて覚える。書くファイル（[`write_file`]
    /// (crate::waypoint::write_file)）、シングルプレイのワールドなら `None`。
    pub fn set(&mut self, world: &WorldId) -> Option<(PathBuf, Vec<u8>)> {
        let address = world.address_key()?;
        let label = match world {
            WorldId::Multiplayer { label, .. } => label.clone(),
            WorldId::Singleplayer { .. } => None,
        };
        let labels = self.labels();
        match label {
            Some(label) => labels.insert(address, label),
            None => labels.remove(&address),
        };
        let file = LabelsFile {
            version: 1,
            labels: labels.clone(),
        };
        let mut json = serde_json::to_vec_pretty(&file).ok()?;
        json.push(b'\n');
        Some((self.path.clone(), json))
    }
}

/// Unix の紀元からの秒（時計がそれより前なら 0）。`created_unix` のため。
pub fn unix_now() -> i64 {
    i64::try_from(crate::waypoint::unix_now()).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::location::{OVERWORLD, THE_NETHER};
    use std::fs::{self, File, OpenOptions};
    use std::io::Write;
    use std::time::Duration;

    fn sp(folder: &str) -> WorldId {
        WorldId::Singleplayer {
            folder: folder.into(),
        }
    }

    fn mp(host: &str, port: u16) -> WorldId {
        WorldId::Multiplayer {
            host: host.into(),
            port,
            label: None,
        }
    }

    /// `secs_ago` 秒前に `world` をロックする。
    fn set_lock_time(game: &Path, world: &str, secs_ago: u64) {
        let dir = saves_dir(game).join(world);
        fs::create_dir_all(&dir).unwrap();
        let lock = File::create(dir.join("session.lock")).unwrap();
        lock.set_modified(SystemTime::now() - Duration::from_secs(secs_ago))
            .unwrap();
    }

    fn append_log(game: &Path, lines: &[&str]) {
        let path = latest_log_path(game);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        for line in lines {
            write!(file, "{line}\r\n").unwrap();
        }
    }

    const STARTING: &str =
        "[12:00:00] [Server thread/INFO]: Starting integrated minecraft server version 1.21.11";
    const STOPPING: &str = "[12:05:00] [Server thread/INFO]: Stopping server";
    const CONNECTING: &str = "[12:10:00] [Render thread/INFO]: Connecting to Example.com, 25565";

    #[test]
    fn watcher_without_a_log() {
        let dir = tempfile::tempdir().unwrap();
        let mut watcher = WorldWatcher::new(dir.path());
        assert_eq!(watcher.state(), &WorldState::NotInWorld);
        assert!(!watcher.poll().unwrap());
        assert!(!watcher.refresh());
        assert_eq!(watcher.state(), &WorldState::NotInWorld);
        assert_eq!(watcher.current(), None);
    }

    #[test]
    fn watcher_follows_singleplayer_and_servers() {
        let dir = tempfile::tempdir().unwrap();
        let game = dir.path();
        set_lock_time(game, "Old", 200);
        set_lock_time(game, "Survival", 100);
        let mut watcher = WorldWatcher::new(game);

        append_log(
            game,
            &["[11:59:59] [Render thread/INFO]: Setting user: Steve"],
        );
        assert!(!watcher.poll().unwrap());
        assert_eq!(watcher.state(), &WorldState::NotInWorld);

        append_log(game, &[STARTING]);
        assert!(watcher.poll().unwrap());
        assert_eq!(watcher.state(), &WorldState::In(sp("Survival")));
        assert_eq!(watcher.current(), Some(&sp("Survival")));
        // 新しいことはなく、何も変わらない。
        assert!(!watcher.poll().unwrap());
        assert!(!watcher.refresh());

        append_log(game, &[STOPPING]);
        assert!(watcher.poll().unwrap());
        assert_eq!(watcher.state(), &WorldState::NotInWorld);
        assert_eq!(watcher.current(), None);

        append_log(game, &[CONNECTING]);
        assert!(watcher.poll().unwrap());
        assert_eq!(watcher.current(), Some(&mp("example.com", 25565)));

        // 1 回の読み込みの間に、シングルプレイに戻って別のワールドへ。
        set_lock_time(game, "Creative", 10);
        append_log(game, &[STOPPING, STARTING]);
        assert!(watcher.poll().unwrap());
        assert_eq!(watcher.current(), Some(&sp("Creative")));
        // 2 回の読み込みの間に入って出ても、何も変わらない。
        append_log(game, &[STOPPING, STARTING]);
        assert!(!watcher.poll().unwrap());
        assert_eq!(watcher.current(), Some(&sp("Creative")));
    }

    #[test]
    fn watcher_unknown_until_the_world_folder_is_found() {
        let dir = tempfile::tempdir().unwrap();
        let game = dir.path();
        let mut watcher = WorldWatcher::new(game);
        append_log(game, &[STARTING]);
        assert!(watcher.poll().unwrap());
        assert_eq!(watcher.state(), &WorldState::Unknown);
        assert_eq!(watcher.current(), None);
        // ログに新しいことがない：読み込みは saves のフォルダをもう一度は見ない。
        set_lock_time(game, "New World", 50);
        assert!(!watcher.poll().unwrap());
        assert_eq!(watcher.state(), &WorldState::Unknown);
        assert!(watcher.refresh());
        assert_eq!(watcher.state(), &WorldState::In(sp("New World")));
        assert!(!watcher.refresh());
        // 止まったら「不明」ではなく「ワールドにいない」に戻る。
        append_log(game, &[STOPPING]);
        assert!(watcher.poll().unwrap());
        assert_eq!(watcher.state(), &WorldState::NotInWorld);
    }

    #[test]
    fn watcher_needs_a_world_opened_just_now() {
        let dir = tempfile::tempdir().unwrap();
        let game = dir.path();
        // 1 時間前に遊んだ。今の始まりの行は、そのワールドを意味しない。
        set_lock_time(game, "Survival", 3600);
        let mut watcher = WorldWatcher::new(game);
        append_log(game, &[STARTING]);
        assert!(watcher.poll().unwrap());
        assert_eq!(watcher.state(), &WorldState::Unknown);
        set_lock_time(game, "Survival", 1);
        assert!(watcher.refresh());
        assert_eq!(watcher.current(), Some(&sp("Survival")));
    }

    #[test]
    fn watcher_ignores_world_lines_logged_during_play() {
        let dir = tempfile::tempdir().unwrap();
        let game = dir.path();
        let mut watcher = WorldWatcher::new(game);
        append_log(game, &[CONNECTING]);
        assert!(watcher.poll().unwrap());
        // 改行を含むサーバーの文字が、そのままログに出た。
        append_log(
            game,
            &[STOPPING, CONNECTING.replace("Example.com", "evil").as_str()],
        );
        assert!(!watcher.poll_while_playing().unwrap());
        assert_eq!(watcher.current(), Some(&mp("example.com", 25565)));
        append_log(game, &[STOPPING]);
        assert!(watcher.poll().unwrap());
        assert_eq!(watcher.state(), &WorldState::NotInWorld);
    }

    #[test]
    fn watcher_reads_the_level_name() {
        let dir = tempfile::tempdir().unwrap();
        let game = dir.path();
        set_lock_time(game, "New World (1)", 1);
        let mut watcher = WorldWatcher::new(game);
        append_log(game, &[STARTING]);
        assert!(watcher.poll().unwrap());
        // level.dat がない：あるのはフォルダの名前だけ。
        assert_eq!(watcher.level_name(), None);
        assert_eq!(watcher.current(), Some(&sp("New World (1)")));
    }

    #[test]
    fn watcher_keeps_the_world_when_the_log_rotates() {
        let dir = tempfile::tempdir().unwrap();
        let game = dir.path();
        let mut watcher = WorldWatcher::new(game);
        append_log(game, &[CONNECTING]);
        assert!(watcher.poll().unwrap());
        assert_eq!(watcher.current(), Some(&mp("example.com", 25565)));

        // 深夜 0 時に log4j は、ワールドの行のない新しい latest.log を始める。
        fs::write(
            latest_log_path(game),
            "[00:00:01] [Render thread/INFO]: [CHAT] <Alex> hello\r\n",
        )
        .unwrap();
        assert!(!watcher.poll().unwrap());
        assert_eq!(watcher.current(), Some(&mp("example.com", 25565)));
        // ログが消えても何も変わらない。
        fs::remove_file(latest_log_path(game)).unwrap();
        assert!(!watcher.poll().unwrap());
        assert_eq!(watcher.current(), Some(&mp("example.com", 25565)));
        // 新しいファイルのイベントは、やはり数える。
        append_log(game, &[STOPPING]);
        assert!(watcher.poll().unwrap());
        assert_eq!(watcher.state(), &WorldState::NotInWorld);
    }

    fn location(dimension: &str, x: f64, y: f64, z: f64) -> Location {
        Location {
            dimension: Some(dimension.into()),
            x,
            y,
            z,
            yaw: 45.0,
            pitch: 10.0,
        }
    }

    fn names(book: &WaypointBook) -> Vec<&str> {
        book.waypoints().iter().map(|w| w.name.as_str()).collect()
    }

    #[test]
    fn book_without_a_world() {
        let dir = tempfile::tempdir().unwrap();
        let mut book = WaypointBook::new(dir.path());
        assert_eq!(book.world(), None);
        assert!(book.waypoints().is_empty());
        assert_eq!(book.problem(), None);
        let here = location(OVERWORLD, 0.0, 64.0, 0.0);
        assert_eq!(book.add(&here, 1), Err(NOT_IN_WORLD.to_owned()));
        assert!(book.rename(1, "x").is_err());
        assert!(book.remove(1).is_err());
        assert_eq!(book.retry_save(), Ok(()));
        assert!(!data_dir_exists(dir.path()));
    }

    fn data_dir_exists(game: &Path) -> bool {
        crate::gamedir::data_dir(game).exists()
    }

    #[test]
    fn book_changes_are_saved_and_reloaded() {
        let dir = tempfile::tempdir().unwrap();
        let world = sp("Survival");
        let mut book = WaypointBook::new(dir.path());
        book.set_world(Some(&world));
        assert_eq!(book.world(), Some(&world));
        assert!(book.waypoints().is_empty());
        // 開くだけでは何も作らない。
        assert!(!data_dir_exists(dir.path()));

        let first = book
            .add(&location(OVERWORLD, 12.3, 64.0, -7.5), 1_700_000_000)
            .unwrap();
        assert_eq!(
            first,
            Waypoint {
                id: 1,
                name: "地点 1".into(),
                dimension: OVERWORLD.into(),
                x: 12.3,
                y: 64.0,
                z: -7.5,
                created_unix: 1_700_000_000,
            }
        );
        let second = book
            .add(&location(THE_NETHER, 1.0, 2.0, 3.0), 1_700_000_100)
            .unwrap();
        assert_eq!((second.id, second.name.as_str()), (2, "地点 2"));
        let third = book
            .add(&location(OVERWORLD, 0.0, 0.0, 0.0), 1_700_000_200)
            .unwrap();
        assert_eq!(names(&book), ["地点 1", "地点 2", "地点 3"]);

        book.rename(first.id, "  拠点  ").unwrap();
        // 空の名前は古い名前のまま。
        book.rename(second.id, "  \t ").unwrap();
        book.rename(second.id, "").unwrap();
        book.remove(third.id).unwrap();
        assert_eq!(names(&book), ["拠点", "地点 2"]);
        assert_eq!(book.rename(99, "x"), Err(NOT_FOUND.to_owned()));
        assert_eq!(book.remove(third.id), Err(NOT_FOUND.to_owned()));
        assert_eq!(book.problem(), None);

        let mut reloaded = WaypointBook::new(dir.path());
        reloaded.set_world(Some(&world));
        assert_eq!(reloaded.waypoints(), book.waypoints());
        // 消した後も id は使い回さない。
        let fourth = reloaded
            .add(&location(OVERWORLD, 5.0, 5.0, 5.0), 1_700_000_300)
            .unwrap();
        assert_eq!((fourth.id, fourth.name.as_str()), (4, "地点 4"));
        let file = waypoints_path(dir.path(), &world);
        assert!(fs::read_to_string(file).unwrap().contains("地点 4"));
    }

    #[test]
    fn book_switches_worlds() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (sp("A"), mp("example.com", 25565));
        let here = location(OVERWORLD, 1.0, 64.0, 1.0);
        let mut book = WaypointBook::new(dir.path());
        book.set_world(Some(&a));
        book.add(&here, 1).unwrap();
        book.add(&here, 2).unwrap();

        book.set_world(Some(&b));
        assert_eq!(book.world(), Some(&b));
        assert!(book.waypoints().is_empty());
        assert_eq!(book.add(&here, 3).unwrap().id, 1);

        book.set_world(None);
        assert_eq!(book.world(), None);
        assert!(book.waypoints().is_empty());
        assert_eq!(book.add(&here, 4), Err(NOT_IN_WORLD.to_owned()));

        book.set_world(Some(&a));
        assert_eq!(names(&book), ["地点 1", "地点 2"]);
        // 同じワールドをもう一度入れても、そのまま残す。
        book.set_world(Some(&a));
        assert_eq!(names(&book), ["地点 1", "地点 2"]);
        assert!(waypoints_path(dir.path(), &b).exists());
    }

    #[test]
    fn book_backs_up_a_corrupt_file() {
        let dir = tempfile::tempdir().unwrap();
        let world = sp("Broken");
        let path = waypoints_path(dir.path(), &world);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "{ not json").unwrap();

        let mut book = WaypointBook::new(dir.path());
        book.set_world(Some(&world));
        assert!(book.waypoints().is_empty());
        let problem = book.problem().unwrap();
        assert!(problem.contains("壊れていた"), "{problem}");
        assert!(problem.contains("sp-Broken.corrupt-"), "{problem}");
        assert!(!path.exists());
        let backups = fs::read_dir(path.parent().unwrap()).unwrap().count();
        assert_eq!(backups, 1);

        // 記録は空のリストから動く。報告はワールドが変わるまで残る。
        book.add(&location(OVERWORLD, 0.0, 0.0, 0.0), 1).unwrap();
        assert!(path.exists());
        assert!(book.problem().is_some());
        book.set_world(None);
        book.set_world(Some(&world));
        assert_eq!(book.problem(), None);
        assert_eq!(names(&book), ["地点 1"]);
    }

    #[test]
    fn book_refuses_a_newer_file() {
        let dir = tempfile::tempdir().unwrap();
        let world = sp("Future");
        let path = waypoints_path(dir.path(), &world);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let newer = r#"{"version": 99, "waypoints": "elsewhere"}"#;
        fs::write(&path, newer).unwrap();

        let mut book = WaypointBook::new(dir.path());
        book.set_world(Some(&world));
        assert!(book.waypoints().is_empty());
        let problem = book.problem().unwrap().to_owned();
        assert!(problem.contains("99"), "{problem}");
        assert_eq!(
            book.add(&location(OVERWORLD, 0.0, 0.0, 0.0), 1),
            Err(problem)
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), newer);
    }

    #[test]
    fn book_reopens_after_an_io_error() {
        let dir = tempfile::tempdir().unwrap();
        let world = sp("Locked");
        // ファイルの場所にあるフォルダは読めない。
        let path = waypoints_path(dir.path(), &world);
        fs::create_dir_all(&path).unwrap();

        let mut book = WaypointBook::new(dir.path());
        book.set_world(Some(&world));
        let problem = book.problem().unwrap().to_owned();
        assert!(
            problem.starts_with("ウェイポイントのファイルを開けなかった："),
            "{problem}"
        );
        let here = location(OVERWORLD, 0.0, 0.0, 0.0);
        assert_eq!(book.add(&here, 1), Err(problem));

        fs::remove_dir(&path).unwrap();
        assert_eq!(book.add(&here, 2).unwrap().name, "地点 1");
        assert_eq!(book.problem(), None);
        assert!(path.is_file());
    }

    #[test]
    fn book_keeps_changes_when_saving_fails() {
        let dir = tempfile::tempdir().unwrap();
        let world = sp("Survival");
        let path = waypoints_path(dir.path(), &world);
        let mut book = WaypointBook::new(dir.path());
        book.set_world(Some(&world));
        // ウェイポイントのフォルダの場所にあるファイル。
        let blocker = path.parent().unwrap().to_owned();
        fs::create_dir_all(blocker.parent().unwrap()).unwrap();
        fs::write(&blocker, "").unwrap();

        let here = location(OVERWORLD, 3.0, 64.0, 4.0);
        let error = book.add(&here, 1).unwrap_err();
        assert!(error.starts_with("保存できなかった："), "{error}");
        // 記憶に残り、メニューがそう言う。
        assert_eq!(names(&book), ["地点 1"]);
        assert!(book.problem().unwrap().starts_with(&error));
        assert!(book.remove(1).is_err());
        assert!(book.waypoints().is_empty());
        book.add(&here, 2).unwrap_err();
        assert!(book.retry_save().is_err());

        // フォルダを作れるようになれば、次の変更が全部を保存する。
        fs::remove_file(&blocker).unwrap();
        book.rename(2, "拠点").unwrap();
        assert_eq!(book.problem(), None);
        let mut reloaded = WaypointBook::new(dir.path());
        reloaded.set_world(Some(&world));
        assert_eq!(names(&reloaded), ["拠点"]);
        assert_eq!(book.retry_save(), Ok(()));
    }

    #[test]
    fn book_retries_the_save_before_switching_worlds() {
        let dir = tempfile::tempdir().unwrap();
        let world = sp("Survival");
        let path = waypoints_path(dir.path(), &world);
        let mut book = WaypointBook::new(dir.path());
        book.set_world(Some(&world));
        let blocker = path.parent().unwrap().to_owned();
        fs::create_dir_all(blocker.parent().unwrap()).unwrap();
        fs::write(&blocker, "").unwrap();
        book.add(&location(OVERWORLD, 0.0, 0.0, 0.0), 1)
            .unwrap_err();

        assert!(book.has_unsaved());

        fs::remove_file(&blocker).unwrap();
        assert_eq!(book.set_world(Some(&sp("Other"))), None);
        assert_eq!(book.problem(), None);
        book.set_world(Some(&world));
        assert_eq!(names(&book), ["地点 1"]);
        assert!(!book.has_unsaved());

        // 切り替えのときにまだ失敗する：変更は失われ、呼び出し側に伝える。
        let blocker = path.parent().unwrap().to_owned();
        fs::remove_dir_all(&blocker).unwrap();
        fs::write(&blocker, "").unwrap();
        book.add(&location(OVERWORLD, 1.0, 0.0, 0.0), 2)
            .unwrap_err();
        let lost = book.set_world(Some(&sp("Other")));
        assert_eq!(
            lost.as_deref(),
            Some("「Survival」のウェイポイントの変更を保存できずに失った（ログを見る）")
        );
        assert!(!book.has_unsaved());
    }

    #[test]
    fn book_retry_save() {
        let dir = tempfile::tempdir().unwrap();
        let world = sp("Survival");
        let path = waypoints_path(dir.path(), &world);
        let mut book = WaypointBook::new(dir.path());
        book.set_world(Some(&world));
        let blocker = path.parent().unwrap().to_owned();
        fs::create_dir_all(blocker.parent().unwrap()).unwrap();
        fs::write(&blocker, "").unwrap();
        book.add(&location(OVERWORLD, 0.0, 0.0, 0.0), 1)
            .unwrap_err();
        fs::remove_file(&blocker).unwrap();
        assert_eq!(book.retry_save(), Ok(()));
        assert_eq!(book.problem(), None);
        assert!(path.is_file());
    }

    #[test]
    fn deferred_saves_are_handed_out_and_reported_back() {
        let dir = tempfile::tempdir().unwrap();
        let game = dir.path();
        let mut book = WaypointBook::deferred(game);
        book.set_world(Some(&sp("w")));
        let revision = book.revision();
        let added = book.add(&location(OVERWORLD, 1.0, 2.0, 3.0), 1).unwrap();
        assert_eq!(added.name, "地点 1");
        assert!(book.revision() > revision);
        // 仕事が書かれるまで、何も書かれない。
        let path = waypoints_path(game, &sp("w"));
        assert!(!path.exists());
        book.rename(1, "home").unwrap();
        let jobs = book.take_saves();
        assert_eq!(jobs.len(), 1, "the newer save replaces the older");
        assert!(book.has_unsaved());
        // 失敗は 1 回だけ伝え、報告の後にだけ試し直す。
        assert_eq!(book.retry_save(), Ok(()));
        assert!(book.take_saves().is_empty());
        let told = book.save_done(&jobs[0], Err("disk full".into()));
        assert_eq!(
            told.as_deref(),
            Some("保存できなかった：disk full（あとで保存し直す）")
        );
        assert_eq!(book.problem(), told.as_deref());
        book.retry_save().unwrap();
        let jobs = book.take_saves();
        assert_eq!(jobs.len(), 1);
        crate::waypoint::write_file(&jobs[0].path, &jobs[0].bytes).unwrap();
        assert_eq!(book.save_done(&jobs[0], Ok(())), None);
        assert!(!book.has_unsaved());
        assert_eq!(book.problem(), None);
        let store = WaypointStore::open(&path, "w").unwrap();
        assert_eq!(store.waypoints()[0].name, "home");
    }

    #[test]
    fn a_deferred_save_of_a_world_left_is_still_written_or_told_lost() {
        let dir = tempfile::tempdir().unwrap();
        let game = dir.path();
        let mut book = WaypointBook::deferred(game);
        book.set_world(Some(&sp("a")));
        book.add(&location(OVERWORLD, 0.0, 0.0, 0.0), 1).unwrap();
        // 保存を渡す前の切り替えでも、それを残す。
        assert_eq!(book.set_world(Some(&sp("b"))), None);
        let jobs = book.take_saves();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].path, waypoints_path(game, &sp("a")));
        assert_eq!(
            book.save_done(&jobs[0], Err("denied".into())).as_deref(),
            Some("「a」のウェイポイントの変更を保存できずに失った（ログを見る）")
        );
        assert_eq!(book.problem(), None);
    }

    #[test]
    fn server_labels_are_kept_by_address() {
        let dir = tempfile::tempdir().unwrap();
        let game = dir.path();
        let server = mp("example.com", 25565);
        let mut labels = ServerLabels::new(game);
        assert_eq!(labels.apply(&server), server);
        let labelled = server.with_label(Some("survival"));
        let (path, bytes) = labels.set(&labelled).unwrap();
        crate::waypoint::write_file(&path, &bytes).unwrap();
        assert_eq!(labels.apply(&server), labelled);
        // ファイルから読み戻す。
        let mut again = ServerLabels::new(game);
        assert_eq!(again.apply(&server), labelled);
        assert_eq!(
            again.apply(&mp("example.com", 25566)),
            mp("example.com", 25566)
        );
        assert_eq!(again.apply(&sp("w")), sp("w"));
        assert!(again.set(&sp("w")).is_none());
        // ラベルなし：またアドレスだけ。
        let (path, bytes) = again.set(&server).unwrap();
        crate::waypoint::write_file(&path, &bytes).unwrap();
        assert_eq!(ServerLabels::new(game).apply(&server), server);
    }

    #[test]
    fn unix_now_is_recent() {
        // 2020-09-13 以降。
        assert!(unix_now() > 1_600_000_000);
    }
}
