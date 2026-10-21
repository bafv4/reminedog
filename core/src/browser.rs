//! ゲーム内ブラウザの OS に依存しない部分：Chrome DevTools Protocol（CDP）の呼び出しとしての
//! ページへの入力、URL としてのアドレス欄の文字、ページの動画を操作するスクリプト。
//!
//! ページはウィンドウのメッセージではなく CDP で入力を受け取る：そうすればブラウザは、ゲームから
//! システムのキーボードのフォーカスを取らない（フルスクリーンのゲームは、それを失うと最小化する）。

use serde::Deserialize;
use serde_json::{Value, json};

/// ページから見たマウスのボタン。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageButton {
    Left,
    Middle,
    Right,
    Back,
    Forward,
}

impl PageButton {
    pub const ALL: [PageButton; 5] = [
        PageButton::Left,
        PageButton::Middle,
        PageButton::Right,
        PageButton::Back,
        PageButton::Forward,
    ];

    /// CDP の名前。
    fn name(self) -> &'static str {
        match self {
            PageButton::Left => "left",
            PageButton::Middle => "middle",
            PageButton::Right => "right",
            PageButton::Back => "back",
            PageButton::Forward => "forward",
        }
    }

    /// [`PageInput`] の `buttons` での、ボタンのビット（CDP のビットのフィールド）。
    pub fn bit(self) -> u8 {
        match self {
            PageButton::Left => 1,
            PageButton::Right => 2,
            PageButton::Middle => 4,
            PageButton::Back => 8,
            PageButton::Forward => 16,
        }
    }
}

/// ページのイベントの間に押している修飾キー。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PageModifiers {
    pub alt: bool,
    pub ctrl: bool,
    pub shift: bool,
}

impl PageModifiers {
    /// CDP のビットのフィールド（Alt 1、Ctrl 2、Meta 4、Shift 8）。
    fn bits(self) -> u8 {
        u8::from(self.alt) | u8::from(self.ctrl) << 1 | u8::from(self.shift) << 3
    }
}

/// ページから見たキー：Windows の仮想キーのコードと、DOM の `code` と `key`（`key` は Shift なし。
/// 文字のものは、Shift を押している間は大文字）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageKey {
    pub vk: u16,
    pub code: &'static str,
    pub key: &'static str,
}

impl PageKey {
    pub const PAGE_DOWN: PageKey = PageKey {
        vk: 0x22,
        code: "PageDown",
        key: "PageDown",
    };
    pub const ENTER: PageKey = PageKey {
        vk: 0x0d,
        code: "Enter",
        key: "Enter",
    };
}

/// ページへの 1 つの入力のイベント。位置は、ページの左上の隅からの CSS ピクセル。
#[derive(Debug, Clone, PartialEq)]
pub enum PageInput {
    /// `buttons`（[`PageButton::bit`]）を押したまま、ポインターが動いた。
    MouseMove {
        pos: [f32; 2],
        buttons: u8,
        modifiers: PageModifiers,
    },
    /// `button` が下りた。`clicks` はダブルクリックのクリックを数える。
    MouseDown {
        pos: [f32; 2],
        button: PageButton,
        clicks: u32,
        buttons: u8,
        modifiers: PageModifiers,
    },
    MouseUp {
        pos: [f32; 2],
        button: PageButton,
        clicks: u32,
        buttons: u8,
        modifiers: PageModifiers,
    },
    /// ホイールが回った。`delta` は CSS ピクセルで、正なら右と下にスクロールする。
    Wheel {
        pos: [f32; 2],
        delta: [f32; 2],
        modifiers: PageModifiers,
    },
    KeyDown {
        key: PageKey,
        repeat: bool,
        modifiers: PageModifiers,
    },
    KeyUp {
        key: PageKey,
        modifiers: PageModifiers,
    },
    /// 確定した文字：キーが下りた直後の 1 文字、または IME で変換した文字列。
    Text(String),
}

impl PageInput {
    /// 呼ぶ CDP のメソッドと、その JSON の引数。
    pub fn cdp(&self) -> (&'static str, String) {
        match self {
            PageInput::MouseMove {
                pos,
                buttons,
                modifiers,
            } => mouse("mouseMoved", *pos, None, 0, *buttons, *modifiers),
            PageInput::MouseDown {
                pos,
                button,
                clicks,
                buttons,
                modifiers,
            } => mouse(
                "mousePressed",
                *pos,
                Some(*button),
                *clicks,
                *buttons,
                *modifiers,
            ),
            PageInput::MouseUp {
                pos,
                button,
                clicks,
                buttons,
                modifiers,
            } => mouse(
                "mouseReleased",
                *pos,
                Some(*button),
                *clicks,
                *buttons,
                *modifiers,
            ),
            PageInput::Wheel {
                pos,
                delta,
                modifiers,
            } => {
                let params = json!({
                    "type": "mouseWheel",
                    "x": pos[0],
                    "y": pos[1],
                    "deltaX": delta[0],
                    "deltaY": delta[1],
                    "modifiers": modifiers.bits(),
                });
                ("Input.dispatchMouseEvent", params.to_string())
            }
            PageInput::KeyDown {
                key,
                repeat,
                modifiers,
            } => {
                let mut params = key_params(*key, *modifiers);
                params["autoRepeat"] = json!(repeat);
                // Enter は、自分が打つ文字で動く（送信する、改行する）が、入力はそれを運ばない：制御文字は
                // 文字ではない。
                if *key == PageKey::ENTER && !modifiers.ctrl && !modifiers.alt {
                    params["type"] = json!("keyDown");
                    params["text"] = json!("\r");
                    params["unmodifiedText"] = json!("\r");
                } else {
                    params["type"] = json!("rawKeyDown");
                }
                ("Input.dispatchKeyEvent", params.to_string())
            }
            PageInput::KeyUp { key, modifiers } => {
                let mut params = key_params(*key, *modifiers);
                params["type"] = json!("keyUp");
                ("Input.dispatchKeyEvent", params.to_string())
            }
            PageInput::Text(text) if text.chars().count() == 1 => {
                let params = json!({ "type": "char", "text": text, "unmodifiedText": text });
                ("Input.dispatchKeyEvent", params.to_string())
            }
            PageInput::Text(text) => {
                let params = json!({ "text": text });
                ("Input.insertText", params.to_string())
            }
        }
    }
}

fn mouse(
    kind: &str,
    pos: [f32; 2],
    button: Option<PageButton>,
    clicks: u32,
    buttons: u8,
    modifiers: PageModifiers,
) -> (&'static str, String) {
    let params = json!({
        "type": kind,
        "x": pos[0],
        "y": pos[1],
        "button": button.map_or("none", PageButton::name),
        "buttons": buttons,
        "clickCount": clicks,
        "modifiers": modifiers.bits(),
    });
    ("Input.dispatchMouseEvent", params.to_string())
}

fn key_params(key: PageKey, modifiers: PageModifiers) -> Value {
    let name = match shifted(key.key) {
        Some(shifted) if modifiers.shift => shifted.to_owned(),
        _ if modifiers.shift && key.key.len() == 1 => key.key.to_ascii_uppercase(),
        _ => key.key.to_owned(),
    };
    json!({
        "windowsVirtualKeyCode": key.vk,
        "nativeVirtualKeyCode": key.vk,
        "code": key.code,
        "key": name,
        "modifiers": modifiers.bits(),
    })
}

/// Shift を押したときの、US 配列の数字か記号のキーの DOM の `key`（文字のものは大文字）。
fn shifted(key: &str) -> Option<&'static str> {
    Some(match key {
        "1" => "!",
        "2" => "@",
        "3" => "#",
        "4" => "$",
        "5" => "%",
        "6" => "^",
        "7" => "&",
        "8" => "*",
        "9" => "(",
        "0" => ")",
        "-" => "_",
        "=" => "+",
        "[" => "{",
        "]" => "}",
        "\\" => "|",
        ";" => ":",
        "'" => "\"",
        "," => "<",
        "." => ">",
        "/" => "?",
        "`" => "~",
        _ => return None,
    })
}

/// スキームを付けて打たない限り、サイトとして開かずに検索するファイル名の拡張子
/// （`options.txt`、`latest.log`）。
const FILE_EXTENSIONS: [&str; 26] = [
    "bat",
    "cfg",
    "dat",
    "dll",
    "exe",
    "gif",
    "htm",
    "html",
    "ini",
    "jar",
    "jpeg",
    "jpg",
    "js",
    "json",
    "log",
    "mca",
    "mcfunction",
    "mcmeta",
    "nbt",
    "png",
    "properties",
    "sh",
    "toml",
    "txt",
    "yaml",
    "yml",
];

/// アドレス欄の文字が指すページ：打ったとおりの URL（スキームがなければ `https://` を足す。
/// 自分の機械と IPv4 のアドレスには `http://`。Dynmap などのサーバーの地図は TLS なしで動く）、
/// それ以外はウェブの検索（`options.txt` のようなファイル名も）。空の文字なら `None`。
pub fn normalize_url(text: &str) -> Option<String> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let lower = text.to_ascii_lowercase();
    const SCHEMES: [&str; 5] = ["http:", "https:", "about:", "data:", "file:"];
    if SCHEMES.iter().any(|scheme| lower.starts_with(scheme)) {
        return Some(text.to_owned());
    }
    if !text.contains(char::is_whitespace) {
        let host = lower.split(['/', ':', '?', '#']).next().unwrap_or_default();
        if host == "localhost" || is_ipv4(host) {
            return Some(format!("http://{text}"));
        }
        let extension = host.rsplit('.').next().unwrap_or_default();
        if is_domain(host) && !FILE_EXTENSIONS.contains(&extension) {
            return Some(format!("https://{text}"));
        }
    }
    Some(format!(
        "https://www.google.com/search?q={}",
        encode_query(text)
    ))
}

/// トップレベルドメインのあるホスト名（`minecraft.wiki`）、または IPv4 のアドレス。`1.21` のような
/// 数はどちらでもない。
fn is_domain(host: &str) -> bool {
    let labels: Vec<&str> = host.split('.').collect();
    if labels.len() < 2 || labels.iter().any(|label| label.is_empty()) {
        return false;
    }
    let numeric = |label: &&str| label.bytes().all(|b| b.is_ascii_digit());
    if labels.iter().all(numeric) {
        return labels.len() == 4;
    }
    labels
        .last()
        .is_some_and(|tld| tld.chars().all(char::is_alphabetic))
}

/// 点で区切った 4 つの数：IPv4 のアドレス。
fn is_ipv4(host: &str) -> bool {
    let labels: Vec<&str> = host.split('.').collect();
    labels.len() == 4
        && labels
            .iter()
            .all(|label| !label.is_empty() && label.bytes().all(|b| b.is_ascii_digit()))
}

/// 問い合わせの値をパーセントエンコードする（`application/x-www-form-urlencoded`：空白は `+`）。
fn encode_query(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(char::from(byte));
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// ページの動画（か音声）をどうするか。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MediaCommand {
    PlayPause,
    /// 位置をこの秒数だけ動かす（負なら戻る）。
    Seek(f64),
    /// 再生中のものを全部止め、それを覚える（ブラウザを隠すところ）。
    PauseAll,
    /// [`MediaCommand::PauseAll`] が止めたものをまた再生する。
    Resume,
}

/// ページのメディアの要素。開いた shadow root の中のもの（ウェブコンポーネントで作った
/// プレイヤー）も。フレームの中のものは除く：スクリプトはいちばん外のフレームでだけ動く。
const MEDIA: &str = "const media = () => { const out = [];\
    const walk = (root) => { out.push(...root.querySelectorAll('video, audio'));\
        for (const el of root.querySelectorAll('*')) { if (el.shadowRoot) walk(el.shadowRoot); } };\
    walk(document); return out; };";

/// ページで `command` を行うスクリプト。[`MediaCommand::PlayPause`] と [`MediaCommand::Seek`] は、
/// 再生中のメディアか、なければページに見えているいちばん大きいもの（ページは隠したプレイヤーを
/// 残していることがある）に働き、その [`MediaState`] を値とする（なければ `null`）。
pub fn media_script(command: MediaCommand) -> String {
    const PICK: &str = "const all = media();\
        const area = (m) => { const r = m.getBoundingClientRect(); return r.width * r.height; };\
        const m = all.find((m) => !m.paused && !m.ended)\
            || all.filter((m) => area(m) > 0).sort((a, b) => area(b) - area(a))[0];\
        if (!m) return null;";
    const STATE: &str = "return { paused: m.paused, time: m.currentTime, \
        duration: Number.isFinite(m.duration) ? m.duration : null };";
    match command {
        MediaCommand::PlayPause => format!(
            "(() => {{ {MEDIA} {PICK} if (m.paused) {{ m.play().catch(() => {{}}); }} else {{ m.pause(); }} {STATE} }})()"
        ),
        MediaCommand::Seek(seconds) => format!(
            "(() => {{ {MEDIA} {PICK} const t = Math.max(0, m.currentTime + ({seconds}));\
             m.currentTime = Number.isFinite(m.duration) ? Math.min(m.duration, t) : t; {STATE} }})()"
        ),
        MediaCommand::PauseAll => format!(
            "(() => {{ {MEDIA} \
            const playing = media().filter((m) => !m.paused && !m.ended);\
            playing.forEach((m) => m.pause());\
            window.__reminedogPaused = playing;\
            return playing.length; }})()"
        ),
        MediaCommand::Resume => "(() => { \
            const paused = window.__reminedogPaused || [];\
            window.__reminedogPaused = [];\
            paused.forEach((m) => { if (m.isConnected) m.play().catch(() => {}); });\
            return paused.length; })()"
            .to_owned(),
    }
}

/// [`media_script`] が、働いたメディアについて報告するもの。
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
pub struct MediaState {
    pub paused: bool,
    /// 秒。
    pub time: f64,
    /// 秒。ライブ配信なら `None`。
    pub duration: Option<f64>,
}

/// `command` がページに働いた後の通知：`result` はスクリプトの値の JSON。何も報告しない命令なら
/// `None`。
pub fn media_notice(command: MediaCommand, result: &str) -> Option<String> {
    let action = match command {
        MediaCommand::PlayPause => None,
        MediaCommand::Seek(seconds) if seconds < 0.0 => {
            Some(format!("{} 秒戻した", format_seconds(-seconds)))
        }
        MediaCommand::Seek(seconds) => Some(format!("{} 秒進めた", format_seconds(seconds))),
        MediaCommand::PauseAll | MediaCommand::Resume => return None,
    };
    let Ok(Some(state)) = serde_json::from_str::<Option<MediaState>>(result) else {
        return Some("ページに動画がありません".to_owned());
    };
    let action = action.unwrap_or_else(|| {
        (if state.paused {
            "一時停止"
        } else {
            "再生"
        })
        .to_owned()
    });
    let position = match state.duration {
        Some(duration) => format!("{} / {}", format_time(state.time), format_time(duration)),
        None => format_time(state.time),
    };
    Some(format!("{action}　{position}"))
}

/// `1:05`, `1:02:03`.
fn format_time(seconds: f64) -> String {
    let total = if seconds.is_finite() && seconds > 0.0 {
        seconds as u64
    } else {
        0
    };
    let (h, m, s) = (total / 3600, total / 60 % 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// `10`, `2.5`.
fn format_seconds(seconds: f64) -> String {
    if seconds.fract() == 0.0 {
        format!("{seconds:.0}")
    } else {
        format!("{seconds}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(input: &PageInput) -> (&'static str, Value) {
        let (method, params) = input.cdp();
        (method, serde_json::from_str(&params).unwrap())
    }

    #[test]
    fn mouse_events_carry_the_button_and_the_held_ones() {
        let ctrl = PageModifiers {
            ctrl: true,
            ..Default::default()
        };
        let (method, p) = params(&PageInput::MouseDown {
            pos: [10.5, 20.0],
            button: PageButton::Left,
            clicks: 2,
            buttons: PageButton::Left.bit(),
            modifiers: ctrl,
        });
        assert_eq!(method, "Input.dispatchMouseEvent");
        assert_eq!(
            p,
            json!({"type": "mousePressed", "x": 10.5, "y": 20.0, "button": "left", "buttons": 1,
                "clickCount": 2, "modifiers": 2})
        );
        let (_, p) = params(&PageInput::MouseMove {
            pos: [1.0, 2.0],
            buttons: PageButton::Right.bit() | PageButton::Middle.bit(),
            modifiers: PageModifiers::default(),
        });
        assert_eq!(p["type"], "mouseMoved");
        assert_eq!(p["button"], "none");
        assert_eq!(p["buttons"], 6);
    }

    #[test]
    fn the_wheel_scrolls_by_pixels() {
        let (method, p) = params(&PageInput::Wheel {
            pos: [5.0, 6.0],
            delta: [0.0, 100.0],
            modifiers: PageModifiers::default(),
        });
        assert_eq!(method, "Input.dispatchMouseEvent");
        assert_eq!(p["type"], "mouseWheel");
        assert_eq!(p["deltaY"], 100.0);
    }

    #[test]
    fn keys_go_down_raw_and_typed_text_follows() {
        let a = PageKey {
            vk: 0x41,
            code: "KeyA",
            key: "a",
        };
        let (method, p) = params(&PageInput::KeyDown {
            key: a,
            repeat: false,
            modifiers: PageModifiers::default(),
        });
        assert_eq!(method, "Input.dispatchKeyEvent");
        assert_eq!(p["type"], "rawKeyDown");
        assert_eq!(p["windowsVirtualKeyCode"], 0x41);
        assert_eq!(p["code"], "KeyA");
        assert_eq!(p["key"], "a");
        assert!(p.get("text").is_none());

        let shift = PageModifiers {
            shift: true,
            ..Default::default()
        };
        let (_, p) = params(&PageInput::KeyUp {
            key: a,
            modifiers: shift,
        });
        assert_eq!(p["type"], "keyUp");
        assert_eq!(p["key"], "A");
        assert_eq!(p["modifiers"], 8);

        let (method, p) = params(&PageInput::Text("a".into()));
        assert_eq!(method, "Input.dispatchKeyEvent");
        assert_eq!(
            p,
            json!({"type": "char", "text": "a", "unmodifiedText": "a"})
        );
        let (method, p) = params(&PageInput::Text("日本語".into()));
        assert_eq!(method, "Input.insertText");
        assert_eq!(p, json!({"text": "日本語"}));
    }

    #[test]
    fn enter_types_its_character() {
        let (_, p) = params(&PageInput::KeyDown {
            key: PageKey::ENTER,
            repeat: false,
            modifiers: PageModifiers::default(),
        });
        assert_eq!(p["type"], "keyDown");
        assert_eq!(p["text"], "\r");
        let ctrl = PageModifiers {
            ctrl: true,
            ..Default::default()
        };
        let (_, p) = params(&PageInput::KeyDown {
            key: PageKey::ENTER,
            repeat: false,
            modifiers: ctrl,
        });
        assert_eq!(p["type"], "rawKeyDown");
    }

    #[test]
    fn addresses_become_urls_and_the_rest_searches() {
        let url = |text| normalize_url(text).unwrap();
        assert_eq!(url("https://example.com/a?b"), "https://example.com/a?b");
        assert_eq!(url("HTTP://Example.com"), "HTTP://Example.com");
        assert_eq!(url("about:blank"), "about:blank");
        assert_eq!(url("  example.com/path  "), "https://example.com/path");
        assert_eq!(url("ja.wikipedia.org"), "https://ja.wikipedia.org");
        assert_eq!(url("localhost:8080/x"), "http://localhost:8080/x");
        assert_eq!(
            url("minecraft wiki"),
            "https://www.google.com/search?q=minecraft+wiki"
        );
        assert_eq!(
            url("エンチャント"),
            "https://www.google.com/search?q=%E3%82%A8%E3%83%B3%E3%83%81%E3%83%A3%E3%83%B3%E3%83%88"
        );
        assert_eq!(url("minecraft.wiki"), "https://minecraft.wiki");
        // LAN のサーバーの地図（Dynmap、BlueMap）は TLS を持たない。
        assert_eq!(url("192.168.0.2:8123"), "http://192.168.0.2:8123");
        // ファイル名はサイトとして開かず、検索する。
        assert_eq!(
            url("options.txt"),
            "https://www.google.com/search?q=options.txt"
        );
        assert_eq!(
            url("latest.log"),
            "https://www.google.com/search?q=latest.log"
        );
        assert_eq!(url("1.21"), "https://www.google.com/search?q=1.21");
        assert_eq!(url("v1.2"), "https://www.google.com/search?q=v1.2");
        assert_eq!(url("a&b"), "https://www.google.com/search?q=a%26b");
        assert_eq!(normalize_url("   "), None);
    }

    #[test]
    fn shift_gives_the_shifted_key() {
        let key = |code, name, shift| {
            let (_, params) = PageInput::KeyDown {
                key: PageKey {
                    vk: 0,
                    code,
                    key: name,
                },
                repeat: false,
                modifiers: PageModifiers {
                    shift,
                    ..PageModifiers::default()
                },
            }
            .cdp();
            let value: Value = serde_json::from_str(&params).unwrap();
            value["key"].as_str().unwrap().to_owned()
        };
        assert_eq!(key("Digit1", "1", true), "!");
        assert_eq!(key("Slash", "/", true), "?");
        assert_eq!(key("Period", ".", true), ">");
        assert_eq!(key("KeyA", "a", true), "A");
        assert_eq!(key("Slash", "/", false), "/");
    }

    #[test]
    fn media_scripts_look_into_shadow_roots() {
        for command in [MediaCommand::PlayPause, MediaCommand::PauseAll] {
            assert!(media_script(command).contains("shadowRoot"));
        }
    }

    #[test]
    fn media_scripts_are_expressions() {
        for command in [
            MediaCommand::PlayPause,
            MediaCommand::Seek(-10.0),
            MediaCommand::PauseAll,
            MediaCommand::Resume,
        ] {
            let script = media_script(command);
            assert!(script.starts_with("(() => {"), "{script}");
            assert!(script.ends_with("})()"), "{script}");
        }
        assert!(media_script(MediaCommand::Seek(-2.5)).contains("+ (-2.5)"));
    }

    #[test]
    fn media_notices_say_what_happened() {
        let notice = |command, result| media_notice(command, result).unwrap();
        assert_eq!(
            notice(
                MediaCommand::PlayPause,
                r#"{"paused":true,"time":83.4,"duration":600}"#
            ),
            "一時停止　1:23 / 10:00"
        );
        assert_eq!(
            notice(
                MediaCommand::PlayPause,
                r#"{"paused":false,"time":3725,"duration":null}"#
            ),
            "再生　1:02:05"
        );
        assert_eq!(
            notice(
                MediaCommand::Seek(-10.0),
                r#"{"paused":false,"time":73,"duration":600}"#
            ),
            "10 秒戻した　1:13 / 10:00"
        );
        assert_eq!(
            notice(
                MediaCommand::Seek(2.5),
                r#"{"paused":false,"time":5,"duration":60}"#
            ),
            "2.5 秒進めた　0:05 / 1:00"
        );
        assert_eq!(
            notice(MediaCommand::PlayPause, "null"),
            "ページに動画がありません"
        );
        assert_eq!(media_notice(MediaCommand::PauseAll, "1"), None);
    }

    #[test]
    fn times_read_like_a_player() {
        assert_eq!(format_time(0.0), "0:00");
        assert_eq!(format_time(59.9), "0:59");
        assert_eq!(format_time(600.0), "10:00");
        assert_eq!(format_time(3600.0), "1:00:00");
        assert_eq!(format_time(f64::NAN), "0:00");
    }
}
