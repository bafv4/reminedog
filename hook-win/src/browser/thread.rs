//! The browser's thread: a COM apartment with a DispatcherQueue (for the compositor and the
//! capture), a window that is never shown (the WebView's host), and a message loop that
//! carries out the mailbox's commands.

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

/// The timer that brings the loop back to the mailbox after a nested message loop (one that
/// a dialog of the WebView runs) dispatched the wake-up away.
const WAKE_TIMER: usize = 1;
/// How often it fires, in milliseconds.
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

/// The thread's life; the status it ends with.
fn run(generation: u64, mailbox: &Mailbox, start: Start) -> BrowserState {
    // SAFETY: initializes COM for this thread, undone below.
    if let Err(e) = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.ok() {
        log::error!("browser: CoInitializeEx failed: {e}");
        return BrowserState::Failed("ブラウザを起動できません".into());
    }
    let status = run_in_apartment(generation, mailbox, start);
    // SAFETY: balances CoInitializeEx; everything COM was dropped in `run_in_apartment`.
    unsafe { CoUninitialize() };
    status
}

fn run_in_apartment(generation: u64, mailbox: &Mailbox, start: Start) -> BrowserState {
    let options = DispatcherQueueOptions {
        dwSize: size_of::<DispatcherQueueOptions>() as u32,
        threadType: DQTYPE_THREAD_CURRENT,
        apartmentType: DQTAT_COM_STA,
    };
    // SAFETY: plain call; the queue is shut down below.
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
            // SAFETY: our own window, on its thread.
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

/// Shuts down the thread's DispatcherQueue, running this thread's messages until it is done:
/// the thread that made it must, before it ends.
fn shut_down(queue: &DispatcherQueueController) {
    // SAFETY: plain call.
    let thread = unsafe { GetCurrentThreadId() };
    let quit_when_done = queue.ShutdownQueueAsync().and_then(|shutdown| {
        shutdown.when(move |_| {
            // SAFETY: plain call; ends the loop below.
            let _ = unsafe { PostThreadMessageW(thread, WM_QUIT, WPARAM(0), LPARAM(0)) };
        })
    });
    if let Err(e) = quit_when_done {
        log::warn!("browser: shutting down its DispatcherQueue failed: {e}");
        return;
    }
    let mut msg = MSG::default();
    // SAFETY: a valid out parameter; the messages are dispatched as they come.
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
    // From here on the game's thread wakes the loop; commands queued while the WebView was
    // being made (their wake-ups went unhandled) run first.
    mailbox.window.store(window.0 as usize, Ordering::Release);
    if !run_commands(&mut browser, mailbox) {
        return BrowserState::Off;
    }
    let mut msg = MSG::default();
    loop {
        // SAFETY: a valid out parameter.
        let got = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if got.0 <= 0 {
            // WM_QUIT (nobody posts it) or an error.
            return BrowserState::Off;
        }
        let wake_timer = msg.message == WM_TIMER && msg.wParam.0 == WAKE_TIMER;
        if msg.hwnd == window && (msg.message == WM_APP_COMMAND || wake_timer) {
            if wake_timer {
                // SAFETY: our window's timer.
                let _ = unsafe { KillTimer(Some(window), WAKE_TIMER) };
            }
            if !run_commands(&mut browser, mailbox) {
                return BrowserState::Off;
            }
        } else if msg.hwnd == window && msg.message == WM_APP_FRAME {
            browser.on_frame();
        } else {
            // SAFETY: a message just retrieved.
            unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        // The browser's process is gone (an event handler said so): nothing will work again.
        if let Some(state) = webview::take_ending() {
            return state;
        }
    }
}

/// Runs the queued commands; `false` once told to quit.
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

/// The WebView's host: a window that is never shown or activated. Visual hosting needs a real
/// window (not a message-only one).
fn host_window() -> windows_core::Result<HWND> {
    let class = w!("reminedog-browser");
    // SAFETY: plain Win32 calls; the class lives for the process.
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(window_proc),
            hInstance: instance.into(),
            lpszClassName: class,
            ..Default::default()
        };
        // Fails once registered (by an earlier start), which is fine.
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
        // Only a nested message loop dispatches this (the thread's loop takes it first), and
        // would lose it: the timer wakes the thread's loop again once that one is back.
        // SAFETY: our own window, on its thread.
        unsafe { SetTimer(Some(window), WAKE_TIMER, WAKE_INTERVAL, None) };
        return LRESULT(0);
    }
    // SAFETY: forwarding the window's own message.
    unsafe { DefWindowProcW(window, msg, wparam, lparam) }
}
