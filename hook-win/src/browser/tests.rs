//! These start a real WebView2, so they need the WebView2 Runtime and a desktop session:
//! `cargo test -p reminedog-hook-win -- --ignored browser`.

use std::sync::{Mutex, mpsc};
use std::time::{Duration, Instant};

use reminedog_core::browser::{PageButton, PageKey, PageModifiers};

use super::*;

/// A red page taller than the view, recording where the mouse went down.
const PAGE: &str = "data:text/html;charset=utf-8,<!doctype html><title>test</title>\
    <body style='margin:0;background:red;height:3000px'>\
    <input id='text' style='position:absolute;left:10px;top:200px;width:100px;height:20px'>\
    <a id='link' href='about:blank' target='_blank' \
    style='position:absolute;left:10px;top:250px;font-size:20px'>link</a>\
    <script>window.downs = []; addEventListener('mousedown', \
    (e) => downs.push([e.clientX, e.clientY]));</script>";

/// The browser is one per process: one test at a time.
static SERIAL: Mutex<()> = Mutex::new(());

fn wait_for(what: &str, timeout: Duration, mut done: impl FnMut() -> bool) {
    let start = Instant::now();
    while !done() {
        assert!(
            start.elapsed() < timeout,
            "timed out waiting for {what}; state: {:?}",
            shared_now()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn shared_now() -> BrowserView {
    lock(&SHARED).view.clone()
}

fn eval(script: &str) -> String {
    let (tx, rx) = mpsc::channel();
    send(Command::Eval(script.to_owned(), tx));
    rx.recv_timeout(Duration::from_secs(10))
        .unwrap_or_else(|_| panic!("no answer to {script}"))
}

/// Stops the browser at the end of a test, also a failed one (the next test starts its own).
struct Running;

impl Drop for Running {
    fn drop(&mut self) {
        quit();
        // Time to close the WebView before its folder goes.
        std::thread::sleep(Duration::from_secs(2));
    }
}

/// Starts the browser on `page` at 400×300 and waits until it is ready.
fn start(page: &str, data: &std::path::Path) -> Running {
    set_layout(PageLayout {
        size: [400, 300],
        scale: 1.0,
        zoom: 1.0,
    });
    show(page, data.to_path_buf());
    let running = Running;
    wait_for("the browser", Duration::from_secs(30), || {
        shared_now().state == BrowserState::Ready
    });
    running
}

/// Waits until the page stops scrolling (the wheel and the keys scroll smoothly).
fn settled_scroll() -> f64 {
    let mut last = f64::NAN;
    loop {
        std::thread::sleep(Duration::from_millis(300));
        let y = eval("window.scrollY").parse::<f64>().unwrap_or(0.0);
        if y == last {
            return y;
        }
        last = y;
    }
}

fn center_pixel() -> Option<[u8; 4]> {
    let frame = FRAME.lock().unwrap();
    if frame.seq == 0 {
        return None;
    }
    let [w, h] = frame.size;
    let i = ((h / 2 * w + w / 2) * 4) as usize;
    frame.bgra.get(i..i + 4).map(|p| [p[0], p[1], p[2], p[3]])
}

fn click(pos: [f32; 2]) {
    let modifiers = PageModifiers::default();
    let bit = PageButton::Left.bit();
    send(Command::Input(PageInput::MouseDown {
        pos,
        button: PageButton::Left,
        clicks: 1,
        buttons: bit,
        modifiers,
    }));
    send(Command::Input(PageInput::MouseUp {
        pos,
        button: PageButton::Left,
        clicks: 1,
        buttons: 0,
        modifiers,
    }));
}

#[test]
#[ignore = "needs the WebView2 Runtime and a desktop session"]
fn browser_draws_and_takes_input() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let data = tempfile::tempdir().unwrap();
    let layout = PageLayout {
        size: [400, 300],
        scale: 1.0,
        zoom: 1.0,
    };
    set_layout(layout);
    show(PAGE, data.path().to_path_buf());
    let _running = Running;
    wait_for("the browser", Duration::from_secs(30), || {
        shared_now().state == BrowserState::Ready
    });

    // The picture arrives, at the page's size, red.
    wait_for("a red picture", Duration::from_secs(10), || {
        center_pixel().is_some_and(|[b, g, r, _]| r > 200 && g < 50 && b < 50)
    });
    assert_eq!(FRAME.lock().unwrap().size, [400, 300]);

    // Clicks land where they are sent, in CSS pixels.
    click([100.0, 50.0]);
    wait_for("the click", Duration::from_secs(5), || {
        eval("downs.length") == "1"
    });
    assert_eq!(eval("downs[0]"), "[100,50]");

    // The page is as many CSS pixels wide as pixels over the scale and the zoom, and
    // positions stay the page's own CSS pixels.
    assert_eq!(eval("innerWidth"), "400");
    set_layout(PageLayout {
        zoom: 1.5,
        ..layout
    });
    wait_for("the zoom", Duration::from_secs(5), || {
        eval("innerWidth") == "266"
    });
    click([100.0, 50.0]);
    wait_for("the zoomed click", Duration::from_secs(5), || {
        eval("downs.length") == "2"
    });
    assert_eq!(eval("downs[1]"), "[100,50]");
    set_layout(PageLayout {
        scale: 2.0,
        ..layout
    });
    wait_for("the scale", Duration::from_secs(5), || {
        eval("innerWidth") == "200"
    });
    set_layout(layout);
    wait_for("the zoom back", Duration::from_secs(5), || {
        eval("innerWidth") == "400"
    });

    // Typing goes into the field clicked.
    click([50.0, 210.0]);
    wait_for("the field's focus", Duration::from_secs(5), || {
        eval("document.activeElement.id") == "\"text\""
    });
    send(Command::Input(PageInput::Text("a".into())));
    send(Command::Input(PageInput::Text("日本語".into())));
    // GLFW hands over committed text one character at a time.
    send(Command::Input(PageInput::Text("ン".into())));
    wait_for("the text", Duration::from_secs(5), || {
        eval("document.getElementById('text').value") == "\"a日本語ン\""
    });

    // Page Down scrolls (the field has the focus, so click the page first).
    click([300.0, 100.0]);
    for key in [
        PageInput::KeyDown {
            key: PageKey::PAGE_DOWN,
            repeat: false,
            modifiers: PageModifiers::default(),
        },
        PageInput::KeyUp {
            key: PageKey::PAGE_DOWN,
            modifiers: PageModifiers::default(),
        },
    ] {
        send(Command::Input(key));
    }
    wait_for("the scroll", Duration::from_secs(5), || {
        eval("window.scrollY").parse::<f64>().unwrap_or(0.0) > 100.0
    });

    // A link to a new window opens in place.
    settled_scroll();
    eval("window.scrollTo({ top: 0, behavior: 'instant' })");
    assert_eq!(settled_scroll(), 0.0);
    click([20.0, 262.0]);
    wait_for("the link", Duration::from_secs(10), || {
        shared_now().url == "about:blank"
    });

    // Hidden, no pictures come; shown again, they do.
    hide();
    std::thread::sleep(Duration::from_millis(500));
    let seq = FRAME.lock().unwrap().seq;
    send(Command::Navigate(PAGE.into()));
    std::thread::sleep(Duration::from_millis(1000));
    assert_eq!(FRAME.lock().unwrap().seq, seq, "pictures while hidden");
    show(PAGE, data.path().to_path_buf());
    wait_for("a picture after showing", Duration::from_secs(10), || {
        FRAME.lock().unwrap().seq > seq
    });

    quit();
    assert_eq!(shared_now().state, BrowserState::Off);
    assert_eq!(FRAME.lock().unwrap().seq, 0);
}

#[test]
#[ignore = "needs the WebView2 Runtime and a desktop session"]
fn the_scroll_keys_work_on_a_page_never_clicked() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let data = tempfile::tempdir().unwrap();
    let _running = start(
        "data:text/html,<body style='height:5000px;background:linear-gradient(red,blue)'>",
        data.path(),
    );
    wait_for("a picture", Duration::from_secs(10), || {
        center_pixel().is_some()
    });
    // A key alone does nothing here (no frame has the focus before a click).
    send(Command::Input(PageInput::KeyDown {
        key: PageKey::PAGE_DOWN,
        repeat: false,
        modifiers: PageModifiers::default(),
    }));
    send(Command::Input(PageInput::KeyUp {
        key: PageKey::PAGE_DOWN,
        modifiers: PageModifiers::default(),
    }));
    send(Command::Scroll(1.0));
    // Seven eighths of the 300 pixels.
    let y = settled_scroll();
    assert!((y - 262.5).abs() < 1.0, "{y}");
    send(Command::Scroll(-1.0));
    assert!(settled_scroll() < 1.0);
}

/// A page with a player of 30 seconds of silence (a WAV made by the page), and a hidden video
/// first in it, which is not the one to play.
const AUDIO_PAGE: &str = "data:text/html,<body><script>\
    const rate = 8000, n = rate * 30, buf = new ArrayBuffer(44 + n), v = new DataView(buf);\
    const w = (o, s) => { for (let i = 0; i < s.length; i++) v.setUint8(o + i, s.charCodeAt(i)); };\
    w(0, 'RIFF'); v.setUint32(4, 36 + n, true); w(8, 'WAVE'); w(12, 'fmt ');\
    v.setUint32(16, 16, true); v.setUint16(20, 1, true); v.setUint16(22, 1, true);\
    v.setUint32(24, rate, true); v.setUint32(28, rate, true); v.setUint16(32, 1, true);\
    v.setUint16(34, 8, true); w(36, 'data'); v.setUint32(40, n, true);\
    for (let i = 0; i < n; i++) v.setUint8(44 + i, 128);\
    document.body.appendChild(document.createElement('video')).style.display = 'none';\
    const a = document.createElement('audio'); a.controls = true;\
    a.src = URL.createObjectURL(new Blob([buf], { type: 'audio/wav' }));\
    document.body.appendChild(a);\
    </script>";

fn muted() -> bool {
    let (tx, rx) = mpsc::channel();
    send(Command::IsMuted(tx));
    rx.recv_timeout(Duration::from_secs(10)).expect("no answer")
}

fn notices() -> Vec<String> {
    before_frame()
        .1
        .into_iter()
        .map(|notice| notice.text)
        .collect()
}

#[test]
#[ignore = "needs the WebView2 Runtime and a desktop session"]
fn the_media_keys_play_seek_and_hiding_pauses() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let data = tempfile::tempdir().unwrap();
    let _running = start(AUDIO_PAGE, data.path());
    wait_for("the audio", Duration::from_secs(10), || {
        eval("document.querySelector('audio') && document.querySelector('audio').readyState")
            .parse::<u32>()
            .is_ok_and(|state| state >= 1)
    });
    notices();
    let paused = || eval("document.querySelector('audio').paused");

    // Played without a click first.
    send(Command::Media(MediaCommand::PlayPause));
    wait_for("playing", Duration::from_secs(5), || paused() == "false");
    let mut said = Vec::new();
    wait_for("the notice", Duration::from_secs(5), || {
        said.extend(notices());
        !said.is_empty()
    });
    assert!(said[0].starts_with("再生　0:0"), "{said:?}");
    assert!(said[0].ends_with(" / 0:30"), "{said:?}");

    send(Command::Media(MediaCommand::Seek(10.0)));
    wait_for("the seek", Duration::from_secs(5), || {
        eval("document.querySelector('audio').currentTime")
            .parse::<f64>()
            .is_ok_and(|t| t >= 10.0)
    });
    send(Command::Media(MediaCommand::Seek(-60.0)));
    wait_for("the seek back", Duration::from_secs(5), || {
        eval("document.querySelector('audio').currentTime")
            .parse::<f64>()
            .is_ok_and(|t| t < 5.0)
    });

    // Hidden, it pauses; shown again, it plays on.
    hide();
    wait_for("paused by hiding", Duration::from_secs(5), || {
        paused() == "true"
    });
    // Players in frames of other sites are not paused, but muted.
    assert!(muted());
    show(AUDIO_PAGE, data.path().to_path_buf());
    wait_for("playing again", Duration::from_secs(5), || {
        paused() == "false"
    });
    assert!(!muted());

    send(Command::Media(MediaCommand::PlayPause));
    wait_for("paused", Duration::from_secs(5), || paused() == "true");
}

#[test]
#[ignore = "needs the WebView2 Runtime and a desktop session"]
fn a_file_input_opens_no_dialog() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let data = tempfile::tempdir().unwrap();
    let _running = start(
        "data:text/html,<body style='margin:0'>\
        <input type='file' id='file' style='position:absolute;left:0;top:0;width:200px;height:40px'>\
        <script>window.cancelled = false;\
        file.addEventListener('cancel', () => window.cancelled = true);</script>",
        data.path(),
    );
    wait_for("a picture", Duration::from_secs(10), || {
        center_pixel().is_some()
    });
    click([20.0, 20.0]);
    // The page hears the dialog was cancelled. Had it opened (in this process, on the browser
    // thread), the script would get no answer.
    wait_for("the cancelled dialog", Duration::from_secs(10), || {
        eval("window.cancelled") == "true"
    });
}
