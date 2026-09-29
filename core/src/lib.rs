//! OS-independent logic shared by the platform hooks.
//!
//! - [`location`]: parsing the F3+C clipboard text (`/execute in ... run tp @s x y z yaw pitch`)
//! - [`waypoint`]: per-world waypoint files with atomic saves
//! - [`world`]: world ids and file names, `latest.log` parsing and tailing, singleplayer detection
//! - [`nav`]: distance and direction to a waypoint, nether coordinate conversion
//! - [`options`]: the `-agentpath:...=<options>` string
//! - [`gamedir`]: finding the game directory and reminedog's files in it
//! - [`logfile`]: a file backend for the `log` crate

pub mod gamedir;
pub mod location;
pub mod logfile;
pub mod nav;
pub mod options;
pub mod waypoint;
pub mod world;

pub use gamedir::{data_dir, detect_game_dir, waypoints_path};
pub use location::{Location, ParseError, parse_f3c};
pub use logfile::FileLogger;
pub use nav::{Bearing, Cardinal, bearing, convert_xz, wrap_degrees};
pub use options::AgentOptions;
pub use waypoint::{StoreError, Waypoint, WaypointStore};
pub use world::{
    LogEvent, LogTail, WorldId, WorldTracker, detect_singleplayer_world, parse_log_line,
    sanitize_file_component,
};
