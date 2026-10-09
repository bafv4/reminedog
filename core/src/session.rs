//! The world the player is in, and that world's waypoints.
//!
//! [`WorldWatcher`] follows `latest.log` and the saves folder. [`WaypointBook`] holds the
//! current world's [`WaypointStore`] and saves every change, at once or (deferred) on another
//! thread. [`ServerLabels`] keeps the label last used on each server. Errors meant for the user
//! are Japanese UI text; the details go to the log in English.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use crate::gamedir::{data_dir, latest_log_path, saves_dir, waypoints_path};
use crate::location::Location;
use crate::waypoint::{StoreError, Waypoint, WaypointStore};
use crate::world::{LogEvent, LogTail, WorldId, WorldTracker, level_name, might_be_world_line};

/// The world opened when an integrated server starts was locked at most this long before the
/// start line was read. An older lock means the line did not come from opening a world (a
/// server can put lines in the log through text the game logs as it is).
const LOCK_WINDOW: Duration = Duration::from_secs(300);

/// Where the player is, according to the log and the saves folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorldState {
    /// Before any world, or after the integrated server stopped.
    NotInWorld,
    /// An integrated server started, but its world folder was not found.
    Unknown,
    In(WorldId),
}

/// Follows latest.log and the saves folder to know which world or server the player is in.
#[derive(Debug)]
pub struct WorldWatcher {
    tail: LogTail,
    tracker: WorldTracker,
    saves: PathBuf,
    /// The last event entered a world (the tracker's `resolve` gives `None` both after
    /// leaving one and when the singleplayer world cannot be found).
    entered: bool,
    /// When the last integrated server start was read.
    started_at: Option<SystemTime>,
    state: WorldState,
    /// The singleplayer world's name in the game, read when it was entered.
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

    /// Reads new lines. A tail restart (latest.log rotated, e.g. at midnight) does NOT reset the
    /// tracker: the startup rotation happens before the agent's first frame, so later restarts
    /// are mid-session and the new file has no world lines yet. Re-resolves once after new
    /// world events. Returns true when `state()` changed. A missing log gives `Ok(false)`.
    pub fn poll(&mut self) -> io::Result<bool> {
        self.poll_lines(true)
    }

    /// [`poll`](Self::poll) when the player was in the game (no screen) all the time since the
    /// last poll: the game changes worlds only through its screens, so world lines read now
    /// were put in the log by a server (in text the game logs as it is) and are ignored.
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

    /// Re-resolves now (singleplayer: the newest session.lock, locked shortly before the
    /// server started); true when `state()` changed.
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

    /// The singleplayer world's name in the game (its `level.dat`), if it could be read.
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

/// A save of a world's waypoints, to write with [`write_file`](crate::waypoint::write_file)
/// on another thread and report back with [`WaypointBook::save_done`].
#[derive(Debug)]
pub struct SaveJob {
    pub path: PathBuf,
    pub bytes: Vec<u8>,
    /// The store this came from, and its change.
    opened: u64,
    revision: u64,
    world: String,
}

/// The waypoints of the current world. Memory is the truth: every change is saved, at once or
/// (deferred) by [`take_saves`](Self::take_saves) on another thread; if saving fails the
/// change stays in memory, `problem` says so, and the next change (or `retry_save`) saves
/// again.
#[derive(Debug)]
pub struct WaypointBook {
    game_dir: PathBuf,
    world: Option<WorldId>,
    store: Option<WaypointStore>,
    /// Why the store could not be opened, or that a corrupt file was moved aside.
    open_problem: Option<String>,
    /// The last save failed; cleared by the next successful one.
    save_problem: Option<String>,
    /// Changes that are not on disk yet.
    dirty: bool,
    /// Opening failed with an I/O error, which may pass; `add` tries again.
    reopen: bool,
    /// Saves are handed out by `take_saves` instead of written.
    deferred: bool,
    /// Saves to hand out, the newest for each file.
    queued: Vec<SaveJob>,
    /// Saves handed out and not reported back yet.
    in_flight: usize,
    /// Counts the stores opened, so a late report is told from one for the store open now.
    opened: u64,
    /// Counts changes to what [`waypoints`](Self::waypoints) returns (also a world switch).
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

    /// A book whose saves are written elsewhere: changes queue a [`SaveJob`] for
    /// [`take_saves`](Self::take_saves), and the outcome comes back through
    /// [`save_done`](Self::save_done). Changes succeed even when saving later fails.
    pub fn deferred(game_dir: &Path) -> Self {
        Self {
            deferred: true,
            ..Self::new(game_dir)
        }
    }

    /// The saves to write, oldest first.
    pub fn take_saves(&mut self) -> Vec<SaveJob> {
        self.in_flight += self.queued.len();
        std::mem::take(&mut self.queued)
    }

    /// The outcome of writing `job`. Returns a message for the UI: changes to a world left
    /// since that are lost, or a new failure (`problem` says so as long as it lasts).
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

    /// Counts changes to the waypoints and switches of the world, so a view of
    /// [`waypoints`](Self::waypoints) is built again only when this changed.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Switches to `world` (None = none): tries to save a dirty store first, then opens
    /// `<game>/reminedog/waypoints/<stem>.json` with `open_or_backup`. On error the store is
    /// None and `problem` says why. A corrupt file that was backed up is reported too. The world
    /// already open is kept as it is. Returns a message for the UI when changes to the old
    /// world could not be saved and are lost.
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
        // The last change's save is queued already, unless it failed: then once more.
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

    /// The current world's waypoints in the order they were added; empty without a store.
    pub fn waypoints(&self) -> &[Waypoint] {
        self.store.as_ref().map_or(&[], |store| store.waypoints())
    }

    /// A problem to show in the menu: a failed save, or the store's file.
    pub fn problem(&self) -> Option<&str> {
        self.save_problem
            .as_deref()
            .or(self.open_problem.as_deref())
    }

    /// Adds a waypoint named "地点 {id}" at `location`. If the store could not be opened for an
    /// I/O error, tries to reopen it first. A failed save is an error, but the waypoint stays.
    pub fn add(&mut self, location: &Location, now_unix: i64) -> Result<Waypoint, String> {
        if self.store.is_none() && self.reopen {
            self.open();
        }
        let store = self.store_mut()?;
        // The store names unnamed waypoints in English; give them the Japanese name instead.
        let mut waypoint = store.add("", location, now_unix).clone();
        waypoint.name = format!("地点 {}", waypoint.id);
        store.rename(waypoint.id, &waypoint.name);
        self.changed();
        self.save()?;
        Ok(waypoint)
    }

    /// Renames a waypoint. An empty (trimmed) name keeps the old name. Unknown id -> Err.
    pub fn rename(&mut self, id: u64, name: &str) -> Result<(), String> {
        let store = self.store_mut()?;
        if store.get(id).is_none() {
            return Err(NOT_FOUND.into());
        }
        // The store would turn an empty name into "Waypoint {id}".
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

    /// Changes an earlier save failed to write are waiting.
    pub fn has_unsaved(&self) -> bool {
        self.dirty
    }

    /// Saves changes that an earlier save failed to write. Nothing to do without any, or
    /// (deferred) while a save is on its way.
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

    /// Notes a failed save (`detail`: why); returns the message when the failure is new.
    fn save_failed(&mut self, detail: &str) -> Option<String> {
        let problem = format!("保存できなかった：{detail}（あとで保存し直す）");
        // A lasting error is logged and told once, not at every retry.
        if self.save_problem.as_ref() == Some(&problem) {
            log::debug!("waypoints: still cannot save: {detail}");
            return None;
        }
        log::warn!("waypoints: cannot save: {detail}");
        self.save_problem = Some(problem.clone());
        Some(problem)
    }
}

/// What went wrong with a waypoint file, for the UI.
fn store_error_text(e: &StoreError) -> String {
    match e {
        StoreError::Io { source, .. } => source.to_string(),
        other => other.to_string(),
    }
}

/// The label last used on each server address, so the next visit opens the same waypoints:
/// `<game>/reminedog/waypoints/labels.json`, `{"version": 1, "labels": {"<host>:<port>":
/// "<label>"}}`. Read once, when first needed; a file that cannot be read gives no labels.
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

    /// `world` under the label last used on its address (a singleplayer world as it is).
    pub fn apply(&mut self, world: &WorldId) -> WorldId {
        let Some(address) = world.address_key() else {
            return world.clone();
        };
        let label = self.labels().get(&address).cloned();
        world.with_label(label.as_deref())
    }

    /// Keeps `world`'s label for its address; the file to write ([`write_file`]
    /// (crate::waypoint::write_file)), or `None` for a singleplayer world.
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

/// Seconds since the Unix epoch (0 if the clock is set before it), for `created_unix`.
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

    /// Locks `world` `secs_ago` seconds ago.
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
        // Nothing new, nothing changed.
        assert!(!watcher.poll().unwrap());
        assert!(!watcher.refresh());

        append_log(game, &[STOPPING]);
        assert!(watcher.poll().unwrap());
        assert_eq!(watcher.state(), &WorldState::NotInWorld);
        assert_eq!(watcher.current(), None);

        append_log(game, &[CONNECTING]);
        assert!(watcher.poll().unwrap());
        assert_eq!(watcher.current(), Some(&mp("example.com", 25565)));

        // Back to singleplayer in another world, all within one poll.
        set_lock_time(game, "Creative", 10);
        append_log(game, &[STOPPING, STARTING]);
        assert!(watcher.poll().unwrap());
        assert_eq!(watcher.current(), Some(&sp("Creative")));
        // Entering and leaving between two polls changes nothing.
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
        // Nothing new in the log: polling does not look at the saves folder again.
        set_lock_time(game, "New World", 50);
        assert!(!watcher.poll().unwrap());
        assert_eq!(watcher.state(), &WorldState::Unknown);
        assert!(watcher.refresh());
        assert_eq!(watcher.state(), &WorldState::In(sp("New World")));
        assert!(!watcher.refresh());
        // Stopping goes back to "not in a world", not "unknown".
        append_log(game, &[STOPPING]);
        assert!(watcher.poll().unwrap());
        assert_eq!(watcher.state(), &WorldState::NotInWorld);
    }

    #[test]
    fn watcher_needs_a_world_opened_just_now() {
        let dir = tempfile::tempdir().unwrap();
        let game = dir.path();
        // Played an hour ago; a start line now does not mean that world.
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
        // A server's text with a line break, logged as it is.
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
        // No level.dat: the folder name is all there is.
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

        // At midnight log4j starts a new latest.log without the world's lines.
        fs::write(
            latest_log_path(game),
            "[00:00:01] [Render thread/INFO]: [CHAT] <Alex> hello\r\n",
        )
        .unwrap();
        assert!(!watcher.poll().unwrap());
        assert_eq!(watcher.current(), Some(&mp("example.com", 25565)));
        // A removed log changes nothing either.
        fs::remove_file(latest_log_path(game)).unwrap();
        assert!(!watcher.poll().unwrap());
        assert_eq!(watcher.current(), Some(&mp("example.com", 25565)));
        // Events in the new file still count.
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
        // Opening alone creates nothing.
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
        // An empty name keeps the old one.
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
        // Ids are not reused after a removal.
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
        // Setting the same world again keeps it as it is.
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

        // The book works from an empty list; the report stays until the world changes.
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
        // A folder where the file should be cannot be read.
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
        // A file where the waypoints folder should be.
        let blocker = path.parent().unwrap().to_owned();
        fs::create_dir_all(blocker.parent().unwrap()).unwrap();
        fs::write(&blocker, "").unwrap();

        let here = location(OVERWORLD, 3.0, 64.0, 4.0);
        let error = book.add(&here, 1).unwrap_err();
        assert!(error.starts_with("保存できなかった："), "{error}");
        // Kept in memory, and the menu says so.
        assert_eq!(names(&book), ["地点 1"]);
        assert!(book.problem().unwrap().starts_with(&error));
        assert!(book.remove(1).is_err());
        assert!(book.waypoints().is_empty());
        book.add(&here, 2).unwrap_err();
        assert!(book.retry_save().is_err());

        // Once the folder can be created, the next change saves everything.
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

        // Still failing at the switch: the change is lost, and the caller is told.
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
        // Nothing is written until the job is.
        let path = waypoints_path(game, &sp("w"));
        assert!(!path.exists());
        book.rename(1, "home").unwrap();
        let jobs = book.take_saves();
        assert_eq!(jobs.len(), 1, "the newer save replaces the older");
        assert!(book.has_unsaved());
        // A failure is told once, and retried only after the report.
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
        // Switching before the save was handed out keeps it.
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
        // Read back from the file.
        let mut again = ServerLabels::new(game);
        assert_eq!(again.apply(&server), labelled);
        assert_eq!(
            again.apply(&mp("example.com", 25566)),
            mp("example.com", 25566)
        );
        assert_eq!(again.apply(&sp("w")), sp("w"));
        assert!(again.set(&sp("w")).is_none());
        // No label: the address alone again.
        let (path, bytes) = again.set(&server).unwrap();
        crate::waypoint::write_file(&path, &bytes).unwrap();
        assert_eq!(ServerLabels::new(game).apply(&server), server);
    }

    #[test]
    fn unix_now_is_recent() {
        // 2020-09-13 or later.
        assert!(unix_now() > 1_600_000_000);
    }
}
