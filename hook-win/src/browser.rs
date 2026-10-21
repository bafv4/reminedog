//! ゲーム内ブラウザ：専用のスレッドで画面の外に描く WebView2（Microsoft Edge）。オーバーレイは
//! それをページの絵として見せる。
//!
//! ブラウザのスレッド（[`thread`]）が WebView（[`webview`]）を持ち、描いたものを [`FRAME`] に
//! 写す（[`capture`]）。ゲームのスレッドは、毎フレーム最新の絵をテクスチャに上げる。ゲームの
//! スレッドはブラウザを待たない：命令をスレッドの [`Mailbox`] に並べ、ウィンドウのメッセージで
//! 起こす。
//!
//! 最初に見せるまでは何もない。起動のたびに新しい世代になる。古い世代のスレッド（まだ閉じる途中）
//! は、もう [`FRAME`] にも [`SHARED`] にも書かない。
//!
//! 最後に開いたページは、WebView2 のデータの隣の `browser.json` に残し（ゲームフォルダの設定には
//! 残さない：アドレスはトークンを含みうる）、次の起動でそれを開く。
//!
//! ロック：[`SHARED`]、[`FRAME`]、メールボックスのキューは末端：どれも、別のものを取る間や
//! WebView2 を呼ぶ間は持たない。[`CONTROL`] は、命令をメールボックスのキューに入れてその
//! ウィンドウを起こす間は持つ（逆はない）。
//!
//! ブラウザそのものがあるのは MSVC のビルドだけ。そのスレッドは Direct3D 11、WinRT、COM
//! （CoreMessaging、combase）を import し、Wine 向け（GNU）にビルドしたエージェントはそれを
//! 読み込めない：import が足りないと、エージェント全体が、つまりゲームが起動しない。
#![cfg_attr(not(target_env = "msvc"), allow(dead_code))]

#[cfg(target_env = "msvc")]
mod capture;
#[cfg(target_env = "msvc")]
mod thread;
#[cfg(target_env = "msvc")]
mod webview;

/// GNU のビルドの代わり：ブラウザは始められない。
#[cfg(not(target_env = "msvc"))]
mod thread {
    use std::io;
    use std::sync::Arc;

    use super::{Mailbox, Start};

    pub(super) fn spawn(_generation: u64, _mailbox: Arc<Mailbox>, _start: Start) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "the GNU build has no browser",
        ))
    }
}

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, TryLockError};

use reminedog_agent::os::Browser;
use reminedog_core::Settings;
use reminedog_core::browser::{MediaCommand, PageInput};
use reminedog_render::{
    BrowserAction, BrowserCommand, BrowserPixels, BrowserState, BrowserView, Notice, PageLayout,
};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

/// ブラウザのスレッドを起こして、メールボックスを動かす。
const WM_APP_COMMAND: u32 = WM_APP + 1;
/// ページの絵を写す用意ができた。
const WM_APP_FRAME: u32 = WM_APP + 2;

/// ページの最新の絵。
pub struct FrameSlot {
    /// それが来たブラウザ（0：なし）。
    pub generation: u64,
    /// ピクセル。
    pub size: [u32; 2],
    /// `size[0] * size[1]` のピクセル。上の行から、それぞれ 4 バイト（青、緑、赤、アルファ）。
    pub bgra: Vec<u8>,
    /// この世代の絵を数える（0：まだない）。
    pub seq: u64,
}

static FRAME: Mutex<FrameSlot> = Mutex::new(FrameSlot {
    generation: 0,
    size: [0, 0],
    bgra: Vec::new(),
    seq: 0,
});

/// [`FRAME`] の変更を数える（ロックしたまま変える）ので、ゲームのスレッドは新しい絵のときだけ
/// それをロックする。
static FRAME_VERSION: AtomicU64 = AtomicU64::new(0);

/// [`FRAME`] の変更を書き留める。ロックしたまま呼ぶこと。
fn frame_changed() {
    FRAME_VERSION.fetch_add(1, Ordering::Release);
}

/// ブラウザのスレッドが報告すること。
#[derive(Debug, Clone)]
struct Shared {
    generation: u64,
    /// `shown` 以外の全部（それはゲームのスレッドが知っている。[`Control::shown`]）。
    view: BrowserView,
    notices: Vec<Notice>,
}

impl Shared {
    const fn new(generation: u64, state: BrowserState) -> Self {
        Self {
            generation,
            view: BrowserView {
                state,
                shown: false,
                url: String::new(),
                title: String::new(),
                can_go_back: false,
                can_go_forward: false,
            },
            notices: Vec::new(),
        }
    }
}

static SHARED: Mutex<Shared> = Mutex::new(Shared::new(0, BrowserState::Off));

/// `generation` がまだ今のブラウザなら、共有の状態を更新する。
fn report(generation: u64, update: impl FnOnce(&mut Shared)) {
    let mut shared = lock(&SHARED);
    if shared.generation == generation {
        update(&mut shared);
    }
}

/// ブラウザの通知を出す秒数。
const NOTICE_SECONDS: f64 = 2.5;

fn notice(text: impl Into<String>, warn: bool) -> Notice {
    Notice::new(text, warn, NOTICE_SECONDS)
}

/// このモジュールの static の 1 つをロックする（持っている間のパニックは、何も途中のままに残さない）。
fn lock<T>(mutex: &'static Mutex<T>) -> MutexGuard<'static, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// `message` をブラウザのスレッドのウィンドウに送る。ウィンドウがもうなければ、呼び出しは害なく
/// 失敗する。
fn post(window: usize, message: u32) {
    // SAFETY: ただの呼び出し。システムがハンドルを確かめる。
    let _ = unsafe { PostMessageW(Some(HWND(window as *mut _)), message, WPARAM(0), LPARAM(0)) };
}

/// ブラウザのスレッドに頼むこと。
#[derive(Debug)]
pub enum Command {
    Show,
    Hide,
    Navigate(String),
    Back,
    Forward,
    Reload,
    Layout(PageLayout),
    Input(PageInput),
    Media(MediaCommand),
    /// このページ数だけスクロールする（負なら上）。
    Scroll(f32),
    Quit,
    /// スクリプトを動かし、その値を JSON で返す。
    #[cfg(test)]
    Eval(String, std::sync::mpsc::Sender<String>),
    /// ページがミュートかを返す。
    #[cfg(test)]
    IsMuted(std::sync::mpsc::Sender<bool>),
}

/// 1 つのブラウザのスレッドへの命令。
struct Mailbox {
    queue: Mutex<VecDeque<Command>>,
    /// スレッドのウィンドウ（あるまでは 0）。
    window: AtomicUsize,
}

impl Mailbox {
    fn post(&self, command: Command) {
        self.queue
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push_back(command);
        let window = self.window.load(Ordering::Acquire);
        if window != 0 {
            post(window, WM_APP_COMMAND);
        }
    }

    fn take(&self) -> Vec<Command> {
        self.queue
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain(..)
            .collect()
    }
}

struct Control {
    generation: u64,
    mailbox: Option<Arc<Mailbox>>,
    shown: bool,
    layout: PageLayout,
}

static CONTROL: Mutex<Control> = Mutex::new(Control {
    generation: 0,
    mailbox: None,
    shown: false,
    // オーバーレイがページのものを送るまで。
    layout: PageLayout {
        size: [800, 450],
        scale: 1.0,
        zoom: 1.0,
    },
});

fn control() -> MutexGuard<'static, Control> {
    lock(&CONTROL)
}

/// ブラウザのスレッドが始めるのに要るもの。
struct Start {
    /// `last_page` になければ開くページ。
    url: String,
    layout: PageLayout,
    /// WebView2 のユーザーデータのフォルダ（クッキー、キャッシュ）。
    data_dir: PathBuf,
    /// 最後に開いたページを残す場所（[`last_page`]）。
    last_page: PathBuf,
}

/// ブラウザを見せる。動いていなければ始める：最後に開いたページで、なければ `url` で。
/// `dir`：ブラウザのフォルダ（[`browser_dir`]）。
pub fn show(url: &str, dir: PathBuf) {
    let mut control = control();
    control.shown = true;
    if let Some(mailbox) = &control.mailbox {
        mailbox.post(Command::Show);
        return;
    }
    control.generation += 1;
    let generation = control.generation;
    let mailbox = Arc::new(Mailbox {
        queue: Mutex::new(VecDeque::new()),
        window: AtomicUsize::new(0),
    });
    control.mailbox = Some(mailbox.clone());
    let start = Start {
        url: url.to_owned(),
        layout: control.layout,
        data_dir: dir.join("WebView2"),
        last_page: dir.join("browser.json"),
    };
    drop(control);
    *lock(&SHARED) = Shared::new(generation, BrowserState::Starting);
    {
        let mut frame = lock(&FRAME);
        frame.generation = generation;
        frame.seq = 0;
        frame_changed();
    }
    if let Err(e) = thread::spawn(generation, mailbox, start) {
        let why = if e.kind() == std::io::ErrorKind::Unsupported {
            log::warn!("browser: {e}");
            "この DLL（GNU でビルドしたもの）ではブラウザを使えません"
        } else {
            log::error!("browser: cannot start its thread: {e}");
            "ブラウザのスレッドを起動できません"
        };
        thread_ended(generation, BrowserState::Failed(why.into()));
    }
}

/// ブラウザを隠す。動き続ける（動画は止める）。
pub fn hide() {
    let mut control = control();
    control.shown = false;
    if let Some(mailbox) = &control.mailbox {
        mailbox.post(Command::Hide);
    }
}

/// ブラウザを止める（そのプロセスは終わる）。
pub fn quit() {
    let mut control = control();
    control.shown = false;
    if let Some(mailbox) = control.mailbox.take() {
        mailbox.post(Command::Quit);
        let generation = control.generation;
        drop(control);
        stopped(generation, BrowserState::Off);
    }
}

/// ページの大きさと拡大率を、今か次の起動のために入れる。
pub fn set_layout(layout: PageLayout) {
    let mut control = control();
    if control.layout == layout {
        return;
    }
    control.layout = layout;
    if let Some(mailbox) = &control.mailbox {
        mailbox.post(Command::Layout(layout));
    }
}

/// 動いているブラウザに命令を渡す。なければ捨てる。
pub fn send(command: Command) {
    if let Some(mailbox) = &control().mailbox {
        mailbox.post(command);
    }
}

/// ブラウザを見せるか（まだ始まる途中かもしれない）。
pub fn shown() -> bool {
    control().shown
}

/// `version` から変わっていれば最新の絵（`version` も更新する）。ただし、ブラウザのスレッドが
/// いまそれを書いていれば別（そのときは最後に上げたものが残る）。
pub fn frame_if_new(version: &mut u64) -> Option<MutexGuard<'static, FrameSlot>> {
    let current = FRAME_VERSION.load(Ordering::Acquire);
    if current == *version {
        return None;
    }
    let frame = match FRAME.try_lock() {
        Ok(frame) => frame,
        Err(TryLockError::Poisoned(e)) => e.into_inner(),
        Err(TryLockError::WouldBlock) => return None,
    };
    *version = current;
    Some(frame)
}

/// このフレームでオーバーレイが見せるブラウザと、ブラウザの新しい通知。
pub fn before_frame() -> (BrowserView, Vec<Notice>) {
    let shown = shown();
    let mut shared = lock(&SHARED);
    let view = BrowserView {
        shown,
        ..shared.view.clone()
    };
    (view, std::mem::take(&mut shared.notices))
}

/// [`frame`] のガードからの、オーバーレイのための絵。
pub fn pixels(frame: &FrameSlot) -> BrowserPixels<'_> {
    BrowserPixels {
        generation: frame.generation,
        seq: frame.seq,
        size: frame.size,
        bgra: &frame.bgra,
    }
}

/// オーバーレイとブラウザのホットキーが頼んだことを、順に行う。
pub fn after_frame(
    commands: Vec<BrowserCommand>,
    actions: Vec<BrowserAction>,
    settings: &Settings,
    game_dir: &Path,
) {
    for command in commands {
        match command {
            BrowserCommand::Show => show(&settings.browser_url, browser_dir(game_dir)),
            BrowserCommand::Hide => hide(),
            BrowserCommand::Quit => quit(),
            BrowserCommand::Navigate(url) => send(Command::Navigate(url)),
            BrowserCommand::Back => send(Command::Back),
            BrowserCommand::Forward => send(Command::Forward),
            BrowserCommand::Reload => send(Command::Reload),
            BrowserCommand::Layout(layout) => set_layout(layout),
            BrowserCommand::Input(input) => send(Command::Input(input)),
        }
    }
    let seconds = f64::from(settings.browser_seek_seconds);
    for action in actions {
        match action {
            BrowserAction::Toggle if shown() => hide(),
            BrowserAction::Toggle => show(&settings.browser_url, browser_dir(game_dir)),
            BrowserAction::PageUp => send(Command::Scroll(-1.0)),
            BrowserAction::PageDown => send(Command::Scroll(1.0)),
            BrowserAction::PlayPause => send(Command::Media(MediaCommand::PlayPause)),
            BrowserAction::SeekBack => send(Command::Media(MediaCommand::Seek(-seconds))),
            BrowserAction::SeekForward => send(Command::Media(MediaCommand::Seek(seconds))),
        }
    }
}

/// ゲームのどのインスタンスも共有する、ブラウザ自身のフォルダ：`%LOCALAPPDATA%\reminedog`
/// （クッキー、つまりログインと、キャッシュを持つ WebView2 のユーザーデータ。最後のページ）。
fn browser_dir(game_dir: &Path) -> PathBuf {
    match std::env::var_os("LOCALAPPDATA") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir).join("reminedog"),
        _ => reminedog_core::data_dir(game_dir),
    }
}

/// 最後のページとして残す、いちばん長いアドレス。
const MAX_LAST_PAGE: usize = 8 * 1024;

/// `path` の、最後に開いたページ（`{"url": "https://..."}`）。
fn last_page(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let url = value.get("url")?.as_str()?;
    (is_web(url) && url.len() <= MAX_LAST_PAGE).then(|| url.to_owned())
}

/// `url` が分別のある長さのウェブのページなら、`path` に最後のページとして残す。
fn keep_last_page(path: &Path, url: &str) {
    if !is_web(url) || url.len() > MAX_LAST_PAGE {
        return;
    }
    let mut json = serde_json::json!({ "url": url }).to_string().into_bytes();
    json.push(b'\n');
    if let Err(e) = reminedog_core::write_file(path, &json) {
        log::debug!("browser: cannot keep the last page: {e}");
    }
}

fn is_web(url: &str) -> bool {
    url.starts_with("https://") || url.starts_with("http://")
}

/// 世代 `generation` は終わった：絵は消え、状態は `state` を言う。
fn stopped(generation: u64, state: BrowserState) {
    {
        let mut frame = lock(&FRAME);
        if frame.generation == generation {
            frame.generation = 0;
            frame.seq = 0;
            frame.bgra = Vec::new();
            frame_changed();
        }
    }
    let mut shared = lock(&SHARED);
    if shared.generation == generation {
        // 世代 0：終わるスレッドはもうここに書けない。
        *shared = Shared::new(0, state);
    }
}

/// ブラウザのスレッド `generation` が自分で終わった（失敗した）：忘れるので、次に見せるときは
/// 新しいものを始める。
fn thread_ended(generation: u64, state: BrowserState) {
    {
        let mut control = control();
        if control.generation == generation {
            control.mailbox = None;
        }
    }
    stopped(generation, state);
}

#[cfg(all(test, target_env = "msvc"))]
mod tests;

/// オーバーレイの側のブラウザ（[`reminedog_agent::os::Browser`]）。
pub struct WebView2;

pub static WEBVIEW2: WebView2 = WebView2;

impl Browser for WebView2 {
    fn before_frame(&self) -> (BrowserView, Vec<Notice>) {
        before_frame()
    }

    fn with_new_picture(&self, version: &mut u64, upload: &mut dyn FnMut(BrowserPixels<'_>)) {
        if let Some(frame) = frame_if_new(version) {
            upload(pixels(&frame));
        }
    }

    fn after_frame(
        &self,
        commands: Vec<BrowserCommand>,
        actions: Vec<BrowserAction>,
        settings: &Settings,
        game_dir: &Path,
    ) {
        after_frame(commands, actions, settings, game_dir);
    }
}
