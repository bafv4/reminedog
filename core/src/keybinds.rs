//! Minecraft's key names and the key bindings in `<game_dir>/options.txt`.
//!
//! options.txt stores a binding as `key_<mapping id>:<key name>`, e.g.
//! `key_key.debug.modifier:key.keyboard.f3`. The key names come from the games' own tables
//! (`keytable.rs`): 26.x (SDL3) has a name for every SDL scancode, 1.13 to 1.21 (GLFW) one
//! for every GLFW key code, and three names mean different keys in the two ([`Naming`]).
//! [`InputId`] is a key or mouse button independent of both, with conversions to and from
//! each library's codes and a label for the menu.
//!
//! F3+C is sent as key events for the debug modifier and copy-location mappings. Recent
//! versions (1.21.11, 26.x) let the user rebind both; older ones (1.16) have no such lines and
//! hard-code F3 and C, which are also the defaults here. The name lookups F3+C uses
//! ([`glfw_key`], [`sdl_scancode`], [`sdl_keycode`]) know only the keys a debug binding is
//! likely to use: letters, digits, F-keys and keypad digits.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use crate::keytable::{GLFW_KEYS, SDL_KEYS};

/// The key name options.txt stores for a mapping without a key.
pub const UNBOUND: &str = "key.keyboard.unknown";

const KEYBOARD_PREFIX: &str = "key.keyboard.";
const MOUSE_PREFIX: &str = "key.mouse.";
/// 1.21's names for keys GLFW has no key code for, by Win32 scancode.
const SCANCODE_PREFIX: &str = "scancode.";

const MODIFIER_ID: &str = "key.debug.modifier";
const OVERLAY_ID: &str = "key.debug.overlay";
const COPY_LOCATION_ID: &str = "key.debug.copyLocation";
const CRASH_ID: &str = "key.debug.crash";
/// Mappings the game only reads as "held" during its tick. A refused F3+C presses and
/// releases the copy key within one poll, so these never see it.
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
/// Drops the held item on a press; with Ctrl physically held, the whole stack.
const DROP_ID: &str = "key.drop";

/// The keys F3+C is made of, as the key names Minecraft stores in options.txt, e.g.
/// "key.keyboard.f3".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DebugKeys {
    pub modifier: String,
    pub copy_location: String,
    pub crash: String,
    /// Other (non-debug) mappings bound to the copy-location key that act on a press, by id
    /// (e.g. "key.drop"). A refused F3+C presses each of them once.
    pub shared_with_copy: Vec<String>,
}

impl DebugKeys {
    /// Whether a refused F3+C drops the held item (a whole stack with Ctrl held).
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

/// A `key_<id>:<name>` line of options.txt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct KeyLine<'a> {
    id: &'a str,
    name: &'a str,
    /// Forge and NeoForge write a modifier the mapping needs after the name
    /// (`key.keyboard.c:CONTROL`); the key alone does not set it off.
    with_modifier: bool,
}

/// The `key_<id>:<name>` lines of options.txt, in file order. The last line wins for a
/// repeated id, which keeps its first position. CRLF, a BOM and whitespace around the parts
/// are tolerated; lines with an empty part are skipped. A Forge modifier after the name is
/// split off.
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

/// Reads the debug keys from the text of options.txt.
///
/// Every `key_<id>:<name>` line is read, and the last one wins for a repeated id. A missing
/// debug line keeps its default (1.16 has none). CRLF, a BOM and whitespace around the parts
/// are tolerated. `shared_with_copy` lists, in file order, the mappings outside `key.debug.*`
/// bound to the copy-location key, except those only read as held and (Forge) those that
/// need a modifier as well; an unbound copy-location key shares nothing.
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

/// The mappings bound to each key or mouse button in the text of options.txt, by mapping id
/// in file order (e.g. `(Key(60), ["key.debug.overlay", "key.debug.modifier"])`), for showing
/// what a key does in the game. Keys appear in the order of their first mapping.
///
/// The lines are read as [`parse_debug_keys`] reads them, and the key names with `naming`.
/// Unbound mappings and names that are not a key are left out, and so are the debug mappings
/// other than the modifier and the overlay toggle: they act only together with the modifier.
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

/// `<game_dir>/options.txt`: the game's options, including the key bindings.
pub fn options_path(game_dir: &Path) -> PathBuf {
    game_dir.join("options.txt")
}

/// Reads the debug keys from `<game_dir>/options.txt`. A missing file (a fresh instance) gives
/// the defaults; text that is not UTF-8 is read lossily.
pub fn load_debug_keys(game_dir: &Path) -> io::Result<DebugKeys> {
    match fs::read(options_path(game_dir)) {
        Ok(bytes) => Ok(parse_debug_keys(&String::from_utf8_lossy(&bytes))),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(DebugKeys::default()),
        Err(e) => Err(e),
    }
}

/// Key ids are below this (SDL3's `SDL_SCANCODE_COUNT`), so an array this long holds a value
/// for every key.
pub const SCANCODE_COUNT: usize = 512;
/// The first key id: SDL3 has no keys below `SDL_SCANCODE_A`, and the names of 1..=3 would
/// read as the digit keys.
const FIRST_SCANCODE: u16 = 4;
/// Mouse ids are 1..=this: GLFW and SDL3 report no other buttons on Windows.
const MOUSE_BUTTONS: u8 = 5;
/// GLFW key codes are below this (`GLFW_KEY_LAST` + 1).
const GLFW_KEY_COUNT: usize = 349;
/// SDLK_SCANCODE_MASK: keys without a character use their scancode with this bit set.
const SCANCODE_MASK: u32 = 1 << 30;

/// A key or mouse button, independent of the window library.
///
/// `Key` is an SDL3 scancode (4..512), `Mouse` an SDL3 button number (1 left, 2 middle,
/// 3 right, 4 and 5 the side buttons). Both are also the values 26.x uses. Named after the
/// game's names with [`input_name`] and [`input_by_name`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum InputId {
    Key(u16),
    Mouse(u8),
}

/// Whose key names to read: a few names mean different keys in 1.21 and 26.x (1.21's
/// `keypad.decimal`, `menu` and `world.2` are 26.x's `keypad.period`, `application` and
/// `world.1`), and mouse buttons are numbered differently.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Naming {
    /// 26.x (SDL3): the names in settings.json, and in options.txt under SDL3.
    Modern,
    /// 1.13 to 1.21 (GLFW), as their options.txt has them: `scancode.<n>` for the keys GLFW
    /// has no key code for (the Japanese keys), and unnamed mouse buttons as GLFW's number + 1.
    Glfw,
}

/// The kind of a modifier key, for the modifier bits of key events.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Modifier {
    Shift,
    Ctrl,
    Alt,
    /// The Windows key (GLFW's Super, SDL3's GUI).
    Super,
}

/// Which of a pair of modifier keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Side {
    Left,
    Right,
}

/// Keys of Japanese keyboards GLFW has no key code for: GLFW reports them as key -1 with their
/// Win32 scancode, and 1.21 names them `scancode.<n>`. (Win32 scancode, SDL3 scancode), as
/// SDL3's Windows scancode table maps them.
const JIS_KEYS: [(u16, u16); 5] = [
    // カタカナ/ひらがな: international2.
    (0x70, 136),
    // ろ: international1.
    (0x73, 135),
    // 変換: international4.
    (0x79, 138),
    // 無変換: international5.
    (0x7b, 139),
    // ¥: international3.
    (0x7d, 137),
];

/// [`SDL_KEYS`] row + 1 by SDL scancode (0: no name). Building it checks the table: a bad or
/// repeated scancode does not compile.
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

/// [`GLFW_KEYS`] row + 1 by GLFW key code (0: no such key).
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

/// [`GLFW_KEYS`] row + 1 by SDL scancode (0: GLFW has no such key). Of two GLFW keys on one
/// scancode (`world.1` and `world.2`), the one Windows has (with a Win32 scancode).
const GLFW_ROW_BY_SDL: [u8; SCANCODE_COUNT] = {
    let mut rows = [0; SCANCODE_COUNT];
    let mut i = 0;
    while i < GLFW_KEYS.len() {
        let (_, _, sc, win) = GLFW_KEYS[i];
        let sc = sc as usize;
        assert!(sc < SCANCODE_COUNT && (sc == 0 || SDL_ROW[sc] != 0));
        if sc != 0 {
            let old = rows[sc];
            // At most one of them has a Win32 scancode.
            assert!(old == 0 || GLFW_KEYS[old as usize - 1].3 == 0 || win == 0);
            if old == 0 || win != 0 {
                rows[sc] = (i + 1) as u8;
            }
        }
        i += 1;
    }
    rows
};

/// The [`SDL_KEYS`] row of a scancode.
const fn sdl_row(sc: u16) -> Option<(u16, &'static str, u32)> {
    if sc as usize >= SCANCODE_COUNT {
        return None;
    }
    match SDL_ROW[sc as usize] {
        0 => None,
        row => Some(SDL_KEYS[row as usize - 1]),
    }
}

/// The [`GLFW_KEYS`] row of a GLFW key code.
const fn glfw_row(key: i32) -> Option<(i32, &'static str, u16, u16)> {
    if key < 0 || key as usize >= GLFW_KEY_COUNT {
        return None;
    }
    match GLFW_ROW[key as usize] {
        0 => None,
        row => Some(GLFW_KEYS[row as usize - 1]),
    }
}

/// The [`GLFW_KEYS`] row of an SDL scancode.
const fn glfw_row_of_sdl(sc: u16) -> Option<(i32, &'static str, u16, u16)> {
    if sc as usize >= SCANCODE_COUNT {
        return None;
    }
    match GLFW_ROW_BY_SDL[sc as usize] {
        0 => None,
        row => Some(GLFW_KEYS[row as usize - 1]),
    }
}

/// The SDL scancode of a Japanese key by Win32 scancode.
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

/// The Win32 scancode of a Japanese key by SDL scancode.
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

/// Whether a scancode is in the range of key ids.
const fn is_key(sc: u16) -> bool {
    sc >= FIRST_SCANCODE && (sc as usize) < SCANCODE_COUNT
}

/// A decimal number as the game writes it for an unnamed key: digits only.
fn number(text: &str) -> Option<u32> {
    if text.is_empty() || text.len() > 9 || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// The rows of [`SDL_KEYS`] by name.
static SDL_NAMES: LazyLock<HashMap<&'static str, u16>> =
    LazyLock::new(|| SDL_KEYS.iter().map(|&(sc, name, _)| (name, sc)).collect());
/// A row of [`GLFW_KEYS`]: the GLFW key code, its name, and SDL scancodes.
type GlfwRow = (i32, &'static str, u16, u16);

/// The rows of [`GLFW_KEYS`] by name.
static GLFW_NAMES: LazyLock<HashMap<&'static str, GlfwRow>> =
    LazyLock::new(|| GLFW_KEYS.iter().map(|&row| (row.1, row)).collect());

/// A key id from 26.x's name after `key.keyboard.`: its name, or the scancode itself.
fn modern_key(rest: &str) -> Option<InputId> {
    let sc = match SDL_NAMES.get(rest) {
        Some(&sc) => sc,
        None => u16::try_from(number(rest)?).ok()?,
    };
    is_key(sc).then_some(InputId::Key(sc))
}

/// The [`GLFW_KEYS`] row of 1.21's name after `key.keyboard.`: its name, or the GLFW key code
/// itself.
fn glfw_name_row(rest: &str) -> Option<(i32, &'static str, u16, u16)> {
    match GLFW_NAMES.get(rest) {
        Some(&row) => Some(row),
        None => glfw_row(i32::try_from(number(rest)?).ok()?),
    }
}

/// The key or mouse button of a key name from options.txt or settings.json, e.g.
/// `key.keyboard.f3` or `key.mouse.4`, read as `naming` reads it. Like the game, a number
/// stands for an unnamed key (`key.keyboard.<scancode>` in 26.x, `key.keyboard.<GLFW key>` in
/// 1.21). `None` for `key.keyboard.unknown`, ids out of range, keys the other library has no
/// equivalent for (GLFW's F25) and anything else.
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

/// 26.x's name of a key or mouse button (what settings.json stores), e.g. `key.keyboard.f3`,
/// `key.mouse.4`; an unnamed key is `key.keyboard.<scancode>`. [`input_by_name`] with
/// [`Naming::Modern`] reads it back for every id in range; ids out of range get names it
/// refuses.
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

/// The menu's label of a key or mouse button: `A`, `1`, `F3`, `左 Ctrl`, `スペース`, `↑`,
/// US symbols for punctuation, `テンキー 1`, `テンキーの Enter`, the Japanese keys (`ろ`,
/// `¥`, `変換`, `無変換`, `カタカナ/ひらがな`), `左クリック`, `マウスのボタン4`, and
/// `キー 130` for keys without a label here.
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
        // Letters, then the digit row 1..9, 0.
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

/// The key of a GLFW key event (1.13 to 1.21): by key code, or for key -1 (the keys GLFW has
/// no code for) by Win32 scancode, which is how the game tells those apart. `None` for keys
/// SDL3 has no equivalent for (F25) and codes out of range.
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

/// The mouse button of a GLFW button number: GLFW counts from 0 and has right before middle
/// (0 left, 1 right, 2 middle, 3 and 4 the side buttons).
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

/// The GLFW key code and Win32 scancode to send a key as: `(-1, scancode)` for the keys GLFW
/// has no code for, else `(key code, scancode or 0)`. `None` for mouse buttons and keys GLFW
/// lacks. The scancode of a coded key is what GLFW 3.4 reports on Windows; the game ignores it.
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

/// The GLFW button number of a mouse button (the reverse of [`input_from_glfw_button`]).
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

/// The SDL3 keycode of a key (the `key` field of a key event), as SDL3's default keymap gives
/// it with a US layout: the character for letters, digits and punctuation, the scancode with
/// `SDLK_SCANCODE_MASK` for other keys. 0 (`SDLK_UNKNOWN`) for mouse buttons and ids out of
/// range.
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

/// The Win32 scancode of a key (set 1, with 0x100 for the 0xE0 prefix), as GLFW 3.4 reports
/// it; `None` for mouse buttons and keys GLFW does not know.
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

/// Which modifier a key is: the left and right Ctrl, Shift, Alt and Windows keys.
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

/// Whether F3+C may use a key (by SDL scancode): letters, digits, F-keys and keypad digits.
const fn f3c_key(sc: u16) -> bool {
    matches!(sc, 4..=39 | 58..=69 | 89..=98 | 104..=115)
}

/// The GLFW key code of a 1.21 key name (what 1.13 to 1.21 match bindings on), for the keys
/// F3+C may use: letters, digits, F1..F25 and keypad digits.
pub fn glfw_key(name: &str) -> Option<i32> {
    let rest = name.strip_prefix(KEYBOARD_PREFIX)?;
    let &(key, _, _, _) = GLFW_NAMES.get(rest)?;
    matches!(key, 48..=57 | 65..=90 | 290..=314 | 320..=329).then_some(key)
}

/// The key of a 26.x key name, for the keys F3+C may use.
fn f3c_scancode(name: &str) -> Option<u16> {
    let rest = name.strip_prefix(KEYBOARD_PREFIX)?;
    let &sc = SDL_NAMES.get(rest)?;
    f3c_key(sc).then_some(sc)
}

/// The SDL3 scancode of a key name (what 26.x matches bindings on), for the keys F3+C may use:
/// letters, digits, F1..F24 (SDL has no F25) and keypad digits.
pub fn sdl_scancode(name: &str) -> Option<u32> {
    f3c_scancode(name).map(u32::from)
}

/// The SDL3 keycode of a key name, for the `key` field of synthetic key events; the keys of
/// [`sdl_scancode`].
pub fn sdl_keycode(name: &str) -> Option<u32> {
    f3c_scancode(name).map(|sc| sdl_keycode_of(InputId::Key(sc)))
}

/// A short label for a key name, e.g. "F3", "C", "テンキー 1", "左 Shift", "マウスの戻る" (as
/// [`input_label`] has them, whether 26.x or 1.21 named it); names of neither are shown as
/// they are.
pub fn key_label(name: &str) -> String {
    if let Some(sc) = f3c_scancode(name) {
        return input_label(InputId::Key(sc));
    }
    if let Some(key @ 290..=314) = glfw_key(name) {
        // GLFW's F25, which SDL3 lacks.
        return format!("F{}", key - 289);
    }
    match input_by_name(name, Naming::Modern).or_else(|| input_by_name(name, Naming::Glfw)) {
        Some(id) => input_label(id),
        None => name.to_owned(),
    }
}

/// The Japanese name of a vanilla mapping id such as "key.drop" (the game's ja_jp names);
/// other ids are returned as they are.
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
        // The shape of the user's 26.3 options.txt: the debug keys at their defaults,
        // "drop" moved to C and the toolbar activator moved away from it.
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
        // 1.16.1 hard-codes F3 and C and writes no debug lines.
        let text = "key_key.drop:key.keyboard.c\nkey_key.inventory:key.keyboard.e\n\
                    key_key.saveToolbarActivator:key.keyboard.c\n";
        // The hotbar saver on C is only read as held, so a refused F3+C cannot trigger it.
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
        // Forge writes the modifier a mapping needs after the key; NONE is none.
        let text = "key_key.debug.copyLocation:key.keyboard.c:NONE\n\
                    key_key.drop:key.keyboard.c:CONTROL\n\
                    key_key.inventory:key.keyboard.c\n";
        let parsed = parse_debug_keys(text);
        assert_eq!(parsed.copy_location, "key.keyboard.c");
        // Drop needs Ctrl as well, so F3+C cannot set it off.
        assert_eq!(parsed.shared_with_copy, ["key.inventory"]);
        // The menu lists both (debug mappings other than the modifier are left out).
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
        // Debug mappings are left out; modded ones are listed by id.
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
        // Other unbound mappings do not "share" the missing key.
        assert!(parsed.shared_with_copy.is_empty());
    }

    #[test]
    fn crlf_bom_and_whitespace() {
        let text = "\u{feff}key_key.debug.modifier:key.keyboard.f6\r\n\
                    \r\n\
                    \x20 key_key.drop : key.keyboard.v \r\n\
                    \tkey_key.debug.copyLocation:key.keyboard.v\r\n";
        assert_eq!(parse_debug_keys(text), keys("f6", "v", "c", &["key.drop"]));
        // A BOM right before the first key line.
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
        // The last line wins, and the mapping keeps its first position.
        assert_eq!(parsed.copy_location, "key.keyboard.q");
        assert_eq!(parsed.shared_with_copy, ["key.drop"]);
        // An empty value is ignored rather than taken as a key.
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
            // The letter, not a function key.
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
        // Keys F3+C cannot use get their labels too.
        assert_eq!(key_label("key.mouse.left"), input_label(InputId::Mouse(1)));
        assert_eq!(key_label(UNBOUND), UNBOUND);
    }

    #[test]
    fn mapping_labels() {
        assert_eq!(mapping_label("key.drop"), "アイテムを捨てる");
        // The names the game's own ja_jp key settings show.
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
        // 1.21.11's and 26.3's mappings, with the game's ja_jp names.
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
        // The other debug mappings are not shown, and keep their ids.
        assert_eq!(mapping_label("key.debug.crash"), "key.debug.crash");
    }

    #[test]
    fn f3c_lookups_keep_to_their_keys() {
        // Named keys outside letters, digits, F-keys and keypad digits stay unsupported for
        // F3+C, and so do numbers standing for keys.
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
        // The keys they know are the table's.
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
        // 26 letters, 10 digits, F1..F25, 10 keypad digits; SDL3 lacks F25.
        assert_eq!(count(|name| glfw_key(name).is_some()), 71);
        assert_eq!(count(|name| sdl_scancode(name).is_some()), 70);
    }

    /// Notable rows of 26.3's table (`InputConstants.Type.KEYBOARD`).
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
        // Every row reads back from its name, and gives its name.
        for (sc, name, _) in SDL_KEYS {
            let full = format!("{KEYBOARD_PREFIX}{name}");
            assert_eq!(
                input_by_name(&full, Naming::Modern),
                Some(InputId::Key(sc)),
                "{full}"
            );
            assert_eq!(input_name(InputId::Key(sc)), full);
        }
        // A number stands for a key, as in the game: unnamed ones, and named ones too.
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
        // The digit keys' names are names, not numbers.
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
        // Ids out of range never panic, and get names that do not read back.
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
        // (1.21 name, GLFW key, 26.x name)
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
            // Named keys are told apart by key code; the scancode does not matter.
            assert_eq!(input_from_glfw_key(key, 0), id, "{full}");
            assert_eq!(
                input_by_name(&format!("{KEYBOARD_PREFIX}{key}"), Naming::Glfw),
                id,
                "{full}"
            );
            // GLFW's F25 has no SDL3 key.
            let Some(id) = id else {
                assert_eq!(name, "f25");
                continue;
            };
            // The same name in 26.x, except for the renamed keys.
            let modern = match renamed.iter().find(|(n, _, _)| *n == name) {
                Some((_, renamed_key, modern)) => {
                    assert_eq!(*renamed_key, key);
                    format!("{KEYBOARD_PREFIX}{modern}")
                }
                None => full.clone(),
            };
            assert_eq!(input_name(id), modern, "{full}");
            if name == "world.1" {
                // Not a key on Windows; the ISO key is world.2.
                assert_eq!(glfw_key_of(id), Some((162, 0x56)));
                continue;
            }
            assert_eq!(glfw_key_of(id), Some((key, i32::from(win))), "{full}");
            assert_eq!(win_scancode_of(id), Some(win), "{full}");
            // Sending a key gives the same key back.
            assert_eq!(input_from_glfw_key(key, i32::from(win)), Some(id));
        }
        // Some rows by GLFW key code and the Win32 scancode GLFW 3.4 reports.
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
        // Keys only SDL3 has.
        for sc in [50, 118, 140, 144, 220, 260] {
            assert_eq!(glfw_key_of(InputId::Key(sc)), None, "{sc}");
        }
    }

    #[test]
    fn names_that_differ_between_versions() {
        // (name, 1.21's key, 26.x's key)
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
        // 26.x's names for 1.21's keys are not 1.21 names.
        assert_eq!(
            input_by_name("key.keyboard.keypad.period", Naming::Glfw),
            None
        );
        assert_eq!(
            input_by_name("key.keyboard.application", Naming::Glfw),
            None
        );
        // A number is a GLFW key code in 1.21 and a scancode in 26.x.
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
        // SDL3's numbers, which 26.x uses as they are.
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
        // 1.21 names a button by GLFW's number + 1, and GLFW has right before middle.
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
        // Neither library reports buttons 6..8 on Windows.
        for button in [-1, 5, 6, 7, i32::MAX, i32::MIN] {
            assert_eq!(input_from_glfw_button(button), None, "{button}");
        }
        assert_eq!(input_name(InputId::Mouse(1)), "key.mouse.left");
        assert_eq!(input_name(InputId::Mouse(2)), "key.mouse.middle");
        assert_eq!(input_name(InputId::Mouse(3)), "key.mouse.right");
        assert_eq!(input_name(InputId::Mouse(4)), "key.mouse.4");
        assert_eq!(input_name(InputId::Mouse(5)), "key.mouse.5");
        // Mouse buttons are not keys.
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
        // (Win32 scancode, SDL3 scancode, label)
        let cases = [
            (0x70, 136, "カタカナ/ひらがな"),
            (0x73, 135, "ろ"),
            (0x79, 138, "変換"),
            (0x7b, 139, "無変換"),
            (0x7d, 137, "¥"),
        ];
        for (win, sc, label) in cases {
            let id = InputId::Key(sc);
            // GLFW sends them as key -1; 1.21 names them by the decimal scancode.
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
        // Other keys GLFW sends as -1 are not known.
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
        // The same words as the menu's hotkeys.
        assert_eq!(input_label(InputId::Mouse(1)), "左クリック");
        assert_eq!(input_label(InputId::Mouse(3)), "右クリック");
        assert_eq!(input_label(InputId::Mouse(2)), "ホイールクリック");
        assert_eq!(input_label(InputId::Mouse(4)), "マウスのボタン4");
        assert_eq!(input_label(InputId::Mouse(5)), "マウスのボタン5");
        // Every key in range has a label of its own.
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
            // Unnamed: the scancode with the mask, as SDL3 makes it.
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
        // Keys GLFW does not know.
        for sc in [50, 118, 140, 144, 220] {
            assert_eq!(win_scancode_of(InputId::Key(sc)), None, "{sc}");
        }
    }

    /// The key lines of the user's 26.3 options.txt.
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

    /// The key lines of the user's 1.21.11 options.txt.
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

    /// The mapping ids bound to `id`.
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
        // F3: the modifier and the overlay toggle, in file order. The other debug mappings
        // act only with F3 held, so C is only "drop".
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
        // Keys bound only to other debug mappings are not listed.
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
        // The labels the menu shows for F3.
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
        // The same file read as 26.x's gives the same keys: it has none of the renamed ones.
        assert_eq!(bindings_by_key(OPTIONS_1_21_11, Naming::Modern), bindings);
    }

    #[test]
    fn bindings_by_naming_and_odd_lines() {
        // 1.21 names the application key "menu" and the Japanese keys by scancode.
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
        // The repeated chat line moved chat to Backspace, keeping its first position; the
        // mouse button 9 and the unknown name are left out.
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
