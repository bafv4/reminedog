//! The WebView2 itself: creation, its settings and events, and the commands it carries out.
//!
//! It is hosted as a visual ("visual hosting"), so it has no window of its own on the
//! screen. Nothing it does may put a window in front of the game either (a fullscreen game
//! loses the keyboard and minimizes): context menus, dialogs, new windows, downloads and
//! external apps are turned off or refused.

use std::path::Path;
use std::sync::mpsc;

use reminedog_core::browser::{MediaCommand, PageInput, PageModifiers, media_notice, media_script};
use webview2_com::Microsoft::Web::WebView2::Win32::{
    COREWEBVIEW2_BOUNDS_MODE_USE_RAW_PIXELS, COREWEBVIEW2_COLOR,
    COREWEBVIEW2_PERMISSION_STATE_DENY, COREWEBVIEW2_PROCESS_FAILED_KIND,
    COREWEBVIEW2_PROCESS_FAILED_KIND_BROWSER_PROCESS_EXITED, COREWEBVIEW2_SCRIPT_DIALOG_KIND,
    COREWEBVIEW2_SCRIPT_DIALOG_KIND_ALERT, COREWEBVIEW2_SCRIPT_DIALOG_KIND_BEFOREUNLOAD,
    COREWEBVIEW2_SCROLLBAR_STYLE_FLUENT_OVERLAY, ICoreWebView2, ICoreWebView2_4, ICoreWebView2_10,
    ICoreWebView2_18, ICoreWebView2CompositionController, ICoreWebView2Controller,
    ICoreWebView2Controller2, ICoreWebView2Controller3, ICoreWebView2Environment,
    ICoreWebView2Environment3, ICoreWebView2EnvironmentOptions, ICoreWebView2Settings3,
    ICoreWebView2Settings4, ICoreWebView2Settings5, ICoreWebView2Settings6,
};
use webview2_com::{
    AddScriptToExecuteOnDocumentCreatedCompletedHandler, BasicAuthenticationRequestedEventHandler,
    CallDevToolsProtocolMethodCompletedHandler, CoreWebView2EnvironmentOptions,
    CreateCoreWebView2CompositionControllerCompletedHandler, DocumentTitleChangedEventHandler,
    DownloadStartingEventHandler, ExecuteScriptCompletedHandler, HistoryChangedEventHandler,
    LaunchingExternalUriSchemeEventHandler, NavigationCompletedEventHandler,
    NavigationStartingEventHandler, NewWindowRequestedEventHandler,
    PermissionRequestedEventHandler, ProcessFailedEventHandler, ScriptDialogOpeningEventHandler,
    SourceChangedEventHandler, take_pwstr,
};
use windows::Win32::Foundation::{HWND, RECT};
use windows_core::{BOOL, HSTRING, Interface, PCWSTR, PWSTR};

use super::capture::Capture;
use super::{Layout, Status, report};

/// Edge's switches: video plays without a click first (the hotkeys start it), and the page
/// keeps drawing though its host window is never on the screen.
const BROWSER_ARGUMENTS: &str =
    "--autoplay-policy=no-user-gesture-required --disable-features=CalculateNativeWinOcclusion";

/// Run in every page before its own scripts: printing would open a dialog.
const PAGE_SCRIPT: &str = "window.print = () => {};";

/// Japanese text for the menu, and the details for the log.
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
    layout: Layout,
    shown: bool,
}

impl Drop for Browser {
    fn drop(&mut self) {
        self.capture.stop();
        // SAFETY: on the browser thread, which owns the controller.
        let _ = unsafe { self.controller.Close() };
    }
}

impl Browser {
    /// Creates the WebView in `window` (pumping this thread's messages until it exists) and
    /// opens `url`.
    pub(super) fn create(
        generation: u64,
        window: HWND,
        url: &str,
        layout: Layout,
        data_dir: &Path,
    ) -> Result<Self, StartError> {
        let capture = Capture::new(generation, window)
            .map_err(|e| StartError::new("画面の取り込みを準備できません", e))?;
        let environment = environment(data_dir)?;
        let composition = composition_controller(&environment, window)
            .map_err(|e| StartError::new("ブラウザを作れません", e))?;
        let mut browser = Self::setup(generation, composition, capture, layout)
            .map_err(|e| StartError::new("ブラウザを設定できません", e))?;
        browser.navigate(url);
        browser.shown = true;
        browser.apply_layout(layout, true);
        Ok(browser)
    }

    fn setup(
        generation: u64,
        composition: ICoreWebView2CompositionController,
        capture: Capture,
        layout: Layout,
    ) -> windows_core::Result<Self> {
        let controller: ICoreWebView2Controller = composition.cast()?;
        // SAFETY: COM calls on the thread that created the controller.
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
        // SAFETY: as above.
        let webview = unsafe { controller.CoreWebView2() }?;
        configure(&webview)?;
        subscribe(&webview, generation)?;
        let browser = Self {
            generation,
            controller,
            _composition: composition,
            webview,
            capture,
            layout,
            shown: false,
        };
        // The page gets its input without the system's focus; it is told it has it, so it
        // shows the caret and its focus styles.
        browser.cdp("Emulation.setFocusEmulationEnabled", r#"{"enabled":true}"#);
        Ok(browser)
    }

    pub(super) fn show(&mut self) {
        if self.shown {
            return;
        }
        self.shown = true;
        self.apply_layout(self.layout, true);
        self.media(MediaCommand::Resume);
    }

    pub(super) fn hide(&mut self) {
        if !self.shown {
            return;
        }
        self.shown = false;
        self.media(MediaCommand::PauseAll);
        self.capture.stop();
        // SAFETY: on the browser thread.
        if let Err(e) = unsafe { self.controller.SetIsVisible(false) } {
            log::debug!("browser: hiding failed: {e}");
        }
    }

    pub(super) fn set_layout(&mut self, layout: Layout) {
        let resized = layout.size != self.layout.size;
        self.layout = layout;
        self.apply_layout(layout, resized);
    }

    /// Sizes the WebView; a new size (or showing it) restarts the capture at that size.
    fn apply_layout(&mut self, layout: Layout, restart: bool) {
        let [width, height] = layout.size.map(|v| v.max(1) as i32);
        // SAFETY: on the browser thread.
        let result = unsafe {
            let controller3: windows_core::Result<ICoreWebView2Controller3> =
                self.controller.cast();
            controller3
                .and_then(|c| c.SetRasterizationScale(layout.scale))
                .and_then(|()| {
                    self.controller.SetBounds(RECT {
                        left: 0,
                        top: 0,
                        right: width,
                        bottom: height,
                    })
                })
                .and_then(|()| self.controller.SetZoomFactor(layout.zoom))
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
        // SAFETY: on the browser thread.
        if let Err(e) = unsafe { self.webview.Navigate(&HSTRING::from(url)) } {
            log::warn!("browser: navigating failed: {e}");
        }
    }

    pub(super) fn back(&self) {
        // SAFETY: on the browser thread.
        let _ = unsafe { self.webview.GoBack() };
    }

    pub(super) fn forward(&self) {
        // SAFETY: on the browser thread.
        let _ = unsafe { self.webview.GoForward() };
    }

    pub(super) fn reload(&self) {
        // SAFETY: on the browser thread.
        let _ = unsafe { self.webview.Reload() };
    }

    /// Calls a DevTools Protocol method, not waiting for its result.
    pub(super) fn cdp(&self, method: &str, params: &str) {
        let what = method.to_owned();
        let handler =
            CallDevToolsProtocolMethodCompletedHandler::create(Box::new(move |result, _json| {
                if let Err(e) = result {
                    log::debug!("browser: {what} failed: {e}");
                }
                Ok(())
            }));
        let (method, params) = (HSTRING::from(method), HSTRING::from(params));
        // SAFETY: on the browser thread; the strings outlive the call (WebView2 copies them).
        if let Err(e) = unsafe {
            self.webview
                .CallDevToolsProtocolMethod(&method, &params, &handler)
        } {
            log::debug!("browser: calling {method} failed: {e}");
        }
    }

    /// Scrolls what is under the middle of the page by `pages` of its height, as Page Down
    /// (or Up) would. The keys themselves only work once the page was clicked: until then
    /// none of its frames has the focus. The wheel needs none.
    pub(super) fn scroll(&self, pages: f32) {
        let layout = self.layout;
        let css = |pixels: u32| (f64::from(pixels) / (layout.scale * layout.zoom)) as f32;
        let [width, height] = layout.size.map(css);
        let wheel = PageInput::Wheel {
            pos: [width / 2.0, height / 2.0],
            // Chromium's page step: seven eighths of the view.
            delta: [0.0, pages * height * 0.875],
            modifiers: PageModifiers::default(),
        };
        let (method, params) = wheel.cdp();
        self.cdp(method, &params);
    }

    /// Controls the page's video; the outcome becomes a notice.
    pub(super) fn media(&self, command: MediaCommand) {
        let generation = self.generation;
        self.eval(&media_script(command), move |json| {
            if let Some(notice) = media_notice(command, json) {
                report(generation, |shared| shared.notices.push((notice, false)));
            }
        });
    }

    /// Runs `script` in the page and hands its value (as JSON) to `then`.
    pub(super) fn eval(&self, script: &str, then: impl FnOnce(&str) + 'static) {
        let handler = ExecuteScriptCompletedHandler::create(Box::new(move |result, json| {
            match result {
                Ok(()) => then(&json),
                Err(e) => log::debug!("browser: a script failed: {e}"),
            }
            Ok(())
        }));
        // SAFETY: on the browser thread.
        if let Err(e) = unsafe { self.webview.ExecuteScript(&HSTRING::from(script), &handler) } {
            log::debug!("browser: running a script failed: {e}");
        }
    }
}

fn environment(data_dir: &Path) -> Result<ICoreWebView2Environment, StartError> {
    let options = CoreWebView2EnvironmentOptions::default();
    // SAFETY: plain setters on the options object.
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

/// The menu's text when the environment could not be made.
fn no_runtime_message(error: &webview2_com::Error) -> String {
    // HRESULT_FROM_WIN32(ERROR_FILE_NOT_FOUND): no WebView2 Runtime installed.
    const NOT_FOUND: i32 = 0x8007_0002_u32 as i32;
    match error {
        webview2_com::Error::WindowsError(e) if e.code().0 == NOT_FOUND => {
            "WebView2 ランタイムが見つかりません（Microsoft Edge WebView2 Runtime を入れてください）"
                .to_owned()
        }
        _ => "ブラウザを起動できません".to_owned(),
    }
}

/// The WebView2 loader is linked statically into the MSVC build (the GNU build would import
/// it from WebView2Loader.dll; it has no browser).
fn create_environment(
    data_dir: &HSTRING,
    options: &ICoreWebView2EnvironmentOptions,
    handler: &webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2CreateCoreWebView2EnvironmentCompletedHandler,
) -> windows_core::Result<()> {
    // SAFETY: valid strings and interfaces; the handler is called on this thread.
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
            // SAFETY: a window of this thread; the handler is called on this thread.
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

/// Turns off whatever would open a window or take keys from the game.
fn configure(webview: &ICoreWebView2) -> windows_core::Result<()> {
    // SAFETY: COM calls on the browser thread. The newer interfaces are optional: an old
    // runtime keeps its defaults there.
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

/// Reports the page's state on its events, and refuses what would open a window.
fn subscribe(webview: &ICoreWebView2, generation: u64) -> windows_core::Result<()> {
    let mut token = 0i64;
    // SAFETY: COM calls on the browser thread; every handler runs on it too.
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
        webview.add_NavigationStarting(
            &NavigationStartingEventHandler::create(Box::new(move |_, _| {
                report(generation, |shared| shared.loading = true);
                Ok(())
            })),
            &mut token,
        )?;
        webview.add_NavigationCompleted(
            &NavigationCompletedEventHandler::create(Box::new(move |sender, _| {
                report(generation, |shared| shared.loading = false);
                refresh(sender, generation);
                Ok(())
            })),
            &mut token,
        )?;
        // A link to a new window opens here instead.
        webview.add_NewWindowRequested(
            &NewWindowRequestedEventHandler::create(Box::new(move |sender, args| {
                if let (Some(webview), Some(args)) = (sender, args) {
                    let mut uri = PWSTR::null();
                    args.Uri(&mut uri)?;
                    let uri = take_pwstr(uri);
                    args.SetHandled(true)?;
                    webview.Navigate(&HSTRING::from(uri))?;
                }
                Ok(())
            })),
            &mut token,
        )?;
        // Dialogs: an alert or "leave the page?" is accepted, anything else cancelled.
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
                    report(generation, |shared| {
                        shared.status = Status::Failed(
                            "ブラウザのプロセスが終了しました（終了してから表示し直してください）"
                                .into(),
                        );
                    });
                } else {
                    report(generation, |shared| {
                        shared.notices.push((
                            "ページが応答しなくなりました。再読み込みしてください".into(),
                            true,
                        ));
                    });
                }
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
                                .push(("ブラウザではダウンロードできません".into(), true));
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
        // Links like mailto: would open another app in front of the game.
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

/// Reads the page's address, title and history into the shared state.
fn refresh(webview: Option<ICoreWebView2>, generation: u64) {
    let Some(webview) = webview else {
        return;
    };
    // SAFETY: COM getters on the browser thread; the strings are freed by `take_pwstr`.
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
    report(generation, |shared| {
        if shared.url != url {
            // The address can hold tokens: the log gets the host only.
            log::debug!("browser: now on {}", host_of(&url));
        }
        shared.url = url;
        shared.title = title;
        shared.can_go_back = back;
        shared.can_go_forward = forward;
    });
}

/// `https://example.com/a?b` → `example.com`.
fn host_of(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
    host.rsplit_once('@').map_or(host, |(_, host)| host)
}
