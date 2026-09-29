//! Overlay rendering (zoom and UI) on top of an OpenGL context owned by the agent.
//!
//! The platform hook makes its own GL context current on the game's drawable, calls
//! [`Overlay::render`] and switches back before the real buffer swap, so nothing here
//! ever touches the game's GL state.

mod overlay;

pub use overlay::{FontSource, FrameParams, Overlay, OverlayError, StatusLine, gl_summary};
