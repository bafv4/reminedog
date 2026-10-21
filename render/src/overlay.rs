use std::borrow::Cow;
use std::fmt;
use std::sync::Arc;

use egui::{
    Color32, FontData, FontDefinitions, FontFamily, Modifiers, Pos2, Rect, RichText, ViewportId,
    vec2,
};
use glow::HasContext as _;

use reminedog_core::settings::ZOOM_FACTOR_RANGE;
use reminedog_core::{InputId, Settings, input_label, mapping_label};

use crate::browser::{
    BrowserCommand, BrowserMenu, BrowserPixels, BrowserView, PageTexture, browser_section,
    browser_ui,
};
use crate::font_metrics;
use crate::hotkey::{Hotkey, Trigger, input_trigger};
use crate::input::{BrowserAction, Captured};
use crate::rebinds_ui::{RebindMenu, rebind_section, rule_sources};
use crate::waypoints::{
    Notice, NoticeList, WaypointCommand, WaypointMenu, WaypointView, draw_destination,
    draw_notices, waypoint_section,
};
use crate::zoom::Zoom;

/// egui に組み込まれたフォントにない字形（日本語）のために、代わりとして足すフォントのファイル。
pub struct FontSource {
    pub name: String,
    /// 呼び出し側がファイルをプロセスの間持つときは借りる（新しいオーバーレイは何も写さない）。
    pub data: Cow<'static, [u8]>,
    /// フォントのコレクション（`.ttc`）の中の面の番号。ただの `.ttf`/`.otf` は 0。
    pub index: u32,
}

/// 状態の窓の、診断の文字の 1 行。
#[derive(Debug, Clone)]
pub struct StatusLine {
    pub label: String,
    pub value: String,
}

impl StatusLine {
    pub fn new(label: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            value: value.into(),
        }
    }
}

/// 各 OS のフックからの、フレームごとの入力。
pub struct FrameParams<'a> {
    /// 物理のピクセルでの、既定のフレームバッファの大きさ。
    pub framebuffer_size: [u32; 2],
    /// egui の 1 ポイントあたりの物理のピクセル（ウィンドウの中身の拡大率と、利用者の UI の拡大率を掛けたもの）。
    pub pixels_per_point: f32,
    /// 単調に増える時刻（秒）。
    pub time: f64,
    pub status: &'a [StatusLine],
    pub input: FrameInput,
}

/// このフレームでズームをどう見せるか。
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum ZoomView {
    #[default]
    Off,
    /// オーバーレイが、ゲームのふつうのフレームの中央を拡大する。
    Magnify,
    /// ゲームが縦に長いフレームを描き、各 OS のフックがもうその中央を `factor` 倍に拡大して画面に
    /// 置いた。
    HighRes { factor: f32 },
}

/// [`crate::InputRouter`] からの、このフレームの入力の状態。
#[derive(Default)]
pub struct FrameInput {
    pub events: Vec<egui::Event>,
    pub modifiers: Modifiers,
    pub ui_open: bool,
    pub zoom: ZoomView,
    /// ゲームが本物のカーソルを隠しているときに、オーバーレイの自前のカーソルを描く場所（ポイント）。
    pub software_cursor: Option<Pos2>,
    /// [`FrameOutput::start_capture`] の後に取ったキー。
    pub captured: Option<Captured>,
    /// 高精細のズームを使えない理由（メニューに出す）。
    pub high_res_note: Option<String>,
    pub waypoints: WaypointView,
    /// このフレームの新しい通知。オーバーレイは期限が切れるまでそれを持つ。
    pub notices: Vec<Notice>,
    /// Minecraft のデバッグの修飾キー、座標のコピーのキー、クラッシュのキー。ウェイポイントと
    /// 案内のホットキーはこれを取ってはならない。
    pub reserved_keys: Vec<egui::Key>,
    /// `reminedog_core::bindings_by_key` が options.txt から読んだ、各キーのゲームの割り当て
    /// （割り当ての id。ラベルでもよい）。置き換えの出力と一緒に出す。
    pub game_bindings: Vec<(InputId, Vec<String>)>,
    /// 置き換えのルールの一部が今は働かない理由（例：元のキーとしてのキー）。メニューに出す。
    pub rebind_note: Option<String>,
    /// 設定ファイルの問題（読めない、脇に移した、保存していない）。メニューの上に出す。
    pub settings_note: Option<String>,
    /// 置き換えのルールのキーとボタンのうち、ゲームのウィンドウのライブラリが報告できない（元のキー
    /// として）か、ゲームに渡せない（出力として）もの。メニューは、それを使うルールに注記を付ける。
    pub unsupported_inputs: Vec<InputId>,
    pub browser: BrowserView,
}

/// フレームの後に、各 OS のフックが行うべきこと。
#[derive(Debug, Default, PartialEq)]
pub struct FrameOutput {
    /// UI の閉じるボタンを押した。
    pub close_ui: bool,
    /// ホットキーのために次のキーを取る（[`crate::InputRouter::start_capture`]）。
    pub start_capture: bool,
    /// 置き換えのために次のキーかマウスのボタンを取る（[`crate::InputRouter::start_input_capture`]）。
    pub start_input_capture: bool,
    pub cancel_capture: bool,
    /// 設定が変わった：ホットキーを当てはめて保存する。
    pub settings: Option<Settings>,
    /// メニューのウェイポイントの欄で頼まれたこと（順に）。
    pub waypoint_commands: Vec<WaypointCommand>,
    /// ブラウザのため（順に）。
    pub browser_commands: Vec<BrowserCommand>,
    /// UI がコピーした文字（入力欄での Ctrl+C、Ctrl+X）。システムのクリップボードのため。
    pub copied_text: Option<String>,
    /// UI の部品がキーボードを持っている（[`crate::InputRouter::set_text_focus`]）。
    pub text_focus: bool,
}

/// 利用者のホットキー。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hotkeys {
    /// メニューを開閉する。
    pub menu: Hotkey,
    /// 押している間ズームする。
    pub zoom: Hotkey,
    /// プレイヤーの位置にウェイポイントを記録する。
    pub waypoint: Hotkey,
    /// プレイヤーの位置を更新し、選んだウェイポイントへの道を示す。
    pub navigate: Hotkey,
    pub browser: BrowserKeys,
}

/// ブラウザのホットキー。どれも割り当てなしでよい。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BrowserKeys {
    /// ブラウザを出し入れする。
    pub toggle: Option<Hotkey>,
    /// 残りは、ブラウザを出している間に働く。
    pub page_up: Option<Hotkey>,
    pub page_down: Option<Hotkey>,
    pub play_pause: Option<Hotkey>,
    pub seek_back: Option<Hotkey>,
    pub seek_forward: Option<Hotkey>,
}

impl Hotkeys {
    pub const DEFAULT: Hotkeys = Hotkeys {
        menu: Hotkey::DEFAULT_MENU,
        zoom: Hotkey::DEFAULT_ZOOM,
        waypoint: Hotkey::DEFAULT_WAYPOINT,
        navigate: Hotkey::DEFAULT_NAVIGATE,
        browser: BrowserKeys {
            toggle: None,
            page_up: Some(Hotkey::DEFAULT_BROWSER_PAGE_UP),
            page_down: Some(Hotkey::DEFAULT_BROWSER_PAGE_DOWN),
            play_pause: Some(Hotkey::DEFAULT_BROWSER_PLAY_PAUSE),
            seek_back: Some(Hotkey::DEFAULT_BROWSER_SEEK_BACK),
            seek_forward: Some(Hotkey::DEFAULT_BROWSER_SEEK_FORWARD),
        },
    };

    /// 操作のキー。割り当てのないブラウザのキーのときだけ `None`。
    pub(crate) fn get(&self, action: Action) -> Option<Hotkey> {
        match action {
            Action::Menu => Some(self.menu),
            Action::Zoom => Some(self.zoom),
            Action::Waypoint => Some(self.waypoint),
            Action::Navigate => Some(self.navigate),
            Action::Browser(key) => *self.browser_key(key),
        }
    }

    /// 操作のキーを入れる。`None` ならブラウザのキーの割り当てを外す（ほかのものは自分のキーを保つ）。
    fn set(&mut self, action: Action, hotkey: Option<Hotkey>) {
        match (action, hotkey) {
            (Action::Menu, Some(hotkey)) => self.menu = hotkey,
            (Action::Zoom, Some(hotkey)) => self.zoom = hotkey,
            (Action::Waypoint, Some(hotkey)) => self.waypoint = hotkey,
            (Action::Navigate, Some(hotkey)) => self.navigate = hotkey,
            (Action::Browser(key), hotkey) => *self.browser_key_mut(key) = hotkey,
            (_, None) => {}
        }
    }

    fn browser_key(&self, key: BrowserAction) -> &Option<Hotkey> {
        let keys = &self.browser;
        match key {
            BrowserAction::Toggle => &keys.toggle,
            BrowserAction::PageUp => &keys.page_up,
            BrowserAction::PageDown => &keys.page_down,
            BrowserAction::PlayPause => &keys.play_pause,
            BrowserAction::SeekBack => &keys.seek_back,
            BrowserAction::SeekForward => &keys.seek_forward,
        }
    }

    fn browser_key_mut(&mut self, key: BrowserAction) -> &mut Option<Hotkey> {
        let keys = &mut self.browser;
        match key {
            BrowserAction::Toggle => &mut keys.toggle,
            BrowserAction::PageUp => &mut keys.page_up,
            BrowserAction::PageDown => &mut keys.page_down,
            BrowserAction::PlayPause => &mut keys.play_pause,
            BrowserAction::SeekBack => &mut keys.seek_back,
            BrowserAction::SeekForward => &mut keys.seek_forward,
        }
    }
}

impl Default for Hotkeys {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// `settings` のホットキー。読めないものは既定に戻し、前のホットキーが隠してしまうもの（`hides`
/// を参照。手で書き換えたか古い設定ファイルにしかない）も同じ。ブラウザのキーは空でよい：割り当て
/// なし。戻したものをログに出す。読んだときの設定のため（メニューからの変更は [`hotkeys_quiet`] を使う）。
pub fn hotkeys(settings: &Settings) -> Hotkeys {
    resolve_hotkeys(settings, true)
}

/// 警告を出さない [`hotkeys`]。メニューが変えた設定のため（スライダーを動かすと、毎フレーム警告を
/// 繰り返してしまう）。
pub fn hotkeys_quiet(settings: &Settings) -> Hotkeys {
    resolve_hotkeys(settings, false)
}

fn resolve_hotkeys(settings: &Settings, log: bool) -> Hotkeys {
    macro_rules! warn {
        ($($arg:tt)*) => {
            if log {
                log::warn!($($arg)*);
            }
        };
    }
    let mut keys = Hotkeys::DEFAULT;
    for action in Action::ALL {
        let text = action.setting(settings);
        let default = action.default_hotkey();
        if action.optional() && text.trim().is_empty() {
            keys.set(action, None);
            continue;
        }
        let hotkey = Hotkey::parse(text).or_else(|| {
            warn!(
                "settings: unknown {} key {text:?}; using {}",
                action.id(),
                label_or_none(default)
            );
            default
        });
        let hotkey = if action.uses_f3c() && hotkey.is_some_and(|hotkey| hotkey.ctrl) {
            warn!(
                "settings: the {} key {text} needs Ctrl, which F3+C cannot be sent with; using {}",
                action.id(),
                label_or_none(default)
            );
            default
        } else {
            hotkey
        };
        keys.set(action, hotkey);
    }
    for action in Action::ROUTER_ORDER {
        let Some(earlier) = hidden_by(&keys, action) else {
            continue;
        };
        let default = action.default_hotkey();
        warn!(
            "settings: the {} key {} is hidden by the {} key {}; using {}",
            action.id(),
            label_or_none(keys.get(action)),
            earlier.id(),
            label_or_none(keys.get(earlier)),
            label_or_none(default)
        );
        keys.set(action, default);
        let Some(earlier) = hidden_by(&keys, action) else {
            continue;
        };
        if action.optional() {
            warn!(
                "settings: the {} key {} is hidden by the {} key too; unassigned",
                action.id(),
                label_or_none(default),
                earlier.id()
            );
            keys.set(action, None);
        } else {
            warn!(
                "settings: the {} key {} is hidden by the {} key {} too",
                action.id(),
                label_or_none(default),
                earlier.id(),
                label_or_none(keys.get(earlier))
            );
        }
    }
    keys
}

/// ログのため。
fn label_or_none(hotkey: Option<Hotkey>) -> String {
    hotkey.map_or_else(|| "none".to_owned(), |hotkey| hotkey.to_string())
}

/// 入力の振り分けが先に確かめる `earlier` が、`later` の押下を全部取るか：同じトリガーで、
/// `later` が要らない修飾キーを要らない（余分な修飾キーがあっても当てはまる）。ズームは最後に
/// 確かめるので、修飾キーを無視して当てはめても、ほかのキーを隠すことはない。割り当てのない
/// キーは何も隠さず、隠されもしない。
fn hides(earlier: Option<Hotkey>, later: Option<Hotkey>) -> bool {
    let (Some(earlier), Some(later)) = (earlier, later) else {
        return false;
    };
    earlier.trigger == later.trigger
        && (!earlier.ctrl || later.ctrl)
        && (!earlier.shift || later.shift)
        && (!earlier.alt || later.alt)
}

/// `action` より前に確かめる操作のうち、そのホットキーが `action` のものを隠すもの。
fn hidden_by(keys: &Hotkeys, action: Action) -> Option<Action> {
    Action::ROUTER_ORDER
        .into_iter()
        .take_while(|&earlier| earlier != action)
        .find(|&earlier| hides(keys.get(earlier), keys.get(action)))
}

/// ホットキーが `action` のものを隠すか、それに隠されるほかの操作。
fn conflict(keys: &Hotkeys, action: Action) -> Option<Action> {
    hidden_by(keys, action).or_else(|| {
        Action::ROUTER_ORDER
            .into_iter()
            .skip_while(|&earlier| earlier != action)
            .skip(1)
            .find(|&later| hides(keys.get(action), keys.get(later)))
    })
}

#[derive(Debug)]
pub struct OverlayError(String);

impl fmt::Display for OverlayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for OverlayError {}

/// オーバーレイが描く（router がポインターを写す）UI の拡大率：ポイントあたりのピクセルで、
/// 0.25〜8。数でなければ 1。
pub(crate) fn clamp_pixels_per_point(pixels_per_point: f32) -> f32 {
    if pixels_per_point.is_finite() {
        pixels_per_point.clamp(0.25, 8.0)
    } else {
        1.0
    }
}

/// 最初のフレームの後に、ホットキーの案内を出す秒数。
const HINT_SECONDS: f64 = 10.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Action {
    Menu,
    Zoom,
    Waypoint,
    Navigate,
    Browser(BrowserAction),
}

impl Action {
    /// 全部のホットキー。
    pub(crate) const ALL: [Action; 10] = [
        Action::Menu,
        Action::Zoom,
        Action::Waypoint,
        Action::Navigate,
        Action::Browser(BrowserAction::Toggle),
        Action::Browser(BrowserAction::PageUp),
        Action::Browser(BrowserAction::PageDown),
        Action::Browser(BrowserAction::PlayPause),
        Action::Browser(BrowserAction::SeekBack),
        Action::Browser(BrowserAction::SeekForward),
    ];
    /// メニューのキーの欄に並ぶとおり。
    pub(crate) const GENERAL: [Action; 4] = [
        Action::Menu,
        Action::Zoom,
        Action::Waypoint,
        Action::Navigate,
    ];
    /// メニューのブラウザの欄に並ぶとおり。
    pub(crate) const BROWSER: [Action; 6] = [
        Action::Browser(BrowserAction::Toggle),
        Action::Browser(BrowserAction::PageUp),
        Action::Browser(BrowserAction::PageDown),
        Action::Browser(BrowserAction::PlayPause),
        Action::Browser(BrowserAction::SeekBack),
        Action::Browser(BrowserAction::SeekForward),
    ];
    /// 入力の振り分けがホットキーを照らし合わせる順。
    pub(crate) const ROUTER_ORDER: [Action; 10] = [
        Action::Menu,
        Action::Waypoint,
        Action::Navigate,
        Action::Browser(BrowserAction::Toggle),
        Action::Browser(BrowserAction::PageUp),
        Action::Browser(BrowserAction::PageDown),
        Action::Browser(BrowserAction::PlayPause),
        Action::Browser(BrowserAction::SeekBack),
        Action::Browser(BrowserAction::SeekForward),
        Action::Zoom,
    ];

    pub(crate) fn name(self) -> &'static str {
        match self {
            Action::Menu => "メニューを開く",
            Action::Zoom => "ズーム",
            Action::Waypoint => "ウェイポイントを記録",
            Action::Navigate => "現在地を更新",
            Action::Browser(BrowserAction::Toggle) => "ブラウザの表示／非表示",
            Action::Browser(BrowserAction::PageUp) => "上へスクロール",
            Action::Browser(BrowserAction::PageDown) => "下へスクロール",
            Action::Browser(BrowserAction::PlayPause) => "再生／一時停止",
            Action::Browser(BrowserAction::SeekBack) => "巻き戻し",
            Action::Browser(BrowserAction::SeekForward) => "早送り",
        }
    }

    /// ログのため。
    pub(crate) fn id(self) -> &'static str {
        match self {
            Action::Menu => "menu",
            Action::Zoom => "zoom",
            Action::Waypoint => "waypoint",
            Action::Navigate => "navigate",
            Action::Browser(BrowserAction::Toggle) => "browser toggle",
            Action::Browser(BrowserAction::PageUp) => "browser page up",
            Action::Browser(BrowserAction::PageDown) => "browser page down",
            Action::Browser(BrowserAction::PlayPause) => "browser play/pause",
            Action::Browser(BrowserAction::SeekBack) => "browser seek back",
            Action::Browser(BrowserAction::SeekForward) => "browser seek forward",
        }
    }

    fn default_hotkey(self) -> Option<Hotkey> {
        Hotkeys::DEFAULT.get(self)
    }

    /// ブラウザのキーは割り当てを外せる。
    pub(crate) fn optional(self) -> bool {
        matches!(self, Action::Browser(_))
    }

    fn setting(self, settings: &Settings) -> &str {
        match self {
            Action::Menu => &settings.menu_key,
            Action::Zoom => &settings.zoom_key,
            Action::Waypoint => &settings.waypoint_key,
            Action::Navigate => &settings.navigate_key,
            Action::Browser(BrowserAction::Toggle) => &settings.browser_toggle_key,
            Action::Browser(BrowserAction::PageUp) => &settings.browser_page_up_key,
            Action::Browser(BrowserAction::PageDown) => &settings.browser_page_down_key,
            Action::Browser(BrowserAction::PlayPause) => &settings.browser_play_pause_key,
            Action::Browser(BrowserAction::SeekBack) => &settings.browser_seek_back_key,
            Action::Browser(BrowserAction::SeekForward) => &settings.browser_seek_forward_key,
        }
    }

    fn setting_mut(self, settings: &mut Settings) -> &mut String {
        match self {
            Action::Menu => &mut settings.menu_key,
            Action::Zoom => &mut settings.zoom_key,
            Action::Waypoint => &mut settings.waypoint_key,
            Action::Navigate => &mut settings.navigate_key,
            Action::Browser(BrowserAction::Toggle) => &mut settings.browser_toggle_key,
            Action::Browser(BrowserAction::PageUp) => &mut settings.browser_page_up_key,
            Action::Browser(BrowserAction::PageDown) => &mut settings.browser_page_down_key,
            Action::Browser(BrowserAction::PlayPause) => &mut settings.browser_play_pause_key,
            Action::Browser(BrowserAction::SeekBack) => &mut settings.browser_seek_back_key,
            Action::Browser(BrowserAction::SeekForward) => &mut settings.browser_seek_forward_key,
        }
    }

    /// F3+C で位置を記録するか更新するので、F3+C 自身のキーは使えない。
    fn uses_f3c(self) -> bool {
        matches!(self, Action::Waypoint | Action::Navigate)
    }
}

/// 設定とメニュー自身の状態。
struct UiState {
    settings: Settings,
    keys: Hotkeys,
    /// 今のフレームの [`FrameInput::reserved_keys`]。
    reserved_keys: Vec<egui::Key>,
    /// このホットキーのためにキーを待っている。
    capturing: Option<Action>,
    /// 最後に取ったキーを受け取らなかった理由。
    key_note: Option<String>,
    /// [`Self::key_note`] が話しているホットキー（それを持つ一覧に出る）。
    key_note_about: Action,
    waypoints: WaypointMenu,
    rebinds: RebindMenu,
    browser: BrowserMenu,
    /// 今のフレームの [`FrameInput::game_bindings`]、[`FrameInput::rebind_note`]、
    /// [`FrameInput::settings_note`]、[`FrameInput::unsupported_inputs`]。
    game_bindings: Vec<(InputId, Vec<String>)>,
    rebind_note: Option<String>,
    settings_note: Option<String>,
    unsupported_inputs: Vec<InputId>,
}

impl UiState {
    fn new(mut settings: Settings) -> Self {
        let keys = hotkeys(&settings);
        // router が使うもの：既定に戻したキーは、それを隠していたキーが動いても戻ってきてはならない。
        // メニューはどれかを言う（ログは理由を言う）。
        let mut changed = None;
        for action in Action::ALL {
            let text = setting_text(keys.get(action));
            let setting = action.setting_mut(&mut settings);
            if setting.trim() != text && changed.is_none() {
                changed = Some((action, setting.clone()));
            }
            *setting = text;
        }
        let key_note_about = changed.as_ref().map_or(Action::Menu, |(action, _)| *action);
        let key_note = changed.map(|(action, was)| {
            format!(
                "settings.json の「{}」のキー（{was}）は使えないので、{} にした",
                action.name(),
                keys.get(action)
                    .map_or_else(|| "なし".to_owned(), |key| key.label())
            )
        });
        Self {
            settings,
            keys,
            reserved_keys: Vec::new(),
            capturing: None,
            key_note,
            key_note_about,
            waypoints: WaypointMenu::default(),
            rebinds: RebindMenu::default(),
            browser: BrowserMenu::default(),
            game_bindings: Vec::new(),
            rebind_note: None,
            settings_note: None,
            unsupported_inputs: Vec::new(),
        }
    }

    fn set_hotkey(&mut self, action: Action, hotkey: Option<Hotkey>) {
        self.keys.set(action, hotkey);
        *action.setting_mut(&mut self.settings) = setting_text(self.keys.get(action));
    }

    /// ブラウザのキーの割り当てを外す。
    pub(crate) fn clear_hotkey(&mut self, action: Action) {
        if action.optional() {
            self.key_note = None;
            self.set_hotkey(action, None);
        }
    }

    /// 取ったキーを割り当てる。ただし、ほかのホットキーに届かなくなる（router は順に確かめ、余分な
    /// 修飾キーを押していても当てはめる）、F3+C を送る操作について F3+C のキーである、置き換えの
    /// ルールの元のキーである（router はルールより先に、その押下をホットキーとして取ってしまう）なら
    /// しない。
    fn assign(&mut self, action: Action, hotkey: Hotkey) {
        self.key_note_about = action;
        if action.uses_f3c()
            && self
                .reserved_keys
                .iter()
                .any(|&key| hotkey.trigger == Trigger::Key(key))
        {
            self.key_note = Some(format!(
                "{} は F3+C に使うキーなので使えない",
                Hotkey::plain(hotkey.trigger).label()
            ));
            return;
        }
        // キーのイベントが、Ctrl を押したままゲームに届いてしまう（Ctrl+捨てるはスタックごと投げる）。
        if action.uses_f3c() && hotkey.ctrl {
            self.key_note = Some(format!(
                "Ctrl との組み合わせ（{}）は「{}」に使えない",
                hotkey.label(),
                action.name()
            ));
            return;
        }
        if let Some(source) =
            rule_sources(&self.settings).find(|&id| input_trigger(id) == Some(hotkey.trigger))
        {
            self.key_note = Some(format!("{} は置き換えに使っている", input_label(source)));
            return;
        }
        let mut keys = self.keys;
        keys.set(action, Some(hotkey));
        if let Some(other) = conflict(&keys, action) {
            self.key_note = Some(format!(
                "{} は「{}」と重なるので使えない",
                hotkey.label(),
                other.name()
            ));
            return;
        }
        self.key_note = None;
        self.set_hotkey(action, Some(hotkey));
    }

    /// `actions` のホットキーを既定に戻す。ただし [`assign`](Self::assign) と同じく、置き換えの
    /// ルールの元のキーは断る：既定がそれである操作は自分のキーを保ち（注記付きで）、既定にすると
    /// ほかのキーを隠すか、それに隠される操作も同じ。
    fn reset_hotkeys(&mut self, actions: &[Action]) {
        if let Some(&first) = actions.first() {
            self.key_note_about = first;
        }
        let mut note = None;
        let mut keep: Vec<Action> = Action::ALL
            .into_iter()
            .filter(|action| !actions.contains(action))
            .collect();
        for &action in actions {
            let Some(default) = action.default_hotkey() else {
                continue;
            };
            // 既定から離れたままのキーだけ、そう言う必要がある。
            let moved = self.keys.get(action) != Some(default);
            if let Some(source) =
                rule_sources(&self.settings).find(|&id| input_trigger(id) == Some(default.trigger))
            {
                if moved {
                    note.get_or_insert_with(|| {
                        format!("{} は置き換えに使っている", input_label(source))
                    });
                }
                keep.push(action);
            } else if action.uses_f3c()
                && self
                    .reserved_keys
                    .iter()
                    .any(|&key| default.trigger == Trigger::Key(key))
            {
                if moved {
                    note.get_or_insert_with(|| {
                        format!(
                            "{} は F3+C に使うキーなので「{}」を元に戻せない",
                            default.label(),
                            action.name()
                        )
                    });
                }
                keep.push(action);
            }
        }
        let keys = loop {
            let mut keys = Hotkeys::DEFAULT;
            for &action in &keep {
                keys.set(action, self.keys.get(action));
            }
            match actions.iter().copied().find_map(|action| {
                (!keep.contains(&action))
                    .then(|| conflict(&keys, action).map(|other| (action, other)))
                    .flatten()
            }) {
                Some((action, other)) => {
                    if self.keys.get(action) != action.default_hotkey() {
                        note.get_or_insert_with(|| {
                            format!(
                                "「{}」は「{}」と重なるので元に戻せない",
                                action.name(),
                                other.name()
                            )
                        });
                    }
                    keep.push(action);
                }
                None => break keys,
            }
        };
        for &action in actions {
            self.set_hotkey(action, keys.get(action));
        }
        self.key_note = note;
    }
}

/// 設定ファイルのとおりのホットキー。なければ空。
fn setting_text(hotkey: Option<Hotkey>) -> String {
    hotkey.map_or_else(String::new, |hotkey| hotkey.to_string())
}

/// `list` のキー（それぞれ新しいキーを待つボタン）、次に最後の変更が言ったことと、それらを戻すボタン。
fn key_grid(
    ui: &mut egui::Ui,
    state: &mut UiState,
    actions: &mut MenuActions,
    list: &[Action],
    id: &str,
) {
    egui::Grid::new(id)
        .num_columns(3)
        .spacing([12.0, 4.0])
        .show(ui, |ui| {
            for &action in list {
                ui.label(action.name());
                let waiting = state.capturing == Some(action);
                let text = if waiting {
                    "キーを押す…".to_owned()
                } else {
                    state
                        .keys
                        .get(action)
                        .map_or_else(|| "なし".to_owned(), |key| key.label())
                };
                if ui.add(egui::Button::new(text).selected(waiting)).clicked() {
                    if waiting {
                        state.capturing = None;
                        actions.cancel_capture = true;
                    } else {
                        actions.capture = Some(action);
                    }
                }
                if action.optional() && state.keys.get(action).is_some() && !waiting {
                    if ui.small_button("外す").clicked() {
                        state.clear_hotkey(action);
                    }
                } else {
                    ui.label("");
                }
                ui.end_row();
            }
        });
    let capturing_here = state.capturing.is_some_and(|action| list.contains(&action));
    let note_here = list.contains(&state.key_note_about);
    if capturing_here {
        ui.label(RichText::new("割り当てるキーかマウスのボタンを押す（Esc で取り消し）").weak());
    } else if let Some(note) = state.key_note.as_ref().filter(|_| note_here) {
        ui.label(RichText::new(note).color(ui.visuals().warn_fg_color));
    }
    for note in game_key_notes(state, list) {
        ui.label(RichText::new(note).weak());
    }
    if ui.button("キーを元に戻す").clicked() {
        state.capturing = None;
        actions.cancel_capture = true;
        state.reset_hotkeys(list);
    }
}

/// ゲームも使うキー（options.txt）に割り当てた `list` のホットキー：reminedog が取っている間、
/// ゲームはそれを受け取らない。
fn game_key_notes(state: &UiState, list: &[Action]) -> Vec<String> {
    let mut notes = Vec::new();
    for &action in list {
        let Some(hotkey) = state.keys.get(action) else {
            continue;
        };
        // 組み合わせなら、キーだけのものはゲームに任せる。
        if hotkey.ctrl || hotkey.shift || hotkey.alt {
            continue;
        }
        let Some((_, mappings)) = state.game_bindings.iter().find(|(id, mappings)| {
            input_trigger(*id) == Some(hotkey.trigger) && !mappings.is_empty()
        }) else {
            continue;
        };
        let names: Vec<String> = mappings.iter().map(|m| mapping_label(m)).collect();
        let when = match action {
            Action::Menu => "",
            Action::Browser(BrowserAction::Toggle)
            | Action::Zoom
            | Action::Waypoint
            | Action::Navigate => "ゲーム中は",
            Action::Browser(_) => "ゲーム中にブラウザを表示している間は",
        };
        notes.push(format!(
            "{} はゲームの「{}」にも割り当てられている。{when}reminedog が使うので、ゲームには届かない",
            hotkey.label(),
            names.join("」「")
        ));
    }
    notes
}

/// このフレームでメニューが頼んだこと。
#[derive(Default)]
struct MenuActions {
    close: bool,
    capture: Option<Action>,
    /// 置き換えのためにキーかボタンを取る。
    capture_input: bool,
    cancel_capture: bool,
    waypoint_commands: Vec<WaypointCommand>,
    browser_commands: Vec<BrowserCommand>,
}

/// エージェント自前の GL のコンテキストで動き、既定のフレームバッファに描く egui。
pub struct Overlay {
    ctx: egui::Context,
    painter: egui_glow::Painter,
    zoom: Zoom,
    state: UiState,
    notices: NoticeList,
    /// ブラウザのページ。
    page: PageTexture,
    /// 変更を見分けるための、最後に各 OS のフックに渡した設定。
    saved_settings: Settings,
    first_time: Option<f64>,
    last_time: Option<f64>,
    frames: u64,
    fps: Fps,
}

impl Overlay {
    /// エージェントの GL のコンテキストを今のものにして呼ぶこと。
    pub fn new(
        gl: Arc<glow::Context>,
        fonts: Vec<FontSource>,
        settings: Settings,
    ) -> Result<Self, OverlayError> {
        let zoom = Zoom::new(&gl);
        let painter = egui_glow::Painter::new(gl, "", None, false)
            .map_err(|e| OverlayError(format!("egui_glow painter: {e}")))?;
        let ctx = egui::Context::default();
        ctx.set_theme(egui::Theme::Dark);
        ctx.set_fonts(font_definitions(fonts));
        // Ctrl+= と Ctrl+- は egui だけを拡大し、router がポインターを写す拡大率からずれてしまう。
        ctx.options_mut(|options| options.zoom_with_keyboard = false);
        let state = UiState::new(settings);
        Ok(Self {
            ctx,
            painter,
            zoom,
            saved_settings: state.settings.clone(),
            state,
            notices: NoticeList::default(),
            page: PageTexture::default(),
            first_time: None,
            last_time: None,
            frames: 0,
            fps: Fps::default(),
        })
    }

    /// ここまでに描いたフレームの数。
    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// [`render`](Self::render) の前に、ブラウザの最新の絵を上げる（`None` なら前のものを残す）。
    /// 呼び出し側は、すぐ後に絵を手放してよい。
    ///
    /// # Safety
    /// エージェントの GL のコンテキストが今のものであること。
    pub unsafe fn upload_page(&mut self, pixels: Option<BrowserPixels<'_>>) {
        // SAFETY: 呼び出し側が保証する。
        unsafe { self.page.update(&mut self.painter, pixels) };
    }

    /// 既定のフレームバッファに 1 フレームを描く。エージェントの GL のコンテキストをゲームの描画先で
    /// 今のものにして、バッファのスワップの直前に呼ぶこと。
    pub fn render(&mut self, params: FrameParams<'_>) -> FrameOutput {
        let [width, height] = params.framebuffer_size;
        if width == 0 || height == 0 {
            return FrameOutput::default();
        }
        let ppp = clamp_pixels_per_point(params.pixels_per_point);
        let dt = self
            .last_time
            .map_or(1.0 / 60.0, |t| (params.time - t) as f32)
            .clamp(0.001, 0.25);
        self.last_time = Some(params.time);
        let first_time = *self.first_time.get_or_insert(params.time);
        self.frames += 1;
        self.fps.tick(params.time);
        let picture = self.page.picture();

        let input = params.input;
        let state = &mut self.state;
        state.reserved_keys = input.reserved_keys;
        state.game_bindings = input.game_bindings;
        state.rebind_note = input.rebind_note;
        state.settings_note = input.settings_note;
        state.unsupported_inputs = input.unsupported_inputs;
        let mut capture_again = false;
        if let Some(captured) = input.captured {
            if let (Captured::Hotkey(hotkey), Some(action)) = (captured, state.capturing.take()) {
                state.assign(action, hotkey);
            }
            capture_again = state
                .rebinds
                .on_captured(captured, &mut state.settings, &state.keys);
        }
        if !input.ui_open {
            state.capturing = None;
            state.key_note = None;
            state.rebinds.cancel();
            capture_again = false;
        }
        state.waypoints.sync(input.ui_open, &input.waypoints);
        self.notices.update(input.notices, params.time);
        let notices = &self.notices;
        let zoom_supported = self.zoom.supported();
        if input.zoom == ZoomView::Magnify {
            // SAFETY: 呼び出し側が、自前のコンテキストがゲームの描画先で今のものであることを保証する。
            unsafe {
                self.zoom.draw(
                    self.painter.gl(),
                    [width, height],
                    state.settings.zoom_factor,
                    state.settings.zoom_smooth,
                );
            }
        }

        let screen = Rect::from_min_size(Pos2::ZERO, vec2(width as f32 / ppp, height as f32 / ppp));
        let mut browser_commands = Vec::new();
        state.browser.layout(
            &state.settings,
            screen,
            ppp,
            params.time,
            &mut browser_commands,
        );
        let mut raw = egui::RawInput {
            screen_rect: Some(screen),
            max_texture_side: Some(self.painter.max_texture_side()),
            time: Some(params.time),
            predicted_dt: dt,
            events: std::iter::once(egui::Event::ModifiersChanged(input.modifiers))
                .chain(input.events)
                .collect(),
            ..Default::default()
        };
        raw.viewports
            .entry(ViewportId::ROOT)
            .or_default()
            .native_pixels_per_point = Some(ppp);

        let info = FrameInfo {
            frames: self.frames,
            fps: self.fps.value,
            size: [width, height],
            ppp,
            zoom_supported,
        };
        // 案内が使う隅には、状態の窓がある。
        let show_hint = !input.ui_open
            && !state.settings.show_status
            && params.time - first_time < HINT_SECONDS;
        let high_res_note = input.high_res_note.as_deref();
        let mut actions = MenuActions::default();
        let mut output = self.ctx.run_ui(raw, |ui| {
            if input.ui_open {
                // オーバーレイが入力を持っていることが分かるように、ゲームを暗くする。
                ui.painter()
                    .rect_filled(ui.max_rect(), 0.0, Color32::from_black_alpha(90));
                actions = main_window(
                    ui.ctx(),
                    state,
                    &info,
                    params.status,
                    high_res_note,
                    &input.waypoints,
                    &input.browser,
                );
            } else if state.settings.show_status {
                status_window(ui.ctx(), &info, params.status);
            }
            browser_ui(
                ui.ctx(),
                &mut state.browser,
                &input.browser,
                &mut state.settings,
                picture,
                input.ui_open,
                &mut browser_commands,
            );
            match input.zoom {
                ZoomView::Off => {}
                ZoomView::Magnify if zoom_supported => {
                    zoom_badge(ui.ctx(), state.settings.zoom_factor);
                }
                ZoomView::Magnify => {}
                ZoomView::HighRes { factor } => zoom_badge(ui.ctx(), factor),
            }
            if show_hint {
                hint(ui.ctx(), state.keys.menu);
            }
            if !input.ui_open {
                draw_destination(ui.ctx(), &input.waypoints, &state.keys);
            }
            draw_notices(ui.ctx(), notices);
            if let Some(pos) = input.software_cursor {
                draw_cursor(ui.ctx(), pos);
            }
        });
        let text_focus = self.ctx.egui_wants_keyboard_input();
        let copied_text = output
            .platform_output
            .commands
            .iter()
            .rev()
            .find_map(|command| match command {
                egui::OutputCommand::CopyText(text) => Some(text.clone()),
                _ => None,
            });
        let primitives = self
            .ctx
            .tessellate(std::mem::take(&mut output.shapes), output.pixels_per_point);

        // 専用の自前のコンテキストでほかのフレームバッファを結び付けるのはズームだけ（終わったら 0 を
        // 結び付け直す）なので、これはウィンドウの既定のフレームバッファに描く。
        self.painter.paint_and_update_textures(
            [width, height],
            output.pixels_per_point,
            &primitives,
            &mut output.textures_delta,
        );
        let mut out = FrameOutput {
            close_ui: actions.close,
            cancel_capture: actions.cancel_capture,
            waypoint_commands: actions.waypoint_commands,
            browser_commands,
            copied_text,
            text_focus,
            ..Default::default()
        };
        out.browser_commands.extend(actions.browser_commands);
        if let Some(action) = actions.capture {
            state.capturing = Some(action);
            state.key_note = None;
            state.rebinds.cancel();
            out.start_capture = true;
        } else if (actions.capture_input || capture_again) && state.rebinds.adding() {
            state.capturing = None;
            out.start_input_capture = true;
        }
        if state.settings != self.saved_settings {
            self.saved_settings.clone_from(&state.settings);
            out.settings = Some(state.settings.clone());
        }
        out
    }

    /// GL の資源を解放する。エージェントの GL のコンテキストを今のものにして呼ぶこと。
    pub fn destroy(&mut self) {
        // SAFETY: 呼び出し側が、自前のコンテキストが今のものであることを保証する。
        unsafe { self.zoom.destroy(self.painter.gl()) };
        self.painter.destroy();
    }
}

/// 今のコンテキストの "version | renderer | vendor"（診断のため）。
pub fn gl_summary(gl: &glow::Context) -> String {
    // SAFETY: 今のコンテキストへのただの問い合わせ。
    unsafe {
        format!(
            "{} | {} | {}",
            gl.get_parameter_string(glow::VERSION),
            gl.get_parameter_string(glow::RENDERER),
            gl.get_parameter_string(glow::VENDOR)
        )
    }
}

fn font_definitions(fonts: Vec<FontSource>) -> FontDefinitions {
    let mut defs = FontDefinitions::default();
    for font in fonts {
        // egui は読めないフォントでパニックする（最初のフレームで。それでオーバーレイはプロセスの間
        // 止まる）：間違った面の番号、読めない形式。
        if let Err(e) = skrifa::FontRef::from_index(&font.data, font.index) {
            log::warn!(
                "font {} (face {}) cannot be read ({e}); egui's fonts only",
                font.name,
                font.index
            );
            continue;
        }
        // これがないと、日本語フォントは egui の行の上の方に（チェックボックスより上に）出る。
        let tweak = font_metrics::centering_tweak(&font.data, font.index);
        log::debug!(
            "font {}: glyphs shifted down by {:.3} em",
            font.name,
            tweak.y_offset_factor
        );
        defs.font_data.insert(
            font.name.clone(),
            Arc::new(FontData {
                font: font.data,
                index: font.index,
                tweak,
            }),
        );
        // プロポーショナルの文字は最初にこのフォントを使う：egui 自身の欧文のフォントと混ぜると、
        // 数字と仮名のベースラインがずれた（"プロトタイプ 1" の "1" が沈む）。
        // 等幅は egui のフォントのままで、このフォントは代わりの字形にだけ使う。
        defs.families
            .entry(FontFamily::Proportional)
            .or_default()
            .insert(0, font.name.clone());
        defs.families
            .entry(FontFamily::Monospace)
            .or_default()
            .push(font.name.clone());
    }
    defs
}

struct FrameInfo {
    frames: u64,
    fps: f32,
    size: [u32; 2],
    ppp: f32,
    /// OpenGL 3.0 以降：ズームが働く。
    zoom_supported: bool,
}

fn status_grid(ui: &mut egui::Ui, info: &FrameInfo, status: &[StatusLine]) {
    egui::Grid::new("reminedog-status-grid")
        .num_columns(2)
        .spacing([12.0, 2.0])
        .show(ui, |ui| {
            let mut row = |label: &str, value: String| {
                ui.label(label);
                ui.label(value);
                ui.end_row();
            };
            row("フレーム", info.frames.to_string());
            row("FPS", format!("{:.0}", info.fps));
            row("解像度", format!("{}×{}", info.size[0], info.size[1]));
            row("スケール", format!("{:.2}", info.ppp));
            for line in status {
                row(&line.label, line.value.clone());
            }
        });
}

/// UI を閉じている間に隅に出す、読むだけの状態（選んだときだけ）。
fn status_window(ctx: &egui::Context, info: &FrameInfo, status: &[StatusLine]) {
    let frame = egui::Frame::window(&ctx.global_style()).fill(Color32::from_black_alpha(200));
    egui::Window::new("reminedog")
        .id(egui::Id::new("reminedog-status"))
        .fixed_pos([12.0, 12.0])
        .resizable(false)
        .collapsible(false)
        .movable(false)
        .interactable(false)
        .frame(frame)
        .show(ctx, |ui| status_grid(ui, info, status));
}

/// メニュー。
fn main_window(
    ctx: &egui::Context,
    state: &mut UiState,
    info: &FrameInfo,
    status: &[StatusLine],
    high_res_note: Option<&str>,
    waypoints: &WaypointView,
    browser: &BrowserView,
) -> MenuActions {
    let mut actions = MenuActions::default();
    let mut open = true;
    // このフレームの画面から求めた、画面の高さまでの中身のための場所（egui は窓を画面の中に保つ）：
    // egui は覚えた大きさを小さくすることしかしないので、ウィンドウが大きくなった後もメニューは
    // 大きくならない。窓自身のスクロールはこの場所を埋めてしまうので、スクロールの領域は自前の
    // もの：中身に合わせて縮み、収まらないものをスクロールする。
    let room = ctx.content_rect().height();
    egui::Window::new("reminedog")
        .id(egui::Id::new("reminedog-main"))
        .open(&mut open)
        .default_pos([40.0, 40.0])
        .resizable(false)
        // 畳むと、メニューは見えないままホットキーかルールのキーを待ち続けてしまう。
        .collapsible(false)
        .resize(move |r| r.min_height(room))
        .show(ctx, |ui| {
            egui::ScrollArea::vertical()
                .id_salt("reminedog-menu")
                .show(ui, |ui| {
                    menu_contents(
                        ui,
                        state,
                        info,
                        status,
                        high_res_note,
                        waypoints,
                        browser,
                        &mut actions,
                    );
                });
        });
    actions.close = !open;
    actions
}

#[allow(clippy::too_many_arguments)]
fn menu_contents(
    ui: &mut egui::Ui,
    state: &mut UiState,
    info: &FrameInfo,
    status: &[StatusLine],
    high_res_note: Option<&str>,
    waypoints: &WaypointView,
    browser: &BrowserView,
    actions: &mut MenuActions,
) {
    ui.label(format!("{} か Esc で閉じる", state.keys.menu.label()));
    if let Some(note) = &state.settings_note {
        ui.label(RichText::new(note).color(ui.visuals().warn_fg_color));
    }
    ui.separator();

    ui.label(RichText::new("ズーム").strong());
    ui.label(format!(
        "ゲーム中に {} を押している間、画面の中央を拡大する",
        state.keys.zoom.label()
    ));
    if !info.zoom_supported {
        ui.label(
            RichText::new("この環境ではズームを使えない（OpenGL 3.0 以降が必要）")
                .color(ui.visuals().warn_fg_color),
        );
    }
    let (lo, hi) = ZOOM_FACTOR_RANGE;
    ui.add(egui::Slider::new(&mut state.settings.zoom_factor, lo..=hi).text("倍率"));
    ui.checkbox(&mut state.settings.zoom_high_res, "高精細にする")
        .on_hover_text(
            "ゲームに縦長の解像度で描かせ、細かいところまで拡大する。\n\
             倍率に比例して重くなる",
        );
    if let Some(note) = high_res_note.filter(|_| state.settings.zoom_high_res) {
        ui.label(RichText::new(note).weak());
    }
    // 高精細がオフか、使えないときに使う。
    ui.add_enabled_ui(
        !state.settings.zoom_high_res || high_res_note.is_some(),
        |ui| {
            ui.checkbox(&mut state.settings.zoom_smooth, "なめらかに拡大する")
                .on_hover_text("高精細でないときの拡大のしかた");
        },
    );
    ui.separator();

    ui.label(RichText::new("キー").strong());
    key_grid(ui, state, actions, &Action::GENERAL, "reminedog-keys");
    ui.separator();

    waypoint_section(
        ui,
        &mut state.waypoints,
        waypoints,
        &state.keys,
        &mut actions.waypoint_commands,
    );
    ui.separator();

    egui::CollapsingHeader::new("ブラウザ")
        .id_salt("reminedog-browser-section")
        .show(ui, |ui| {
            browser_section(
                ui,
                browser,
                &mut state.settings,
                &mut actions.browser_commands,
            );
            ui.label(RichText::new("キー（ブラウザを表示していて、ゲーム中のとき）").strong());
            key_grid(
                ui,
                state,
                actions,
                &Action::BROWSER,
                "reminedog-browser-keys",
            );
        });
    ui.separator();

    let rebinds = rebind_section(
        ui,
        &mut state.rebinds,
        &mut state.settings,
        &state.keys,
        &state.game_bindings,
        state.rebind_note.as_deref(),
        &state.unsupported_inputs,
    );
    actions.capture_input |= rebinds.capture;
    actions.cancel_capture |= rebinds.cancel_capture;
    ui.separator();

    egui::CollapsingHeader::new("状態")
        .id_salt("reminedog-status-section")
        .show(ui, |ui| status_grid(ui, info, status));
    ui.checkbox(
        &mut state.settings.show_status,
        "メニューを閉じても状態を表示する",
    );
}

fn zoom_badge(ctx: &egui::Context, factor: f32) {
    egui::Area::new(egui::Id::new("reminedog-zoom"))
        .anchor(egui::Align2::CENTER_TOP, [0.0, 12.0])
        .interactable(false)
        .show(ctx, |ui| {
            egui::Frame::popup(&ctx.global_style()).show(ui, |ui| {
                ui.label(format!("ズーム ×{factor:.1}"));
            });
        });
}

fn hint(ctx: &egui::Context, menu_key: Hotkey) {
    egui::Area::new(egui::Id::new("reminedog-hint"))
        .anchor(egui::Align2::LEFT_TOP, [12.0, 12.0])
        .interactable(false)
        .show(ctx, |ui| {
            egui::Frame::popup(&ctx.global_style()).show(ui, |ui| {
                ui.label(format!("reminedog：{} でメニューを開く", menu_key.label()));
            });
        });
}

/// 矢印のカーソル。ゲームが本物のカーソルを隠している間、全部の上に描く。
fn draw_cursor(ctx: &egui::Context, pos: Pos2) {
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Debug,
        egui::Id::new("reminedog-cursor"),
    ));
    let points = [
        pos,
        pos + vec2(0.0, 17.0),
        pos + vec2(4.5, 13.0),
        pos + vec2(12.0, 12.5),
    ];
    painter.add(egui::Shape::convex_polygon(
        points.to_vec(),
        Color32::WHITE,
        egui::Stroke::new(1.0, Color32::BLACK),
    ));
}

/// 0.5 秒の区間でならした、1 秒あたりのフレーム数。
#[derive(Default)]
struct Fps {
    window_start: Option<f64>,
    frames: u32,
    value: f32,
}

impl Fps {
    fn tick(&mut self, now: f64) {
        // 区間を開く tick はその始まりの印で、その中には数えない。
        let Some(start) = self.window_start else {
            self.window_start = Some(now);
            return;
        };
        self.frames += 1;
        let elapsed = now - start;
        if elapsed >= 0.5 {
            self.value = (f64::from(self.frames) / elapsed) as f32;
            self.window_start = Some(now);
            self.frames = 0;
        } else if elapsed < 0.0 {
            // 時計が戻った。やり直す。
            self.window_start = Some(now);
            self.frames = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_keys_load_unassigned_or_as_set() {
        let keys = hotkeys(&Settings::default());
        assert_eq!(keys.browser.toggle, None);
        assert_eq!(
            keys.browser.page_down,
            Some(Hotkey::plain(Trigger::Key(egui::Key::PageDown)))
        );
        assert_eq!(
            keys.browser.play_pause,
            Some(Hotkey::plain(Trigger::Key(egui::Key::ArrowDown)))
        );
        let settings = Settings {
            browser_toggle_key: "Ctrl+B".into(),
            browser_page_up_key: String::new(),
            browser_seek_back_key: "nonsense".into(),
            ..Settings::default()
        };
        let keys = hotkeys(&settings);
        assert_eq!(keys.browser.toggle, Hotkey::parse("Ctrl+B"));
        assert_eq!(keys.browser.page_up, None);
        assert_eq!(keys.browser.seek_back, Hotkeys::DEFAULT.browser.seek_back);
        // 設定は router が使うものを持つ。
        let state = UiState::new(settings);
        assert_eq!(state.settings.browser_toggle_key, "Ctrl+B");
        assert_eq!(state.settings.browser_page_up_key, "");
        assert_eq!(state.settings.browser_seek_back_key, "Left");
        assert_eq!(state.settings.browser_play_pause_key, "Down");
    }

    #[test]
    fn a_browser_key_hidden_by_another_falls_back_or_goes() {
        // Page Down のウェイポイントのキーはブラウザのものを隠す：既定が戻る。
        let settings = Settings {
            waypoint_key: "PageDown".into(),
            browser_page_down_key: "Shift+PageDown".into(),
            ..Settings::default()
        };
        let keys = hotkeys(&settings);
        assert_eq!(keys.waypoint, Hotkey::parse("PageDown").unwrap());
        assert_eq!(keys.browser.page_down, None, "the default is hidden too");
        // End のメニューのキーは、既定で割り当てのない End の切り替えを隠す：なし。
        let settings = Settings {
            menu_key: "End".into(),
            browser_toggle_key: "End".into(),
            ..Settings::default()
        };
        assert_eq!(hotkeys(&settings).browser.toggle, None);
    }

    #[test]
    fn browser_keys_are_assigned_cleared_and_reset() {
        let mut state = state();
        let toggle = Hotkey::parse("Ctrl+B").unwrap();
        let action = Action::Browser(BrowserAction::Toggle);
        state.assign(action, toggle);
        assert_eq!(state.keys.browser.toggle, Some(toggle));
        assert_eq!(state.settings.browser_toggle_key, "Ctrl+B");
        // ほかのブラウザのキーが取っている。
        let note = refused(
            &mut state,
            Action::Browser(BrowserAction::PlayPause),
            "Ctrl+B",
        );
        assert!(note.is_some_and(|note| note.contains("ブラウザの表示／非表示")));
        assert!(Action::BROWSER.contains(&state.key_note_about));
        // ブラウザのキーはズームのものを取れない。
        assert!(refused(&mut state, Action::Browser(BrowserAction::PageUp), "Z").is_some());
        // 外した。
        let page_up = Action::Browser(BrowserAction::PageUp);
        state.clear_hotkey(page_up);
        assert_eq!(state.keys.browser.page_up, None);
        assert_eq!(state.settings.browser_page_up_key, "");
        // 全般のキーは外せない。
        state.clear_hotkey(Action::Zoom);
        assert_eq!(state.keys.zoom, Hotkey::DEFAULT_ZOOM);
        // ブラウザのキーを戻しても、ほかには触れない。
        state.assign(Action::Zoom, Hotkey::parse("X").unwrap());
        state.reset_hotkeys(&Action::BROWSER);
        assert_eq!(state.keys.browser, Hotkeys::DEFAULT.browser);
        assert_eq!(state.keys.zoom, Hotkey::parse("X").unwrap());
        assert_eq!(state.settings.browser_toggle_key, "");
        assert_eq!(state.settings.browser_page_up_key, "PageUp");
    }

    #[test]
    fn fps_averages_over_half_second() {
        let mut fps = Fps::default();
        for i in 0..=60 {
            fps.tick(f64::from(i) / 120.0);
        }
        assert!((fps.value - 120.0).abs() < 1.0, "{}", fps.value);
    }

    /// egui が読めるフォント：egui 自身のもの。
    fn readable_font() -> Cow<'static, [u8]> {
        let defs = FontDefinitions::default();
        let data = defs.font_data.values().next().expect("egui has fonts");
        Cow::Owned(data.font.to_vec())
    }

    #[test]
    fn japanese_font_leads_proportional_and_backs_up_monospace() {
        let defs = font_definitions(vec![FontSource {
            name: "jp".into(),
            data: readable_font(),
            index: 0,
        }]);
        assert_eq!(defs.font_data["jp"].index, 0);
        let proportional = &defs.families[&FontFamily::Proportional];
        assert_eq!(proportional.first().map(String::as_str), Some("jp"));
        assert!(proportional.len() > 1, "egui's fonts remain as fallbacks");
        let monospace = &defs.families[&FontFamily::Monospace];
        assert_eq!(monospace.last().map(String::as_str), Some("jp"));
        assert!(monospace.len() > 1, "egui's monospace font stays first");
    }

    #[test]
    fn unreadable_fonts_are_left_out() {
        let defaults = FontDefinitions::default();
        for (data, index) in [
            (Cow::Borrowed(&b"not a font"[..]), 0),
            // ただのフォントには面 0 しかない（fontconfig の名前付きインスタンスは、もっと多くのビットを持つ）。
            (readable_font(), 0x10000),
        ] {
            let defs = font_definitions(vec![FontSource {
                name: "jp".into(),
                data,
                index,
            }]);
            assert!(!defs.font_data.contains_key("jp"));
            assert_eq!(defs.families, defaults.families);
        }
    }

    fn state() -> UiState {
        UiState::new(Settings::default())
    }

    #[test]
    fn assigning_a_hotkey_updates_the_settings() {
        let mut state = state();
        state.assign(Action::Zoom, Hotkey::parse("C").unwrap());
        assert_eq!(state.settings.zoom_key, "C");
        assert_eq!(state.keys.zoom, Hotkey::parse("C").unwrap());
        assert_eq!(state.key_note, None);
        state.assign(Action::Waypoint, Hotkey::parse("Mouse4").unwrap());
        assert_eq!(state.settings.waypoint_key, "Mouse4");
        state.assign(Action::Navigate, Hotkey::parse("Shift+N").unwrap());
        assert_eq!(state.settings.navigate_key, "Shift+N");
        assert_eq!(state.keys.navigate, Hotkey::parse("Shift+N").unwrap());
    }

    #[test]
    fn a_hotkey_that_would_hide_the_zoom_is_refused() {
        let mut state = state();
        // ただの Z のメニューは、ズームの押下を全部飲み込んでしまう。
        state.assign(Action::Menu, Hotkey::parse("Z").unwrap());
        assert_eq!(state.settings.menu_key, "Ctrl+I");
        assert!(state.key_note.is_some());
        // メニュー自身のキーのズームも同じ。
        state.assign(Action::Zoom, Hotkey::parse("Ctrl+I").unwrap());
        assert_eq!(state.settings.zoom_key, "Z");
        // ただの I のズームはよい：Ctrl+I がメニューを開き、I がズームする。
        state.assign(Action::Zoom, Hotkey::parse("I").unwrap());
        assert_eq!(state.settings.zoom_key, "I");
        assert_eq!(state.key_note, None);
    }

    fn refused(state: &mut UiState, action: Action, text: &str) -> Option<String> {
        let before = state.keys;
        state.assign(action, Hotkey::parse(text).unwrap());
        assert_eq!(state.keys, before, "{action:?} on {text}");
        state.key_note.take()
    }

    #[test]
    fn waypoint_hotkeys_must_not_hide_or_be_hidden() {
        let mut state = state();
        // ズームより先に確かめる：J は、Ctrl があってもなくても、ズームの押下を全部取ってしまう。
        let note = refused(&mut state, Action::Zoom, "J");
        assert_eq!(
            note.as_deref(),
            Some("J は「ウェイポイントを記録」と重なるので使えない")
        );
        refused(&mut state, Action::Zoom, "Ctrl+K");
        refused(&mut state, Action::Waypoint, "Z");
        refused(&mut state, Action::Navigate, "Z");
        // メニューが先（Ctrl なしのキーで。この 2 つは Ctrl を使えない）。
        state.assign(Action::Menu, Hotkey::parse("Alt+I").unwrap());
        let note = refused(&mut state, Action::Waypoint, "Alt+I");
        assert_eq!(
            note.as_deref(),
            Some("Alt+I は「メニューを開く」と重なるので使えない")
        );
        refused(&mut state, Action::Navigate, "Alt+Shift+I");
        state.assign(Action::Menu, Hotkey::DEFAULT_MENU);
        let note = refused(&mut state, Action::Menu, "K");
        assert_eq!(
            note.as_deref(),
            Some("K は「現在地を更新」と重なるので使えない")
        );
        // ウェイポイントのキーは、案内のキーより先。
        refused(&mut state, Action::Navigate, "J");
        refused(&mut state, Action::Waypoint, "K");
        refused(&mut state, Action::Navigate, "Alt+J");
        // 前のキーより多くの修飾キーが要るのはよい。
        state.assign(Action::Waypoint, Hotkey::parse("Alt+K").unwrap());
        assert_eq!(state.key_note, None);
        assert_eq!(state.settings.waypoint_key, "Alt+K");
        state.assign(Action::Zoom, Hotkey::parse("J").unwrap());
        assert_eq!(state.settings.zoom_key, "J");
        state.assign(Action::Waypoint, Hotkey::parse("Shift+J").unwrap());
        assert_eq!(state.settings.waypoint_key, "Shift+J");
        // マウスのボタンも同じ決まりに従う。
        state.assign(Action::Menu, Hotkey::parse("Mouse5").unwrap());
        refused(&mut state, Action::Navigate, "Mouse5");
        state.assign(Action::Navigate, Hotkey::parse("Shift+Mouse5").unwrap());
        assert_eq!(
            state.key_note.as_deref(),
            Some("Shift+マウスのボタン5 は「メニューを開く」と重なるので使えない")
        );
    }

    #[test]
    fn f3c_hotkeys_cannot_need_ctrl() {
        let mut state = state();
        let note = refused(&mut state, Action::Waypoint, "Ctrl+L");
        assert_eq!(
            note.as_deref(),
            Some("Ctrl との組み合わせ（Ctrl+L）は「ウェイポイントを記録」に使えない")
        );
        refused(&mut state, Action::Navigate, "Ctrl+Shift+L");
        // ほかのホットキーはよい。
        state.assign(Action::Zoom, Hotkey::parse("Ctrl+L").unwrap());
        assert_eq!(state.settings.zoom_key, "Ctrl+L");
        // 保存したものは既定に戻る。
        let settings = Settings {
            waypoint_key: "Ctrl+L".into(),
            ..Settings::default()
        };
        assert_eq!(hotkeys(&settings).waypoint, Hotkey::DEFAULT_WAYPOINT);
    }

    #[test]
    fn the_menu_and_the_router_agree_on_fallen_back_keys() {
        // J のズームは Z に戻る。ウェイポイントのキーを J から動かしても、J が戻ってきてはならない。
        let mut state = UiState::new(Settings {
            zoom_key: "J".into(),
            ..Settings::default()
        });
        assert_eq!(state.settings.zoom_key, "Z");
        state.assign(Action::Waypoint, Hotkey::parse("L").unwrap());
        assert_eq!(state.key_note, None);
        assert_eq!(hotkeys(&state.settings), state.keys);
        assert_eq!(state.keys.zoom, Hotkey::DEFAULT_ZOOM);
    }

    #[test]
    fn f3c_keys_are_refused_for_the_waypoint_hotkeys() {
        let mut state = state();
        state.reserved_keys = vec![egui::Key::F3, egui::Key::C];
        let note = refused(&mut state, Action::Waypoint, "Ctrl+C");
        assert_eq!(note.as_deref(), Some("C は F3+C に使うキーなので使えない"));
        refused(&mut state, Action::Navigate, "F3");
        // ズームはよい：F3 を押している間、router はそのキーをゲームに渡す。
        state.assign(Action::Zoom, Hotkey::parse("C").unwrap());
        assert_eq!(state.settings.zoom_key, "C");
    }

    #[test]
    fn rule_sources_are_refused_as_hotkeys() {
        let mut state = UiState::new(Settings {
            rebinds: vec![
                reminedog_core::Rebind {
                    from: "key.keyboard.keypad.1".into(),
                    to: "key.keyboard.f3".into(),
                },
                reminedog_core::Rebind {
                    from: "key.mouse.4".into(),
                    to: "key.keyboard.c".into(),
                },
            ],
            ..Settings::default()
        });
        // フックはテンキーの 1 を 1 として渡す：1 のホットキーは、その押下を取ってしまう。
        let note = refused(&mut state, Action::Zoom, "1");
        assert_eq!(note.as_deref(), Some("テンキー 1 は置き換えに使っている"));
        let note = refused(&mut state, Action::Waypoint, "Shift+Mouse4");
        assert_eq!(
            note.as_deref(),
            Some("マウスのボタン4 は置き換えに使っている")
        );
        state.assign(Action::Zoom, Hotkey::parse("Mouse5").unwrap());
        assert_eq!(state.settings.zoom_key, "Mouse5");
    }

    /// `size` ポイントの画面での、いくつかのフレームの後のメニューの窓。
    fn menu_rect(size: egui::Vec2, state: &mut UiState, waypoints: &WaypointView) -> Rect {
        menu_rect_in(&egui::Context::default(), size, state, waypoints)
    }

    /// 前のフレームを覚えている `ctx` の中の [`menu_rect`]。
    fn menu_rect_in(
        ctx: &egui::Context,
        size: egui::Vec2,
        state: &mut UiState,
        waypoints: &WaypointView,
    ) -> Rect {
        let info = FrameInfo {
            frames: 1,
            fps: 60.0,
            size: [size.x as u32, size.y as u32],
            ppp: 1.0,
            zoom_supported: true,
        };
        for _ in 0..4 {
            let raw = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)),
                ..Default::default()
            };
            let mut output = ctx.run_ui(raw, |ui| {
                main_window(
                    ui.ctx(),
                    state,
                    &info,
                    &[],
                    None,
                    waypoints,
                    &BrowserView::default(),
                );
            });
            output.textures_delta.clear();
        }
        ctx.memory(|m| m.area_rect(egui::Id::new("reminedog-main")))
            .expect("the menu was shown")
    }

    fn many_waypoints() -> WaypointView {
        WaypointView {
            world: crate::WorldLabel::Singleplayer("w".into()),
            waypoints: (0..20)
                .map(|id| reminedog_core::Waypoint {
                    id,
                    name: format!("地点 {id}"),
                    dimension: "minecraft:overworld".into(),
                    x: 0.0,
                    y: 64.0,
                    z: 0.0,
                    created_unix: 0,
                })
                .collect(),
            playing: true,
            ..WaypointView::default()
        }
    }

    #[test]
    fn the_menu_scrolls_inside_a_small_window() {
        let waypoints = many_waypoints();
        let mut state = state();
        // UI の拡大率 1.5 と 1 での、Minecraft の既定の 854×480。
        for small in [vec2(854.0 / 1.5, 480.0 / 1.5), vec2(854.0, 480.0)] {
            let rect = menu_rect(small, &mut state, &waypoints);
            let screen = Rect::from_min_size(Pos2::ZERO, small);
            assert!(screen.contains_rect(rect), "{rect:?} on {small:?}");
            assert!(rect.height() > small.y - 20.0, "uses the height: {rect:?}");
        }
        // 大きな画面では、窓は中身と同じ高さ。
        let large = vec2(1920.0, 1080.0);
        let rect = menu_rect(large, &mut state, &waypoints);
        assert!(rect.height() < 900.0, "{rect:?}");
        assert_eq!(rect.min, egui::pos2(40.0, 40.0));
    }

    #[test]
    fn the_menu_grows_back_when_the_screen_gets_bigger() {
        let waypoints = many_waypoints();
        let mut state = state();
        let large = vec2(1920.0, 1080.0);
        let fresh = menu_rect(large, &mut state, &waypoints);
        // 小さいウィンドウで開き、それから最大化した（そして戻した）。
        let ctx = egui::Context::default();
        let small = vec2(854.0, 480.0);
        let rect = menu_rect_in(&ctx, small, &mut state, &waypoints);
        assert!(rect.height() <= small.y, "{rect:?}");
        let rect = menu_rect_in(&ctx, large, &mut state, &waypoints);
        assert!(rect.height() > small.y, "{rect:?}");
        assert_eq!(rect.height(), fresh.height(), "as tall as its contents");
        let rect = menu_rect_in(&ctx, small, &mut state, &waypoints);
        assert!(rect.height() <= small.y, "{rect:?}");
    }

    #[test]
    fn the_menu_cannot_be_folded() {
        let ctx = egui::Context::default();
        let mut state = state();
        let rect = menu_rect_in(&ctx, vec2(1920.0, 1080.0), &mut state, &many_waypoints());
        // タイトルバーのダブルクリックは、畳める窓を畳む。
        let title = rect.min + vec2(rect.width() / 2.0, 12.0);
        let click = |pressed| egui::Event::PointerButton {
            pos: title,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        };
        let info = FrameInfo {
            frames: 1,
            fps: 60.0,
            size: [1920, 1080],
            ppp: 1.0,
            zoom_supported: true,
        };
        for events in [
            vec![egui::Event::PointerMoved(title), click(true), click(false)],
            vec![click(true), click(false)],
            vec![],
            vec![],
        ] {
            let raw = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1920.0, 1080.0))),
                events,
                ..Default::default()
            };
            let mut output = ctx.run_ui(raw, |ui| {
                main_window(
                    ui.ctx(),
                    &mut state,
                    &info,
                    &[],
                    None,
                    &many_waypoints(),
                    &BrowserView::default(),
                );
            });
            output.textures_delta.clear();
        }
        let after = ctx
            .memory(|m| m.area_rect(egui::Id::new("reminedog-main")))
            .expect("shown");
        assert_eq!(after.height(), rect.height());
    }

    fn rule(from: &str, to: &str) -> reminedog_core::Rebind {
        reminedog_core::Rebind {
            from: from.into(),
            to: to.into(),
        }
    }

    #[test]
    fn resetting_the_hotkeys_keeps_those_on_rule_sources() {
        let mut state = state();
        state.assign(Action::Zoom, Hotkey::parse("Mouse5").unwrap());
        state.assign(Action::Menu, Hotkey::parse("F6").unwrap());
        // 今は Z が空いている：ルールが使ってよい。
        state
            .settings
            .rebinds
            .push(rule("key.keyboard.z", "key.keyboard.left.shift"));
        state.reset_hotkeys(&Action::GENERAL);
        assert_eq!(state.settings.zoom_key, "Mouse5");
        assert_eq!(state.settings.menu_key, "Ctrl+I");
        assert_eq!(state.key_note.as_deref(), Some("Z は置き換えに使っている"));
        assert_eq!(hotkeys(&state.settings), state.keys);
        // 邪魔するものはない：全部戻り、注記なし。
        state.settings.rebinds.clear();
        state.reset_hotkeys(&Action::GENERAL);
        assert_eq!(state.keys, Hotkeys::DEFAULT);
        assert_eq!(state.key_note, None);
        // 既定のキーへのルールで、何も変えないもの（メニューはもう Ctrl+I）：やはり注記なし。
        state
            .settings
            .rebinds
            .push(rule("key.keyboard.i", "key.keyboard.o"));
        state.reset_hotkeys(&Action::GENERAL);
        assert_eq!(state.keys, Hotkeys::DEFAULT);
        assert_eq!(state.key_note, None);
    }

    #[test]
    fn resetting_the_hotkeys_keeps_the_ones_a_kept_key_would_hide() {
        let mut state = state();
        state.assign(Action::Waypoint, Hotkey::parse("M").unwrap());
        state.assign(Action::Zoom, Hotkey::parse("J").unwrap());
        state
            .settings
            .rebinds
            .push(rule("key.keyboard.z", "key.keyboard.c"));
        state.reset_hotkeys(&Action::GENERAL);
        // ズームが J のままなので、ウェイポイントのキーは J に戻れない。
        assert_eq!(state.settings.zoom_key, "J");
        assert_eq!(state.settings.waypoint_key, "M");
        assert_eq!(state.settings.navigate_key, "K");
        assert_eq!(state.key_note.as_deref(), Some("Z は置き換えに使っている"));
        assert_eq!(hotkeys(&state.settings), state.keys);
    }

    #[test]
    fn unreadable_hotkeys_fall_back_to_the_defaults() {
        assert_eq!(hotkeys(&Settings::default()), Hotkeys::DEFAULT);
        let settings = Settings {
            menu_key: "Nope".into(),
            zoom_key: "Mouse4".into(),
            waypoint_key: "".into(),
            navigate_key: "Shift+L".into(),
            ..Settings::default()
        };
        let keys = hotkeys(&settings);
        assert_eq!(keys.menu, Hotkey::DEFAULT_MENU);
        assert_eq!(keys.zoom.to_string(), "Mouse4");
        assert_eq!(keys.waypoint, Hotkey::DEFAULT_WAYPOINT);
        assert_eq!(keys.navigate.to_string(), "Shift+L");
    }

    #[test]
    fn saved_hotkeys_that_would_be_hidden_fall_back_to_the_defaults() {
        // ズームが J の古い設定ファイル。今は J はウェイポイントのキーの既定。
        let settings = Settings {
            zoom_key: "J".into(),
            ..Settings::default()
        };
        assert_eq!(hotkeys(&settings).zoom, Hotkey::DEFAULT_ZOOM);
        let settings = Settings {
            menu_key: "Alt+I".into(),
            waypoint_key: "Alt+I".into(),
            navigate_key: "J".into(),
            ..Settings::default()
        };
        let keys = hotkeys(&settings);
        // ウェイポイントのキーは J に戻り、それが案内のキーを隠す。
        assert_eq!(keys.waypoint, Hotkey::DEFAULT_WAYPOINT);
        assert_eq!(keys.navigate, Hotkey::DEFAULT_NAVIGATE);
        // J のメニュー（ウェイポイントのキーができる前は許していた）も既定を隠す：既定のまま残し
        // （ログに出す）、メニューは J に両方を出す。
        let settings = Settings {
            menu_key: "J".into(),
            ..Settings::default()
        };
        let keys = hotkeys(&settings);
        assert_eq!(keys.menu.to_string(), "J");
        assert_eq!(keys.waypoint, Hotkey::DEFAULT_WAYPOINT);
        assert_eq!(keys.zoom, Hotkey::DEFAULT_ZOOM);
    }
}
