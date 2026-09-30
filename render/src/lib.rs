//! Overlay rendering (zoom and UI) on top of an OpenGL context owned by the agent.
//!
//! The platform hook makes its own GL context current on the game's drawable, calls
//! [`Overlay::render`] and switches back before the real buffer swap, so nothing here
//! ever touches the game's GL state.

mod font_metrics;
mod hotkey;
mod input;
mod overlay;
mod pointer;
mod waypoints;
mod zoom;

pub use egui::{Key, Modifiers, PointerButton};
pub use hotkey::{Hotkey, Trigger};
pub use input::{Captured, HotkeyAction, InputRouter, Route};
pub use overlay::{
    FontSource, FrameInput, FrameOutput, FrameParams, Hotkeys, Overlay, OverlayError, StatusLine,
    ZoomView, gl_summary, hotkeys,
};
pub use pointer::{PointerSpeed, WINDOWS_DEFAULT_CURVE, parse_windows_curve};
pub use waypoints::{
    Notice, WaypointCommand, WaypointView, WorldLabel, cardinal_name, dimension_label,
    format_distance, format_xyz, navigation_text, row_guidance, turn_to,
};
pub use zoom::{middle_row, tall_size};
