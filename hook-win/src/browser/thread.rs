//! ブラウザのスレッド：DispatcherQueue のある COM のアパートメント（コンポジターと取り込みの
//! ため）、見せることのないウィンドウ（WebView のホスト）、メールボックスの命令を行うメッセージの
//! ループ。

use std::io;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use reminedog_render::BrowserState;
use windows::System::DispatcherQueueController;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::WinRT::{
    CreateDispatcherQueueController, DQTAT_COM_STA, DQTYPE_THREAD_CURRENT, DispatcherQueueOptions,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW, KillTimer, MSG,
    PostThreadMessageW, RegisterClassExW, SetTimer, TranslateMessage, WINDOW_EX_STYLE, WM_QUIT,
    WM_TIMER, WNDCLASSEXW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_POPUP,
};
use windows_core::w;
use windows_sys::Win32::System::Threading::GetCurrentThreadId;

use super::webview::{self, Browser};
use super::{
    Command, Mailbox, Start, WM_APP_COMMAND, WM_APP_FRAME, last_page, report, thread_ended,
};
use crate::ffi;

/// 入れ子のメッセージのループ（WebView のダイアログが動かすもの）が起こすメッセージを持って
/// いってしまった後に、ループをメールボックスに戻すタイマー。
const WAKE_TIMER: usize = 1;
/// それが動く間隔（ミリ秒）。
const WAKE_INTERVAL: u32 = 100;

pub(super) fn spawn(generation: u64, mailbox: Arc<Mailbox>, start: Start) -> io::Result<()> {
    std::thread::Builder::new()
        .name("reminedog-browser".into())
        .spawn(move || {
            let status = ffi::catch("the browser thread", || run(generation, &mailbox, start))
                .unwrap_or_else(|| BrowserState::Failed("ブラウザが異常終了しました".into()));
            mailbox.window.store(0, Ordering::Release);
            thread_ended(generation, status);
            log::info!("browser: stopped");
        })
        .map(drop)
}

/// スレッドの一生。終わるときの状態。
fn run(generation: u64, mailbox: &Mailbox, start: Start) -> BrowserState {
    // SAFETY: このスレッドの COM を初期化する。下で戻す。
    if let Err(e) = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.ok() {
        log::error!("browser: CoInitializeEx failed: {e}");
        return BrowserState::Failed("ブラウザを起動できません".into());
    }
    let status = run_in_apartment(generation, mailbox, start);
    // SAFETY: CoInitializeEx と対になる。COM のものは全部 `run_in_apartment` で捨てた。
    unsafe { CoUninitialize() };
    status
}

fn run_in_apartment(generation: u64, mailbox: &Mailbox, start: Start) -> BrowserState {
    let options = DispatcherQueueOptions {
        dwSize: size_of::<DispatcherQueueOptions>() as u32,
        threadType: DQTYPE_THREAD_CURRENT,
        apartmentType: DQTAT_COM_STA,
    };
    // SAFETY: ただの呼び出し。キューは下で止める。
    let queue = match unsafe { CreateDispatcherQueueController(options) } {
        Ok(queue) => queue,
        Err(e) => {
            log::error!("browser: CreateDispatcherQueueController failed: {e}");
            return BrowserState::Failed("ブラウザを起動できません".into());
        }
    };
    let status = match host_window() {
        Ok(window) => {
            let status = run_with_window(generation, mailbox, start, window);
            // SAFETY: 自分のウィンドウを、そのスレッドで。
            let _ = unsafe { DestroyWindow(window) };
            status
        }
        Err(e) => {
            log::error!("browser: cannot create its window: {e}");
            BrowserState::Failed("ブラウザを起動できません".into())
        }
    };
    shut_down(&queue);
    status
}

/// スレッドの DispatcherQueue を止め、終わるまでこのスレッドのメッセージを動かす：作った
/// スレッドが、終わる前にそうしなければならない。
fn shut_down(queue: &DispatcherQueueController) {
    // SAFETY: ただの呼び出し。
    let thread = unsafe { GetCurrentThreadId() };
    let quit_when_done = queue.ShutdownQueueAsync().and_then(|shutdown| {
        shutdown.when(move |_| {
            // SAFETY: ただの呼び出し。下のループを終える。
            let _ = unsafe { PostThreadMessageW(thread, WM_QUIT, WPARAM(0), LPARAM(0)) };
        })
    });
    if let Err(e) = quit_when_done {
        log::warn!("browser: shutting down its DispatcherQueue failed: {e}");
        return;
    }
    let mut msg = MSG::default();
    // SAFETY: 有効な出力の引数。メッセージは来たとおりに配る。
    while unsafe { GetMessageW(&mut msg, None, 0, 0) }.0 > 0 {
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

fn run_with_window(generation: u64, mailbox: &Mailbox, start: Start, window: HWND) -> BrowserState {
    log::info!("browser: starting");
    let url = last_page(&start.last_page).unwrap_or(start.url);
    let mut browser = match Browser::create(
        generation,
        window,
        &url,
        start.layout,
        &start.data_dir,
        start.last_page,
    ) {
        Ok(browser) => browser,
        Err(e) => {
            log::error!("browser: {}: {}", e.message, e.detail);
            return BrowserState::Failed(e.message);
        }
    };
    report(generation, |shared| shared.view.state = BrowserState::Ready);
    log::info!("browser: ready");
    // ここからはゲームのスレッドがループを起こす。WebView を作っている間に並んだ命令（その起こしは
    // 処理されなかった）が先に動く。
    mailbox.window.store(window.0 as usize, Ordering::Release);
    if !run_commands(&mut browser, mailbox) {
        return BrowserState::Off;
    }
    let mut msg = MSG::default();
    loop {
        // SAFETY: 有効な出力の引数。
        let got = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if got.0 <= 0 {
            // WM_QUIT（誰も送らない）かエラー。
            return BrowserState::Off;
        }
        let wake_timer = msg.message == WM_TIMER && msg.wParam.0 == WAKE_TIMER;
        if msg.hwnd == window && (msg.message == WM_APP_COMMAND || wake_timer) {
            if wake_timer {
                // SAFETY: 自分のウィンドウのタイマー。
                let _ = unsafe { KillTimer(Some(window), WAKE_TIMER) };
            }
            if !run_commands(&mut browser, mailbox) {
                return BrowserState::Off;
            }
        } else if msg.hwnd == window && msg.message == WM_APP_FRAME {
            browser.on_frame();
        } else {
            // SAFETY: いま取り出したメッセージ。
            unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        // ブラウザのプロセスがなくなった（イベントのハンドラがそう言った）：もう何も動かない。
        if let Some(state) = webview::take_ending() {
            return state;
        }
    }
}

/// 並んだ命令を動かす。終われと言われたら `false`。
fn run_commands(browser: &mut Browser, mailbox: &Mailbox) -> bool {
    for command in mailbox.take() {
        match command {
            Command::Show => browser.show(),
            Command::Hide => browser.hide(),
            Command::Navigate(url) => browser.navigate(&url),
            Command::Back => browser.back(),
            Command::Forward => browser.forward(),
            Command::Reload => browser.reload(),
            Command::Layout(layout) => browser.set_layout(layout),
            Command::Input(input) => {
                let (method, params) = input.cdp();
                browser.cdp(method, &params);
            }
            Command::Media(command) => browser.media(command),
            Command::Scroll(pages) => browser.scroll(pages),
            Command::Quit => return false,
            #[cfg(test)]
            Command::Eval(script, reply) => browser.eval(&script, move |json| {
                let _ = reply.send(json.to_owned());
            }),
            #[cfg(test)]
            Command::IsMuted(reply) => {
                let _ = reply.send(browser.is_muted());
            }
        }
    }
    true
}

/// WebView のホスト：見せることも前に出すこともないウィンドウ。visual hosting には本物の
/// ウィンドウが要る（メッセージ専用のものではだめ）。
fn host_window() -> windows_core::Result<HWND> {
    let class = w!("reminedog-browser");
    // SAFETY: ただの Win32 の呼び出し。クラスはプロセスの間ある。
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(window_proc),
            hInstance: instance.into(),
            lpszClassName: class,
            ..Default::default()
        };
        // （前の起動で）登録済みなら失敗するが、それでよい。
        RegisterClassExW(&wc);
        CreateWindowExW(
            WINDOW_EX_STYLE(WS_EX_TOOLWINDOW.0 | WS_EX_NOACTIVATE.0),
            class,
            w!("reminedog browser"),
            WS_POPUP,
            0,
            0,
            1,
            1,
            None,
            None,
            Some(instance.into()),
            None,
        )
    }
}

extern "system" fn window_proc(window: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_APP_COMMAND {
        // これを配るのは入れ子のメッセージのループだけ（スレッドのループが先に取る）で、それはこれを
        // 失ってしまう：スレッドのループが戻ったら、タイマーがまた起こす。
        // SAFETY: 自分のウィンドウを、そのスレッドで。
        unsafe { SetTimer(Some(window), WAKE_TIMER, WAKE_INTERVAL, None) };
        return LRESULT(0);
    }
    // SAFETY: ウィンドウ自身のメッセージを渡す。
    unsafe { DefWindowProcW(window, msg, wparam, lparam) }
}
