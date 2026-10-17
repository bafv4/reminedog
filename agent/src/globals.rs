//! Process-wide agent state, set once by the platform's `Agent_OnLoad`.

use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Instant;

use reminedog_core::options::AgentOptions;

pub struct Globals {
    pub options: AgentOptions,
    pub game_dir: PathBuf,
    pub start: Instant,
    /// The agent library's version (the release's, else the crate's).
    pub version: &'static str,
    /// Identifies the build in logs ("local" for one that is not CI's).
    pub build_id: &'static str,
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
