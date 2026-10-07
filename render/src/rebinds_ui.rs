//! The menu's 「キーの置き換え」 section, and [`resolve`]: the rules in the settings that the
//! input router uses.

use std::fmt;

use egui::RichText;
use reminedog_core::{
    InputId, Modifier, Naming, Rebind, Settings, input_by_name, input_label, input_name,
    mapping_label, modifier_kind,
};

use crate::hotkey::{Hotkey, input_trigger};
use crate::input::Captured;
use crate::overlay::{Action, Hotkeys};
use crate::rebind::MAX_REBINDS;

const DESCRIPTION: &str = "Minecraft に届くキーを別のキーやマウスのボタンに置き換える。\
     ゲーム中だけ置き換える（チャットやインベントリなどの画面と、このメニューを開いている間は元のキーのまま）";

/// The mouse buttons offered as outputs, which cannot be captured: the left and right buttons
/// work the menu.
const CLICK_TARGETS: [(&str, InputId); 3] = [
    ("左クリック", InputId::Mouse(1)),
    ("右クリック", InputId::Mouse(3)),
    ("ホイールクリック", InputId::Mouse(2)),
];

/// Keys that cannot be a source: Esc stays the way to the game's menu and out of ours, the
/// Windows keys open the Start menu whatever the game gets, PrintScreen arrives only when it
/// is released, and the IME keys of Japanese keyboards (international1-5, lang1-5) may never
/// send a release.
fn refused_source(id: InputId) -> bool {
    matches!(
        id,
        InputId::Key(41 | 70 | 227 | 231 | 135..=139 | 144..=148)
    )
}

/// Keys that cannot be an output: Esc (the game's own menu stays on Esc) and the Windows keys.
fn refused_target(id: InputId) -> bool {
    matches!(id, InputId::Key(41 | 227 | 231))
}

/// Why a rule in the settings is not used.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Unused {
    /// Not one of 26.x's key names.
    UnknownName(String),
    Source(InputId),
    Target(InputId),
    /// The game's window library cannot report this source or give the game this output.
    Unsupported(InputId),
    SameKey,
    /// An earlier rule has the same source.
    Duplicate(InputId),
    /// A hotkey without modifiers takes every press of the source.
    Hotkey(InputId, Action),
    TooMany,
}

impl Unused {
    /// For the menu.
    fn note(&self) -> String {
        match self {
            Unused::UnknownName(name) => {
                format!("「{name}」はキーの名前として読めないので使わない")
            }
            Unused::Source(id) => format!("{} は置き換えるキーにできない", input_label(*id)),
            Unused::Target(id) => format!("{} はゲームに送るキーにできない", input_label(*id)),
            Unused::Unsupported(id) => {
                format!("{} はこの版のゲームでは使えない", input_label(*id))
            }
            Unused::SameKey => "同じキーには置き換えられない".to_owned(),
            Unused::Duplicate(id) => format!("{} はもう置き換えている", input_label(*id)),
            Unused::Hotkey(id, action) => format!(
                "{} は「{}」に使っているので置き換えられない",
                input_label(*id),
                action.name()
            ),
            Unused::TooMany => format!("置き換えは {MAX_REBINDS} 個まで"),
        }
    }
}

/// For the log.
impl fmt::Display for Unused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Unused::UnknownName(name) => write!(f, "unknown key name {name:?}"),
            Unused::Source(id) => write!(f, "{} cannot be rebound", input_name(*id)),
            Unused::Target(id) => write!(f, "{} cannot be sent", input_name(*id)),
            Unused::Unsupported(id) => {
                write!(f, "the game's window library has no {}", input_name(*id))
            }
            Unused::SameKey => f.write_str("rebinds a key to itself"),
            Unused::Duplicate(id) => write!(f, "{} is rebound by an earlier rule", input_name(*id)),
            Unused::Hotkey(id, action) => {
                write!(f, "{} is the {} hotkey", input_name(*id), action.id())
            }
            Unused::TooMany => write!(f, "more than {MAX_REBINDS} rules"),
        }
    }
}

/// The hotkey without modifiers on `source`, which takes every press of it.
fn plain_hotkey_on(source: InputId, keys: &Hotkeys) -> Option<Action> {
    let trigger = input_trigger(source)?;
    Action::ALL
        .into_iter()
        .find(|&action| keys.get(action) == Some(Hotkey::plain(trigger)))
}

/// Each rule of the settings as the ids it uses, or why it is not used, in order: names that
/// are not keys, refused sources and outputs, keys and buttons in `unsupported` (the game's
/// window library cannot report the source or send the output), a key on itself, a source an
/// earlier rule has, a source a hotkey without modifiers takes, and rules beyond
/// [`MAX_REBINDS`].
fn check_rules(
    rebinds: &[Rebind],
    keys: &Hotkeys,
    unsupported: &[InputId],
) -> Vec<Result<(InputId, InputId), Unused>> {
    let mut used: Vec<InputId> = Vec::new();
    rebinds
        .iter()
        .map(|rule| {
            let id = |name: &str| {
                input_by_name(name, Naming::Modern).ok_or_else(|| Unused::UnknownName(name.into()))
            };
            let (from, to) = (id(&rule.from)?, id(&rule.to)?);
            if refused_source(from) {
                return Err(Unused::Source(from));
            }
            if refused_target(to) {
                return Err(Unused::Target(to));
            }
            if let Some(&id) = [from, to].iter().find(|id| unsupported.contains(id)) {
                return Err(Unused::Unsupported(id));
            }
            if from == to {
                return Err(Unused::SameKey);
            }
            if used.contains(&from) {
                return Err(Unused::Duplicate(from));
            }
            if let Some(action) = plain_hotkey_on(from, keys) {
                return Err(Unused::Hotkey(from, action));
            }
            if used.len() >= MAX_REBINDS {
                return Err(Unused::TooMany);
            }
            used.push(from);
            Ok((from, to))
        })
        .collect()
}

/// The rules in `settings` the router uses, as (source, output): none while the rebinding is
/// off; rules that cannot be used are left out (see [`check_rules`]; `unsupported`: keys and
/// buttons the game's window library cannot handle), each logged. Logs, so call it when the
/// rules or the hotkeys change, not every frame.
pub fn resolve(
    settings: &Settings,
    keys: &Hotkeys,
    unsupported: &[InputId],
) -> Vec<(InputId, InputId)> {
    if !settings.rebinds_enabled {
        log::info!("rebinds: off ({} in the settings)", settings.rebinds.len());
        return Vec::new();
    }
    let mut rules = Vec::new();
    for (rule, check) in
        settings
            .rebinds
            .iter()
            .zip(check_rules(&settings.rebinds, keys, unsupported))
    {
        match check {
            Ok(pair) => rules.push(pair),
            Err(unused) => log::warn!("rebinds: left out {} -> {}: {unused}", rule.from, rule.to),
        }
    }
    rules
}

/// The sources of the rules in `settings` (known names only), for refusing hotkeys on them.
pub(crate) fn rule_sources(settings: &Settings) -> impl Iterator<Item = InputId> + '_ {
    settings
        .rebinds
        .iter()
        .filter_map(|rule| input_by_name(&rule.from, Naming::Modern))
}

/// Warnings for a rule in use whose source a hotkey also uses: with its modifiers, or as one.
fn hotkey_warnings(source: InputId, keys: &Hotkeys) -> Vec<String> {
    let trigger = input_trigger(source);
    let modifier = modifier_kind(source).map(|(kind, _)| kind);
    let mut warnings = Vec::new();
    for action in Action::ALL {
        let Some(hotkey) = keys.get(action) else {
            continue;
        };
        if trigger == Some(hotkey.trigger) {
            warnings.push(format!(
                "{} は「{}」（{}）にも使っている",
                input_label(source),
                action.name(),
                hotkey.label()
            ));
        }
        let is_modifier = match modifier {
            Some(Modifier::Ctrl) => hotkey.ctrl,
            Some(Modifier::Shift) => hotkey.shift,
            Some(Modifier::Alt) => hotkey.alt,
            Some(Modifier::Super) | None => false,
        };
        if is_modifier {
            warnings.push(format!(
                "{} は「{}」（{}）の修飾キーにも使っている",
                input_label(source),
                action.name(),
                hotkey.label()
            ));
        }
    }
    warnings
}

/// What the game does with `id`, from its key bindings: "（前進）", "（デバッグ修飾キー・オーバーレイの
/// 切り替え）", or nothing when nothing is bound to it.
fn mappings_text(id: InputId, bindings: &[(InputId, Vec<String>)]) -> String {
    let Some((_, mappings)) = bindings.iter().find(|(key, _)| *key == id) else {
        return String::new();
    };
    if mappings.is_empty() {
        return String::new();
    }
    let labels: Vec<String> = mappings.iter().map(|m| mapping_label(m)).collect();
    format!("（{}）", labels.join("・"))
}

/// A row of the list: "マウスのボタン4 → F3（デバッグ修飾キー・オーバーレイの切り替え）".
fn row_text(
    rule: &Rebind,
    check: &Result<(InputId, InputId), Unused>,
    bindings: &[(InputId, Vec<String>)],
) -> String {
    let label = |name: &str| match input_by_name(name, Naming::Modern) {
        Some(id) => input_label(id),
        None => name.to_owned(),
    };
    let mappings = match check {
        Ok((_, to)) => mappings_text(*to, bindings),
        Err(_) => String::new(),
    };
    format!("{} → {}{mappings}", label(&rule.from), label(&rule.to))
}

/// Where adding a rule is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Adding {
    #[default]
    No,
    /// Waiting for the source.
    Source,
    /// Waiting for the output of this source.
    Target(InputId),
}

/// The section's own state.
#[derive(Debug, Default)]
pub(crate) struct RebindMenu {
    adding: Adding,
    /// Why the last key pressed for a rule was not taken.
    note: Option<String>,
}

/// What the section asks the router for.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct RebindActions {
    /// Capture the next key or button ([`crate::InputRouter::start_input_capture`]).
    pub(crate) capture: bool,
    pub(crate) cancel_capture: bool,
}

impl RebindMenu {
    /// Whether a key is being waited for.
    pub(crate) fn adding(&self) -> bool {
        self.adding != Adding::No
    }

    /// Stops waiting for a key (the menu closed, or a hotkey is being assigned instead), and
    /// forgets why a key was not taken.
    pub(crate) fn cancel(&mut self) {
        self.adding = Adding::No;
        self.note = None;
    }

    fn start(&mut self) {
        self.adding = Adding::Source;
        self.note = None;
    }

    /// The router's capture ended. Returns whether to capture again (for the output).
    pub(crate) fn on_captured(
        &mut self,
        captured: Captured,
        settings: &mut Settings,
        keys: &Hotkeys,
    ) -> bool {
        let adding = std::mem::take(&mut self.adding);
        let id = match (adding, captured) {
            (Adding::No, _) => return false,
            (_, Captured::Input(id)) => id,
            // Esc, or the menu closing.
            (_, Captured::Cancelled) => {
                self.note = None;
                return false;
            }
            // A capture that was not ours.
            (_, Captured::Hotkey(_)) => return false,
        };
        match adding {
            Adding::Source => match source_refusal(id, settings, keys) {
                Some(note) => {
                    self.note = Some(note);
                    false
                }
                None => {
                    self.note = None;
                    self.adding = Adding::Target(id);
                    true
                }
            },
            Adding::Target(source) => self.target(source, id, settings),
            Adding::No => false,
        }
    }

    /// The output for `source` was chosen: adds the rule, or stays at the output with a note.
    /// Returns whether still waiting for the output.
    fn target(&mut self, source: InputId, target: InputId, settings: &mut Settings) -> bool {
        let refusal = if refused_target(target) {
            Some(Unused::Target(target))
        } else if target == source {
            Some(Unused::SameKey)
        } else {
            None
        };
        if let Some(refusal) = refusal {
            self.note = Some(refusal.note());
            self.adding = Adding::Target(source);
            return true;
        }
        self.adding = Adding::No;
        if settings.rebinds.len() >= MAX_REBINDS {
            self.note = Some(Unused::TooMany.note());
            return false;
        }
        self.note = None;
        settings.rebinds.push(Rebind {
            from: input_name(source),
            to: input_name(target),
        });
        false
    }
}

/// Why `id` cannot be the source of a new rule.
fn source_refusal(id: InputId, settings: &Settings, keys: &Hotkeys) -> Option<String> {
    let unused = if refused_source(id) {
        Unused::Source(id)
    } else if rule_sources(settings).any(|source| source == id) {
        Unused::Duplicate(id)
    } else if let Some(action) = plain_hotkey_on(id, keys) {
        Unused::Hotkey(id, action)
    } else {
        return None;
    };
    Some(unused.note())
}

/// Where the settings file was copied because rebinding entries in it could not be read.
fn kept_note(settings: &Settings) -> Option<String> {
    let copy = settings.rebinds_kept_in.as_ref()?;
    let file = copy.file_name().map_or_else(
        || copy.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    );
    Some(format!(
        "settings.json の置き換えに読めない項目があったので {file} に残した"
    ))
}

/// The 「キーの置き換え」 section. `bindings`: the game's mappings by key, from options.txt;
/// `note`: why some rules cannot work now (shown as a warning); `unsupported`: keys and
/// buttons the game's window library cannot handle.
pub(crate) fn rebind_section(
    ui: &mut egui::Ui,
    menu: &mut RebindMenu,
    settings: &mut Settings,
    keys: &Hotkeys,
    bindings: &[(InputId, Vec<String>)],
    note: Option<&str>,
    unsupported: &[InputId],
) -> RebindActions {
    let mut actions = RebindActions::default();
    let title = format!("キーの置き換え（{} 個）", settings.rebinds.len());
    let shown = egui::CollapsingHeader::new(title)
        .id_salt("reminedog-rebinds-section")
        .show(ui, |ui| {
            section_body(
                ui,
                menu,
                settings,
                keys,
                bindings,
                note,
                unsupported,
                &mut actions,
            )
        });
    if shown.body_returned.is_none() && menu.adding() {
        // Folded while waiting for a key.
        menu.cancel();
        actions.cancel_capture = true;
    }
    actions
}

#[allow(clippy::too_many_arguments)]
fn section_body(
    ui: &mut egui::Ui,
    menu: &mut RebindMenu,
    settings: &mut Settings,
    keys: &Hotkeys,
    bindings: &[(InputId, Vec<String>)],
    note: Option<&str>,
    unsupported: &[InputId],
    actions: &mut RebindActions,
) {
    let warn = ui.visuals().warn_fg_color;
    ui.label(DESCRIPTION);
    ui.checkbox(&mut settings.rebinds_enabled, "キーの置き換えを使う");
    if let Some(note) = note {
        ui.label(RichText::new(note).color(warn));
    }
    if let Some(note) = kept_note(settings) {
        ui.label(RichText::new(note).color(warn));
    }
    let checks = check_rules(&settings.rebinds, keys, unsupported);
    let mut delete = None;
    ui.add_enabled_ui(settings.rebinds_enabled, |ui| {
        for (i, (rule, check)) in settings.rebinds.iter().zip(&checks).enumerate() {
            ui.push_id(i, |ui| {
                // Wrapped, so a long row does not widen the menu.
                ui.horizontal_wrapped(|ui| {
                    ui.label(row_text(rule, check, bindings));
                    if ui.button("削除").clicked() {
                        delete = Some(i);
                    }
                });
                let warnings = match check {
                    Ok((from, _)) => hotkey_warnings(*from, keys),
                    Err(unused) => vec![unused.note()],
                };
                for warning in warnings {
                    ui.label(RichText::new(warning).color(warn));
                }
            });
        }
    });
    if let Some(i) = delete {
        settings.rebinds.remove(i);
    }
    if settings.rebinds.is_empty() {
        ui.label(RichText::new("置き換えはまだない").weak());
    }

    match menu.adding {
        Adding::No => {
            let full = settings.rebinds.len() >= MAX_REBINDS;
            if ui
                .add_enabled(!full, egui::Button::new("追加"))
                .on_disabled_hover_text(Unused::TooMany.note())
                .clicked()
            {
                menu.start();
                actions.capture = true;
            }
        }
        Adding::Source => {
            ui.label(
                RichText::new("置き換えるキーかマウスのボタンを押す（Esc で取り消し）").weak(),
            );
            if ui.button("取り消し").clicked() {
                menu.cancel();
                actions.cancel_capture = true;
            }
        }
        Adding::Target(source) => {
            ui.label(
                RichText::new(format!(
                    "{} → ？ ゲームに送るキーかマウスのボタンを押す（Esc で取り消し）",
                    input_label(source)
                ))
                .weak(),
            );
            let mut picked = None;
            ui.horizontal_wrapped(|ui| {
                for (label, id) in CLICK_TARGETS {
                    if ui.button(label).clicked() {
                        picked = Some(id);
                    }
                }
                if ui.button("取り消し").clicked() {
                    menu.cancel();
                    actions.cancel_capture = true;
                }
            });
            if let Some(id) = picked
                && !menu.target(source, id, settings)
            {
                // Chosen with a click: the router is still waiting for a key.
                actions.cancel_capture = true;
            }
        }
    }
    if let Some(note) = &menu.note {
        ui.label(RichText::new(note).color(warn));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MOUSE4: InputId = InputId::Mouse(4);
    const MOUSE5: InputId = InputId::Mouse(5);
    const F3: InputId = InputId::Key(60);
    const C: InputId = InputId::Key(6);
    const Z: InputId = InputId::Key(29);
    const CAPS: InputId = InputId::Key(57);
    const LCTRL: InputId = InputId::Key(224);
    const ESC: InputId = InputId::Key(41);
    const LEFT_WIN: InputId = InputId::Key(227);

    fn rebind(from: &str, to: &str) -> Rebind {
        Rebind {
            from: from.into(),
            to: to.into(),
        }
    }

    fn settings(rules: &[(&str, &str)]) -> Settings {
        Settings {
            rebinds: rules.iter().map(|(from, to)| rebind(from, to)).collect(),
            ..Settings::default()
        }
    }

    #[test]
    fn resolve_reads_the_names_and_leaves_out_what_cannot_work() {
        let keys = Hotkeys::DEFAULT;
        let s = settings(&[
            ("key.mouse.4", "key.keyboard.f3"),
            ("key.keyboard.nope", "key.keyboard.c"),
            ("key.keyboard.escape", "key.keyboard.c"),
            ("key.keyboard.lang1", "key.keyboard.c"),
            ("key.keyboard.caps.lock", "key.keyboard.left.win"),
            ("key.mouse.5", "key.mouse.5"),
            ("key.mouse.4", "key.keyboard.c"),
            // Z is the zoom.
            ("key.keyboard.z", "key.keyboard.c"),
            ("key.keyboard.caps.lock", "key.keyboard.left.control"),
            ("key.mouse.5", "key.mouse.left"),
        ]);
        assert_eq!(
            resolve(&s, &keys, &[]),
            [(MOUSE4, F3), (CAPS, LCTRL), (MOUSE5, InputId::Mouse(1))]
        );
        let checks = check_rules(&s.rebinds, &keys, &[]);
        let errors: Vec<_> = checks.iter().filter_map(|c| c.clone().err()).collect();
        assert_eq!(
            errors,
            [
                Unused::UnknownName("key.keyboard.nope".into()),
                Unused::Source(ESC),
                Unused::Source(InputId::Key(144)),
                Unused::Target(LEFT_WIN),
                Unused::SameKey,
                Unused::Duplicate(MOUSE4),
                Unused::Hotkey(Z, Action::Zoom),
            ]
        );
        // Off: nothing.
        let off = Settings {
            rebinds_enabled: false,
            ..s
        };
        assert!(resolve(&off, &keys, &[]).is_empty());
    }

    #[test]
    fn rules_the_window_library_cannot_handle_are_left_out_with_a_note() {
        // GLFW has no menu key and no mute key.
        let menu_key = InputId::Key(118);
        let mute = InputId::Key(127);
        let s = settings(&[
            ("key.keyboard.menu", "key.keyboard.c"),
            ("key.mouse.4", "key.keyboard.mute"),
            ("key.mouse.5", "key.keyboard.f3"),
        ]);
        let unsupported = [menu_key, mute];
        assert_eq!(resolve(&s, &Hotkeys::DEFAULT, &unsupported), [(MOUSE5, F3)]);
        let checks = check_rules(&s.rebinds, &Hotkeys::DEFAULT, &unsupported);
        assert_eq!(checks[0], Err(Unused::Unsupported(menu_key)));
        assert_eq!(checks[1], Err(Unused::Unsupported(mute)));
        assert_eq!(
            Unused::Unsupported(menu_key).note(),
            "キー 118 はこの版のゲームでは使えない"
        );
        // Its source is not taken: a later rule from it is used.
        let s = settings(&[
            ("key.mouse.4", "key.keyboard.mute"),
            ("key.mouse.4", "key.keyboard.f3"),
        ]);
        assert_eq!(resolve(&s, &Hotkeys::DEFAULT, &unsupported), [(MOUSE4, F3)]);
    }

    #[test]
    fn resolve_keeps_32_rules() {
        let names: Vec<(String, String)> = (4..50)
            .filter(|&sc| !refused_source(InputId::Key(sc)) && sc != 29)
            .map(|sc| (input_name(InputId::Key(sc)), "key.mouse.4".to_owned()))
            .collect();
        let rules: Vec<(&str, &str)> = names
            .iter()
            .map(|(a, b)| (a.as_str(), b.as_str()))
            .collect();
        let s = settings(&rules);
        // Of the 42 usable rules (J and K are the waypoint hotkeys), the first 32.
        assert_eq!(resolve(&s, &Hotkeys::DEFAULT, &[]).len(), MAX_REBINDS);
        let checks = check_rules(&s.rebinds, &Hotkeys::DEFAULT, &[]);
        assert!(checks.contains(&Err(Unused::TooMany)));
    }

    #[test]
    fn a_source_with_modifier_hotkeys_is_used_with_a_warning() {
        let keys = Hotkeys {
            zoom: Hotkey::parse("Shift+B").unwrap(),
            ..Hotkeys::DEFAULT
        };
        let s = settings(&[("key.keyboard.b", "key.keyboard.c")]);
        assert_eq!(resolve(&s, &keys, &[]), [(InputId::Key(5), C)]);
        assert_eq!(
            hotkey_warnings(InputId::Key(5), &keys),
            ["B は「ズーム」（Shift+B）にも使っている"]
        );
        // Left Ctrl is the menu's modifier (Ctrl+I).
        assert_eq!(
            hotkey_warnings(LCTRL, &keys),
            ["左 Ctrl は「メニューを開く」（Ctrl+I）の修飾キーにも使っている"]
        );
        assert!(hotkey_warnings(MOUSE4, &keys).is_empty());
    }

    #[test]
    fn rows_show_what_the_output_does_in_the_game() {
        let bindings = vec![
            (
                F3,
                vec![
                    "key.debug.overlay".to_owned(),
                    "key.debug.modifier".to_owned(),
                ],
            ),
            (C, vec![]),
        ];
        let rule = rebind("key.mouse.4", "key.keyboard.f3");
        assert_eq!(
            row_text(&rule, &Ok((MOUSE4, F3)), &bindings),
            "マウスのボタン4 → F3（オーバーレイの切り替え・デバッグ修飾キー）"
        );
        let rule = rebind("key.mouse.5", "key.keyboard.c");
        assert_eq!(
            row_text(&rule, &Ok((MOUSE5, C)), &bindings),
            "マウスのボタン5 → C"
        );
        let rule = rebind("key.keyboard.nope", "key.mouse.left");
        let unused = Unused::UnknownName(rule.from.clone());
        assert_eq!(
            row_text(&rule, &Err(unused.clone()), &bindings),
            "key.keyboard.nope → 左クリック"
        );
        assert_eq!(
            unused.note(),
            "「key.keyboard.nope」はキーの名前として読めないので使わない"
        );
    }

    /// Starts adding a rule and presses `source`.
    fn add_source(menu: &mut RebindMenu, s: &mut Settings, source: InputId) -> bool {
        menu.start();
        menu.on_captured(Captured::Input(source), s, &Hotkeys::DEFAULT)
    }

    #[test]
    fn adding_a_rule_takes_the_source_then_the_output() {
        let mut menu = RebindMenu::default();
        let mut s = Settings::default();
        assert!(
            add_source(&mut menu, &mut s, MOUSE4),
            "waits for the output"
        );
        assert_eq!(menu.adding, Adding::Target(MOUSE4));
        // The same key again: stays at the output.
        assert!(menu.on_captured(Captured::Input(MOUSE4), &mut s, &Hotkeys::DEFAULT));
        assert_eq!(menu.note.as_deref(), Some("同じキーには置き換えられない"));
        assert!(menu.on_captured(Captured::Input(LEFT_WIN), &mut s, &Hotkeys::DEFAULT));
        assert_eq!(
            menu.note.as_deref(),
            Some("左 Windows はゲームに送るキーにできない")
        );
        assert!(!menu.on_captured(Captured::Input(F3), &mut s, &Hotkeys::DEFAULT));
        assert!(!menu.adding());
        assert_eq!(menu.note, None);
        assert_eq!(s.rebinds, [rebind("key.mouse.4", "key.keyboard.f3")]);
        // An output chosen with a button.
        assert!(add_source(&mut menu, &mut s, CAPS));
        assert!(!menu.target(CAPS, InputId::Mouse(3), &mut s));
        assert_eq!(
            s.rebinds[1],
            rebind("key.keyboard.caps.lock", "key.mouse.right")
        );
    }

    #[test]
    fn refused_sources_end_the_capture_with_a_note() {
        let mut menu = RebindMenu::default();
        let mut s = settings(&[("key.mouse.4", "key.keyboard.f3")]);
        let cases = [
            (ESC, "Esc は置き換えるキーにできない"),
            (LEFT_WIN, "左 Windows は置き換えるキーにできない"),
            (InputId::Key(70), "PrintScreen は置き換えるキーにできない"),
            (
                InputId::Key(136),
                "カタカナ/ひらがな は置き換えるキーにできない",
            ),
            (MOUSE4, "マウスのボタン4 はもう置き換えている"),
            (Z, "Z は「ズーム」に使っているので置き換えられない"),
            (
                InputId::Key(13),
                "J は「ウェイポイントを記録」に使っているので置き換えられない",
            ),
        ];
        for (source, note) in cases {
            assert!(!add_source(&mut menu, &mut s, source), "{source:?}");
            assert!(!menu.adding());
            assert_eq!(menu.note.as_deref(), Some(note));
        }
        // I is free: the menu is on Ctrl+I.
        assert!(add_source(&mut menu, &mut s, InputId::Key(12)));
        assert_eq!(s.rebinds.len(), 1);
    }

    #[test]
    fn cancelling_or_another_capture_stops_adding() {
        let mut menu = RebindMenu::default();
        let mut s = Settings::default();
        menu.start();
        assert!(!menu.on_captured(Captured::Cancelled, &mut s, &Hotkeys::DEFAULT));
        assert!(!menu.adding());
        assert!(add_source(&mut menu, &mut s, MOUSE5));
        assert!(!menu.on_captured(Captured::Cancelled, &mut s, &Hotkeys::DEFAULT));
        assert!(!menu.adding());
        assert!(s.rebinds.is_empty());
        // The note of a refused key goes with the add: Esc at the output, 取り消し, or the
        // menu closing.
        assert!(add_source(&mut menu, &mut s, MOUSE5));
        assert!(menu.on_captured(Captured::Input(MOUSE5), &mut s, &Hotkeys::DEFAULT));
        assert!(menu.note.is_some());
        assert!(!menu.on_captured(Captured::Cancelled, &mut s, &Hotkeys::DEFAULT));
        assert_eq!(menu.note, None);
        assert!(add_source(&mut menu, &mut s, MOUSE5));
        assert!(menu.on_captured(Captured::Input(MOUSE5), &mut s, &Hotkeys::DEFAULT));
        menu.cancel();
        assert_eq!((menu.adding(), menu.note.as_deref()), (false, None));
        assert!(!add_source(&mut menu, &mut s, ESC));
        assert!(menu.note.is_some());
        menu.cancel();
        assert_eq!(menu.note, None);
        // Not adding: captures are someone else's.
        assert!(!menu.on_captured(Captured::Input(MOUSE5), &mut s, &Hotkeys::DEFAULT));
        assert!(s.rebinds.is_empty());
    }

    /// The section's contents in a window like the menu's, after a few frames.
    fn section_rect(menu: &mut RebindMenu, s: &mut Settings) -> egui::Rect {
        let ctx = egui::Context::default();
        let bindings = vec![(
            F3,
            vec![
                "key.debug.overlay".to_owned(),
                "key.debug.modifier".to_owned(),
            ],
        )];
        for _ in 0..4 {
            let raw = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1920.0, 1080.0),
                )),
                ..Default::default()
            };
            let mut output = ctx.run_ui(raw, |ui| {
                egui::Window::new("test")
                    .id(egui::Id::new("test"))
                    .resizable(false)
                    .show(ui.ctx(), |ui| {
                        let mut actions = RebindActions::default();
                        let note = Some("キーボードのキーは置き換えられない（理由）");
                        section_body(
                            ui,
                            menu,
                            s,
                            &Hotkeys::DEFAULT,
                            &bindings,
                            note,
                            &[],
                            &mut actions,
                        );
                    });
            });
            output.textures_delta.clear();
        }
        ctx.memory(|m| m.area_rect(egui::Id::new("test")))
            .expect("shown")
    }

    #[test]
    fn long_rows_wrap_instead_of_widening_the_menu() {
        let mut s = settings(&[
            ("key.mouse.4", "key.keyboard.f3"),
            (
                "key.keyboard.nope.nope.nope.nope",
                "key.keyboard.keypad.enter",
            ),
            ("key.keyboard.right.control", "key.keyboard.keypad.multiply"),
        ]);
        let mut menu = RebindMenu::default();
        let narrow = section_rect(&mut menu, &mut s);
        assert!(narrow.width() <= 360.0, "{narrow:?}");
        // Waiting for an output, with the click buttons.
        menu.adding = Adding::Target(InputId::Key(229));
        menu.note = Some("同じキーには置き換えられない".into());
        let rect = section_rect(&mut menu, &mut s);
        assert!(rect.width() <= 360.0, "{rect:?}");
    }

    #[test]
    fn the_copy_of_unreadable_entries_is_noted() {
        let mut s = Settings::default();
        assert_eq!(kept_note(&s), None);
        s.rebinds_kept_in = Some(
            std::path::Path::new("game")
                .join("reminedog")
                .join("settings.rebinds-1700000000.json"),
        );
        assert_eq!(
            kept_note(&s).as_deref(),
            Some(
                "settings.json の置き換えに読めない項目があったので settings.rebinds-1700000000.json に残した"
            )
        );
    }

    #[test]
    fn at_most_32_rules_are_added() {
        let mut menu = RebindMenu::default();
        let names: Vec<String> = (0..MAX_REBINDS)
            .map(|i| format!("key.keyboard.{}", 200 + i))
            .collect();
        let mut s = Settings {
            rebinds: names.iter().map(|n| rebind(n, "key.mouse.4")).collect(),
            ..Settings::default()
        };
        assert!(add_source(&mut menu, &mut s, CAPS));
        assert!(!menu.on_captured(Captured::Input(C), &mut s, &Hotkeys::DEFAULT));
        assert_eq!(s.rebinds.len(), MAX_REBINDS);
        assert_eq!(menu.note.as_deref(), Some("置き換えは 32 個まで"));
    }
}
