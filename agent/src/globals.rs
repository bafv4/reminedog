//! Process-wide agent state, set once by the platform's `Agent_OnLoad`.

use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Instant;

use reminedog_core::options::AgentOptions;

pub struct Globals {
    pub options: AgentOptions,
    pub game_dir: PathBuf,
    pub start: Instant,
}

static GLOBALS: OnceLock<Globals> = OnceLock::new();

/// Sets the globals; false if they were set already (the agent was loaded twice).
pub fn init(globals: Globals) -> bool {
    GLOBALS.set(globals).is_ok()
}

/// `None` until `Agent_OnLoad` has run.
pub fn globals() -> Option<&'static Globals> {
    GLOBALS.get()
}
