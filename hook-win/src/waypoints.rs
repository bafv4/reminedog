//! Waypoints on the swap path: the world the player is in (latest.log and the saves folder),
//! Minecraft's key bindings (options.txt: the debug keys, and every key's mappings for the
//! menu's key rebinding), the hotkeys and menu commands that become F3+C requests ([`f3c`]), and
//! the positions and failures that come back as waypoints and notices.
//!
//! The state is a static, so it outlives the overlay's `Runtime` (rebuilt when the game's
//! device context changes). Only the frame code uses it, inside the swap detour. With it
//! locked the router and the f3c state may be locked, never the other way round, and no game
//! code runs.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime};

use reminedog_core::{
    DebugKeys, InputId, Location, Naming, SaveJob, ServerLabels, UNBOUND, Waypoint, WaypointBook,
    WorldId, WorldState, WorldWatcher, bindings_by_key, glfw_key, glfw_key_of, input_by_name,
    input_label, key_label, mapping_label, options_path, parse_debug_keys, unix_now,
};
use reminedog_render::{
    HotkeyAction, Key, Notice, WaypointCommand, WaypointView, WorldLabel, format_xyz,
    navigation_text, turn_to,
};

use crate::f3c::{self, DebugKey, Failure, Job, Outcome, Purpose};
use crate::input;
use crate::saver;

/// How often latest.log is read for world changes.
const POLL_INTERVAL: Duration = Duration::from_secs(1);
/// How often options.txt is looked at for changed key bindings (also right before a request).
const KEYS_INTERVAL: Duration = Duration::from_secs(5);
/// A larger options.txt is not read.
const MAX_OPTIONS_LEN: u64 = 1024 * 1024;
/// How often waypoints an earlier save failed to write are saved again.
const RETRY_INTERVAL: Duration = Duration::from_secs(10);
/// A request the platform did not take within this long fails.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
/// How long notices show, in seconds.
const NOTICE_SECONDS: f64 = 6.0;
const NAVIGATION_SECONDS: f64 = 8.0;

const BLOCKED: &str = "このワールドでは座標を取得できない（メニューの「もう一度試す」で再開）";
const NO_DESTINATION: &str = "目的地が選ばれていない（メニューで選ぶ）";
const UNKNOWN_WORLD: &str =
    "ワールドを判定できていないので記録できない（ワールドに入り直すと判定できる）";

static WAYPOINTS: Mutex<Option<Waypoints>> = Mutex::new(None);

/// What the overlay gets from the waypoints this frame.
pub struct Frame {
    pub view: WaypointView,
    pub notices: Vec<Notice>,
    /// Minecraft's debug modifier, copy-location and crash keys.
    pub reserved_keys: Vec<Key>,
}

/// Every frame of the overlay's window, before the overlay renders: follows the world and the
/// key bindings, turns F3+C outcomes into waypoints and notices, and the hotkeys pressed since
/// the last frame into requests. `playing`: the game has captured the cursor. `naming`: how
/// the hooked window library's Minecraft names keys in options.txt.
pub fn before_frame(game_dir: &Path, window: usize, playing: bool, naming: Naming) -> Frame {
    let mut state = lock();
    let now = Instant::now();
    let waypoints = state.get_or_insert_with(|| Waypoints::new(game_dir, now));
    waypoints.naming = naming;
    if !playing {
        waypoints.in_game_since_poll = false;
    }
    if now >= waypoints.next_poll {
        waypoints.next_poll = now + POLL_INTERVAL;
        waypoints.poll_world();
    }
    waypoints.poll_saves();
    if !waypoints.book.has_unsaved() {
        // The first retry comes an interval after the failure, not at the next frame.
        waypoints.next_retry = now + RETRY_INTERVAL;
    } else if now >= waypoints.next_retry {
        waypoints.next_retry = now + RETRY_INTERVAL;
        let _ = waypoints.book.retry_save();
    }
    waypoints.submit_saves();
    if now >= waypoints.next_keys {
        waypoints.next_keys = now + KEYS_INTERVAL;
        waypoints.load_keys(naming);
    }
    f3c::expire(REQUEST_TIMEOUT);
    for outcome in f3c::take_outcomes() {
        waypoints.on_outcome(outcome, now);
    }
    let actions = input::router().take_actions();
    for action in actions {
        if !playing {
            log::debug!("waypoints: {action:?} dropped; the game shows a screen");
            continue;
        }
        let job = match action {
            HotkeyAction::RecordWaypoint => waypoints.job(Purpose::Record, window),
            HotkeyAction::Navigate => waypoints.navigate(window),
        };
        waypoints.send(job);
    }
    waypoints.submit_saves();
    Frame {
        view: waypoints.view(playing, f3c::busy(), now),
        notices: std::mem::take(&mut waypoints.pending_notices),
        reserved_keys: waypoints.reserved_keys.clone(),
    }
}

/// The game's mappings on each key, as options.txt had them at the last read (for the menu's
/// key rebinding).
pub fn game_bindings() -> Vec<(InputId, Vec<String>)> {
    lock()
        .as_ref()
        .map(|waypoints| waypoints.game_bindings.clone())
        .unwrap_or_default()
}

/// The menu's waypoint commands from the frame just rendered; their notices show next frame.
pub fn after_frame(commands: Vec<WaypointCommand>, window: usize) {
    if commands.is_empty() {
        return;
    }
    let mut state = lock();
    let Some(waypoints) = state.as_mut() else {
        return;
    };
    for command in commands {
        let job = waypoints.command(command, window);
        waypoints.send(job);
    }
    waypoints.submit_saves();
}

fn lock() -> MutexGuard<'static, Option<Waypoints>> {
    WAYPOINTS.lock().unwrap_or_else(|e| e.into_inner())
}

/// A file's modification time and size (`None`: there was no file).
type FileStamp = Option<(Option<SystemTime>, u64)>;

struct Waypoints {
    game_dir: PathBuf,
    watcher: WorldWatcher,
    book: WaypointBook,
    /// The label last used on each server.
    labels: ServerLabels,
    /// Saves on the saving thread: the waypoints', and the labels' (`None`).
    saving: Vec<(saver::Pending, Option<SaveJob>)>,
    /// The waypoints as the menu shows them, made again when the book's revision changes.
    shown: (u64, Arc<[Waypoint]>),
    next_poll: Instant,
    next_keys: Instant,
    next_retry: Instant,
    /// The game had the cursor in every frame since the last poll of the log.
    in_game_since_poll: bool,
    /// How the window library's Minecraft names keys.
    naming: Naming,
    keys: DebugKeys,
    /// options.txt was read at least once.
    keys_read: bool,
    /// options.txt as it was read, and the naming it was read with.
    keys_stamp: Option<(FileStamp, Naming)>,
    /// The menu's warnings about `keys`.
    key_problems: Vec<String>,
    reserved_keys: Vec<Key>,
    /// Every key's mappings in options.txt (`bindings_by_key`).
    game_bindings: Vec<(InputId, Vec<String>)>,
    /// The player's last known position, and when it came.
    location: Option<(Location, Instant)>,
    /// The destination.
    selected: Option<u64>,
    /// The game refused F3+C in this world; nothing is sent until the menu's retry or another
    /// world.
    blocked: bool,
    pending_notices: Vec<Notice>,
    /// Error kinds already logged, so a lasting error is logged once.
    log_errors: Vec<io::ErrorKind>,
    keys_errors: Vec<io::ErrorKind>,
}

impl Waypoints {
    /// Reads nothing yet: the first frame polls the log and reads options.txt.
    fn new(game_dir: &Path, now: Instant) -> Self {
        let keys = DebugKeys::default();
        Self {
            game_dir: game_dir.to_owned(),
            watcher: WorldWatcher::new(game_dir),
            book: WaypointBook::deferred(game_dir),
            labels: ServerLabels::new(game_dir),
            saving: Vec::new(),
            shown: (u64::MAX, Arc::from([])),
            next_poll: now,
            next_keys: now,
            next_retry: now,
            in_game_since_poll: false,
            naming: Naming::Modern,
            key_problems: binding_problems(&keys),
            reserved_keys: reserved_keys(&keys),
            game_bindings: Vec::new(),
            keys,
            keys_read: false,
            keys_stamp: None,
            location: None,
            selected: None,
            blocked: false,
            pending_notices: Vec::new(),
            log_errors: Vec::new(),
            keys_errors: Vec::new(),
        }
    }

    /// Reads the log for world changes. World lines read after the game had the cursor all
    /// the time since the last poll did not come from its screens: they are ignored.
    fn poll_world(&mut self) {
        let polled = if self.in_game_since_poll {
            self.watcher.poll_while_playing()
        } else {
            self.watcher.poll()
        };
        self.in_game_since_poll = true;
        match polled {
            Ok(true) => self.world_changed(),
            Ok(false) => {}
            Err(e) => log_once(&mut self.log_errors, &e, "world: cannot read latest.log"),
        }
    }

    /// Opens the new world's waypoints (a server's under its last label); what belonged to the
    /// old world goes.
    fn world_changed(&mut self) {
        let world = self.watcher.current().map(|world| self.labels.apply(world));
        self.switch_book(world.as_ref());
        self.location = None;
        self.blocked = false;
        match self.watcher.state() {
            WorldState::NotInWorld => log::info!("world: not in a world"),
            WorldState::Unknown => log::info!("world: unknown"),
            WorldState::In(_) => {
                log::info!(
                    "world: {}",
                    world
                        .as_ref()
                        .map_or_else(String::new, WorldId::display_name)
                )
            }
        }
    }

    fn switch_book(&mut self, world: Option<&WorldId>) {
        if let Some(lost) = self.book.set_world(world) {
            self.warn(lost);
        }
        self.selected = None;
    }

    /// Hands the book's saves to the saving thread.
    fn submit_saves(&mut self) {
        for mut job in self.book.take_saves() {
            let bytes = std::mem::take(&mut job.bytes);
            self.saving
                .push((saver::write(job.path.clone(), bytes), Some(job)));
        }
    }

    /// Takes in the saves done since the last frame.
    fn poll_saves(&mut self) {
        let mut i = 0;
        while i < self.saving.len() {
            let Some(result) = self.saving[i].0.poll() else {
                i += 1;
                continue;
            };
            let (_, job) = self.saving.remove(i);
            let Some(job) = job else {
                if let Err(e) = result {
                    log::warn!("waypoints: cannot save the servers' labels: {e}");
                    self.warn(format!("ラベルを保存できなかった：{e}"));
                }
                continue;
            };
            let was_failing = self.book.problem().is_some();
            let result = result.map_err(|e| e.to_string());
            let ok = result.is_ok();
            if let Some(text) = self.book.save_done(&job, result) {
                self.warn(text);
            } else if ok && was_failing && self.book.problem().is_none() {
                log::info!("waypoints: saved after an earlier failure");
            }
        }
    }

    /// Reads the key bindings if options.txt changed since (or `naming` did), with key names as
    /// `naming` has them; on an error the ones read before stay. A missing file (a fresh
    /// instance) has the defaults.
    fn load_keys(&mut self, naming: Naming) {
        let path = options_path(&self.game_dir);
        let file = match fs::metadata(&path) {
            Ok(meta) => Some((meta.modified().ok(), meta.len())),
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => {
                log_once(&mut self.keys_errors, &e, "options.txt: cannot read");
                return;
            }
        };
        let stamp = Some((file, naming));
        if self.keys_read && stamp == self.keys_stamp {
            return;
        }
        let text = match file {
            Some((_, len)) if len > MAX_OPTIONS_LEN => {
                let e = io::Error::other(format!("{len} bytes, too large"));
                log_once(&mut self.keys_errors, &e, "options.txt: not read");
                return;
            }
            Some(_) => match fs::read(&path) {
                Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
                Err(e) => {
                    log_once(&mut self.keys_errors, &e, "options.txt: cannot read");
                    return;
                }
            },
            None => {
                if !self.keys_read {
                    log::info!("options.txt: not found; the game's default keys are assumed");
                }
                String::new()
            }
        };
        self.keys_stamp = stamp;
        self.game_bindings = bindings_by_key(&text, naming);
        let keys = parse_debug_keys(&text);
        // The user's own F3+C: the copy key as the platform reports it to the game.
        let copy = input_by_name(&keys.copy_location, naming);
        f3c::set_passive_keys(
            copy.and_then(glfw_key_of)
                .map(|(key, _)| key)
                .filter(|&key| key != -1),
            match copy {
                Some(InputId::Key(scancode)) => Some(u32::from(scancode)),
                _ => None,
            },
        );
        input::router().set_debug_modifier(input_by_name(&keys.modifier, naming));
        if self.keys_read && keys == self.keys {
            return;
        }
        self.keys_read = true;
        log::info!(
            "options.txt: debug modifier {}, copy location {}, crash {}{}",
            keys.modifier,
            keys.copy_location,
            keys.crash,
            if keys.shared_with_copy.is_empty() {
                String::new()
            } else {
                format!(
                    "; also on the copy key: {}",
                    keys.shared_with_copy.join(", ")
                )
            }
        );
        self.key_problems = binding_problems(&keys);
        self.reserved_keys = reserved_keys(&keys);
        self.keys = keys;
    }

    fn on_outcome(&mut self, outcome: Outcome, now: Instant) {
        match outcome {
            Outcome::Located { location, purpose } => self.located(location, purpose, now),
            Outcome::Failed { purpose, reason } => self.failed(purpose, reason),
        }
    }

    /// A position from F3+C; `purpose` is `None` for the user's own F3+C.
    fn located(&mut self, location: Location, purpose: Option<Purpose>, now: Instant) {
        // The position belongs to a world entered since the last poll, if any.
        self.poll_world();
        if purpose == Some(Purpose::Record) && self.book.world().is_none() && self.watcher.refresh()
        {
            // The world folder was not found when the server started; it may be there now.
            self.world_changed();
        }
        if purpose.is_none() && self.blocked {
            log::info!("F3+C: the user's own F3+C worked; requests allowed again");
            self.blocked = false;
        }
        match purpose {
            Some(Purpose::Record) => self.record(&location),
            Some(Purpose::Refresh) | None => self.show_way(&location),
        }
        self.location = Some((location, now));
    }

    fn record(&mut self, location: &Location) {
        if self.book.world().is_none() {
            self.warn(UNKNOWN_WORLD);
            return;
        }
        match self.book.add(location, unix_now()) {
            Ok(waypoint) => {
                log::info!("waypoints: waypoint {} recorded", waypoint.id);
                self.notice(
                    format!(
                        "「{}」を記録した（{}）",
                        waypoint.name,
                        format_xyz(waypoint.x, waypoint.y, waypoint.z)
                    ),
                    false,
                    NOTICE_SECONDS,
                );
            }
            Err(e) => self.warn(e),
        }
    }

    /// The way from `from` to the destination, if one is selected; it replaces the way shown
    /// from an older position.
    fn show_way(&mut self, from: &Location) {
        if let Some(notice) = self.destination().map(|waypoint| Notice {
            arrow: turn_to(from, waypoint),
            ..Notice::new(
                navigation_text(&waypoint.name, from, waypoint),
                false,
                NAVIGATION_SECONDS,
            )
            .replacing(Notice::NAVIGATION)
        }) {
            self.pending_notices.push(notice);
        }
    }

    fn failed(&mut self, purpose: Purpose, reason: Failure) {
        if reason == Failure::Refused && !self.blocked {
            log::info!(
                "F3+C: {purpose:?} refused; no more requests in this world until the menu's retry"
            );
            self.blocked = true;
        }
        self.warn(failure_text(&reason, &self.keys));
    }

    /// The navigate hotkey: refreshes the position for the way to the destination. Without a
    /// destination nothing is sent (F3+C would only print its chat line).
    fn navigate(&mut self, window: usize) -> Option<Job> {
        if self.destination().is_none() {
            self.notice(NO_DESTINATION, false, NOTICE_SECONDS);
            return None;
        }
        self.job(Purpose::Refresh, window)
    }

    /// The request to send, unless the game refused F3+C in this world. The key bindings are
    /// read again first if options.txt changed (the user may just have changed them).
    fn job(&mut self, purpose: Purpose, window: usize) -> Option<Job> {
        self.load_keys(self.naming);
        if self.blocked {
            log::info!("F3+C: {purpose:?} not sent; the game refused F3+C in this world");
            self.warn(BLOCKED);
            return None;
        }
        Some(Job {
            purpose,
            window,
            keys: self.keys.clone(),
        })
    }

    fn send(&mut self, job: Option<Job>) {
        let Some(job) = job else {
            return;
        };
        let purpose = job.purpose;
        if let Err(reason) = f3c::request(job) {
            self.failed(purpose, reason);
        }
    }

    /// Acts on a menu command; `Record` and `Refresh` give the request to send.
    fn command(&mut self, command: WaypointCommand, window: usize) -> Option<Job> {
        match command {
            WaypointCommand::Record => return self.job(Purpose::Record, window),
            // The menu's button refreshes the position even without a destination.
            WaypointCommand::Refresh => return self.job(Purpose::Refresh, window),
            WaypointCommand::Select(id) => {
                self.selected = id.filter(|&id| self.waypoint(id).is_some());
            }
            WaypointCommand::Rename { id, name } => match self.book.rename(id, &name) {
                Ok(()) => log::info!("waypoints: waypoint {id} renamed"),
                Err(e) => self.warn(e),
            },
            WaypointCommand::Delete(id) => {
                match self.book.remove(id) {
                    Ok(()) => log::info!("waypoints: waypoint {id} removed"),
                    Err(e) => self.warn(e),
                }
                // Gone from memory even if the file could not be saved.
                if self.selected == Some(id) && self.waypoint(id).is_none() {
                    self.selected = None;
                }
            }
            WaypointCommand::Unblock => {
                if std::mem::take(&mut self.blocked) {
                    log::info!("F3+C: requests allowed again from the menu");
                }
            }
            WaypointCommand::SetLabel(label) => self.set_label(label.as_deref()),
        }
        None
    }

    /// On a server: keeps its waypoints under `label` from now on (and on later visits).
    fn set_label(&mut self, label: Option<&str>) {
        let Some(world) = self
            .book
            .world()
            .filter(|world| world.address_key().is_some())
            .map(|world| world.with_label(label))
        else {
            return;
        };
        if self.book.world() == Some(&world) {
            return;
        }
        if let Some((path, bytes)) = self.labels.set(&world) {
            self.saving.push((saver::write(path, bytes), None));
        }
        log::info!("waypoints: server label changed");
        self.switch_book(Some(&world));
    }

    fn waypoint(&self, id: u64) -> Option<&Waypoint> {
        self.book
            .waypoints()
            .iter()
            .find(|waypoint| waypoint.id == id)
    }

    fn destination(&self) -> Option<&Waypoint> {
        self.selected.and_then(|id| self.waypoint(id))
    }

    fn notice(&mut self, text: impl Into<String>, warn: bool, seconds: f64) {
        self.pending_notices.push(Notice::new(text, warn, seconds));
    }

    fn warn(&mut self, text: impl Into<String>) {
        self.notice(text, true, NOTICE_SECONDS);
    }

    fn view(&mut self, playing: bool, busy: bool, now: Instant) -> WaypointView {
        let mut problems: Vec<String> =
            self.book.problem().map(str::to_owned).into_iter().collect();
        problems.extend(self.key_problems.iter().cloned());
        let revision = self.book.revision();
        if self.shown.0 != revision {
            self.shown = (revision, Arc::from(self.book.waypoints()));
        }
        let label = match self.book.world() {
            Some(WorldId::Multiplayer { label, .. }) => label.clone(),
            _ => None,
        };
        WaypointView {
            world: world_label(self.watcher.state(), self.watcher.level_name()),
            label,
            waypoints: self.shown.1.clone(),
            selected: self.selected,
            location: self.location.as_ref().map(|(location, _)| location.clone()),
            location_age: self
                .location
                .as_ref()
                .map(|(_, at)| now.saturating_duration_since(*at).as_secs_f64()),
            playing,
            busy,
            blocked: self.blocked,
            problems,
        }
    }
}

fn log_once(logged: &mut Vec<io::ErrorKind>, error: &io::Error, what: &str) {
    if !logged.contains(&error.kind()) {
        logged.push(error.kind());
        log::warn!("{what}: {error}");
    }
}

/// The world for the menu: a singleplayer world by its name in the game (`level_name`) and
/// its folder when that differs (two worlds can have one name), a server by its address.
fn world_label(state: &WorldState, level_name: Option<&str>) -> WorldLabel {
    match state {
        WorldState::NotInWorld => WorldLabel::NotInWorld,
        WorldState::Unknown => WorldLabel::Unknown,
        WorldState::In(WorldId::Singleplayer { folder }) => {
            WorldLabel::Singleplayer(match level_name {
                Some(name) if name != folder => format!("{name}（フォルダ：{folder}）"),
                _ => folder.clone(),
            })
        }
        WorldState::In(world @ WorldId::Multiplayer { .. }) => {
            WorldLabel::Multiplayer(world.with_label(None).display_name())
        }
    }
}

/// The egui key the platforms report for a Minecraft key name: letters, digits, F-keys, and
/// keypad digits as the plain digits (as the GLFW and SDL3 hooks translate them).
fn egui_key(name: &str) -> Option<Key> {
    // Only the keys core knows the codes of.
    glfw_key(name)?;
    let key = name.strip_prefix("key.keyboard.")?;
    let key = key.strip_prefix("keypad.").unwrap_or(key);
    Key::from_name(&key.to_ascii_uppercase())
}

/// The debug keys as egui keys, which the waypoint and navigate hotkeys must not take.
fn reserved_keys(keys: &DebugKeys) -> Vec<Key> {
    let mut reserved = Vec::new();
    for name in [&keys.modifier, &keys.copy_location, &keys.crash] {
        if let Some(key) = egui_key(name)
            && !reserved.contains(&key)
        {
            reserved.push(key);
        }
    }
    reserved
}

/// The menu's warnings about Minecraft's F3+C bindings: one the agent cannot send, and other
/// mappings on the copy key, which a refused F3+C sets off once.
fn binding_problems(keys: &DebugKeys) -> Vec<String> {
    let mut problems = Vec::new();
    // GLFW's table has every key SDL3's has, and F25; on SDL3 a request reports F25 itself.
    if let Err(reason) = f3c::key_codes(keys, glfw_key) {
        problems.push(failure_text(&reason, keys));
    }
    if !keys.shared_with_copy.is_empty() {
        let names: String = keys
            .shared_with_copy
            .iter()
            .map(|id| format!("「{}」", mapping_label(id)))
            .collect();
        problems.push(format!(
            "{} には{names}も割り当てられている。座標を取得できないワールドでは、その操作が 1 回動くことがある",
            key_label(&keys.copy_location)
        ));
    }
    problems
}

/// `F3・C`: the bound debug keys, each once.
fn debug_key_labels(keys: &DebugKeys) -> String {
    let mut labels: Vec<String> = Vec::new();
    for name in [&keys.modifier, &keys.copy_location, &keys.crash] {
        let label = key_label(name);
        if name != UNBOUND && !labels.contains(&label) {
            labels.push(label);
        }
    }
    labels.join("・")
}

/// The notice for a failed request.
fn failure_text(reason: &Failure, keys: &DebugKeys) -> String {
    match reason {
        Failure::Refused => {
            let mut text =
                "座標を取得できなかった（このワールドではデバッグ情報が制限されている）".to_owned();
            if !keys.shared_with_copy.is_empty() {
                let names: Vec<String> = keys
                    .shared_with_copy
                    .iter()
                    .map(|id| mapping_label(id))
                    .collect();
                text.push_str(&format!(
                    "。{} の操作（{}）が 1 回動いた可能性がある",
                    key_label(&keys.copy_location),
                    names.join("、")
                ));
            }
            text
        }
        Failure::Unreadable => "座標を読み取れなかった".to_owned(),
        Failure::NotPlaying => "ゲームの画面を閉じてから押す".to_owned(),
        Failure::KeysHeld(Some((source, output))) => format!(
            "{}（{} に置き換え）を押している間は座標を取得できない",
            input_label(*source),
            input_label(*output)
        ),
        Failure::KeysHeld(None) if keys.copy_drops_items() => format!(
            "{} か Ctrl を押している間は座標を取得できない",
            debug_key_labels(keys)
        ),
        Failure::KeysHeld(None) => format!(
            "{} を押している間は座標を取得できない",
            debug_key_labels(keys)
        ),
        Failure::Unsupported(which, label) => format!(
            "F3+C の{}のキー（{label}）は送れない（Minecraft のキー設定で、英字・数字・F キー・テンキーの数字にする）",
            match which {
                DebugKey::Modifier => "デバッグ用の修飾キー",
                DebugKey::Copy => "座標のコピー",
            }
        ),
        Failure::Unbound if keys.copy_location == UNBOUND => {
            "座標のコピーにキーが割り当てられていない".to_owned()
        }
        Failure::Unbound => "デバッグ用の修飾キー（F3）にキーが割り当てられていない".to_owned(),
        Failure::NoCallback => "座標を取得できなかった（ゲームにキーを送れない）".to_owned(),
        Failure::NoHooks => {
            "この環境では座標を取得できない（座標を取るのに必要なフックが入らなかった。ログの F3+C の行を見る）"
                .to_owned()
        }
        Failure::Timeout => "座標を取得できなかった（時間切れ）".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use reminedog_core::gamedir::{latest_log_path, saves_dir};
    use reminedog_core::location::{OVERWORLD, THE_NETHER};
    use reminedog_core::options_path;
    use reminedog_core::waypoints_path;

    use super::*;

    const WINDOW: usize = 0x1000;
    const STARTING: &str =
        "[12:00:00] [Server thread/INFO]: Starting integrated minecraft server version 1.21.11\n";

    fn here(dimension: &str, x: f64, y: f64, z: f64) -> Location {
        Location {
            dimension: Some(dimension.to_owned()),
            x,
            y,
            z,
            yaw: 180.0,
            pitch: 0.0,
        }
    }

    fn located(location: Location, purpose: Option<Purpose>) -> Outcome {
        Outcome::Located { location, purpose }
    }

    fn failed(reason: Failure) -> Outcome {
        Outcome::Failed {
            purpose: Purpose::Record,
            reason,
        }
    }

    /// Writes the saves handed out so far and takes in how they went.
    fn settle(waypoints: &mut Waypoints) {
        waypoints.submit_saves();
        let saving = std::mem::take(&mut waypoints.saving);
        for (pending, job) in saving {
            let result = pending
                .wait(Duration::from_secs(10))
                .expect("saved in time");
            if let Some(job) = job {
                let result = result.map_err(|e| e.to_string());
                assert_eq!(waypoints.book.save_done(&job, result), None);
            }
        }
    }

    /// Polls the log as after a frame with one of the game's screens (they change worlds).
    fn poll_after_a_screen(waypoints: &mut Waypoints) {
        waypoints.in_game_since_poll = false;
        waypoints.poll_world();
    }

    /// The notices so far, taken.
    fn notices(waypoints: &mut Waypoints) -> Vec<String> {
        std::mem::take(&mut waypoints.pending_notices)
            .into_iter()
            .map(|notice| notice.text)
            .collect()
    }

    fn create_world(game: &Path, name: &str) {
        let world = saves_dir(game).join(name);
        fs::create_dir_all(&world).unwrap();
        fs::write(world.join("session.lock"), "").unwrap();
    }

    fn start_server(game: &Path) {
        let log = latest_log_path(game);
        fs::create_dir_all(log.parent().unwrap()).unwrap();
        fs::write(log, STARTING).unwrap();
    }

    /// Waypoints in the singleplayer world `name`, with one waypoint recorded and selected.
    fn with_destination(game: &Path, name: &str, now: Instant) -> Waypoints {
        create_world(game, name);
        start_server(game);
        let mut waypoints = Waypoints::new(game, now);
        poll_after_a_screen(&mut waypoints);
        let origin = here(OVERWORLD, 0.0, 64.0, 0.0);
        waypoints.on_outcome(located(origin, Some(Purpose::Record)), now);
        assert_eq!(
            notices(&mut waypoints),
            ["「地点 1」を記録した（0, 64, 0）"]
        );
        assert_eq!(
            waypoints.command(WaypointCommand::Select(Some(1)), WINDOW),
            None
        );
        assert_eq!(waypoints.selected, Some(1));
        waypoints
    }

    #[test]
    fn records_in_the_current_world() {
        let dir = tempfile::tempdir().unwrap();
        let game = dir.path();
        let now = Instant::now();
        let mut waypoints = Waypoints::new(game, now);
        assert_eq!(
            waypoints.view(true, false, now),
            WaypointView {
                playing: true,
                ..WaypointView::default()
            }
        );

        // No world yet: the position is known, but not where to record it.
        let spot = here(OVERWORLD, 12.3, 64.0, -7.2);
        waypoints.on_outcome(located(spot.clone(), Some(Purpose::Record)), now);
        assert_eq!(notices(&mut waypoints), [UNKNOWN_WORLD]);
        let view = waypoints.view(true, false, now);
        assert_eq!(view.location.as_ref(), Some(&spot));
        assert_eq!(view.location_age, Some(0.0));
        assert!(view.waypoints.is_empty());

        create_world(game, "New World");
        start_server(game);
        poll_after_a_screen(&mut waypoints);
        let view = waypoints.view(true, false, now);
        assert_eq!(view.world, WorldLabel::Singleplayer("New World".into()));
        // The old world's position went with it.
        assert_eq!(view.location, None);

        waypoints.on_outcome(located(spot.clone(), Some(Purpose::Record)), now);
        assert_eq!(
            notices(&mut waypoints),
            ["「地点 1」を記録した（12, 64, -7）"]
        );
        let view = waypoints.view(true, false, now + Duration::from_secs(3));
        assert_eq!(view.waypoints.len(), 1);
        assert_eq!(view.location, Some(spot));
        assert_eq!(view.location_age, Some(3.0));
        let world = WorldId::Singleplayer {
            folder: "New World".into(),
        };
        // Saved on the saving thread.
        settle(&mut waypoints);
        assert!(waypoints_path(game, &world).is_file());
        assert!(!waypoints.book.has_unsaved());
    }

    #[test]
    fn an_unknown_world_is_looked_up_again_when_recording() {
        let dir = tempfile::tempdir().unwrap();
        let game = dir.path();
        let now = Instant::now();
        let mut waypoints = Waypoints::new(game, now);
        // The server started, but its world folder is not there (yet).
        start_server(game);
        poll_after_a_screen(&mut waypoints);
        assert_eq!(waypoints.view(true, false, now).world, WorldLabel::Unknown);

        create_world(game, "Later");
        let spot = here(THE_NETHER, 1.0, 70.0, 2.0);
        waypoints.on_outcome(located(spot.clone(), Some(Purpose::Record)), now);
        assert_eq!(
            notices(&mut waypoints),
            ["「地点 1」を記録した（1, 70, 2）"]
        );
        let view = waypoints.view(true, false, now);
        assert_eq!(view.world, WorldLabel::Singleplayer("Later".into()));
        assert_eq!(view.location, Some(spot));
    }

    #[test]
    fn a_refusal_blocks_requests_until_the_retry() {
        let dir = tempfile::tempdir().unwrap();
        let now = Instant::now();
        let mut waypoints = Waypoints::new(dir.path(), now);
        // Read again right before each request.
        fs::write(
            options_path(dir.path()),
            "key_key.drop:key.keyboard.c
",
        )
        .unwrap();
        waypoints.load_keys(Naming::Modern);
        assert_eq!(waypoints.keys.shared_with_copy, ["key.drop"]);
        let job = Job {
            purpose: Purpose::Record,
            window: WINDOW,
            keys: waypoints.keys.clone(),
        };
        assert_eq!(waypoints.job(Purpose::Record, WINDOW), Some(job.clone()));

        waypoints.on_outcome(failed(Failure::Refused), now);
        assert_eq!(
            notices(&mut waypoints),
            [
                "座標を取得できなかった（このワールドではデバッグ情報が制限されている）。C の操作（アイテムを捨てる）が 1 回動いた可能性がある"
            ]
        );
        assert!(waypoints.view(true, false, now).blocked);
        // Neither the hotkey nor the menu's buttons send anything now.
        assert_eq!(waypoints.job(Purpose::Record, WINDOW), None);
        assert_eq!(waypoints.command(WaypointCommand::Record, WINDOW), None);
        assert_eq!(waypoints.command(WaypointCommand::Refresh, WINDOW), None);
        assert_eq!(notices(&mut waypoints), [BLOCKED, BLOCKED, BLOCKED]);

        // The menu's 「もう一度試す」.
        assert_eq!(waypoints.command(WaypointCommand::Unblock, WINDOW), None);
        assert!(!waypoints.view(true, false, now).blocked);
        assert_eq!(waypoints.job(Purpose::Record, WINDOW), Some(job));
        assert!(notices(&mut waypoints).is_empty());

        // Another world starts unblocked.
        waypoints.on_outcome(failed(Failure::Refused), now);
        assert!(waypoints.blocked);
        create_world(dir.path(), "Other");
        start_server(dir.path());
        poll_after_a_screen(&mut waypoints);
        assert!(!waypoints.blocked);

        // So does a world where the user's own F3+C worked.
        waypoints.on_outcome(failed(Failure::Refused), now);
        let spot = here(OVERWORLD, 0.0, 64.0, 0.0);
        waypoints.on_outcome(located(spot, None), now);
        assert!(!waypoints.blocked);
    }

    #[test]
    fn other_failures_do_not_block() {
        let dir = tempfile::tempdir().unwrap();
        let now = Instant::now();
        let mut waypoints = Waypoints::new(dir.path(), now);
        for reason in [
            Failure::Unreadable,
            Failure::NotPlaying,
            Failure::KeysHeld(None),
            Failure::Timeout,
            Failure::NoHooks,
        ] {
            waypoints.on_outcome(failed(reason), now);
        }
        assert!(!waypoints.blocked);
        assert_eq!(
            notices(&mut waypoints),
            [
                "座標を読み取れなかった",
                "ゲームの画面を閉じてから押す",
                "F3・C を押している間は座標を取得できない",
                "座標を取得できなかった（時間切れ）",
                "この環境では座標を取得できない（座標を取るのに必要なフックが入らなかった。ログの F3+C の行を見る）",
            ]
        );
        assert!(waypoints.job(Purpose::Refresh, WINDOW).is_some());
    }

    #[test]
    fn the_navigate_key_needs_a_destination() {
        let dir = tempfile::tempdir().unwrap();
        let now = Instant::now();
        let mut waypoints = Waypoints::new(dir.path(), now);
        assert_eq!(waypoints.navigate(WINDOW), None);
        assert_eq!(notices(&mut waypoints), [NO_DESTINATION]);
        // The menu's button refreshes the position all the same, without a notice for it.
        let job = waypoints.command(WaypointCommand::Refresh, WINDOW).unwrap();
        assert_eq!(job.purpose, Purpose::Refresh);
        let spot = here(OVERWORLD, 5.0, 64.0, 5.0);
        waypoints.on_outcome(located(spot.clone(), Some(Purpose::Refresh)), now);
        assert!(notices(&mut waypoints).is_empty());
        assert_eq!(waypoints.view(true, false, now).location, Some(spot));
    }

    #[test]
    fn a_refreshed_position_shows_the_way() {
        let dir = tempfile::tempdir().unwrap();
        let now = Instant::now();
        let mut waypoints = with_destination(dir.path(), "World", now);
        let job = waypoints.navigate(WINDOW).unwrap();
        assert_eq!(job.purpose, Purpose::Refresh);

        // Facing north, 100 blocks south of the waypoint.
        let spot = here(OVERWORLD, 0.0, 64.0, 100.0);
        waypoints.on_outcome(located(spot.clone(), Some(Purpose::Refresh)), now);
        let shown = std::mem::take(&mut waypoints.pending_notices);
        assert_eq!(
            shown,
            [Notice {
                arrow: Some(0.0),
                ..Notice::new("地点 1：北 100 m（正面）", false, NAVIGATION_SECONDS)
                    .replacing(Notice::NAVIGATION)
            }]
        );
        // The user's own F3+C too.
        waypoints.on_outcome(located(spot, None), now);
        assert_eq!(notices(&mut waypoints), ["地点 1：北 100 m（正面）"]);
    }

    #[test]
    fn selection_follows_the_waypoints() {
        let dir = tempfile::tempdir().unwrap();
        let now = Instant::now();
        let mut waypoints = with_destination(dir.path(), "World", now);
        // Unknown ids select nothing.
        waypoints.command(WaypointCommand::Select(Some(99)), WINDOW);
        assert_eq!(waypoints.selected, None);
        waypoints.command(WaypointCommand::Select(Some(1)), WINDOW);
        waypoints.command(WaypointCommand::Select(None), WINDOW);
        assert_eq!(waypoints.selected, None);

        waypoints.command(WaypointCommand::Select(Some(1)), WINDOW);
        waypoints.command(
            WaypointCommand::Rename {
                id: 1,
                name: "拠点".into(),
            },
            WINDOW,
        );
        assert_eq!(waypoints.view(true, false, now).waypoints[0].name, "拠点");
        assert_eq!(waypoints.selected, Some(1));
        waypoints.command(WaypointCommand::Delete(1), WINDOW);
        assert_eq!(waypoints.selected, None);
        assert!(waypoints.view(true, false, now).waypoints.is_empty());
        assert!(notices(&mut waypoints).is_empty());

        // Errors from the book become notices.
        waypoints.command(WaypointCommand::Delete(1), WINDOW);
        waypoints.command(
            WaypointCommand::Rename {
                id: 1,
                name: "x".into(),
            },
            WINDOW,
        );
        assert_eq!(
            notices(&mut waypoints),
            ["その地点は見つからない", "その地点は見つからない"]
        );
    }

    #[test]
    fn a_new_world_clears_the_selection() {
        let dir = tempfile::tempdir().unwrap();
        let now = Instant::now();
        let mut waypoints = with_destination(dir.path(), "World", now);
        let log = latest_log_path(dir.path());
        let stopped = format!("{STARTING}[12:30:00] [Server thread/INFO]: Stopping server\n");
        fs::write(log, stopped).unwrap();
        poll_after_a_screen(&mut waypoints);
        let view = waypoints.view(false, false, now);
        assert_eq!(view.world, WorldLabel::NotInWorld);
        assert_eq!((view.selected, view.location), (None, None));
        assert!(view.waypoints.is_empty());
    }

    #[test]
    fn key_bindings_from_options_txt() {
        let dir = tempfile::tempdir().unwrap();
        let now = Instant::now();
        let mut waypoints = Waypoints::new(dir.path(), now);
        // Missing: the defaults.
        waypoints.load_keys(Naming::Glfw);
        assert_eq!(waypoints.keys, DebugKeys::default());
        assert_eq!(waypoints.reserved_keys, [Key::F3, Key::C]);
        assert!(waypoints.key_problems.is_empty());
        assert!(waypoints.game_bindings.is_empty());

        fs::write(
            options_path(dir.path()),
            "key_key.debug.modifier:key.keyboard.f4\n\
             key_key.debug.copyLocation:key.keyboard.keypad.1\n\
             key_key.debug.crash:key.keyboard.unknown\n\
             key_key.drop:key.keyboard.keypad.1\n\
             key_key.attack:key.mouse.left\n\
             key_key.use:key.mouse.right\n",
        )
        .unwrap();
        waypoints.load_keys(Naming::Glfw);
        assert_eq!(waypoints.keys.modifier, "key.keyboard.f4");
        // Every key's mappings, for the menu (debug ones other than the modifier left out).
        let ids = |ids: &[&str]| ids.iter().map(|&id| id.to_owned()).collect::<Vec<_>>();
        assert_eq!(
            waypoints.game_bindings,
            [
                (InputId::Key(61), ids(&["key.debug.modifier"])),
                (InputId::Key(89), ids(&["key.drop"])),
                (InputId::Mouse(1), ids(&["key.attack"])),
                (InputId::Mouse(3), ids(&["key.use"])),
            ]
        );
        assert_eq!(waypoints.reserved_keys, [Key::F4, Key::Num1]);
        assert_eq!(
            waypoints.key_problems,
            [
                "テンキー 1 には「アイテムを捨てる」も割り当てられている。座標を取得できないワールドでは、その操作が 1 回動くことがある"
            ]
        );
        let job = waypoints.job(Purpose::Record, WINDOW).unwrap();
        assert_eq!(job.keys, waypoints.keys);

        fs::write(
            options_path(dir.path()),
            "key_key.debug.modifier:key.keyboard.left.shift\n",
        )
        .unwrap();
        waypoints.load_keys(Naming::Modern);
        assert_eq!(waypoints.reserved_keys, [Key::C]);
        assert_eq!(
            waypoints.game_bindings,
            [(InputId::Key(225), ids(&["key.debug.modifier"]))]
        );
        assert_eq!(
            waypoints.key_problems,
            [format!(
                "F3+C のデバッグ用の修飾キーのキー（{}）は送れない（Minecraft のキー設定で、英字・数字・F キー・テンキーの数字にする）",
                key_label("key.keyboard.left.shift")
            )]
        );
        // The menu shows them.
        let view = waypoints.view(true, false, now);
        assert_eq!(view.problems, waypoints.key_problems);
    }

    #[test]
    fn failure_texts() {
        let keys = |modifier: &str, copy: &str, crash: &str| DebugKeys {
            modifier: modifier.into(),
            copy_location: copy.into(),
            crash: crash.into(),
            shared_with_copy: Vec::new(),
        };
        let f4 = "key.keyboard.f4";
        let x = "key.keyboard.x";
        let y = "key.keyboard.y";
        assert_eq!(
            failure_text(&Failure::KeysHeld(None), &keys(f4, x, y)),
            "F4・X・Y を押している間は座標を取得できない"
        );
        assert_eq!(
            failure_text(&Failure::KeysHeld(None), &keys(f4, x, UNBOUND)),
            "F4・X を押している間は座標を取得できない"
        );
        // Ctrl matters only where the copy key drops items.
        let mut drops = keys(f4, x, y);
        drops.shared_with_copy = vec!["key.drop".into()];
        assert_eq!(
            failure_text(&Failure::KeysHeld(None), &drops),
            "F4・X・Y か Ctrl を押している間は座標を取得できない"
        );
        // Held by a rebinding rule: the key to let go of.
        let mouse4_f3 = Some((InputId::Mouse(4), InputId::Key(60)));
        assert_eq!(
            failure_text(&Failure::KeysHeld(mouse4_f3), &drops),
            "マウスのボタン4（F3 に置き換え）を押している間は座標を取得できない"
        );
        assert_eq!(
            failure_text(&Failure::Refused, &DebugKeys::default()),
            "座標を取得できなかった（このワールドではデバッグ情報が制限されている）"
        );
        let mut shared = keys(f4, x, y);
        shared.shared_with_copy = vec!["key.drop".into(), "key.hotbar.2".into()];
        assert_eq!(
            failure_text(&Failure::Refused, &shared),
            "座標を取得できなかった（このワールドではデバッグ情報が制限されている）。X の操作（アイテムを捨てる、ホットバースロット2）が 1 回動いた可能性がある"
        );
        assert_eq!(
            binding_problems(&shared),
            [
                "X には「アイテムを捨てる」「ホットバースロット2」も割り当てられている。座標を取得できないワールドでは、その操作が 1 回動くことがある"
            ]
        );
        assert_eq!(
            failure_text(&Failure::Unbound, &keys(f4, UNBOUND, y)),
            "座標のコピーにキーが割り当てられていない"
        );
        assert_eq!(
            failure_text(&Failure::Unbound, &keys(UNBOUND, x, y)),
            "デバッグ用の修飾キー（F3）にキーが割り当てられていない"
        );
        assert_eq!(
            binding_problems(&keys(f4, UNBOUND, y)),
            ["座標のコピーにキーが割り当てられていない"]
        );
        assert_eq!(
            failure_text(
                &Failure::Unsupported(DebugKey::Copy, "マウスの戻る".into()),
                &keys(f4, x, y)
            ),
            "F3+C の座標のコピーのキー（マウスの戻る）は送れない（Minecraft のキー設定で、英字・数字・F キー・テンキーの数字にする）"
        );
        assert_eq!(
            failure_text(&Failure::NoCallback, &keys(f4, x, y)),
            "座標を取得できなかった（ゲームにキーを送れない）"
        );
    }

    #[test]
    fn egui_keys_of_key_names() {
        let key = |name: &str| egui_key(&format!("key.keyboard.{name}"));
        assert_eq!(key("a"), Some(Key::A));
        assert_eq!(key("z"), Some(Key::Z));
        assert_eq!(key("0"), Some(Key::Num0));
        assert_eq!(key("9"), Some(Key::Num9));
        assert_eq!(key("f1"), Some(Key::F1));
        assert_eq!(key("f25"), Some(Key::F25));
        assert_eq!(key("keypad.0"), Some(Key::Num0));
        assert_eq!(key("keypad.9"), Some(Key::Num9));
        assert_eq!(key("f26"), None);
        assert_eq!(key("left.shift"), None);
        assert_eq!(key("unknown"), None);
        assert_eq!(egui_key("key.mouse.left"), None);
        assert_eq!(egui_key("a"), None);
    }

    #[test]
    fn world_labels() {
        assert_eq!(
            world_label(&WorldState::NotInWorld, None),
            WorldLabel::NotInWorld
        );
        assert_eq!(world_label(&WorldState::Unknown, None), WorldLabel::Unknown);
        let server = WorldId::Multiplayer {
            host: "example.com".into(),
            port: 25565,
            label: Some("survival".into()),
        };
        // The label shows in its own row.
        assert_eq!(
            world_label(&WorldState::In(server), None),
            WorldLabel::Multiplayer("example.com".into())
        );
        let world = WorldState::In(WorldId::Singleplayer {
            folder: "New World (1)".into(),
        });
        assert_eq!(
            world_label(&world, Some("New World")),
            WorldLabel::Singleplayer("New World（フォルダ：New World (1)）".into())
        );
        assert_eq!(
            world_label(&world, Some("New World (1)")),
            WorldLabel::Singleplayer("New World (1)".into())
        );
        assert_eq!(
            world_label(&world, None),
            WorldLabel::Singleplayer("New World (1)".into())
        );
    }

    #[test]
    fn a_server_label_keeps_its_own_waypoints() {
        let dir = tempfile::tempdir().unwrap();
        let game = dir.path();
        let now = Instant::now();
        let log = latest_log_path(game);
        fs::create_dir_all(log.parent().unwrap()).unwrap();
        fs::write(
            &log,
            "[12:00:00] [Render thread/INFO]: Connecting to example.com, 25565\n",
        )
        .unwrap();
        let mut waypoints = Waypoints::new(game, now);
        poll_after_a_screen(&mut waypoints);
        let spot = here(OVERWORLD, 1.0, 64.0, 1.0);
        waypoints.on_outcome(located(spot.clone(), Some(Purpose::Record)), now);
        assert_eq!(waypoints.view(true, false, now).waypoints.len(), 1);
        assert_eq!(waypoints.view(true, false, now).label, None);

        waypoints.command(WaypointCommand::SetLabel(Some("survival".into())), WINDOW);
        let view = waypoints.view(true, false, now);
        assert_eq!(view.label.as_deref(), Some("survival"));
        assert!(view.waypoints.is_empty());
        settle(&mut waypoints);

        // The next visit opens the label's waypoints.
        let mut again = Waypoints::new(game, now);
        poll_after_a_screen(&mut again);
        assert_eq!(
            again.view(true, false, now).label.as_deref(),
            Some("survival")
        );
        again.command(WaypointCommand::SetLabel(None), WINDOW);
        assert_eq!(again.view(true, false, now).waypoints.len(), 1);
    }

    #[test]
    fn world_lines_read_during_play_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let game = dir.path();
        let now = Instant::now();
        let mut waypoints = with_destination(game, "World", now);
        // A frame in game, then a poll: a server's text put a world line in the log.
        let log = latest_log_path(game);
        let mut text = fs::read_to_string(&log).unwrap();
        text.push_str("[12:30:00] [Server thread/INFO]: Stopping server\n");
        fs::write(&log, text).unwrap();
        waypoints.in_game_since_poll = true;
        Waypoints::poll_world(&mut waypoints);
        assert_eq!(waypoints.selected, Some(1));
        assert!(matches!(
            waypoints.view(true, false, now).world,
            WorldLabel::Singleplayer(_)
        ));
    }
}
