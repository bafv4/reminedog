//! F3+C：エージェントがゲームのコードを使わずに、プレイヤーの位置を知る方法。各 OS のフックが
//! ゲームにデバッグの修飾キーと座標のコピーのキーのイベントを送り（options.txt の割り当てで）、
//! そのとき Minecraft がクリップボードに書く座標を捕まえる。このモジュールは OS に依存しない
//! 半分：フレームからの要求、クリップボードへの書き込みが自分のもの（握りつぶす）か利用者自身の
//! F3+C（そのまま通す）かの窓、フレームが通知にする結果。
//!
//! 要求は、各 OS の次の機会（GLFW：そのウィンドウの次のスワップの後。SDL3：次の
//! `SDL_PollEvent`）に受け取られ、その場で、そのスワップかそのポーリングのループの中で終わる
//! （ゲームはキーのイベントを 1 つずつその場で処理するため）。次のフレームでまだ途中の仕事は
//! 放り出された（パニック、通らなかった道）もので、[`expire`] が失敗にする。
//!
//! ロックの順序：`router -> f3c` と `INJECTED -> f3c` は許す。f3c のロックを持つ間は何も
//! ロックせず、ゲームのコードも動かさない。`INJECTED` をロックする前に [`take_job`] を呼ぶこと。

use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use reminedog_core::{
    DebugKeys, InputId, Location, Naming, UNBOUND, input_by_name, key_label, parse_f3c,
};

/// 見るクリップボードの文字の最大の長さ。F3+C の書き込みは 200 バイトに十分収まる。
const MAX_TEXT: usize = 512;
/// フレームのために覚えておく結果の数（毎フレーム取り出すので、これは止まったときの上限だけ）。
const MAX_OUTCOMES: usize = 16;
/// 下の atomic の「受け身のキーなし」。どちらも本物のキーのコードではない。
const NO_GLFW_KEY: i32 = i32::MIN;
const NO_SDL_KEY: u32 = u32::MAX;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Purpose {
    /// その位置にウェイポイントを記録する。
    Record,
    /// 分かっている位置を更新する（選んだウェイポイントへの道も示す）。
    Refresh,
}

impl Purpose {
    fn name(self) -> &'static str {
        match self {
            Purpose::Record => "record",
            Purpose::Refresh => "refresh",
        }
    }
}

/// F3+C の要求。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Job {
    pub purpose: Purpose,
    /// キーのイベントをゲームに渡すウィンドウ（`GLFWwindow*` か `SDL_Window*`）。
    pub window: usize,
    /// options.txt から読んだゲームの割り当て。
    pub keys: DebugKeys,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    /// F3+C から得た位置。利用者自身の F3+C なら `purpose` は `None`。
    Located {
        location: Location,
        purpose: Option<Purpose>,
    },
    Failed {
        purpose: Purpose,
        reason: Failure,
    },
}

/// 要求で位置が得られなかった理由。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Failure {
    /// ゲームが何もコピーしなかった：プレイヤーがいないか、ワールドがデバッグ情報を制限している。
    Refused,
    /// ゲームが座標でない文字をコピーした（それでも握りつぶした）。
    Unreadable,
    /// ゲームがカーソルを持っていなかった（Minecraft の画面が開いていた）。
    NotPlaying,
    /// 修飾キー、コピーのキー、クラッシュのキー、Ctrl のどれかを、ゲームから見て押していた：
    /// 物理的に、または置き換えのルールの出力として（元のキー、出力）、元のキーが押さえていた。
    KeysHeld(Option<(InputId, InputId)>),
    /// デバッグの修飾キーか座標のコピーの割り当てが、エージェントが送れないキーになっている
    /// （キーのラベル）。
    Unsupported(DebugKey, String),
    /// 修飾キーか座標のコピーの割り当てにキーがない。
    Unbound,
    /// ゲームに、キーを送るキーのコールバック（GLFW）かウィンドウの番号（SDL3）がなかった。
    NoCallback,
    /// クリップボードか入力のフックが入っていない。
    NoHooks,
    /// 時間内に答えがなかった。
    Timeout,
}

/// F3+C が送るデバッグの割り当て。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DebugKey {
    Modifier,
    Copy,
}

/// キーのイベントをゲームに渡している間に、クリップボードで見えたこと。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Injected {
    /// 座標：仕事は終わった。
    Captured,
    /// 読めなかった書き込み（握りつぶした）：ゲームはキーを処理した。
    Written,
    /// 書き込みなし：ゲームが断った。
    Nothing,
}

/// F3+C が送るキーのゲームのコード（GLFW のキーコード、または SDL のスキャンコードとキーコード）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyCodes<T> {
    pub modifier: T,
    pub copy: T,
}

/// デバッグの修飾キーとコピーのキーを `code` で引く。割り当てのないものがあれば F3+C は
/// できない。`code` が知らないキーは送れない。
pub fn key_codes<T>(
    keys: &DebugKeys,
    code: impl Fn(&str) -> Option<T>,
) -> Result<KeyCodes<T>, Failure> {
    let map = |name: &str, which| {
        if name == UNBOUND {
            return Err(Failure::Unbound);
        }
        code(name).ok_or_else(|| Failure::Unsupported(which, key_label(name)))
    };
    Ok(KeyCodes {
        modifier: map(&keys.modifier, DebugKey::Modifier)?,
        copy: map(&keys.copy_location, DebugKey::Copy)?,
    })
}

/// 確かめるキーボードのキーとしてのクラッシュのキー（送ることはないので、どのキーでもよい）：
/// 割り当てがない（それなら働かない）かマウスのボタン（確かめない：フックはキーボードの状態
/// だけを読む）なら `None`。
pub fn crash_key(keys: &DebugKeys, naming: Naming) -> Option<InputId> {
    input_by_name(&keys.crash, naming).filter(|id| matches!(id, InputId::Key(_)))
}

/// 仕事のキーのイベントがゲームにある間に見えたクリップボード。
#[derive(Debug)]
struct Window {
    purpose: Purpose,
    written: bool,
    captured: bool,
}

#[derive(Debug)]
struct State {
    /// どの OS もまだ受け取っていない要求と、その時刻。
    pending: Option<(Job, Instant)>,
    /// 受け取られたが、まだ結果のない仕事。
    in_flight: Option<Purpose>,
    injected: Option<Window>,
    /// 利用者自身のコピーのキーがゲームに行った。
    passive: bool,
    outcomes: Vec<Outcome>,
}

impl State {
    const fn new() -> Self {
        Self {
            pending: None,
            in_flight: None,
            injected: None,
            passive: false,
            outcomes: Vec::new(),
        }
    }

    fn busy(&self) -> bool {
        self.pending.is_some() || self.in_flight.is_some()
    }

    fn watching(&self) -> bool {
        self.injected.is_some() || self.passive
    }

    fn push(&mut self, outcome: Outcome) {
        if self.outcomes.len() >= MAX_OUTCOMES {
            self.outcomes.remove(0);
        }
        self.outcomes.push(outcome);
    }

    /// ほかの仕事が待っているか途中の間は false（何も変えない）。
    fn request(&mut self, job: Job, now: Instant) -> bool {
        if self.busy() {
            return false;
        }
        self.pending = Some((job, now));
        true
    }

    fn take_job(&mut self, window: Option<usize>) -> Option<Job> {
        let (job, _) = self
            .pending
            .take_if(|(job, _)| window.is_none_or(|w| w == job.window))?;
        self.in_flight = Some(job.purpose);
        Some(job)
    }

    fn begin_injected(&mut self, purpose: Purpose) {
        self.injected = Some(Window {
            purpose,
            written: false,
            captured: false,
        });
    }

    fn end_injected(&mut self) -> Injected {
        let copied = match self.injected.take() {
            Some(Window { captured: true, .. }) => Injected::Captured,
            Some(Window { written: true, .. }) => Injected::Written,
            _ => Injected::Nothing,
        };
        if copied == Injected::Captured {
            self.in_flight = None;
        }
        copied
    }

    fn fail(&mut self, purpose: Purpose, reason: Failure) {
        self.injected = None;
        // もう `expire` が失敗にした：通知は 1 つでよい。
        if self.in_flight.take().is_none() {
            log::debug!(
                "F3+C: {} already settled; {reason:?} dropped",
                purpose.name()
            );
            return;
        }
        match reason {
            Failure::Refused => log::info!("F3+C: refused ({})", purpose.name()),
            _ => log::info!("F3+C: {} failed: {reason:?}", purpose.name()),
        }
        self.push(Outcome::Failed { purpose, reason });
    }

    fn on_clipboard(&mut self, text: &str) -> bool {
        if let Some(window) = &mut self.injected {
            // 自分のキーのイベントが起こした書き込み：クリップボードには届かない。
            window.written = true;
            if window.captured {
                return true;
            }
            if text.len() > MAX_TEXT {
                log::warn!(
                    "F3+C: the game copied {} bytes, too long for a location",
                    text.len()
                );
                return true;
            }
            let purpose = window.purpose;
            match parse_f3c(text) {
                Ok(location) => {
                    window.captured = true;
                    log::info!("F3+C: location captured ({})", purpose.name());
                    self.push(Outcome::Located {
                        location,
                        purpose: Some(purpose),
                    });
                }
                Err(e) => log::warn!("F3+C: the game copied something else: {e}"),
            }
            return true;
        }
        if self.passive
            && text.len() <= MAX_TEXT
            && let Ok(location) = parse_f3c(text)
        {
            log::info!("F3+C: location captured (manual)");
            self.push(Outcome::Located {
                location,
                purpose: None,
            });
        }
        false
    }

    fn expire(&mut self, now: Instant, timeout: Duration) {
        if let Some((job, _)) = self
            .pending
            .take_if(|(_, at)| now.saturating_duration_since(*at) >= timeout)
        {
            log::warn!(
                "F3+C: the {} request was not taken in time",
                job.purpose.name()
            );
            self.push(Outcome::Failed {
                purpose: job.purpose,
                reason: Failure::Timeout,
            });
        }
        if let Some(purpose) = self.in_flight.take() {
            log::warn!("F3+C: the {} request did not finish", purpose.name());
            self.push(Outcome::Failed {
                purpose,
                reason: Failure::Timeout,
            });
        }
        if self.watching() {
            log::debug!("F3+C: closing a clipboard window left open");
            self.injected = None;
            self.passive = false;
        }
    }
}

static STATE: Mutex<State> = Mutex::new(State::new());
/// F3+C が要るウィンドウのライブラリのフックが入っているか（[`set_hooks`]）。
static HOOKS: OnceLock<fn() -> bool> = OnceLock::new();
/// フックが入ったのを 1 回見た（そのときにログに出した）。
static HOOKS_SEEN: AtomicBool = AtomicBool::new(false);
/// `State::watching` の写し。クリップボードへの書き込みのほとんどは、ロックを取らずに済む。
static WATCHING: AtomicBool = AtomicBool::new(false);
/// `State.pending.is_some()` の写し。各 OS が、スワップやポーリングのたびに確かめるため。
static PENDING: AtomicBool = AtomicBool::new(false);
static PASSIVE_GLFW: AtomicI32 = AtomicI32::new(NO_GLFW_KEY);
static PASSIVE_SDL: AtomicU32 = AtomicU32::new(NO_SDL_KEY);

/// 状態について `f` を動かし、ロックなしで読むフラグを更新する。
fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> R {
    let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
    let result = f(&mut state);
    WATCHING.store(state.watching(), Ordering::Release);
    PENDING.store(state.pending.is_some(), Ordering::Release);
    result
}

/// ウィンドウのライブラリの入力が、フックしたときに入れる：F3+C が要るフックが入っているかを
/// 答える（LWJGL は、ライブラリをフックした後、関数を引くたびにフックを受け取る）。
pub fn set_hooks(ready: fn() -> bool) {
    let _ = HOOKS.set(ready);
}

/// F3+C を送れるか。初めて送れるようになったときにログに出す（フレームがスワップのたびに
/// 聞くので、ログでフックが入った時が分かる）。
pub fn hooks_ready() -> bool {
    let ready = HOOKS.get().is_some_and(|ready| ready());
    if ready && !HOOKS_SEEN.swap(true, Ordering::Relaxed) {
        log::info!("F3+C: clipboard hooks ready");
    }
    ready
}

/// OS に F3+C を送るよう頼む。ほかの仕事が待っているか途中の間は何もしない（その間、UI は
/// ボタンを使えなくする）。
pub fn request(job: Job) -> Result<(), Failure> {
    if !hooks_ready() {
        log::warn!("F3+C: hooks missing; no waypoints");
        return Err(Failure::NoHooks);
    }
    let purpose = job.purpose;
    if with_state(|s| s.request(job, Instant::now())) {
        log::info!("F3+C: requested ({})", purpose.name());
    } else {
        log::debug!("F3+C: busy; {} request ignored", purpose.name());
    }
    Ok(())
}

pub fn busy() -> bool {
    with_state(|s| s.busy())
}

/// 待っている仕事（OS がいま送るため）。GLFW はいまスワップしたウィンドウを渡す（仕事は自分の
/// ウィンドウにだけ行く）。SDL3 は `None` を渡す。
pub fn take_job(window: Option<usize>) -> Option<Job> {
    if !PENDING.load(Ordering::Acquire) {
        return None;
    }
    with_state(|s| s.take_job(window))
}

/// コピーのキーのイベントがゲームに届くところ：[`end_injected`] までのクリップボードへの
/// 書き込みは自分のもの。
pub fn begin_injected(purpose: Purpose) {
    with_state(|s| s.begin_injected(purpose));
}

/// 注入の窓を閉じる：Captured（座標を読めた）、Written（何か書かれたが読めなかった）、
/// Nothing（書き込みなし：ゲームが断った）。Captured なら仕事は終わり。それ以外は、OS が
/// [`fail`] で報告する。
pub fn end_injected() -> Injected {
    with_state(State::end_injected)
}

/// 途中の仕事を `reason` で終わらせる（もう終わっていれば無視する）。
pub fn fail(purpose: Purpose, reason: Failure) {
    with_state(|s| s.fail(purpose, reason));
}

/// 座標のコピーのキーのコード（利用者自身の F3+C を見つけるため）。
pub fn set_passive_keys(glfw_copy: Option<i32>, sdl_copy: Option<u32>) {
    PASSIVE_GLFW.store(glfw_copy.unwrap_or(NO_GLFW_KEY), Ordering::Relaxed);
    PASSIVE_SDL.store(sdl_copy.unwrap_or(NO_SDL_KEY), Ordering::Relaxed);
}

pub fn passive_key_glfw(key: i32) -> bool {
    key != NO_GLFW_KEY && PASSIVE_GLFW.load(Ordering::Relaxed) == key
}

pub fn passive_key_sdl(scancode: u32) -> bool {
    scancode != NO_SDL_KEY && PASSIVE_SDL.load(Ordering::Relaxed) == scancode
}

/// 利用者のコピーのキーがゲームに行くところ：[`end_passive`] までにコピーされる座標は
/// 利用者のもの（クリップボードにも届く）。
pub fn begin_passive() {
    with_state(|s| s.passive = true);
}

pub fn end_passive() {
    if WATCHING.load(Ordering::Acquire) {
        with_state(|s| s.passive = false);
    }
}

/// 今のクリップボードへの書き込みが F3+C のもの（自分のか利用者の）でありうるか：フックは
/// そのときだけ文字を読む。
pub fn watching() -> bool {
    WATCHING.load(Ordering::Acquire)
}

/// クリップボードのフックから。true なら書き込みを握りつぶす（自分のものだった）。
pub fn on_clipboard(text: &str) -> bool {
    if !WATCHING.load(Ordering::Acquire) {
        return false;
    }
    with_state(|s| s.on_clipboard(text))
}

pub fn take_outcomes() -> Vec<Outcome> {
    with_state(|s| std::mem::take(&mut s.outcomes))
}

/// 毎フレーム呼ぶ：`timeout` の間に受け取られなかった仕事、または受け取られたが次のフレームで
/// まだ途中の仕事（どちらの OS も 1 回のスワップか 1 回のポーリングのループの中で終える）は、
/// Timeout で失敗する。開いたままのクリップボードの窓は閉じる。
pub fn expire(timeout: Duration) {
    with_state(|s| s.expire(Instant::now(), timeout));
}

#[cfg(test)]
mod tests {
    use reminedog_core::{glfw_key, sdl_keycode, sdl_scancode};

    use super::*;

    const COPIED: &str =
        "/execute in minecraft:the_nether run tp @s 12.50 64.00 -7.25 -179.90 12.30";
    const WINDOW: usize = 0x1000;
    const TIMEOUT: Duration = Duration::from_secs(2);

    fn job(purpose: Purpose) -> Job {
        Job {
            purpose,
            window: WINDOW,
            keys: DebugKeys::default(),
        }
    }

    fn located(purpose: Option<Purpose>) -> Outcome {
        Outcome::Located {
            location: parse_f3c(COPIED).unwrap(),
            purpose,
        }
    }

    fn failed(purpose: Purpose, reason: Failure) -> Outcome {
        Outcome::Failed { purpose, reason }
    }

    /// `purpose` の仕事を OS が受け取った状態。
    fn in_flight(purpose: Purpose) -> State {
        let mut s = State::new();
        assert!(s.request(job(purpose), Instant::now()));
        assert_eq!(s.take_job(Some(WINDOW)), Some(job(purpose)));
        s
    }

    #[test]
    fn captured_location_is_swallowed() {
        let mut s = State::new();
        let now = Instant::now();
        assert!(!s.busy());
        assert!(s.request(job(Purpose::Record), now));
        assert!(s.busy());
        // 忙しい：2 つ目の要求は何も変えない。
        assert!(!s.request(job(Purpose::Refresh), now));
        // GLFW：仕事を受け取るのは、その仕事のウィンドウだけ。
        assert_eq!(s.take_job(Some(WINDOW + 1)), None);
        assert_eq!(s.take_job(Some(WINDOW)), Some(job(Purpose::Record)));
        assert_eq!(s.take_job(Some(WINDOW)), None);
        assert!(s.busy());

        s.begin_injected(Purpose::Record);
        assert!(s.watching());
        assert!(s.on_clipboard(COPIED));
        // 同じ窓の 2 つ目の書き込みも自分のもの。
        assert!(s.on_clipboard("anything"));
        assert_eq!(s.end_injected(), Injected::Captured);
        assert!(!s.busy());
        assert!(!s.watching());
        assert_eq!(s.outcomes, [located(Some(Purpose::Record))]);
        // 窓の外の書き込みには触れない。
        assert!(!s.on_clipboard(COPIED));
        assert_eq!(s.outcomes.len(), 1);
    }

    #[test]
    fn sdl_takes_the_job_for_any_window() {
        let mut s = State::new();
        assert!(s.request(job(Purpose::Refresh), Instant::now()));
        assert_eq!(s.take_job(None), Some(job(Purpose::Refresh)));
        assert_eq!(s.in_flight, Some(Purpose::Refresh));
    }

    #[test]
    fn refused_without_a_write() {
        let mut s = in_flight(Purpose::Refresh);
        s.begin_injected(Purpose::Refresh);
        assert_eq!(s.end_injected(), Injected::Nothing);
        // OS がオーバーレイの切り替えを戻して報告するまでは忙しい。
        assert!(s.busy());
        s.fail(Purpose::Refresh, Failure::Refused);
        assert!(!s.busy());
        assert_eq!(s.outcomes, [failed(Purpose::Refresh, Failure::Refused)]);
    }

    #[test]
    fn unreadable_write_is_swallowed() {
        let mut s = in_flight(Purpose::Record);
        s.begin_injected(Purpose::Record);
        assert!(s.on_clipboard("/execute in bad:DIM run tp @s 1 2 3 4 5"));
        assert_eq!(s.end_injected(), Injected::Written);
        s.fail(Purpose::Record, Failure::Unreadable);
        assert_eq!(s.outcomes, [failed(Purpose::Record, Failure::Unreadable)]);

        // 長すぎて読めないが、それでも自分のキーへのゲームの答え。
        let mut s = in_flight(Purpose::Record);
        s.begin_injected(Purpose::Record);
        assert!(s.on_clipboard(&"x".repeat(MAX_TEXT + 1)));
        assert_eq!(s.end_injected(), Injected::Written);
    }

    #[test]
    fn validation_failure() {
        let mut s = in_flight(Purpose::Record);
        s.fail(Purpose::Record, Failure::KeysHeld(None));
        assert!(!s.busy());
        assert_eq!(
            s.outcomes,
            [failed(Purpose::Record, Failure::KeysHeld(None))]
        );
    }

    #[test]
    fn users_own_f3c_is_passed_on() {
        let mut s = State::new();
        s.passive = true;
        assert!(s.watching());
        assert!(!s.on_clipboard("some chat text"));
        assert!(!s.on_clipboard(&format!("{COPIED}{}", " ".repeat(MAX_TEXT))));
        assert!(s.outcomes.is_empty());
        assert!(!s.on_clipboard(COPIED));
        assert_eq!(s.outcomes, [located(None)]);
        s.passive = false;
        assert!(!s.watching());
        assert!(!s.on_clipboard(COPIED));
        assert_eq!(s.outcomes.len(), 1);
    }

    #[test]
    fn unanswered_requests_time_out() {
        let start = Instant::now();
        let mut s = State::new();
        assert!(s.request(job(Purpose::Record), start));
        s.expire(start + Duration::from_millis(500), TIMEOUT);
        assert!(s.busy());
        assert!(s.outcomes.is_empty());
        s.expire(start + TIMEOUT, TIMEOUT);
        assert!(!s.busy());
        assert_eq!(s.outcomes, [failed(Purpose::Record, Failure::Timeout)]);
    }

    #[test]
    fn abandoned_job_times_out_at_the_next_frame() {
        let mut s = in_flight(Purpose::Refresh);
        s.begin_injected(Purpose::Refresh);
        s.passive = true;
        s.expire(Instant::now(), TIMEOUT);
        assert!(!s.busy());
        assert!(!s.watching());
        assert_eq!(s.outcomes, [failed(Purpose::Refresh, Failure::Timeout)]);
        // 同じ仕事の遅れた報告は、2 つ目の通知を足さない。
        s.fail(Purpose::Refresh, Failure::Refused);
        assert_eq!(s.outcomes.len(), 1);
        // そして次の要求は通る。
        assert!(s.request(job(Purpose::Record), Instant::now()));
    }

    #[test]
    fn outcomes_are_bounded() {
        let mut s = State::new();
        s.passive = true;
        for _ in 0..MAX_OUTCOMES + 5 {
            s.on_clipboard(COPIED);
        }
        assert_eq!(s.outcomes.len(), MAX_OUTCOMES);
    }

    #[test]
    fn debug_key_codes() {
        let keys = DebugKeys::default();
        assert_eq!(
            key_codes(&keys, glfw_key),
            Ok(KeyCodes {
                modifier: 292,
                copy: 67,
            })
        );
        let sdl = |name: &str| sdl_scancode(name).zip(sdl_keycode(name));
        assert_eq!(
            key_codes(&keys, sdl),
            Ok(KeyCodes {
                modifier: (60, 0x4000_003C),
                copy: (6, 0x63),
            })
        );
        assert_eq!(crash_key(&keys, Naming::Modern), Some(InputId::Key(6)));

        let with = |modifier: &str, copy: &str, crash: &str| DebugKeys {
            modifier: modifier.into(),
            copy_location: copy.into(),
            crash: crash.into(),
            shared_with_copy: Vec::new(),
        };
        let f3 = "key.keyboard.f3";
        let c = "key.keyboard.c";
        // 割り当てのないクラッシュのキーは確かめなくてよい。マウスのボタンも（確かめない）。
        // キーなら確かめる。
        assert_eq!(crash_key(&with(f3, c, UNBOUND), Naming::Glfw), None);
        assert_eq!(
            crash_key(&with(f3, c, "key.mouse.left"), Naming::Glfw),
            None
        );
        assert_eq!(
            crash_key(&with(f3, c, "key.keyboard.pause"), Naming::Modern),
            input_by_name("key.keyboard.pause", Naming::Modern)
        );
        assert!(key_codes(&with(f3, c, "key.keyboard.pause"), glfw_key).is_ok());
        assert_eq!(
            key_codes(&with(UNBOUND, c, c), glfw_key),
            Err(Failure::Unbound)
        );
        assert_eq!(
            key_codes(&with(f3, UNBOUND, c), glfw_key),
            Err(Failure::Unbound)
        );
        assert_eq!(
            key_codes(&with(f3, "key.keyboard.left.shift", c), glfw_key),
            Err(Failure::Unsupported(
                DebugKey::Copy,
                key_label("key.keyboard.left.shift")
            ))
        );
        // SDL3 に F25 はない。
        assert_eq!(
            key_codes(&with("key.keyboard.f25", c, c), sdl),
            Err(Failure::Unsupported(DebugKey::Modifier, "F25".into()))
        );
    }
}
