//! Per-world waypoint storage backed by a JSON file.
//!
//! File format (pretty-printed UTF-8 JSON):
//! `{ "version": 1, "world": "<display name>", "next_id": 4, "waypoints": [ ... ] }`.
//! Unknown fields are ignored on load. Files with a newer `version` are refused rather than
//! overwritten, so a downgrade never destroys data written by a newer build.

use std::collections::HashSet;
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::location::Location;

/// The file format version this build reads and writes.
pub const FORMAT_VERSION: u64 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Waypoint {
    pub id: u64,
    pub name: String,
    pub dimension: String,
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub created_unix: i64,
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("I/O error on {}: {source}", .path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("waypoint file {} is corrupt: {reason}", .path.display())]
    Corrupt { path: PathBuf, reason: String },
    #[error(
        "waypoint file {} has format version {version}, newer than the supported {FORMAT_VERSION}",
        .path.display()
    )]
    UnsupportedVersion { path: PathBuf, version: u64 },
}

#[derive(Serialize)]
struct FileOut<'a> {
    version: u64,
    world: &'a str,
    next_id: u64,
    waypoints: &'a [Waypoint],
}

#[derive(Deserialize)]
struct FileIn {
    #[serde(default)]
    next_id: u64,
    #[serde(default)]
    waypoints: Vec<Waypoint>,
}

/// The waypoints of one world, loaded from and saved to a single JSON file.
#[derive(Debug, Clone)]
pub struct WaypointStore {
    path: PathBuf,
    world: String,
    next_id: u64,
    waypoints: Vec<Waypoint>,
}

impl WaypointStore {
    /// Loads the store at `path`. A missing file gives an empty store; the file is not
    /// created until [`save`](Self::save). An unparsable file is an error and is left untouched.
    pub fn open(path: impl Into<PathBuf>, world_display: &str) -> Result<Self, StoreError> {
        let path = path.into();
        let mut store = Self::empty(path, world_display);
        if let Some(file) = load(&store.path)? {
            store.set_contents(file);
        }
        Ok(store)
    }

    /// Like [`open`](Self::open), but a corrupt file is renamed to
    /// `<stem>.corrupt-<unix>.json` next to it and an empty store is returned together with
    /// the backup path. Files with an unsupported version are still refused.
    pub fn open_or_backup(
        path: impl Into<PathBuf>,
        world_display: &str,
    ) -> Result<(Self, Option<PathBuf>), StoreError> {
        let path = path.into();
        match Self::open(path.clone(), world_display) {
            Ok(store) => Ok((store, None)),
            Err(StoreError::Corrupt { reason, .. }) => {
                let backup = backup_path(&path, unix_now());
                fs::rename(&path, &backup).map_err(|source| StoreError::Io {
                    path: path.clone(),
                    source,
                })?;
                log::warn!(
                    "waypoint file {} is corrupt ({reason}); moved it to {}",
                    path.display(),
                    backup.display()
                );
                Ok((Self::empty(path, world_display), Some(backup)))
            }
            Err(e) => Err(e),
        }
    }

    fn empty(path: PathBuf, world_display: &str) -> Self {
        Self {
            path,
            world: world_display.to_owned(),
            next_id: 1,
            waypoints: Vec::new(),
        }
    }

    fn set_contents(&mut self, file: FileIn) {
        self.waypoints = file.waypoints;
        let max_id = self.waypoints.iter().map(|w| w.id).max();
        self.next_id = file
            .next_id
            .max(max_id.map_or(1, |id| id.saturating_add(1)))
            .max(1);
        // Hand-edited files may repeat ids; give duplicates fresh ones to keep ids unique.
        let mut seen = HashSet::new();
        for i in 0..self.waypoints.len() {
            let old = self.waypoints[i].id;
            if !seen.insert(old) {
                let id = self.allocate_id();
                log::warn!(
                    "{}: duplicate waypoint id {old} renumbered to {id}",
                    self.path.display()
                );
                self.waypoints[i].id = id;
                seen.insert(id);
            }
        }
    }

    /// Hands out `next_id`. Once ids reach `u64::MAX` (only possible with a hand-edited file)
    /// uniqueness wins over never reusing an id: the lowest free id is returned instead.
    fn allocate_id(&mut self) -> u64 {
        let id = self.next_id;
        if id < u64::MAX {
            self.next_id = id + 1;
            return id;
        }
        let used: HashSet<u64> = self.waypoints.iter().map(|w| w.id).collect();
        if !used.contains(&id) {
            return id;
        }
        (1..u64::MAX)
            .find(|id| !used.contains(id))
            .expect("fewer waypoints than ids")
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The display name written to the file's `world` field.
    pub fn world(&self) -> &str {
        &self.world
    }

    pub fn set_world(&mut self, world_display: &str) {
        world_display.clone_into(&mut self.world);
    }

    /// The id the next [`add`](Self::add) will use (unless ids ran up to `u64::MAX`).
    pub fn next_id(&self) -> u64 {
        self.next_id
    }

    /// Waypoints in insertion order.
    pub fn waypoints(&self) -> &[Waypoint] {
        &self.waypoints
    }

    pub fn get(&self, id: u64) -> Option<&Waypoint> {
        self.waypoints.iter().find(|w| w.id == id)
    }

    /// Adds a waypoint at `location`. The name is trimmed; an empty name becomes
    /// `Waypoint <id>`. A missing dimension means the overworld. Non-finite coordinates
    /// (which JSON cannot represent) are stored as 0.
    pub fn add(&mut self, name: &str, location: &Location, now_unix: i64) -> &Waypoint {
        let id = self.allocate_id();
        let finite = |v: f64| if v.is_finite() { v } else { 0.0 };
        self.waypoints.push(Waypoint {
            id,
            name: normalize_name(name, id),
            dimension: location.dimension_or_overworld().to_owned(),
            x: finite(location.x),
            y: finite(location.y),
            z: finite(location.z),
            created_unix: now_unix,
        });
        self.waypoints.last().expect("just pushed")
    }

    /// Renames a waypoint with the same normalization as [`add`](Self::add).
    /// Returns `false` if there is no waypoint with that id.
    pub fn rename(&mut self, id: u64, name: &str) -> bool {
        match self.waypoints.iter_mut().find(|w| w.id == id) {
            Some(waypoint) => {
                waypoint.name = normalize_name(name, id);
                true
            }
            None => false,
        }
    }

    pub fn remove(&mut self, id: u64) -> Option<Waypoint> {
        let index = self.waypoints.iter().position(|w| w.id == id)?;
        Some(self.waypoints.remove(index))
    }

    /// Writes the store atomically: `<file>.tmp` in the same directory is written and
    /// synced, then renamed over the target. Parent directories are created as needed.
    pub fn save(&self) -> Result<(), StoreError> {
        let io_error = |path: &Path| {
            let path = path.to_owned();
            move |source| StoreError::Io { path, source }
        };
        if let Some(dir) = self.path.parent().filter(|d| !d.as_os_str().is_empty()) {
            fs::create_dir_all(dir).map_err(io_error(dir))?;
        }
        let mut json = serde_json::to_vec_pretty(&FileOut {
            version: FORMAT_VERSION,
            world: &self.world,
            next_id: self.next_id,
            waypoints: &self.waypoints,
        })
        .map_err(|e| io_error(&self.path)(io::Error::other(e)))?;
        json.push(b'\n');

        let tmp = tmp_path(&self.path);
        let result = write_synced(&tmp, &json)
            .map_err(io_error(&tmp))
            .and_then(|()| fs::rename(&tmp, &self.path).map_err(io_error(&self.path)));
        if result.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        result?;
        sync_parent_dir(&self.path);
        Ok(())
    }
}

fn load(path: &Path) -> Result<Option<FileIn>, StoreError> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(StoreError::Io {
                path: path.to_owned(),
                source,
            });
        }
    };
    let corrupt = |reason: String| StoreError::Corrupt {
        path: path.to_owned(),
        reason,
    };
    let text = std::str::from_utf8(&bytes).map_err(|e| corrupt(format!("not UTF-8: {e}")))?;
    // Tolerate a BOM added by editors such as Notepad.
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|e| corrupt(e.to_string()))?;
    // Check the version before the full schema: a newer format may not match it.
    let version = match value.get("version") {
        Some(v) => v
            .as_u64()
            .ok_or_else(|| corrupt(format!("invalid version {v}")))?,
        None => return Err(corrupt("missing version".into())),
    };
    if version > FORMAT_VERSION {
        return Err(StoreError::UnsupportedVersion {
            path: path.to_owned(),
            version,
        });
    }
    if version == 0 {
        return Err(corrupt("invalid version 0".into()));
    }
    serde_json::from_value(value)
        .map(Some)
        .map_err(|e| corrupt(e.to_string()))
}

fn normalize_name(name: &str, id: u64) -> String {
    let name: String = name
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    match name.trim() {
        "" => format!("Waypoint {id}"),
        trimmed => trimmed.to_owned(),
    }
}

pub(crate) fn tmp_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".tmp");
    PathBuf::from(name)
}

/// `<stem>.corrupt-<unix>.json` next to `path`, with `-<n>` added if that already exists.
pub(crate) fn backup_path(path: &Path, unix: u64) -> PathBuf {
    let stem = path.file_stem().unwrap_or_default();
    let candidate = |suffix: &str| {
        let mut name = OsString::from(stem);
        name.push(format!(".corrupt-{unix}{suffix}.json"));
        path.with_file_name(name)
    };
    let mut backup = candidate("");
    let mut n = 1u32;
    while backup.symlink_metadata().is_ok() && n < 10_000 {
        backup = candidate(&format!("-{n}"));
        n += 1;
    }
    backup
}

pub(crate) fn write_synced(path: &Path, data: &[u8]) -> io::Result<()> {
    let mut file = File::create(path)?;
    file.write_all(data)?;
    file.flush()?;
    file.sync_all()
}

/// Makes the rename durable on Unix; Windows has no equivalent for directories.
pub(crate) fn sync_parent_dir(path: &Path) {
    #[cfg(unix)]
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        let _ = File::open(dir).and_then(|d| d.sync_all());
    }
    #[cfg(not(unix))]
    let _ = path;
}

pub(crate) fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::location::{OVERWORLD, THE_NETHER};

    fn loc(dimension: Option<&str>, x: f64, y: f64, z: f64) -> Location {
        Location {
            dimension: dimension.map(str::to_owned),
            x,
            y,
            z,
            yaw: 12.5,
            pitch: -3.0,
        }
    }

    fn dir_entries(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn missing_file_is_empty_and_not_created() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("waypoints").join("sp-World.json");
        let store = WaypointStore::open(&path, "World").unwrap();
        assert!(store.waypoints().is_empty());
        assert_eq!(store.path(), path);
        assert_eq!(store.world(), "World");
        assert_eq!(store.next_id(), 1);
        assert!(!path.exists());
        assert!(!path.parent().unwrap().exists());
    }

    #[test]
    fn add_normalizes_name_and_dimension() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = WaypointStore::open(dir.path().join("w.json"), "W").unwrap();
        let first = store.add("  Home base \n", &loc(None, 1.5, 64.0, -2.5), 1_700_000_000);
        assert_eq!(
            first,
            &Waypoint {
                id: 1,
                name: "Home base".into(),
                dimension: OVERWORLD.into(),
                x: 1.5,
                y: 64.0,
                z: -2.5,
                created_unix: 1_700_000_000,
            }
        );
        let second = store.add("   ", &loc(Some(THE_NETHER), 0.0, 0.0, 0.0), 5);
        assert_eq!(second.id, 2);
        assert_eq!(second.name, "Waypoint 2");
        assert_eq!(second.dimension, THE_NETHER);
        let third = store.add("a\tb", &loc(None, f64::NAN, f64::INFINITY, 3.0), 5);
        assert_eq!(third.name, "a b");
        assert_eq!((third.x, third.y, third.z), (0.0, 0.0, 3.0));
        assert_eq!(store.waypoints().len(), 3);
        assert_eq!(store.get(2).unwrap().name, "Waypoint 2");
        assert!(store.get(4).is_none());
    }

    #[test]
    fn rename_and_remove() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = WaypointStore::open(dir.path().join("w.json"), "W").unwrap();
        store.add("a", &loc(None, 0.0, 0.0, 0.0), 0);
        store.add("b", &loc(None, 0.0, 0.0, 0.0), 0);
        store.add("c", &loc(None, 0.0, 0.0, 0.0), 0);
        assert!(store.rename(2, "  Bee  "));
        assert_eq!(store.get(2).unwrap().name, "Bee");
        assert!(store.rename(2, ""));
        assert_eq!(store.get(2).unwrap().name, "Waypoint 2");
        assert!(!store.rename(9, "x"));
        let removed = store.remove(2).unwrap();
        assert_eq!(removed.id, 2);
        assert!(store.remove(2).is_none());
        let names: Vec<&str> = store.waypoints().iter().map(|w| w.name.as_str()).collect();
        assert_eq!(names, ["a", "c"]);
    }

    #[test]
    fn ids_are_never_reused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("w.json");
        let mut store = WaypointStore::open(&path, "W").unwrap();
        for name in ["a", "b", "c"] {
            store.add(name, &loc(None, 0.0, 0.0, 0.0), 0);
        }
        store.remove(3);
        assert_eq!(store.add("d", &loc(None, 0.0, 0.0, 0.0), 0).id, 4);
        store.remove(4);
        store.save().unwrap();

        let mut reopened = WaypointStore::open(&path, "W").unwrap();
        assert_eq!(reopened.next_id(), 5);
        assert_eq!(reopened.add("e", &loc(None, 0.0, 0.0, 0.0), 0).id, 5);
    }

    #[test]
    fn next_id_is_repaired_on_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("w.json");
        let waypoint = r#"{"id": 7, "name": "x", "dimension": "minecraft:overworld",
            "x": 1.0, "y": 2.0, "z": 3.0, "created_unix": 0}"#;
        fs::write(
            &path,
            format!(r#"{{"version": 1, "world": "W", "next_id": 2, "waypoints": [{waypoint}]}}"#),
        )
        .unwrap();
        assert_eq!(WaypointStore::open(&path, "W").unwrap().next_id(), 8);

        fs::write(
            &path,
            format!(r#"{{"version": 1, "waypoints": [{waypoint}]}}"#),
        )
        .unwrap();
        assert_eq!(WaypointStore::open(&path, "W").unwrap().next_id(), 8);

        fs::write(&path, r#"{"version": 1, "next_id": 40, "waypoints": []}"#).unwrap();
        assert_eq!(WaypointStore::open(&path, "W").unwrap().next_id(), 40);

        fs::write(&path, r#"{"version": 1, "next_id": 0}"#).unwrap();
        assert_eq!(WaypointStore::open(&path, "W").unwrap().next_id(), 1);
    }

    #[test]
    fn ids_stay_unique_when_the_id_space_is_exhausted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("w.json");
        let wp = |id: u64| {
            format!(
                r#"{{"id": {id}, "name": "n", "dimension": "minecraft:overworld",
                "x": 0.0, "y": 0.0, "z": 0.0, "created_unix": 0}}"#
            )
        };
        let unique = |store: &WaypointStore| {
            let ids: HashSet<u64> = store.waypoints().iter().map(|w| w.id).collect();
            assert_eq!(
                ids.len(),
                store.waypoints().len(),
                "{:?}",
                store.waypoints()
            );
        };
        // A hand-edited id at the very top of the range.
        fs::write(
            &path,
            format!(
                r#"{{"version": 1, "waypoints": [{}, {}]}}"#,
                wp(u64::MAX),
                wp(1)
            ),
        )
        .unwrap();
        let mut store = WaypointStore::open(&path, "W").unwrap();
        for _ in 0..3 {
            store.add("x", &loc(None, 0.0, 0.0, 0.0), 0);
            unique(&store);
        }
        // Duplicates cannot be renumbered past the top either.
        fs::write(
            &path,
            format!(
                r#"{{"version": 1, "waypoints": [{}, {}, {}]}}"#,
                wp(u64::MAX),
                wp(u64::MAX),
                wp(2)
            ),
        )
        .unwrap();
        let mut store = WaypointStore::open(&path, "W").unwrap();
        unique(&store);
        store.add("x", &loc(None, 0.0, 0.0, 0.0), 0);
        unique(&store);
        // `next_id` alone at the top still yields one fresh id, then unique ones.
        fs::write(
            &path,
            format!(r#"{{"version": 1, "next_id": {}}}"#, u64::MAX),
        )
        .unwrap();
        let mut store = WaypointStore::open(&path, "W").unwrap();
        assert_eq!(store.add("x", &loc(None, 0.0, 0.0, 0.0), 0).id, u64::MAX);
        store.add("y", &loc(None, 0.0, 0.0, 0.0), 0);
        unique(&store);
    }

    #[test]
    fn duplicate_ids_are_renumbered() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("w.json");
        let wp = |id: u64, name: &str| {
            format!(
                r#"{{"id": {id}, "name": "{name}", "dimension": "minecraft:overworld",
                "x": 0.0, "y": 0.0, "z": 0.0, "created_unix": 0}}"#
            )
        };
        fs::write(
            &path,
            format!(
                r#"{{"version": 1, "next_id": 3, "waypoints": [{}, {}, {}]}}"#,
                wp(1, "a"),
                wp(2, "b"),
                wp(1, "c")
            ),
        )
        .unwrap();
        let store = WaypointStore::open(&path, "W").unwrap();
        let ids: Vec<u64> = store.waypoints().iter().map(|w| w.id).collect();
        assert_eq!(ids, [1, 2, 3]);
        assert_eq!(store.get(3).unwrap().name, "c");
        assert_eq!(store.next_id(), 4);
    }

    #[test]
    fn round_trip_and_file_format() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir
            .path()
            .join("nested")
            .join("dirs")
            .join("mp-example.com-25565.json");
        let mut store = WaypointStore::open(&path, "example.com").unwrap();
        store.add(
            "家",
            &loc(Some("modid:some/path"), -0.5, 70.0, 1e7),
            1_750_000_000,
        );
        store.add("Base", &loc(None, 30_000_000.0, -64.0, -29_999_999.99), -1);
        store.save().unwrap();

        let reopened = WaypointStore::open(&path, "example.com").unwrap();
        assert_eq!(reopened.waypoints(), store.waypoints());
        assert_eq!(reopened.next_id(), 3);

        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("\n  \"version\": 1,\n"), "{text}");
        assert!(text.ends_with("}\n"));
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["version"], 1);
        assert_eq!(value["world"], "example.com");
        assert_eq!(value["next_id"], 3);
        assert_eq!(value["waypoints"][0]["name"], "家");
        assert_eq!(value["waypoints"][1]["created_unix"], -1);
    }

    #[test]
    fn save_is_atomic_and_leaves_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("w.json");
        let mut store = WaypointStore::open(&path, "W").unwrap();
        store.add("a", &loc(None, 1.0, 2.0, 3.0), 0);
        store.save().unwrap();
        // A stale temp file from a crash in the middle of a save must not get in the way.
        fs::write(dir.path().join("w.json.tmp"), "garbage").unwrap();
        store.add("b", &loc(None, 4.0, 5.0, 6.0), 0);
        store.save().unwrap();
        assert_eq!(dir_entries(dir.path()), ["w.json"]);
        assert_eq!(
            WaypointStore::open(&path, "W").unwrap().waypoints().len(),
            2
        );
    }

    #[test]
    fn save_failure_keeps_old_file_and_cleans_temp() {
        let dir = tempfile::tempdir().unwrap();
        // The target is a non-empty directory, so the final rename must fail.
        let path = dir.path().join("w.json");
        fs::create_dir(&path).unwrap();
        fs::write(path.join("keep"), "x").unwrap();
        let mut store = WaypointStore::empty(path.clone(), "W");
        store.add("a", &loc(None, 1.0, 2.0, 3.0), 0);
        assert!(matches!(store.save(), Err(StoreError::Io { .. })));
        assert_eq!(dir_entries(dir.path()), ["w.json"]);
        assert!(path.join("keep").exists());
    }

    #[test]
    fn world_display_is_updated_on_save() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("w.json");
        let store = WaypointStore::open(&path, "Old").unwrap();
        store.save().unwrap();
        let mut store = WaypointStore::open(&path, "New").unwrap();
        assert_eq!(store.world(), "New");
        store.set_world("Newer");
        store.save().unwrap();
        let value: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(value["world"], "Newer");
    }

    #[test]
    fn unknown_fields_bom_and_crlf_are_tolerated() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("w.json");
        let text = "\u{feff}{\r\n  \"version\": 1,\r\n  \"world\": \"W\",\r\n  \"extra\": [1, 2],\r\n  \
                    \"waypoints\": [{\"id\": 3, \"name\": \"n\", \"dimension\": \"minecraft:the_end\", \
                    \"x\": 1, \"y\": 2, \"z\": 3, \"created_unix\": 9, \"color\": \"red\"}]\r\n}\r\n";
        fs::write(&path, text).unwrap();
        let store = WaypointStore::open(&path, "W").unwrap();
        assert_eq!(store.waypoints().len(), 1);
        assert_eq!(store.get(3).unwrap().dimension, "minecraft:the_end");
        assert_eq!(store.next_id(), 4);
    }

    #[test]
    fn corrupt_file_is_an_error_and_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("w.json");
        for content in [
            &b"{ not json"[..],
            b"",
            b"[]",
            b"{\"waypoints\": []}",
            b"{\"version\": 0}",
            b"{\"version\": \"1\"}",
            b"{\"version\": -1}",
            b"{\"version\": 1, \"waypoints\": [{\"id\": 1}]}",
            b"{\"version\": 1, \"waypoints\": {}}",
            b"\xff\xfe{}",
        ] {
            fs::write(&path, content).unwrap();
            let result = WaypointStore::open(&path, "W");
            assert!(
                matches!(result, Err(StoreError::Corrupt { .. })),
                "{:?} -> {result:?}",
                String::from_utf8_lossy(content)
            );
            assert_eq!(fs::read(&path).unwrap(), content);
            assert_eq!(dir_entries(dir.path()), ["w.json"]);
        }
    }

    #[test]
    fn open_or_backup_moves_corrupt_file_aside() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sp-World.json");
        fs::write(&path, "{ truncated").unwrap();
        let before = unix_now();
        let (mut store, backup) = WaypointStore::open_or_backup(&path, "World").unwrap();
        let backup = backup.unwrap();
        assert!(store.waypoints().is_empty());
        assert!(!path.exists());
        assert_eq!(fs::read_to_string(&backup).unwrap(), "{ truncated");
        assert_eq!(backup.parent(), Some(dir.path()));
        let name = backup.file_name().unwrap().to_str().unwrap();
        let unix: u64 = name
            .strip_prefix("sp-World.corrupt-")
            .and_then(|rest| rest.strip_suffix(".json"))
            .unwrap()
            .parse()
            .unwrap();
        assert!(unix >= before && unix <= unix_now());

        store.add("fresh", &loc(None, 0.0, 0.0, 0.0), 0);
        store.save().unwrap();
        assert_eq!(
            WaypointStore::open(&path, "World")
                .unwrap()
                .waypoints()
                .len(),
            1
        );
    }

    #[test]
    fn open_or_backup_passes_through_good_and_missing_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("w.json");
        let (store, backup) = WaypointStore::open_or_backup(&path, "W").unwrap();
        assert!(backup.is_none());
        store.save().unwrap();
        let (_, backup) = WaypointStore::open_or_backup(&path, "W").unwrap();
        assert!(backup.is_none());
        assert_eq!(dir_entries(dir.path()), ["w.json"]);
    }

    #[test]
    fn backup_names_do_not_collide() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("w.json");
        assert_eq!(backup_path(&path, 42), dir.path().join("w.corrupt-42.json"));
        fs::write(dir.path().join("w.corrupt-42.json"), "").unwrap();
        assert_eq!(
            backup_path(&path, 42),
            dir.path().join("w.corrupt-42-1.json")
        );
        fs::write(dir.path().join("w.corrupt-42-1.json"), "").unwrap();
        assert_eq!(
            backup_path(&path, 42),
            dir.path().join("w.corrupt-42-2.json")
        );
    }

    #[test]
    fn newer_version_is_refused_and_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("w.json");
        // A future format whose waypoints no longer match this schema.
        let future = r#"{"version": 2, "waypoints": {"by_id": {}}}"#;
        fs::write(&path, future).unwrap();
        assert!(matches!(
            WaypointStore::open(&path, "W"),
            Err(StoreError::UnsupportedVersion { version: 2, .. })
        ));
        assert!(matches!(
            WaypointStore::open_or_backup(&path, "W"),
            Err(StoreError::UnsupportedVersion { version: 2, .. })
        ));
        assert_eq!(fs::read_to_string(&path).unwrap(), future);
        assert_eq!(dir_entries(dir.path()), ["w.json"]);
    }

    #[test]
    fn io_errors_are_not_reported_as_corruption() {
        let dir = tempfile::tempdir().unwrap();
        // Reading a directory fails with an I/O error on every platform.
        let path = dir.path().join("w.json");
        fs::create_dir(&path).unwrap();
        assert!(matches!(
            WaypointStore::open(&path, "W"),
            Err(StoreError::Io { .. })
        ));
        assert!(matches!(
            WaypointStore::open_or_backup(&path, "W"),
            Err(StoreError::Io { .. })
        ));
        assert!(path.is_dir());
    }

    #[test]
    fn error_messages_name_the_file() {
        let err = StoreError::UnsupportedVersion {
            path: PathBuf::from("w.json"),
            version: 3,
        };
        assert_eq!(
            err.to_string(),
            "waypoint file w.json has format version 3, newer than the supported 1"
        );
        let err = StoreError::Corrupt {
            path: PathBuf::from("w.json"),
            reason: "bad".into(),
        };
        assert_eq!(err.to_string(), "waypoint file w.json is corrupt: bad");
    }
}
