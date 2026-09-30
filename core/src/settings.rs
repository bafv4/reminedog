//! The user's settings: `<game_dir>/reminedog/settings.json`.
//!
//! Every field has a default, so older and newer files load: unknown fields are ignored and
//! missing ones take their defaults. Values out of range are clamped rather than refused.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::waypoint::{backup_path, sync_parent_dir, tmp_path, unix_now, write_synced};

/// Range of the zoom factor.
pub const ZOOM_FACTOR_RANGE: (f32, f32) = (1.5, 8.0);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Opens and closes the menu, e.g. `"Ctrl+I"`.
    pub menu_key: String,
    /// Zooms while held, e.g. `"Z"` or `"Mouse4"`.
    pub zoom_key: String,
    pub zoom_factor: f32,
    /// Has the game render at a taller resolution for real detail, instead of enlarging
    /// the pixels of its normal frame.
    pub zoom_high_res: bool,
    /// Filters the enlarged pixels when not zooming in high resolution.
    pub zoom_smooth: bool,
    /// Shows the status window while the menu is closed.
    pub show_status: bool,
    /// Records a waypoint at the player's position (through F3+C).
    pub waypoint_key: String,
    /// Refreshes the player's position and shows the way to the selected waypoint.
    pub navigate_key: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            menu_key: "Ctrl+I".into(),
            zoom_key: "Z".into(),
            zoom_factor: 4.0,
            zoom_high_res: true,
            zoom_smooth: true,
            show_status: false,
            waypoint_key: "J".into(),
            navigate_key: "K".into(),
        }
    }
}

impl Settings {
    /// Brings values from a hand-edited file back into range.
    pub fn sanitized(mut self) -> Self {
        let (lo, hi) = ZOOM_FACTOR_RANGE;
        self.zoom_factor = if self.zoom_factor.is_finite() {
            self.zoom_factor.clamp(lo, hi)
        } else {
            Settings::default().zoom_factor
        };
        self
    }

    /// Loads the settings at `path`. A missing file gives the defaults. A file that is not
    /// valid JSON is moved aside (`settings.corrupt-<unix>.json`) so the next save does not
    /// destroy the user's edits, and the defaults are used.
    pub fn load(path: &Path) -> Settings {
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Settings::default(),
            Err(e) => {
                log::warn!(
                    "cannot read {}: {e}; using default settings",
                    path.display()
                );
                return Settings::default();
            }
        };
        match serde_json::from_slice::<Settings>(&bytes) {
            Ok(settings) => settings.sanitized(),
            Err(e) => {
                let backup = backup_path(path, unix_now());
                match fs::rename(path, &backup) {
                    Ok(()) => log::warn!(
                        "settings file {} is invalid ({e}); moved it to {}",
                        path.display(),
                        backup.display()
                    ),
                    Err(re) => log::warn!(
                        "settings file {} is invalid ({e}) and cannot be moved aside ({re})",
                        path.display()
                    ),
                }
                Settings::default()
            }
        }
    }

    /// Writes the settings atomically (temporary file, then rename), creating the folder.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            fs::create_dir_all(dir)?;
        }
        let mut json = serde_json::to_vec_pretty(self).map_err(io::Error::other)?;
        json.push(b'\n');
        let tmp = tmp_path(path);
        let result = write_synced(&tmp, &json).and_then(|()| fs::rename(&tmp, path));
        if result.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        result?;
        sync_parent_dir(path);
        Ok(())
    }
}

/// `<game_dir>/reminedog/settings.json`.
pub fn settings_path(game_dir: &Path) -> PathBuf {
    crate::gamedir::data_dir(game_dir).join("settings.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_gives_defaults() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            Settings::load(&dir.path().join("settings.json")),
            Settings::default()
        );
    }

    #[test]
    fn round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("reminedog").join("settings.json");
        let settings = Settings {
            menu_key: "Ctrl+Shift+M".into(),
            zoom_key: "Mouse5".into(),
            zoom_factor: 6.0,
            zoom_high_res: false,
            zoom_smooth: false,
            show_status: true,
            waypoint_key: "Ctrl+J".into(),
            navigate_key: "Mouse4".into(),
        };
        settings.save(&path).unwrap();
        assert_eq!(Settings::load(&path), settings);
        assert!(!tmp_path(&path).exists());
    }

    #[test]
    fn partial_and_unknown_fields_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(&path, r#"{"zoom_key": "C", "from_the_future": 1}"#).unwrap();
        let settings = Settings::load(&path);
        assert_eq!(settings.zoom_key, "C");
        assert_eq!(settings.menu_key, "Ctrl+I");
        // Files from before the waypoint keys existed get their defaults.
        assert_eq!(settings.waypoint_key, "J");
        assert_eq!(settings.navigate_key, "K");
    }

    #[test]
    fn out_of_range_values_are_clamped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(&path, r#"{"zoom_factor": 100}"#).unwrap();
        assert_eq!(Settings::load(&path).zoom_factor, ZOOM_FACTOR_RANGE.1);
        fs::write(&path, r#"{"zoom_factor": 0.1}"#).unwrap();
        assert_eq!(Settings::load(&path).zoom_factor, ZOOM_FACTOR_RANGE.0);
    }

    #[test]
    fn invalid_file_is_moved_aside() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(&path, "{ not json").unwrap();
        assert_eq!(Settings::load(&path), Settings::default());
        assert!(!path.exists());
        let moved: Vec<_> = fs::read_dir(dir.path()).unwrap().collect();
        assert_eq!(moved.len(), 1);
    }

    #[test]
    fn settings_live_in_the_data_dir() {
        assert_eq!(
            settings_path(Path::new("game")),
            Path::new("game").join("reminedog").join("settings.json")
        );
    }
}
