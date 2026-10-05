//! The user's settings: `<game_dir>/reminedog/settings.json`.
//!
//! Every field has a default, so older and newer files load: unknown fields are ignored and
//! missing ones take their defaults. Values out of range are clamped rather than refused, and
//! the rebinding fields, which hand edits are likely to break, drop a malformed value or entry
//! (with a warning, and a copy of the file to keep it from the next save) instead of the whole
//! file.

use std::cell::Cell;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::waypoint::{backup_path, copy_path, sync_parent_dir, tmp_path, unix_now, write_synced};

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
    /// Applies `rebinds` (the menu's switch for all of them).
    #[serde(deserialize_with = "lenient_rebinds_enabled")]
    pub rebinds_enabled: bool,
    /// Keys and mouse buttons that reach the game as other ones while in game.
    #[serde(deserialize_with = "lenient_rebinds")]
    pub rebinds: Vec<Rebind>,
    /// Where [`Settings::load`] copied the file because it dropped rebinding values or entries
    /// it could not read (the next save would lose them). Not saved.
    #[serde(skip)]
    pub rebinds_kept_in: Option<PathBuf>,
}

/// One rule of the key rebinding: `from` reaches the game as `to`. Both are 26.x key names,
/// e.g. `"key.mouse.4"` and `"key.keyboard.f3"` (`keybinds::input_by_name` with
/// `Naming::Modern`); names that are not a key are kept here and left out where the rules are
/// used.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rebind {
    pub from: String,
    pub to: String,
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
            rebinds_enabled: true,
            rebinds: Vec::new(),
            rebinds_kept_in: None,
        }
    }
}

thread_local! {
    /// A lenient reader dropped something since [`Settings::load`] started reading.
    static DROPPED: Cell<bool> = const { Cell::new(false) };
}

/// `rebinds_enabled`: `true` or `false`, also written as a string; anything else gives the
/// default.
fn lenient_rebinds_enabled<'de, D: Deserializer<'de>>(deserializer: D) -> Result<bool, D::Error> {
    Ok(match Value::deserialize(deserializer)? {
        Value::Bool(enabled) => enabled,
        Value::String(text) if text == "true" || text == "false" => text == "true",
        other => {
            let enabled = Settings::default().rebinds_enabled;
            log::warn!("settings: rebinds_enabled is not true or false ({other}); using {enabled}");
            DROPPED.set(true);
            enabled
        }
    })
}

/// `rebinds`: a value that is not a list gives none, and entries without a `from` and a `to`
/// string are dropped.
fn lenient_rebinds<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<Rebind>, D::Error> {
    let Value::Array(entries) = Value::deserialize(deserializer)? else {
        log::warn!("settings: rebinds is not a list; ignoring it");
        DROPPED.set(true);
        return Ok(Vec::new());
    };
    Ok(entries
        .into_iter()
        .filter_map(|entry| match Rebind::deserialize(&entry) {
            Ok(rebind) => Some(rebind),
            Err(e) => {
                log::warn!("settings: dropped the rebind {entry} ({e})");
                DROPPED.set(true);
                None
            }
        })
        .collect())
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
    /// destroy the user's edits, and the defaults are used. A file whose rebinding values or
    /// entries could not all be read is copied to `settings.rebinds-<unix>.json`
    /// ([`Settings::rebinds_kept_in`]), for the same reason.
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
        DROPPED.set(false);
        let parsed = serde_json::from_slice::<Settings>(&bytes);
        let dropped = DROPPED.replace(false);
        match parsed {
            Ok(mut settings) => {
                if dropped {
                    settings.rebinds_kept_in = keep_copy(path);
                }
                settings.sanitized()
            }
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

/// Copies the settings file to `settings.rebinds-<unix>.json` next to it; where, if it could.
/// A copy with the same content already there is reused, so loading the same file again (the
/// next start, an overlay rebuild) does not pile up copies.
fn keep_copy(path: &Path) -> Option<PathBuf> {
    if let Some(same) = existing_copy(path) {
        log::debug!(
            "settings: unreadable rebinding values in {} are kept in {} already",
            path.display(),
            same.display()
        );
        return Some(same);
    }
    let copy = copy_path(path, "rebinds", unix_now());
    match fs::copy(path, &copy) {
        Ok(_) => {
            log::warn!(
                "settings: some rebinding values in {} cannot be read; copied the file to {}",
                path.display(),
                copy.display()
            );
            Some(copy)
        }
        Err(e) => {
            log::warn!(
                "settings: some rebinding values in {} cannot be read, and the file cannot be copied to {} ({e})",
                path.display(),
                copy.display()
            );
            None
        }
    }
}

/// A `settings.rebinds-*.json` next to `path` with exactly its content.
fn existing_copy(path: &Path) -> Option<PathBuf> {
    let content = fs::read(path).ok()?;
    let stem = path.file_stem()?.to_str()?;
    let prefix = format!("{stem}.rebinds-");
    fs::read_dir(path.parent()?)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|copy| {
            copy.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(&prefix) && name.ends_with(".json"))
        })
        .find(|copy| fs::read(copy).is_ok_and(|bytes| bytes == content))
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
            rebinds_enabled: false,
            rebinds: vec![
                rebind("key.mouse.4", "key.keyboard.f3"),
                rebind("key.keyboard.caps.lock", "key.keyboard.left.control"),
            ],
            rebinds_kept_in: None,
        };
        settings.save(&path).unwrap();
        assert_eq!(Settings::load(&path), settings);
        assert!(!tmp_path(&path).exists());
    }

    fn rebind(from: &str, to: &str) -> Rebind {
        Rebind {
            from: from.into(),
            to: to.into(),
        }
    }

    #[test]
    fn rebinds_in_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let settings = Settings {
            rebinds: vec![rebind("key.mouse.4", "key.keyboard.f3")],
            ..Settings::default()
        };
        settings.save(&path).unwrap();
        let json: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(json["rebinds_enabled"], Value::Bool(true));
        assert_eq!(
            json["rebinds"],
            serde_json::json!([{"from": "key.mouse.4", "to": "key.keyboard.f3"}])
        );
        // Files from before the rebinding: on, with no rules.
        fs::write(&path, r#"{"zoom_key": "C"}"#).unwrap();
        let settings = Settings::load(&path);
        assert!(settings.rebinds_enabled);
        assert!(settings.rebinds.is_empty());
        // Names are kept as written, known or not.
        fs::write(
            &path,
            r#"{"rebinds_enabled": false, "rebinds": [{"from": "key.keyboard.nope", "to": "x", "note": 1}]}"#,
        )
        .unwrap();
        let settings = Settings::load(&path);
        assert!(!settings.rebinds_enabled);
        assert_eq!(settings.rebinds, [rebind("key.keyboard.nope", "x")]);
        assert_eq!(settings.rebinds_kept_in, None, "nothing dropped");
        // The switch written as a string.
        for (text, enabled) in [("false", false), ("true", true)] {
            fs::write(&path, format!(r#"{{"rebinds_enabled": "{text}"}}"#)).unwrap();
            let settings = Settings::load(&path);
            assert_eq!(settings.rebinds_enabled, enabled, "{text}");
            assert_eq!(settings.rebinds_kept_in, None);
        }
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1, "no copies");
    }

    /// The `settings.rebinds-*.json` copies in `dir`.
    fn copies(dir: &Path) -> Vec<PathBuf> {
        fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("settings.rebinds-"))
            })
            .collect()
    }

    #[test]
    fn malformed_rebinds_are_dropped_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(
            &path,
            r#"{
                "zoom_key": "C",
                "rebinds_enabled": "yes",
                "rebinds": [
                    {"from": "key.mouse.4", "to": "key.keyboard.f3"},
                    {"from": "key.mouse.5"},
                    {"from": 4, "to": "key.keyboard.c"},
                    "key.mouse.5",
                    null,
                    {"from": "key.keyboard.b", "to": "key.mouse.left"}
                ]
            }"#,
        )
        .unwrap();
        let original = fs::read(&path).unwrap();
        let settings = Settings::load(&path);
        // The file is not moved aside, and the other settings stay.
        assert!(path.exists());
        assert_eq!(settings.zoom_key, "C");
        assert!(settings.rebinds_enabled);
        assert_eq!(
            settings.rebinds,
            [
                rebind("key.mouse.4", "key.keyboard.f3"),
                rebind("key.keyboard.b", "key.mouse.left"),
            ]
        );
        // What was dropped is kept in a copy of the file, which saving does not touch.
        let copy = settings.rebinds_kept_in.clone().expect("copied");
        assert_eq!(copies(dir.path()), std::slice::from_ref(&copy));
        assert_eq!(fs::read(&copy).unwrap(), original);
        // Loading the same file again (the next start) reuses that copy.
        assert_eq!(Settings::load(&path).rebinds_kept_in, Some(copy.clone()));
        assert_eq!(copies(dir.path()).len(), 1);
        settings.save(&path).unwrap();
        assert_eq!(fs::read(&copy).unwrap(), original);
        let saved = Settings::load(&path);
        assert_eq!(saved.rebinds_kept_in, None, "nothing dropped any more");
        assert_eq!(copies(dir.path()).len(), 1);
        for rebinds in [r#"{"a": 1}"#, "null", r#""key.mouse.4""#, "4"] {
            fs::write(
                &path,
                format!(r#"{{"zoom_key": "C", "rebinds_enabled": null, "rebinds": {rebinds}}}"#),
            )
            .unwrap();
            let settings = Settings::load(&path);
            assert_eq!(settings.zoom_key, "C", "{rebinds}");
            assert!(settings.rebinds_enabled, "{rebinds}");
            assert!(settings.rebinds.is_empty(), "{rebinds}");
            assert!(settings.rebinds_kept_in.is_some(), "{rebinds}");
        }
        // Only the switch.
        fs::write(&path, r#"{"rebinds_enabled": "no"}"#).unwrap();
        let settings = Settings::load(&path);
        assert!(settings.rebinds_enabled);
        assert!(settings.rebinds_kept_in.is_some());
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
