//! オーバーレイの中のウェイポイント：各 OS のフックが渡すものと受け取るもの、メニューの欄、
//! 照準の上の通知、方角と距離の日本語の文言。

use std::sync::Arc;

use egui::RichText;
use reminedog_core::location::{OVERWORLD, THE_END, THE_NETHER};
use reminedog_core::{Cardinal, Guide, Location, Waypoint, guide};

use crate::overlay::Hotkeys;

/// メニューのための、プレイヤーがいるワールド。
#[derive(Clone, Debug, Default, PartialEq)]
pub enum WorldLabel {
    /// タイトル画面：まだワールドに入っていないか、内蔵サーバーが止まった。
    #[default]
    NotInWorld,
    /// 名前が分からなかったワールドの中。
    Unknown,
    /// 表示名でのシングルプレイのワールド。
    Singleplayer(String),
    /// アドレスでのサーバー。
    Multiplayer(String),
}

/// 各 OS のフックがメニューに出すウェイポイントの状態。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WaypointView {
    pub world: WorldLabel,
    /// サーバーで：そのアドレスの裏のワールド（ネットワークのロビーとサバイバル）を見分けるラベル。
    /// アドレスだけなら `None`。
    pub label: Option<String>,
    /// 各 OS のフックと共有する。フックは、ウェイポイントが変わったときだけ作り直す。
    pub waypoints: Arc<[Waypoint]>,
    /// 目的地。
    pub selected: Option<u64>,
    /// 最後に分かったプレイヤーの位置。
    pub location: Option<Location>,
    /// `location` の古さ（秒）。
    pub location_age: Option<f64>,
    /// ゲームがカーソルを捕まえている（Minecraft の画面は開いていない）。
    pub playing: bool,
    /// 位置の要求が途中。
    pub busy: bool,
    /// このワールドではゲームが F3+C を断った。要求はメニューの試し直しのボタンを待つ。
    pub blocked: bool,
    /// 出す警告（ウェイポイントのファイル、Minecraft のキーの割り当て）。
    pub problems: Vec<String>,
}

/// メニューがあってもなくても、照準の上に数秒出すメッセージ。
#[derive(Clone, Debug, PartialEq)]
pub struct Notice {
    pub text: String,
    /// 警告の色で出す。
    pub warn: bool,
    pub seconds: f64,
    /// 文字の前の矢印。上から右にこの角度だけ回す（[`turn_to`] を参照）。
    pub arrow: Option<f32>,
    /// まだ出ている同じ種類の通知は、この下に積まずに置き換える（古い位置からの目的地への道、
    /// 動画の古い位置）。
    pub replaces: Option<&'static str>,
}

impl Notice {
    /// 目的地への道の種類。
    pub const NAVIGATION: &'static str = "navigation";
    /// 動画の状態の種類。
    pub const MEDIA: &'static str = "media";

    pub fn new(text: impl Into<String>, warn: bool, seconds: f64) -> Self {
        Self {
            text: text.into(),
            warn,
            seconds,
            arrow: None,
            replaces: None,
        }
    }

    /// まだ出ている `kind` の通知を置き換える。
    pub fn replacing(mut self, kind: &'static str) -> Self {
        self.replaces = Some(kind);
        self
    }
}

/// メニューが各 OS のフックにウェイポイントについて頼むこと。
#[derive(Clone, Debug, PartialEq)]
pub enum WaypointCommand {
    /// プレイヤーの位置にウェイポイントを記録する。
    Record,
    /// プレイヤーの位置を更新する。
    Refresh,
    /// 目的地を選ぶ。`None` なら外す。
    Select(Option<u64>),
    Rename {
        id: u64,
        name: String,
    },
    Delete(u64),
    /// ゲームが F3+C を断ったワールドで、また F3+C を許す。
    Unblock,
    /// サーバーで：これからはこのラベルの下にウェイポイントを置く（`None`：アドレスだけ）。
    SetLabel(Option<String>),
}

/// メニューで打てるウェイポイントの名前のいちばん長い文字数。
const NAME_LIMIT: usize = 64;
/// サーバーのワールドのラベルのいちばん長い文字数。
const LABEL_LIMIT: usize = 32;
/// 「もう一度押すと削除」の 2 回目のクリックがこれ（秒）より近ければ、確認ではなく同じダブル
/// クリック。
const CONFIRM_DELAY: f64 = 0.5;
/// スクロールが始まるまでのウェイポイントの一覧の高さ（ポイント）。
const LIST_HEIGHT: f32 = 220.0;
/// これより近ければ（水平にブロックで）、プレイヤーはウェイポイントにいる。
const NEAR: f64 = 1.0;
/// 矢印：画面の左のものと、文字の行の横のもの（ポイント）。
const ARROW_HUD: f32 = 36.0;
const ARROW_TEXT: f32 = 16.0;

/// 日本語の 8 方位。
pub fn cardinal_name(cardinal: Cardinal) -> &'static str {
    match cardinal {
        Cardinal::N => "北",
        Cardinal::NE => "北東",
        Cardinal::E => "東",
        Cardinal::SE => "南東",
        Cardinal::S => "南",
        Cardinal::SW => "南西",
        Cardinal::W => "西",
        Cardinal::NW => "北西",
    }
}

/// 日本語のバニラのディメンション（`minecraft:` の namespace があってもなくても）。ほかの id は
/// そのまま。
pub fn dimension_label(id: &str) -> String {
    let full = if id.contains(':') {
        id.to_owned()
    } else {
        format!("minecraft:{id}")
    };
    match full.as_str() {
        OVERWORLD => "オーバーワールド".to_owned(),
        THE_NETHER => "ネザー".to_owned(),
        THE_END => "ジ・エンド".to_owned(),
        _ => id.to_owned(),
    }
}

/// 1 km 未満は `128 m`、そこからは `1.3 km`。
pub fn format_distance(meters: f64) -> String {
    let rounded = meters.round();
    if rounded < 1000.0 {
        format!("{} m", rounded as i64)
    } else {
        format!("{:.1} km", meters / 1000.0)
    }
}

/// 丸めた座標（例：`12, 64, -7`）。
pub fn format_xyz(x: f64, y: f64, z: f64) -> String {
    format!("{}, {}, {}", round(x), round(y), round(z))
}

/// 通知のためのウェイポイントへの方角と距離。プレイヤーが見ていた向きからの回転付き。
/// 例：`地点 3：北東 128 m（右に 35°・12 m 上）`。
pub fn navigation_text(name: &str, from: &Location, wp: &Waypoint) -> String {
    let (bearing, converted) = match guide(from, &wp.dimension, [wp.x, wp.y, wp.z]) {
        Guide::Bearing { bearing, converted } => (bearing, converted),
        Guide::OtherDimension => {
            return format!(
                "{name} は別のディメンション（{}）",
                dimension_label(&wp.dimension)
            );
        }
    };
    let mut text = name.to_owned();
    if let Some((x, z)) = converted {
        text.push_str(&converted_text(from, x, z));
    }
    text.push('：');
    let height = height_text(bearing.dy);
    if bearing.horizontal_distance < NEAR {
        text.push_str("ここ");
        if let Some(height) = height {
            text.push_str(&format!("（{height}）"));
        }
        return text;
    }
    let mut details = vec![turn_text(bearing.relative_yaw)];
    details.extend(height);
    text.push_str(&format!(
        "{} {}（{}）",
        cardinal_name(bearing.cardinal),
        format_distance(bearing.horizontal_distance),
        details.join("・")
    ));
    text
}

/// メニューの一覧のためのウェイポイントへの方角と距離。回転なし（位置を取ってからプレイヤーは
/// 見回している）。例：`北東 128 m・12 m 上`。
pub fn row_guidance(from: &Location, wp: &Waypoint) -> String {
    let (bearing, converted) = match guide(from, &wp.dimension, [wp.x, wp.y, wp.z]) {
        Guide::Bearing { bearing, converted } => (bearing, converted),
        Guide::OtherDimension => return "別のディメンション".to_owned(),
    };
    let mut parts = vec![if bearing.horizontal_distance < NEAR {
        "ここ".to_owned()
    } else {
        format!(
            "{} {}",
            cardinal_name(bearing.cardinal),
            format_distance(bearing.horizontal_distance)
        )
    }];
    parts.extend(height_text(bearing.dy));
    let mut text = parts.join("・");
    if let Some((x, z)) = converted {
        text.push_str(&converted_text(from, x, z));
    }
    text
}

/// 位置を取ったときにプレイヤーが見ていた方向から、ウェイポイントへの回転の角度（正 = 右）。
/// 矢印のため。別のディメンションやウェイポイントにいるなら `None`。プレイヤーの向きは F3+C から
/// しか分からないので、矢印は新しい位置でしか変わらない。
pub fn turn_to(from: &Location, wp: &Waypoint) -> Option<f32> {
    match guide(from, &wp.dimension, [wp.x, wp.y, wp.z]) {
        Guide::Bearing { bearing, .. } if bearing.horizontal_distance >= NEAR => {
            Some(bearing.relative_yaw)
        }
        _ => None,
    }
}

/// `center` の周りの `size` の四角の中の、矢印の頭（三角）と軸（四角）。上から時計回りに
/// `turn` 度向く。どちらも凸で時計回り（egui がそう塗るため）。
fn arrow_shape(center: egui::Pos2, size: f32, turn: f32) -> ([egui::Pos2; 3], [egui::Pos2; 4]) {
    let (sin, cos) = turn.to_radians().sin_cos();
    // 画面の y は下向きに増えるので、この回転は画面で時計回りになる。
    let at = |x: f32, y: f32| center + egui::vec2(x * cos - y * sin, x * sin + y * cos) * size;
    (
        [at(0.0, -0.5), at(0.38, -0.02), at(-0.38, -0.02)],
        [
            at(-0.13, -0.05),
            at(0.13, -0.05),
            at(0.13, 0.5),
            at(-0.13, 0.5),
        ],
    )
}

/// `ui` の `size` の四角の中の、上から右に `turn` 度回した矢印。
fn arrow(ui: &mut egui::Ui, turn: f32, size: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::Vec2::splat(size), egui::Sense::hover());
    paint_arrow(ui, rect.center(), size, turn);
}

fn paint_arrow(ui: &egui::Ui, center: egui::Pos2, size: f32, turn: f32) {
    let color = ui.visuals().strong_text_color();
    let (head, shaft) = arrow_shape(center, size, turn);
    let painter = ui.painter();
    painter.add(egui::Shape::convex_polygon(
        shaft.to_vec(),
        color,
        egui::Stroke::NONE,
    ));
    painter.add(egui::Shape::convex_polygon(
        head.to_vec(),
        color,
        egui::Stroke::NONE,
    ));
}

fn round(v: f64) -> i64 {
    // `as` は飽和し、-0.0 を 0 にする。
    v.round() as i64
}

/// `（ネザーでは 16, -1）`：プレイヤーのいるディメンションでのウェイポイントの x と z。
fn converted_text(from: &Location, x: f64, z: f64) -> String {
    format!(
        "（{}では {}, {}）",
        dimension_label(from.dimension_or_overworld()),
        round(x),
        round(z)
    )
}

/// どちらに回るか。正 = 右。
fn turn_text(relative_yaw: f32) -> String {
    let degrees = relative_yaw.abs();
    if degrees < 10.0 {
        "正面".to_owned()
    } else if degrees > 170.0 {
        "後ろ".to_owned()
    } else {
        let side = if relative_yaw > 0.0 { "右" } else { "左" };
        format!("{side}に {}°", degrees.round() as i32)
    }
}

/// `12 m 上`／`12 m 下`。3 ブロック以内ならなし。
fn height_text(dy: f64) -> Option<String> {
    (dy.abs() >= 3.0).then(|| {
        let side = if dy > 0.0 { "上" } else { "下" };
        format!("{} m {side}", round(dy.abs()))
    })
}

/// `15 秒前`, `3 分前`, `2 時間前`.
fn format_age(seconds: f64) -> String {
    let seconds = if seconds.is_finite() {
        seconds.max(0.0) as u64
    } else {
        0
    };
    match seconds {
        0..60 => format!("{seconds} 秒前"),
        60..3600 => format!("{} 分前", seconds / 60),
        _ => format!("{} 時間前", seconds / 3600),
    }
}

/// `現在地：12, 64, -7（オーバーワールド・15 秒前）`.
fn location_text(location: Option<&Location>, age: Option<f64>) -> String {
    let Some(location) = location else {
        return "現在地：未取得".to_owned();
    };
    let mut details = vec![dimension_label(location.dimension_or_overworld())];
    details.extend(age.map(format_age));
    format!(
        "現在地：{}（{}）",
        format_xyz(location.x, location.y, location.z),
        details.join("・")
    )
}

/// 画面の通知（古い順）。
#[derive(Default)]
pub(crate) struct NoticeList {
    shown: Vec<Shown>,
    next_serial: u64,
}

struct Shown {
    notice: Notice,
    serial: u64,
    until: f64,
    /// このフレームで足した（egui はまだ測っていない）。
    fresh: bool,
}

impl NoticeList {
    const MAX: usize = 3;
    /// 順に使う別々の egui の領域。`MAX` より多いので、出ている通知が領域を共有することはない。
    const AREAS: u64 = 8;

    /// 期限の切れた通知を捨て、新しい通知を `now` から `seconds` の間足し、新しい 3 つを残す。
    /// 新しい通知は同じ種類のものを置き換える（[`Notice::replaces`]）。まだ出ている通知と同じ文字の
    /// ものは、それを長く残すだけ。
    pub(crate) fn update(&mut self, new: Vec<Notice>, now: f64) {
        self.shown.retain(|shown| shown.until > now);
        for shown in &mut self.shown {
            shown.fresh = false;
        }
        for notice in new {
            if let Some(kind) = notice.replaces {
                self.shown
                    .retain(|shown| shown.notice.replaces != Some(kind));
            } else if let Some(same) = self
                .shown
                .iter_mut()
                .find(|shown| shown.notice.text == notice.text && shown.notice.warn == notice.warn)
            {
                same.until = same.until.max(now + notice.seconds);
                continue;
            }
            self.shown.push(Shown {
                until: now + notice.seconds,
                notice,
                serial: self.next_serial,
                fresh: true,
            });
            self.next_serial += 1;
        }
        let excess = self.shown.len().saturating_sub(Self::MAX);
        self.shown.drain(..excess);
    }

    #[cfg(test)]
    fn texts(&self) -> Vec<&str> {
        self.shown.iter().map(|s| s.notice.text.as_str()).collect()
    }
}

/// 画面の上から約 5 分の 1 から下に積む通知：ボスバーとプレイヤーの一覧の下、照準の上。
/// メニューの上にも。
pub(crate) fn draw_notices(ctx: &egui::Context, notices: &NoticeList) {
    const TOP: f32 = 0.22;
    const GAP: f32 = 6.0;
    let screen = ctx.content_rect();
    let mut y = screen.height() * TOP;
    for shown in &notices.shown {
        let id = egui::Id::new("reminedog-notice").with(shown.serial % NoticeList::AREAS);
        let response = egui::Area::new(id)
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::CENTER_TOP, [0.0, y])
            .interactable(false)
            .sizing_pass(shown.fresh)
            .show(ctx, |ui| {
                ui.set_max_width(screen.width() * 0.8);
                egui::Frame::popup(&ctx.global_style()).show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        if let Some(turn) = shown.notice.arrow {
                            arrow(ui, turn, ARROW_TEXT);
                        }
                        let text = RichText::new(&shown.notice.text);
                        let text = if shown.notice.warn {
                            text.color(ui.visuals().warn_fg_color)
                        } else {
                            text
                        };
                        ui.add(egui::Label::new(text).wrap());
                    });
                });
            })
            .response;
        y += response.rect.height() + GAP;
    }
}

/// メニューを閉じている間の、画面の左の目的地：名前と場所、最後に分かった位置からの道と、
/// そのとき見ていた方向に対する矢印。
pub(crate) fn draw_destination(ctx: &egui::Context, view: &WaypointView, keys: &Hotkeys) {
    let Some(wp) = view
        .selected
        .and_then(|id| view.waypoints.iter().find(|wp| wp.id == id))
    else {
        return;
    };
    let frame = egui::Frame::window(&ctx.global_style()).fill(egui::Color32::from_black_alpha(160));
    egui::Area::new(egui::Id::new("reminedog-destination"))
        .anchor(egui::Align2::LEFT_CENTER, [12.0, 0.0])
        .interactable(false)
        .show(ctx, |ui| {
            // 領域は最初の幅を保つ。行が長くなっても折り返してはならない（矢印や長い名前は後で来る）。
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
            frame.show(ui, |ui| {
                ui.horizontal(|ui| {
                    let turn = view.location.as_ref().and_then(|from| turn_to(from, wp));
                    // 矢印のための場所。矢印は、行の高さが分かってから描く。
                    let slot = turn.map(|_| {
                        ui.allocate_exact_size(egui::vec2(ARROW_HUD, 0.0), egui::Sense::hover())
                            .0
                    });
                    let lines = ui.vertical(|ui| destination_lines(ui, view, wp, keys));
                    if let (Some(turn), Some(slot)) = (turn, slot) {
                        let center = egui::pos2(slot.center().x, lines.response.rect.center().y);
                        paint_arrow(ui, center, ARROW_HUD, turn);
                    }
                });
            });
        });
}

fn destination_lines(ui: &mut egui::Ui, view: &WaypointView, wp: &Waypoint, keys: &Hotkeys) {
    ui.label(RichText::new(format!("目的地：{}", wp.name)).strong());
    ui.label(format!(
        "{}・{}",
        dimension_label(&wp.dimension),
        format_xyz(wp.x, wp.y, wp.z)
    ));
    let Some(from) = &view.location else {
        ui.label(
            RichText::new(format!(
                "現在地が未取得（{} で更新）",
                keys.navigate.label()
            ))
            .weak(),
        );
        return;
    };
    ui.label(row_guidance(from, wp));
    if let Some(age) = view.location_age {
        ui.label(RichText::new(format!("{}の位置と向きから", format_age(age))).weak());
    }
}

/// ウェイポイントの欄でのメニュー自身の状態。メニューを閉じたときとワールドが変わったときに
/// 消すので、名前の変更や削除が、同じ id の別のワールドのウェイポイントに当たることはない。
#[derive(Default)]
pub(crate) struct WaypointMenu {
    /// 名前を変えているウェイポイントと、ここまでに打った名前。
    renaming: Option<(u64, String)>,
    /// 次のフレームで、名前の欄にカーソルを置く。
    focus_name: bool,
    /// 削除のボタンを 1 回押したウェイポイントと、その時刻（秒）。
    confirm_delete: Option<(u64, f64)>,
    /// 書き換えているサーバーのワールドのラベル。
    label: Option<String>,
    world: WorldLabel,
}

impl WaypointMenu {
    pub(crate) fn sync(&mut self, ui_open: bool, view: &WaypointView) {
        if !ui_open || self.world != view.world {
            self.renaming = None;
            self.focus_name = false;
            self.confirm_delete = None;
            self.label = None;
            self.world.clone_from(&view.world);
        }
        let exists = |id: u64| view.waypoints.iter().any(|wp| wp.id == id);
        if self.renaming.as_ref().is_some_and(|(id, _)| !exists(*id)) {
            self.renaming = None;
        }
        if self.confirm_delete.is_some_and(|(id, _)| !exists(id)) {
            self.confirm_delete = None;
        }
    }
}

/// メニューの「ウェイポイント」の欄。
pub(crate) fn waypoint_section(
    ui: &mut egui::Ui,
    menu: &mut WaypointMenu,
    view: &WaypointView,
    keys: &Hotkeys,
    commands: &mut Vec<WaypointCommand>,
) {
    let warn = ui.visuals().warn_fg_color;
    ui.label(RichText::new("ウェイポイント").strong());
    ui.label(format!(
        "ゲーム中に {} で現在地を記録し、{} で目的地への方角と距離を出す",
        keys.waypoint.label(),
        keys.navigate.label()
    ));
    match &view.world {
        WorldLabel::NotInWorld => {
            ui.label("ワールドに入っていない（ワールドに入ると使える）");
        }
        WorldLabel::Unknown => {
            ui.label(
                RichText::new("ワールドを判定できていない（ワールドに入り直すと判定できる）")
                    .color(warn),
            );
        }
        WorldLabel::Singleplayer(name) => {
            ui.label(format!("ワールド：{name}"));
        }
        WorldLabel::Multiplayer(host) => {
            ui.label(format!("サーバー：{host}"));
            label_row(ui, menu, view, commands);
        }
    }
    // メニューはゲームのカーソルを捕まえたままにするので、これらはメニューを開いていても働く。
    let usable = view.playing && !view.busy;
    ui.horizontal(|ui| {
        if ui
            .add_enabled(usable, egui::Button::new("現在地を記録"))
            .clicked()
        {
            commands.push(WaypointCommand::Record);
        }
        if ui
            .add_enabled(usable, egui::Button::new("現在地を更新"))
            .clicked()
        {
            commands.push(WaypointCommand::Refresh);
        }
    });
    if !view.playing && view.world != WorldLabel::NotInWorld {
        ui.label(RichText::new("ゲームの画面を閉じると使える").weak());
    }
    ui.label(location_text(view.location.as_ref(), view.location_age));
    for problem in &view.problems {
        ui.label(RichText::new(problem).color(warn));
    }
    if view.blocked {
        ui.label(
            RichText::new("このワールドでは座標を取得できない（デバッグ情報が制限されている）")
                .color(warn),
        );
        if ui.button("もう一度試す").clicked() {
            commands.push(WaypointCommand::Unblock);
        }
    }

    if view.waypoints.is_empty() {
        if view.problems.is_empty() && view.world != WorldLabel::NotInWorld {
            ui.label(RichText::new("記録した地点はまだない").weak());
        }
        return;
    }
    ui.label(RichText::new("名前を押すと目的地になる").weak());
    if view.location.is_some() {
        ui.label(RichText::new("矢印は、座標を取ったときに向いていた方向が上").weak());
    }
    egui::ScrollArea::vertical()
        .id_salt("reminedog-waypoints")
        .max_height(LIST_HEIGHT)
        .show(ui, |ui| {
            for wp in view.waypoints.iter() {
                ui.push_id(wp.id, |ui| waypoint_row(ui, menu, view, wp, commands));
            }
        });
}

/// サーバーのワールドのラベル：それが何かと、変える欄。同じアドレスの裏のワールド（ネットワークの
/// ロビーとサバイバル）はログから見分けられない。ラベルがそれぞれのウェイポイントを分ける。
fn label_row(
    ui: &mut egui::Ui,
    menu: &mut WaypointMenu,
    view: &WaypointView,
    commands: &mut Vec<WaypointCommand>,
) {
    ui.horizontal_wrapped(|ui| {
        ui.label("ワールドのラベル：");
        let mut text = menu
            .label
            .clone()
            .unwrap_or_else(|| view.label.clone().unwrap_or_default());
        let edit = ui.add(
            egui::TextEdit::singleline(&mut text)
                .char_limit(LABEL_LIMIT)
                .desired_width(120.0)
                .hint_text("なし"),
        );
        let entered = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if edit.has_focus() {
            menu.label = Some(text.clone());
        }
        let label = Some(text.trim().to_owned()).filter(|label| !label.is_empty());
        let changed = label != view.label;
        if (ui
            .add_enabled(changed, egui::Button::new("変える"))
            .clicked()
            || entered)
            && changed
        {
            commands.push(WaypointCommand::SetLabel(label));
            menu.label = None;
        } else if !edit.has_focus() && !changed {
            menu.label = None;
        }
    });
    ui.label(
        RichText::new(
            "地点はアドレスとラベルごとに保存する。同じアドレスで別のワールドに入ったら（サーバー網のロビーとサバイバル など）、ラベルを変えると地点を分けられる",
        )
        .weak(),
    );
}

fn waypoint_row(
    ui: &mut egui::Ui,
    menu: &mut WaypointMenu,
    view: &WaypointView,
    wp: &Waypoint,
    commands: &mut Vec<WaypointCommand>,
) {
    let place = format!(
        "{}・{}",
        dimension_label(&wp.dimension),
        format_xyz(wp.x, wp.y, wp.z)
    );
    if let Some((_, name)) = menu.renaming.take_if(|(id, _)| *id == wp.id) {
        rename_row(ui, menu, wp, name, commands);
        ui.label(RichText::new(place).weak());
        return;
    }
    let selected = view.selected == Some(wp.id);
    ui.horizontal_wrapped(|ui| {
        let hover = if selected {
            "目的地から外す"
        } else {
            "目的地にする"
        };
        if ui
            .selectable_label(selected, &wp.name)
            .on_hover_text(hover)
            .clicked()
        {
            commands.push(WaypointCommand::Select((!selected).then_some(wp.id)));
        }
        ui.label(RichText::new(place).weak());
    });
    ui.horizontal_wrapped(|ui| {
        if let Some(from) = &view.location {
            if let Some(turn) = turn_to(from, wp) {
                arrow(ui, turn, ARROW_TEXT);
            }
            ui.label(row_guidance(from, wp));
        }
        if ui.button("名前を変える").clicked() {
            menu.renaming = Some((wp.id, wp.name.clone()));
            menu.focus_name = true;
            menu.confirm_delete = None;
        }
        let now = ui.input(|i| i.time);
        let confirming = menu.confirm_delete.filter(|&(id, _)| id == wp.id);
        let label = if confirming.is_some() {
            "もう一度押すと削除"
        } else {
            "削除"
        };
        if ui.button(label).clicked() {
            match confirming {
                // ダブルクリックの 2 回目のクリック：まだ確かめではない。
                Some((_, at)) if now - at < CONFIRM_DELAY => {}
                Some(_) => {
                    commands.push(WaypointCommand::Delete(wp.id));
                    menu.confirm_delete = None;
                }
                None => menu.confirm_delete = Some((wp.id, now)),
            }
        }
    });
}

/// 名前を変えているウェイポイントの名前の欄。Enter か「決定」で確定し（名前が空なら確定しない）、
/// 「取り消し」で取り消す。Esc はメニュー全体を閉じ、それでも取り消す。
fn rename_row(
    ui: &mut egui::Ui,
    menu: &mut WaypointMenu,
    wp: &Waypoint,
    mut name: String,
    commands: &mut Vec<WaypointCommand>,
) {
    let mut done = false;
    ui.horizontal(|ui| {
        let edit = ui.add(
            egui::TextEdit::singleline(&mut name)
                .char_limit(NAME_LIMIT)
                .hint_text("名前"),
        );
        if std::mem::take(&mut menu.focus_name) {
            edit.request_focus();
        }
        let valid = !name.trim().is_empty();
        let entered = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        let commit = ui.add_enabled(valid, egui::Button::new("決定")).clicked();
        if (commit || entered) && valid {
            let name = name.trim();
            if name != wp.name {
                commands.push(WaypointCommand::Rename {
                    id: wp.id,
                    name: name.to_owned(),
                });
            }
            done = true;
        } else if entered {
            // 空の名前で Enter：書き換えを続ける。
            menu.focus_name = true;
        }
        if ui.button("取り消し").clicked() {
            done = true;
        }
    });
    if !done {
        menu.renaming = Some((wp.id, name));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn location(dimension: &str, x: f64, y: f64, z: f64, yaw: f32) -> Location {
        Location {
            dimension: Some(dimension.to_owned()),
            x,
            y,
            z,
            yaw,
            pitch: 0.0,
        }
    }

    fn waypoint(id: u64, dimension: &str, x: f64, y: f64, z: f64) -> Waypoint {
        Waypoint {
            id,
            name: format!("地点 {id}"),
            dimension: dimension.to_owned(),
            x,
            y,
            z,
            created_unix: 0,
        }
    }

    #[test]
    fn cardinal_names() {
        let names: Vec<_> = [
            Cardinal::N,
            Cardinal::NE,
            Cardinal::E,
            Cardinal::SE,
            Cardinal::S,
            Cardinal::SW,
            Cardinal::W,
            Cardinal::NW,
        ]
        .into_iter()
        .map(cardinal_name)
        .collect();
        assert_eq!(
            names,
            ["北", "北東", "東", "南東", "南", "南西", "西", "北西"]
        );
    }

    #[test]
    fn dimension_labels() {
        assert_eq!(dimension_label(OVERWORLD), "オーバーワールド");
        assert_eq!(dimension_label(THE_NETHER), "ネザー");
        assert_eq!(dimension_label(THE_END), "ジ・エンド");
        assert_eq!(dimension_label("the_nether"), "ネザー");
        assert_eq!(dimension_label("mod:overworld"), "mod:overworld");
        assert_eq!(dimension_label("aether"), "aether");
    }

    #[test]
    fn distances() {
        assert_eq!(format_distance(0.2), "0 m");
        assert_eq!(format_distance(128.4), "128 m");
        assert_eq!(format_distance(999.4), "999 m");
        // 丸めると "1000 m" になってしまう。
        assert_eq!(format_distance(999.6), "1.0 km");
        assert_eq!(format_distance(1000.0), "1.0 km");
        assert_eq!(format_distance(12_345.0), "12.3 km");
    }

    #[test]
    fn coordinates_are_rounded_without_negative_zero() {
        assert_eq!(format_xyz(12.3, 64.0, -7.2), "12, 64, -7");
        assert_eq!(format_xyz(-0.4, 63.5, -7.5), "0, 64, -8");
    }

    #[test]
    fn navigation_in_the_same_dimension() {
        // 南を向いている（yaw 0）。ウェイポイントは北東で、左の後ろ。
        let from = location(OVERWORLD, 0.0, 64.0, 0.0, 0.0);
        let wp = waypoint(3, OVERWORLD, 90.0, 76.0, -90.0);
        assert_eq!(
            navigation_text("地点 3", &from, &wp),
            "地点 3：北東 127 m（左に 135°・12 m 上）"
        );
        assert_eq!(row_guidance(&from, &wp), "北東 127 m・12 m 上");
        // 東を向いている（yaw -90）：北東は 45 度左。
        let from = location(OVERWORLD, 0.0, 64.0, 0.0, -90.0);
        assert_eq!(
            navigation_text("地点 3", &from, &wp),
            "地点 3：北東 127 m（左に 45°・12 m 上）"
        );
        // 北を向いている（yaw 180）：北東は 45 度右。
        let from = location(OVERWORLD, 0.0, 70.0, 0.0, 180.0);
        assert_eq!(
            navigation_text("地点 3", &from, &wp),
            "地点 3：北東 127 m（右に 45°・6 m 上）"
        );
    }

    #[test]
    fn arrows_turn_with_the_way() {
        // 南を向いている（yaw 0）。ウェイポイントは北東で、135 度左。
        let from = location(OVERWORLD, 0.0, 64.0, 0.0, 0.0);
        let turn = turn_to(&from, &waypoint(3, OVERWORLD, 90.0, 76.0, -90.0)).unwrap();
        assert!((turn + 135.0).abs() < 1e-3, "{turn}");
        // ウェイポイントにいるときと別のディメンションでは矢印なし。
        assert_eq!(
            turn_to(&from, &waypoint(4, OVERWORLD, 0.5, 90.0, 0.0)),
            None
        );
        assert_eq!(
            turn_to(&from, &waypoint(5, THE_END, 90.0, 76.0, -90.0)),
            None
        );

        let center = egui::pos2(100.0, 100.0);
        let tip = |turn: f32| arrow_shape(center, 20.0, turn).0[0] - center;
        let close =
            |v: egui::Vec2, x: f32, y: f32| (v.x - x).abs() < 1e-3 && (v.y - y).abs() < 1e-3;
        // 上はまっすぐ前。正は右に回る（画面で時計回り。y は下向き）。
        assert!(close(tip(0.0), 0.0, -10.0), "{:?}", tip(0.0));
        assert!(close(tip(90.0), 10.0, 0.0), "{:?}", tip(90.0));
        assert!(close(tip(-90.0), -10.0, 0.0), "{:?}", tip(-90.0));
        assert!(close(tip(180.0), 0.0, 10.0), "{:?}", tip(180.0));
        // 尾は先の反対に残る。
        let (_, shaft) = arrow_shape(center, 20.0, 90.0);
        assert!(shaft[2].x < center.x && shaft[3].x < center.x);
    }

    #[test]
    fn turn_and_height_words() {
        assert_eq!(turn_text(9.9), "正面");
        assert_eq!(turn_text(-9.9), "正面");
        assert_eq!(turn_text(10.0), "右に 10°");
        assert_eq!(turn_text(-35.4), "左に 35°");
        assert_eq!(turn_text(170.0), "右に 170°");
        assert_eq!(turn_text(170.1), "後ろ");
        assert_eq!(turn_text(180.0), "後ろ");
        assert_eq!(height_text(2.9), None);
        assert_eq!(height_text(-2.9), None);
        assert_eq!(height_text(3.0).as_deref(), Some("3 m 上"));
        assert_eq!(height_text(-12.4).as_deref(), Some("12 m 下"));
        // まっすぐ前で同じ高さ：高さなし。
        let from = location(OVERWORLD, 0.0, 64.0, 0.0, 0.0);
        let wp = waypoint(1, OVERWORLD, 0.0, 65.0, 50.0);
        assert_eq!(navigation_text("家", &from, &wp), "家：南 50 m（正面）");
        assert_eq!(row_guidance(&from, &wp), "南 50 m");
    }

    #[test]
    fn standing_at_the_waypoint() {
        let from = location(OVERWORLD, 10.2, 64.0, 10.2, 45.0);
        let wp = waypoint(1, OVERWORLD, 10.5, 90.0, 10.5);
        assert_eq!(navigation_text("塔", &from, &wp), "塔：ここ（26 m 上）");
        assert_eq!(row_guidance(&from, &wp), "ここ・26 m 上");
        let wp = waypoint(1, OVERWORLD, 10.5, 64.0, 10.5);
        assert_eq!(navigation_text("塔", &from, &wp), "塔：ここ");
    }

    #[test]
    fn navigation_converts_between_overworld_and_nether() {
        // ネザーでは、オーバーワールドの (128, 70, -8) のウェイポイントは (16, -1)。
        let from = location(THE_NETHER, 16.0, 70.0, 19.0, 180.0);
        let wp = waypoint(3, OVERWORLD, 128.0, 70.0, -8.0);
        assert_eq!(
            navigation_text("地点 3", &from, &wp),
            "地点 3（ネザーでは 16, -1）：北 20 m（正面）"
        );
        assert_eq!(row_guidance(&from, &wp), "北 20 m（ネザーでは 16, -1）");
        // オーバーワールドから見たネザーのウェイポイント。
        let from = location(OVERWORLD, 0.0, 64.0, 0.0, 0.0);
        let wp = waypoint(4, THE_NETHER, 2.0, 40.0, 0.0);
        assert_eq!(
            navigation_text("地点 4", &from, &wp),
            "地点 4（オーバーワールドでは 16, 0）：東 16 m（左に 90°・24 m 下）"
        );
    }

    #[test]
    fn other_dimensions_have_no_direction() {
        let from = location(OVERWORLD, 0.0, 64.0, 0.0, 0.0);
        let wp = waypoint(3, THE_END, 100.0, 50.0, 0.0);
        assert_eq!(
            navigation_text("地点 3", &from, &wp),
            "地点 3 は別のディメンション（ジ・エンド）"
        );
        assert_eq!(row_guidance(&from, &wp), "別のディメンション");
    }

    #[test]
    fn location_line() {
        assert_eq!(location_text(None, None), "現在地：未取得");
        let here = location(OVERWORLD, 12.3, 64.0, -7.2, 0.0);
        assert_eq!(
            location_text(Some(&here), Some(15.7)),
            "現在地：12, 64, -7（オーバーワールド・15 秒前）"
        );
        assert_eq!(
            location_text(Some(&here), None),
            "現在地：12, 64, -7（オーバーワールド）"
        );
        assert_eq!(format_age(-1.0), "0 秒前");
        assert_eq!(format_age(59.9), "59 秒前");
        assert_eq!(format_age(60.0), "1 分前");
        assert_eq!(format_age(3599.0), "59 分前");
        assert_eq!(format_age(7200.0), "2 時間前");
        assert_eq!(format_age(f64::NAN), "0 秒前");
    }

    fn notice(text: &str, seconds: f64) -> Notice {
        Notice::new(text, false, seconds)
    }

    #[test]
    fn a_notice_replaces_its_kind_and_the_same_text_shows_once() {
        let mut list = NoticeList::default();
        let way = |text: &str| notice(text, 8.0).replacing(Notice::NAVIGATION);
        list.update(vec![way("北 120 m"), notice("記録した", 6.0)], 0.0);
        list.update(vec![way("北 100 m")], 2.0);
        assert_eq!(list.texts(), ["記録した", "北 100 m"]);
        // 同じ警告がまた来ても、出ているものを長く残すだけ。
        list.update(vec![notice("取得できない", 6.0)], 3.0);
        list.update(vec![notice("取得できない", 6.0)], 5.0);
        assert_eq!(list.texts(), ["記録した", "北 100 m", "取得できない"]);
        // 9 秒：最初の通知（6 秒）は消え、延ばした警告（11 秒まで）は残る。
        list.update(vec![], 9.0);
        assert_eq!(list.texts(), ["北 100 m", "取得できない"]);
    }

    #[test]
    fn notices_expire_after_their_seconds() {
        let mut list = NoticeList::default();
        list.update(vec![notice("a", 6.0)], 10.0);
        list.update(vec![notice("b", 8.0)], 12.0);
        assert_eq!(list.texts(), ["a", "b"]);
        list.update(vec![], 15.9);
        assert_eq!(list.texts(), ["a", "b"]);
        list.update(vec![], 16.0);
        assert_eq!(list.texts(), ["b"]);
        list.update(vec![], 20.0);
        assert!(list.texts().is_empty());
    }

    #[test]
    fn at_most_the_newest_three_notices_show() {
        let mut list = NoticeList::default();
        list.update(vec![notice("a", 6.0), notice("b", 6.0)], 0.0);
        list.update(
            vec![notice("c", 6.0), notice("d", 6.0), notice("e", 6.0)],
            1.0,
        );
        assert_eq!(list.texts(), ["c", "d", "e"]);
        assert!(list.shown.iter().all(|s| s.fresh));
        list.update(vec![notice("f", 6.0)], 2.0);
        assert_eq!(list.texts(), ["d", "e", "f"]);
        let fresh: Vec<_> = list.shown.iter().map(|s| s.fresh).collect();
        assert_eq!(fresh, [false, false, true]);
        // 出ている通知が egui の領域を共有することはない。
        let areas: Vec<_> = list
            .shown
            .iter()
            .map(|s| s.serial % NoticeList::AREAS)
            .collect();
        assert_eq!(areas, [3, 4, 5]);
    }

    fn view(world: WorldLabel, ids: &[u64]) -> WaypointView {
        WaypointView {
            world,
            waypoints: ids
                .iter()
                .map(|&id| waypoint(id, OVERWORLD, 0.0, 0.0, 0.0))
                .collect::<Vec<_>>()
                .into(),
            ..WaypointView::default()
        }
    }

    #[test]
    fn unfinished_edits_are_forgotten_on_close_and_world_change() {
        let world_a = WorldLabel::Singleplayer("A".into());
        let world_b = WorldLabel::Singleplayer("B".into());
        let mut menu = WaypointMenu::default();
        menu.sync(true, &view(world_a.clone(), &[1, 2]));
        menu.renaming = Some((1, "新しい名前".into()));
        menu.confirm_delete = Some((2, 0.0));
        menu.sync(true, &view(world_a.clone(), &[1, 2]));
        assert!(menu.renaming.is_some() && menu.confirm_delete.is_some());
        // メニューを閉じた。
        menu.sync(false, &view(world_a.clone(), &[1, 2]));
        assert!(menu.renaming.is_none() && menu.confirm_delete.is_none());
        // 同じ id の別のワールド。
        menu.sync(true, &view(world_a.clone(), &[1, 2]));
        menu.renaming = Some((1, "x".into()));
        menu.confirm_delete = Some((2, 0.0));
        menu.sync(true, &view(world_b, &[1, 2]));
        assert!(menu.renaming.is_none() && menu.confirm_delete.is_none());
        // ウェイポイントがなくなった。
        menu.renaming = Some((1, "x".into()));
        menu.confirm_delete = Some((1, 0.0));
        let world_b = WorldLabel::Singleplayer("B".into());
        menu.sync(true, &view(world_b, &[2]));
        assert!(menu.renaming.is_none() && menu.confirm_delete.is_none());
    }
}
