//! The in-game browser: a WebView2 (Microsoft Edge) that draws off screen on a thread of its
//! own, shown by the overlay as a picture of the page.
//!
//! The browser thread ([`thread`]) owns the WebView ([`webview`]) and copies what it draws
//! into [`FRAME`] ([`capture`]); the game's thread uploads the newest picture to a texture
//! each frame. The game's thread never waits for the browser: it queues commands in the
//! thread's [`Mailbox`] and wakes it with a window message.
//!
//! Nothing exists until the browser is first shown. Each start gets a new generation; the
//! thread of an older one (still closing) no longer writes [`FRAME`] or [`SHARED`].
//!
//! The last page opened is kept in `browser.json` next to WebView2's data (not in the game
//! folder's settings: an address can hold tokens), and the next start opens it.
//!
//! Locks: [`SHARED`], [`FRAME`] and a mailbox's queue are leaves: none is held while taking
//! another one, or while calling into WebView2. [`CONTROL`] is held while a command goes into
//! the mailbox's queue and its window is woken (never the other way round).
//!
//! Only the MSVC build has the browser itself. Its thread imports Direct3D 11, WinRT and
//! COM (CoreMessaging, combase), which an agent built for Wine (GNU) could not load with:
//! a missing import keeps the whole agent, and so the game, from starting.
#![cfg_attr(not(target_env = "msvc"), allow(dead_code))]

#[cfg(target_env = "msvc")]
mod capture;
#[cfg(target_env = "msvc")]
mod thread;
#[cfg(target_env = "msvc")]
mod webview;

/// The GNU build's stand-in: the browser cannot start.
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

use reminedog_core::Settings;
use reminedog_core::browser::{MediaCommand, PageInput};
use reminedog_render::{
    BrowserAction, BrowserCommand, BrowserPixels, BrowserState, BrowserView, Notice, PageLayout,
};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

/// Wakes the browser thread to run its mailbox.
const WM_APP_COMMAND: u32 = WM_APP + 1;
/// A picture of the page is ready to be copied.
const WM_APP_FRAME: u32 = WM_APP + 2;

/// The newest picture of the page.
pub struct FrameSlot {
    /// The browser it came from (0: none).
    pub generation: u64,
    /// Pixels.
    pub size: [u32; 2],
    /// `size[0] * size[1]` pixels, top row first, 4 bytes each (blue, green, red, alpha).
    pub bgra: Vec<u8>,
    /// Counts the pictures of this generation (0: none yet).
    pub seq: u64,
}

static FRAME: Mutex<FrameSlot> = Mutex::new(FrameSlot {
    generation: 0,
    size: [0, 0],
    bgra: Vec::new(),
    seq: 0,
});

/// Counts the changes of [`FRAME`] (made with it locked), so the game's thread locks it only
/// for a new picture.
static FRAME_VERSION: AtomicU64 = AtomicU64::new(0);

/// A version no picture has: [`frame_if_new`] takes the next one.
pub const NO_PICTURE: u64 = u64::MAX;

/// Notes a change of [`FRAME`]; call with it locked.
fn frame_changed() {
    FRAME_VERSION.fetch_add(1, Ordering::Release);
}

/// What the browser thread reports.
#[derive(Debug, Clone)]
struct Shared {
    generation: u64,
    /// All but `shown`, which the game's thread knows ([`Control::shown`]).
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

/// Updates the shared state if `generation` is still the current browser.
fn report(generation: u64, update: impl FnOnce(&mut Shared)) {
    let mut shared = lock(&SHARED);
    if shared.generation == generation {
        update(&mut shared);
    }
}

/// How long the browser's notices show, in seconds.
const NOTICE_SECONDS: f64 = 2.5;

fn notice(text: impl Into<String>, warn: bool) -> Notice {
    Notice::new(text, warn, NOTICE_SECONDS)
}

/// Locks one of the module's statics (a panic while holding it left nothing half done).
fn lock<T>(mutex: &'static Mutex<T>) -> MutexGuard<'static, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// Posts `message` to a window of the browser thread; once the window is gone the call fails
/// harmlessly.
fn post(window: usize, message: u32) {
    // SAFETY: plain call; the system checks the handle.
    let _ = unsafe { PostMessageW(Some(HWND(window as *mut _)), message, WPARAM(0), LPARAM(0)) };
}

/// What the browser thread is asked to do.
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
    /// Scrolls by this many pages (negative: up).
    Scroll(f32),
    Quit,
    /// Runs a script and sends back its value as JSON.
    #[cfg(test)]
    Eval(String, std::sync::mpsc::Sender<String>),
    /// Sends back whether the page is muted.
    #[cfg(test)]
    IsMuted(std::sync::mpsc::Sender<bool>),
}

/// Commands for one browser thread.
struct Mailbox {
    queue: Mutex<VecDeque<Command>>,
    /// The thread's window (0 until it exists).
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
    // Until the overlay sends the page's.
    layout: PageLayout {
        size: [800, 450],
        scale: 1.0,
        zoom: 1.0,
    },
});

fn control() -> MutexGuard<'static, Control> {
    lock(&CONTROL)
}

/// What the browser thread needs to start.
struct Start {
    /// The page to open when `last_page` has none.
    url: String,
    layout: PageLayout,
    /// WebView2's user data folder (cookies, cache).
    data_dir: PathBuf,
    /// Where the last page opened is kept ([`last_page`]).
    last_page: PathBuf,
}

/// Shows the browser, starting it if it is not running: on the last page opened, else on
/// `url`. `dir`: the browser's folder ([`browser_dir`]).
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

/// Hides the browser; it keeps running (with its video paused).
pub fn hide() {
    let mut control = control();
    control.shown = false;
    if let Some(mailbox) = &control.mailbox {
        mailbox.post(Command::Hide);
    }
}

/// Stops the browser (its processes exit).
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

/// Sets the page's size and scale, now or for the next start.
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

/// Hands the running browser a command; without one it is dropped.
pub fn send(command: Command) {
    if let Some(mailbox) = &control().mailbox {
        mailbox.post(command);
    }
}

/// Whether the browser is to be seen (it may still be starting).
pub fn shown() -> bool {
    control().shown
}

/// The newest picture if it changed since `version` (which it updates), unless the browser
/// thread is writing it right now (then the last one uploaded stays).
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

/// What the overlay shows of the browser this frame, and the browser's new notices.
pub fn before_frame() -> (BrowserView, Vec<Notice>) {
    let shown = shown();
    let mut shared = lock(&SHARED);
    let view = BrowserView {
        shown,
        ..shared.view.clone()
    };
    (view, std::mem::take(&mut shared.notices))
}

/// The picture for the overlay, from a [`frame`] guard.
pub fn pixels(frame: &FrameSlot) -> BrowserPixels<'_> {
    BrowserPixels {
        generation: frame.generation,
        seq: frame.seq,
        size: frame.size,
        bgra: &frame.bgra,
    }
}

/// Carries out what the overlay and the browser's hotkeys asked for, in order.
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

/// The browser's own folder, shared by every instance of the game: `%LOCALAPPDATA%\reminedog`
/// (WebView2's user data with cookies, so logins, and the cache; the last page).
fn browser_dir(game_dir: &Path) -> PathBuf {
    match std::env::var_os("LOCALAPPDATA") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir).join("reminedog"),
        _ => reminedog_core::data_dir(game_dir),
    }
}

/// The longest address kept as the last page.
const MAX_LAST_PAGE: usize = 8 * 1024;

/// The last page opened, from `path` (`{"url": "https://..."}`).
fn last_page(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let url = value.get("url")?.as_str()?;
    (is_web(url) && url.len() <= MAX_LAST_PAGE).then(|| url.to_owned())
}

/// Keeps `url` as the last page in `path`, if it is a web page of a sensible length.
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

/// Generation `generation` is over: the picture goes and the state says `state`.
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
        // Generation 0: the ending thread can no longer write here.
        *shared = Shared::new(0, state);
    }
}

/// The browser thread `generation` ended on its own (it failed): forget it, so the next
/// show starts a new one.
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
