//! OS-independent logic shared by the platform hooks.
//!
//! - [`browser`]: the in-game browser's page input (as CDP calls), address bar and video scripts
//! - [`location`]: parsing the F3+C clipboard text (`/execute in ... run tp @s x y z yaw pitch`)
//! - [`waypoint`]: per-world waypoint files with atomic saves
//! - [`world`]: world ids and file names, `latest.log` parsing and tailing, singleplayer detection
//! - [`session`]: the world the player is in, and that world's waypoints
//! - [`nav`]: distance and direction to a waypoint, nether coordinate conversion
//! - [`keybinds`]: Minecraft's key names and keys ([`InputId`]), and the key bindings in
//!   `options.txt` (F3+C's, and every key's mappings)
//! - [`options`]: the `-agentpath:...=<options>` string
//! - [`gamedir`]: finding the game directory and reminedog's files in it
//! - [`logfile`]: a file backend for the `log` crate
//! - [`settings`]: the user's settings file

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
