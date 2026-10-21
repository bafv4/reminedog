//! エージェントが持つ OpenGL のコンテキストの上での、オーバーレイの描画（ズームと UI）。
//!
//! 各 OS のフックは、ゲームの描画先で自前の GL のコンテキストを今のものにして
//! [`Overlay::render`] を呼び、本物のバッファのスワップの前に戻すので、ここではゲームの GL の
//! 状態に触れることはない。

mod browser;
mod font_metrics;
mod hotkey;
mod input;
mod overlay;
mod pointer;
mod rebind;
mod rebinds_ui;
mod waypoints;
mod zoom;

pub use browser::{BrowserCommand, BrowserPixels, BrowserState, BrowserView, PageLayout};
pub use egui::{Key, Modifiers, PointerButton};
pub use hotkey::{Hotkey, Trigger};
pub use input::{BrowserAction, Captured, HotkeyAction, InputRouter, Route};
pub use overlay::{
    FontSource, FrameInput, FrameOutput, FrameParams, Hotkeys, Overlay, OverlayError, StatusLine,
    ZoomView, gl_summary, hotkeys, hotkeys_quiet,
};
pub use pointer::{PointerSpeed, WINDOWS_DEFAULT_CURVE, parse_windows_curve};
pub use rebind::{Delivery, MAX_REBINDS, Output, Phase, RebindState};
pub use rebinds_ui::resolve;
pub use waypoints::{
    Notice, WaypointCommand, WaypointView, WorldLabel, cardinal_name, dimension_label,
    format_distance, format_xyz, navigation_text, row_guidance, turn_to,
};
pub use zoom::{middle_row, tall_size};
