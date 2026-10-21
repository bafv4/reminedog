//! プロセスに 1 つのエージェントの状態。各 OS の `Agent_OnLoad` が 1 回だけ入れる。

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

/// 状態を入れる。もう入っていれば false（エージェントが 2 回読み込まれた）。
pub fn init(globals: Globals) -> bool {
    GLOBALS.set(globals).is_ok()
}

/// `Agent_OnLoad` が動くまでは `None`。
pub fn globals() -> Option<&'static Globals> {
    GLOBALS.get()
}
