//! WebView2 そのもの：作成、その設定とイベント、行う命令。
//!
//! visual としてホストする（"visual hosting"）ので、画面の上に自分のウィンドウを持たない。
//! それがすることも、ゲームの前にウィンドウを出してはならない（フルスクリーンのゲームはキーボードを
//! 失って最小化する）：右クリックメニュー、ダイアログ、新しいウィンドウ、ダウンロード、外部の
//! アプリはオフにするか断る。

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc;

use reminedog_core::browser::{MediaCommand, PageInput, PageModifiers, media_notice, media_script};
use reminedog_render::{BrowserState, Notice, PageLayout};
use webview2_com::Microsoft::Web::WebView2::Win32::{
    COREWEBVIEW2_BOUNDS_MODE_USE_RAW_PIXELS, COREWEBVIEW2_COLOR,
    COREWEBVIEW2_PERMISSION_STATE_DENY, COREWEBVIEW2_PROCESS_FAILED_KIND,
    COREWEBVIEW2_PROCESS_FAILED_KIND_BROWSER_PROCESS_EXITED,
    COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_EXITED,
    COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_UNRESPONSIVE, COREWEBVIEW2_SCRIPT_DIALOG_KIND,
    COREWEBVIEW2_SCRIPT_DIALOG_KIND_ALERT, COREWEBVIEW2_SCRIPT_DIALOG_KIND_BEFOREUNLOAD,
    COREWEBVIEW2_SCROLLBAR_STYLE_FLUENT_OVERLAY, ICoreWebView2, ICoreWebView2_4, ICoreWebView2_5,
    ICoreWebView2_8, ICoreWebView2_10, ICoreWebView2_18, ICoreWebView2_27,
    ICoreWebView2CompositionController, ICoreWebView2Controller, ICoreWebView2Controller2,
    ICoreWebView2Controller3, ICoreWebView2Environment, ICoreWebView2Environment3,
    ICoreWebView2EnvironmentOptions, ICoreWebView2Settings3, ICoreWebView2Settings4,
    ICoreWebView2Settings5, ICoreWebView2Settings6,
};
use webview2_com::{
    AddScriptToExecuteOnDocumentCreatedCompletedHandler, BasicAuthenticationRequestedEventHandler,
    CallDevToolsProtocolMethodCompletedHandler, ClientCertificateRequestedEventHandler,
    CoreWebView2EnvironmentOptions, CreateCoreWebView2CompositionControllerCompletedHandler,
    DocumentTitleChangedEventHandler, DownloadStartingEventHandler, ExecuteScriptCompletedHandler,
    HistoryChangedEventHandler, LaunchingExternalUriSchemeEventHandler,
    NavigationCompletedEventHandler, NewWindowRequestedEventHandler,
    PermissionRequestedEventHandler, ProcessFailedEventHandler, ScreenCaptureStartingEventHandler,
    ScriptDialogOpeningEventHandler, SourceChangedEventHandler, take_pwstr,
};
use windows::Win32::Foundation::{HWND, RECT};
use windows_core::{BOOL, HSTRING, Interface, PCWSTR, PWSTR};

use super::capture::Capture;
use super::{keep_last_page, notice, report};

/// Edge のスイッチ：動画は先にクリックしなくても再生する（ホットキーが始める）。ホストの
/// ウィンドウが画面に出なくても、ページは描き続ける。
const BROWSER_ARGUMENTS: &str =
    "--autoplay-policy=no-user-gesture-required --disable-features=CalculateNativeWinOcclusion";

/// どのページでも、その自身のスクリプトより先に動く。印刷とファイルの選択はダイアログを開き、
/// 自分にフォーカスを移すページはそのウィンドウを前に出すかもしれない：どれも何もしない（ページは
/// その `<input type=file>` に戻り、そのダイアログはキャンセルする）。ページが自分で作るフレーム
/// （スクリプトが動かない `about:blank`）も、ページが `contentWindow` で届く限り、同じものを受け取る。
const PAGE_SCRIPT: &str = "(() => {\
    const quiet = (w) => { try {\
        w.print = () => {}; w.focus = () => {};\
        delete w.showOpenFilePicker; delete w.showSaveFilePicker; delete w.showDirectoryPicker;\
    } catch (e) {} return w; };\
    quiet(window);\
    const frame = Object.getOwnPropertyDescriptor(HTMLIFrameElement.prototype, 'contentWindow');\
    if (frame && frame.get) {\
        Object.defineProperty(HTMLIFrameElement.prototype, 'contentWindow', { ...frame,\
            get() { const w = frame.get.call(this); return w ? quiet(w) : w; } });\
    }\
})();";

/// 出すアドレスとタイトルのいちばん長い文字数（ページはアドレスを数百 KB にできる）。長いものは切る。
const MAX_SHOWN: usize = 2048;

thread_local! {
    /// スレッドのブラウザがどう終わったか。スレッドのループのために、イベントのハンドラが入れる。
    static ENDING: RefCell<Option<BrowserState>> = const { RefCell::new(None) };
}

/// イベントが終わったと言っていれば、ブラウザがどう終わったか（1 回）。
pub(super) fn take_ending() -> Option<BrowserState> {
    ENDING.with(|ending| ending.borrow_mut().take())
}

/// メニューのための日本語の文字と、ログのための詳しいこと。
pub(super) struct StartError {
    pub(super) message: String,
    pub(super) detail: String,
}

impl StartError {
    fn new(message: &str, detail: impl std::fmt::Display) -> Self {
        Self {
            message: message.to_owned(),
            detail: detail.to_string(),
        }
    }
}

pub(super) struct Browser {
    generation: u64,
    controller: ICoreWebView2Controller,
    _composition: ICoreWebView2CompositionController,
    webview: ICoreWebView2,
    capture: Capture,
    layout: PageLayout,
    shown: bool,
}

impl Drop for Browser {
    fn drop(&mut self) {
        self.capture.stop();
        // SAFETY: コントローラーを持つブラウザのスレッドで。
        let _ = unsafe { self.controller.Close() };
    }
}

impl Browser {
    /// `window` の中に WebView を作り（あるまでこのスレッドのメッセージを動かす）、`url` を開く。
    pub(super) fn create(
        generation: u64,
        window: HWND,
        url: &str,
        layout: PageLayout,
        data_dir: &Path,
        last_page: PathBuf,
    ) -> Result<Self, StartError> {
        let capture = Capture::new(generation, window)
            .map_err(|e| StartError::new("画面の取り込みを準備できません", e))?;
        let environment = environment(data_dir)?;
        let composition = composition_controller(&environment, window)
            .map_err(|e| StartError::new("ブラウザを作れません", e))?;
        let mut browser = Self::setup(generation, composition, capture, layout, last_page)
            .map_err(|e| StartError::new("ブラウザを設定できません", e))?;
        browser.navigate(url);
        browser.shown = true;
        browser.apply_layout(true);
        Ok(browser)
    }

    fn setup(
        generation: u64,
        composition: ICoreWebView2CompositionController,
        capture: Capture,
        layout: PageLayout,
        last_page: PathBuf,
    ) -> windows_core::Result<Self> {
        let controller: ICoreWebView2Controller = composition.cast()?;
        // SAFETY: コントローラーを作ったスレッドでの COM の呼び出し。
        unsafe {
            let controller3: ICoreWebView2Controller3 = controller.cast()?;
            controller3.SetBoundsMode(COREWEBVIEW2_BOUNDS_MODE_USE_RAW_PIXELS)?;
            controller3.SetShouldDetectMonitorScaleChanges(false)?;
            if let Ok(controller2) = controller.cast::<ICoreWebView2Controller2>() {
                controller2.SetDefaultBackgroundColor(COREWEBVIEW2_COLOR {
                    A: 255,
                    R: 255,
                    G: 255,
                    B: 255,
                })?;
            }
            composition.SetRootVisualTarget(&capture.root)?;
        }
        // SAFETY: 上と同じ。
        let webview = unsafe { controller.CoreWebView2() }?;
        configure(&webview)?;
        subscribe(&webview, generation, last_page)?;
        let browser = Self {
            generation,
            controller,
            _composition: composition,
            webview,
            capture,
            layout,
            shown: false,
        };
        // ページはシステムのフォーカスなしに入力を受け取る。フォーカスがあると伝えるので、キャレットと
        // フォーカスの見た目を出す。
        browser.cdp("Emulation.setFocusEmulationEnabled", r#"{"enabled":true}"#);
        // ファイルの入力は、ゲームの前にシステムのファイルのダイアログを開いてしまう（このスレッドで、
        // スレッドを止めて）：代わりに、利用者が閉じたのと同じようにダイアログをキャンセルする。
        // 横取りには Page のドメインをオンにする必要がある。
        browser.cdp("Page.enable", "{}");
        browser.cdp(
            "Page.setInterceptFileChooserDialog",
            r#"{"enabled":true,"cancel":true}"#,
        );
        Ok(browser)
    }

    pub(super) fn show(&mut self) {
        if self.shown {
            return;
        }
        self.shown = true;
        self.apply_layout(true);
        self.set_muted(false);
        self.media(MediaCommand::Resume);
    }

    pub(super) fn hide(&mut self) {
        if !self.shown {
            return;
        }
        self.shown = false;
        self.media(MediaCommand::PauseAll);
        self.set_muted(true);
        self.capture.stop();
        // SAFETY: ブラウザのスレッドで。
        if let Err(e) = unsafe { self.controller.SetIsVisible(false) } {
            log::debug!("browser: hiding failed: {e}");
        }
    }

    /// 隠している間はページをミュートする：一時停止はページ自身のプレイヤーにしか届かず、そのフレームの
    /// 中のもの（ほかのサイトから埋め込んだ動画）は聞こえないまま再生を続ける。
    fn set_muted(&self, muted: bool) {
        // SAFETY: ブラウザのスレッドで。
        let result = self
            .webview
            .cast::<ICoreWebView2_8>()
            .and_then(|webview| unsafe { webview.SetIsMuted(muted) });
        if let Err(e) = result {
            log::debug!("browser: muting failed: {e}");
        }
    }

    #[cfg(test)]
    pub(super) fn is_muted(&self) -> bool {
        let mut muted = BOOL::default();
        // SAFETY: ブラウザのスレッドで。有効な出力の引数。
        let _ = self
            .webview
            .cast::<ICoreWebView2_8>()
            .and_then(|webview| unsafe { webview.IsMuted(&mut muted) });
        muted.as_bool()
    }

    pub(super) fn set_layout(&mut self, layout: PageLayout) {
        let resized = layout.size != self.layout.size;
        self.layout = layout;
        self.apply_layout(resized);
    }

    /// [`Self::layout`] のとおりに WebView の大きさを決める。新しい大きさ（か、見せること）は、その
    /// 大きさで取り込みをやり直す。
    fn apply_layout(&mut self, restart: bool) {
        let layout = self.layout;
        let [width, height] = layout.size.map(|v| v.max(1) as i32);
        // SAFETY: ブラウザのスレッドで。
        let result = unsafe {
            let controller3: windows_core::Result<ICoreWebView2Controller3> =
                self.controller.cast();
            controller3
                .and_then(|c| c.SetRasterizationScale(f64::from(layout.scale)))
                .and_then(|()| {
                    self.controller.SetBounds(RECT {
                        left: 0,
                        top: 0,
                        right: width,
                        bottom: height,
                    })
                })
                .and_then(|()| self.controller.SetZoomFactor(f64::from(layout.zoom)))
                .and_then(|()| self.controller.SetIsVisible(self.shown))
        };
        if let Err(e) = result {
            log::warn!("browser: sizing the page failed: {e}");
        }
        if self.shown
            && restart
            && let Err(e) = self.capture.start(layout.size)
        {
            log::warn!("browser: capturing the page failed: {e}");
        }
    }

    pub(super) fn on_frame(&mut self) {
        if self.shown {
            self.capture.on_frame();
        }
    }

    pub(super) fn navigate(&self, url: &str) {
        // SAFETY: ブラウザのスレッドで。
        if let Err(e) = unsafe { self.webview.Navigate(&HSTRING::from(url)) } {
            log::warn!("browser: navigating failed: {e}");
        }
    }

    pub(super) fn back(&self) {
        // SAFETY: ブラウザのスレッドで。
        let _ = unsafe { self.webview.GoBack() };
    }

    pub(super) fn forward(&self) {
        // SAFETY: ブラウザのスレッドで。
        let _ = unsafe { self.webview.GoForward() };
    }

    pub(super) fn reload(&self) {
        // SAFETY: ブラウザのスレッドで。
        let _ = unsafe { self.webview.Reload() };
    }

    /// DevTools Protocol のメソッドを呼ぶ。その結果は待たない。
    pub(super) fn cdp(&self, method: &'static str, params: &str) {
        let handler =
            CallDevToolsProtocolMethodCompletedHandler::create(Box::new(move |result, _json| {
                if let Err(e) = result {
                    log::debug!("browser: {method} failed: {e}");
                }
                Ok(())
            }));
        // SAFETY: ブラウザのスレッドで。文字列は呼び出しより長く生きる（WebView2 は写す）。
        if let Err(e) = unsafe {
            self.webview.CallDevToolsProtocolMethod(
                &HSTRING::from(method),
                &HSTRING::from(params),
                &handler,
            )
        } {
            log::debug!("browser: calling {method} failed: {e}");
        }
    }

    /// Page Down（か Up）と同じく、ページの中央の下にあるものを、その高さの `pages` 分だけ
    /// スクロールする。キーそのものは、ページを一度クリックするまで効かない：それまでは、どの
    /// フレームもフォーカスを持たない。ホイールには要らない。
    pub(super) fn scroll(&self, pages: f32) {
        let layout = self.layout;
        let css = |pixels: u32| pixels as f32 / (layout.scale * layout.zoom);
        let [width, height] = layout.size.map(css);
        let wheel = PageInput::Wheel {
            pos: [width / 2.0, height / 2.0],
            // Chromium のページの送り：見え方の 8 分の 7。
            delta: [0.0, pages * height * 0.875],
            modifiers: PageModifiers::default(),
        };
        let (method, params) = wheel.cdp();
        self.cdp(method, &params);
    }

    /// ページの動画を操作する。結果は通知になる。
    pub(super) fn media(&self, command: MediaCommand) {
        let generation = self.generation;
        self.eval(&media_script(command), move |json| {
            if let Some(text) = media_notice(command, json) {
                report(generation, |shared| {
                    shared
                        .notices
                        .push(notice(text, false).replacing(Notice::MEDIA))
                });
            }
        });
    }

    /// ページで `script` を動かし、その値（JSON）を `then` に渡す。
    pub(super) fn eval(&self, script: &str, then: impl FnOnce(&str) + 'static) {
        let handler = ExecuteScriptCompletedHandler::create(Box::new(move |result, json| {
            match result {
                Ok(()) => then(&json),
                Err(e) => log::debug!("browser: a script failed: {e}"),
            }
            Ok(())
        }));
        // SAFETY: ブラウザのスレッドで。
        if let Err(e) = unsafe { self.webview.ExecuteScript(&HSTRING::from(script), &handler) } {
            log::debug!("browser: running a script failed: {e}");
        }
    }
}

fn environment(data_dir: &Path) -> Result<ICoreWebView2Environment, StartError> {
    let options = CoreWebView2EnvironmentOptions::default();
    // SAFETY: オプションの物のただの setter。
    unsafe {
        options.set_additional_browser_arguments(BROWSER_ARGUMENTS.to_owned());
        options.set_language("ja-JP".to_owned());
        options.set_scroll_bar_style(COREWEBVIEW2_SCROLLBAR_STYLE_FLUENT_OVERLAY);
    }
    let options: ICoreWebView2EnvironmentOptions = options.into();
    let (tx, rx) = mpsc::channel();
    let data_dir = HSTRING::from(data_dir.as_os_str());
    webview2_com::CreateCoreWebView2EnvironmentCompletedHandler::wait_for_async_operation(
        Box::new(move |handler| {
            create_environment(&data_dir, &options, &handler).map_err(webview2_com::Error::from)
        }),
        Box::new(move |result, environment| {
            result?;
            let _ = tx.send(environment);
            Ok(())
        }),
    )
    .map_err(|e| StartError::new(&no_runtime_message(&e), format!("{e:?}")))?;
    rx.try_recv()
        .ok()
        .flatten()
        .ok_or_else(|| StartError::new("ブラウザを起動できません", "no environment"))
}

/// 環境を作れなかったときのメニューの文字。
fn no_runtime_message(error: &webview2_com::Error) -> String {
    // HRESULT_FROM_WIN32(ERROR_FILE_NOT_FOUND)：WebView2 ランタイムが入っていない。
    const NOT_FOUND: i32 = 0x8007_0002_u32 as i32;
    // HRESULT_FROM_WIN32(ERROR_INVALID_STATE)：ユーザーデータのフォルダを、別のオプションで使って
    // いる（別の reminedog の版の、別のゲームのブラウザ）。
    const INVALID_STATE: i32 = 0x8007_139F_u32 as i32;
    match error {
        webview2_com::Error::WindowsError(e) if e.code().0 == NOT_FOUND => {
            "WebView2 ランタイムが見つかりません（Microsoft Edge WebView2 Runtime を入れてください）"
                .to_owned()
        }
        webview2_com::Error::WindowsError(e) if e.code().0 == INVALID_STATE => {
            "別のゲームが違う版の reminedog でブラウザを使っているので起動できません（そちらのブラウザを終了するか、版をそろえてください）"
                .to_owned()
        }
        _ => "ブラウザを起動できません".to_owned(),
    }
}

/// WebView2 のローダーは MSVC のビルドに静的にリンクする（GNU のビルドは WebView2Loader.dll から
/// import することになるが、それにはブラウザがない）。
fn create_environment(
    data_dir: &HSTRING,
    options: &ICoreWebView2EnvironmentOptions,
    handler: &webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2CreateCoreWebView2EnvironmentCompletedHandler,
) -> windows_core::Result<()> {
    // SAFETY: 有効な文字列とインターフェイス。ハンドラはこのスレッドで呼ばれる。
    unsafe {
        webview2_com::Microsoft::Web::WebView2::Win32::CreateCoreWebView2EnvironmentWithOptions(
            PCWSTR::null(),
            data_dir,
            options,
            handler,
        )
    }
}

fn composition_controller(
    environment: &ICoreWebView2Environment,
    window: HWND,
) -> webview2_com::Result<ICoreWebView2CompositionController> {
    let environment: ICoreWebView2Environment3 = environment.cast()?;
    let (tx, rx) = mpsc::channel();
    CreateCoreWebView2CompositionControllerCompletedHandler::wait_for_async_operation(
        Box::new(move |handler| {
            // SAFETY: このスレッドのウィンドウ。ハンドラはこのスレッドで呼ばれる。
            unsafe { environment.CreateCoreWebView2CompositionController(window, &handler) }
                .map_err(webview2_com::Error::from)
        }),
        Box::new(move |result, controller| {
            result?;
            let _ = tx.send(controller);
            Ok(())
        }),
    )?;
    rx.try_recv()
        .ok()
        .flatten()
        .ok_or(webview2_com::Error::CallbackError("no controller".into()))
}

/// ウィンドウを開くか、ゲームからキーを取るものは、何でもオフにする。
fn configure(webview: &ICoreWebView2) -> windows_core::Result<()> {
    // SAFETY: ブラウザのスレッドでの COM の呼び出し。新しいインターフェイスはなくてもよい：古い
    // ランタイムはそこで既定のまま。
    unsafe {
        let settings = webview.Settings()?;
        settings.SetAreDefaultContextMenusEnabled(false)?;
        settings.SetAreDevToolsEnabled(false)?;
        settings.SetIsStatusBarEnabled(false)?;
        settings.SetAreDefaultScriptDialogsEnabled(false)?;
        settings.SetIsZoomControlEnabled(false)?;
        settings.SetAreHostObjectsAllowed(false)?;
        settings.SetIsWebMessageEnabled(false)?;
        if let Ok(settings) = settings.cast::<ICoreWebView2Settings3>() {
            let _ = settings.SetAreBrowserAcceleratorKeysEnabled(false);
        }
        if let Ok(settings) = settings.cast::<ICoreWebView2Settings4>() {
            let _ = settings.SetIsPasswordAutosaveEnabled(false);
            let _ = settings.SetIsGeneralAutofillEnabled(false);
        }
        if let Ok(settings) = settings.cast::<ICoreWebView2Settings5>() {
            let _ = settings.SetIsPinchZoomEnabled(false);
        }
        if let Ok(settings) = settings.cast::<ICoreWebView2Settings6>() {
            let _ = settings.SetIsSwipeNavigationEnabled(false);
        }
        let done =
            AddScriptToExecuteOnDocumentCreatedCompletedHandler::create(Box::new(|result, _id| {
                if let Err(e) = result {
                    log::debug!("browser: adding the page script failed: {e}");
                }
                Ok(())
            }));
        webview.AddScriptToExecuteOnDocumentCreated(&HSTRING::from(PAGE_SCRIPT), &done)?;
    }
    Ok(())
}

/// イベントでページの状態を報告し、最後のページを `last_page` に残し、ウィンドウを開くものは断る。
fn subscribe(
    webview: &ICoreWebView2,
    generation: u64,
    last_page: PathBuf,
) -> windows_core::Result<()> {
    let mut token = 0i64;
    let kept = Rc::new(RefCell::new(String::new()));
    // SAFETY: ブラウザのスレッドでの COM の呼び出し。どのハンドラもそこで動く。
    unsafe {
        webview.add_SourceChanged(
            &SourceChangedEventHandler::create(Box::new(move |sender, _| {
                refresh(sender, generation);
                Ok(())
            })),
            &mut token,
        )?;
        webview.add_HistoryChanged(
            &HistoryChangedEventHandler::create(Box::new(move |sender, _| {
                refresh(sender, generation);
                Ok(())
            })),
            &mut token,
        )?;
        webview.add_DocumentTitleChanged(
            &DocumentTitleChangedEventHandler::create(Box::new(move |sender, _| {
                refresh(sender, generation);
                Ok(())
            })),
            &mut token,
        )?;
        // ページを読み込んだ：次の起動でそれを開く（読み込んだ文書だけ。自分のアドレスを変えるページ
        // （ずっとそうするものもある）は除く）。
        webview.add_NavigationCompleted(
            &NavigationCompletedEventHandler::create(Box::new(move |sender, args| {
                let mut success = BOOL::default();
                if let Some(args) = &args {
                    args.IsSuccess(&mut success)?;
                }
                if let Some(webview) = &sender
                    && success.as_bool()
                {
                    let mut url = PWSTR::null();
                    let _ = webview.Source(&mut url);
                    let url = take_pwstr(url);
                    if *kept.borrow() != url {
                        keep_last_page(&last_page, &url);
                        *kept.borrow_mut() = url;
                    }
                }
                refresh(sender, generation);
                Ok(())
            })),
            &mut token,
        )?;
        // 新しいウィンドウへのリンクは代わりにここで開く。ページが自分で開くもの（タイマーの広告）は、
        // ポップアップのブロックと同じく何も得ない。
        webview.add_NewWindowRequested(
            &NewWindowRequestedEventHandler::create(Box::new(move |sender, args| {
                if let (Some(webview), Some(args)) = (sender, args) {
                    let mut user = BOOL::default();
                    args.IsUserInitiated(&mut user)?;
                    args.SetHandled(true)?;
                    if !user.as_bool() {
                        log::debug!("browser: a window the user did not ask for was refused");
                        return Ok(());
                    }
                    let mut uri = PWSTR::null();
                    args.Uri(&mut uri)?;
                    let uri = take_pwstr(uri);
                    webview.Navigate(&HSTRING::from(uri))?;
                }
                Ok(())
            })),
            &mut token,
        )?;
        // ダイアログ：alert か「ページを離れますか？」は受け入れ、それ以外はキャンセルする。
        webview.add_ScriptDialogOpening(
            &ScriptDialogOpeningEventHandler::create(Box::new(|_, args| {
                if let Some(args) = args {
                    let mut kind = COREWEBVIEW2_SCRIPT_DIALOG_KIND::default();
                    args.Kind(&mut kind)?;
                    if kind == COREWEBVIEW2_SCRIPT_DIALOG_KIND_ALERT
                        || kind == COREWEBVIEW2_SCRIPT_DIALOG_KIND_BEFOREUNLOAD
                    {
                        args.Accept()?;
                    }
                }
                Ok(())
            })),
            &mut token,
        )?;
        webview.add_PermissionRequested(
            &PermissionRequestedEventHandler::create(Box::new(|_, args| {
                if let Some(args) = args {
                    args.SetState(COREWEBVIEW2_PERMISSION_STATE_DENY)?;
                }
                Ok(())
            })),
            &mut token,
        )?;
        webview.add_ProcessFailed(
            &ProcessFailedEventHandler::create(Box::new(move |_, args| {
                let mut kind = COREWEBVIEW2_PROCESS_FAILED_KIND::default();
                if let Some(args) = args {
                    args.ProcessFailedKind(&mut kind)?;
                }
                log::warn!("browser: a browser process failed ({})", kind.0);
                if kind == COREWEBVIEW2_PROCESS_FAILED_KIND_BROWSER_PROCESS_EXITED {
                    // WebView が死んだ：スレッドは終わり、次に見せるときに初めからやり直す。
                    ENDING.with(|ending| {
                        *ending.borrow_mut() = Some(BrowserState::Failed(
                            "ブラウザのプロセスが終了しました（表示し直すと起動し直します）".into(),
                        ));
                    });
                } else if kind == COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_EXITED
                    || kind == COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_UNRESPONSIVE
                {
                    report(generation, |shared| {
                        shared.notices.push(notice(
                            "ページが応答しなくなりました。再読み込みしてください",
                            true,
                        ));
                    });
                }
                // ほかのプロセス（フレームのもの、GPU のもの）は WebView2 が作り直す。
                Ok(())
            })),
            &mut token,
        )?;
        if let Ok(webview) = webview.cast::<ICoreWebView2_4>() {
            webview.add_DownloadStarting(
                &DownloadStartingEventHandler::create(Box::new(move |_, args| {
                    if let Some(args) = args {
                        args.SetCancel(true)?;
                        report(generation, |shared| {
                            shared
                                .notices
                                .push(notice("ブラウザではダウンロードできません", true));
                        });
                    }
                    Ok(())
                })),
                &mut token,
            )?;
        }
        if let Ok(webview) = webview.cast::<ICoreWebView2_10>() {
            webview.add_BasicAuthenticationRequested(
                &BasicAuthenticationRequestedEventHandler::create(Box::new(|_, args| {
                    if let Some(args) = args {
                        args.SetCancel(true)?;
                    }
                    Ok(())
                })),
                &mut token,
            )?;
        }
        // クライアント証明書を求めるサイトには、システムの選択が出てしまう：証明書なしで続ける。
        if let Ok(webview) = webview.cast::<ICoreWebView2_5>() {
            webview.add_ClientCertificateRequested(
                &ClientCertificateRequestedEventHandler::create(Box::new(|_, args| {
                    if let Some(args) = args {
                        args.SetHandled(true)?;
                    }
                    Ok(())
                })),
                &mut token,
            )?;
        }
        // 画面の共有はシステムの選択を開いてしまう。
        if let Ok(webview) = webview.cast::<ICoreWebView2_27>() {
            webview.add_ScreenCaptureStarting(
                &ScreenCaptureStartingEventHandler::create(Box::new(|_, args| {
                    if let Some(args) = args {
                        args.SetCancel(true)?;
                    }
                    Ok(())
                })),
                &mut token,
            )?;
        }
        // mailto: などのリンクは、ゲームの前に別のアプリを開いてしまう。
        if let Ok(webview) = webview.cast::<ICoreWebView2_18>() {
            webview.add_LaunchingExternalUriScheme(
                &LaunchingExternalUriSchemeEventHandler::create(Box::new(|_, args| {
                    if let Some(args) = args {
                        args.SetCancel(true)?;
                    }
                    Ok(())
                })),
                &mut token,
            )?;
        }
    }
    Ok(())
}

/// ページのアドレス、タイトル、履歴を共有の状態に読む。
fn refresh(webview: Option<ICoreWebView2>, generation: u64) {
    let Some(webview) = webview else {
        return;
    };
    // SAFETY: ブラウザのスレッドでの COM の getter。文字列は `take_pwstr` が解放する。
    let (url, title, back, forward) = unsafe {
        let mut url = PWSTR::null();
        let mut title = PWSTR::null();
        let mut back = BOOL::default();
        let mut forward = BOOL::default();
        let _ = webview.Source(&mut url);
        let _ = webview.DocumentTitle(&mut title);
        let _ = webview.CanGoBack(&mut back);
        let _ = webview.CanGoForward(&mut forward);
        (
            take_pwstr(url),
            take_pwstr(title),
            back.as_bool(),
            forward.as_bool(),
        )
    };
    let (url, title) = (cut(url), cut(title));
    report(generation, |shared| {
        let view = &mut shared.view;
        if view.url != url {
            // アドレスはトークンを含みうる：ログにはホストだけを出す。
            log::debug!("browser: now on {}", host_of(&url));
        }
        view.url = url;
        view.title = title;
        view.can_go_back = back;
        view.can_go_forward = forward;
    });
}

/// [`MAX_SHOWN`] 文字に切った `text`。切ったら "…" を付ける。
fn cut(mut text: String) -> String {
    if let Some((end, _)) = text.char_indices().nth(MAX_SHOWN) {
        text.truncate(end);
        text.push('…');
    }
    text
}

/// `https://example.com/a?b` → `example.com`。ほかのアドレス（`data`、`about`）はスキームだけ
/// （その残りはホストではない）。
fn host_of(url: &str) -> &str {
    let Some((scheme, rest)) = url.split_once(':') else {
        return "";
    };
    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return scheme;
    }
    let rest = rest.trim_start_matches('/');
    let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
    host.rsplit_once('@').map_or(host, |(_, host)| host)
}

#[cfg(test)]
mod host_tests {
    use super::*;

    #[test]
    fn hosts_for_the_log() {
        assert_eq!(host_of("https://example.com/a?b#c"), "example.com");
        assert_eq!(
            host_of("http://user:pw@example.com:8123/"),
            "example.com:8123"
        );
        assert_eq!(host_of("data:text/html,<a href='https://x/'>"), "data");
        assert_eq!(host_of("about:blank"), "about");
        assert_eq!(host_of("nothing"), "");
    }

    #[test]
    fn long_text_is_cut() {
        assert_eq!(cut("short".into()), "short");
        let long = "あ".repeat(MAX_SHOWN + 5);
        let cut = cut(long);
        assert_eq!(cut.chars().count(), MAX_SHOWN + 1);
        assert!(cut.ends_with('…'));
    }
}
