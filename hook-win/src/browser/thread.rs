//! The browser's thread: a COM apartment with a DispatcherQueue (for the compositor and the
//! capture), a window that is never shown (the WebView's host), and a message loop that
//! carries out the mailbox's commands.

use std::io;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::WinRT::{
    CreateDispatcherQueueController, DQTAT_COM_STA, DQTYPE_THREAD_CURRENT, DispatcherQueueOptions,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW, MSG,
    RegisterClassExW, TranslateMessage, WINDOW_EX_STYLE, WNDCLASSEXW, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_POPUP,
};
use windows_core::w;

use super::webview::Browser;
use super::{Command, Mailbox, Start, Status, WM_APP_COMMAND, WM_APP_FRAME, report, thread_ended};
use crate::ffi;

pub(super) fn spawn(generation: u64, mailbox: Arc<Mailbox>, start: Start) -> io::Result<()> {
    std::thread::Builder::new()
        .name("reminedog-browser".into())
        .spawn(move || {
            let status = ffi::catch("the browser thread", || run(generation, &mailbox, start))
                .unwrap_or_else(|| Status::Failed("ブラウザが異常終了しました".into()));
            mailbox.window.store(0, Ordering::Release);
            thread_ended(generation, status);
            log::info!("browser: stopped");
        })
        .map(drop)
}

/// The thread's life; the status it ends with.
fn run(generation: u64, mailbox: &Mailbox, start: Start) -> Status {
    // SAFETY: initializes COM for this thread, undone below.
    if let Err(e) = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.ok() {
        log::error!("browser: CoInitializeEx failed: {e}");
        return Status::Failed("ブラウザを起動できません".into());
    }
    let status = run_in_apartment(generation, mailbox, start);
    // SAFETY: balances CoInitializeEx; everything COM was dropped in `run_in_apartment`.
    unsafe { CoUninitialize() };
    status
}

fn run_in_apartment(generation: u64, mailbox: &Mailbox, start: Start) -> Status {
    let options = DispatcherQueueOptions {
        dwSize: size_of::<DispatcherQueueOptions>() as u32,
        threadType: DQTYPE_THREAD_CURRENT,
        apartmentType: DQTAT_COM_STA,
    };
    // SAFETY: plain call; the controller lives until the end of the thread.
    let _queue = match unsafe { CreateDispatcherQueueController(options) } {
        Ok(queue) => queue,
        Err(e) => {
            log::error!("browser: CreateDispatcherQueueController failed: {e}");
            return Status::Failed("ブラウザを起動できません".into());
        }
    };
    let window = match host_window() {
        Ok(window) => window,
        Err(e) => {
            log::error!("browser: cannot create its window: {e}");
            return Status::Failed("ブラウザを起動できません".into());
        }
    };
    let status = run_with_window(generation, mailbox, start, window);
    // SAFETY: our own window, on its thread.
    let _ = unsafe { DestroyWindow(window) };
    status
}

fn run_with_window(generation: u64, mailbox: &Mailbox, start: Start, window: HWND) -> Status {
    log::info!("browser: starting");
    let mut browser = match Browser::create(
        generation,
        window,
        &start.url,
        start.layout,
        &start.data_dir,
    ) {
        Ok(browser) => browser,
        Err(e) => {
            log::error!("browser: {}: {}", e.message, e.detail);
            return Status::Failed(e.message);
        }
    };
    report(generation, |shared| shared.status = Status::Ready);
    log::info!("browser: ready");
    // From here on the game's thread wakes the loop; commands queued while the WebView was
    // being made (their wake-ups went unhandled) run first.
    mailbox.window.store(window.0 as usize, Ordering::Release);
    if !run_commands(&mut browser, mailbox) {
        return Status::Off;
    }
    let mut msg = MSG::default();
    loop {
        // SAFETY: a valid out parameter.
        let got = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if got.0 <= 0 {
            // WM_QUIT (nobody posts it) or an error.
            return Status::Off;
        }
        if msg.hwnd == window && msg.message == WM_APP_COMMAND {
            if !run_commands(&mut browser, mailbox) {
                return Status::Off;
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
    // SAFETY: forwarding the window's own message.
    unsafe { DefWindowProcW(window, msg, wparam, lparam) }
}
