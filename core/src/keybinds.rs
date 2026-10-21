//! Minecraft のキーの名前と、`<game_dir>/options.txt` のキーの割り当て。
//!
//! options.txt は割り当てを `key_<割り当ての id>:<キーの名前>` で書く（例：
//! `key_key.debug.modifier:key.keyboard.f3`）。キーの名前はゲーム自身の表から来る（`keytable.rs`）：
//! 26.x（SDL3）は SDL のスキャンコードごとに、1.13〜1.21（GLFW）は GLFW のキーコードごとに名前を
//! 持ち、3 つの名前は両者で別のキーを指す（[`Naming`]）。[`InputId`] はどちらにも依らないキーか
//! マウスのボタンで、各ライブラリのコードとの相互の変換と、メニューのためのラベルを持つ。
//!
//! F3+C は、デバッグの修飾キーと座標のコピーの割り当てのキーのイベントとして送る。新しい版
//! （1.21.11、26.x）は両方を変えられる。古い版（1.16）にはその行がなく、F3 と C に決め打ちで、
//! これはここでの既定でもある。F3+C が使う名前の引き当て（[`glfw_key`]、[`sdl_scancode`]、
//! [`sdl_keycode`]）が知っているのは、デバッグの割り当てが使いそうなキーだけ：文字、数字、
//! F キー、テンキーの数字。

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use crate::keytable::{GLFW_KEYS, SDL_KEYS};

/// キーのない割り当てに options.txt が書くキーの名前。
pub const UNBOUND: &str = "key.keyboard.unknown";

const KEYBOARD_PREFIX: &str = "key.keyboard.";
const MOUSE_PREFIX: &str = "key.mouse.";
/// GLFW にキーコードのないキーの 1.21 の名前（Win32 のスキャンコードごと）。
const SCANCODE_PREFIX: &str = "scancode.";

const MODIFIER_ID: &str = "key.debug.modifier";
const OVERLAY_ID: &str = "key.debug.overlay";
const COPY_LOCATION_ID: &str = "key.debug.copyLocation";
const CRASH_ID: &str = "key.debug.crash";
/// ゲームが tick の間に「押している」としてしか読まない割り当て。断られた F3+C は 1 回の
/// ポーリングの間にコピーのキーを押して離すので、これらには見えない。
const HELD_ONLY_IDS: [&str; 8] = [
    "key.forward",
    "key.left",
    "key.back",
    "key.right",
    "key.jump",
    "key.playerlist",
    "key.saveToolbarActivator",
    "key.loadToolbarActivator",
];
/// 押すと持っているアイテムを捨てる。Ctrl を物理的に押していればスタックごと。
const DROP_ID: &str = "key.drop";

/// options.txt に Minecraft が書くキーの名前での、F3+C を作るキー（例："key.keyboard.f3"）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DebugKeys {
    pub modifier: String,
    pub copy_location: String,
    pub crash: String,
    /// 座標のコピーのキーに割り当てられた、押下で動くほかの（デバッグでない）割り当て（id で。
    /// 例："key.drop"）。断られた F3+C は、それぞれを 1 回押す。
    pub shared_with_copy: Vec<String>,
}

impl DebugKeys {
    /// 断られた F3+C が持っているアイテムを捨てるか（Ctrl を押していればスタックごと）。
    pub fn copy_drops_items(&self) -> bool {
        self.shared_with_copy.iter().any(|id| id == DROP_ID)
    }
}

impl Default for DebugKeys {
    fn default() -> Self {
        Self {
            modifier: "key.keyboard.f3".into(),
            copy_location: "key.keyboard.c".into(),
            crash: "key.keyboard.c".into(),
            shared_with_copy: Vec::new(),
        }
    }
}

/// options.txt の `key_<id>:<name>` の行。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct KeyLine<'a> {
    id: &'a str,
    name: &'a str,
    /// Forge と NeoForge は、割り当てに要る修飾キーを名前の後に書く（`key.keyboard.c:CONTROL`）。
    /// キーだけでは動かない。
    with_modifier: bool,
}

/// options.txt の `key_<id>:<name>` の行（ファイルの順）。同じ id が繰り返されたら最後の行が
/// 勝ち、最初の位置を保つ。CRLF、BOM、各部分の前後の空白は許す。空の部分のある行は飛ばす。
/// 名前の後の Forge の修飾キーは切り離す。
fn key_lines(options_txt: &str) -> Vec<KeyLine<'_>> {
    let text = options_txt.strip_prefix('\u{feff}').unwrap_or(options_txt);
    let mut bindings: Vec<KeyLine<'_>> = Vec::new();
    let mut index: HashMap<&str, usize> = HashMap::new();
    for line in text.lines() {
        let Some((id, name)) = line
            .trim()
            .strip_prefix("key_")
            .and_then(|rest| rest.split_once(':'))
        else {
            continue;
        };
        let (id, name) = (id.trim(), name.trim());
        let (name, with_modifier) = match name.rsplit_once(':') {
            Some((key, modifier))
                if !modifier.is_empty()
                    && modifier
                        .bytes()
                        .all(|b| b.is_ascii_uppercase() || b == b'_') =>
            {
                (key.trim(), modifier != "NONE")
            }
            _ => (name, false),
        };
        if id.is_empty() || name.is_empty() {
            continue;
        }
        let line = KeyLine {
            id,
            name,
            with_modifier,
        };
        match index.get(id) {
            Some(&i) => bindings[i] = line,
            None => {
                index.insert(id, bindings.len());
                bindings.push(line);
            }
        }
    }
    bindings
}

/// options.txt の文字からデバッグのキーを読む。
///
/// `key_<id>:<name>` の行を全部読み、同じ id が繰り返されたら最後のものが勝つ。デバッグの行が
/// なければ既定のまま（1.16 にはない）。CRLF、BOM、各部分の前後の空白は許す。
/// `shared_with_copy` は、座標のコピーのキーに割り当てられた `key.debug.*` 以外の割り当てを
/// ファイルの順に並べる。ただし、押しているとしてしか読まれないものと、（Forge で）修飾キーも
/// 要るものは除く。座標のコピーのキーに割り当てがなければ、何も共有しない。
pub fn parse_debug_keys(options_txt: &str) -> DebugKeys {
    let bindings = key_lines(options_txt);
    let bound = |id: &str| {
        bindings
            .iter()
            .find(|line| line.id == id)
            .map(|line| line.name.to_owned())
    };
    let defaults = DebugKeys::default();
    let copy_location = bound(COPY_LOCATION_ID).unwrap_or(defaults.copy_location);
    let shared_with_copy = if copy_location == UNBOUND {
        Vec::new()
    } else {
        bindings
            .iter()
            .filter(|line| {
                !line.id.starts_with("key.debug.")
                    && !HELD_ONLY_IDS.contains(&line.id)
                    && !line.with_modifier
                    && line.name == copy_location
            })
            .map(|line| line.id.to_owned())
            .collect()
    };
    DebugKeys {
        modifier: bound(MODIFIER_ID).unwrap_or(defaults.modifier),
        copy_location,
        crash: bound(CRASH_ID).unwrap_or(defaults.crash),
        shared_with_copy,
    }
}

/// options.txt の文字の、各キーかマウスのボタンに割り当てた割り当て（割り当ての id で、
/// ファイルの順。例：`(Key(60), ["key.debug.overlay", "key.debug.modifier"])`）。キーが
/// ゲームで何をするかを見せるため。キーは最初の割り当ての順に並ぶ。
///
/// 行は [`parse_debug_keys`] と同じく読み、キーの名前は `naming` で読む。割り当てのないものと
/// キーでない名前は除き、修飾キーとオーバーレイの切り替え以外のデバッグの割り当ても除く：
/// それらは修飾キーと一緒のときだけ動く。
pub fn bindings_by_key(options_txt: &str, naming: Naming) -> Vec<(InputId, Vec<String>)> {
    let mut by_key: Vec<(InputId, Vec<String>)> = Vec::new();
    let mut index: HashMap<InputId, usize> = HashMap::new();
    for KeyLine { id, name, .. } in key_lines(options_txt) {
        if id.starts_with("key.debug.") && id != MODIFIER_ID && id != OVERLAY_ID {
            continue;
        }
        let Some(input) = input_by_name(name, naming) else {
            continue;
        };
        match index.get(&input) {
            Some(&i) => by_key[i].1.push(id.to_owned()),
            None => {
                index.insert(input, by_key.len());
                by_key.push((input, vec![id.to_owned()]));
            }
        }
    }
    by_key
}

/// `<game_dir>/options.txt`：キーの割り当てを含む、ゲームの設定。
pub fn options_path(game_dir: &Path) -> PathBuf {
    game_dir.join("options.txt")
}

/// `<game_dir>/options.txt` からデバッグのキーを読む。ファイルがなければ（新しいインスタンス）
/// 既定のもの。UTF-8 でない文字は、壊れた部分を置き換えて読む。
pub fn load_debug_keys(game_dir: &Path) -> io::Result<DebugKeys> {
    match fs::read(options_path(game_dir)) {
        Ok(bytes) => Ok(parse_debug_keys(&String::from_utf8_lossy(&bytes))),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(DebugKeys::default()),
        Err(e) => Err(e),
    }
}

/// キーの id はこれより小さい（SDL3 の `SDL_SCANCODE_COUNT`）。この長さの配列は、どのキーの値も持てる。
pub const SCANCODE_COUNT: usize = 512;
/// 最初のキーの id：SDL3 に `SDL_SCANCODE_A` より下のキーはなく、1..=3 の名前は数字のキーと
/// 読まれてしまう。
const FIRST_SCANCODE: u16 = 4;
/// マウスの id は 1..=これ：Windows では GLFW も SDL3 もほかのボタンを報告しない。
const MOUSE_BUTTONS: u8 = 5;
/// GLFW のキーコードはこれより小さい（`GLFW_KEY_LAST` + 1）。
const GLFW_KEY_COUNT: usize = 349;
/// SDLK_SCANCODE_MASK：文字のないキーは、スキャンコードにこのビットを立てたものを使う。
const SCANCODE_MASK: u32 = 1 << 30;

/// ウィンドウのライブラリに依らない、キーかマウスのボタン。
///
/// `Key` は SDL3 のスキャンコード（4..512）、`Mouse` は SDL3 のボタンの番号（1 左、2 中、3 右、
/// 4 と 5 は横のボタン）。どちらも 26.x が使う値。ゲームの名前で [`input_name`] と
/// [`input_by_name`] で名付ける。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum InputId {
    Key(u16),
    Mouse(u8),
}

/// どのキーの名前で読むか：いくつかの名前は 1.21 と 26.x で別のキーを指し（1.21 の
/// `keypad.decimal`、`menu`、`world.2` は 26.x の `keypad.period`、`application`、
/// `world.1`）、マウスのボタンの番号の振り方も違う。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Naming {
    /// 26.x（SDL3）：settings.json と、SDL3 のもとでの options.txt の名前。
    Modern,
    /// 1.13〜1.21（GLFW）の options.txt のとおり：GLFW にキーコードのないキー（日本語のキー）は
    /// `scancode.<n>`、名前のないマウスのボタンは GLFW の番号 + 1。
    Glfw,
}

/// キーのイベントの修飾キーのビットのための、修飾キーの種類。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Modifier {
    Shift,
    Ctrl,
    Alt,
    /// Windows キー（GLFW の Super、SDL3 の GUI）。
    Super,
}

/// 左右の修飾キーのどちらか。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Side {
    Left,
    Right,
}

/// GLFW にキーコードのない日本語のキーボードのキー：GLFW はこれを Win32 のスキャンコード付きの
/// key -1 として報告し、1.21 は `scancode.<n>` と名付ける。SDL3 の Windows のスキャンコードの表の
/// とおりの (Win32 のスキャンコード, SDL3 のスキャンコード)。
const JIS_KEYS: [(u16, u16); 5] = [
    // カタカナ/ひらがな: international2.
    (0x70, 136),
    // ろ: international1.
    (0x73, 135),
    // 変換: international4.
    (0x79, 138),
    // 無変換: international5.
    (0x7b, 139),
    // ¥：international3。
    (0x7d, 137),
];

/// SDL のスキャンコードごとの [`SDL_KEYS`] の行 + 1（0：名前なし）。作るときに表を確かめる：
/// 間違った、または重なったスキャンコードはコンパイルできない。
const SDL_ROW: [u8; SCANCODE_COUNT] = {
    assert!(SDL_KEYS.len() < u8::MAX as usize);
    let mut rows = [0; SCANCODE_COUNT];
    let mut i = 0;
    while i < SDL_KEYS.len() {
        let sc = SDL_KEYS[i].0 as usize;
        assert!(sc >= FIRST_SCANCODE as usize && sc < SCANCODE_COUNT && rows[sc] == 0);
        rows[sc] = (i + 1) as u8;
        i += 1;
    }
    rows
};

/// GLFW のキーコードごとの [`GLFW_KEYS`] の行 + 1（0：そのキーはない）。
const GLFW_ROW: [u8; GLFW_KEY_COUNT] = {
    assert!(GLFW_KEYS.len() < u8::MAX as usize);
    let mut rows = [0; GLFW_KEY_COUNT];
    let mut i = 0;
    while i < GLFW_KEYS.len() {
        let key = GLFW_KEYS[i].0;
        assert!(key >= 0 && (key as usize) < GLFW_KEY_COUNT && rows[key as usize] == 0);
        rows[key as usize] = (i + 1) as u8;
        i += 1;
    }
    rows
};

/// SDL のスキャンコードごとの [`GLFW_KEYS`] の行 + 1（0：GLFW にそのキーはない）。1 つの
/// スキャンコードに 2 つの GLFW のキー（`world.1` と `world.2`）があれば、Windows にある方
/// （Win32 のスキャンコードを持つ方）。
const GLFW_ROW_BY_SDL: [u8; SCANCODE_COUNT] = {
    let mut rows = [0; SCANCODE_COUNT];
    let mut i = 0;
    while i < GLFW_KEYS.len() {
        let (_, _, sc, win) = GLFW_KEYS[i];
        let sc = sc as usize;
        assert!(sc < SCANCODE_COUNT && (sc == 0 || SDL_ROW[sc] != 0));
        if sc != 0 {
            let old = rows[sc];
            // Win32 のスキャンコードを持つのは、多くても 1 つ。
            assert!(old == 0 || GLFW_KEYS[old as usize - 1].3 == 0 || win == 0);
            if old == 0 || win != 0 {
                rows[sc] = (i + 1) as u8;
            }
        }
        i += 1;
    }
    rows
};

/// スキャンコードの [`SDL_KEYS`] の行。
const fn sdl_row(sc: u16) -> Option<(u16, &'static str, u32)> {
    if sc as usize >= SCANCODE_COUNT {
        return None;
    }
    match SDL_ROW[sc as usize] {
        0 => None,
        row => Some(SDL_KEYS[row as usize - 1]),
    }
}

/// GLFW のキーコードの [`GLFW_KEYS`] の行。
const fn glfw_row(key: i32) -> Option<(i32, &'static str, u16, u16)> {
    if key < 0 || key as usize >= GLFW_KEY_COUNT {
        return None;
    }
    match GLFW_ROW[key as usize] {
        0 => None,
        row => Some(GLFW_KEYS[row as usize - 1]),
    }
}

/// SDL のスキャンコードの [`GLFW_KEYS`] の行。
const fn glfw_row_of_sdl(sc: u16) -> Option<(i32, &'static str, u16, u16)> {
    if sc as usize >= SCANCODE_COUNT {
        return None;
    }
    match GLFW_ROW_BY_SDL[sc as usize] {
        0 => None,
        row => Some(GLFW_KEYS[row as usize - 1]),
    }
}

/// Win32 のスキャンコードから、日本語のキーの SDL のスキャンコード。
const fn jis_by_win_scancode(win: i32) -> Option<u16> {
    let mut i = 0;
    while i < JIS_KEYS.len() {
        if JIS_KEYS[i].0 as i32 == win {
            return Some(JIS_KEYS[i].1);
        }
        i += 1;
    }
    None
}

/// SDL のスキャンコードから、日本語のキーの Win32 のスキャンコード。
const fn jis_win_scancode(sc: u16) -> Option<u16> {
    let mut i = 0;
    while i < JIS_KEYS.len() {
        if JIS_KEYS[i].1 == sc {
            return Some(JIS_KEYS[i].0);
        }
        i += 1;
    }
    None
}

/// スキャンコードがキーの id の範囲にあるか。
const fn is_key(sc: u16) -> bool {
    sc >= FIRST_SCANCODE && (sc as usize) < SCANCODE_COUNT
}

/// 名前のないキーにゲームが書く 10 進の数：数字だけ。
fn number(text: &str) -> Option<u32> {
    if text.is_empty() || text.len() > 9 || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// 名前ごとの [`SDL_KEYS`] の行。
static SDL_NAMES: LazyLock<HashMap<&'static str, u16>> =
    LazyLock::new(|| SDL_KEYS.iter().map(|&(sc, name, _)| (name, sc)).collect());
/// [`GLFW_KEYS`] の行：GLFW のキーコード、その名前、SDL のスキャンコード。
type GlfwRow = (i32, &'static str, u16, u16);

/// 名前ごとの [`GLFW_KEYS`] の行。
static GLFW_NAMES: LazyLock<HashMap<&'static str, GlfwRow>> =
    LazyLock::new(|| GLFW_KEYS.iter().map(|&row| (row.1, row)).collect());

/// 26.x の `key.keyboard.` の後の名前からキーの id：その名前か、スキャンコードそのもの。
fn modern_key(rest: &str) -> Option<InputId> {
    let sc = match SDL_NAMES.get(rest) {
        Some(&sc) => sc,
        None => u16::try_from(number(rest)?).ok()?,
    };
    is_key(sc).then_some(InputId::Key(sc))
}

/// 1.21 の `key.keyboard.` の後の名前の [`GLFW_KEYS`] の行：その名前か、GLFW のキーコードそのもの。
fn glfw_name_row(rest: &str) -> Option<(i32, &'static str, u16, u16)> {
    match GLFW_NAMES.get(rest) {
        Some(&row) => Some(row),
        None => glfw_row(i32::try_from(number(rest)?).ok()?),
    }
}

/// options.txt か settings.json のキーの名前（例：`key.keyboard.f3`、`key.mouse.4`）を、
/// `naming` の読み方で読んだキーかマウスのボタン。ゲームと同じく、数は名前のないキーを表す
/// （26.x では `key.keyboard.<スキャンコード>`、1.21 では `key.keyboard.<GLFW のキー>`）。
/// `key.keyboard.unknown`、範囲外の id、もう一方のライブラリに同じもののないキー（GLFW の F25）、
/// そのほかは `None`。
pub fn input_by_name(name: &str, naming: Naming) -> Option<InputId> {
    if let Some(rest) = name.strip_prefix(KEYBOARD_PREFIX) {
        return match naming {
            Naming::Modern => modern_key(rest),
            Naming::Glfw => {
                let (_, _, sc, _) = glfw_name_row(rest)?;
                is_key(sc).then_some(InputId::Key(sc))
            }
        };
    }
    if let Some(rest) = name.strip_prefix(MOUSE_PREFIX) {
        return match naming {
            Naming::Modern => {
                let button = match rest {
                    "left" => 1,
                    "middle" => 2,
                    "right" => 3,
                    _ => number(rest)?,
                };
                (1..=u32::from(MOUSE_BUTTONS))
                    .contains(&button)
                    .then_some(InputId::Mouse(button as u8))
            }
            Naming::Glfw => {
                let button = match rest {
                    "left" => 0,
                    "right" => 1,
                    "middle" => 2,
                    _ => number(rest)?.checked_sub(1)?,
                };
                input_from_glfw_button(i32::try_from(button).ok()?)
            }
        };
    }
    match naming {
        Naming::Glfw => {
            let win = number(name.strip_prefix(SCANCODE_PREFIX)?)?;
            jis_by_win_scancode(i32::try_from(win).ok()?).map(InputId::Key)
        }
        Naming::Modern => None,
    }
}

/// キーかマウスのボタンの 26.x の名前（settings.json に書くもの。例：`key.keyboard.f3`、
/// `key.mouse.4`）。名前のないキーは `key.keyboard.<スキャンコード>`。範囲内のどの id でも、
/// [`Naming::Modern`] の [`input_by_name`] で読み戻せる。範囲外の id には、読み戻せない名前を付ける。
pub fn input_name(id: InputId) -> String {
    match id {
        InputId::Key(sc) => match sdl_row(sc) {
            Some((_, name, _)) => format!("{KEYBOARD_PREFIX}{name}"),
            None if sc < FIRST_SCANCODE => UNBOUND.to_owned(),
            None => format!("{KEYBOARD_PREFIX}{sc}"),
        },
        InputId::Mouse(1) => format!("{MOUSE_PREFIX}left"),
        InputId::Mouse(2) => format!("{MOUSE_PREFIX}middle"),
        InputId::Mouse(3) => format!("{MOUSE_PREFIX}right"),
        InputId::Mouse(button) => format!("{MOUSE_PREFIX}{button}"),
    }
}

/// キーかマウスのボタンのメニューでのラベル：`A`、`1`、`F3`、`左 Ctrl`、`スペース`、`↑`、
/// 記号は US 配列のもの、`テンキー 1`、`テンキーの Enter`、日本語のキー（`ろ`、`¥`、`変換`、
/// `無変換`、`カタカナ/ひらがな`）、`左クリック`、`マウスのボタン4`。ここにラベルのないキーは
/// `キー 130`。
pub fn input_label(id: InputId) -> String {
    match id {
        InputId::Key(sc) => key_id_label(sc),
        InputId::Mouse(button) => match button {
            1 => "左クリック".to_owned(),
            2 => "ホイールクリック".to_owned(),
            3 => "右クリック".to_owned(),
            _ => format!("マウスのボタン{button}"),
        },
    }
}

fn key_id_label(sc: u16) -> String {
    let label = match sc {
        // 文字、次に数字の列の 1..9、0。
        4..=29 => return char::from(b'A' + (sc - 4) as u8).to_string(),
        30..=38 => return (sc - 29).to_string(),
        39 => "0",
        58..=69 => return format!("F{}", sc - 57),
        104..=115 => return format!("F{}", sc - 91),
        89..=97 => return format!("テンキー {}", sc - 88),
        98 => "テンキー 0",
        40 => "Enter",
        41 => "Esc",
        42 => "Backspace",
        43 => "Tab",
        44 => "スペース",
        45 => "-",
        46 => "=",
        47 => "[",
        48 => "]",
        49 => "\\",
        51 => ";",
        52 => "'",
        53 => "`",
        54 => ",",
        55 => ".",
        56 => "/",
        57 => "CapsLock",
        70 => "PrintScreen",
        71 => "ScrollLock",
        72 => "Pause",
        73 => "Insert",
        74 => "Home",
        75 => "PageUp",
        76 => "Delete",
        77 => "End",
        78 => "PageDown",
        79 => "→",
        80 => "←",
        81 => "↓",
        82 => "↑",
        83 => "NumLock",
        84 => "テンキーの /",
        85 => "テンキーの *",
        86 => "テンキーの -",
        87 => "テンキーの +",
        88 => "テンキーの Enter",
        99 => "テンキーの .",
        101 => "アプリケーション",
        103 => "テンキーの =",
        133 => "テンキーの ,",
        135 => "ろ",
        136 => "カタカナ/ひらがな",
        137 => "¥",
        138 => "変換",
        139 => "無変換",
        224 => "左 Ctrl",
        225 => "左 Shift",
        226 => "左 Alt",
        227 => "左 Windows",
        228 => "右 Ctrl",
        229 => "右 Shift",
        230 => "右 Alt",
        231 => "右 Windows",
        _ => return format!("キー {sc}"),
    };
    label.to_owned()
}

/// GLFW のキーのイベント（1.13〜1.21）のキー：キーコードで。key -1（GLFW にコードのないキー）は
/// Win32 のスキャンコードで（ゲームもそれで見分ける）。SDL3 に同じもののないキー（F25）と
/// 範囲外のコードは `None`。
pub const fn input_from_glfw_key(key: i32, win_scancode: i32) -> Option<InputId> {
    if key == -1 {
        return match jis_by_win_scancode(win_scancode) {
            Some(sc) => Some(InputId::Key(sc)),
            None => None,
        };
    }
    match glfw_row(key) {
        Some((_, _, sc, _)) if is_key(sc) => Some(InputId::Key(sc)),
        _ => None,
    }
}

/// GLFW のボタンの番号のマウスのボタン：GLFW は 0 から数え、中より右が先（0 左、1 右、2 中、
/// 3 と 4 は横のボタン）。
pub const fn input_from_glfw_button(button: i32) -> Option<InputId> {
    match button {
        0 => Some(InputId::Mouse(1)),
        1 => Some(InputId::Mouse(3)),
        2 => Some(InputId::Mouse(2)),
        3 => Some(InputId::Mouse(4)),
        4 => Some(InputId::Mouse(5)),
        _ => None,
    }
}

/// キーを送るときの GLFW のキーコードと Win32 のスキャンコード：GLFW にコードのないキーは
/// `(-1, スキャンコード)`、それ以外は `(キーコード, スキャンコードか 0)`。マウスのボタンと、GLFW に
/// ないキーは `None`。コードのあるキーのスキャンコードは GLFW 3.4 が Windows で報告するもので、
/// ゲームは無視する。
pub const fn glfw_key_of(id: InputId) -> Option<(i32, i32)> {
    let InputId::Key(sc) = id else {
        return None;
    };
    if let Some(win) = jis_win_scancode(sc) {
        return Some((-1, win as i32));
    }
    match glfw_row_of_sdl(sc) {
        Some((key, _, _, win)) => Some((key, win as i32)),
        None => None,
    }
}

/// マウスのボタンの GLFW のボタンの番号（[`input_from_glfw_button`] の逆）。
pub const fn glfw_button_of(id: InputId) -> Option<i32> {
    match id {
        InputId::Mouse(1) => Some(0),
        InputId::Mouse(3) => Some(1),
        InputId::Mouse(2) => Some(2),
        InputId::Mouse(4) => Some(3),
        InputId::Mouse(5) => Some(4),
        _ => None,
    }
}

/// キーの SDL3 のキーコード（キーのイベントの `key` のフィールド）。US 配列の SDL3 の既定の
/// キーマップのとおり：文字、数字、記号はその文字、ほかのキーはスキャンコードに
/// `SDLK_SCANCODE_MASK` を立てたもの。マウスのボタンと範囲外の id は 0（`SDLK_UNKNOWN`）。
pub const fn sdl_keycode_of(id: InputId) -> u32 {
    match id {
        InputId::Key(sc) => match sdl_row(sc) {
            Some((_, _, keycode)) => keycode,
            None if is_key(sc) => SCANCODE_MASK | sc as u32,
            None => 0,
        },
        InputId::Mouse(_) => 0,
    }
}

/// GLFW 3.4 が報告するキーの Win32 のスキャンコード（セット 1。0xE0 の前置きは 0x100）。
/// マウスのボタンと、GLFW が知らないキーは `None`。
pub const fn win_scancode_of(id: InputId) -> Option<u16> {
    let InputId::Key(sc) = id else {
        return None;
    };
    if let Some(win) = jis_win_scancode(sc) {
        return Some(win);
    }
    match glfw_row_of_sdl(sc) {
        Some((_, _, _, win)) if win != 0 => Some(win),
        _ => None,
    }
}

/// キーがどの修飾キーか：左右の Ctrl、Shift、Alt、Windows キー。
pub const fn modifier_kind(id: InputId) -> Option<(Modifier, Side)> {
    let InputId::Key(sc) = id else {
        return None;
    };
    Some(match sc {
        224 => (Modifier::Ctrl, Side::Left),
        225 => (Modifier::Shift, Side::Left),
        226 => (Modifier::Alt, Side::Left),
        227 => (Modifier::Super, Side::Left),
        228 => (Modifier::Ctrl, Side::Right),
        229 => (Modifier::Shift, Side::Right),
        230 => (Modifier::Alt, Side::Right),
        231 => (Modifier::Super, Side::Right),
        _ => return None,
    })
}

/// F3+C がキー（SDL のスキャンコードで）を使えるか：文字、数字、F キー、テンキーの数字。
const fn f3c_key(sc: u16) -> bool {
    matches!(sc, 4..=39 | 58..=69 | 89..=98 | 104..=115)
}

/// 1.21 のキーの名前の GLFW のキーコード（1.13〜1.21 はこれで割り当てを照らし合わせる）。
/// F3+C が使えるキー：文字、数字、F1..F25、テンキーの数字。
pub fn glfw_key(name: &str) -> Option<i32> {
    let rest = name.strip_prefix(KEYBOARD_PREFIX)?;
    let &(key, _, _, _) = GLFW_NAMES.get(rest)?;
    matches!(key, 48..=57 | 65..=90 | 290..=314 | 320..=329).then_some(key)
}

/// F3+C が使えるキーについて、26.x のキーの名前のキー。
fn f3c_scancode(name: &str) -> Option<u16> {
    let rest = name.strip_prefix(KEYBOARD_PREFIX)?;
    let &sc = SDL_NAMES.get(rest)?;
    f3c_key(sc).then_some(sc)
}

/// キーの名前の SDL3 のスキャンコード（26.x はこれで割り当てを照らし合わせる）。F3+C が使える
/// キー：文字、数字、F1..F24（SDL に F25 はない）、テンキーの数字。
pub fn sdl_scancode(name: &str) -> Option<u32> {
    f3c_scancode(name).map(u32::from)
}

/// 作ったキーのイベントの `key` のフィールドのための、キーの名前の SDL3 のキーコード。
/// キーは [`sdl_scancode`] と同じ。
pub fn sdl_keycode(name: &str) -> Option<u32> {
    f3c_scancode(name).map(|sc| sdl_keycode_of(InputId::Key(sc)))
}

/// キーの名前の短いラベル。例："F3"、"C"、"テンキー 1"、"左 Shift"、"マウスの戻る"
/// （[`input_label`] と同じ。26.x と 1.21 のどちらの名前でも）。どちらの名前でもないものは
/// そのまま出す。
pub fn key_label(name: &str) -> String {
    if let Some(sc) = f3c_scancode(name) {
        return input_label(InputId::Key(sc));
    }
    if let Some(key @ 290..=314) = glfw_key(name) {
        // SDL3 にない、GLFW の F25。
        return format!("F{}", key - 289);
    }
    match input_by_name(name, Naming::Modern).or_else(|| input_by_name(name, Naming::Glfw)) {
        Some(id) => input_label(id),
        None => name.to_owned(),
    }
}

/// "key.drop" のようなバニラの割り当ての id の日本語の名前（ゲームの ja_jp の名前）。ほかの
/// id はそのまま返す。
pub fn mapping_label(id: &str) -> String {
    let label = match id {
        "key.forward" => "前進",
        "key.left" => "左",
        "key.back" => "後退",
        "key.right" => "右",
        "key.jump" => "ジャンプ",
        "key.sneak" => "スニーク",
        "key.sprint" => "ダッシュ",
        "key.inventory" => "インベントリの開閉",
        "key.swapOffhand" => "アイテムをオフハンドと交換",
        "key.drop" => "アイテムを捨てる",
        "key.use" => "アイテムの使用／ブロックの設置",
        "key.attack" => "攻撃する／壊す",
        "key.pickItem" => "ブロック選択",
        "key.chat" => "チャットを開く",
        "key.playerlist" => "プレイヤーリストの表示",
        "key.command" => "コマンドラインを開く",
        "key.socialInteractions" => "社交設定画面",
        "key.friends" => "フレンド画面",
        "key.screenshot" => "スクリーンショットの撮影",
        "key.togglePerspective" => "視点の切り替え",
        "key.smoothCamera" => "滑らかなカメラ動作の切り替え",
        "key.fullscreen" => "フルスクリーンの切り替え",
        "key.toggleGui" => "GUIの切り替え",
        "key.spectatorOutlines" => "プレイヤーの強調表示",
        "key.spectatorHotbar" => "ホットバーから選択",
        "key.toggleSpectatorShaderEffects" => "スペクテイターでのシェーダーを切り替え",
        "key.advancements" => "進捗",
        "key.quickActions" => "クイックアクション",
        "key.saveToolbarActivator" => "ホットバーの保存",
        "key.loadToolbarActivator" => "ホットバーの読み込み",
        "key.debug.modifier" => "デバッグ修飾キー",
        "key.debug.overlay" => "オーバーレイの切り替え",
        _ => match id.strip_prefix("key.hotbar.") {
            Some(n @ ("1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")) => {
                return format!("ホットバースロット{n}");
            }
            _ => id,
        },
    };
    label.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(modifier: &str, copy: &str, crash: &str, shared: &[&str]) -> DebugKeys {
        DebugKeys {
            modifier: format!("key.keyboard.{modifier}"),
            copy_location: format!("key.keyboard.{copy}"),
            crash: format!("key.keyboard.{crash}"),
            shared_with_copy: shared.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    #[test]
    fn defaults_are_f3_and_c() {
        assert_eq!(DebugKeys::default(), keys("f3", "c", "c", &[]));
        assert_eq!(parse_debug_keys(""), DebugKeys::default());
        assert_eq!(
            parse_debug_keys("version:3953\nlang:ja_jp\nresourcePacks:[\"vanilla\"]\n"),
            DebugKeys::default()
        );
    }

    #[test]
    fn default_bindings_of_recent_versions() {
        // 利用者の 26.3 の options.txt の形：デバッグのキーは既定のまま、「捨てる」を C に移し、
        // ツールバーの切り替えをそこから外した。
        let text = "version:5023\n\
                    key_key.attack:key.mouse.left\n\
                    key_key.drop:key.keyboard.c\n\
                    key_key.saveToolbarActivator:key.keyboard.9\n\
                    key_key.hotbar.1:key.keyboard.1\n\
                    key_key.debug.overlay:key.keyboard.f3\n\
                    key_key.debug.modifier:key.keyboard.f3\n\
                    key_key.debug.crash:key.keyboard.c\n\
                    key_key.debug.copyLocation:key.keyboard.c\n\
                    key_key.debug.copyRecreateCommand:key.keyboard.i\n\
                    soundCategory_master:1.0\n";
        assert_eq!(parse_debug_keys(text), keys("f3", "c", "c", &["key.drop"]));
    }

    #[test]
    fn old_versions_without_debug_lines_compare_against_c() {
        // 1.16.1 は F3 と C に決め打ちで、デバッグの行を書かない。
        let text = "key_key.drop:key.keyboard.c\nkey_key.inventory:key.keyboard.e\n\
                    key_key.saveToolbarActivator:key.keyboard.c\n";
        // C のホットバーの保存は押しているとしてしか読まれないので、断られた F3+C では動かない。
        let parsed = parse_debug_keys(text);
        assert_eq!(parsed, keys("f3", "c", "c", &["key.drop"]));
        assert!(parsed.copy_drops_items());
        let held_only = parse_debug_keys(
            "key_key.saveToolbarActivator:key.keyboard.c\nkey_key.jump:key.keyboard.c\n",
        );
        assert!(held_only.shared_with_copy.is_empty());
        assert!(!held_only.copy_drops_items());
    }

    #[test]
    fn forge_modifiers_are_split_off() {
        // Forge は割り当てに要る修飾キーをキーの後に書く。NONE はなし。
        let text = "key_key.debug.copyLocation:key.keyboard.c:NONE\n\
                    key_key.drop:key.keyboard.c:CONTROL\n\
                    key_key.inventory:key.keyboard.c\n";
        let parsed = parse_debug_keys(text);
        assert_eq!(parsed.copy_location, "key.keyboard.c");
        // 捨てるには Ctrl も要るので、F3+C では動かない。
        assert_eq!(parsed.shared_with_copy, ["key.inventory"]);
        // メニューには両方を出す（修飾キー以外のデバッグの割り当ては除く）。
        assert_eq!(
            bindings_by_key(text, Naming::Modern),
            [(
                InputId::Key(6),
                vec!["key.drop".to_owned(), "key.inventory".to_owned()]
            )]
        );
    }

    #[test]
    fn key_labels_of_any_key() {
        assert_eq!(key_label("key.keyboard.f3"), "F3");
        assert_eq!(key_label("key.keyboard.f25"), "F25");
        assert_eq!(
            key_label("key.keyboard.left.shift"),
            input_label(InputId::Key(225))
        );
        assert_eq!(key_label("key.mouse.4"), input_label(InputId::Mouse(4)));
        assert_eq!(key_label("something.else"), "something.else");
    }

    #[test]
    fn rebound_debug_keys() {
        let text = "key_key.debug.modifier:key.keyboard.f4\n\
                    key_key.debug.copyLocation:key.keyboard.x\n\
                    key_key.debug.crash:key.keyboard.keypad.5\n\
                    key_key.drop:key.keyboard.c\n\
                    key_key.inventory:key.keyboard.x\n\
                    key_key.debug.overlay:key.keyboard.x\n\
                    key_modid.custom_action:key.keyboard.x\n";
        let parsed = parse_debug_keys(text);
        assert_eq!(parsed.modifier, "key.keyboard.f4");
        assert_eq!(parsed.copy_location, "key.keyboard.x");
        assert_eq!(parsed.crash, "key.keyboard.keypad.5");
        // デバッグの割り当ては除く。Mod のものは id で出す。
        assert_eq!(
            parsed.shared_with_copy,
            ["key.inventory", "modid.custom_action"]
        );
    }

    #[test]
    fn unbound_keys() {
        let text = "key_key.debug.copyLocation:key.keyboard.unknown\n\
                    key_key.debug.crash:key.keyboard.unknown\n\
                    key_key.smoothCamera:key.keyboard.unknown\n\
                    key_key.spectatorOutlines:key.keyboard.unknown\n";
        let parsed = parse_debug_keys(text);
        assert_eq!(parsed.modifier, "key.keyboard.f3");
        assert_eq!(parsed.copy_location, UNBOUND);
        assert_eq!(parsed.crash, UNBOUND);
        // 割り当てのないほかの割り当ては、ないキーを「共有」しない。
        assert!(parsed.shared_with_copy.is_empty());
    }

    #[test]
    fn crlf_bom_and_whitespace() {
        let text = "\u{feff}key_key.debug.modifier:key.keyboard.f6\r\n\
                    \r\n\
                    \x20 key_key.drop : key.keyboard.v \r\n\
                    \tkey_key.debug.copyLocation:key.keyboard.v\r\n";
        assert_eq!(parse_debug_keys(text), keys("f6", "v", "c", &["key.drop"]));
        // 最初のキーの行の直前の BOM。
        assert_eq!(
            parse_debug_keys("\u{feff}key_key.debug.copyLocation:key.keyboard.b").copy_location,
            "key.keyboard.b"
        );
    }

    #[test]
    fn repeated_and_malformed_lines() {
        let text = "key_key.debug.copyLocation:key.keyboard.x\n\
                    key_key.drop:key.keyboard.c\n\
                    key_key.drop:key.keyboard.q\n\
                    key_key.debug.copyLocation:key.keyboard.q\n\
                    key_key.debug.modifier:\n\
                    key_:key.keyboard.q\n\
                    key_key.chat\n\
                    keykey.jump:key.keyboard.q\n";
        let parsed = parse_debug_keys(text);
        // 最後の行が勝ち、割り当ては最初の位置を保つ。
        assert_eq!(parsed.copy_location, "key.keyboard.q");
        assert_eq!(parsed.shared_with_copy, ["key.drop"]);
        // 空の値はキーとせず、無視する。
        assert_eq!(parsed.modifier, "key.keyboard.f3");
    }

    #[test]
    fn options_file() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(options_path(dir.path()), dir.path().join("options.txt"));
        assert_eq!(load_debug_keys(dir.path()).unwrap(), DebugKeys::default());
        fs::write(
            options_path(dir.path()),
            b"key_key.debug.modifier:key.keyboard.f7\nlang:\xff\xfe\nkey_key.drop:key.keyboard.c\n",
        )
        .unwrap();
        assert_eq!(
            load_debug_keys(dir.path()).unwrap(),
            keys("f7", "c", "c", &["key.drop"])
        );
    }

    #[test]
    fn glfw_key_codes() {
        let cases = [
            ("a", 65),
            ("c", 67),
            // ファンクションキーではなく、文字。
            ("f", 70),
            ("z", 90),
            ("0", 48),
            ("9", 57),
            ("f1", 290),
            ("f3", 292),
            ("f12", 301),
            ("f25", 314),
            ("keypad.0", 320),
            ("keypad.9", 329),
        ];
        for (name, code) in cases {
            assert_eq!(
                glfw_key(&format!("key.keyboard.{name}")),
                Some(code),
                "{name}"
            );
        }
    }

    #[test]
    fn sdl_scancodes() {
        let cases = [
            ("a", 4),
            ("c", 6),
            ("z", 29),
            ("1", 30),
            ("9", 38),
            ("0", 39),
            ("f1", 58),
            ("f3", 60),
            ("f12", 69),
            ("f13", 104),
            ("f24", 115),
            ("keypad.1", 89),
            ("keypad.9", 97),
            ("keypad.0", 98),
        ];
        for (name, code) in cases {
            assert_eq!(
                sdl_scancode(&format!("key.keyboard.{name}")),
                Some(code),
                "{name}"
            );
        }
        assert_eq!(sdl_scancode("key.keyboard.f25"), None);
    }

    #[test]
    fn sdl_keycodes() {
        let cases = [
            ("a", 0x61),
            ("c", 0x63),
            ("z", 0x7a),
            ("0", 0x30),
            ("1", 0x31),
            ("9", 0x39),
            ("f1", 0x4000_003a),
            ("f3", 0x4000_003c),
            ("f12", 0x4000_0045),
            ("f13", 0x4000_0068),
            ("f24", 0x4000_0073),
            ("keypad.1", 0x4000_0059),
            ("keypad.9", 0x4000_0061),
            ("keypad.0", 0x4000_0062),
        ];
        for (name, code) in cases {
            assert_eq!(
                sdl_keycode(&format!("key.keyboard.{name}")),
                Some(code),
                "{name}"
            );
        }
        assert_eq!(sdl_keycode("key.keyboard.f25"), None);
    }

    #[test]
    fn unsupported_key_names() {
        for name in [
            UNBOUND,
            "",
            "c",
            "f3",
            "key.keyboard.",
            "key.keyboard.C",
            "key.keyboard.F3",
            "key.keyboard.ab",
            "key.keyboard.f0",
            "key.keyboard.f01",
            "key.keyboard.f26",
            "key.keyboard.f100",
            "key.keyboard.f-1",
            "key.keyboard.keypad.",
            "key.keyboard.keypad.10",
            "key.keyboard.keypad.enter",
            "key.keyboard.left.shift",
            "key.keyboard.space",
            "key.keyboard.67",
            "key.mouse.left",
            "key.mouse.4",
            " key.keyboard.c",
        ] {
            assert_eq!(glfw_key(name), None, "{name}");
            assert_eq!(sdl_scancode(name), None, "{name}");
            assert_eq!(sdl_keycode(name), None, "{name}");
        }
    }

    #[test]
    fn key_labels() {
        assert_eq!(key_label("key.keyboard.f3"), "F3");
        assert_eq!(key_label("key.keyboard.f25"), "F25");
        assert_eq!(key_label("key.keyboard.c"), "C");
        assert_eq!(key_label("key.keyboard.z"), "Z");
        assert_eq!(key_label("key.keyboard.f"), "F");
        assert_eq!(key_label("key.keyboard.0"), "0");
        assert_eq!(key_label("key.keyboard.keypad.1"), "テンキー 1");
        // F3+C が使えないキーにもラベルがある。
        assert_eq!(key_label("key.mouse.left"), input_label(InputId::Mouse(1)));
        assert_eq!(key_label(UNBOUND), UNBOUND);
    }

    #[test]
    fn mapping_labels() {
        assert_eq!(mapping_label("key.drop"), "アイテムを捨てる");
        // ゲーム自身の ja_jp のキーの設定が出す名前。
        assert_eq!(mapping_label("key.attack"), "攻撃する／壊す");
        assert_eq!(
            mapping_label("key.loadToolbarActivator"),
            "ホットバーの読み込み"
        );
        assert_eq!(mapping_label("key.hotbar.1"), "ホットバースロット1");
        assert_eq!(mapping_label("key.hotbar.9"), "ホットバースロット9");
        assert_eq!(mapping_label("key.hotbar.0"), "key.hotbar.0");
        assert_eq!(mapping_label("key.hotbar.10"), "key.hotbar.10");
        assert_eq!(mapping_label("modid.custom_action"), "modid.custom_action");
    }

    #[test]
    fn mapping_labels_of_recent_mappings() {
        // 1.21.11 と 26.3 の割り当て（ゲームの ja_jp の名前で）。
        let cases = [
            ("key.debug.modifier", "デバッグ修飾キー"),
            ("key.debug.overlay", "オーバーレイの切り替え"),
            ("key.toggleGui", "GUIの切り替え"),
            ("key.spectatorHotbar", "ホットバーから選択"),
            ("key.quickActions", "クイックアクション"),
            ("key.socialInteractions", "社交設定画面"),
            (
                "key.toggleSpectatorShaderEffects",
                "スペクテイターでのシェーダーを切り替え",
            ),
            ("key.friends", "フレンド画面"),
        ];
        for (id, label) in cases {
            assert_eq!(mapping_label(id), label, "{id}");
        }
        // ほかのデバッグの割り当ては出さず、id のまま。
        assert_eq!(mapping_label("key.debug.crash"), "key.debug.crash");
    }

    #[test]
    fn f3c_lookups_keep_to_their_keys() {
        // 文字、数字、F キー、テンキーの数字以外の名前のあるキーは、F3+C では使えないまま。キーを
        // 表す数も同じ。
        for name in [
            "key.keyboard.left.shift",
            "key.keyboard.keypad.enter",
            "key.keyboard.international1",
            "key.keyboard.67",
            "scancode.115",
        ] {
            assert_eq!(glfw_key(name), None, "{name}");
            assert_eq!(sdl_scancode(name), None, "{name}");
            assert_eq!(sdl_keycode(name), None, "{name}");
        }
        // それらが知っているキーは表のもの。
        for (key, name, sc, _) in GLFW_KEYS {
            let full = format!("{KEYBOARD_PREFIX}{name}");
            let Some(code) = glfw_key(&full) else {
                continue;
            };
            assert_eq!(code, key);
            if sc != 0 {
                assert_eq!(sdl_scancode(&full), Some(u32::from(sc)), "{full}");
                assert_eq!(
                    sdl_keycode(&full),
                    Some(sdl_keycode_of(InputId::Key(sc))),
                    "{full}"
                );
                assert_eq!(key_label(&full), input_label(InputId::Key(sc)), "{full}");
            }
        }
        let count = |f: fn(&str) -> bool| {
            GLFW_KEYS
                .iter()
                .filter(|(_, name, _, _)| f(&format!("{KEYBOARD_PREFIX}{name}")))
                .count()
        };
        // 26 の文字、10 の数字、F1..F25、10 のテンキーの数字。SDL3 に F25 はない。
        assert_eq!(count(|name| glfw_key(name).is_some()), 71);
        assert_eq!(count(|name| sdl_scancode(name).is_some()), 70);
    }

    /// 26.3 の表（`InputConstants.Type.KEYBOARD`）の目立つ行。
    const MODERN_ROWS: [(&str, u16); 31] = [
        ("a", 4),
        ("z", 29),
        ("1", 30),
        ("0", 39),
        ("enter", 40),
        ("escape", 41),
        ("space", 44),
        ("world.2", 50),
        ("grave.accent", 53),
        ("caps.lock", 57),
        ("f1", 58),
        ("print.screen", 70),
        ("delete", 76),
        ("num.lock", 83),
        ("keypad.subtract", 86),
        ("keypad.enter", 88),
        ("keypad.period", 99),
        ("world.1", 100),
        ("application", 101),
        ("f13", 104),
        ("f24", 115),
        ("menu", 118),
        ("international1", 135),
        ("lang1", 144),
        ("enter2", 158),
        ("keypad.decimal", 220),
        ("left.control", 224),
        ("left.win", 227),
        ("right.win", 231),
        ("channel.up", 260),
        ("end.call", 290),
    ];

    #[test]
    fn modern_names() {
        assert_eq!(SDL_KEYS.len(), 246);
        for (name, sc) in MODERN_ROWS {
            let full = format!("{KEYBOARD_PREFIX}{name}");
            assert_eq!(
                input_by_name(&full, Naming::Modern),
                Some(InputId::Key(sc)),
                "{full}"
            );
        }
        // どの行も名前から読み戻せ、その名前を返す。
        for (sc, name, _) in SDL_KEYS {
            let full = format!("{KEYBOARD_PREFIX}{name}");
            assert_eq!(
                input_by_name(&full, Naming::Modern),
                Some(InputId::Key(sc)),
                "{full}"
            );
            assert_eq!(input_name(InputId::Key(sc)), full);
        }
        // ゲームと同じく、数はキーを表す：名前のないものも、名前のあるものも。
        assert_eq!(input_name(InputId::Key(130)), "key.keyboard.130");
        assert_eq!(
            input_by_name("key.keyboard.130", Naming::Modern),
            Some(InputId::Key(130))
        );
        assert_eq!(
            input_by_name("key.keyboard.60", Naming::Modern),
            Some(InputId::Key(60))
        );
        assert_eq!(
            input_by_name("key.keyboard.511", Naming::Modern),
            Some(InputId::Key(511))
        );
        // 数字のキーの名前は名前で、数ではない。
        assert_eq!(
            input_by_name("key.keyboard.4", Naming::Modern),
            Some(InputId::Key(33))
        );
    }

    #[test]
    fn every_id_in_range_round_trips() {
        for sc in FIRST_SCANCODE..SCANCODE_COUNT as u16 {
            let id = InputId::Key(sc);
            assert_eq!(input_by_name(&input_name(id), Naming::Modern), Some(id));
        }
        for button in 1..=MOUSE_BUTTONS {
            let id = InputId::Mouse(button);
            assert_eq!(input_by_name(&input_name(id), Naming::Modern), Some(id));
        }
    }

    #[test]
    fn ids_out_of_range() {
        for name in [
            UNBOUND,
            "key.keyboard.0000",
            "key.keyboard.01",
            "key.keyboard.3x",
            "key.keyboard.512",
            "key.keyboard.65535",
            "key.keyboard.65540",
            "key.keyboard.99999999999",
            "key.keyboard.-1",
            "key.keyboard.+60",
            "key.keyboard. 60",
            "key.keyboard.",
            "key.keyboard.F3",
            "key.keyboard.f25",
            "key.mouse.0",
            "key.mouse.6",
            "key.mouse.8",
            "key.mouse.256",
            "key.mouse.-1",
            "key.mouse.",
            "key.mouse.Left",
            "scancode.115",
            "key.keyboard",
            "",
            " key.keyboard.c",
            "key.keyboard.c ",
        ] {
            assert_eq!(input_by_name(name, Naming::Modern), None, "{name}");
        }
        for name in [
            UNBOUND,
            "key.keyboard.f25",
            "key.keyboard.31",
            "key.keyboard.349",
            "key.keyboard.-1",
            "key.keyboard.4294967295",
            "key.keyboard.international1",
            "key.mouse.0",
            "key.mouse.6",
            "key.mouse.4294967296",
            "scancode.",
            "scancode.0",
            "scancode.30",
            "scancode.-115",
            "scancode.0x73",
            "scancode.99999999999",
        ] {
            assert_eq!(input_by_name(name, Naming::Glfw), None, "{name}");
        }
        // 範囲外の id でもパニックせず、読み戻せない名前を得る。
        for id in [
            InputId::Key(0),
            InputId::Key(1),
            InputId::Key(3),
            InputId::Key(512),
            InputId::Key(u16::MAX),
            InputId::Mouse(0),
            InputId::Mouse(6),
            InputId::Mouse(u8::MAX),
        ] {
            assert_eq!(input_by_name(&input_name(id), Naming::Modern), None);
            assert!(!input_label(id).is_empty());
            assert_eq!(glfw_key_of(id), None);
            assert_eq!(glfw_button_of(id), None);
            assert_eq!(sdl_keycode_of(id), 0);
            assert_eq!(win_scancode_of(id), None);
            assert_eq!(modifier_kind(id), None);
        }
        assert_eq!(input_name(InputId::Key(0)), UNBOUND);
        for (key, scancode) in [
            (i32::MIN, 0),
            (i32::MAX, 0),
            (-2, 0x73),
            (0, 0),
            (31, 0),
            (349, 0),
            (-1, 0),
            (-1, -1),
            (-1, i32::MIN),
            (-1, i32::MAX),
            (-1, 0x173),
        ] {
            assert_eq!(input_from_glfw_key(key, scancode), None, "{key} {scancode}");
        }
    }

    #[test]
    fn glfw_names_and_codes() {
        assert_eq!(GLFW_KEYS.len(), 120);
        // (1.21 の名前, GLFW のキー, 26.x の名前)
        let renamed = [
            ("keypad.decimal", 330, "keypad.period"),
            ("menu", 348, "application"),
            ("world.1", 161, "world.1"),
            ("world.2", 162, "world.1"),
        ];
        for (key, name, sc, win) in GLFW_KEYS {
            let full = format!("{KEYBOARD_PREFIX}{name}");
            let id = (sc != 0).then_some(InputId::Key(sc));
            assert_eq!(input_by_name(&full, Naming::Glfw), id, "{full}");
            assert_eq!(input_from_glfw_key(key, i32::from(win)), id, "{full}");
            // 名前のあるキーはキーコードで見分ける。スキャンコードは関係ない。
            assert_eq!(input_from_glfw_key(key, 0), id, "{full}");
            assert_eq!(
                input_by_name(&format!("{KEYBOARD_PREFIX}{key}"), Naming::Glfw),
                id,
                "{full}"
            );
            // GLFW の F25 に SDL3 のキーはない。
            let Some(id) = id else {
                assert_eq!(name, "f25");
                continue;
            };
            // 名前を変えたキーを除き、26.x でも同じ名前。
            let modern = match renamed.iter().find(|(n, _, _)| *n == name) {
                Some((_, renamed_key, modern)) => {
                    assert_eq!(*renamed_key, key);
                    format!("{KEYBOARD_PREFIX}{modern}")
                }
                None => full.clone(),
            };
            assert_eq!(input_name(id), modern, "{full}");
            if name == "world.1" {
                // Windows のキーではない。ISO 配列のキーは world.2。
                assert_eq!(glfw_key_of(id), Some((162, 0x56)));
                continue;
            }
            assert_eq!(glfw_key_of(id), Some((key, i32::from(win))), "{full}");
            assert_eq!(win_scancode_of(id), Some(win), "{full}");
            // キーを送ると同じキーが返る。
            assert_eq!(input_from_glfw_key(key, i32::from(win)), Some(id));
        }
        // GLFW のキーコードと GLFW 3.4 が報告する Win32 のスキャンコードの、いくつかの行。
        let cases = [
            ("space", 32, 44, 0x39),
            ("0", 48, 39, 0x0b),
            ("c", 67, 6, 0x2e),
            ("f3", 292, 60, 0x3d),
            ("f12", 301, 69, 0x58),
            ("f24", 313, 115, 0x76),
            ("up", 265, 82, 0x148),
            ("pause", 284, 72, 0x45),
            ("num.lock", 282, 83, 0x145),
            ("keypad.enter", 335, 88, 0x11c),
            ("keypad.decimal", 330, 99, 0x53),
            ("left.shift", 340, 225, 0x2a),
            ("right.control", 345, 228, 0x11d),
            ("right.win", 347, 231, 0x15c),
            ("menu", 348, 101, 0x15d),
            ("world.2", 162, 100, 0x56),
        ];
        for (name, key, sc, win) in cases {
            let full = format!("{KEYBOARD_PREFIX}{name}");
            assert_eq!(
                input_by_name(&full, Naming::Glfw),
                Some(InputId::Key(sc)),
                "{full}"
            );
            assert_eq!(glfw_key_of(InputId::Key(sc)), Some((key, win)), "{full}");
        }
        assert_eq!(input_from_glfw_key(314, 0), None);
        assert_eq!(input_from_glfw_key(161, -1), Some(InputId::Key(100)));
        // SDL3 だけにあるキー。
        for sc in [50, 118, 140, 144, 220, 260] {
            assert_eq!(glfw_key_of(InputId::Key(sc)), None, "{sc}");
        }
    }

    #[test]
    fn names_that_differ_between_versions() {
        // (名前, 1.21 のキー, 26.x のキー)
        let cases = [
            ("key.keyboard.keypad.decimal", 99, 220),
            ("key.keyboard.menu", 101, 118),
            ("key.keyboard.world.2", 100, 50),
        ];
        for (name, glfw, modern) in cases {
            assert_eq!(input_by_name(name, Naming::Glfw), Some(InputId::Key(glfw)));
            assert_eq!(
                input_by_name(name, Naming::Modern),
                Some(InputId::Key(modern))
            );
        }
        // 1.21 のキーの 26.x の名前は、1.21 の名前ではない。
        assert_eq!(
            input_by_name("key.keyboard.keypad.period", Naming::Glfw),
            None
        );
        assert_eq!(
            input_by_name("key.keyboard.application", Naming::Glfw),
            None
        );
        // 数は、1.21 では GLFW のキーコード、26.x ではスキャンコード。
        assert_eq!(
            input_by_name("key.keyboard.67", Naming::Glfw),
            Some(InputId::Key(6))
        );
        assert_eq!(
            input_by_name("key.keyboard.67", Naming::Modern),
            Some(InputId::Key(67))
        );
    }

    #[test]
    fn mouse_buttons() {
        // 26.x がそのまま使う SDL3 の番号。
        let modern = [
            ("key.mouse.left", 1),
            ("key.mouse.middle", 2),
            ("key.mouse.right", 3),
            ("key.mouse.4", 4),
            ("key.mouse.5", 5),
            ("key.mouse.1", 1),
            ("key.mouse.2", 2),
            ("key.mouse.3", 3),
        ];
        for (name, button) in modern {
            assert_eq!(
                input_by_name(name, Naming::Modern),
                Some(InputId::Mouse(button)),
                "{name}"
            );
        }
        // 1.21 はボタンを GLFW の番号 + 1 で名付け、GLFW は中より右が先。
        let glfw = [
            ("key.mouse.left", 1),
            ("key.mouse.right", 3),
            ("key.mouse.middle", 2),
            ("key.mouse.4", 4),
            ("key.mouse.5", 5),
            ("key.mouse.1", 1),
            ("key.mouse.2", 3),
            ("key.mouse.3", 2),
        ];
        for (name, button) in glfw {
            assert_eq!(
                input_by_name(name, Naming::Glfw),
                Some(InputId::Mouse(button)),
                "{name}"
            );
        }
        let buttons = [(0, 1), (1, 3), (2, 2), (3, 4), (4, 5)];
        for (glfw, sdl) in buttons {
            let id = InputId::Mouse(sdl);
            assert_eq!(input_from_glfw_button(glfw), Some(id));
            assert_eq!(glfw_button_of(id), Some(glfw));
        }
        // Windows ではどちらのライブラリも 6..8 のボタンを報告しない。
        for button in [-1, 5, 6, 7, i32::MAX, i32::MIN] {
            assert_eq!(input_from_glfw_button(button), None, "{button}");
        }
        assert_eq!(input_name(InputId::Mouse(1)), "key.mouse.left");
        assert_eq!(input_name(InputId::Mouse(2)), "key.mouse.middle");
        assert_eq!(input_name(InputId::Mouse(3)), "key.mouse.right");
        assert_eq!(input_name(InputId::Mouse(4)), "key.mouse.4");
        assert_eq!(input_name(InputId::Mouse(5)), "key.mouse.5");
        // マウスのボタンはキーではない。
        for button in 1..=MOUSE_BUTTONS {
            let id = InputId::Mouse(button);
            assert_eq!(glfw_key_of(id), None);
            assert_eq!(sdl_keycode_of(id), 0);
            assert_eq!(win_scancode_of(id), None);
            assert_eq!(modifier_kind(id), None);
        }
        assert_eq!(glfw_button_of(InputId::Key(4)), None);
    }

    #[test]
    fn japanese_keys() {
        // (Win32 のスキャンコード, SDL3 のスキャンコード, ラベル)
        let cases = [
            (0x70, 136, "カタカナ/ひらがな"),
            (0x73, 135, "ろ"),
            (0x79, 138, "変換"),
            (0x7b, 139, "無変換"),
            (0x7d, 137, "¥"),
        ];
        for (win, sc, label) in cases {
            let id = InputId::Key(sc);
            // GLFW はこれらを key -1 として送る。1.21 は 10 進のスキャンコードで名付ける。
            assert_eq!(input_from_glfw_key(-1, win), Some(id));
            let old_name = format!("scancode.{win}");
            assert_eq!(input_by_name(&old_name, Naming::Glfw), Some(id));
            assert_eq!(input_by_name(&old_name, Naming::Modern), None);
            let name = input_name(id);
            assert_eq!(name, format!("key.keyboard.international{}", sc - 134));
            assert_eq!(input_by_name(&name, Naming::Modern), Some(id));
            assert_eq!(input_by_name(&name, Naming::Glfw), None);
            assert_eq!(glfw_key_of(id), Some((-1, win)));
            assert_eq!(win_scancode_of(id).map(i32::from), Some(win));
            assert_eq!(input_label(id), label);
        }
        assert_eq!(
            input_by_name("scancode.115", Naming::Glfw),
            Some(InputId::Key(135))
        );
        // GLFW が -1 として送るほかのキーは知らない。
        assert_eq!(input_from_glfw_key(-1, 0x71), None);
        assert_eq!(input_from_glfw_key(-1, 0x2e), None);
    }

    #[test]
    fn labels() {
        let cases = [
            (4, "A"),
            (29, "Z"),
            (30, "1"),
            (38, "9"),
            (39, "0"),
            (58, "F1"),
            (69, "F12"),
            (104, "F13"),
            (115, "F24"),
            (224, "左 Ctrl"),
            (228, "右 Ctrl"),
            (225, "左 Shift"),
            (229, "右 Shift"),
            (226, "左 Alt"),
            (230, "右 Alt"),
            (227, "左 Windows"),
            (231, "右 Windows"),
            (57, "CapsLock"),
            (44, "スペース"),
            (40, "Enter"),
            (43, "Tab"),
            (42, "Backspace"),
            (41, "Esc"),
            (73, "Insert"),
            (76, "Delete"),
            (74, "Home"),
            (77, "End"),
            (75, "PageUp"),
            (78, "PageDown"),
            (82, "↑"),
            (81, "↓"),
            (80, "←"),
            (79, "→"),
            (53, "`"),
            (45, "-"),
            (46, "="),
            (47, "["),
            (48, "]"),
            (49, "\\"),
            (51, ";"),
            (52, "'"),
            (54, ","),
            (55, "."),
            (56, "/"),
            (98, "テンキー 0"),
            (89, "テンキー 1"),
            (97, "テンキー 9"),
            (88, "テンキーの Enter"),
            (87, "テンキーの +"),
            (86, "テンキーの -"),
            (85, "テンキーの *"),
            (84, "テンキーの /"),
            (99, "テンキーの ."),
            (70, "PrintScreen"),
            (83, "NumLock"),
            (101, "アプリケーション"),
            (135, "ろ"),
            (137, "¥"),
            (140, "キー 140"),
            (130, "キー 130"),
        ];
        for (sc, label) in cases {
            assert_eq!(input_label(InputId::Key(sc)), label, "{sc}");
        }
        // メニューのホットキーと同じ言葉。
        assert_eq!(input_label(InputId::Mouse(1)), "左クリック");
        assert_eq!(input_label(InputId::Mouse(3)), "右クリック");
        assert_eq!(input_label(InputId::Mouse(2)), "ホイールクリック");
        assert_eq!(input_label(InputId::Mouse(4)), "マウスのボタン4");
        assert_eq!(input_label(InputId::Mouse(5)), "マウスのボタン5");
        // 範囲内のどのキーにも自分のラベルがある。
        let mut seen = std::collections::HashSet::new();
        for sc in FIRST_SCANCODE..SCANCODE_COUNT as u16 {
            assert!(seen.insert(input_label(InputId::Key(sc))), "{sc}");
        }
    }

    #[test]
    fn keycodes() {
        let cases = [
            (4, 0x61),
            (39, 0x30),
            (40, 0x0d),
            (41, 0x1b),
            (42, 0x08),
            (43, 0x09),
            (44, 0x20),
            (45, 0x2d),
            (53, 0x60),
            (56, 0x2f),
            (57, 0x4000_0039),
            (60, 0x4000_003c),
            (76, 0x7f),
            (100, 0x4000_0064),
            (135, 0x4000_0087),
            (224, 0x4000_00e0),
            (290, 0x4000_0122),
            // 名前なし：SDL3 が作るのと同じく、スキャンコードにマスクを立てたもの。
            (130, 0x4000_0082),
            (511, 0x4000_01ff),
        ];
        for (sc, keycode) in cases {
            assert_eq!(sdl_keycode_of(InputId::Key(sc)), keycode, "{sc}");
        }
    }

    #[test]
    fn modifier_keys() {
        let cases = [
            (224, Modifier::Ctrl, Side::Left),
            (225, Modifier::Shift, Side::Left),
            (226, Modifier::Alt, Side::Left),
            (227, Modifier::Super, Side::Left),
            (228, Modifier::Ctrl, Side::Right),
            (229, Modifier::Shift, Side::Right),
            (230, Modifier::Alt, Side::Right),
            (231, Modifier::Super, Side::Right),
        ];
        for (sc, modifier, side) in cases {
            assert_eq!(modifier_kind(InputId::Key(sc)), Some((modifier, side)));
        }
        for sc in [4, 57, 83, 223, 232] {
            assert_eq!(modifier_kind(InputId::Key(sc)), None, "{sc}");
        }
    }

    #[test]
    fn win32_scancodes() {
        let cases = [
            (60, 0x3d),
            (228, 0x11d),
            (82, 0x148),
            (88, 0x11c),
            (72, 0x45),
            (83, 0x145),
            (100, 0x56),
            (135, 0x73),
        ];
        for (sc, win) in cases {
            assert_eq!(win_scancode_of(InputId::Key(sc)), Some(win), "{sc}");
        }
        // GLFW が知らないキー。
        for sc in [50, 118, 140, 144, 220] {
            assert_eq!(win_scancode_of(InputId::Key(sc)), None, "{sc}");
        }
    }

    /// 利用者の 26.3 の options.txt のキーの行。
    const OPTIONS_26_3: &str = "version:5023\n\
        key_key.attack:key.mouse.left\n\
        key_key.use:key.mouse.right\n\
        key_key.forward:key.keyboard.w\n\
        key_key.left:key.keyboard.a\n\
        key_key.back:key.keyboard.s\n\
        key_key.right:key.keyboard.d\n\
        key_key.jump:key.keyboard.space\n\
        key_key.sneak:key.keyboard.left.shift\n\
        key_key.sprint:key.keyboard.m\n\
        key_key.drop:key.keyboard.c\n\
        key_key.inventory:key.keyboard.e\n\
        key_key.chat:key.keyboard.backspace\n\
        key_key.playerlist:key.keyboard.f5\n\
        key_key.pickItem:key.keyboard.left.alt\n\
        key_key.command:key.keyboard.slash\n\
        key_key.friends:key.keyboard.o\n\
        key_key.socialInteractions:key.keyboard.p\n\
        key_key.toggleGui:key.keyboard.f1\n\
        key_key.toggleSpectatorShaderEffects:key.keyboard.f4\n\
        key_key.screenshot:key.keyboard.f2\n\
        key_key.togglePerspective:key.keyboard.tab\n\
        key_key.smoothCamera:key.keyboard.0\n\
        key_key.fullscreen:key.keyboard.f11\n\
        key_key.spectatorOutlines:key.keyboard.unknown\n\
        key_key.spectatorHotbar:key.mouse.middle\n\
        key_key.swapOffhand:key.keyboard.caps.lock\n\
        key_key.saveToolbarActivator:key.keyboard.9\n\
        key_key.loadToolbarActivator:key.keyboard.z\n\
        key_key.advancements:key.keyboard.semicolon\n\
        key_key.quickActions:key.keyboard.8\n\
        key_key.debug.overlay:key.keyboard.f3\n\
        key_key.debug.modifier:key.keyboard.f3\n\
        key_key.hotbar.1:key.keyboard.q\n\
        key_key.hotbar.2:key.keyboard.1\n\
        key_key.hotbar.3:key.keyboard.2\n\
        key_key.hotbar.4:key.keyboard.3\n\
        key_key.hotbar.5:key.keyboard.v\n\
        key_key.hotbar.6:key.keyboard.t\n\
        key_key.hotbar.7:key.keyboard.g\n\
        key_key.hotbar.8:key.keyboard.r\n\
        key_key.hotbar.9:key.keyboard.f\n\
        key_key.debug.reloadChunk:key.keyboard.a\n\
        key_key.debug.showHitboxes:key.keyboard.b\n\
        key_key.debug.clearChat:key.keyboard.d\n\
        key_key.debug.crash:key.keyboard.c\n\
        key_key.debug.showChunkBorders:key.keyboard.g\n\
        key_key.debug.showAdvancedTooltips:key.keyboard.h\n\
        key_key.debug.copyRecreateCommand:key.keyboard.i\n\
        key_key.debug.spectate:key.keyboard.n\n\
        key_key.debug.switchGameMode:key.keyboard.f4\n\
        key_key.debug.debugOptions:key.keyboard.f6\n\
        key_key.debug.focusPause:key.keyboard.p\n\
        key_key.debug.dumpDynamicTextures:key.keyboard.s\n\
        key_key.debug.reloadResourcePacks:key.keyboard.t\n\
        key_key.debug.profiling:key.keyboard.l\n\
        key_key.debug.copyLocation:key.keyboard.c\n\
        key_key.debug.dumpVersion:key.keyboard.v\n\
        key_key.debug.profilingChart:key.keyboard.1\n\
        key_key.debug.fpsCharts:key.keyboard.2\n\
        key_key.debug.networkCharts:key.keyboard.3\n\
        key_key.debug.lightmapTexture:key.keyboard.4\n\
        key_key.debug.improvedTransparency:key.keyboard.x\n";

    /// 利用者の 1.21.11 の options.txt のキーの行。
    const OPTIONS_1_21_11: &str = "version:4671\n\
        key_key.attack:key.mouse.left\n\
        key_key.use:key.mouse.right\n\
        key_key.forward:key.keyboard.w\n\
        key_key.left:key.keyboard.a\n\
        key_key.back:key.keyboard.s\n\
        key_key.right:key.keyboard.d\n\
        key_key.jump:key.keyboard.space\n\
        key_key.sneak:key.keyboard.left.shift\n\
        key_key.sprint:key.keyboard.left.control\n\
        key_key.drop:key.keyboard.q\n\
        key_key.inventory:key.keyboard.e\n\
        key_key.chat:key.keyboard.t\n\
        key_key.playerlist:key.keyboard.tab\n\
        key_key.pickItem:key.mouse.middle\n\
        key_key.command:key.keyboard.slash\n\
        key_key.socialInteractions:key.keyboard.p\n\
        key_key.toggleGui:key.keyboard.f1\n\
        key_key.toggleSpectatorShaderEffects:key.keyboard.f4\n\
        key_key.screenshot:key.keyboard.f2\n\
        key_key.togglePerspective:key.keyboard.f5\n\
        key_key.smoothCamera:key.keyboard.unknown\n\
        key_key.fullscreen:key.keyboard.f11\n\
        key_key.spectatorOutlines:key.keyboard.unknown\n\
        key_key.spectatorHotbar:key.mouse.middle\n\
        key_key.swapOffhand:key.keyboard.f\n\
        key_key.saveToolbarActivator:key.keyboard.c\n\
        key_key.loadToolbarActivator:key.keyboard.x\n\
        key_key.advancements:key.keyboard.l\n\
        key_key.quickActions:key.keyboard.g\n\
        key_key.debug.overlay:key.keyboard.f3\n\
        key_key.debug.modifier:key.keyboard.f3\n\
        key_key.hotbar.1:key.keyboard.1\n\
        key_key.hotbar.2:key.keyboard.2\n\
        key_key.hotbar.3:key.keyboard.3\n\
        key_key.hotbar.4:key.keyboard.4\n\
        key_key.hotbar.5:key.keyboard.5\n\
        key_key.hotbar.6:key.keyboard.6\n\
        key_key.hotbar.7:key.keyboard.7\n\
        key_key.hotbar.8:key.keyboard.8\n\
        key_key.hotbar.9:key.keyboard.9\n\
        key_key.debug.reloadChunk:key.keyboard.a\n\
        key_key.debug.showHitboxes:key.keyboard.b\n\
        key_key.debug.clearChat:key.keyboard.d\n\
        key_key.debug.crash:key.keyboard.c\n\
        key_key.debug.showChunkBorders:key.keyboard.g\n\
        key_key.debug.showAdvancedTooltips:key.keyboard.h\n\
        key_key.debug.copyRecreateCommand:key.keyboard.i\n\
        key_key.debug.spectate:key.keyboard.n\n\
        key_key.debug.switchGameMode:key.keyboard.f4\n\
        key_key.debug.debugOptions:key.keyboard.f6\n\
        key_key.debug.focusPause:key.keyboard.p\n\
        key_key.debug.dumpDynamicTextures:key.keyboard.s\n\
        key_key.debug.reloadResourcePacks:key.keyboard.t\n\
        key_key.debug.profiling:key.keyboard.l\n\
        key_key.debug.copyLocation:key.keyboard.c\n\
        key_key.debug.dumpVersion:key.keyboard.v\n\
        key_key.debug.profilingChart:key.keyboard.1\n\
        key_key.debug.fpsCharts:key.keyboard.2\n\
        key_key.debug.networkCharts:key.keyboard.3\n";

    /// `id` に割り当てた割り当ての id。
    fn bound(bindings: &[(InputId, Vec<String>)], id: InputId) -> Vec<&str> {
        bindings
            .iter()
            .find(|(i, _)| *i == id)
            .map(|(_, ids)| ids.iter().map(String::as_str).collect())
            .unwrap_or_default()
    }

    #[test]
    fn bindings_of_the_users_26_3() {
        let bindings = bindings_by_key(OPTIONS_26_3, Naming::Modern);
        assert_eq!(bindings.len(), 39);
        assert_eq!(bindings[0], (InputId::Mouse(1), vec!["key.attack".into()]));
        let key = |name: &str| input_by_name(&format!("key.keyboard.{name}"), Naming::Modern);
        // F3：修飾キーとオーバーレイの切り替え（ファイルの順）。ほかのデバッグの割り当ては F3 を
        // 押しているときだけ動くので、C は「捨てる」だけ。
        assert_eq!(
            bound(&bindings, key("f3").unwrap()),
            ["key.debug.overlay", "key.debug.modifier"]
        );
        assert_eq!(bound(&bindings, key("c").unwrap()), ["key.drop"]);
        assert_eq!(
            bound(&bindings, key("f4").unwrap()),
            ["key.toggleSpectatorShaderEffects"]
        );
        assert_eq!(bound(&bindings, key("a").unwrap()), ["key.left"]);
        assert_eq!(bound(&bindings, key("q").unwrap()), ["key.hotbar.1"]);
        assert_eq!(
            bound(&bindings, key("caps.lock").unwrap()),
            ["key.swapOffhand"]
        );
        assert_eq!(bound(&bindings, key("left.alt").unwrap()), ["key.pickItem"]);
        assert_eq!(bound(&bindings, InputId::Mouse(2)), ["key.spectatorHotbar"]);
        // ほかのデバッグの割り当てにだけ割り当てたキーは出さない。
        assert!(bound(&bindings, key("b").unwrap()).is_empty());
        assert!(bound(&bindings, key("x").unwrap()).is_empty());
        for (_, ids) in &bindings {
            for id in ids {
                assert!(
                    !id.starts_with("key.debug.") || id == MODIFIER_ID || id == OVERLAY_ID,
                    "{id}"
                );
            }
        }
        // メニューが F3 について出すラベル。
        let labels: Vec<String> = bound(&bindings, key("f3").unwrap())
            .into_iter()
            .map(mapping_label)
            .collect();
        assert_eq!(
            labels.join("・"),
            "オーバーレイの切り替え・デバッグ修飾キー"
        );
    }

    #[test]
    fn bindings_of_the_users_1_21_11() {
        let bindings = bindings_by_key(OPTIONS_1_21_11, Naming::Glfw);
        assert_eq!(bindings.len(), 36);
        let key = |name: &str| input_by_name(&format!("key.keyboard.{name}"), Naming::Glfw);
        assert_eq!(
            bound(&bindings, InputId::Mouse(2)),
            ["key.pickItem", "key.spectatorHotbar"]
        );
        assert_eq!(bound(&bindings, InputId::Mouse(3)), ["key.use"]);
        assert_eq!(bound(&bindings, InputId::Key(224)), ["key.sprint"]);
        assert_eq!(
            bound(&bindings, key("c").unwrap()),
            ["key.saveToolbarActivator"]
        );
        assert_eq!(
            bound(&bindings, key("f3").unwrap()),
            ["key.debug.overlay", "key.debug.modifier"]
        );
        assert_eq!(bound(&bindings, key("9").unwrap()), ["key.hotbar.9"]);
        // 同じファイルを 26.x のものとして読んでも、同じキーになる：名前を変えたものがないため。
        assert_eq!(bindings_by_key(OPTIONS_1_21_11, Naming::Modern), bindings);
    }

    #[test]
    fn bindings_by_naming_and_odd_lines() {
        // 1.21 はアプリケーションキーを "menu"、日本語のキーをスキャンコードで名付ける。
        let text = "key_key.chat:key.keyboard.menu\n\
                    key_key.inventory:key.keyboard.menu\n\
                    key_key.drop:scancode.115\n\
                    key_key.jump:key.keyboard.backspace\n\
                    key_key.chat:key.keyboard.backspace\n\
                    key_Create New World:key.keyboard.u\n\
                    key_key.use:key.mouse.2\n\
                    key_key.attack:key.mouse.9\n\
                    key_key.sneak:key.keyboard.nonsense\n";
        let glfw = bindings_by_key(text, Naming::Glfw);
        // 繰り返されたチャットの行がチャットを Backspace に移し、最初の位置を保つ。マウスのボタン 9 と
        // 知らない名前は除く。
        assert_eq!(
            glfw,
            [
                (InputId::Key(42), vec!["key.chat".into(), "key.jump".into()]),
                (InputId::Key(101), vec!["key.inventory".into()]),
                (InputId::Key(135), vec!["key.drop".into()]),
                (InputId::Key(24), vec!["Create New World".into()]),
                (InputId::Mouse(3), vec!["key.use".into()]),
            ]
        );
        let modern = bindings_by_key(text, Naming::Modern);
        assert_eq!(
            modern,
            [
                (InputId::Key(42), vec!["key.chat".into(), "key.jump".into()]),
                (InputId::Key(118), vec!["key.inventory".into()]),
                (InputId::Key(24), vec!["Create New World".into()]),
                (InputId::Mouse(2), vec!["key.use".into()]),
            ]
        );
        assert!(bindings_by_key("", Naming::Modern).is_empty());
        assert!(
            bindings_by_key(
                "\u{feff}key_key.jump:key.keyboard.unknown\r\n",
                Naming::Glfw
            )
            .is_empty()
        );
    }
}
