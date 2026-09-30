//! OS-independent logic shared by the platform hooks.
//!
//! - [`location`]: parsing the F3+C clipboard text (`/execute in ... run tp @s x y z yaw pitch`)
//! - [`waypoint`]: per-world waypoint files with atomic saves
//! - [`world`]: world ids and file names, `latest.log` parsing and tailing, singleplayer detection
//! - [`session`]: the world the player is in, and that world's waypoints
//! - [`nav`]: distance and direction to a waypoint, nether coordinate conversion
//! - [`keybinds`]: Minecraft's debug key bindings (F3+C) from `options.txt`
//! - [`options`]: the `-agentpath:...=<options>` string
//! - [`gamedir`]: finding the game directory and reminedog's files in it
//! - [`logfile`]: a file backend for the `log` crate
//! - [`settings`]: the user's settings file

pub mod gamedir;
pub mod keybinds;
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
    DebugKeys, UNBOUND, glfw_key, key_label, load_debug_keys, mapping_label, options_path,
    parse_debug_keys, sdl_keycode, sdl_scancode,
};
pub use location::{Location, ParseError, parse_f3c};
pub use logfile::FileLogger;
pub use nav::{Bearing, Cardinal, Guide, bearing, convert_xz, guide, wrap_degrees};
pub use options::AgentOptions;
pub use session::{WaypointBook, WorldState, WorldWatcher, unix_now};
pub use settings::{Settings, settings_path};
pub use waypoint::{StoreError, Waypoint, WaypointStore};
pub use world::{
    LogEvent, LogTail, WorldId, WorldTracker, detect_singleplayer_world, parse_log_line,
    sanitize_file_component,
};
