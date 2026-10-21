//! 各 OS のフックが共有する、OS に依存しない処理。
//!
//! - [`browser`]：ゲーム内ブラウザのページへの入力（CDP の呼び出しとして）、アドレス欄、動画のスクリプト
//! - [`location`]：F3+C のクリップボードの文字（`/execute in ... run tp @s x y z yaw pitch`）の解析
//! - [`waypoint`]：アトミックに保存する、ワールドごとのウェイポイントのファイル
//! - [`world`]：ワールドの id とファイルの名前、`latest.log` の解析と追跡、シングルプレイの判定
//! - [`session`]：プレイヤーがいるワールドと、そのワールドのウェイポイント
//! - [`nav`]：ウェイポイントへの距離と方角、ネザーの座標の変換
//! - [`keybinds`]：Minecraft のキーの名前とキー（[`InputId`]）、`options.txt` のキーの割り当て
//!   （F3+C のものと、各キーの割り当て）
//! - [`options`]：`-agentpath:...=<options>` の文字列
//! - [`gamedir`]：ゲームフォルダと、その中の reminedog のファイルを探す
//! - [`logfile`]：`log` crate のファイルへの出力
//! - [`settings`]：利用者の設定ファイル

pub mod browser;
pub mod gamedir;
pub mod keybinds;
mod keytable;
pub mod location;
pub mod logfile;
pub mod nav;
pub mod options;
pub mod session;
pub mod settings;
pub mod waypoint;
pub mod world;

pub use gamedir::{data_dir, detect_game_dir, waypoints_path};
pub use keybinds::{
    DebugKeys, InputId, Modifier, Naming, SCANCODE_COUNT, Side, UNBOUND, bindings_by_key,
    glfw_button_of, glfw_key, glfw_key_of, input_by_name, input_from_glfw_button,
    input_from_glfw_key, input_label, input_name, key_label, load_debug_keys, mapping_label,
    modifier_kind, options_path, parse_debug_keys, sdl_keycode, sdl_keycode_of, sdl_scancode,
    win_scancode_of,
};
pub use location::{Location, ParseError, parse_f3c};
pub use logfile::FileLogger;
pub use nav::{Bearing, Cardinal, Guide, bearing, convert_xz, guide, wrap_degrees};
pub use options::AgentOptions;
pub use session::{SaveJob, ServerLabels, WaypointBook, WorldState, WorldWatcher, unix_now};
pub use settings::{Rebind, Settings, settings_path};
pub use waypoint::{StoreError, Waypoint, WaypointStore, write_file};
pub use world::{
    LogEvent, LogTail, WorldId, WorldTracker, detect_singleplayer_world,
    detect_singleplayer_world_since, level_name, might_be_world_line, parse_log_line,
    sanitize_file_component,
};
