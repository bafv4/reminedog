//! オーバーレイの上のゲーム内ブラウザ：利用者が置いた場所のページの絵、メニューを開いている
//! 間のその周りの窓（アドレス欄、移動、大きさの変更、ページ自身への入力）、メニューの欄。
//!
//! ブラウザは各 OS のフックの中で動く。こちらは [`BrowserCommand`]（その中には [`PageInput`]
//! としてのページへの入力もある）を送り、渡された絵（[`BrowserPixels`]）を描く。

use egui::{
    Color32, Event, EventFilter, Id, Key, LayerId, Order, Pos2, Rect, RichText, Sense, Stroke,
    StrokeKind, TextureId, Vec2, pos2, vec2,
};
use glow::HasContext as _;
use reminedog_core::Settings;
use reminedog_core::browser::{PageButton, PageInput, PageKey, PageModifiers, normalize_url};
use reminedog_core::settings::{BROWSER_OPACITY_RANGE, BROWSER_SEEK_RANGE, BROWSER_ZOOM_RANGE};

/// ブラウザがどこまで進んだか。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum BrowserState {
    /// 動いていない。
    #[default]
    Off,
    Starting,
    Ready,
    /// 始められなかったか、動かなくなった。メニューのための理由。
    Failed(String),
}

/// 各 OS のフックから来るブラウザの状態。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct BrowserView {
    pub state: BrowserState,
    /// 見せる（まだ始まる途中かもしれない）。
    pub shown: bool,
    pub url: String,
    pub title: String,
    pub can_go_back: bool,
    pub can_go_forward: bool,
}

/// オーバーレイがブラウザに頼むこと。
#[derive(Debug, Clone, PartialEq)]
pub enum BrowserCommand {
    /// ブラウザを見せる。動いていなければ始める。
    Show,
    /// 隠す。動画は止める。
    Hide,
    /// 止める。
    Quit,
    Navigate(String),
    Back,
    Forward,
    Reload,
    Layout(PageLayout),
    Input(PageInput),
}

/// ページの大きさと拡大率。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PageLayout {
    /// ピクセル。
    pub size: [u32; 2],
    /// ポイントあたりのピクセル：100 % のページの 1 CSS ピクセルが、オーバーレイの 1 ポイント。
    pub scale: f32,
    /// ページの拡大率（1.0 が 100 %）。
    pub zoom: f32,
}

/// ページの最新の絵。
pub struct BrowserPixels<'a> {
    /// ブラウザのどの起動のものか。`seq` と合わせて、新しい絵かが分かる。
    pub generation: u64,
    /// 0：まだ絵がない。
    pub seq: u64,
    /// ピクセル。
    pub size: [u32; 2],
    /// `size[0] * size[1]` のピクセル。上の行から：青、緑、赤、アルファ。
    pub bgra: &'a [u8],
}

/// ページのいちばん小さい大きさ（ポイント）。
const MIN_SIZE: Vec2 = vec2(160.0, 90.0);
/// 既定の位置に置くときの、ページと画面の端の間（ポイント）。
const MARGIN: f32 = 16.0;
/// 測るまでの、窓のタイトルバーとアドレス欄のためのページの上の場所。
const DEFAULT_OFFSET: Vec2 = vec2(7.0, 64.0);
/// 大きさが変わっている間、ページの大きさを送るのはせいぜいこの間隔（秒）。
const LAYOUT_INTERVAL: f64 = 0.1;
/// 時間と場所がこれより近いクリックは、ダブルクリックとして数える。
const DOUBLE_CLICK_SECONDS: f64 = 0.5;
const DOUBLE_CLICK_DISTANCE: f32 = 4.0;
/// ホイールの 1 刻みの CSS ピクセル（Windows の Chromium のもの）。
const WHEEL_STEP: f32 = 100.0;
/// ページが現れたときに、窓をページがあった場所に置くためのフレーム数。
const PLACING_TRIES: u8 = 3;
/// アドレス欄が出す、ページのアドレスの最大の文字数（ページはアドレスを数 MB にできる）。
const MAX_ADDRESS_CHARS: usize = 2048;
/// ポインターが出たときに、ページに伝える行き先（その見え方の外）。
const OUTSIDE: [f32; 2] = [-1.0, -1.0];

/// オーバーレイのコンテキストの GL のテクスチャとしての、ページの絵。
#[derive(Default)]
pub(crate) struct PageTexture {
    texture: Option<(glow::Texture, TextureId)>,
    size: [u32; 2],
    /// テクスチャの上の絵の世代と番号。出すものがなければ `None`。
    uploaded: Option<(u64, u64)>,
}

impl PageTexture {
    /// 新しい絵を上げる。`None`（各 OS のフックがこのフレームで取れなかった）なら前のものを残す。
    ///
    /// # Safety
    /// 描く者の GL のコンテキストが今のものであること。
    pub(crate) unsafe fn update(
        &mut self,
        painter: &mut egui_glow::Painter,
        pixels: Option<BrowserPixels<'_>>,
    ) {
        let Some(pixels) = pixels else {
            return;
        };
        let [width, height] = pixels.size;
        let complete = pixels.bgra.len() >= width as usize * height as usize * 4;
        if pixels.seq == 0 || width == 0 || height == 0 || !complete {
            self.uploaded = None;
            return;
        }
        if self.uploaded == Some((pixels.generation, pixels.seq)) {
            return;
        }
        let gl = painter.gl().clone();
        // SAFETY: 呼び出し側がコンテキストを今のものにした。スライスは絵の全部を持つ。
        unsafe {
            let texture = match self.texture {
                Some((texture, _)) => texture,
                None => match gl.create_texture() {
                    Ok(texture) => {
                        let id = painter.register_native_texture(texture);
                        self.texture = Some((texture, id));
                        self.size = [0, 0];
                        texture
                    }
                    Err(e) => {
                        log::warn!("browser: cannot create a texture: {e}");
                        return;
                    }
                },
            };
            gl.bind_texture(glow::TEXTURE_2D, Some(texture));
            gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 4);
            gl.pixel_store_i32(glow::UNPACK_ROW_LENGTH, 0);
            let data = glow::PixelUnpackData::Slice(Some(pixels.bgra));
            if self.size == pixels.size {
                gl.tex_sub_image_2d(
                    glow::TEXTURE_2D,
                    0,
                    0,
                    0,
                    width as i32,
                    height as i32,
                    glow::BGRA,
                    glow::UNSIGNED_BYTE,
                    data,
                );
            } else {
                for (parameter, value) in [
                    (glow::TEXTURE_MIN_FILTER, glow::LINEAR),
                    (glow::TEXTURE_MAG_FILTER, glow::LINEAR),
                    (glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE),
                    (glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE),
                ] {
                    gl.tex_parameter_i32(glow::TEXTURE_2D, parameter, value as i32);
                }
                gl.tex_image_2d(
                    glow::TEXTURE_2D,
                    0,
                    glow::RGBA8 as i32,
                    width as i32,
                    height as i32,
                    0,
                    glow::BGRA,
                    glow::UNSIGNED_BYTE,
                    data,
                );
                self.size = pixels.size;
            }
            gl.bind_texture(glow::TEXTURE_2D, None);
        }
        self.uploaded = Some((pixels.generation, pixels.seq));
    }

    /// ページの載ったテクスチャ。
    pub(crate) fn picture(&self) -> Option<TextureId> {
        self.uploaded.and(self.texture).map(|(_, id)| id)
    }
}

/// ブラウザの窓自身の状態。
#[derive(Default)]
pub(crate) struct BrowserMenu {
    /// 書き換えている間のアドレス欄の文字。
    address: Option<String>,
    /// 測った、窓の隅に対するページの隅の位置。
    offset: Option<Vec2>,
    /// 現れてから、ページがあった場所に窓を置く試み（ずれは描いてからしか分からない）。そこに
    /// 置けたら [`PLACING_TRIES`]。
    placing: u8,
    /// ページの上で下りて、まだ上がっていないボタン（[`PageButton::bit`]）。
    buttons: u8,
    /// ページの上で下りて、まだ上がっていないキー。
    keys: Vec<PageKey>,
    /// ページが最後にポインターを持っていた場所（CSS ピクセル）。
    pointer: [f32; 2],
    /// ページに最後に、ポインターが上にあると伝えた：出たときにそれを伝えなければならない
    /// （そうしないとホバーのメニューやプレビューが残る）。
    pointer_inside: bool,
    last_click: Option<Click>,
    /// 最後に送った大きさと、その時刻（秒）。
    sent: Option<(PageLayout, f64)>,
}

#[derive(Clone, Copy)]
struct Click {
    time: f64,
    pos: Pos2,
    button: PageButton,
    count: u32,
}

impl BrowserMenu {
    /// ページがまだ下ろしているものを離す：窓がなくなると（メニューを閉じるかブラウザを隠すと）、
    /// ページはもう入力を受け取らない。
    fn release(&mut self, commands: &mut Vec<BrowserCommand>) {
        self.release_keys(commands);
        self.leave(commands);
        for button in PageButton::ALL {
            if self.buttons & button.bit() != 0 {
                self.buttons &= !button.bit();
                commands.push(BrowserCommand::Input(PageInput::MouseUp {
                    pos: self.pointer,
                    button,
                    clicks: 1,
                    buttons: self.buttons,
                    modifiers: PageModifiers::default(),
                }));
            }
        }
    }

    /// ポインターが出たことをページに伝える。
    fn leave(&mut self, commands: &mut Vec<BrowserCommand>) {
        if std::mem::take(&mut self.pointer_inside) && self.buttons == 0 {
            commands.push(BrowserCommand::Input(PageInput::MouseMove {
                pos: OUTSIDE,
                buttons: 0,
                modifiers: PageModifiers::default(),
            }));
        }
    }

    /// ページがまだ下ろしているキーを離す：フォーカスを失うと、もうキーを受け取らない。
    fn release_keys(&mut self, commands: &mut Vec<BrowserCommand>) {
        for key in self.keys.drain(..) {
            commands.push(BrowserCommand::Input(PageInput::KeyUp {
                key,
                modifiers: PageModifiers::default(),
            }));
        }
    }

    /// 大きさが変わったら、ページの大きさを送る（せいぜい [`LAYOUT_INTERVAL`] ごと）。
    pub(crate) fn layout(
        &mut self,
        settings: &Settings,
        screen: Rect,
        ppp: f32,
        time: f64,
        commands: &mut Vec<BrowserCommand>,
    ) {
        let rect = page_rect(settings, screen);
        let size = (rect.size() * ppp).round();
        let layout = PageLayout {
            size: [size.x.max(1.0) as u32, size.y.max(1.0) as u32],
            scale: ppp,
            zoom: settings.browser_zoom,
        };
        let due = self
            .sent
            .is_none_or(|(sent, at)| sent != layout && (time - at >= LAYOUT_INTERVAL || time < at));
        if due {
            self.sent = Some((layout, time));
            commands.push(BrowserCommand::Layout(layout));
        }
    }
}

/// ページを置く場所（ポイント）：設定のとおり（画面の中に収めて）、または右上の隅。
pub(crate) fn page_rect(settings: &Settings, screen: Rect) -> Rect {
    let max = (screen.size() - Vec2::splat(MARGIN)).max(MIN_SIZE);
    let rect = match settings.browser_rect {
        Some([x, y, w, h]) => Rect::from_min_size(pos2(x, y), vec2(w, h)),
        None => {
            let width = (screen.width() * 0.4).max(MIN_SIZE.x);
            let size = vec2(width, width * 9.0 / 16.0);
            Rect::from_min_size(
                pos2(
                    screen.right() - size.x - MARGIN,
                    screen.top() + MARGIN + DEFAULT_OFFSET.y,
                ),
                size,
            )
        }
    };
    let size = rect.size().clamp(MIN_SIZE, max);
    let min = pos2(
        rect.min
            .x
            .clamp(screen.left(), (screen.right() - size.x).max(screen.left())),
        rect.min
            .y
            .clamp(screen.top(), (screen.bottom() - size.y).max(screen.top())),
    );
    Rect::from_min_size(min, size)
}

/// 見せている間のブラウザを描く：メニューを開いている間は窓、そうでなければページだけ。
pub(crate) fn browser_ui(
    ctx: &egui::Context,
    menu: &mut BrowserMenu,
    view: &BrowserView,
    settings: &mut Settings,
    picture: Option<TextureId>,
    ui_open: bool,
    commands: &mut Vec<BrowserCommand>,
) {
    if !(ui_open && view.shown) {
        // 窓なし：現れたときにまた置く。
        menu.placing = 0;
        menu.release(commands);
    }
    if !ui_open {
        menu.address = None;
    }
    if !view.shown {
        return;
    }
    let rect = page_rect(settings, ctx.content_rect());
    if !ui_open {
        let painter = ctx.layer_painter(LayerId::new(
            Order::Background,
            Id::new("reminedog-browser-page"),
        ));
        let alpha = (settings.browser_opacity.clamp(0.0, 1.0) * 255.0).round() as u8;
        paint_page(
            &painter,
            rect,
            picture,
            view,
            Color32::from_white_alpha(alpha),
        );
        return;
    }
    browser_window(ctx, menu, view, settings, picture, rect, commands);
}

fn browser_window(
    ctx: &egui::Context,
    menu: &mut BrowserMenu,
    view: &BrowserView,
    settings: &mut Settings,
    picture: Option<TextureId>,
    rect: Rect,
    commands: &mut Vec<BrowserCommand>,
) {
    let mut open = true;
    let mut window = egui::Window::new(window_title(ctx, view, rect.width()))
        .id(Id::new("reminedog-browser"))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .auto_sized()
        // タイトルバーだけで動かすなら、窓は `current_pos` より覚えた位置を保つ。ここでは
        // どちらにしても、ページとその隅が自分でドラッグを受け取る。
        .drag_area(egui::WindowDrag::Anywhere);
    if menu.placing < PLACING_TRIES {
        window = window.current_pos(rect.min - menu.offset.unwrap_or(DEFAULT_OFFSET));
    }
    let shown = window.show(ctx, |ui| {
        toolbar(ui, menu, view, rect.width(), commands);
        page(ui, menu, view, settings, picture, rect.size(), commands)
    });
    if !open {
        commands.push(BrowserCommand::Hide);
    }
    let Some((window_rect, Some(page_rect))) = shown.map(|s| (s.response.rect, s.inner)) else {
        return;
    };
    if menu.placing < PLACING_TRIES {
        // ページを元の場所に戻す（ずれは描いてからしか分からない）。
        if (page_rect.min - rect.min).length() > 0.5 {
            menu.offset = Some(page_rect.min - window_rect.min);
            menu.placing += 1;
            return;
        }
        menu.placing = PLACING_TRIES;
    }
    // 窓がページを動かす。大きさは設定のもの（隅がいま変えたかもしれない）。
    let size = match settings.browser_rect {
        Some([_, _, w, h]) => vec2(w, h),
        None => page_rect.size(),
    };
    let placed = [page_rect.min.x, page_rect.min.y, size.x, size.y];
    let moved = settings.browser_rect.is_none_or(|old| {
        old.iter()
            .zip(placed)
            .any(|(old, new)| (old - new).abs() > 0.01)
    });
    if moved {
        settings.browser_rect = Some(placed);
    }
}

/// "ブラウザ：<title>"。ページの幅に収まる長さにする（ウィンドウはタイトルに合わせて
/// 広がる）。
fn window_title(ctx: &egui::Context, view: &BrowserView, width: f32) -> String {
    let title = if view.title.is_empty() {
        &view.url
    } else {
        &view.title
    };
    if title.is_empty() {
        return "ブラウザ".to_owned();
    }
    // タイトルバーのフォントで測る。egui は中身に合わせた大きさの窓のタイトルを縮めないので、
    // 長いタイトルはタイトルバーを窓より広く描いてしまう。バーには閉じるボタン（フォントの行の
    // 高さの四角）と、タイトル、その周りの余白、ボタンの間の隙間もある。
    let room = title_room(ctx, width);
    let style = ctx.global_style();
    let font = egui::TextStyle::Heading.resolve(&style);
    let fits = |text: &str| {
        ctx.fonts_mut(|fonts| {
            fonts
                .layout_no_wrap(text.to_owned(), font.clone(), Color32::WHITE)
                .size()
                .x
        }) <= room
    };
    let titled = |chars: usize| format!("ブラウザ：{}", shorten(title, chars));
    let count = title.chars().count();
    if fits(&titled(count)) {
        return titled(count);
    }
    // "…" 付きで収まる最大の文字数。
    let (mut fit, mut over) = (0, count);
    while over - fit > 1 {
        let middle = (fit + over) / 2;
        if fits(&titled(middle)) {
            fit = middle;
        } else {
            over = middle;
        }
    }
    titled(fit)
}

/// `width` ポイントの幅の窓で、タイトルの文字が使ってよい幅。
fn title_room(ctx: &egui::Context, width: f32) -> f32 {
    let style = ctx.global_style();
    let font = egui::TextStyle::Heading.resolve(&style);
    let button = ctx.fonts_mut(|fonts| fonts.row_height(&font));
    width - button - style.spacing.item_spacing.x * 3.0 - 1.0
}

/// `text` の最初の `max` 文字。もっとあれば "…" を付ける（それより先は読まない）。
fn shorten(text: &str, max: usize) -> String {
    let mut chars = text.chars();
    let mut short: String = chars.by_ref().take(max).collect();
    if chars.next().is_some() {
        short.push('…');
    }
    short
}

/// 戻る、進む、読み込み直し、アドレス欄。
fn toolbar(
    ui: &mut egui::Ui,
    menu: &mut BrowserMenu,
    view: &BrowserView,
    width: f32,
    commands: &mut Vec<BrowserCommand>,
) {
    ui.horizontal(|ui| {
        let ready = view.state == BrowserState::Ready;
        if ui
            .add_enabled(ready && view.can_go_back, egui::Button::new("⏴"))
            .on_hover_text("戻る")
            .clicked()
        {
            commands.push(BrowserCommand::Back);
        }
        if ui
            .add_enabled(ready && view.can_go_forward, egui::Button::new("⏵"))
            .on_hover_text("進む")
            .clicked()
        {
            commands.push(BrowserCommand::Forward);
        }
        if ui
            .add_enabled(ready, egui::Button::new("⟳"))
            .on_hover_text("再読み込み")
            .clicked()
        {
            commands.push(BrowserCommand::Reload);
        }
        let shown = shorten(&view.url, MAX_ADDRESS_CHARS);
        let mut text = menu.address.clone().unwrap_or_else(|| shown.clone());
        let room = (width - (ui.min_rect().width() + ui.spacing().item_spacing.x)).max(60.0);
        // ブラウザが動いていなければ、アドレスを開く先がない。
        let usable = matches!(view.state, BrowserState::Ready | BrowserState::Starting);
        let edit = ui.add_enabled(
            usable,
            egui::TextEdit::singleline(&mut text)
                .id(Id::new("reminedog-browser-address"))
                .desired_width(room)
                .char_limit(MAX_ADDRESS_CHARS)
                .hint_text("URL か検索する言葉"),
        );
        if edit.has_focus() {
            menu.address = Some(text);
        } else if edit.lost_focus() {
            menu.address = None;
            if ui.input(|i| i.key_pressed(Key::Enter)) {
                // 元のままのアドレスで Enter なら、それをもう一度開く（縮めていれば全部を）。
                let url = if text == shown {
                    Some(view.url.clone()).filter(|url| !url.is_empty())
                } else {
                    normalize_url(&text)
                };
                if let Some(url) = url {
                    commands.push(BrowserCommand::Navigate(url));
                }
            }
        }
    });
}

/// ページ：その絵、その入力、大きさを変える隅。ページの場所を返す。
fn page(
    ui: &mut egui::Ui,
    menu: &mut BrowserMenu,
    view: &BrowserView,
    settings: &mut Settings,
    picture: Option<TextureId>,
    size: Vec2,
    commands: &mut Vec<BrowserCommand>,
) -> Rect {
    let page_id = Id::new("reminedog-browser-input");
    let (_, rect) = ui.allocate_space(size);
    let response = ui.interact(rect, page_id, Sense::click_and_drag());
    paint_page(ui.painter(), rect, picture, view, Color32::WHITE);

    // ページ自身の入力の上の、ページの大きさを変える隅。
    let grip = Rect::from_min_max(rect.max - Vec2::splat(16.0), rect.max);
    let grip_response = ui.interact(grip, Id::new("reminedog-browser-grip"), Sense::drag());
    paint_grip(
        ui.painter(),
        grip,
        grip_response.hovered() || grip_response.dragged(),
    );
    if grip_response.dragged() {
        let size = (rect.size() + grip_response.drag_delta()).max(MIN_SIZE);
        settings.browser_rect = Some([rect.min.x, rect.min.y, size.x, size.y]);
    }

    if !response.has_focus() {
        menu.release_keys(commands);
    }
    if view.state == BrowserState::Ready {
        let input = PageArea {
            rect,
            grip,
            layer: ui.layer_id(),
            zoom: settings.browser_zoom.max(0.01),
        };
        let events = ui.input(|i| i.events.clone());
        let time = ui.input(|i| i.time);
        let hovered = ui.rect_contains_pointer(rect) && !ui.rect_contains_pointer(grip);
        let pressed_on_page = page_input(
            ui.ctx(),
            menu,
            &input,
            &events,
            time,
            hovered,
            response.has_focus(),
            commands,
        );
        if pressed_on_page {
            response.request_focus();
        } else if response.has_focus()
            && events
                .iter()
                .any(|e| matches!(e, Event::PointerButton { pressed: true, .. }))
        {
            // ほかの場所をクリックした：キーはもうページのものではない。
            ui.memory_mut(|m| m.surrender_focus(page_id));
        }
        if response.has_focus() {
            ui.memory_mut(|m| {
                m.set_focus_lock_filter(
                    page_id,
                    EventFilter {
                        tab: true,
                        horizontal_arrows: true,
                        vertical_arrows: true,
                        escape: false,
                    },
                );
            });
        }
    }
    rect
}

/// ページの入力が行く場所。
struct PageArea {
    rect: Rect,
    /// 大きさを変える隅：ページのものではない。
    grip: Rect,
    layer: LayerId,
    zoom: f32,
}

impl PageArea {
    /// 画面の位置を、ページの CSS ピクセルで。
    fn css(&self, pos: Pos2) -> [f32; 2] {
        let v = (pos - self.rect.min) / self.zoom;
        [v.x, v.y]
    }
}

/// このフレームのイベントを、順に、ページへの入力にする。ページの上でボタンが下りたかを返す。
/// 2 つのほかのイベントの間の移動は、2 つ目の前に送る（移動の連なりごとに 1 つ）ので、ドラッグは
/// 押下と解放の間にページに届く。
#[allow(clippy::too_many_arguments)]
fn page_input(
    ctx: &egui::Context,
    menu: &mut BrowserMenu,
    area: &PageArea,
    events: &[Event],
    time: f64,
    hovered: bool,
    focused: bool,
    commands: &mut Vec<BrowserCommand>,
) -> bool {
    let mut pressed_on_page = false;
    let mut moved_to: Option<Pos2> = None;
    // 今押している修飾キー（移動は自分の修飾キーを持たない）。
    let modifiers = ctx.input(|i| page_modifiers(i.modifiers));
    let send = |commands: &mut Vec<BrowserCommand>, input| {
        commands.push(BrowserCommand::Input(input));
    };
    let flush_move = |menu: &mut BrowserMenu,
                      commands: &mut Vec<BrowserCommand>,
                      moved_to: &mut Option<Pos2>| {
        if let Some(pos) = moved_to.take() {
            menu.pointer = area.css(pos);
            menu.pointer_inside = true;
            send(
                commands,
                PageInput::MouseMove {
                    pos: menu.pointer,
                    buttons: menu.buttons,
                    modifiers,
                },
            );
        }
    };
    for event in events {
        match event {
            Event::PointerMoved(pos) => {
                let over = area.rect.contains(*pos) && ctx.layer_id_at(*pos) == Some(area.layer);
                if over || menu.buttons != 0 {
                    moved_to = Some(*pos);
                } else {
                    moved_to = None;
                    menu.leave(commands);
                }
            }
            Event::PointerGone => {
                moved_to = None;
                menu.leave(commands);
            }
            Event::WindowFocused(false) => {
                // 今押しているものの解放は来ない。
                moved_to = None;
                menu.release(commands);
            }
            Event::PointerButton {
                pos,
                button,
                pressed,
                modifiers: held,
            } => {
                let Some(button) = page_button(*button) else {
                    continue;
                };
                flush_move(menu, commands, &mut moved_to);
                let modifiers = page_modifiers(*held);
                if *pressed {
                    let on_page = area.rect.contains(*pos)
                        && !area.grip.contains(*pos)
                        && ctx.layer_id_at(*pos) == Some(area.layer);
                    if !on_page {
                        continue;
                    }
                    let count = match menu.last_click {
                        Some(last)
                            if last.button == button
                                && time - last.time < DOUBLE_CLICK_SECONDS
                                && last.pos.distance(*pos) < DOUBLE_CLICK_DISTANCE =>
                        {
                            last.count % 3 + 1
                        }
                        _ => 1,
                    };
                    menu.last_click = Some(Click {
                        time,
                        pos: *pos,
                        button,
                        count,
                    });
                    menu.buttons |= button.bit();
                    menu.pointer = area.css(*pos);
                    menu.pointer_inside = true;
                    pressed_on_page = true;
                    send(
                        commands,
                        PageInput::MouseDown {
                            pos: menu.pointer,
                            button,
                            clicks: count,
                            buttons: menu.buttons,
                            modifiers,
                        },
                    );
                } else if menu.buttons & button.bit() != 0 {
                    menu.buttons &= !button.bit();
                    menu.pointer = area.css(*pos);
                    let clicks = menu.last_click.map_or(1, |click| click.count);
                    send(
                        commands,
                        PageInput::MouseUp {
                            pos: menu.pointer,
                            button,
                            clicks,
                            buttons: menu.buttons,
                            modifiers,
                        },
                    );
                    if !area.rect.contains(*pos) {
                        menu.leave(commands);
                    }
                }
            }
            Event::MouseWheel {
                unit,
                delta,
                modifiers: held,
                ..
            } if hovered => {
                flush_move(menu, commands, &mut moved_to);
                let step = match unit {
                    egui::MouseWheelUnit::Point => 1.0,
                    egui::MouseWheelUnit::Line => WHEEL_STEP,
                    egui::MouseWheelUnit::Page => area.rect.height() / area.zoom,
                };
                let pos = ctx.pointer_latest_pos().unwrap_or(area.rect.center());
                send(
                    commands,
                    PageInput::Wheel {
                        pos: area.css(pos),
                        // egui の delta は中身を動かす。ページのものは見え方をスクロールする。
                        delta: [-delta.x * step, -delta.y * step],
                        modifiers: page_modifiers(*held),
                    },
                );
            }
            Event::Key {
                key,
                pressed,
                repeat,
                modifiers: held,
                ..
            } if focused => {
                let Some(page_key) = page_key(*key) else {
                    continue;
                };
                let modifiers = page_modifiers(*held);
                if !*pressed {
                    menu.keys.retain(|held| *held != page_key);
                } else if !menu.keys.contains(&page_key) {
                    menu.keys.push(page_key);
                }
                send(
                    commands,
                    if *pressed {
                        PageInput::KeyDown {
                            key: page_key,
                            repeat: *repeat,
                            modifiers,
                        }
                    } else {
                        PageInput::KeyUp {
                            key: page_key,
                            modifiers,
                        }
                    },
                );
            }
            Event::Text(text) if focused => send(commands, PageInput::Text(text.clone())),
            _ => {}
        }
    }
    flush_move(menu, commands, &mut moved_to);
    pressed_on_page
}

fn page_button(button: egui::PointerButton) -> Option<PageButton> {
    Some(match button {
        egui::PointerButton::Primary => PageButton::Left,
        egui::PointerButton::Secondary => PageButton::Right,
        egui::PointerButton::Middle => PageButton::Middle,
        egui::PointerButton::Extra1 => PageButton::Back,
        egui::PointerButton::Extra2 => PageButton::Forward,
    })
}

fn page_modifiers(modifiers: egui::Modifiers) -> PageModifiers {
    PageModifiers {
        alt: modifiers.alt,
        ctrl: modifiers.ctrl || modifiers.command,
        shift: modifiers.shift,
    }
}

/// egui のキー（US 配列）のページの名前（ページが使うキーについて）。
pub(crate) fn page_key(key: Key) -> Option<PageKey> {
    macro_rules! table {
        ($($key:ident => $vk:literal, $code:literal, $name:literal;)*) => {
            match key {
                $(Key::$key => Some(PageKey { vk: $vk, code: $code, key: $name }),)*
                _ => None,
            }
        };
    }
    table! {
        Backspace => 0x08, "Backspace", "Backspace";
        Tab => 0x09, "Tab", "Tab";
        Enter => 0x0d, "Enter", "Enter";
        Escape => 0x1b, "Escape", "Escape";
        Space => 0x20, "Space", " ";
        PageUp => 0x21, "PageUp", "PageUp";
        PageDown => 0x22, "PageDown", "PageDown";
        End => 0x23, "End", "End";
        Home => 0x24, "Home", "Home";
        ArrowLeft => 0x25, "ArrowLeft", "ArrowLeft";
        ArrowUp => 0x26, "ArrowUp", "ArrowUp";
        ArrowRight => 0x27, "ArrowRight", "ArrowRight";
        ArrowDown => 0x28, "ArrowDown", "ArrowDown";
        Insert => 0x2d, "Insert", "Insert";
        Delete => 0x2e, "Delete", "Delete";
        Num0 => 0x30, "Digit0", "0";
        Num1 => 0x31, "Digit1", "1";
        Num2 => 0x32, "Digit2", "2";
        Num3 => 0x33, "Digit3", "3";
        Num4 => 0x34, "Digit4", "4";
        Num5 => 0x35, "Digit5", "5";
        Num6 => 0x36, "Digit6", "6";
        Num7 => 0x37, "Digit7", "7";
        Num8 => 0x38, "Digit8", "8";
        Num9 => 0x39, "Digit9", "9";
        A => 0x41, "KeyA", "a";
        B => 0x42, "KeyB", "b";
        C => 0x43, "KeyC", "c";
        D => 0x44, "KeyD", "d";
        E => 0x45, "KeyE", "e";
        F => 0x46, "KeyF", "f";
        G => 0x47, "KeyG", "g";
        H => 0x48, "KeyH", "h";
        I => 0x49, "KeyI", "i";
        J => 0x4a, "KeyJ", "j";
        K => 0x4b, "KeyK", "k";
        L => 0x4c, "KeyL", "l";
        M => 0x4d, "KeyM", "m";
        N => 0x4e, "KeyN", "n";
        O => 0x4f, "KeyO", "o";
        P => 0x50, "KeyP", "p";
        Q => 0x51, "KeyQ", "q";
        R => 0x52, "KeyR", "r";
        S => 0x53, "KeyS", "s";
        T => 0x54, "KeyT", "t";
        U => 0x55, "KeyU", "u";
        V => 0x56, "KeyV", "v";
        W => 0x57, "KeyW", "w";
        X => 0x58, "KeyX", "x";
        Y => 0x59, "KeyY", "y";
        Z => 0x5a, "KeyZ", "z";
        F1 => 0x70, "F1", "F1";
        F2 => 0x71, "F2", "F2";
        F3 => 0x72, "F3", "F3";
        F4 => 0x73, "F4", "F4";
        F5 => 0x74, "F5", "F5";
        F6 => 0x75, "F6", "F6";
        F7 => 0x76, "F7", "F7";
        F8 => 0x77, "F8", "F8";
        F9 => 0x78, "F9", "F9";
        F10 => 0x79, "F10", "F10";
        F11 => 0x7a, "F11", "F11";
        F12 => 0x7b, "F12", "F12";
        Semicolon => 0xba, "Semicolon", ";";
        Equals => 0xbb, "Equal", "=";
        Comma => 0xbc, "Comma", ",";
        Minus => 0xbd, "Minus", "-";
        Period => 0xbe, "Period", ".";
        Slash => 0xbf, "Slash", "/";
        Backtick => 0xc0, "Backquote", "`";
        OpenBracket => 0xdb, "BracketLeft", "[";
        Backslash => 0xdc, "Backslash", "\\";
        CloseBracket => 0xdd, "BracketRight", "]";
        Quote => 0xde, "Quote", "'";
    }
}

/// 絵、絵がなければブラウザがしていること。失敗は、前の絵（もう変わらない）の上にも出す。
fn paint_page(
    painter: &egui::Painter,
    rect: Rect,
    picture: Option<TextureId>,
    view: &BrowserView,
    tint: Color32,
) {
    let failed = match &view.state {
        BrowserState::Failed(why) => Some(why.as_str()),
        _ => None,
    };
    let text = match (picture, failed) {
        (Some(texture), _) => {
            let uv = Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0));
            painter.image(texture, rect, uv, tint);
            failed
        }
        (None, Some(why)) => Some(why),
        (None, None) if view.state == BrowserState::Ready => Some("読み込み中…"),
        (None, None) => Some("ブラウザを起動しています…"),
    };
    if let Some(text) = text {
        painter.rect_filled(rect, 0.0, Color32::from_black_alpha(200));
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            text,
            egui::FontId::proportional(14.0),
            Color32::LIGHT_GRAY,
        );
    }
    painter.rect_stroke(
        rect,
        0.0,
        Stroke::new(1.0, Color32::from_black_alpha(160)),
        StrokeKind::Outside,
    );
}

/// 隅の 3 本の斜めの線。
fn paint_grip(painter: &egui::Painter, rect: Rect, hot: bool) {
    let color = if hot {
        Color32::WHITE
    } else {
        Color32::from_gray(160)
    };
    let shadow = Stroke::new(3.0, Color32::from_black_alpha(140));
    let stroke = Stroke::new(1.0, color);
    for i in 1..=3 {
        let d = i as f32 * 4.0;
        let line = [
            pos2(rect.right() - d, rect.bottom() - 1.0),
            pos2(rect.right() - 1.0, rect.bottom() - d),
        ];
        painter.line_segment(line, shadow);
        painter.line_segment(line, stroke);
    }
}

/// ブラウザのキーの上の、メニューの欄。
pub(crate) fn browser_section(
    ui: &mut egui::Ui,
    view: &BrowserView,
    settings: &mut Settings,
    commands: &mut Vec<BrowserCommand>,
) {
    ui.label("記事や動画をゲームの画面の上に表示する。移動と大きさの変更（右下の角）はメニューを開いている間だけ");
    ui.horizontal(|ui| {
        if view.shown {
            if ui.button("非表示にする").clicked() {
                commands.push(BrowserCommand::Hide);
            }
        } else if ui.button("表示する").clicked() {
            commands.push(BrowserCommand::Show);
        }
        if ui
            .add_enabled(view.state != BrowserState::Off, egui::Button::new("終了"))
            .on_hover_text("ブラウザを止めてメモリを空ける")
            .clicked()
        {
            commands.push(BrowserCommand::Quit);
        }
    });
    match &view.state {
        BrowserState::Off => {
            ui.label(RichText::new("止まっている（表示すると起動する）").weak());
        }
        BrowserState::Starting => {
            ui.label(RichText::new("起動中…").weak());
        }
        BrowserState::Ready if view.shown => {
            ui.label(RichText::new("表示中").weak());
        }
        BrowserState::Ready => {
            ui.label(RichText::new("非表示（動画は一時停止し、音は消している）").weak());
        }
        BrowserState::Failed(why) => {
            ui.label(RichText::new(why).color(ui.visuals().warn_fg_color));
        }
    }
    percent_slider(
        ui,
        &mut settings.browser_zoom,
        BROWSER_ZOOM_RANGE,
        "ページの拡大率",
    );
    percent_slider(
        ui,
        &mut settings.browser_opacity,
        BROWSER_OPACITY_RANGE,
        "不透明度",
    )
    .on_hover_text("メニューを閉じているときのページの不透明度");
    let (lo, hi) = BROWSER_SEEK_RANGE;
    ui.add(
        egui::Slider::new(&mut settings.browser_seek_seconds, lo..=hi)
            .step_by(1.0)
            .fixed_decimals(0)
            .suffix(" 秒")
            .text("巻き戻し・早送り"),
    );
}

/// 割合のスライダー（パーセントで出す）。
fn percent_slider(
    ui: &mut egui::Ui,
    value: &mut f32,
    (lo, hi): (f32, f32),
    text: &str,
) -> egui::Response {
    ui.add(
        egui::Slider::new(value, lo..=hi)
            .text(text)
            .custom_formatter(|v, _| format!("{:.0} %", v * 100.0))
            .custom_parser(|text| {
                text.trim_end_matches(['%', ' '])
                    .parse::<f64>()
                    .ok()
                    .map(|v| v / 100.0)
            }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen() -> Rect {
        Rect::from_min_size(Pos2::ZERO, vec2(1280.0, 720.0))
    }

    #[test]
    fn the_page_starts_in_the_top_right_corner() {
        let rect = page_rect(&Settings::default(), screen());
        assert_eq!(rect.width(), 512.0);
        assert_eq!(rect.height(), 288.0);
        assert_eq!(rect.right(), 1280.0 - MARGIN);
        assert!(rect.top() > 0.0);
    }

    #[test]
    fn the_page_stays_on_the_screen() {
        let settings = |rect| Settings {
            browser_rect: Some(rect),
            ..Settings::default()
        };
        // 右下にはみ出した（画面が小さくなった）。
        let rect = page_rect(&settings([1200.0, 700.0, 400.0, 300.0]), screen());
        assert_eq!(rect.max, pos2(1280.0, 720.0));
        assert_eq!(rect.size(), vec2(400.0, 300.0));
        // 小さすぎる、大きすぎる。
        let rect = page_rect(&settings([10.0, 10.0, 20.0, 20.0]), screen());
        assert_eq!(rect.size(), MIN_SIZE);
        let rect = page_rect(&settings([0.0, 0.0, 5000.0, 5000.0]), screen());
        assert_eq!(rect.size(), vec2(1280.0 - MARGIN, 720.0 - MARGIN));
    }

    #[test]
    fn the_layout_is_sent_when_it_changes_but_not_too_often() {
        let mut menu = BrowserMenu::default();
        let mut settings = Settings::default();
        let mut commands = Vec::new();
        menu.layout(&settings, screen(), 1.5, 10.0, &mut commands);
        assert_eq!(
            commands,
            vec![BrowserCommand::Layout(PageLayout {
                size: [768, 432],
                scale: 1.5,
                zoom: 1.0,
            })]
        );
        commands.clear();
        menu.layout(&settings, screen(), 1.5, 10.01, &mut commands);
        assert!(commands.is_empty(), "unchanged");
        settings.browser_zoom = 0.5;
        menu.layout(&settings, screen(), 1.5, 10.05, &mut commands);
        assert!(commands.is_empty(), "too soon");
        menu.layout(&settings, screen(), 1.5, 10.11, &mut commands);
        assert_eq!(commands.len(), 1);
    }

    #[test]
    fn keys_have_their_windows_codes() {
        assert_eq!(page_key(Key::PageDown), Some(PageKey::PAGE_DOWN));
        assert_eq!(page_key(Key::Enter), Some(PageKey::ENTER));
        let a = page_key(Key::A).unwrap();
        assert_eq!((a.vk, a.code, a.key), (0x41, "KeyA", "a"));
        let nine = page_key(Key::Num9).unwrap();
        assert_eq!((nine.vk, nine.code), (0x39, "Digit9"));
        assert_eq!(page_key(Key::F12).unwrap().vk, 0x7b);
        assert_eq!(page_key(Key::Copy), None);
    }

    #[test]
    fn long_titles_are_cut_to_the_page_width() {
        let ctx = egui::Context::default();
        // フォントは最初の 1 回の後にある。
        ctx.run_ui(egui::RawInput::default(), |_| {})
            .textures_delta
            .clear();
        let width = |text: &str| {
            let font = egui::TextStyle::Heading.resolve(&ctx.global_style());
            ctx.fonts_mut(|fonts| {
                fonts
                    .layout_no_wrap(text.to_owned(), font, Color32::WHITE)
                    .size()
                    .x
            })
        };
        let view = BrowserView {
            title: "Long title of a page ".repeat(20),
            ..BrowserView::default()
        };
        for page in [200.0, 360.0, 800.0] {
            let title = window_title(&ctx, &view, page);
            assert!(title.starts_with("ブラウザ：Long"), "{title}");
            assert!(title.ends_with('…'), "{title}");
            assert!(
                width(&title) <= title_room(&ctx, page),
                "{title} is wider than the title bar of {page}"
            );
        }
        // 広いページほど多くを見せる。
        assert!(window_title(&ctx, &view, 800.0).len() > window_title(&ctx, &view, 360.0).len());
        let short = BrowserView {
            title: "Wiki".into(),
            ..BrowserView::default()
        };
        assert_eq!(window_title(&ctx, &short, 360.0), "ブラウザ：Wiki");
        assert_eq!(
            window_title(&ctx, &BrowserView::default(), 360.0),
            "ブラウザ"
        );
    }

    /// いくつかのフレームの間、最後のフレームにこれらのイベントを入れて窓を動かす。そのフレームの命令。
    fn run(
        ctx: &egui::Context,
        menu: &mut BrowserMenu,
        settings: &mut Settings,
        view: &BrowserView,
        events: Vec<Event>,
    ) -> Vec<BrowserCommand> {
        for _ in 0..3 {
            frame(ctx, menu, settings, view, true, Vec::new());
        }
        frame(ctx, menu, settings, view, true, events)
    }

    /// メニューを開いた、または閉じた 1 フレーム。その命令。
    fn frame(
        ctx: &egui::Context,
        menu: &mut BrowserMenu,
        settings: &mut Settings,
        view: &BrowserView,
        ui_open: bool,
        events: Vec<Event>,
    ) -> Vec<BrowserCommand> {
        let mut commands = Vec::new();
        let raw = egui::RawInput {
            screen_rect: Some(screen()),
            events,
            ..Default::default()
        };
        let mut output = ctx.run_ui(raw, |ui| {
            browser_ui(ui.ctx(), menu, view, settings, None, ui_open, &mut commands);
        });
        output.textures_delta.clear();
        commands
    }

    fn primary(pos: Pos2, pressed: bool) -> Event {
        Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        }
    }

    fn key(key: Key, pressed: bool) -> Event {
        Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }
    }

    #[test]
    fn the_corner_resizes_the_page() {
        let ctx = egui::Context::default();
        let mut menu = BrowserMenu::default();
        let mut settings = Settings {
            browser_rect: Some([300.0, 200.0, 400.0, 225.0]),
            ..Settings::default()
        };
        let view = ready();
        run(&ctx, &mut menu, &mut settings, &view, Vec::new());
        let [x, y, w, h] = settings.browser_rect.unwrap();
        let grip = pos2(x + w - 8.0, y + h - 8.0);
        let end = grip + vec2(80.0, 50.0);
        for events in [
            vec![Event::PointerMoved(grip)],
            vec![primary(grip, true)],
            vec![Event::PointerMoved(end)],
            vec![primary(end, false)],
        ] {
            frame(&ctx, &mut menu, &mut settings, &view, true, events);
        }
        let [x2, y2, w2, h2] = settings.browser_rect.unwrap();
        assert_eq!([x2, y2], [x, y], "the page stays where it was");
        assert!(w2 > w + 40.0 && h2 > h + 20.0, "{w2}x{h2}");
    }

    #[test]
    fn closing_the_menu_lets_go_of_the_page() {
        let ctx = egui::Context::default();
        let mut menu = BrowserMenu::default();
        let mut settings = Settings {
            browser_rect: Some([300.0, 200.0, 400.0, 225.0]),
            ..Settings::default()
        };
        let view = ready();
        run(&ctx, &mut menu, &mut settings, &view, Vec::new());
        let at = pos2(350.0, 250.0);
        // クリックでページにキーを渡す。それから、ページの上でキーとボタンが下りる。
        frame(
            &ctx,
            &mut menu,
            &mut settings,
            &view,
            true,
            vec![
                Event::PointerMoved(at),
                primary(at, true),
                primary(at, false),
            ],
        );
        frame(
            &ctx,
            &mut menu,
            &mut settings,
            &view,
            true,
            vec![key(Key::ArrowRight, true), primary(at, true)],
        );
        let released = frame(&ctx, &mut menu, &mut settings, &view, false, Vec::new());
        assert!(
            released
                .iter()
                .any(|c| matches!(c, BrowserCommand::Input(PageInput::KeyUp { key, .. }) if key.code == "ArrowRight")),
            "{released:?}"
        );
        assert!(
            released.iter().any(|c| matches!(
                c,
                BrowserCommand::Input(PageInput::MouseUp {
                    button: PageButton::Left,
                    buttons: 0,
                    ..
                })
            )),
            "{released:?}"
        );
        // それらの本物の解放（今はゲームのもの）は、もう何も送らない。
        let later = frame(
            &ctx,
            &mut menu,
            &mut settings,
            &view,
            false,
            vec![key(Key::ArrowRight, false), primary(at, false)],
        );
        assert!(later.is_empty(), "{later:?}");
    }

    #[test]
    fn a_key_down_on_the_page_is_let_go_when_it_loses_the_focus() {
        let ctx = egui::Context::default();
        let mut menu = BrowserMenu::default();
        let mut settings = Settings {
            browser_rect: Some([300.0, 200.0, 400.0, 225.0]),
            ..Settings::default()
        };
        let view = ready();
        run(&ctx, &mut menu, &mut settings, &view, Vec::new());
        let at = pos2(350.0, 250.0);
        frame(
            &ctx,
            &mut menu,
            &mut settings,
            &view,
            true,
            vec![
                Event::PointerMoved(at),
                primary(at, true),
                primary(at, false),
            ],
        );
        frame(
            &ctx,
            &mut menu,
            &mut settings,
            &view,
            true,
            vec![key(Key::A, true)],
        );
        // ページの外のクリックで、ページからキーを取る。
        let away = pos2(50.0, 600.0);
        let mut commands = Vec::new();
        for events in [
            vec![Event::PointerMoved(away), primary(away, true)],
            vec![primary(away, false)],
        ] {
            commands.extend(frame(&ctx, &mut menu, &mut settings, &view, true, events));
        }
        assert!(
            commands
                .iter()
                .any(|c| matches!(c, BrowserCommand::Input(PageInput::KeyUp { key, .. }) if key.code == "KeyA")),
            "{commands:?}"
        );
    }

    fn ready() -> BrowserView {
        BrowserView {
            state: BrowserState::Ready,
            shown: true,
            url: "https://example.com/".into(),
            title: "Example".into(),
            ..BrowserView::default()
        }
    }

    #[test]
    fn the_window_puts_the_page_where_it_was() {
        let ctx = egui::Context::default();
        let mut menu = BrowserMenu::default();
        let mut settings = Settings {
            browser_rect: Some([300.0, 200.0, 400.0, 225.0]),
            ..Settings::default()
        };
        run(&ctx, &mut menu, &mut settings, &ready(), Vec::new());
        let [x, y, w, h] = settings.browser_rect.unwrap();
        assert!(
            (x - 300.0).abs() < 0.6 && (y - 200.0).abs() < 0.6,
            "{x} {y}"
        );
        assert_eq!((w, h), (400.0, 225.0));
    }

    #[test]
    fn shown_with_the_menu_open_the_page_goes_to_its_place() {
        let ctx = egui::Context::default();
        let mut menu = BrowserMenu::default();
        let mut settings = Settings::default();
        let hidden = BrowserView {
            shown: false,
            ..ready()
        };
        run(&ctx, &mut menu, &mut settings, &hidden, Vec::new());
        run(&ctx, &mut menu, &mut settings, &ready(), Vec::new());
        let [x, y, w, h] = settings.browser_rect.unwrap();
        let expected = page_rect(&Settings::default(), screen());
        assert!(
            (x - expected.min.x).abs() < 0.6 && (y - expected.min.y).abs() < 0.6,
            "{x} {y} (expected {expected:?})"
        );
        assert_eq!((w, h), (expected.width(), expected.height()));
    }

    #[test]
    fn opening_and_closing_the_menu_does_not_move_the_page() {
        let ctx = egui::Context::default();
        let mut menu = BrowserMenu::default();
        let mut settings = Settings {
            browser_rect: Some([300.0, 200.0, 400.0, 225.0]),
            ..Settings::default()
        };
        for _ in 0..3 {
            run(&ctx, &mut menu, &mut settings, &ready(), Vec::new());
            let mut commands = Vec::new();
            let raw = egui::RawInput {
                screen_rect: Some(screen()),
                ..Default::default()
            };
            let mut output = ctx.run_ui(raw, |ui| {
                browser_ui(
                    ui.ctx(),
                    &mut menu,
                    &ready(),
                    &mut settings,
                    None,
                    false,
                    &mut commands,
                );
            });
            output.textures_delta.clear();
            assert_eq!(settings.browser_rect, Some([300.0, 200.0, 400.0, 225.0]));
        }
    }

    #[test]
    fn clicks_and_wheel_on_the_page_reach_it_in_css_pixels() {
        let ctx = egui::Context::default();
        let mut menu = BrowserMenu::default();
        let mut settings = Settings {
            browser_rect: Some([300.0, 200.0, 400.0, 225.0]),
            browser_zoom: 2.0,
            ..Settings::default()
        };
        run(&ctx, &mut menu, &mut settings, &ready(), Vec::new());
        let [x, y, ..] = settings.browser_rect.unwrap();
        let at = pos2(x + 40.0, y + 20.0);
        let press = |pressed| Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        let commands = run(
            &ctx,
            &mut menu,
            &mut settings,
            &ready(),
            vec![Event::PointerMoved(at), press(true), press(false)],
        );
        let inputs: Vec<_> = commands
            .into_iter()
            .filter_map(|c| match c {
                BrowserCommand::Input(input) => Some(input),
                _ => None,
            })
            .collect();
        let near = |pos: [f32; 2]| (pos[0] - 20.0).abs() < 0.6 && (pos[1] - 10.0).abs() < 0.6;
        // 起きた順に：ページへの移動、次にクリック。
        assert!(
            matches!(inputs[0], PageInput::MouseMove { pos, buttons: 0, .. } if near(pos)),
            "{inputs:?}"
        );
        assert!(
            matches!(inputs[1], PageInput::MouseDown { pos, button: PageButton::Left, clicks: 1, buttons: 1, .. } if near(pos)),
            "{inputs:?}"
        );
        assert!(
            matches!(inputs[2], PageInput::MouseUp { buttons: 0, .. }),
            "{inputs:?}"
        );
        // 今はページがキーを持っている。
        let commands = run(
            &ctx,
            &mut menu,
            &mut settings,
            &ready(),
            vec![
                Event::Key {
                    key: Key::A,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                },
                Event::Text("a".into()),
                Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Line,
                    delta: vec2(0.0, -1.0),
                    phase: egui::TouchPhase::Move,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
        let inputs: Vec<_> = commands
            .into_iter()
            .filter_map(|c| match c {
                BrowserCommand::Input(input) => Some(input),
                _ => None,
            })
            .collect();
        assert!(
            matches!(&inputs[0], PageInput::KeyDown { key, .. } if key.code == "KeyA"),
            "{inputs:?}"
        );
        assert_eq!(inputs[1], PageInput::Text("a".into()));
        assert!(
            matches!(
                inputs[2],
                PageInput::Wheel {
                    delta: [0.0, 100.0],
                    ..
                }
            ),
            "{inputs:?}"
        );
    }

    #[test]
    fn a_drag_in_one_frame_moves_between_press_and_release_and_leaving_is_told() {
        let ctx = egui::Context::default();
        let mut menu = BrowserMenu::default();
        let mut settings = Settings {
            browser_rect: Some([300.0, 200.0, 400.0, 225.0]),
            ..Settings::default()
        };
        let view = ready();
        run(&ctx, &mut menu, &mut settings, &view, Vec::new());
        let a = pos2(350.0, 250.0);
        let b = pos2(380.0, 260.0);
        let commands = frame(
            &ctx,
            &mut menu,
            &mut settings,
            &view,
            true,
            vec![
                Event::PointerMoved(a),
                primary(a, true),
                Event::PointerMoved(b),
                primary(b, false),
            ],
        );
        let kinds: Vec<&str> = commands
            .iter()
            .filter_map(|c| match c {
                BrowserCommand::Input(PageInput::MouseMove { buttons: 1, .. }) => Some("drag"),
                BrowserCommand::Input(PageInput::MouseMove { .. }) => Some("move"),
                BrowserCommand::Input(PageInput::MouseDown { .. }) => Some("down"),
                BrowserCommand::Input(PageInput::MouseUp { .. }) => Some("up"),
                _ => None,
            })
            .collect();
        assert_eq!(kinds, ["move", "down", "drag", "up"]);
        // ページの外：ポインターが出たことを聞く。
        let away = pos2(50.0, 600.0);
        let commands = frame(
            &ctx,
            &mut menu,
            &mut settings,
            &view,
            true,
            vec![Event::PointerMoved(away)],
        );
        assert!(
            commands.iter().any(|c| matches!(
                c,
                BrowserCommand::Input(PageInput::MouseMove { pos: OUTSIDE, .. })
            )),
            "{commands:?}"
        );
    }

    #[test]
    fn enter_in_the_address_bar_opens_the_page() {
        let ctx = egui::Context::default();
        let mut menu = BrowserMenu::default();
        let mut settings = Settings::default();
        let view = BrowserView {
            url: String::new(),
            ..ready()
        };
        run(&ctx, &mut menu, &mut settings, &view, Vec::new());
        ctx.memory_mut(|m| m.request_focus(Id::new("reminedog-browser-address")));
        let enter = Event::Key {
            key: Key::Enter,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        let commands = run(
            &ctx,
            &mut menu,
            &mut settings,
            &view,
            vec![Event::Text("minecraft wiki".into())],
        );
        assert!(
            commands
                .iter()
                .all(|c| !matches!(c, BrowserCommand::Input(_))),
            "typing in the address bar is not the page's: {commands:?}"
        );
        let commands = run(&ctx, &mut menu, &mut settings, &view, vec![enter]);
        assert!(
            commands.contains(&BrowserCommand::Navigate(
                "https://www.google.com/search?q=minecraft+wiki".into()
            )),
            "{commands:?}"
        );
    }

    #[test]
    fn the_page_address_stays_out_of_the_settings() {
        // トークンを含みうる。各 OS のフックは、それを別の場所に置く。
        let ctx = egui::Context::default();
        let mut menu = BrowserMenu::default();
        let mut settings = Settings::default();
        let before = settings.browser_url.clone();
        run(&ctx, &mut menu, &mut settings, &ready(), Vec::new());
        assert_eq!(settings.browser_url, before);
    }
}
