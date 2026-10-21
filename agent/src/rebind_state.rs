//! キーの置き換えが働いている間に、ゲームが押していると読むキー：SDL のスキャンコードごとの
//! ロックなしの表 1 つと、来ない解放のための安全網（[`lost_releases`]）。
//!
//! Minecraft は、受け取ったイベントから、どのキーが押されているかを自分で覚えているが、
//! ウィンドウのライブラリにキーの状態も聞く：画面を閉じたとき（`KeyMapping.setAll` が
//! キーボードの割り当てを全部読み直す）、Ctrl のとき（スタックごと捨てる、データ付きで
//! ブロックを取る）、キーのイベントのたびのデバッグのクラッシュのキー、そして 1.16 では F3。
//! 押下を別のキーとしてゲームに渡した元のキーは、そこでは離していると読まれ、出力は押して
//! いると読まれなければならない。そうでないと、画面を閉じたときに元のキーの割り当てが
//! 押されたまま残り、出力の割り当ては離されてしまう。
//!
//! 各 OS の処理は、ゲームにイベントを渡す直前（GLFW もコールバックの前に自分のキーの状態を
//! 更新する）、フォーカスを失った後、安全網の後に、router の [`RebindState`] から表を書き直す。
//! `glfwGetKey`、`SDL_GetKeyboardState`、キーのイベントの修飾キーのビット、F3+C の確認が
//! これを読む。マウスのボタンは入れない：ゲームはその状態を聞かない。
//!
//! ロックの順序：`router -> SUSPECTS` と `router -> PRESS_SUSPECTS`。表はロックを取らない。
//! 書くのは router を持っている者だけ。

use std::ops::RangeInclusive;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU16, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use reminedog_core::{InputId, SCANCODE_COUNT, input_name};
use reminedog_render::{Output, RebindState};

use crate::input;

/// キーの本当の状態。
const REAL: u8 = 0;
/// ゲームはキーを押していると読む：ルールが押さえている。
const PRESSED: u8 = 1;
/// ゲームはキーを離していると読む：ルールが押下を取った。
const RELEASED: u8 = 2;

/// 修飾キーの SDL のスキャンコード：左の Ctrl、Shift、Alt、Windows キー、その後に右のもの。
pub const MODIFIER_SCANCODES: RangeInclusive<usize> = 224..=231;

/// 押さえている元のキーが、どれだけ離して見えたら解放を取り逃したとみなすか。途中の解放
/// （ゲームが最後にイベントを読んだ後にキーが上がった）は、それより前にゲームに届く。
const LOST_RELEASE_DELAY: Duration = Duration::from_millis(100);

static TABLE: [AtomicU8; SCANCODE_COUNT] = [const { AtomicU8::new(REAL) }; SCANCODE_COUNT];
/// 項目が `REAL` でないキーの最初の [`FORCED_KEY_COUNT`] 個（収まらなければ `usize::MAX`）。
/// 写したキーの状態は、そのキーだけを直す（[`patch_forced`]）。
static FORCED_KEYS: [AtomicU16; MAX_FORCED_KEYS] = [const { AtomicU16::new(0) }; MAX_FORCED_KEYS];
static FORCED_KEY_COUNT: AtomicUsize = AtomicUsize::new(0);
const MAX_FORCED_KEYS: usize = 32;
/// 表を最後に書いたときのイベントのウィンドウ（`GLFWwindow*` か `SDL_Window*`）。
static WINDOW: AtomicUsize = AtomicUsize::new(0);
/// `REAL` でない項目がある。
static FORCED: AtomicBool = AtomicBool::new(false);
/// `REAL` でない修飾キーの項目がある。
static MODIFIERS_FORCED: AtomicBool = AtomicBool::new(false);
/// ルールが出力（キーかマウスのボタン）を押さえている。
static REBOUND: AtomicBool = AtomicBool::new(false);
/// ゲームがそのまま受け取った押下が、ルールの出力を押さえている（[`RebindState::passthrough`]）。
static PASSED: AtomicBool = AtomicBool::new(false);
/// 安全網が見ている id のうち、前の確認で Windows が離していると答えたものと、その時刻。
static SUSPECTS: Mutex<Vec<(InputId, Instant)>> = Mutex::new(Vec::new());
/// router 自身の押下についての同じもの（[`forget_lost_presses`]）。
static PRESS_SUSPECTS: Mutex<Vec<(InputId, Instant)>> = Mutex::new(Vec::new());

fn key_index(id: InputId) -> Option<usize> {
    match id {
        InputId::Key(sc) => Some(usize::from(sc)).filter(|&i| i < SCANCODE_COUNT),
        InputId::Mouse(_) => None,
    }
}

/// `state` の表。取られていて、かつ押さえられているキー（A と B を入れ替えて両方押している）は、
/// ゲームでは押されている。
fn table_of(state: &RebindState) -> [u8; SCANCODE_COUNT] {
    let mut table = [REAL; SCANCODE_COUNT];
    for &id in &state.taken {
        if let Some(i) = key_index(id) {
            table[i] = RELEASED;
        }
    }
    for &(_, id) in &state.held {
        if let Some(i) = key_index(id) {
            table[i] = PRESSED;
        }
    }
    table
}

/// router の状態から、`window` のイベントのために表を書き直す。router をロックしたまま、
/// 状態を変えた呼び出しの直後で、ゲームがイベントを受け取る前に呼ぶこと。
pub fn apply(window: usize, state: &RebindState) {
    let rebound = !state.held.is_empty();
    WINDOW.store(window, Ordering::Relaxed);
    PASSED.store(!state.passthrough.is_empty(), Ordering::Release);
    if !rebound && state.taken.is_empty() && !FORCED.load(Ordering::Acquire) {
        REBOUND.store(false, Ordering::Release);
        return;
    }
    let table = table_of(state);
    for (entry, &value) in TABLE.iter().zip(&table) {
        if entry.load(Ordering::Relaxed) != value {
            entry.store(value, Ordering::Relaxed);
        }
    }
    let mut count = 0;
    for (index, _) in table.iter().enumerate().filter(|&(_, &v)| v != REAL) {
        if let Some(slot) = FORCED_KEYS.get(count) {
            slot.store(index as u16, Ordering::Relaxed);
        }
        count += 1;
    }
    FORCED_KEY_COUNT.store(
        if count <= MAX_FORCED_KEYS {
            count
        } else {
            usize::MAX
        },
        Ordering::Release,
    );
    MODIFIERS_FORCED.store(
        table[MODIFIER_SCANCODES].iter().any(|&v| v != REAL),
        Ordering::Release,
    );
    FORCED.store(table.iter().any(|&v| v != REAL), Ordering::Release);
    REBOUND.store(rebound, Ordering::Release);
}

/// キーについての置き換えの判断：`Some(true)` はゲームが押していると、`Some(false)` は
/// 離していると読む。`None`（マウスのボタンも）は本当の状態。
pub fn forced(id: InputId) -> Option<bool> {
    if !FORCED.load(Ordering::Acquire) {
        return None;
    }
    match TABLE.get(key_index(id)?)?.load(Ordering::Relaxed) {
        PRESSED => Some(true),
        RELEASED => Some(false),
        _ => None,
    }
}

/// `window` についての問い合わせの [`forced`]（GLFW のキーの状態はウィンドウごと）。
pub fn forced_in(window: usize, id: InputId) -> Option<bool> {
    if !FORCED.load(Ordering::Acquire) || WINDOW.load(Ordering::Relaxed) != window {
        return None;
    }
    forced(id)
}

/// ゲームが `id` を押していると読むか：置き換えの判断、なければ `physically()`。
pub fn held(id: InputId, physically: impl FnOnce() -> bool) -> bool {
    forced(id).unwrap_or_else(physically)
}

/// キーの状態 `real`（SDL のスキャンコードごとの C の bool）を `out` に写し、置き換えが判断を
/// 持つ少しのキーだけを直す。判断を持つキーが多すぎて並べられないときは false（何もしない）：
/// その場合はキーごとに [`forced`] を聞くこと。
pub fn patch_forced(real: &[u8], out: &[AtomicU8]) -> bool {
    let count = FORCED_KEY_COUNT.load(Ordering::Acquire);
    if count > MAX_FORCED_KEYS {
        return false;
    }
    for (entry, &down) in out.iter().zip(real) {
        entry.store(down, Ordering::Relaxed);
    }
    for slot in &FORCED_KEYS[..count] {
        let index = usize::from(slot.load(Ordering::Relaxed));
        let (Some(entry), Some(value)) = (out.get(index), TABLE.get(index)) else {
            continue;
        };
        match value.load(Ordering::Relaxed) {
            PRESSED => entry.store(1, Ordering::Relaxed),
            RELEASED => entry.store(0, Ordering::Relaxed),
            _ => {}
        }
    }
    true
}

/// どれかのキーの状態が置き換えによるものか。
pub fn any_forced() -> bool {
    FORCED.load(Ordering::Acquire)
}

/// 修飾キーの状態が置き換えによるものか（そのときは、キーのイベントの修飾キーのビットも
/// それに合わせる）。
pub fn modifiers_forced() -> bool {
    MODIFIERS_FORCED.load(Ordering::Acquire)
}

/// `ids` のどれかをゲームの中で押さえているルール（元のキー、出力）。どのキーを離せばよいかを
/// 利用者に伝えるため。
pub fn rule_holding(ids: &[InputId]) -> Option<(InputId, InputId)> {
    if !REBOUND.load(Ordering::Acquire) {
        return None;
    }
    input::router()
        .rebind_state()
        .held
        .into_iter()
        .find(|(_, output)| ids.contains(output))
}

/// 来ない解放の安全網（IME が取った、ウィンドウがフォーカスを失った、26.x がワールドの読み込み中に
/// 捨てた、そのほか）。ルールが押さえている元のキーかボタンを、Windows が
/// [`LOST_RELEASE_DELAY`] の間離していると答えたら、解放が来たときと同じように離す。ゲームが
/// そのまま受け取ったルールの出力（[`RebindState::passthrough`]）は、そのときゲームに何も送らずに
/// 忘れる：ゲームは自分で離している（26.x は画面が変わるとキーの状態を戻す）し、残すとルール自身の
/// 押下を飲み込んでしまう。`window` のスワップの後に毎フレーム呼ぶ。確かめるのは表を書いた
/// ウィンドウだけ。すぐにゲームに渡す解放を返す（そのときには表は最新）。
pub fn lost_releases(window: usize) -> Vec<Output> {
    if !REBOUND.load(Ordering::Acquire) && !PASSED.load(Ordering::Acquire) {
        SUSPECTS.lock().unwrap_or_else(|e| e.into_inner()).clear();
        return Vec::new();
    }
    if WINDOW.load(Ordering::Relaxed) != window {
        return Vec::new();
    }
    let mut router = input::router();
    let state = router.rebind_state();
    let watched: Vec<InputId> = state
        .held
        .iter()
        .map(|&(source, _)| source)
        .chain(state.passthrough.iter().copied())
        .collect();
    let lost = {
        let mut suspects = SUSPECTS.lock().unwrap_or_else(|e| e.into_inner());
        let (lost, still) = check_suspects(&watched, &suspects, Instant::now(), is_up);
        *suspects = still;
        lost
    };
    if lost.is_empty() {
        return Vec::new();
    }
    let mut releases = Vec::new();
    for source in lost {
        if state.passthrough.contains(&source) {
            if router.forget_passthrough(source) {
                log::debug!(
                    "rebinds: {} is up, but its release never came; forgetting its press",
                    input_name(source)
                );
            }
            continue;
        }
        let release = router.release_source(source);
        match release {
            Some(output) => log::debug!(
                "rebinds: {} is up, but its release never came; releasing {}",
                input_name(source),
                input_name(output.id)
            ),
            None => log::debug!(
                "rebinds: {} is up, but its release never came; its output stays held by another key",
                input_name(source)
            ),
        }
        releases.extend(release);
    }
    apply(window, &router.rebind_state());
    releases
}

/// router 自身の押下（ズームのキー、ゲーム中のホットキー、ゲームが押しているデバッグの修飾キー。
/// [`reminedog_render::InputRouter::held_presses`]）の安全網：Windows が
/// [`LOST_RELEASE_DELAY`] の間離していると答えたものは、解放が来たときと同じように忘れる
/// （26.x はワールドの読み込み中や抜ける間、入力のイベントを捨てる）。そうしないと、次の
/// ワールドでズームが戻ったり、ホットキーがゲームの文字を取り続けたりする。スワップの後に
/// 毎フレーム呼ぶ。
pub fn forget_lost_presses() {
    let mut router = input::router();
    let held = router.held_presses();
    let mut suspects = PRESS_SUSPECTS.lock().unwrap_or_else(|e| e.into_inner());
    if held.is_empty() {
        suspects.clear();
        return;
    }
    let (lost, still) = check_suspects(&held, &suspects, Instant::now(), is_up);
    *suspects = still;
    for id in lost {
        log::debug!(
            "input: {} is up, but its release never came; forgetting its press",
            input_name(id)
        );
        router.forget_press(id);
    }
}

/// `watched` の id（押さえているルールの元のキー、そのまま押されたルールの出力）のうち、
/// [`LOST_RELEASE_DELAY`] より前から（`up` で）離れているもの（取り逃した）と、それより短く
/// 離れているもの（残す容疑。最初に離れて見えた時刻と一緒に）。押して見えた id や、もう
/// 見ていない id は容疑ではない。
fn check_suspects(
    watched: &[InputId],
    suspects: &[(InputId, Instant)],
    now: Instant,
    up: impl Fn(InputId) -> bool,
) -> (Vec<InputId>, Vec<(InputId, Instant)>) {
    let (mut lost, mut still) = (Vec::new(), Vec::new());
    for &source in watched {
        if !up(source) {
            continue;
        }
        let since = suspects
            .iter()
            .find(|&&(id, _)| id == source)
            .map_or(now, |&(_, at)| at);
        if now.saturating_duration_since(since) >= LOST_RELEASE_DELAY {
            lost.push(source);
        } else {
            still.push((source, since));
        }
    }
    (lost, still)
}

/// キーかマウスのボタンを、システムが今離していると答えるか。分からなければ false。
fn is_up(id: InputId) -> bool {
    crate::os::get().input_up(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: InputId = InputId::Key(4);
    const B: InputId = InputId::Key(5);
    const F3: InputId = InputId::Key(60);
    const MOUSE4: InputId = InputId::Mouse(4);

    #[test]
    fn table_takes_sources_and_holds_outputs() {
        let state = RebindState {
            held: vec![(MOUSE4, F3), (A, InputId::Mouse(1))],
            taken: vec![MOUSE4, A],
            passthrough: vec![InputId::Key(6)],
        };
        let table = table_of(&state);
        assert_eq!(table[60], PRESSED);
        assert_eq!(table[4], RELEASED);
        // マウスのボタンは入れない（Mouse(4) はスキャンコード 4 のキーではない）。
        assert_eq!(table.iter().filter(|&&v| v != REAL).count(), 2);
        assert_eq!(table_of(&RebindState::default()), [REAL; SCANCODE_COUNT]);
    }

    #[test]
    fn a_held_output_wins_over_a_taken_source() {
        // A と B を入れ替えて両方押している：ゲームでは両方押されている。
        let state = RebindState {
            held: vec![(A, B), (B, A)],
            taken: vec![A, B],
            ..RebindState::default()
        };
        let table = table_of(&state);
        assert_eq!((table[4], table[5]), (PRESSED, PRESSED));
        // B だけを押している（A として）：B は離して、A は押して読まれる。
        let state = RebindState {
            held: vec![(B, A)],
            taken: vec![B],
            ..RebindState::default()
        };
        let table = table_of(&state);
        assert_eq!((table[4], table[5]), (PRESSED, RELEASED));
    }

    /// 共有の表に書く唯一のテスト。
    #[test]
    fn the_shared_table() {
        const WINDOW_A: usize = 0x1000;
        const LCTRL: InputId = InputId::Key(224);
        let state = RebindState {
            held: vec![(MOUSE4, F3), (A, LCTRL)],
            taken: vec![MOUSE4, A],
            ..RebindState::default()
        };
        apply(WINDOW_A, &state);
        assert!(any_forced() && modifiers_forced());
        assert_eq!(forced(F3), Some(true));
        assert_eq!(forced(LCTRL), Some(true));
        assert_eq!(forced(A), Some(false));
        assert_eq!((forced(B), forced(MOUSE4)), (None, None));
        assert_eq!(forced_in(WINDOW_A, F3), Some(true));
        assert_eq!(forced_in(WINDOW_A + 1, F3), None, "another window");
        assert!(held(F3, || false));
        assert!(!held(A, || true));
        assert!(held(B, || true) && !held(B, || false));
        // マウスだけのルールはキーを強制しないが、押さえていることは分かる。
        let state = RebindState {
            held: vec![(MOUSE4, InputId::Mouse(1))],
            taken: vec![MOUSE4],
            ..RebindState::default()
        };
        apply(WINDOW_A, &state);
        assert!(!any_forced() && !modifiers_forced());
        assert!(REBOUND.load(Ordering::Acquire));
        assert_eq!(forced(F3), None);
        // そのまま押されたルールの出力：安全網が見るが、何も強制しない。
        let state = RebindState {
            passthrough: vec![InputId::Mouse(1)],
            ..RebindState::default()
        };
        apply(WINDOW_A + 1, &state);
        assert!(PASSED.load(Ordering::Acquire) && !REBOUND.load(Ordering::Acquire));
        assert!(!any_forced());
        assert_eq!(WINDOW.load(Ordering::Relaxed), WINDOW_A + 1);
        apply(WINDOW_A, &RebindState::default());
        assert!(!REBOUND.load(Ordering::Acquire) && !PASSED.load(Ordering::Acquire));
        assert!(TABLE.iter().all(|e| e.load(Ordering::Relaxed) == REAL));
        assert!(lost_releases(WINDOW_A).is_empty());
    }

    #[test]
    fn ids_out_of_range_are_left_out() {
        assert_eq!(key_index(InputId::Key(511)), Some(511));
        assert_eq!(key_index(InputId::Key(512)), None);
        assert_eq!(key_index(InputId::Key(u16::MAX)), None);
        let state = RebindState {
            held: vec![(A, InputId::Key(600))],
            taken: vec![InputId::Key(512)],
            ..RebindState::default()
        };
        assert_eq!(table_of(&state), [REAL; SCANCODE_COUNT]);
    }

    #[test]
    fn a_release_counts_as_lost_once_it_is_late() {
        let start = Instant::now();
        // 押さえているルールの元のキーと、そのまま押されたルールの出力。
        let held = [MOUSE4, A];
        let up = |id: InputId| id == MOUSE4;
        // 初めて離れて見えた：容疑。
        let (lost, suspects) = check_suspects(&held, &[], start, up);
        assert!(lost.is_empty());
        assert_eq!(suspects, [(MOUSE4, start)]);
        // まだ離れているが、十分ではない。
        let soon = start + LOST_RELEASE_DELAY / 2;
        let (lost, suspects) = check_suspects(&held, &suspects, soon, up);
        assert!(lost.is_empty());
        assert_eq!(suspects, [(MOUSE4, start)]);
        let late = start + LOST_RELEASE_DELAY;
        let (lost, suspects) = check_suspects(&held, &suspects, late, up);
        assert_eq!(lost, [MOUSE4]);
        assert!(suspects.is_empty());
    }

    #[test]
    fn a_key_seen_down_again_is_no_suspect() {
        let start = Instant::now();
        let held = [MOUSE4];
        let (_, suspects) = check_suspects(&held, &[], start, |_| true);
        let later = start + LOST_RELEASE_DELAY;
        let (lost, suspects) = check_suspects(&held, &suspects, later, |_| false);
        assert!(lost.is_empty() && suspects.is_empty());
        // また押された：待ち直し。
        let (lost, suspects) = check_suspects(&held, &suspects, later, |_| true);
        assert!(lost.is_empty());
        assert_eq!(suspects, [(MOUSE4, later)]);
        // その間に解放された（もう押さえていない）：忘れる。
        let (lost, suspects) = check_suspects(&[], &suspects, later + LOST_RELEASE_DELAY, |_| true);
        assert!(lost.is_empty() && suspects.is_empty());
    }
}
