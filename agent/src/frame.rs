//! バッファのスワップの中で毎フレーム行う処理（Minecraft 1.21 までは `glfwSwapBuffers`、
//! 26.x は `SDL_GL_SwapWindow`）。

use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, TryLockError};
use std::time::{Duration, Instant};

use glow::HasContext as _;
use reminedog_core::{InputId, Naming, Settings, input_by_name, input_name, settings_path};
use reminedog_render::{
    BrowserState, FrameInput, FrameParams, Hotkeys, Notice, Overlay, PointerSpeed, StatusLine,
    ZoomView, gl_summary, hotkeys, hotkeys_quiet, resolve,
};

use crate::gl::{self, OwnContext};
use crate::globals::{self, Globals};
use crate::input;
use crate::os;
use crate::saver;
use crate::tall::TallZoom;
use crate::waypoints;

/// 設定は、これだけ変更がなければ保存する（メニューで変えたなら、メニューを閉じたときに）。
/// スライダーを動かすたびにファイルを書かないように。
const SAVE_DELAY: Duration = Duration::from_secs(1);
/// 保存に失敗したら、これだけ後にもう一度試す。
const SAVE_RETRY: Duration = Duration::from_secs(10);
/// オーバーレイがなくなるときに、設定の保存を待つ時間。
const SAVE_WAIT: Duration = Duration::from_secs(2);
/// 保存に失敗した通知を出す秒数。
const NOTICE_SECONDS: f64 = 6.0;

/// これだけ続けてフレームに失敗したら、オーバーレイはそのセッションではあきらめる。
const MAX_FAILURES: u32 = 30;
/// 黙るまでに報告する GL のエラーの数。
const MAX_GL_ERRORS_LOGGED: u32 = 10;

enum State {
    Uninit,
    Ready(Box<Runtime>),
    Disabled,
}

/// プロセスに 1 つのオーバーレイ。スレッドをまたいでウィンドウについていく：Forge と
/// NeoForge は、Minecraft の描画のスレッドが引き継ぐ前に、早い読み込み画面のスレッドから
/// 同じウィンドウをスワップする。static なので捨てられることはなく、終了時に何も動かない。
static STATE: Mutex<State> = Mutex::new(State::Uninit);

/// ゲームのウィンドウを持つライブラリ（GLFW か SDL3）から、オーバーレイが要るもの。
pub trait WindowSystem: Sync {
    /// 名前と版（ログと状態の窓のため）。
    fn describe(&self) -> String;
    /// 隠れた補助のウィンドウ（Mod、Minecraft 26.x の描画）にはオーバーレイを描かない。
    fn is_visible(&self, window: *mut c_void) -> bool;
    /// 描画先の大きさ（ピクセル）。ウィンドウをスワップするスレッドで呼ぶこと。
    fn framebuffer_size(&self, window: *mut c_void) -> (i32, i32);
    /// ウィンドウの DPI の拡大率。
    fn content_scale(&self, window: *mut c_void) -> f32;
    /// `window` の裏の OS のウィンドウ（[`crate::os`]）。
    fn native_window(&self, window: *mut c_void) -> Option<usize>;
    /// ウィンドウが最小化されているか。
    fn minimized(&self, window: *mut c_void) -> bool;
    /// ゲームがカーソルを捕まえている間に、システムのポインターの動きではなく、システムの
    /// ポインターの速度と加速が効いていない生のマウスの数を受け取っているか（そのときは、
    /// オーバーレイが自前のカーソルにそれらを掛ける）。
    fn raw_motion(&self, window: *mut c_void) -> bool;
    /// ゲームがカーソルを捕まえているか（プレイ中で、メニューを出していない）。
    fn captured(&self, window: *mut c_void) -> bool;
    /// このライブラリのもとで、ゲームの options.txt がキーをどう名付けるか。
    fn naming(&self) -> Naming;
    /// このライブラリのもとで、ゲームに `id` を（キーかマウスのボタンのイベントとして）渡せるか。
    fn can_send(&self, id: InputId) -> bool;
    /// このライブラリが `id` の押下を報告するか（ルールの元のキーはそうでなければならない）。
    fn can_receive(&self, id: InputId) -> bool;
}

/// ウィンドウのシステムが見せる直前に、後ろのバッファにオーバーレイを描く。
pub fn before_swap(ws: &dyn WindowSystem, window: *mut c_void) {
    let Some(agent) = globals::globals() else {
        return;
    };
    if !agent.options.overlay || window.is_null() {
        return;
    }
    // 最小化されたウィンドウには描く先がない（Windows では、GetDC がウィンドウ自身の DC ではなく、
    // 呼ぶたびに新しい一時的な DC を返す）。フレームを飛ばし、オーバーレイは残す。
    // ズームは終える：ゲームは見えない縦長のフレームを描き続けてしまう。
    let os = os::get();
    let minimized = os
        .window_minimized(ws.native_window(window))
        .unwrap_or_else(|| ws.minimized(window));
    if minimized {
        if let Ok(mut state) = STATE.try_lock()
            && let State::Ready(runtime) = &mut *state
            && runtime.window == window as usize
        {
            runtime.tall.release();
        }
        return;
    }
    // 取れないのは、別のスレッドが別のウィンドウをスワップしているため。毒されているのは、
    // フレームの途中でパニックしたため（先に状態を Disabled にした）。どちらでもフレームを飛ばす。
    let mut state = match STATE.try_lock() {
        Ok(state) => state,
        Err(TryLockError::WouldBlock) => return,
        Err(TryLockError::Poisoned(_)) => {
            // 止まったままにし、見えない UI がゲームの入力を取り続けないようにする。
            input::router().set_enabled(false);
            return;
        }
    };
    // 作業の間は Disabled にしておく：下で何かがパニックしても、オーバーレイは毎フレーム失敗し直す
    // のではなく、止まったままになる。
    let next = match std::mem::replace(&mut *state, State::Disabled) {
        State::Disabled => State::Disabled,
        // 隠れた補助のウィンドウ（作る Mod がある）にはオーバーレイを描かない。見えるウィンドウを待つ。
        State::Uninit if !ws.is_visible(window) => State::Uninit,
        // オーバーレイのコンテキストを作れるスレッドのスワップの前にも、作らない。
        State::Uninit if !os.can_create_context_here() => State::Uninit,
        State::Uninit => match Runtime::create(ws, window, agent) {
            Ok(runtime) => run_frame(runtime, ws, window, agent),
            Err(e) => {
                log::error!("overlay disabled: {e}");
                State::Disabled
            }
        },
        State::Ready(runtime) => run_frame(runtime, ws, window, agent),
    };
    if matches!(next, State::Disabled) {
        input::router().set_enabled(false);
    }
    *state = next;
}

/// ゲームがマウスを捕まえている間に、相対移動でオーバーレイのカーソルをどう動かすか。
fn pointer_speed(ws: &dyn WindowSystem, window: *mut c_void) -> PointerSpeed {
    if ws.raw_motion(window) {
        os::get().pointer_speed()
    } else {
        log::debug!("the game reads the system pointer's motion; no pointer speed applied");
        PointerSpeed::RAW
    }
}

/// ゲームのコンテキストがウィンドウを描く描画先（[`os::Os::window_surface`]）。
fn window_surface(ws: &dyn WindowSystem, window: *mut c_void) -> usize {
    os::get().window_surface(ws.native_window(window))
}

fn run_frame(
    mut runtime: Box<Runtime>,
    ws: &dyn WindowSystem,
    window: *mut c_void,
    agent: &Globals,
) -> State {
    match runtime.render(ws, window, agent) {
        Ok(()) => {
            runtime.failures = 0;
            State::Ready(runtime)
        }
        Err(FrameError::DrawableChanged) => {
            log::warn!("the game's device context changed; recreating the overlay context");
            runtime.teardown();
            State::Uninit
        }
        Err(FrameError::Failed(e)) => {
            runtime.failures += 1;
            if runtime.failures == 1 {
                log::warn!("overlay frame failed: {e}");
            }
            if runtime.failures >= MAX_FAILURES {
                log::error!("overlay disabled after {MAX_FAILURES} failed frames in a row: {e}");
                runtime.teardown();
                State::Disabled
            } else {
                State::Ready(runtime)
            }
        }
    }
}

enum FrameError {
    DrawableChanged,
    Failed(String),
}

struct Runtime {
    /// オーバーレイを描くウィンドウ。ほかのウィンドウのスワップには触れない。
    window: usize,
    context: OwnContext,
    gl: Arc<glow::Context>,
    overlay: Overlay,
    /// 変わらない診断と、最後に 1 フレームの処理時間。
    status: Vec<StatusLine>,
    /// オーバーレイの 1 フレームの CPU 時間をならしたもの（コンテキストの切り替えを含む）。ミリ秒。
    cost_ms: f64,
    failures: u32,
    gl_errors_logged: u32,
    /// 前のフレームで UI が開いていた。
    ui_was_open: bool,
    settings: Settings,
    settings_path: PathBuf,
    /// 変わってから保存していなければ、設定を保存する時刻と、メニューで変わったか（そのときは
    /// メニューを閉じたときに保存する）。
    unsaved: Option<(Instant, bool)>,
    /// 保存のスレッドでの保存。
    saving: Option<saver::Pending>,
    /// 読んだときの設定ファイルの問題（[`settings_problem`]）。
    load_problem: Option<String>,
    /// 最後の保存に失敗した理由。
    save_problem: Option<String>,
    /// 次のフレームのため。
    notices: Vec<Notice>,
    /// 最後に取ったブラウザの絵（[`os::Browser::with_new_picture`]）。
    page_version: u64,
    tall: TallZoom,
    /// `settings` から作った router のホットキー。
    hotkeys: Hotkeys,
}

// SAFETY: GL のハンドルと物は、STATE をロックしている間のスワップのフックの中でだけ使い、
// そのとき自前のコンテキストを呼び出したスレッドで今のものにし、ロックを放す前に外す。
// コンテキストはどのスレッドで今のものにしてもよい（1 度に 1 つ）。
unsafe impl Send for Runtime {}

impl Runtime {
    fn create(
        ws: &dyn WindowSystem,
        window: *mut c_void,
        agent: &Globals,
    ) -> Result<Box<Self>, String> {
        let os = os::get();
        let surface = window_surface(ws, window);
        if surface == 0 {
            return Err("cannot get the window's surface".into());
        }
        let (current_surface, game_context) = os.current_gl();
        let (width, height) = ws.framebuffer_size(window);
        // ゲームのコンテキストを説明するのは、それがこのウィンドウで今のものであるときだけ。
        let game_gl = if current_surface == surface && game_context != 0 {
            gl::describe_current()
        } else {
            "? (the window's context was not current)".to_owned()
        };
        log::info!("window system: {}", ws.describe());
        log::info!(
            "window framebuffer {width}x{height}, content scale {:.2}",
            ws.content_scale(window)
        );
        log::info!("game GL: {game_gl}");
        log::info!("pixel format {}", os.describe_surface(surface));

        let settings_path = settings_path(&agent.game_dir);
        let settings = Settings::load(&settings_path);
        let load_problem = settings_problem(&settings);
        log::info!(
            "settings: {} (menu {}, zoom {}, waypoint {}, navigate {}, {})",
            settings_path.display(),
            settings.menu_key,
            settings.zoom_key,
            settings.waypoint_key,
            settings.navigate_key,
            rebinds_summary(&settings)
        );

        let context = OwnContext::create(surface)?;
        let created = (|| {
            let _current = context.make_current()?;
            // SAFETY: 自前のコンテキストが今のもので、glow のコンテキストはその間だけ使う。
            let gl = Arc::new(unsafe { gl::glow_context() });
            let overlay_gl = gl_summary(&gl);
            log::info!("overlay GL: {overlay_gl}");
            let overlay = Overlay::new(gl.clone(), crate::fonts::japanese(), settings.clone())
                .map_err(|e| e.to_string())?;
            Ok::<_, String>((gl, overlay, overlay_gl))
        })();
        let (gl, overlay, overlay_gl) = match created {
            Ok(created) => created,
            Err(e) => {
                context.delete();
                return Err(e);
            }
        };
        log::info!("overlay: initialized");
        let keys = hotkeys(&settings);
        let rules = rebind_rules(ws, &settings, &keys);
        let active = {
            let mut router = input::router();
            router.set_hotkeys(keys);
            router.set_rebinds(rules, true);
            router.set_pointer_speed(pointer_speed(ws, window));
            router.rebinds().to_vec()
        };
        log_active_rebinds(&active);

        let status = vec![
            StatusLine::new(
                "ビルド",
                format!("{} ({})", crate::VERSION, crate::BUILD_ID),
            ),
            StatusLine::new("ウィンドウ", ws.describe()),
            StatusLine::new("ゲームのGL", game_gl),
            StatusLine::new("オーバーレイのGL", overlay_gl),
            StatusLine::new("ゲームフォルダ", screen_path(&agent.game_dir)),
            StatusLine::new("処理時間", "-"),
        ];
        Ok(Box::new(Self {
            window: window as usize,
            context,
            gl,
            overlay,
            status,
            cost_ms: 0.0,
            failures: 0,
            gl_errors_logged: 0,
            ui_was_open: false,
            settings,
            settings_path,
            unsaved: None,
            saving: None,
            load_problem,
            save_problem: None,
            notices: Vec::new(),
            page_version: os::NO_PICTURE,
            tall: TallZoom::default(),
            hotkeys: keys,
        }))
    }

    fn render(
        &mut self,
        ws: &dyn WindowSystem,
        window: *mut c_void,
        agent: &Globals,
    ) -> Result<(), FrameError> {
        if window as usize != self.window {
            return Ok(());
        }
        let os = os::get();
        let surface = window_surface(ws, window);
        if surface == 0 {
            return Err(FrameError::Failed("cannot get the window's surface".into()));
        }
        if surface != self.context.surface() {
            log::debug!(
                "surface 0x{:x} -> 0x{surface:x} (native window {:?}, current {:x?})",
                self.context.surface(),
                ws.native_window(window),
                os.current_gl()
            );
            return Err(FrameError::DrawableChanged);
        }
        let (width, height) = ws.framebuffer_size(window);
        if width <= 0 || height <= 0 {
            // 最小化している。
            return Ok(());
        }
        let scale = ws.content_scale(window) * agent.options.ui_scale.unwrap_or(1.0);

        let started = Instant::now();
        // ポインターの設定はゲームの動いている間に変わりうる。UI を開くたびに読む（Ctrl+I の直後の
        // いくつかのイベントは、前の設定のままのことがある）。
        let ui_open = input::router().ui_open();
        if ui_open && !self.ui_was_open {
            input::router().set_pointer_speed(pointer_speed(ws, window));
        }
        self.ui_was_open = ui_open;
        // 入力欄の Ctrl+V：クリップボードは router のロックを外して読み、その文字はこのフレームの
        // イベントと一緒に入れる。
        if input::router().take_paste_request()
            && let Some(text) = os.read_clipboard(ws.native_window(window))
        {
            input::router().paste(&text);
        }

        // ズームはゲームのコンテキストで働く（スワップのときに今のもの）。ゲームがメニューを出したら
        // 終わる（ズームのキーを押したままインベントリを開いた）。
        let playing = ws.captured(window);
        let want_zoom = input::router().zoom_active() && playing;
        let zoom = self.tall.frame(
            window,
            [width, height],
            want_zoom && self.settings.zoom_high_res,
            self.settings.zoom_factor,
        );
        let zoom = if want_zoom && zoom == ZoomView::Off {
            ZoomView::Magnify
        } else {
            zoom
        };
        let high_res_note = self
            .tall
            .failure()
            .map(|reason| format!("高精細は使えない：{reason}"));
        let (waypoint_commands, browser_commands, browser_actions) = {
            let _current = self.context.make_current().map_err(FrameError::Failed)?;
            // router のホットキーの操作を取るので、下で router をロックする前に。
            let marks = waypoints::before_frame(&agent.game_dir, self.window, playing, ws.naming());
            let (browser, browser_notices) = os
                .browser()
                .map(|browser| browser.before_frame())
                .unwrap_or_default();
            let browser_shown = browser.shown;
            // ブラウザのキーは、操作するページがある間だけ。
            let browser_keys = browser_shown && browser.state == BrowserState::Ready;
            // 出すのはメニューだけ。
            let (game_bindings, unsupported_inputs) = if ui_open {
                (
                    waypoints::game_bindings(),
                    unsupported_inputs(ws, &self.settings),
                )
            } else {
                (Vec::new(), Vec::new())
            };
            let (input, browser_actions) = {
                let mut router = input::router();
                router.set_enabled(true);
                router.set_screen([width as u32, height as u32], scale);
                router.set_browser_shown(browser_keys);
                let input = FrameInput {
                    events: router.take_events(),
                    modifiers: router.modifiers(),
                    ui_open: router.ui_open(),
                    zoom,
                    software_cursor: router.software_cursor(),
                    captured: router.take_captured(),
                    high_res_note,
                    waypoints: marks.view,
                    notices: marks
                        .notices
                        .into_iter()
                        .chain(browser_notices)
                        .chain(self.notices.drain(..))
                        .collect(),
                    reserved_keys: marks.reserved_keys,
                    game_bindings,
                    rebind_note: None,
                    settings_note: self.settings_note(),
                    unsupported_inputs,
                    browser,
                };
                (input, router.take_browser_actions())
            };
            // ブラウザのスレッドが待つのは新しい絵を上げる間だけ。いま書いている絵は次のフレームを
            // 待つ。隠している間は最後の絵が残る。
            if browser_shown && let Some(browser) = os.browser() {
                let overlay = &mut self.overlay;
                browser.with_new_picture(&mut self.page_version, &mut |pixels| {
                    // SAFETY: 自前のコンテキストが今のもの。
                    unsafe { overlay.upload_page(Some(pixels)) }
                });
            }
            let output = self.overlay.render(FrameParams {
                framebuffer_size: [width as u32, height as u32],
                pixels_per_point: scale,
                time: agent.start.elapsed().as_secs_f64(),
                status: &self.status,
                input,
            });
            // ルールを渡し直すのは、ルールかホットキーが変わったときだけ：解決するとログに出る（ズームの
            // スライダーは、動かしている間、毎フレーム設定を変える）。ホットキーの問題は、設定を読んだ
            // ときにログに出した。
            let changed = output.settings.as_ref().map(|settings| {
                let keys = hotkeys_quiet(settings);
                let rules = (settings.rebinds_enabled != self.settings.rebinds_enabled
                    || settings.rebinds != self.settings.rebinds
                    || keys != self.hotkeys)
                    .then(|| rebind_rules(ws, settings, &keys));
                (keys, rules)
            });
            let active = {
                let mut router = input::router();
                router.set_text_focus(output.text_focus);
                if output.close_ui {
                    router.set_ui_open(false);
                }
                if output.start_capture {
                    router.start_capture();
                }
                if output.start_input_capture {
                    router.start_input_capture();
                }
                if output.cancel_capture {
                    router.cancel_capture();
                }
                match &changed {
                    Some((keys, rules)) => {
                        router.set_hotkeys(*keys);
                        rules.as_ref().map(|rules| {
                            router.set_rebinds(rules.clone(), true);
                            router.rebinds().to_vec()
                        })
                    }
                    None => None,
                }
            };
            if let Some(active) = active {
                log_active_rebinds(&active);
            }
            if let Some((keys, _)) = changed {
                self.hotkeys = keys;
            }
            if let Some(settings) = output.settings {
                if settings.zoom_factor != self.settings.zoom_factor
                    || settings.zoom_high_res != self.settings.zoom_high_res
                {
                    self.tall.retry();
                }
                self.settings = settings;
                self.unsaved = Some((Instant::now() + SAVE_DELAY, ui_open));
            }
            if let Some(text) = &output.copied_text
                && !os.write_clipboard(ws.native_window(window), text)
            {
                log::debug!("cannot put the menu's copied text on the clipboard");
            }
            self.log_gl_errors();
            (
                output.waypoint_commands,
                output.browser_commands,
                browser_actions,
            )
        };
        waypoints::after_frame(waypoint_commands, self.window);
        if let Some(browser) = os.browser() {
            browser.after_frame(
                browser_commands,
                browser_actions,
                &self.settings,
                &agent.game_dir,
            );
        }
        self.save_settings_when_due();
        let cost_ms = started.elapsed().as_secs_f64() * 1000.0;
        self.cost_ms = if self.overlay.frames() <= 1 {
            cost_ms
        } else {
            self.cost_ms * 0.95 + cost_ms * 0.05
        };
        if let Some(line) = self.status.last_mut() {
            line.value = format!("{:.2} ms", self.cost_ms);
        }

        match self.overlay.frames() {
            1 => {
                log::info!("overlay: first frame rendered ({width}x{height}, scale {scale:.2})");
            }
            n @ (100 | 1000 | 10_000) => log::info!(
                "overlay: {n} frames rendered, {:.2} ms per frame",
                self.cost_ms
            ),
            _ => {}
        }
        Ok(())
    }

    /// 保存する時が来たら設定を保存のスレッドに渡し、前の保存の結果を受け取る。保存の失敗は
    /// 1 回伝え、後でもう一度試す。
    fn save_settings_when_due(&mut self) {
        if let Some(saving) = &self.saving {
            let Some(result) = saving.poll() else {
                return;
            };
            self.saving = None;
            self.saved(result);
        }
        let Some((due, in_menu)) = self.unsaved else {
            return;
        };
        let menu_closed = in_menu && !input::router().ui_open();
        if Instant::now() < due && !menu_closed {
            return;
        }
        self.unsaved = None;
        if self.settings.unreadable {
            log::debug!("settings: not saved; the file could not be read");
            return;
        }
        match self.settings.to_json() {
            Ok(bytes) => self.saving = Some(saver::write(self.settings_path.clone(), bytes)),
            Err(e) => self.saved(Err(e)),
        }
    }

    fn saved(&mut self, result: std::io::Result<()>) {
        match result {
            Ok(()) => {
                if self.save_problem.take().is_some() {
                    log::info!("settings saved after an earlier failure");
                }
                log::info!(
                    "settings saved (menu {}, zoom {}, waypoint {}, navigate {}, ×{:.1}, high resolution {}, {})",
                    self.settings.menu_key,
                    self.settings.zoom_key,
                    self.settings.waypoint_key,
                    self.settings.navigate_key,
                    self.settings.zoom_factor,
                    self.settings.zoom_high_res,
                    rebinds_summary(&self.settings)
                );
            }
            Err(e) => {
                let problem = format!("設定を保存できなかった：{e}（あとで保存し直す）");
                if self.save_problem.as_ref() != Some(&problem) {
                    log::error!(
                        "cannot save settings to {}: {e}",
                        self.settings_path.display()
                    );
                    self.notices
                        .push(Notice::new(problem.clone(), true, NOTICE_SECONDS));
                    self.save_problem = Some(problem);
                }
                // 新しい変更は自分の時に保存する。そうでなければ、これをもう一度、後で。
                self.unsaved
                    .get_or_insert((Instant::now() + SAVE_RETRY, false));
            }
        }
    }

    /// 設定ファイルについてメニューが言うこと。
    fn settings_note(&self) -> Option<String> {
        let notes: Vec<&str> = [self.load_problem.as_deref(), self.save_problem.as_deref()]
            .into_iter()
            .flatten()
            .collect();
        (!notes.is_empty()).then(|| notes.join("\n"))
    }

    /// オーバーレイがなくなる前に、まだ保存していないものを保存し（ファイルから作り直すため）、
    /// 少し待つ。
    fn flush_settings(&mut self) {
        if let Some(saving) = self.saving.take()
            && let Some(result) = saving.wait(SAVE_WAIT)
        {
            self.saved(result);
        }
        if self.unsaved.take().is_none() || self.settings.unreadable {
            return;
        }
        let result = self
            .settings
            .to_json()
            .map(|bytes| saver::write(self.settings_path.clone(), bytes))
            .map(|saving| saving.wait(SAVE_WAIT));
        match result {
            Ok(Some(result)) => self.saved(result),
            Ok(None) => log::warn!("settings: still saving as the overlay goes"),
            Err(e) => self.saved(Err(e)),
        }
    }

    /// 自前のコンテキストの GL のエラーのキューを空にする（ゲームのものには触れない）。
    fn log_gl_errors(&mut self) {
        for _ in 0..8 {
            // SAFETY: 自前のコンテキストが今のもの。
            let error = unsafe { self.gl.get_error() };
            if error == glow::NO_ERROR {
                break;
            }
            self.gl_errors_logged += 1;
            if self.gl_errors_logged <= MAX_GL_ERRORS_LOGGED {
                log::warn!("GL error 0x{error:04X} in the overlay context");
            }
        }
    }

    /// 設定を保存し、ズームを終え（スワップのときと同じく、ゲームのコンテキストを今のものにして）、
    /// GL の資源を解放し、コンテキストを消す。できる範囲で：DC がもうないこともある。
    fn teardown(mut self: Box<Self>) {
        self.flush_settings();
        self.tall.release();
        let Runtime {
            context,
            mut overlay,
            ..
        } = *self;
        match context.make_current() {
            Ok(_current) => overlay.destroy(),
            Err(e) => log::warn!("cannot free overlay resources: {e}"),
        }
        context.delete();
    }
}

/// 画面に出す `path`。利用者のフォルダを `%APPDATA%` などに（Windows）、ホームのフォルダを
/// `~` に（Linux、macOS）置き換える：状態は配信中に画面に出ることがあり、そのままのパスには
/// 利用者の名前が入る。
fn screen_path(path: &std::path::Path) -> String {
    hide_user(&path.display().to_string(), |var| {
        std::env::var(var).ok().filter(|dir| !dir.is_empty())
    })
}

/// `text`（パス）の、最初に当てはまる利用者のフォルダをその名前に置き換えたもの。
/// `var` は環境変数を読む。
fn hide_user(text: &str, var: impl Fn(&str) -> Option<String>) -> String {
    for name in ["APPDATA", "LOCALAPPDATA", "USERPROFILE"] {
        let Some(dir) = var(name) else {
            continue;
        };
        let dir = dir.trim_end_matches('\\');
        if let Some(head) = text.get(..dir.len())
            && head.eq_ignore_ascii_case(dir)
            && (text.len() == dir.len() || text[dir.len()..].starts_with('\\'))
        {
            return format!("%{name}%{}", &text[dir.len()..]);
        }
    }
    // Unix のパス：大文字と小文字を区別し、区切りは "/" だけ。（Git のシェルは Windows でも
    // HOME を設定する。そこでは上のフォルダが先に当たる。）
    if let Some(home) = var("HOME") {
        let home = home.trim_end_matches('/');
        if !home.is_empty()
            && let Some(rest) = text.strip_prefix(home)
            && (rest.is_empty() || rest.starts_with('/'))
        {
            return format!("~{rest}");
        }
    }
    text.to_owned()
}

/// そのままでは読めなかった設定ファイルについての、メニューの注記。
fn settings_problem(settings: &Settings) -> Option<String> {
    if let Some(moved) = &settings.moved_aside_to {
        let name = moved
            .file_name()
            .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
        return Some(format!(
            "settings.json の書き方に誤りがあったので reminedog/{name} に移し、既定の設定で起動した（直して settings.json に戻すと、次の起動で読み込む）"
        ));
    }
    settings.unreadable.then(|| {
        "settings.json を読み込めなかったので既定の設定で動いている（上書きしないよう、この起動中は設定を保存しない）"
            .to_owned()
    })
}

/// `settings` のキーの置き換えのルールのうち、router が使うもの（[`resolve`]）：元のキーを
/// ウィンドウのライブラリが報告できないもの、出力をゲームに渡せないものを除く。
fn rebind_rules(
    ws: &dyn WindowSystem,
    settings: &Settings,
    keys: &Hotkeys,
) -> Vec<(InputId, InputId)> {
    resolve(settings, keys, &unsupported_inputs(ws, settings))
}

/// `settings` のルールのキーとボタンのうち、ウィンドウのライブラリが報告できない（元のキーと
/// して）か、ゲームに渡せない（出力として）もの。
fn unsupported_inputs(ws: &dyn WindowSystem, settings: &Settings) -> Vec<InputId> {
    let mut ids = Vec::new();
    for rule in &settings.rebinds {
        let from = input_by_name(&rule.from, Naming::Modern).filter(|&id| !ws.can_receive(id));
        let to = input_by_name(&rule.to, Naming::Modern).filter(|&id| !ws.can_send(id));
        for id in from.into_iter().chain(to) {
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
    }
    ids
}

/// 最終的に router が使うルール。
fn log_active_rebinds(rules: &[(InputId, InputId)]) {
    let list: Vec<String> = rules
        .iter()
        .map(|&(from, to)| format!("{} -> {}", input_name(from), input_name(to)))
        .collect();
    log::info!("rebinds: {} active ({})", rules.len(), list.join(", "));
}

/// 設定のログの行のための `rebinds 2` か `rebinds off (2)`。
fn rebinds_summary(settings: &Settings) -> String {
    if settings.rebinds_enabled {
        format!("rebinds {}", settings.rebinds.len())
    } else {
        format!("rebinds off ({})", settings.rebinds.len())
    }
}

#[cfg(test)]
mod tests {
    use super::hide_user;

    fn env<'a>(vars: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| {
            vars.iter()
                .find(|(var, _)| *var == name)
                .map(|(_, value)| (*value).to_owned())
        }
    }

    #[test]
    fn windows_folders_hide_the_user_name() {
        let vars = [
            ("APPDATA", r"C:\Users\Alice\AppData\Roaming"),
            ("USERPROFILE", r"C:\Users\Alice"),
            ("HOME", "/c/Users/Alice"),
        ];
        assert_eq!(
            hide_user(r"c:\users\alice\AppData\Roaming\.minecraft", env(&vars)),
            r"%APPDATA%\.minecraft"
        );
        assert_eq!(
            hide_user(r"C:\Users\Alice\games\mc", env(&vars)),
            r"%USERPROFILE%\games\mc"
        );
        assert_eq!(hide_user(r"D:\mc", env(&vars)), r"D:\mc");
    }

    #[test]
    fn the_home_folder_becomes_a_tilde() {
        let vars = [("HOME", "/home/alice/")];
        assert_eq!(
            hide_user("/home/alice/.local/share/PrismLauncher", env(&vars)),
            "~/.local/share/PrismLauncher"
        );
        assert_eq!(hide_user("/home/alice", env(&vars)), "~");
        // 同じ文字で始まるだけの、別の利用者のフォルダ。
        assert_eq!(hide_user("/home/alice2/mc", env(&vars)), "/home/alice2/mc");
        assert_eq!(hide_user("/srv/mc", env(&vars)), "/srv/mc");
        // "/" のホームは何も隠さない。
        assert_eq!(hide_user("/srv/mc", env(&[("HOME", "/")])), "/srv/mc");
    }
}
