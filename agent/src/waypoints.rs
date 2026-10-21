//! スワップの道の上のウェイポイント：プレイヤーがいるワールド（latest.log と saves のフォルダ）、
//! Minecraft のキーの割り当て（options.txt：デバッグのキーと、メニューのキーの置き換えのための
//! 各キーの割り当て）、F3+C の要求になるホットキーとメニューの命令（[`f3c`]）、ウェイポイントと
//! 通知になって返ってくる位置と失敗。
//!
//! 状態は static なので、オーバーレイの `Runtime`（ゲームのデバイスコンテキストが変わると
//! 作り直す）より長く生きる。使うのはフレームの処理だけで、スワップのフックの中。これをロック
//! している間に router と f3c の状態をロックしてよいが、逆は不可で、ゲームのコードも動かさない。

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

/// ワールドの変化のために latest.log を読む間隔。
const POLL_INTERVAL: Duration = Duration::from_secs(1);
/// キーの割り当ての変化のために options.txt を見る間隔（要求の直前にも見る）。
const KEYS_INTERVAL: Duration = Duration::from_secs(5);
/// これより大きい options.txt は読まない。
const MAX_OPTIONS_LEN: u64 = 1024 * 1024;
/// 前の保存が書けなかったウェイポイントを、もう一度保存する間隔。
const RETRY_INTERVAL: Duration = Duration::from_secs(10);
/// OS がこの時間のうちに受け取らなかった要求は失敗する。
const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
/// 通知を出す秒数。
const NOTICE_SECONDS: f64 = 6.0;
const NAVIGATION_SECONDS: f64 = 8.0;

const BLOCKED: &str = "このワールドでは座標を取得できない（メニューの「もう一度試す」で再開）";
const NO_DESTINATION: &str = "目的地が選ばれていない（メニューで選ぶ）";
const UNKNOWN_WORLD: &str =
    "ワールドを判定できていないので記録できない（ワールドに入り直すと判定できる）";

static WAYPOINTS: Mutex<Option<Waypoints>> = Mutex::new(None);

/// このフレームでオーバーレイがウェイポイントから受け取るもの。
pub struct Frame {
    pub view: WaypointView,
    pub notices: Vec<Notice>,
    /// Minecraft のデバッグの修飾キー、座標のコピーのキー、クラッシュのキー。
    pub reserved_keys: Vec<Key>,
}

/// オーバーレイのウィンドウの毎フレーム、オーバーレイが描く前に：ワールドとキーの割り当てを
/// 追い、F3+C の結果をウェイポイントと通知に、前のフレームから押されたホットキーを要求にする。
/// `playing`：ゲームがカーソルを捕まえている。`naming`：フックしたウィンドウのライブラリの
/// Minecraft が options.txt でキーをどう名付けるか。
pub fn before_frame(game_dir: &Path, window: usize, playing: bool, naming: Naming) -> Frame {
    let mut state = lock();
    let now = Instant::now();
    let waypoints = state.get_or_insert_with(|| Waypoints::new(game_dir, now));
    waypoints.naming = naming;
    f3c::hooks_ready();
    if !playing {
        waypoints.in_game_since_poll = false;
    }
    if now >= waypoints.next_poll {
        waypoints.next_poll = now + POLL_INTERVAL;
        waypoints.poll_world();
    }
    waypoints.poll_saves();
    if !waypoints.book.has_unsaved() {
        // 最初の試し直しは、次のフレームではなく、失敗から 1 間隔後に来る。
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

/// 最後に読んだときの options.txt の、各キーのゲームの割り当て（メニューのキーの置き換えのため）。
pub fn game_bindings() -> Vec<(InputId, Vec<String>)> {
    lock()
        .as_ref()
        .map(|waypoints| waypoints.game_bindings.clone())
        .unwrap_or_default()
}

/// いま描いたフレームのメニューのウェイポイントの命令。その通知は次のフレームで出る。
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

/// ファイルの更新時刻と大きさ（`None`：ファイルがなかった）。
type FileStamp = Option<(Option<SystemTime>, u64)>;

struct Waypoints {
    game_dir: PathBuf,
    watcher: WorldWatcher,
    book: WaypointBook,
    /// 各サーバーで最後に使ったラベル。
    labels: ServerLabels,
    /// 保存のスレッドでの保存：ウェイポイントのものと、ラベルのもの（`None`）。
    saving: Vec<(saver::Pending, Option<SaveJob>)>,
    /// メニューが見せるウェイポイント。記録の版が変わったら作り直す。
    shown: (u64, Arc<[Waypoint]>),
    next_poll: Instant,
    next_keys: Instant,
    next_retry: Instant,
    /// 前にログを読んでから、毎フレームゲームがカーソルを持っていた。
    in_game_since_poll: bool,
    /// ウィンドウのライブラリの Minecraft がキーをどう名付けるか。
    naming: Naming,
    keys: DebugKeys,
    /// options.txt を少なくとも 1 回読んだ。
    keys_read: bool,
    /// 読んだときの options.txt と、読んだときの名付け方。
    keys_stamp: Option<(FileStamp, Naming)>,
    /// `keys` についてのメニューの警告。
    key_problems: Vec<String>,
    reserved_keys: Vec<Key>,
    /// options.txt の各キーの割り当て（`bindings_by_key`）。
    game_bindings: Vec<(InputId, Vec<String>)>,
    /// 最後に分かったプレイヤーの位置と、その時刻。
    location: Option<(Location, Instant)>,
    /// 目的地。
    selected: Option<u64>,
    /// このワールドではゲームが F3+C を断った。メニューの「もう一度試す」か別のワールドまで、
    /// 何も送らない。
    blocked: bool,
    pending_notices: Vec<Notice>,
    /// もうログに出したエラーの種類（続くエラーを 1 回だけ出すため）。
    log_errors: Vec<io::ErrorKind>,
    keys_errors: Vec<io::ErrorKind>,
}

impl Waypoints {
    /// まだ何も読まない：最初のフレームがログを読み、options.txt を読む。
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

    /// ワールドの変化のためにログを読む。前に読んでからずっとゲームがカーソルを持っていた後に
    /// 読んだワールドの行は、ゲームの画面から来たものではない：無視する。
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

    /// 新しいワールドのウェイポイントを開く（サーバーなら最後のラベルの下で）。古いワールドの
    /// ものはなくなる。
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

    /// 記録の保存を保存のスレッドに渡す。
    fn submit_saves(&mut self) {
        for mut job in self.book.take_saves() {
            let bytes = std::mem::take(&mut job.bytes);
            self.saving
                .push((saver::write(job.path.clone(), bytes), Some(job)));
        }
    }

    /// 前のフレームから終わった保存を受け取る。
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

    /// options.txt（か `naming`）が前から変わっていれば、`naming` のキーの名前でキーの割り当てを
    /// 読む。エラーなら前に読んだものが残る。ファイルがない（新しいインスタンス）なら既定のもの。
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
        // 利用者自身の F3+C：OS がゲームに報告するコピーのキー。
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

    /// F3+C から得た位置。利用者自身の F3+C なら `purpose` は `None`。
    fn located(&mut self, location: Location, purpose: Option<Purpose>, now: Instant) {
        // 位置は、前に読んでから入ったワールドのもの（あれば）。
        self.poll_world();
        if purpose == Some(Purpose::Record) && self.book.world().is_none() && self.watcher.refresh()
        {
            // サーバーが始まったときにワールドのフォルダが見つからなかった。今はあるかもしれない。
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

    /// 目的地を選んでいれば、`from` から目的地への道。古い位置から見せていた道を置き換える。
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

    /// 案内のホットキー：目的地への道のために位置を更新する。目的地がなければ何も送らない
    /// （F3+C はチャットに行を出すだけになる）。
    fn navigate(&mut self, window: usize) -> Option<Job> {
        if self.destination().is_none() {
            self.notice(NO_DESTINATION, false, NOTICE_SECONDS);
            return None;
        }
        self.job(Purpose::Refresh, window)
    }

    /// 送る要求（このワールドでゲームが F3+C を断っていなければ）。options.txt が変わっていれば、
    /// 先にキーの割り当てを読み直す（利用者がいま変えたかもしれない）。
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

    /// メニューの命令を行う。`Record` と `Refresh` は送る要求を返す。
    fn command(&mut self, command: WaypointCommand, window: usize) -> Option<Job> {
        match command {
            WaypointCommand::Record => return self.job(Purpose::Record, window),
            // メニューのボタンは、目的地がなくても位置を更新する。
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
                // ファイルを保存できなくても、記憶からは消える。
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

    /// サーバーで：これからは（次に来たときも）そのウェイポイントを `label` の下に置く。
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

/// メニューのためのワールド：シングルプレイのワールドはゲームの中の名前（`level_name`）と、
/// それと違えばフォルダ（2 つのワールドが同じ名前を持てる）、サーバーはアドレス。
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

/// Minecraft のキーの名前に対して OS が報告する egui のキー：文字、数字、F キー、テンキーの
/// 数字はふつうの数字として（GLFW と SDL3 のフックがそう変換する）。
fn egui_key(name: &str) -> Option<Key> {
    // core がコードを知っているキーだけ。
    glfw_key(name)?;
    let key = name.strip_prefix("key.keyboard.")?;
    let key = key.strip_prefix("keypad.").unwrap_or(key);
    Key::from_name(&key.to_ascii_uppercase())
}

/// egui のキーとしてのデバッグのキー。ウェイポイントと案内のホットキーはこれを取ってはいけない。
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

/// Minecraft の F3+C の割り当てについてのメニューの警告：エージェントが送れないキーと、
/// コピーのキーのほかの割り当て（断られた F3+C が 1 回動かしてしまう）。
fn binding_problems(keys: &DebugKeys) -> Vec<String> {
    let mut problems = Vec::new();
    // GLFW の表には SDL3 の表のキーが全部と F25 がある。SDL3 では、要求が F25 を自分で報告する。
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

/// `F3・C`：割り当てたデバッグのキー（それぞれ 1 回）。
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

/// 失敗した要求の通知。
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

    /// ここまでに渡した保存を書き、その結果を受け取る。
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

    /// ゲームの画面があるフレームの後と同じように、ログを読む（画面はワールドを変える）。
    fn poll_after_a_screen(waypoints: &mut Waypoints) {
        waypoints.in_game_since_poll = false;
        waypoints.poll_world();
    }

    /// ここまでの通知を取り出す。
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

    /// シングルプレイのワールド `name` のウェイポイント。1 つ記録して選んである。
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

        // まだワールドがない：位置は分かるが、どこに記録するかが分からない。
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
        // 古いワールドの位置は、ワールドと一緒になくなった。
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
        // 保存のスレッドで保存した。
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
        // サーバーは始まったが、そのワールドのフォルダが（まだ）ない。
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
        // 要求のたびに、その直前に読み直す。
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
        // ホットキーもメニューのボタンも、いまは何も送らない。
        assert_eq!(waypoints.job(Purpose::Record, WINDOW), None);
        assert_eq!(waypoints.command(WaypointCommand::Record, WINDOW), None);
        assert_eq!(waypoints.command(WaypointCommand::Refresh, WINDOW), None);
        assert_eq!(notices(&mut waypoints), [BLOCKED, BLOCKED, BLOCKED]);

        // メニューの「もう一度試す」。
        assert_eq!(waypoints.command(WaypointCommand::Unblock, WINDOW), None);
        assert!(!waypoints.view(true, false, now).blocked);
        assert_eq!(waypoints.job(Purpose::Record, WINDOW), Some(job));
        assert!(notices(&mut waypoints).is_empty());

        // 別のワールドは止められていない状態で始まる。
        waypoints.on_outcome(failed(Failure::Refused), now);
        assert!(waypoints.blocked);
        create_world(dir.path(), "Other");
        start_server(dir.path());
        poll_after_a_screen(&mut waypoints);
        assert!(!waypoints.blocked);

        // 利用者自身の F3+C がうまくいったワールドも同じ。
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
        // メニューのボタンはそれでも位置を更新する。そのための通知は出さない。
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

        // 北を向き、ウェイポイントの 100 ブロック南。
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
        // 利用者自身の F3+C も。
        waypoints.on_outcome(located(spot, None), now);
        assert_eq!(notices(&mut waypoints), ["地点 1：北 100 m（正面）"]);
    }

    #[test]
    fn selection_follows_the_waypoints() {
        let dir = tempfile::tempdir().unwrap();
        let now = Instant::now();
        let mut waypoints = with_destination(dir.path(), "World", now);
        // 知らない id は何も選ばない。
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

        // 記録のエラーは通知になる。
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
        // ない：既定のもの。
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
        // 各キーの割り当て（メニューのため。修飾キー以外のデバッグのものは除く）。
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
        // メニューがそれを見せる。
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
        // Ctrl が関係するのは、コピーのキーがアイテムを捨てるときだけ。
        let mut drops = keys(f4, x, y);
        drops.shared_with_copy = vec!["key.drop".into()];
        assert_eq!(
            failure_text(&Failure::KeysHeld(None), &drops),
            "F4・X・Y か Ctrl を押している間は座標を取得できない"
        );
        // 置き換えのルールが押さえている：離すべきキー。
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
        // ラベルは自分の行に出る。
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

        // 次に来たときは、ラベルのウェイポイントを開く。
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
        // ゲーム中のフレームの後にログを読む：サーバーの文字がワールドの行をログに入れた。
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
