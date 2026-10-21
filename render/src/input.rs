//! オーバーレイを使っている間のゲームの入力の振り分け：ホットキー、各イベントをオーバーレイ
//! （egui）とゲームのどちらに行かせるか、キーの置き換え。
//!
//! OS に依存しない：GLFW と SDL3 のフックは、自分のイベントをこれらの呼び出しに変え、言われた
//! とおりにそれぞれを捨てるか、渡すか、置き換える。UI を開いている間は、解放（キー、マウスの
//! ボタン）を除いて全部 egui に行く。解放はゲームも受け取るので、押されたまま残るものはない。
//! ホットキーと UI は物理のキーで決まり、それらがゲームに任せた押下について、置き換えがゲームに
//! 何を渡すかを決める（[`crate::rebind`] を参照）。

use crate::hotkey::{self, Hotkey, Trigger};
use crate::overlay::{Action, Hotkeys};
use crate::pointer::PointerSpeed;
use crate::rebind::{Delivery, Output, Phase, RebindState, Rebinder};
use egui::{Event, Key, Modifiers, MouseWheelUnit, PointerButton, Pos2, TouchPhase, pos2, vec2};
use reminedog_core::{InputId, modifier_kind};

/// キーかマウスのボタン以外の入力のイベントをどうするか。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// ゲームに渡す。
    Forward,
    /// オーバーレイが使った。ゲームに見せてはならない。
    Consume,
}

/// 各 OS のフックに何かを頼むホットキー（[`InputRouter::take_actions`] を参照）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyAction {
    RecordWaypoint,
    Navigate,
}

/// ブラウザのホットキー（[`InputRouter::take_browser_actions`] を参照）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowserAction {
    Toggle,
    PageUp,
    PageDown,
    PlayPause,
    SeekBack,
    SeekForward,
}

impl BrowserAction {
    /// 押したままなら、リピートのたびにまた働く。
    fn repeats(self) -> bool {
        matches!(
            self,
            BrowserAction::PageUp
                | BrowserAction::PageDown
                | BrowserAction::SeekBack
                | BrowserAction::SeekForward
        )
    }
}

/// 次のフレームが取るまで覚える操作。
const MAX_ACTIONS: usize = 4;
/// UI への貼り付けが受け取る最大の文字数。
const MAX_PASTE: usize = 4096;
/// 次のフレームが取るまで覚えるブラウザの操作（押しているキーはリピートする）。
const MAX_BROWSER_ACTIONS: usize = 8;

/// ゲーム中に働くホットキー：ウェイポイントと案内のキー、次にブラウザのもの。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InGame {
    Hotkey(HotkeyAction),
    Browser(BrowserAction),
}

impl InGame {
    fn action(self) -> Action {
        match self {
            InGame::Hotkey(HotkeyAction::RecordWaypoint) => Action::Waypoint,
            InGame::Hotkey(HotkeyAction::Navigate) => Action::Navigate,
            InGame::Browser(action) => Action::Browser(action),
        }
    }
}

/// メニューとズームの間の、[`Action::ROUTER_ORDER`] の順。
const IN_GAME: [InGame; 8] = [
    InGame::Hotkey(HotkeyAction::RecordWaypoint),
    InGame::Hotkey(HotkeyAction::Navigate),
    InGame::Browser(BrowserAction::Toggle),
    InGame::Browser(BrowserAction::PageUp),
    InGame::Browser(BrowserAction::PageDown),
    InGame::Browser(BrowserAction::PlayPause),
    InGame::Browser(BrowserAction::SeekBack),
    InGame::Browser(BrowserAction::SeekForward),
];

/// [`IN_GAME`] のキー（その順で）。
fn in_game_keys(keys: &Hotkeys) -> [Option<Hotkey>; IN_GAME.len()] {
    IN_GAME.map(|action| keys.get(action.action()))
}

/// [`InputRouter::start_capture`] と [`InputRouter::start_input_capture`] の結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Captured {
    Hotkey(Hotkey),
    /// 置き換えのための、ゲームが知っているキーかマウスのボタン。
    Input(InputId),
    /// Esc を押したか、UI が閉じた。
    Cancelled,
}

/// 割り当て待ちが待つもの。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Capture {
    /// 押している修飾キー付きの、egui の名前のキーか追加のマウスのボタン。
    Hotkey,
    /// ゲームが知っているどのキー（修飾キーも）か、中か横のマウスのボタン。
    Input,
}

/// Minecraft の既定のデバッグの修飾キー、F3。
const DEFAULT_DEBUG_MODIFIER: InputId = InputId::Key(60);
/// 割り当て待ちをやめる Esc のキー。
const ESCAPE: InputId = InputId::Key(41);

pub struct InputRouter {
    /// オーバーレイが描けるまではオフ。オフの間はどのイベントも渡すので、見せられなかった UI が
    /// 入力を飲み込むことはない。
    enabled: bool,
    ui_open: bool,
    /// 前の `take_ui_changed` から UI が開いたか閉じた。
    ui_changed: bool,
    zoom_held: bool,
    /// 押下でズームを始めたキーかボタン。
    zoom_raw: Option<InputId>,
    modifiers: Modifiers,
    /// ウィンドウのピクセルでの、オーバーレイのポインター。
    pointer: Pos2,
    screen_px: [f32; 2],
    pixels_per_point: f32,
    events: Vec<Event>,
    /// OS が最後に報告したカーソルの絶対位置（GLFW）と、そのときカーソルを捕まえていたか。
    last_cursor: Option<(f64, f64)>,
    last_captured: bool,
    /// 捕まえたカーソルの上で UI を開いている間に飲み込んだ動き。GLFW は捕まえたカーソルを
    /// 増え続ける仮想の位置として報告し、ゲームは前に見た位置との差でカメラを回すので、後で渡す
    /// 位置からはこれを引く。そうしないと、UI を閉じたときに視点が飛ぶ。
    offset: (f64, f64),
    /// オーバーレイの自前のカーソルを動かす相対移動に掛ける。
    pointer_speed: PointerSpeed,
    /// `menu` は UI を開閉し、`zoom` は押している間ズームし、ほかのもの（[`IN_GAME`]）は操作を
    /// 並べる。これらはゲーム中（カーソルを捕まえている）で UI を閉じているとき。
    keys: Hotkeys,
    /// 同じ番号の [`IN_GAME`] のキーを押して取ったので、その解放もこちらのもの。
    action_held: [bool; IN_GAME.len()],
    /// 押している各 [`IN_GAME`] の押下のキーかボタン。
    action_raw: [Option<InputId>; IN_GAME.len()],
    actions: Vec<HotkeyAction>,
    browser_actions: Vec<BrowserAction>,
    /// ブラウザを出している：その切り替え以外のキーを取る。
    browser_shown: bool,
    /// Minecraft のデバッグの修飾キー（options.txt で変えていなければ F3）。ゲームがそれを下ろして
    /// いる間は、ゲーム中のホットキーはゲームに行くので、F3+<キー> の組み合わせは働き続ける。
    debug_modifier: Option<InputId>,
    /// 物理のキーではなく、ゲームが受け取るものに従う：修飾キーに置き換えた押下は押さえ、別のキーに
    /// 置き換えた修飾キーは押さえない。
    debug_held: bool,
    /// UI で次に押したキーかマウスのボタンを取る。
    capturing: Option<Capture>,
    /// 入力の割り当て待ちの間に修飾キーが下りた：それだけで上がったら取るが、それとの組み合わせ
    /// （メニューを閉じる Ctrl+I）は、それの割り当てではない。
    pending_modifier: Option<InputId>,
    captured: Option<Captured>,
    /// 割り当て待ちが押下を取ったキーかボタン：そのリピートと解放もゲームのものではない（F3 の
    /// 解放だけで、メニューの後ろのデバッグの画面が切り替わってしまう。解放のないリピートは、
    /// ゲームの中でキーを押したまま残してしまう）。
    capture_held: Vec<InputId>,
    rebinder: Rebinder,
    /// ゲームが別のキーとして受け取ったキーか、ホットキーを押したかリピートした：その文字もゲームの
    /// ものではない（X → T は、T が開いたチャットに x を打ってしまう）。次の文字かキーのイベントまで。
    suppress_text: bool,
    /// UI の部品がキーボードを持っている（入力欄、ブラウザのページ）：文字を打つメニューのキーは、
    /// 代わりにそこに打つ。
    text_focus: bool,
    /// Ctrl+V（か Shift+Insert）が UI に行った：OS がクリップボードの文字を渡す（[`paste`](Self::paste)）。
    paste_requested: bool,
}

impl Default for InputRouter {
    fn default() -> Self {
        Self::new()
    }
}

impl InputRouter {
    pub const fn new() -> Self {
        Self {
            enabled: false,
            ui_open: false,
            ui_changed: false,
            zoom_held: false,
            zoom_raw: None,
            modifiers: Modifiers::NONE,
            pointer: Pos2::ZERO,
            screen_px: [0.0, 0.0],
            pixels_per_point: 1.0,
            events: Vec::new(),
            last_cursor: None,
            last_captured: false,
            offset: (0.0, 0.0),
            pointer_speed: PointerSpeed::RAW,
            keys: Hotkeys::DEFAULT,
            action_held: [false; IN_GAME.len()],
            action_raw: [None; IN_GAME.len()],
            actions: Vec::new(),
            browser_actions: Vec::new(),
            browser_shown: false,
            debug_modifier: Some(DEFAULT_DEBUG_MODIFIER),
            debug_held: false,
            capturing: None,
            pending_modifier: None,
            captured: None,
            capture_held: Vec::new(),
            rebinder: Rebinder::new(),
            suppress_text: false,
            text_focus: false,
            paste_requested: false,
        }
    }

    /// オーバーレイが描くようになったら振り分けをオンにし、止まったらオフにする（UI を閉じる）。
    pub fn set_enabled(&mut self, enabled: bool) {
        if !enabled {
            self.set_ui_open(false);
            self.zoom_held = false;
            self.zoom_raw = None;
            self.action_held = [false; IN_GAME.len()];
            self.action_raw = [None; IN_GAME.len()];
            self.debug_held = false;
            self.suppress_text = false;
            self.capture_held.clear();
            self.events.clear();
            self.actions.clear();
            self.browser_actions.clear();
        }
        self.enabled = enabled;
    }

    pub fn ui_open(&self) -> bool {
        self.ui_open
    }

    pub fn modifiers(&self) -> Modifiers {
        self.modifiers
    }

    pub fn zoom_active(&self) -> bool {
        self.zoom_held && !self.ui_open
    }

    /// UI を開くか閉じた後に 1 回だけ `Some(open)`。OS の側の後処理のため（SDL3 は UI の入力欄の
    /// ために文字の入力をオンにする必要がある）。
    pub fn take_ui_changed(&mut self) -> Option<bool> {
        std::mem::take(&mut self.ui_changed).then_some(self.ui_open)
    }

    /// ピクセルでのウィンドウの大きさと UI の拡大率。毎フレーム更新する。
    pub fn set_screen(&mut self, size_px: [u32; 2], pixels_per_point: f32) {
        self.screen_px = [size_px[0] as f32, size_px[1] as f32];
        self.pixels_per_point = crate::overlay::clamp_pixels_per_point(pixels_per_point);
    }

    /// UI の部品がキーボードを持っているか（前のフレームから）。
    pub fn set_text_focus(&mut self, focus: bool) {
        self.text_focus = focus;
    }

    /// 前の呼び出しから、UI がクリップボードの文字を求めたか。
    pub fn take_paste_request(&mut self) -> bool {
        std::mem::take(&mut self.paste_requested)
    }

    /// UI が求めた貼り付けのためのクリップボードの文字：1 行で、多くても [`MAX_PASTE`] 文字
    /// （UI の入力欄は 1 行）。
    pub fn paste(&mut self, text: &str) {
        if !self.ui_open {
            return;
        }
        let text: String = text
            .chars()
            .map(|c| {
                if matches!(c, '\r' | '\n' | '\t') {
                    ' '
                } else {
                    c
                }
            })
            .filter(|c| !c.is_control())
            .take(MAX_PASTE)
            .collect();
        let text = text.trim();
        if !text.is_empty() {
            self.push_event(Event::Paste(text.to_owned()));
        }
    }

    /// ゲームが本物のカーソルを隠しているとき（カーソルを捕まえたまま UI を開いている）に、
    /// オーバーレイの自前のカーソルを描く場所（ポイント）。
    pub fn software_cursor(&self) -> Option<Pos2> {
        (self.ui_open && self.last_captured).then(|| self.pointer_points())
    }

    /// 相対移動でオーバーレイの自前のカーソルをどう動かすか：ゲームが生のマウスの数を読むなら
    /// システムのポインターの速度、数がもうシステムのポインターの動き方を通っているなら
    /// [`PointerSpeed::RAW`]。
    pub fn set_pointer_speed(&mut self, speed: PointerSpeed) {
        self.pointer_speed = speed;
    }

    pub fn set_hotkeys(&mut self, keys: Hotkeys) {
        if keys.zoom != self.keys.zoom {
            self.drop_zoom();
        }
        let (old, new) = (in_game_keys(&self.keys), in_game_keys(&keys));
        for (i, (old, new)) in old.into_iter().zip(new).enumerate() {
            if old != new {
                self.drop_action(i);
            }
        }
        self.keys = keys;
    }

    /// ズームの押下を忘れる。その解放（とリピート）は、押下を受け取っていないゲームには渡さない。
    fn drop_zoom(&mut self) {
        if std::mem::take(&mut self.zoom_held) {
            let raw = self.zoom_raw.take();
            self.hold_captured(raw);
        }
    }

    /// `i` の [`IN_GAME`] の押下についての [`drop_zoom`](Self::drop_zoom)。
    fn drop_action(&mut self, i: usize) {
        if std::mem::take(&mut self.action_held[i]) {
            let raw = self.action_raw[i].take();
            self.hold_captured(raw);
        }
    }

    /// router が押下を押さえているキーとボタン（ズームのもの、ゲーム中のホットキーのもの、ゲームが
    /// 下ろしているデバッグの修飾キー）。来ない解放についての OS の確認のため
    /// （[`forget_press`](Self::forget_press)）。
    pub fn held_presses(&self) -> Vec<InputId> {
        let mut held: Vec<InputId> = self
            .zoom_raw
            .filter(|_| self.zoom_held)
            .into_iter()
            .collect();
        for (i, raw) in self.action_raw.iter().enumerate() {
            if self.action_held[i]
                && let Some(id) = *raw
                && !held.contains(&id)
            {
                held.push(id);
            }
        }
        if self.debug_held
            && let Some(id) = self.debug_modifier
            && !held.contains(&id)
        {
            held.push(id);
        }
        held
    }

    /// 解放は来なかったが、キーかボタン `id` が離れていると分かった（26.x はワールドの読み込み中に
    /// 入力のイベントを捨てる）：その router の押下を、解放が来たときと同じように忘れる。
    pub fn forget_press(&mut self, id: InputId) {
        if self.zoom_held && self.zoom_raw == Some(id) {
            self.zoom_held = false;
            self.zoom_raw = None;
        }
        for i in 0..IN_GAME.len() {
            if self.action_held[i] && self.action_raw[i] == Some(id) {
                self.action_held[i] = false;
                self.action_raw[i] = None;
            }
        }
        if self.debug_held && self.debug_modifier == Some(id) && !self.rebinder.rule_holds(id) {
            self.debug_held = false;
        }
    }

    /// Minecraft のデバッグの修飾キーかボタン。割り当てがなければ `None`。
    /// router が渡した限りで、ゲームがデバッグの修飾キーを下ろしているか。
    pub fn debug_held(&self) -> bool {
        self.debug_held
    }

    pub fn set_debug_modifier(&mut self, id: Option<InputId>) {
        if id != self.debug_modifier {
            self.debug_modifier = id;
            self.debug_held = false;
        }
    }

    /// これからの押下についての置き換えのルール（[`crate::resolve`] から）。今押さえている押下は、
    /// 解放まで、したことのまま。`keyboard_sources` がなければ、キーからのルールは除く（OS が
    /// 押しているキーをゲームから隠せなかった）。
    pub fn set_rebinds(&mut self, rules: Vec<(InputId, InputId)>, keyboard_sources: bool) {
        let rules = rules
            .into_iter()
            .filter(|(source, _)| keyboard_sources || !matches!(source, InputId::Key(_)))
            .collect();
        self.rebinder.set_rules(rules);
    }

    /// 使っているルール：(元のキー, 出力)。
    pub fn rebinds(&self) -> &[(InputId, InputId)] {
        self.rebinder.rules()
    }

    /// 置き換えが今、ゲームの中で押さえているものと、ゲームから隠しているもの。
    pub fn rebind_state(&self) -> RebindState {
        self.rebinder.state()
    }

    /// 解放は来なかったが、元のキーが離れていると分かった（OS が本当のキーの状態を確かめた）：
    /// 解放と同じようにその押下を忘れ、ゲームに渡すイベントがあれば返す。
    #[must_use = "the release must reach the game"]
    pub fn release_source(&mut self, id: InputId) -> Option<Output> {
        let output = self.rebinder.release_source(id);
        if let Some(output) = output {
            self.delivered(None, Phase::Release, Delivery::Send(output));
        }
        output
    }

    /// 解放は来なかったが、[`RebindState::passthrough`] のキーかボタンが離れていると分かった：
    /// ゲームに解放を渡さずに（ゲームは自分で離した）その押下を忘れる。そういう押下があったかを返す。
    pub fn forget_passthrough(&mut self, id: InputId) -> bool {
        let forgotten = self.rebinder.forget_passthrough(id);
        if forgotten && self.debug_modifier == Some(id) && !self.rebinder.rule_holds(id) {
            self.debug_held = false;
        }
        forgotten
    }

    /// 今の押下に置き換えが効くか：ゲーム中で UI を閉じているとき。
    fn rebind_active(&self, captured: bool) -> bool {
        self.enabled && captured && !self.ui_open && self.capturing.is_none()
    }

    /// 前の呼び出しから押したウェイポイントと案内のホットキー（古い順）。
    pub fn take_actions(&mut self) -> Vec<HotkeyAction> {
        std::mem::take(&mut self.actions)
    }

    /// 前の呼び出しから押した（かリピートした）ブラウザのホットキー（古い順）。
    pub fn take_browser_actions(&mut self) -> Vec<BrowserAction> {
        std::mem::take(&mut self.browser_actions)
    }

    /// ブラウザを出しているか：そのときだけ、そのキー（切り替え以外）が働く。毎フレーム入れる。
    pub fn set_browser_shown(&mut self, shown: bool) {
        self.browser_shown = shown;
    }

    /// UI を開いている間に押した次のキー（修飾キー付き）か追加のマウスのボタンを、egui に渡したり
    /// ホットキーとして働かせたりせずに取る。[`take_captured`](Self::take_captured) を参照。
    pub fn start_capture(&mut self) {
        self.begin_capture(Capture::Hotkey);
    }

    /// UI を開いている間に押した、ゲームが知っている次のキー（修飾キーも）か中か横のマウスのボタンを、
    /// [`Captured::Input`] として取る。Esc でやめる。クリックは UI を操作し続ける。
    /// [`take_captured`](Self::take_captured) を参照。
    pub fn start_input_capture(&mut self) {
        self.begin_capture(Capture::Input);
    }

    fn begin_capture(&mut self, capture: Capture) {
        if self.ui_open {
            self.capturing = Some(capture);
            self.pending_modifier = None;
            self.captured = None;
        }
    }

    pub fn cancel_capture(&mut self) {
        self.pending_modifier = None;
        if self.capturing.take().is_some() {
            self.captured = Some(Captured::Cancelled);
        }
    }

    pub fn take_captured(&mut self) -> Option<Captured> {
        self.captured.take()
    }

    fn finish_capture(&mut self, result: Captured) {
        self.capturing = None;
        self.pending_modifier = None;
        self.captured = Some(result);
    }

    /// イベントが、割り当て待ちが取ったキーかボタンの解放か（それもゲームは受け取ってはならない）。
    /// 新しい押下は、その解放がなくなったことを意味する：忘れる。
    fn capture_release(&mut self, raw: Option<InputId>, phase: Phase) -> bool {
        let Some(i) = raw.and_then(|id| self.capture_held.iter().position(|&held| held == id))
        else {
            return false;
        };
        match phase {
            Phase::Repeat => true,
            Phase::Release => {
                self.capture_held.remove(i);
                true
            }
            // また押された：その解放は来なかった。
            Phase::Press => {
                self.capture_held.remove(i);
                false
            }
        }
    }

    /// 取った押下を覚え、そのリピートと解放をゲームに渡さない。
    fn hold_captured(&mut self, raw: Option<InputId>) {
        const MAX: usize = 4;
        if let Some(id) = raw
            && !self.capture_held.contains(&id)
        {
            if self.capture_held.len() == MAX {
                self.capture_held.remove(0);
            }
            self.capture_held.push(id);
        }
    }

    /// 前の呼び出しから集めた egui のイベント。
    pub fn take_events(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.events)
    }

    pub fn set_ui_open(&mut self, open: bool) {
        if open == self.ui_open {
            return;
        }
        self.ui_open = open;
        self.ui_changed = true;
        log::info!("ui {}", if open { "opened" } else { "closed" });
        self.drop_zoom();
        for i in 0..IN_GAME.len() {
            self.drop_action(i);
        }
        self.cancel_capture();
        if !open && matches!(self.captured, Some(Captured::Input(_))) {
            // メニューが閉じたのと同じフレームで取った（キー、次に Esc）：それを待っていたメニューはもうない。
            self.captured = Some(Captured::Cancelled);
        }
        self.paste_requested = false;
        if open {
            if self.last_captured || self.pointer == Pos2::ZERO {
                self.pointer = pos2(self.screen_px[0] / 2.0, self.screen_px[1] / 2.0);
            }
            self.push_event(Event::PointerMoved(self.pointer_points()));
        } else {
            self.push_event(Event::PointerGone);
        }
    }

    /// キーの押下、リピート、解放。
    ///
    /// `key` はホットキーと UI が照らし合わせるもの（egui に名前のないキーなら `None`）、`raw` は
    /// ゲームが知っている物理のキー（知らないキーなら `None`）で、置き換えとデバッグの修飾キーは
    /// これで決まる。`captured`：ゲームがカーソルを捕まえている。つまりメニューではなくプレイ中。
    /// `stateless`：OS はこのキーの状態を持たず、オートリピートを押下として送り、フォーカスを
    /// 失っても解放を送らない（GLFW の key -1）。
    #[allow(clippy::too_many_arguments)]
    pub fn key(
        &mut self,
        key: Option<Key>,
        raw: Option<InputId>,
        pressed: bool,
        repeat: bool,
        modifiers: Modifiers,
        captured: bool,
        stateless: bool,
    ) -> Delivery {
        // メニューのキーは、ゲームがカーソルを捕まえたか放した後（チャットを閉じた）、どの動きより先に
        // 来ることがある：メニューは、どちらのカーソルを見せるかを知らなければならない。
        self.observe_capture(captured);
        self.modifiers = modifiers;
        self.suppress_text = false;
        let phase = phase(pressed, repeat);
        let delivery = self
            .rebound(raw, phase, captured, stateless)
            .unwrap_or_else(|| {
                let route = self.key_route(key, raw, pressed, repeat, modifiers, captured);
                self.after_route(route, raw, phase, captured)
            });
        self.delivered(raw, phase, delivery);
        if phase != Phase::Release
            && let Some(id @ InputId::Key(_)) = raw
            && self.rebinder.takes(id)
        {
            self.suppress_text = true;
        }
        delivery
    }

    /// 置き換えの最初の判断：前に記録した元のキーについては、それだけで決める（置き換えた押下の
    /// リピートと解放は、UI やホットキーが今何をしても、その押下に従わなければならない）。
    fn rebound(
        &mut self,
        raw: Option<InputId>,
        phase: Phase,
        captured: bool,
        stateless: bool,
    ) -> Option<Delivery> {
        let active = self.rebind_active(captured);
        self.rebinder.latched(raw?, phase, active, stateless)
    }

    /// router 自身の判断の後に：渡す押下は置き換えに行き（置き換えるかもしれない）、解放はその押下が
    /// したことを終える。
    fn after_route(
        &mut self,
        route: Route,
        raw: Option<InputId>,
        phase: Phase,
        captured: bool,
    ) -> Delivery {
        let consumed = route == Route::Consume;
        let Some(id) = raw else {
            return if consumed {
                Delivery::Consume
            } else {
                Delivery::Forward
            };
        };
        match phase {
            Phase::Press if !consumed => {
                let active = self.rebind_active(captured);
                self.rebinder.press(id, active)
            }
            Phase::Release => {
                let delivery = self.rebinder.release(id);
                if consumed {
                    Delivery::Consume
                } else {
                    delivery
                }
            }
            Phase::Press | Phase::Repeat if consumed => Delivery::Consume,
            Phase::Press | Phase::Repeat => Delivery::Forward,
        }
    }

    /// `debug_held` を、ゲームが受け取るものに合わせる。
    fn delivered(&mut self, raw: Option<InputId>, phase: Phase, delivery: Delivery) {
        let delivered = match delivery {
            Delivery::Forward => raw.map(|id| Output { id, phase }),
            Delivery::Send(output) => Some(output),
            Delivery::Consume => None,
        };
        if let Some(output) = delivered
            && self.debug_modifier == Some(output.id)
        {
            self.debug_held = output.phase != Phase::Release;
        }
    }

    /// 物理のキーによる、router 自身のキーの処理：割り当て待ち、メニューのキー、UI、ゲーム中の
    /// ホットキー。
    fn key_route(
        &mut self,
        key: Option<Key>,
        raw: Option<InputId>,
        pressed: bool,
        repeat: bool,
        modifiers: Modifiers,
        captured: bool,
    ) -> Route {
        if !self.enabled {
            return Route::Forward;
        }
        if self.capture_release(raw, phase(pressed, repeat)) {
            return Route::Consume;
        }
        let menu_key = pressed
            && self.keys.menu.matches_key(key, modifiers)
            && !self.menu_key_types_here(key, captured);
        if self.ui_open
            && let Some(capture) = self.capturing
        {
            if pressed && !repeat {
                let escape = key == Some(Key::Escape) || raw == Some(ESCAPE);
                let combined = self.pending_modifier.is_some() && menu_key;
                match (capture, key, raw) {
                    _ if escape => self.finish_capture(Captured::Cancelled),
                    // 修飾キーを押したままのメニューのキー：メニューを閉じる。
                    _ if combined => {
                        self.hold_captured(raw);
                        self.set_ui_open(false);
                    }
                    (Capture::Hotkey, Some(k), _) if hotkey::assignable(Trigger::Key(k)) => {
                        self.finish_capture(Captured::Hotkey(Hotkey::with_modifiers(
                            Trigger::Key(k),
                            modifiers,
                        )));
                        self.hold_captured(raw);
                    }
                    (Capture::Input, _, Some(id)) if modifier_kind(id).is_some() => {
                        self.pending_modifier = Some(id);
                    }
                    (Capture::Input, _, Some(id)) => {
                        self.finish_capture(Captured::Input(id));
                        self.hold_captured(raw);
                    }
                    // 修飾キーだけ（ホットキーのため）と、名前のないキー：待ち続ける。
                    _ => {}
                }
            } else if !pressed
                && let Some(id) = raw
                && self.pending_modifier == Some(id)
            {
                // 修飾キーがそれだけで上がった：それがキー。
                self.finish_capture(Captured::Input(id));
                return Route::Consume;
            }
            return if pressed {
                Route::Consume
            } else {
                Route::Forward
            };
        }
        if menu_key {
            if !repeat {
                self.set_ui_open(!self.ui_open);
                // その解放もゲームのものではない（メニューのキーとしての F3 は、デバッグの画面を切り替えてしまう）。
                self.hold_captured(raw);
            }
            return Route::Consume;
        }
        if self.ui_open {
            if key == Some(Key::Escape) && pressed {
                if !repeat {
                    self.set_ui_open(false);
                    self.hold_captured(raw);
                }
                return Route::Consume;
            }
            if let Some(key) = key {
                self.push_event(Event::Key {
                    key,
                    physical_key: None,
                    pressed,
                    repeat,
                    modifiers,
                });
                if pressed {
                    self.clipboard_key(key, modifiers);
                }
            }
            return if pressed {
                Route::Consume
            } else {
                Route::Forward
            };
        }
        let Some(key) = key else {
            return Route::Forward;
        };
        self.game_hotkey(Trigger::Key(key), raw, pressed, repeat, modifiers, captured)
    }

    /// Ctrl や Alt なしで文字を打つメニューのキー（M、スペース）は、文字の行き先に打つ：キーボードを
    /// 持つ UI の入力欄かページ、そしてゲームの画面（チャット、看板）では、そこでメニューを開かずに。
    fn menu_key_types_here(&self, key: Option<Key>, captured: bool) -> bool {
        let menu = self.keys.menu;
        !menu.ctrl
            && !menu.alt
            && key.is_some_and(hotkey::types_text)
            && if self.ui_open {
                self.text_focus
            } else {
                !captured
            }
    }

    /// UI のクリップボードのキー：入力欄のコピー、切り取り、貼り付け（ブラウザのページはキーそのものを
    /// 受け取る）。
    fn clipboard_key(&mut self, key: Key, modifiers: Modifiers) {
        let command = modifiers.ctrl || modifiers.command;
        match key {
            Key::V if command && !modifiers.alt => self.paste_requested = true,
            Key::Insert if modifiers.shift => self.paste_requested = true,
            Key::C if command && !modifiers.alt => self.push_event(Event::Copy),
            Key::Insert if command => self.push_event(Event::Copy),
            Key::X if command && !modifiers.alt => self.push_event(Event::Cut),
            _ => {}
        }
    }

    /// ゲーム中のホットキー（UI を閉じている）：ウェイポイントと案内のキー、ブラウザのもの（切り替え
    /// 以外は出している間だけ）、次にズーム（修飾キーを無視して照らし合わせる）。デバッグの
    /// 修飾キーを押している間の新しい押下はゲームに行く。前に取ったキーの解放は、まだこちらのもの。
    fn game_hotkey(
        &mut self,
        trigger: Trigger,
        raw: Option<InputId>,
        pressed: bool,
        repeat: bool,
        modifiers: Modifiers,
        captured: bool,
    ) -> Route {
        let keys = in_game_keys(&self.keys);
        for (i, (action, hotkey)) in IN_GAME.into_iter().zip(keys).enumerate() {
            let Some(hotkey) = hotkey.filter(|hotkey| hotkey.trigger == trigger) else {
                continue;
            };
            if !pressed {
                if std::mem::take(&mut self.action_held[i]) {
                    self.action_raw[i] = None;
                    return Route::Consume;
                }
            } else if repeat {
                if self.action_held[i] {
                    if let InGame::Browser(action) = action
                        && action.repeats()
                    {
                        self.queue(InGame::Browser(action));
                    }
                    self.suppress_own_text(trigger);
                    return Route::Consume;
                }
            } else {
                let active = match action {
                    InGame::Browser(action) => {
                        action == BrowserAction::Toggle || self.browser_shown
                    }
                    InGame::Hotkey(_) => true,
                };
                if active && captured && !self.debug_held && hotkey.modifiers_held(modifiers) {
                    self.action_held[i] = true;
                    self.action_raw[i] = raw;
                    self.queue(action);
                    self.suppress_own_text(trigger);
                    return Route::Consume;
                }
            }
        }
        // デバッグの修飾キーそのものへのズームは、それとの組み合わせではない。
        let is_modifier = raw.is_some() && raw == self.debug_modifier;
        if self.keys.zoom.trigger == trigger && (self.zoom_held || !self.debug_held || is_modifier)
        {
            let route = self.zoom_input(raw, pressed, repeat, modifiers, captured);
            if route == Route::Consume && pressed {
                self.suppress_own_text(trigger);
            }
            return route;
        }
        Route::Forward
    }

    /// ホットキー自身の押下かリピートが打つ文字はゲームのものではない（次の文字のイベントだけ：
    /// その間に開いたチャットで打っている間に押したままのホットキーが、チャットの文字を飲み込んでは
    /// ならない）。
    fn suppress_own_text(&mut self, trigger: Trigger) {
        if matches!(trigger, Trigger::Key(_)) {
            self.suppress_text = true;
        }
    }

    fn queue(&mut self, action: InGame) {
        let (len, max) = match action {
            InGame::Hotkey(_) => (self.actions.len(), MAX_ACTIONS),
            InGame::Browser(_) => (self.browser_actions.len(), MAX_BROWSER_ACTIONS),
        };
        if len >= max {
            log::debug!("hotkey {action:?} dropped: {max} actions pending");
            return;
        }
        match action {
            InGame::Hotkey(action) => self.actions.push(action),
            InGame::Browser(action) => self.browser_actions.push(action),
        }
    }

    /// ズームのホットキーが下りたか上がった（UI を閉じている）。始めるのは新しい押下だけ：ゲームが
    /// 受け取った押下（そのとき修飾キーを押していた）のリピートは、解放をゲームに任せなければならない。
    fn zoom_input(
        &mut self,
        raw: Option<InputId>,
        pressed: bool,
        repeat: bool,
        modifiers: Modifiers,
        captured: bool,
    ) -> Route {
        if pressed {
            let starts = !repeat && captured && self.keys.zoom.modifiers_held(modifiers);
            if self.zoom_held || starts {
                if !self.zoom_held {
                    self.zoom_raw = raw;
                }
                self.zoom_held = true;
                return Route::Consume;
            }
        } else if self.zoom_held {
            self.zoom_held = false;
            self.zoom_raw = None;
            return Route::Consume;
        }
        Route::Forward
    }

    /// 打った文字（キーボードの配列と IME の後）。
    pub fn text(&mut self, text: &str) -> Route {
        if std::mem::take(&mut self.suppress_text) {
            // ゲームが別のキーとして受け取ったキーの文字、またはホットキーの文字。
            return Route::Consume;
        }
        if !self.ui_open || !self.enabled {
            return Route::Forward;
        }
        let text: String = text.chars().filter(|c| !c.is_control()).collect();
        if !text.is_empty() {
            self.push_event(Event::Text(text));
        }
        Route::Consume
    }

    /// マウスのボタンの押下か解放。`raw` と `captured` は [`key`](Self::key) と同じ。
    pub fn button(
        &mut self,
        button: PointerButton,
        raw: Option<InputId>,
        pressed: bool,
        captured: bool,
    ) -> Delivery {
        self.observe_capture(captured);
        let phase = phase(pressed, false);
        let delivery = self
            .rebound(raw, phase, captured, false)
            .unwrap_or_else(|| {
                let route = self.button_route(button, raw, pressed, captured);
                self.after_route(route, raw, phase, captured)
            });
        self.delivered(raw, phase, delivery);
        delivery
    }

    /// [`key_route`](Self::key_route) と同じ、router 自身のマウスのボタンの処理。
    fn button_route(
        &mut self,
        button: PointerButton,
        raw: Option<InputId>,
        pressed: bool,
        captured: bool,
    ) -> Route {
        if !self.enabled {
            return Route::Forward;
        }
        if self.capture_release(raw, phase(pressed, false)) {
            return Route::Consume;
        }
        if self.ui_open
            && let Some(capture) = self.capturing
        {
            let result = match capture {
                Capture::Hotkey => hotkey::assignable(Trigger::Mouse(button)).then(|| {
                    Captured::Hotkey(Hotkey::with_modifiers(
                        Trigger::Mouse(button),
                        self.modifiers,
                    ))
                }),
                // 左と右のクリックは UI を操作する。
                Capture::Input => raw
                    .filter(|id| matches!(id, InputId::Mouse(2 | 4 | 5)))
                    .map(Captured::Input),
            };
            if pressed && let Some(result) = result {
                self.finish_capture(result);
                self.hold_captured(raw);
                return Route::Consume;
            }
        } else if pressed && self.keys.menu.matches_button(button, self.modifiers) {
            self.set_ui_open(!self.ui_open);
            self.hold_captured(raw);
            return Route::Consume;
        }
        if !self.ui_open {
            let modifiers = self.modifiers;
            return self.game_hotkey(
                Trigger::Mouse(button),
                raw,
                pressed,
                false,
                modifiers,
                captured,
            );
        }
        self.push_event(Event::PointerButton {
            pos: self.pointer_points(),
            button,
            pressed,
            modifiers: self.modifiers,
        });
        if pressed {
            Route::Consume
        } else {
            Route::Forward
        }
    }

    /// カーソルの絶対位置（GLFW）。ゲームに渡す位置を返す。イベントを捨てるなら `None`。
    pub fn cursor_position(&mut self, x: f64, y: f64, captured: bool) -> Option<(f64, f64)> {
        self.observe_capture(captured);
        let (dx, dy) = self
            .last_cursor
            .map_or((0.0, 0.0), |(lx, ly)| (x - lx, y - ly));
        self.last_cursor = Some((x, y));
        if self.ui_open {
            if captured {
                self.offset.0 += dx;
                self.offset.1 += dy;
                self.move_pointer(dx as f32, dy as f32);
            } else {
                self.set_pointer(x as f32, y as f32);
            }
            return None;
        }
        if !captured {
            // メニューをその上に開くための、自由なカーソルの場所。
            self.pointer = pos2(x as f32, y as f32);
        }
        Some(if captured {
            (x - self.offset.0, y - self.offset.1)
        } else {
            (x, y)
        })
    }

    /// どのイベントからでも、ゲームがカーソルを持っているか：ゲームはカーソルを捕まえるか放すときに、
    /// カーソルとその基準の点を中央に戻すので、前の動きはもう関係ない。
    fn observe_capture(&mut self, captured: bool) {
        if captured != self.last_captured {
            self.last_captured = captured;
            self.last_cursor = None;
            self.offset = (0.0, 0.0);
        }
    }

    /// 絶対位置付きの相対的なマウスの動き（SDL3）。
    pub fn cursor_motion(&mut self, dx: f32, dy: f32, x: f32, y: f32, captured: bool) -> Route {
        self.observe_capture(captured);
        if !self.ui_open {
            if !captured {
                // メニューをその上に開くための、自由なカーソルの場所。
                self.pointer = pos2(x, y);
            }
            return Route::Forward;
        }
        if captured {
            self.move_pointer(dx, dy);
        } else {
            self.set_pointer(x, y);
        }
        Route::Consume
    }

    /// 行でのホイールの動き（正の y = 利用者から離れる向き）。
    pub fn scroll(&mut self, dx: f32, dy: f32) -> Route {
        if !self.ui_open {
            return Route::Forward;
        }
        self.push_event(Event::MouseWheel {
            unit: MouseWheelUnit::Line,
            delta: vec2(dx, dy),
            phase: TouchPhase::Move,
            modifiers: self.modifiers,
        });
        Route::Consume
    }

    /// ウィンドウがフォーカスを失った：押しているキーの解放は来ないかもしれない。ゲームがいま受け取る
    /// べき、置き換えの出力の解放を返す（元のキーについての OS 自身の解放より先に。そのときは
    /// それを捨てる）。`platform_releases(source)`：OS はフォーカスを失った後に、そのキーかボタンの
    /// 解放を送る（GLFW：key -1 以外のどのキーも、どのボタンも。SDL3：キーだけ）。
    #[must_use = "the releases must reach the game"]
    pub fn focus_lost(&mut self, platform_releases: impl Fn(InputId) -> bool) -> Vec<Output> {
        if self.ui_open {
            // UI は押さえているものを離す（ブラウザのページは、そのボタンとキーを）。
            self.push_event(Event::WindowFocused(false));
        }
        self.zoom_held = false;
        self.zoom_raw = None;
        self.action_held = [false; IN_GAME.len()];
        self.action_raw = [None; IN_GAME.len()];
        self.debug_held = false;
        self.suppress_text = false;
        self.rebinder.focus_lost(platform_releases)
    }

    fn push_event(&mut self, event: Event) {
        // フレームを描かない間（最小化したウィンドウ）は、誰もキューを空にしない。
        const MAX_QUEUED: usize = 1024;
        if self.events.len() >= MAX_QUEUED {
            self.events.clear();
        }
        self.events.push(event);
    }

    fn pointer_points(&self) -> Pos2 {
        (self.pointer.to_vec2() / self.pixels_per_point).to_pos2()
    }

    fn set_pointer(&mut self, x: f32, y: f32) {
        self.pointer = pos2(x, y);
        self.push_event(Event::PointerMoved(self.pointer_points()));
    }

    fn move_pointer(&mut self, dx: f32, dy: f32) {
        let (dx, dy) = self.pointer_speed.apply(dx, dy);
        let [w, h] = self.screen_px;
        let x = (self.pointer.x + dx).clamp(0.0, (w - 1.0).max(0.0));
        let y = (self.pointer.y + dy).clamp(0.0, (h - 1.0).max(0.0));
        self.set_pointer(x, y);
    }
}

fn phase(pressed: bool, repeat: bool) -> Phase {
    match (pressed, repeat) {
        (false, _) => Phase::Release,
        (true, true) => Phase::Repeat,
        (true, false) => Phase::Press,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reminedog_core::{Naming, input_by_name};

    const CTRL: Modifiers = Modifiers::CTRL;
    const NONE: Modifiers = Modifiers::NONE;

    fn router() -> InputRouter {
        let mut r = InputRouter::new();
        r.set_screen([800, 600], 1.0);
        r.set_enabled(true);
        r
    }

    #[test]
    fn in_game_hotkeys_match_in_the_menus_order() {
        // メニューの重なりの確認は、この順を前提にする。
        let order = Action::ROUTER_ORDER;
        assert_eq!(order[0], Action::Menu);
        assert_eq!(order[order.len() - 1], Action::Zoom);
        assert_eq!(IN_GAME.map(InGame::action), order[1..order.len() - 1]);
    }

    /// 同じ名前の egui のキーについて、ゲームが知っているキー（文字、F キー、Esc）。
    fn raw(key: Key) -> Option<InputId> {
        let name = format!("key.keyboard.{}", key.name().to_ascii_lowercase());
        input_by_name(&name, Naming::Modern)
    }

    fn mouse_raw(button: PointerButton) -> InputId {
        InputId::Mouse(match button {
            PointerButton::Primary => 1,
            PointerButton::Middle => 2,
            PointerButton::Secondary => 3,
            PointerButton::Extra1 => 4,
            PointerButton::Extra2 => 5,
        })
    }

    fn route(delivery: Delivery) -> Route {
        match delivery {
            Delivery::Forward => Route::Forward,
            Delivery::Consume => Route::Consume,
            Delivery::Send(output) => panic!("rebound to {output:?}"),
        }
    }

    /// 何も置き換えていないときの、router 自身の振り分け。
    impl InputRouter {
        fn route_key(
            &mut self,
            key: Option<Key>,
            pressed: bool,
            repeat: bool,
            modifiers: Modifiers,
            captured: bool,
        ) -> Route {
            let raw = key.and_then(raw);
            route(self.key(key, raw, pressed, repeat, modifiers, captured, false))
        }

        fn route_button(&mut self, button: PointerButton, pressed: bool, captured: bool) -> Route {
            route(self.button(button, Some(mouse_raw(button)), pressed, captured))
        }
    }

    #[test]
    fn disabled_router_forwards_everything() {
        let mut r = InputRouter::new();
        assert_eq!(
            r.route_key(Some(Key::I), true, false, CTRL, true),
            Route::Forward
        );
        assert!(!r.ui_open());
        assert_eq!(
            r.route_key(Some(Key::Z), true, false, NONE, true),
            Route::Forward
        );
        let mut r = router();
        ctrl_i(&mut r);
        r.set_enabled(false);
        assert!(!r.ui_open(), "stopping the overlay closes the UI");
        assert_eq!(
            r.route_key(Some(Key::W), true, false, NONE, true),
            Route::Forward
        );
        assert!(r.take_events().is_empty());
    }

    fn ctrl_i(r: &mut InputRouter) -> Route {
        r.route_key(Some(Key::I), true, false, CTRL, true)
    }

    #[test]
    fn ctrl_i_toggles_the_ui_and_is_never_forwarded() {
        let mut r = router();
        assert_eq!(ctrl_i(&mut r), Route::Consume);
        assert!(r.ui_open());
        assert_eq!(r.take_ui_changed(), Some(true));
        assert_eq!(r.take_ui_changed(), None);
        assert_eq!(
            r.route_key(Some(Key::I), true, true, CTRL, true),
            Route::Consume,
            "repeat"
        );
        assert!(r.ui_open(), "repeats do not toggle");
        assert_eq!(ctrl_i(&mut r), Route::Consume);
        assert!(!r.ui_open());
        assert_eq!(r.take_ui_changed(), Some(false));
        // ただの I はゲームのもの。余分な修飾キー付きでも切り替わる。
        assert_eq!(
            r.route_key(Some(Key::I), true, false, NONE, true),
            Route::Forward
        );
        let ctrl_shift = Modifiers {
            shift: true,
            ..CTRL
        };
        assert_eq!(
            r.route_key(Some(Key::I), true, false, ctrl_shift, true),
            Route::Consume
        );
    }

    #[test]
    fn open_ui_takes_presses_but_forwards_releases() {
        let mut r = router();
        ctrl_i(&mut r);
        r.take_events();
        assert_eq!(
            r.route_key(Some(Key::W), true, false, NONE, true),
            Route::Consume
        );
        assert_eq!(
            r.route_key(Some(Key::W), false, false, NONE, true),
            Route::Forward
        );
        assert_eq!(
            r.route_key(None, true, false, NONE, true),
            Route::Consume,
            "unnamed keys too"
        );
        assert_eq!(
            r.route_button(PointerButton::Primary, true, true),
            Route::Consume
        );
        assert_eq!(
            r.route_button(PointerButton::Primary, false, true),
            Route::Forward
        );
        assert_eq!(r.text("あ"), Route::Consume);
        assert_eq!(r.scroll(0.0, 1.0), Route::Consume);
        let events = r.take_events();
        assert!(events.contains(&Event::Text("あ".into())));
        assert!(events.iter().any(|e| matches!(
            e,
            Event::Key {
                key: Key::W,
                pressed: false,
                ..
            }
        )));
    }

    #[test]
    fn escape_closes_the_ui_without_reaching_the_game() {
        let mut r = router();
        ctrl_i(&mut r);
        assert_eq!(
            r.route_key(Some(Key::Escape), true, false, NONE, true),
            Route::Consume
        );
        assert!(!r.ui_open());
        // UI を閉じると、Escape はまたゲームのもの。
        assert_eq!(
            r.route_key(Some(Key::Escape), true, false, NONE, true),
            Route::Forward
        );
    }

    #[test]
    fn closed_ui_forwards_everything_but_hotkeys() {
        let mut r = router();
        assert_eq!(
            r.route_key(Some(Key::W), true, false, NONE, true),
            Route::Forward
        );
        assert_eq!(r.text("w"), Route::Forward);
        assert_eq!(
            r.route_button(PointerButton::Primary, true, true),
            Route::Forward
        );
        assert_eq!(r.scroll(0.0, 1.0), Route::Forward);
        assert_eq!(r.cursor_position(10.0, 20.0, true), Some((10.0, 20.0)));
        assert!(r.take_events().is_empty());
    }

    #[test]
    fn zoom_is_held_in_game_only() {
        let mut r = router();
        assert_eq!(
            r.route_key(Some(Key::Z), true, false, NONE, true),
            Route::Consume
        );
        assert!(r.zoom_active());
        assert_eq!(r.text("z"), Route::Consume, "the key's characters too");
        assert_eq!(
            r.route_key(Some(Key::Z), true, true, NONE, true),
            Route::Consume,
            "repeat"
        );
        assert_eq!(
            r.route_key(Some(Key::Z), false, false, NONE, true),
            Route::Consume
        );
        assert!(!r.zoom_active());
        // メニュー（カーソルは自由）では、Z はゲームのもの（チャットで打つ）。
        assert_eq!(
            r.route_key(Some(Key::Z), true, false, NONE, false),
            Route::Forward
        );
        assert_eq!(
            r.route_key(Some(Key::Z), false, false, NONE, false),
            Route::Forward
        );
        assert!(!r.zoom_active());
    }

    #[test]
    fn opening_the_ui_or_losing_focus_ends_the_zoom() {
        let mut r = router();
        r.route_key(Some(Key::Z), true, false, NONE, true);
        ctrl_i(&mut r);
        assert!(!r.zoom_active());
        ctrl_i(&mut r);
        assert!(!r.zoom_active(), "stays off after closing");
        r.route_key(Some(Key::Z), true, false, NONE, true);
        assert!(r.focus_lost(|_| true).is_empty());
        assert!(!r.zoom_active());
    }

    #[test]
    fn captured_cursor_motion_under_the_ui_does_not_turn_the_camera() {
        let mut r = router();
        assert_eq!(
            r.cursor_position(1000.0, 1000.0, true),
            Some((1000.0, 1000.0))
        );
        ctrl_i(&mut r);
        assert_eq!(r.software_cursor(), Some(pos2(400.0, 300.0)));
        assert_eq!(r.cursor_position(1050.0, 980.0, true), None);
        assert_eq!(r.software_cursor(), Some(pos2(450.0, 280.0)));
        ctrl_i(&mut r);
        // ゲームは最後に (1000, 1000) を見た：次の位置はそこから続く。
        assert_eq!(
            r.cursor_position(1060.0, 980.0, true),
            Some((1010.0, 1000.0))
        );
    }

    #[test]
    fn software_cursor_stays_inside_the_window() {
        let mut r = router();
        r.cursor_position(0.0, 0.0, true);
        ctrl_i(&mut r);
        r.cursor_position(-5000.0, 9000.0, true);
        assert_eq!(r.software_cursor(), Some(pos2(0.0, 599.0)));
    }

    #[test]
    fn grabbing_or_releasing_the_cursor_resets_the_correction() {
        let mut r = router();
        r.cursor_position(100.0, 100.0, true);
        ctrl_i(&mut r);
        r.cursor_position(200.0, 100.0, true);
        ctrl_i(&mut r);
        // ゲームがメニューを開く：カーソルは自由で、位置はそのまま通る。
        assert_eq!(r.cursor_position(300.0, 300.0, false), Some((300.0, 300.0)));
        // ゲームに戻った：ゲームは中央に戻し、直すものは残っていない。
        assert_eq!(r.cursor_position(400.0, 300.0, true), Some((400.0, 300.0)));
    }

    #[test]
    fn free_cursor_positions_drive_the_ui_pointer_in_points() {
        let mut r = router();
        r.set_screen([800, 600], 2.0);
        r.cursor_position(0.0, 0.0, false);
        ctrl_i(&mut r);
        r.take_events();
        assert_eq!(r.cursor_position(100.0, 50.0, false), None);
        assert_eq!(r.take_events(), vec![Event::PointerMoved(pos2(50.0, 25.0))]);
        assert_eq!(r.software_cursor(), None, "the real cursor is visible");
    }

    #[test]
    fn the_menu_opens_where_the_free_cursor_is() {
        let mut r = router();
        r.cursor_position(100.0, 100.0, false);
        ctrl_i(&mut r);
        ctrl_i(&mut r);
        // ゲームの画面が開いている：メニューのポインターは本物のカーソルについていく。
        r.cursor_position(120.0, 80.0, false);
        r.take_events();
        r.route_key(Some(Key::I), true, false, CTRL, false);
        assert!(r.ui_open());
        assert_eq!(r.software_cursor(), None, "the real cursor is visible");
        assert!(
            r.take_events()
                .contains(&Event::PointerMoved(pos2(120.0, 80.0)))
        );
        r.route_key(Some(Key::I), true, false, CTRL, false);
        // SDL3 は、自由なカーソルの位置も動きと一緒に報告する。
        r.cursor_motion(1.0, 1.0, 300.0, 200.0, false);
        r.take_events();
        r.route_key(Some(Key::I), true, false, CTRL, false);
        assert!(
            r.take_events()
                .contains(&Event::PointerMoved(pos2(300.0, 200.0)))
        );
    }

    #[test]
    fn the_menu_key_tells_that_the_game_took_the_cursor() {
        let mut r = router();
        // 最後の動きは画面（チャット）の上で、その画面はその後、動きなしに閉じた。
        r.cursor_position(120.0, 80.0, false);
        ctrl_i(&mut r);
        // ゲームがカーソルを隠す：メニューは中央から自分のものを描く。
        assert_eq!(r.software_cursor(), Some(pos2(400.0, 300.0)));
    }

    #[test]
    fn relative_motion_moves_the_software_cursor() {
        let mut r = router();
        assert_eq!(r.cursor_motion(5.0, 5.0, 0.0, 0.0, true), Route::Forward);
        ctrl_i(&mut r);
        assert_eq!(r.cursor_motion(10.0, -20.0, 0.0, 0.0, true), Route::Consume);
        assert_eq!(r.software_cursor(), Some(pos2(410.0, 280.0)));
    }

    #[test]
    fn pointer_speed_scales_captured_motion_only() {
        let mut r = router();
        r.set_pointer_speed(PointerSpeed::Linear(0.5));
        ctrl_i(&mut r);
        r.cursor_motion(10.0, -20.0, 0.0, 0.0, true);
        assert_eq!(r.software_cursor(), Some(pos2(405.0, 290.0)));
        // 自由なカーソルはシステム自身のポインターで、もうシステムの速度になっている。
        r.cursor_motion(0.0, 0.0, 100.0, 50.0, false);
        r.take_events();
        r.cursor_motion(10.0, 10.0, 110.0, 60.0, false);
        assert_eq!(
            r.take_events(),
            vec![Event::PointerMoved(pos2(110.0, 60.0))]
        );
    }

    #[test]
    fn pointer_speed_does_not_change_the_camera_correction() {
        let mut r = router();
        r.set_pointer_speed(PointerSpeed::Linear(2.0));
        r.cursor_position(100.0, 100.0, true);
        ctrl_i(&mut r);
        r.cursor_position(110.0, 100.0, true);
        assert_eq!(r.software_cursor(), Some(pos2(420.0, 300.0)));
        ctrl_i(&mut r);
        // UI が取った分の動きは、ゲームにはやはり見えない。
        assert_eq!(r.cursor_position(111.0, 100.0, true), Some((101.0, 100.0)));
    }

    #[test]
    fn rebound_hotkeys_replace_the_defaults() {
        let mut r = router();
        let menu = Hotkey::parse("F6").unwrap();
        let zoom = Hotkey::parse("C").unwrap();
        r.set_hotkeys(Hotkeys {
            menu,
            zoom,
            ..Hotkeys::DEFAULT
        });
        assert_eq!(ctrl_i(&mut r), Route::Forward, "Ctrl+I is the game's again");
        assert!(!r.ui_open());
        assert_eq!(
            r.route_key(Some(Key::Z), true, false, NONE, true),
            Route::Forward
        );
        assert_eq!(
            r.route_key(Some(Key::C), true, false, NONE, true),
            Route::Consume
        );
        assert!(r.zoom_active());
        assert_eq!(
            r.route_key(Some(Key::C), false, false, NONE, true),
            Route::Consume
        );
        assert_eq!(
            r.route_key(Some(Key::F6), true, false, NONE, true),
            Route::Consume
        );
        assert!(r.ui_open());
    }

    #[test]
    fn zoom_starts_with_extra_modifiers_held() {
        let mut r = router();
        // Ctrl を押したままダッシュ。
        assert_eq!(
            r.route_key(Some(Key::Z), true, false, CTRL, true),
            Route::Consume
        );
        assert!(r.zoom_active());
    }

    #[test]
    fn zoom_on_a_mouse_button() {
        let mut r = router();
        r.set_hotkeys(Hotkeys {
            zoom: Hotkey::plain(Trigger::Mouse(PointerButton::Extra1)),
            ..Hotkeys::DEFAULT
        });
        // ゲーム中ではない（ゲームのメニューが開いている）：ゲームがボタンを受け取る。
        assert_eq!(
            r.route_button(PointerButton::Extra1, true, false),
            Route::Forward
        );
        assert!(!r.zoom_active());
        r.route_button(PointerButton::Extra1, false, false);
        assert_eq!(
            r.route_button(PointerButton::Extra1, true, true),
            Route::Consume
        );
        assert!(r.zoom_active());
        assert_eq!(
            r.route_button(PointerButton::Extra1, false, true),
            Route::Consume
        );
        assert!(!r.zoom_active());
        assert_eq!(
            r.route_button(PointerButton::Extra2, true, true),
            Route::Forward
        );
    }

    #[test]
    fn menu_on_a_mouse_button() {
        let mut r = router();
        r.set_hotkeys(Hotkeys {
            menu: Hotkey::plain(Trigger::Mouse(PointerButton::Middle)),
            ..Hotkeys::DEFAULT
        });
        assert_eq!(
            r.route_button(PointerButton::Middle, true, true),
            Route::Consume
        );
        assert!(r.ui_open());
        assert_eq!(
            r.route_button(PointerButton::Middle, true, true),
            Route::Consume
        );
        assert!(!r.ui_open());
    }

    #[test]
    fn capture_takes_the_next_key_with_its_modifiers() {
        let mut r = router();
        ctrl_i(&mut r);
        r.take_events();
        r.start_capture();
        // 修飾キーだけ（名前のないキー）なら待ち続ける。
        assert_eq!(r.route_key(None, true, false, CTRL, true), Route::Consume);
        assert_eq!(r.take_captured(), None);
        // メニューのホットキーそのものも、メニューを閉じずに取れる。
        assert_eq!(ctrl_i(&mut r), Route::Consume);
        assert!(r.ui_open());
        assert_eq!(
            r.take_captured(),
            Some(Captured::Hotkey(Hotkey::DEFAULT_MENU))
        );
        assert!(r.take_events().is_empty(), "egui does not see the key");
        // 割り当て待ちは終わった：次の Ctrl+I はメニューを閉じる。
        ctrl_i(&mut r);
        assert!(!r.ui_open());
    }

    #[test]
    fn capture_takes_extra_mouse_buttons_but_not_clicks() {
        let mut r = router();
        ctrl_i(&mut r);
        // Ctrl と I を離した。
        r.route_key(Some(Key::I), false, false, NONE, true);
        r.start_capture();
        assert_eq!(
            r.route_button(PointerButton::Primary, true, true),
            Route::Consume
        );
        assert_eq!(r.take_captured(), None, "a click still works the UI");
        assert_eq!(
            r.route_button(PointerButton::Extra2, true, true),
            Route::Consume
        );
        assert_eq!(
            r.take_captured(),
            Some(Captured::Hotkey(Hotkey::plain(Trigger::Mouse(
                PointerButton::Extra2
            ))))
        );
    }

    #[test]
    fn capture_is_cancelled_by_escape_or_closing() {
        let mut r = router();
        ctrl_i(&mut r);
        r.start_capture();
        assert_eq!(
            r.route_key(Some(Key::Escape), true, false, NONE, true),
            Route::Consume
        );
        assert_eq!(r.take_captured(), Some(Captured::Cancelled));
        assert!(r.ui_open(), "Escape cancelled the capture, not the menu");
        r.start_capture();
        r.set_ui_open(false);
        assert_eq!(r.take_captured(), Some(Captured::Cancelled));
        // 割り当て待ちには UI が要る。
        r.start_capture();
        assert_eq!(
            r.route_key(Some(Key::Z), true, false, NONE, true),
            Route::Consume
        );
        assert_eq!(r.take_captured(), None);
    }

    #[test]
    fn waypoint_and_navigate_keys_queue_actions_in_game() {
        let mut r = router();
        assert_eq!(
            r.route_key(Some(Key::J), true, false, NONE, true),
            Route::Consume
        );
        assert_eq!(
            r.route_key(Some(Key::J), true, true, NONE, true),
            Route::Consume,
            "repeat"
        );
        assert_eq!(r.text("j"), Route::Consume, "the key's characters too");
        assert_eq!(
            r.route_key(Some(Key::J), false, false, NONE, true),
            Route::Consume
        );
        // Ctrl を押したままダッシュ。
        assert_eq!(
            r.route_key(Some(Key::K), true, false, CTRL, true),
            Route::Consume
        );
        r.route_key(Some(Key::K), false, false, NONE, true);
        assert_eq!(
            r.take_actions(),
            vec![HotkeyAction::RecordWaypoint, HotkeyAction::Navigate]
        );
        assert!(r.take_actions().is_empty());
        assert!(r.take_events().is_empty());
        assert!(!r.zoom_active());
    }

    #[test]
    fn waypoint_keys_are_the_games_outside_play_and_with_the_ui_open() {
        let mut r = router();
        // ゲームの画面が開いている（チャットで "j" を打つ）。
        assert_eq!(
            r.route_key(Some(Key::J), true, false, NONE, false),
            Route::Forward
        );
        assert_eq!(
            r.route_key(Some(Key::J), true, true, NONE, false),
            Route::Forward,
            "repeat"
        );
        assert_eq!(r.text("j"), Route::Forward);
        assert_eq!(
            r.route_key(Some(Key::J), false, false, NONE, false),
            Route::Forward
        );
        // こちらのメニューは、代わりに egui のために押下を取る。
        ctrl_i(&mut r);
        assert_eq!(
            r.route_key(Some(Key::K), true, false, NONE, true),
            Route::Consume
        );
        assert!(r.take_actions().is_empty());
    }

    #[test]
    fn a_release_is_consumed_only_after_a_consumed_press() {
        let mut r = router();
        // ゲームの画面で押し、ゲーム中に離した：ゲームは押下を見た。
        r.route_key(Some(Key::J), true, false, NONE, false);
        assert_eq!(
            r.route_key(Some(Key::J), false, false, NONE, true),
            Route::Forward
        );
        // ゲーム中に押し、ゲームの画面が開いた後に離した：まだこちらのもの。
        r.route_key(Some(Key::J), true, false, NONE, true);
        assert_eq!(
            r.route_key(Some(Key::J), false, false, NONE, false),
            Route::Consume
        );
        // メニューを開くと押下を忘れる。その解放は、押下を受け取っていないゲームには渡さない（ホット
        // キーとしての F3 は、デバッグの画面を切り替えてしまう）。
        r.route_key(Some(Key::J), true, false, NONE, true);
        ctrl_i(&mut r);
        ctrl_i(&mut r);
        assert_eq!(
            r.route_key(Some(Key::J), false, false, NONE, true),
            Route::Consume
        );
        assert_eq!(
            r.route_key(Some(Key::J), false, false, NONE, true),
            Route::Forward,
            "once"
        );
        assert_eq!(r.take_actions().len(), 2);
    }

    #[test]
    fn debug_modifier_hands_the_in_game_hotkeys_to_the_game() {
        let mut r = router();
        assert_eq!(
            r.route_key(Some(Key::F3), true, false, NONE, true),
            Route::Forward,
            "the modifier itself is the game's"
        );
        for key in [Key::J, Key::K, Key::Z] {
            assert_eq!(
                r.route_key(Some(key), true, false, NONE, true),
                Route::Forward,
                "{key:?}"
            );
            assert_eq!(
                r.route_key(Some(key), false, false, NONE, true),
                Route::Forward,
                "{key:?}"
            );
        }
        assert!(r.take_actions().is_empty());
        assert!(!r.zoom_active());
        // メニューのキーはまだ働く。
        assert_eq!(ctrl_i(&mut r), Route::Consume);
        ctrl_i(&mut r);
        r.route_key(Some(Key::F3), false, false, NONE, true);
        assert_eq!(
            r.route_key(Some(Key::J), true, false, NONE, true),
            Route::Consume
        );
        assert_eq!(r.take_actions(), vec![HotkeyAction::RecordWaypoint]);
    }

    #[test]
    fn keys_taken_before_the_debug_modifier_are_released_by_us() {
        let mut r = router();
        r.route_key(Some(Key::Z), true, false, NONE, true);
        r.route_key(Some(Key::J), true, false, NONE, true);
        r.route_key(Some(Key::F3), true, false, NONE, true);
        assert!(r.zoom_active(), "a zoom already held goes on");
        assert_eq!(
            r.route_key(Some(Key::Z), true, true, NONE, true),
            Route::Consume
        );
        assert_eq!(
            r.route_key(Some(Key::J), true, true, NONE, true),
            Route::Consume
        );
        assert_eq!(
            r.route_key(Some(Key::Z), false, false, NONE, true),
            Route::Consume
        );
        assert!(!r.zoom_active());
        assert_eq!(
            r.route_key(Some(Key::J), false, false, NONE, true),
            Route::Consume
        );
        // 新しいズームの押下は、F3 の解放を待つ。
        assert_eq!(
            r.route_key(Some(Key::Z), true, false, NONE, true),
            Route::Forward
        );
        assert!(!r.zoom_active());
    }

    #[test]
    fn the_debug_modifier_can_be_rebound_or_unbound() {
        let mut r = router();
        r.set_debug_modifier(raw(Key::F6));
        r.route_key(Some(Key::F3), true, false, NONE, true);
        assert_eq!(
            r.route_key(Some(Key::J), true, false, NONE, true),
            Route::Consume
        );
        r.route_key(Some(Key::J), false, false, NONE, true);
        r.route_key(Some(Key::F6), true, false, NONE, true);
        assert_eq!(
            r.route_key(Some(Key::J), true, false, NONE, true),
            Route::Forward
        );
        r.route_key(Some(Key::J), false, false, NONE, true);
        // キーを変えると、押している状態を忘れる。
        r.set_debug_modifier(None);
        assert_eq!(
            r.route_key(Some(Key::K), true, false, NONE, true),
            Route::Consume
        );
        assert_eq!(
            r.take_actions(),
            vec![HotkeyAction::RecordWaypoint, HotkeyAction::Navigate]
        );
    }

    #[test]
    fn debug_modifier_state_resets_on_focus_loss_and_disable() {
        let mut r = router();
        r.route_key(Some(Key::F3), true, false, NONE, true);
        assert!(r.focus_lost(|_| true).is_empty());
        assert_eq!(
            r.route_key(Some(Key::J), true, false, NONE, true),
            Route::Consume
        );
        r.route_key(Some(Key::J), false, false, NONE, true);
        r.route_key(Some(Key::F3), true, false, NONE, true);
        r.set_enabled(false);
        r.set_enabled(true);
        assert_eq!(
            r.route_key(Some(Key::J), true, false, NONE, true),
            Route::Consume
        );
        // オフの間も追う。
        r.set_enabled(false);
        r.route_key(Some(Key::F3), true, false, NONE, true);
        r.set_enabled(true);
        assert_eq!(
            r.route_key(Some(Key::K), true, false, NONE, true),
            Route::Forward
        );
    }

    #[test]
    fn at_most_four_actions_wait() {
        let mut r = router();
        for _ in 0..6 {
            r.route_key(Some(Key::J), true, false, NONE, true);
            r.route_key(Some(Key::J), false, false, NONE, true);
        }
        assert_eq!(r.take_actions().len(), 4);
        r.route_key(Some(Key::K), true, false, NONE, true);
        r.set_enabled(false);
        assert!(r.take_actions().is_empty(), "disabling drops them");
    }

    /// ゲーム中の押下とその解放。
    fn tap(r: &mut InputRouter, key: Key) -> Route {
        let route = r.route_key(Some(key), true, false, NONE, true);
        assert_eq!(r.route_key(Some(key), false, false, NONE, true), route);
        route
    }

    #[test]
    fn browser_keys_work_in_game_while_it_shows() {
        let mut r = router();
        // 隠している：矢印と Page Down はゲームのもの。
        assert_eq!(tap(&mut r, Key::PageDown), Route::Forward);
        assert_eq!(tap(&mut r, Key::ArrowDown), Route::Forward);
        assert!(r.take_browser_actions().is_empty());

        r.set_browser_shown(true);
        assert_eq!(tap(&mut r, Key::PageDown), Route::Consume);
        assert_eq!(tap(&mut r, Key::PageUp), Route::Consume);
        assert_eq!(tap(&mut r, Key::ArrowDown), Route::Consume);
        assert_eq!(tap(&mut r, Key::ArrowLeft), Route::Consume);
        assert_eq!(tap(&mut r, Key::ArrowRight), Route::Consume);
        assert_eq!(
            r.take_browser_actions(),
            vec![
                BrowserAction::PageDown,
                BrowserAction::PageUp,
                BrowserAction::PlayPause,
                BrowserAction::SeekBack,
                BrowserAction::SeekForward,
            ]
        );
        assert!(r.take_actions().is_empty(), "not the waypoint queue");

        // ゲームの画面が開いている（矢印はチャットでキャレットを動かす）。
        assert_eq!(
            r.route_key(Some(Key::ArrowLeft), true, false, NONE, false),
            Route::Forward
        );
        assert_eq!(
            r.route_key(Some(Key::ArrowLeft), false, false, NONE, false),
            Route::Forward
        );
        // F3 を押している：ゲームのもの。
        r.route_key(Some(Key::F3), true, false, NONE, true);
        assert_eq!(
            r.route_key(Some(Key::PageDown), true, false, NONE, true),
            Route::Forward
        );
        assert!(r.take_browser_actions().is_empty());
    }

    #[test]
    fn scrolling_and_seeking_repeat_while_held() {
        let mut r = router();
        r.set_browser_shown(true);
        for key in [Key::PageDown, Key::ArrowLeft, Key::ArrowDown] {
            r.route_key(Some(key), true, false, NONE, true);
            assert_eq!(
                r.route_key(Some(key), true, true, NONE, true),
                Route::Consume
            );
            r.route_key(Some(key), true, true, NONE, true);
            r.route_key(Some(key), false, false, NONE, true);
        }
        assert_eq!(
            r.take_browser_actions(),
            vec![
                BrowserAction::PageDown,
                BrowserAction::PageDown,
                BrowserAction::PageDown,
                BrowserAction::SeekBack,
                BrowserAction::SeekBack,
                BrowserAction::SeekBack,
                BrowserAction::PlayPause,
            ]
        );
    }

    #[test]
    fn hiding_the_browser_mid_press_keeps_the_release_ours() {
        let mut r = router();
        r.set_browser_shown(true);
        r.route_key(Some(Key::PageDown), true, false, NONE, true);
        r.set_browser_shown(false);
        assert_eq!(
            r.route_key(Some(Key::PageDown), false, false, NONE, true),
            Route::Consume
        );
    }

    #[test]
    fn the_browser_toggle_works_whether_it_shows_or_not() {
        let mut r = router();
        let toggle = Hotkey::parse("Ctrl+B").unwrap();
        r.set_hotkeys(Hotkeys {
            browser: crate::overlay::BrowserKeys {
                toggle: Some(toggle),
                page_down: None,
                ..Hotkeys::DEFAULT.browser
            },
            ..Hotkeys::DEFAULT
        });
        assert_eq!(
            r.route_key(Some(Key::B), true, false, CTRL, true),
            Route::Consume
        );
        assert_eq!(
            r.route_key(Some(Key::B), true, true, CTRL, true),
            Route::Consume,
            "repeats do nothing"
        );
        r.route_key(Some(Key::B), false, false, NONE, true);
        assert_eq!(r.take_browser_actions(), vec![BrowserAction::Toggle]);
        assert_eq!(
            r.route_key(Some(Key::B), true, false, NONE, true),
            Route::Forward,
            "without Ctrl"
        );
        // 割り当てのないキーは、ブラウザを出している間もゲームのもの。
        r.set_browser_shown(true);
        assert_eq!(tap(&mut r, Key::PageDown), Route::Forward);
    }

    #[test]
    fn at_most_eight_browser_actions_wait() {
        let mut r = router();
        r.set_browser_shown(true);
        r.route_key(Some(Key::PageDown), true, false, NONE, true);
        for _ in 0..20 {
            r.route_key(Some(Key::PageDown), true, true, NONE, true);
        }
        assert_eq!(r.take_browser_actions().len(), 8);
        r.route_key(Some(Key::PageDown), true, true, NONE, true);
        r.set_enabled(false);
        assert!(r.take_browser_actions().is_empty(), "disabling drops them");
    }

    #[test]
    fn waypoint_hotkeys_with_modifiers_and_mouse_buttons() {
        let mut r = router();
        r.set_hotkeys(Hotkeys {
            waypoint: Hotkey::parse("Shift+Z").unwrap(),
            navigate: Hotkey::plain(Trigger::Mouse(PointerButton::Extra2)),
            ..Hotkeys::DEFAULT
        });
        // ただの Z はまだズームする。Shift+Z は記録する。
        assert_eq!(
            r.route_key(Some(Key::Z), true, false, NONE, true),
            Route::Consume
        );
        assert!(r.zoom_active());
        r.route_key(Some(Key::Z), false, false, NONE, true);
        let shift = Modifiers::SHIFT;
        assert_eq!(
            r.route_key(Some(Key::Z), true, false, shift, true),
            Route::Consume
        );
        assert!(!r.zoom_active());
        // 先に Shift を離した：解放はまだこちらのもの。
        assert_eq!(
            r.route_key(Some(Key::Z), false, false, NONE, true),
            Route::Consume
        );
        assert_eq!(
            r.route_button(PointerButton::Extra2, true, false),
            Route::Forward
        );
        r.route_button(PointerButton::Extra2, false, false);
        assert_eq!(
            r.route_button(PointerButton::Extra2, true, true),
            Route::Consume
        );
        assert_eq!(
            r.route_button(PointerButton::Extra2, false, true),
            Route::Consume
        );
        r.route_key(Some(Key::F3), true, false, NONE, true);
        assert_eq!(
            r.route_button(PointerButton::Extra2, true, true),
            Route::Forward
        );
        assert_eq!(
            r.take_actions(),
            vec![HotkeyAction::RecordWaypoint, HotkeyAction::Navigate]
        );
    }

    #[test]
    fn a_zoom_on_the_debug_modifier_still_zooms() {
        let mut r = router();
        r.set_hotkeys(Hotkeys {
            zoom: Hotkey::plain(Trigger::Key(Key::F3)),
            ..Hotkeys::DEFAULT
        });
        assert_eq!(
            r.route_key(Some(Key::F3), true, false, NONE, true),
            Route::Consume
        );
        assert!(r.zoom_active());
        assert_eq!(
            r.route_key(Some(Key::F3), false, false, NONE, true),
            Route::Consume
        );
        assert!(!r.zoom_active());
    }

    #[test]
    fn a_repeat_never_starts_the_zoom() {
        let mut r = router();
        // F3 を押している間に Z がゲームに行った。F3 は Z より先に離す。
        r.route_key(Some(Key::F3), true, false, NONE, true);
        assert_eq!(
            r.route_key(Some(Key::Z), true, false, NONE, true),
            Route::Forward
        );
        r.route_key(Some(Key::F3), false, false, NONE, true);
        assert_eq!(
            r.route_key(Some(Key::Z), true, true, NONE, true),
            Route::Forward
        );
        assert!(!r.zoom_active());
        assert_eq!(
            r.route_key(Some(Key::Z), false, false, NONE, true),
            Route::Forward,
            "the game gets the release of the press it saw"
        );
    }

    const F3: InputId = InputId::Key(60);
    const C: InputId = InputId::Key(6);
    const Q: InputId = InputId::Key(20);
    const T: InputId = InputId::Key(23);
    const W: InputId = InputId::Key(26);
    const X: InputId = InputId::Key(27);
    const LCTRL: InputId = InputId::Key(224);
    const MOUSE4: InputId = InputId::Mouse(4);

    fn send(id: InputId, phase: Phase) -> Delivery {
        Delivery::Send(Output { id, phase })
    }

    /// `key` の生の id のキーのイベント。
    fn key(r: &mut InputRouter, key: Key, pressed: bool, repeat: bool, captured: bool) -> Delivery {
        r.key(Some(key), raw(key), pressed, repeat, NONE, captured, false)
    }

    fn mouse4(r: &mut InputRouter, pressed: bool) -> Delivery {
        r.button(PointerButton::Extra1, Some(MOUSE4), pressed, true)
    }

    #[test]
    fn rebound_presses_reach_the_game_as_their_output() {
        let mut r = router();
        r.set_rebinds(vec![(X, Q)], true);
        assert_eq!(
            key(&mut r, Key::X, true, false, true),
            send(Q, Phase::Press)
        );
        assert_eq!(
            key(&mut r, Key::X, true, true, true),
            send(Q, Phase::Repeat)
        );
        assert_eq!(
            key(&mut r, Key::X, false, false, true),
            send(Q, Phase::Release)
        );
        // ゲームの画面（チャット、インベントリ）では、キーはそのままゲームのもの。
        assert_eq!(key(&mut r, Key::X, true, false, false), Delivery::Forward);
        assert_eq!(key(&mut r, Key::X, false, false, true), Delivery::Forward);
        assert_eq!(r.rebinds(), [(X, Q)]);
    }

    #[test]
    fn a_rebound_debug_modifier_hands_the_hotkeys_to_the_game() {
        let mut r = router();
        r.set_rebinds(vec![(MOUSE4, F3)], true);
        assert_eq!(mouse4(&mut r, true), send(F3, Phase::Press));
        // ゲームが F3 を下ろしている：J は F3+J。
        assert_eq!(key(&mut r, Key::J, true, false, true), Delivery::Forward);
        assert_eq!(key(&mut r, Key::J, false, false, true), Delivery::Forward);
        assert_eq!(mouse4(&mut r, false), send(F3, Phase::Release));
        assert_eq!(key(&mut r, Key::J, true, false, true), Delivery::Consume);
        // F3 を別のキーに置き換えた：ゲームは F3 を見ず、J はこちらのまま。
        let mut r = router();
        r.set_rebinds(vec![(F3, C)], true);
        assert_eq!(
            key(&mut r, Key::F3, true, false, true),
            send(C, Phase::Press)
        );
        assert_eq!(key(&mut r, Key::J, true, false, true), Delivery::Consume);
        assert_eq!(r.take_actions(), [HotkeyAction::RecordWaypoint]);
    }

    #[test]
    fn the_debug_modifier_can_be_a_mouse_button() {
        let mut r = router();
        r.set_debug_modifier(Some(MOUSE4));
        assert_eq!(mouse4(&mut r, true), Delivery::Forward);
        assert_eq!(key(&mut r, Key::K, true, false, true), Delivery::Forward);
        mouse4(&mut r, false);
        assert_eq!(key(&mut r, Key::K, true, false, true), Delivery::Consume);
    }

    #[test]
    fn hotkeys_and_the_menu_go_by_the_physical_key() {
        let mut r = router();
        r.set_rebinds(
            vec![(raw(Key::Z).unwrap(), C), (raw(Key::I).unwrap(), X)],
            true,
        );
        // ズームは、どのルールより先に Z を取る。
        assert_eq!(key(&mut r, Key::Z, true, false, true), Delivery::Consume);
        assert!(r.zoom_active());
        assert_eq!(key(&mut r, Key::Z, false, false, true), Delivery::Consume);
        // ただの I は置き換える。Ctrl+I はメニューを開く。
        assert_eq!(
            key(&mut r, Key::I, true, false, true),
            send(X, Phase::Press)
        );
        assert_eq!(
            key(&mut r, Key::I, false, false, true),
            send(X, Phase::Release)
        );
        assert_eq!(ctrl_i(&mut r), Route::Consume);
        assert!(r.ui_open());
        // メニューを開いている間は何も置き換えない。解放はやはりゲームに届く。
        assert_eq!(key(&mut r, Key::I, true, false, true), Delivery::Consume);
        assert_eq!(key(&mut r, Key::I, false, false, true), Delivery::Forward);
    }

    #[test]
    fn a_held_rebind_survives_the_menu_without_repeating() {
        let mut r = router();
        r.set_rebinds(vec![(X, Q)], true);
        assert_eq!(
            key(&mut r, Key::X, true, false, true),
            send(Q, Phase::Press)
        );
        r.set_ui_open(true);
        // メニューを開いている間に、Q が何度も捨てることはない。
        assert_eq!(key(&mut r, Key::X, true, true, true), Delivery::Consume);
        assert_eq!(r.text("x"), Route::Consume, "nor typed into the menu");
        r.set_enabled(false);
        assert_eq!(
            key(&mut r, Key::X, false, false, true),
            send(Q, Phase::Release),
            "the release is Q's whatever happened since"
        );
    }

    #[test]
    fn the_characters_of_a_rebound_key_are_dropped() {
        let mut r = router();
        r.set_rebinds(vec![(X, T)], true);
        // X → T はチャットを開く。そこに x を打ってはならない。
        assert_eq!(
            key(&mut r, Key::X, true, false, true),
            send(T, Phase::Press)
        );
        assert_eq!(r.text("x"), Route::Consume);
        assert_eq!(r.text("a"), Route::Forward, "only once");
        // チャットを開いている間のオートリピート（カーソルは自由）。
        assert_eq!(key(&mut r, Key::X, true, true, false), Delivery::Consume);
        assert_eq!(r.text("x"), Route::Consume);
        assert_eq!(
            key(&mut r, Key::X, false, false, false),
            send(T, Phase::Release)
        );
        assert_eq!(key(&mut r, Key::A, true, false, false), Delivery::Forward);
        assert_eq!(r.text("a"), Route::Forward);
        // マウスの元のキーは何も打たない。
        r.set_rebinds(vec![(MOUSE4, T)], true);
        mouse4(&mut r, true);
        assert_eq!(r.text("t"), Route::Forward);
    }

    #[test]
    fn focus_loss_returns_the_releases_of_held_outputs() {
        let mut r = router();
        r.set_rebinds(vec![(MOUSE4, F3), (X, Q)], true);
        assert_eq!(key(&mut r, Key::W, true, false, true), Delivery::Forward);
        mouse4(&mut r, true);
        key(&mut r, Key::X, true, false, true);
        // SDL3：フォーカスを失った後に、キーは解放を受け取り、マウスのボタンは受け取らない。
        let mut releases = r.focus_lost(|id| matches!(id, InputId::Key(_)));
        releases.sort_by_key(|o| o.id);
        let release = |id| Output {
            id,
            phase: Phase::Release,
        };
        assert_eq!(releases, [release(Q), release(F3)]);
        // SDL 自身の解放：W のものはゲームのもの。X のものは、もう Q のものとして渡した。
        assert_eq!(key(&mut r, Key::W, false, false, false), Delivery::Forward);
        assert_eq!(key(&mut r, Key::X, false, false, false), Delivery::Consume);
        assert_eq!(r.rebind_state(), RebindState::default());
        // ゲームに戻ると、ボタンはすぐに働く。
        assert_eq!(mouse4(&mut r, true), send(F3, Phase::Press));
        assert_eq!(
            key(&mut r, Key::J, true, false, true),
            Delivery::Forward,
            "F3+J"
        );
    }

    #[test]
    fn keyboard_sources_can_be_left_out() {
        let mut r = router();
        r.set_rebinds(vec![(X, Q), (MOUSE4, F3)], false);
        assert_eq!(r.rebinds(), [(MOUSE4, F3)]);
        assert_eq!(key(&mut r, Key::X, true, false, true), Delivery::Forward);
    }

    #[test]
    fn a_stateless_key_repeats_with_presses() {
        // GLFW での ろ：key -1、自動のリピートは押下として来る、egui の名前はない。
        let ro = InputId::Key(135);
        let mut r = router();
        r.set_rebinds(vec![(ro, W)], true);
        let press = |r: &mut InputRouter, pressed: bool| {
            r.key(None, Some(ro), pressed, false, NONE, true, true)
        };
        assert_eq!(press(&mut r, true), send(W, Phase::Press));
        assert_eq!(press(&mut r, true), send(W, Phase::Repeat));
        assert_eq!(press(&mut r, false), send(W, Phase::Release));
    }

    #[test]
    fn release_source_lets_go_of_the_output() {
        let mut r = router();
        r.set_rebinds(vec![(MOUSE4, F3)], true);
        mouse4(&mut r, true);
        assert_eq!(r.rebind_state().held, [(MOUSE4, F3)]);
        assert_eq!(
            r.release_source(MOUSE4),
            Some(Output {
                id: F3,
                phase: Phase::Release
            })
        );
        // ゲームが F3 を離した：J はまたこちらのもの。
        assert_eq!(key(&mut r, Key::J, true, false, true), Delivery::Consume);
        assert_eq!(mouse4(&mut r, false), Delivery::Forward);
    }

    #[test]
    fn input_capture_takes_any_key_and_some_buttons() {
        let mut r = router();
        ctrl_i(&mut r);
        r.key(Some(Key::I), raw(Key::I), false, false, NONE, true, false);
        r.take_events();
        r.start_input_capture();
        // 左と右のクリックは UI を操作する。
        assert_eq!(
            r.route_button(PointerButton::Primary, true, true),
            Route::Consume
        );
        assert_eq!(r.take_captured(), None);
        assert_eq!(r.take_events().len(), 1);
        // 修飾キーだけでも、ほかのキーと同じキー。それだけで上がったときに取る。
        let ctrl = r.key(None, Some(LCTRL), true, false, CTRL, true, false);
        assert_eq!(ctrl, Delivery::Consume);
        assert_eq!(r.take_captured(), None);
        assert!(r.take_events().is_empty(), "egui does not see it");
        let ctrl = r.key(None, Some(LCTRL), false, false, NONE, true, false);
        assert_eq!(
            ctrl,
            Delivery::Consume,
            "its release is not the game's either"
        );
        assert_eq!(r.take_captured(), Some(Captured::Input(LCTRL)));
        let ctrl = r.key(None, Some(LCTRL), false, false, NONE, true, false);
        assert_eq!(ctrl, Delivery::Forward, "capture is over");
        r.start_input_capture();
        assert_eq!(
            r.route_button(PointerButton::Extra1, true, true),
            Route::Consume
        );
        assert_eq!(r.take_captured(), Some(Captured::Input(MOUSE4)));
        assert_eq!(
            r.route_button(PointerButton::Extra1, false, true),
            Route::Consume
        );
        // リピートは新しい押下ではない。Esc でやめる。
        r.start_input_capture();
        assert_eq!(key(&mut r, Key::Q, true, true, true), Delivery::Consume);
        assert_eq!(r.take_captured(), None);
        assert_eq!(
            key(&mut r, Key::Escape, true, false, true),
            Delivery::Consume
        );
        assert_eq!(r.take_captured(), Some(Captured::Cancelled));
        assert!(r.ui_open(), "Esc cancelled the capture, not the menu");
        // その後のホットキーの割り当て待ちは、また egui のキーを取る。
        r.start_capture();
        r.key(None, Some(LCTRL), true, false, CTRL, true, false);
        assert_eq!(r.take_captured(), None, "a modifier alone is no hotkey");
    }

    #[test]
    fn ctrl_i_while_waiting_for_a_key_closes_the_menu_without_taking_ctrl() {
        let mut r = router();
        ctrl_i(&mut r);
        r.key(Some(Key::I), raw(Key::I), false, false, NONE, true, false);
        r.start_input_capture();
        assert_eq!(
            r.key(None, Some(LCTRL), true, false, CTRL, true, false),
            Delivery::Consume
        );
        assert_eq!(
            r.key(Some(Key::I), raw(Key::I), true, false, CTRL, true, false),
            Delivery::Consume
        );
        assert!(!r.ui_open());
        assert_eq!(r.take_captured(), Some(Captured::Cancelled));
        assert_eq!(
            r.key(Some(Key::I), raw(Key::I), false, false, CTRL, true, false),
            Delivery::Consume,
            "the menu key's release is not the game's"
        );
        r.key(None, Some(LCTRL), false, false, NONE, true, false);
        assert_eq!(r.take_captured(), None, "nothing is taken afterwards");

        // メニューが閉じるフレームで取ったキー（キー、次に Esc）も捨てる。
        ctrl_i(&mut r);
        r.start_input_capture();
        key(&mut r, Key::B, true, false, true);
        key(&mut r, Key::Escape, true, false, true);
        assert!(!r.ui_open());
        assert_eq!(r.take_captured(), Some(Captured::Cancelled));
    }

    #[test]
    fn the_ui_gets_copy_cut_and_the_clipboard_on_paste() {
        let mut r = router();
        ctrl_i(&mut r);
        r.take_events();
        r.route_key(Some(Key::V), true, false, CTRL, true);
        assert!(r.take_paste_request());
        assert!(!r.take_paste_request(), "once");
        r.paste("https://example.com/\r\nnext");
        r.route_key(Some(Key::C), true, false, CTRL, true);
        r.route_key(Some(Key::X), true, false, CTRL, true);
        let events: Vec<_> = r
            .take_events()
            .into_iter()
            .filter(|e| !matches!(e, Event::Key { .. }))
            .collect();
        assert_eq!(
            events,
            vec![
                Event::Paste("https://example.com/  next".into()),
                Event::Copy,
                Event::Cut
            ]
        );
        // メニューを閉じている間は違う。
        ctrl_i(&mut r);
        r.take_events();
        r.paste("text");
        assert!(
            r.take_events()
                .iter()
                .all(|e| !matches!(e, Event::Paste(_)))
        );
    }

    #[test]
    fn a_menu_key_that_types_text_types_it_where_text_goes() {
        let mut r = router();
        r.set_hotkeys(Hotkeys {
            menu: Hotkey::parse("M").unwrap(),
            ..Hotkeys::DEFAULT
        });
        // ゲームの画面（チャット）：ゲームが受け取る。
        assert_eq!(
            r.route_key(Some(Key::M), true, false, NONE, false),
            Route::Forward
        );
        assert!(!r.ui_open());
        // ゲーム中はメニューを開く。
        assert_eq!(
            r.route_key(Some(Key::M), true, false, NONE, true),
            Route::Consume
        );
        assert!(r.ui_open());
        r.route_key(Some(Key::M), false, false, NONE, true);
        // 入力欄にフォーカスがあれば、そこに打つ。
        r.set_text_focus(true);
        r.take_events();
        assert_eq!(
            r.route_key(Some(Key::M), true, false, NONE, true),
            Route::Consume
        );
        assert!(r.ui_open());
        assert!(
            r.take_events()
                .iter()
                .any(|e| matches!(e, Event::Key { key: Key::M, .. }))
        );
        r.set_text_focus(false);
        r.route_key(Some(Key::M), true, false, NONE, true);
        assert!(!r.ui_open());
    }

    #[test]
    fn hotkey_presses_whose_release_never_came_are_forgotten() {
        let mut r = router();
        assert_eq!(key(&mut r, Key::Z, true, false, true), Delivery::Consume);
        assert!(r.zoom_active());
        let z = raw(Key::Z).unwrap();
        assert_eq!(r.held_presses(), vec![z]);
        // ホットキー自身の文字は消えるが、それだけ。
        assert_eq!(r.text("z"), Route::Consume);
        assert_eq!(r.text("t"), Route::Forward);
        r.forget_press(z);
        assert!(!r.zoom_active());
        assert!(r.held_presses().is_empty());
    }

    #[test]
    fn the_release_of_a_captured_key_stays_out_of_the_game() {
        let mut r = router();
        ctrl_i(&mut r);
        r.key(Some(Key::I), raw(Key::I), false, false, NONE, true, false);
        // ルールのための F3：その解放だけで、ゲームのデバッグの画面が切り替わってしまう。
        r.start_input_capture();
        assert_eq!(key(&mut r, Key::F3, true, false, true), Delivery::Consume);
        assert_eq!(r.take_captured(), Some(Captured::Input(F3)));
        assert_eq!(key(&mut r, Key::F3, true, true, true), Delivery::Consume);
        assert_eq!(key(&mut r, Key::F3, false, false, true), Delivery::Consume);
        // 間にメニューが閉じた後も。
        r.start_capture();
        assert_eq!(key(&mut r, Key::F6, true, false, true), Delivery::Consume);
        assert!(matches!(r.take_captured(), Some(Captured::Hotkey(_))));
        ctrl_i(&mut r);
        assert!(!r.ui_open());
        assert_eq!(key(&mut r, Key::F6, false, false, true), Delivery::Consume);
        assert_eq!(key(&mut r, Key::F6, true, false, true), Delivery::Forward);
        assert_eq!(key(&mut r, Key::F6, false, false, true), Delivery::Forward);
        // 来なかった解放を、次の押下を超えて待つことはない。
        ctrl_i(&mut r);
        r.start_input_capture();
        assert_eq!(mouse4(&mut r, true), Delivery::Consume);
        assert_eq!(r.take_captured(), Some(Captured::Input(MOUSE4)));
        ctrl_i(&mut r);
        assert_eq!(mouse4(&mut r, true), Delivery::Forward);
        assert_eq!(mouse4(&mut r, false), Delivery::Forward);
    }

    #[test]
    fn a_captured_key_held_past_the_menu_never_half_reaches_the_game() {
        // W を取り、メニューを閉じたときにまだ押している：そのオートリピートもゲームに届いてはならない。
        // そうしないと、ゲームは解放なしで W を下ろしてしまう。
        let mut r = router();
        ctrl_i(&mut r);
        r.start_input_capture();
        assert_eq!(key(&mut r, Key::W, true, false, true), Delivery::Consume);
        assert_eq!(
            r.take_captured(),
            Some(Captured::Input(raw(Key::W).unwrap()))
        );
        r.set_ui_open(false);
        assert_eq!(key(&mut r, Key::W, true, true, true), Delivery::Consume);
        assert_eq!(key(&mut r, Key::W, false, false, true), Delivery::Consume);
        assert_eq!(key(&mut r, Key::W, true, false, true), Delivery::Forward);
        assert_eq!(key(&mut r, Key::W, false, false, true), Delivery::Forward);

        // 対象を取る間も F3 を押している：どちらの解放も渡さない。
        ctrl_i(&mut r);
        r.start_input_capture();
        assert_eq!(key(&mut r, Key::F3, true, false, true), Delivery::Consume);
        r.take_captured();
        r.start_input_capture();
        assert_eq!(key(&mut r, Key::B, true, false, true), Delivery::Consume);
        r.take_captured();
        assert_eq!(key(&mut r, Key::B, false, false, true), Delivery::Consume);
        assert_eq!(key(&mut r, Key::F3, false, false, true), Delivery::Consume);
    }

    #[test]
    fn a_release_of_a_key_a_rule_holds_waits_for_the_rule() {
        // CapsLock → 左の Ctrl を押したまま、Ctrl+I を 2 回：メニューが 2 回目の Ctrl の押下を取った。
        const CAPS: InputId = InputId::Key(57);
        let mut r = router();
        r.set_rebinds(vec![(CAPS, LCTRL)], true);
        let caps = |r: &mut InputRouter, pressed| {
            r.key(None, Some(CAPS), pressed, false, NONE, true, false)
        };
        let ctrl = |r: &mut InputRouter, pressed| {
            r.key(None, Some(LCTRL), pressed, false, CTRL, true, false)
        };
        assert_eq!(caps(&mut r, true), send(LCTRL, Phase::Press));
        assert_eq!(
            ctrl(&mut r, true),
            Delivery::Consume,
            "shares the rule's press"
        );
        assert_eq!(ctrl_i(&mut r), Route::Consume);
        assert!(r.ui_open());
        assert_eq!(ctrl(&mut r, false), Delivery::Consume);
        assert_eq!(ctrl(&mut r, true), Delivery::Consume, "the menu's");
        assert_eq!(ctrl_i(&mut r), Route::Consume);
        assert!(!r.ui_open());
        // CapsLock を押している間、ゲームは左の Ctrl を下ろしたまま。
        assert_eq!(ctrl(&mut r, false), Delivery::Consume);
        assert!(r.rebind_state().holds(LCTRL));
        assert_eq!(caps(&mut r, false), send(LCTRL, Phase::Release));
        assert_eq!(r.rebind_state(), RebindState::default());
    }
}
