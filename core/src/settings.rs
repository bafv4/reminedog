//! 利用者の設定：`<game_dir>/reminedog/settings.json`。
//!
//! どのフィールドにも既定があるので、古いファイルも新しいファイルも読める：知らないフィールドは
//! 無視し、ないものは既定になる。範囲外の値は断らずに範囲に収める。手で書き換えると壊れやすい
//! 置き換えのフィールドは、ファイル全体ではなく、読めない値や項目だけを捨てる（警告を出し、
//! 次の保存で失わないようにファイルの写しを残す）。

use std::cell::Cell;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::waypoint::{backup_path, copy_path, unix_now, write_file};

/// ズームの倍率の範囲。
pub const ZOOM_FACTOR_RANGE: (f32, f32) = (1.5, 8.0);
/// ブラウザのページの拡大率の範囲。
pub const BROWSER_ZOOM_RANGE: (f32, f32) = (0.25, 3.0);
/// メニューを閉じている間のブラウザの不透明度の範囲。
pub const BROWSER_OPACITY_RANGE: (f32, f32) = (0.2, 1.0);
/// ブラウザの送り・戻しのキーが動画を動かす秒数の範囲。
pub const BROWSER_SEEK_RANGE: (f32, f32) = (1.0, 60.0);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// メニューを開閉する（例：`"Ctrl+I"`）。
    pub menu_key: String,
    /// 押している間ズームする（例：`"Z"`、`"Mouse4"`）。
    pub zoom_key: String,
    pub zoom_factor: f32,
    /// ふつうのフレームのピクセルを引き伸ばすのではなく、ゲームに縦長の解像度で描かせて本物の
    /// 細かさにする。
    pub zoom_high_res: bool,
    /// 高精細でないズームのときに、引き伸ばしたピクセルを滑らかにする。
    pub zoom_smooth: bool,
    /// メニューを閉じている間も状態の窓を出す。
    pub show_status: bool,
    /// プレイヤーの位置にウェイポイントを記録する（F3+C で）。
    pub waypoint_key: String,
    /// プレイヤーの位置を更新し、選んだウェイポイントへの道を示す。
    pub navigate_key: String,
    /// `rebinds` を使う（全部をまとめたメニューのスイッチ）。
    #[serde(deserialize_with = "lenient_rebinds_enabled")]
    pub rebinds_enabled: bool,
    /// ゲーム中に、別のものとしてゲームに届くキーとマウスのボタン。
    #[serde(deserialize_with = "lenient_rebinds")]
    pub rebinds: Vec<Rebind>,
    /// ブラウザを出し入れする。空ならなし（メニューのボタンだけ）。
    pub browser_toggle_key: String,
    /// ブラウザを出していてゲームをプレイしている間の、ブラウザのキー（空ならなし）。
    pub browser_page_up_key: String,
    pub browser_page_down_key: String,
    pub browser_play_pause_key: String,
    pub browser_seek_back_key: String,
    pub browser_seek_forward_key: String,
    /// 古い版でブラウザが開いたページ。読む（OS のフックが 1 回引き継ぐ）が、もう書かない：
    /// アドレスにトークンが入りうるし、このファイルはゲームフォルダと一緒に動く（書き出した
    /// インスタンス、報告）。最後のページは、代わりにブラウザ自身のデータに残す。
    #[serde(skip_serializing)]
    pub browser_url: String,
    /// 画面のどこにページがあるか：左、上、幅、高さ（ポイント）。`None` なら右上の隅。
    pub browser_rect: Option<[f32; 4]>,
    /// ページの拡大率（1.0 が 100 %）。
    pub browser_zoom: f32,
    /// メニューを閉じている間のページの不透明度。
    pub browser_opacity: f32,
    /// 送り・戻しのキーが動画を動かす秒数。
    pub browser_seek_seconds: f32,
    /// 読めない置き換えの値か項目を捨てたので、[`Settings::load`] がファイルを写した場所（次の
    /// 保存でそれらが失われるため）。保存しない。
    #[serde(skip)]
    pub rebinds_kept_in: Option<PathBuf>,
    /// 有効な JSON でなかったファイルを [`Settings::load`] が移した場所（そのときは既定のもの）。
    /// 保存しない。
    #[serde(skip)]
    pub moved_aside_to: Option<PathBuf>,
    /// [`Settings::load`] がファイルを読めなかった（ないのではなく）：これは既定のもので、保存すると
    /// 利用者のファイルを上書きしてしまう。保存しない。
    #[serde(skip)]
    pub unreadable: bool,
}

/// キーの置き換えの 1 つのルール：`from` が `to` としてゲームに届く。どちらも 26.x のキーの名前
/// （例：`"key.mouse.4"`、`"key.keyboard.f3"`。[`Naming::Modern`] の `keybinds::input_by_name`）。
/// キーでない名前はここに残し、ルールを使うところで除く。
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
            browser_toggle_key: String::new(),
            browser_page_up_key: "PageUp".into(),
            browser_page_down_key: "PageDown".into(),
            browser_play_pause_key: "Down".into(),
            browser_seek_back_key: "Left".into(),
            browser_seek_forward_key: "Right".into(),
            browser_url: "https://www.google.com/".into(),
            browser_rect: None,
            browser_zoom: 1.0,
            browser_opacity: 1.0,
            browser_seek_seconds: 10.0,
            rebinds_kept_in: None,
            moved_aside_to: None,
            unreadable: false,
        }
    }
}

thread_local! {
    /// [`Settings::load`] が読み始めてから、寛容な読み手が何かを捨てた。
    static DROPPED: Cell<bool> = const { Cell::new(false) };
}

/// `rebinds_enabled`：`true` か `false`（文字列でもよい）。それ以外は既定。
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

/// `rebinds`：リストでない値ならなし。`from` と `to` の文字列のない項目は捨てる。
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
    /// 手で書き換えたファイルの値を範囲に戻す。
    pub fn sanitized(mut self) -> Self {
        let defaults = Settings::default();
        let clamp = |value: f32, (lo, hi): (f32, f32), default: f32| {
            if value.is_finite() {
                value.clamp(lo, hi)
            } else {
                default
            }
        };
        self.zoom_factor = clamp(self.zoom_factor, ZOOM_FACTOR_RANGE, defaults.zoom_factor);
        self.browser_zoom = clamp(self.browser_zoom, BROWSER_ZOOM_RANGE, defaults.browser_zoom);
        self.browser_opacity = clamp(
            self.browser_opacity,
            BROWSER_OPACITY_RANGE,
            defaults.browser_opacity,
        );
        self.browser_seek_seconds = clamp(
            self.browser_seek_seconds,
            BROWSER_SEEK_RANGE,
            defaults.browser_seek_seconds,
        );
        if self.browser_rect.is_some_and(|rect| {
            rect.iter().any(|v| !v.is_finite()) || rect[2] <= 0.0 || rect[3] <= 0.0
        }) {
            self.browser_rect = None;
        }
        self
    }

    /// `path` の設定を読む。ファイルがなければ既定のもの。読めなければ、[`Settings::unreadable`] を
    /// 立てた既定のもの。UTF-8 の BOM（メモ帳、PowerShell 5.1）は飛ばす。有効な JSON でない
    /// ファイルは、次の保存で利用者の書き換えを壊さないように脇に移し
    /// （`settings.corrupt-<unix>.json`、[`Settings::moved_aside_to`]）、既定のものを使う。
    /// 置き換えの値か項目を全部は読めなかったファイルは、同じ理由で `settings.rebinds-<unix>.json` に
    /// 写す（[`Settings::rebinds_kept_in`]）。
    pub fn load(path: &Path) -> Settings {
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Settings::default(),
            Err(e) => {
                log::warn!(
                    "cannot read {}: {e}; using default settings, and not saving them",
                    path.display()
                );
                return Settings {
                    unreadable: true,
                    ..Settings::default()
                };
            }
        };
        let json = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes);
        DROPPED.set(false);
        let parsed = serde_json::from_slice::<Settings>(json);
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
                    Ok(()) => {
                        log::warn!(
                            "settings file {} is invalid ({e}); moved it to {}",
                            path.display(),
                            backup.display()
                        );
                        Settings {
                            moved_aside_to: Some(backup),
                            ..Settings::default()
                        }
                    }
                    Err(re) => {
                        log::warn!(
                            "settings file {} is invalid ({e}) and cannot be moved aside ({re}); not saving over it",
                            path.display()
                        );
                        Settings {
                            unreadable: true,
                            ..Settings::default()
                        }
                    }
                }
            }
        }
    }

    /// 設定をアトミックに書く（一時ファイルに書いてから名前を変える）。フォルダは作る。
    pub fn save(&self, path: &Path) -> io::Result<()> {
        write_file(path, &self.to_json()?)
    }

    /// [`write_file`] のための（別のスレッドで）ファイルの中身。
    pub fn to_json(&self) -> io::Result<Vec<u8>> {
        let mut json = serde_json::to_vec_pretty(self).map_err(io::Error::other)?;
        json.push(b'\n');
        Ok(json)
    }
}

/// 設定ファイルを隣の `settings.rebinds-<unix>.json` に写す。写せたらその場所。同じ中身の写しが
/// もうあればそれを使うので、同じファイルをまた読んでも（次の起動、オーバーレイの作り直し）
/// 写しが積み上がらない。
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

/// `path` の隣の、ちょうど同じ中身の `settings.rebinds-*.json`。
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

/// `<game_dir>/reminedog/settings.json`。
pub fn settings_path(game_dir: &Path) -> PathBuf {
    crate::gamedir::data_dir(game_dir).join("settings.json")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::waypoint::tmp_path;

    #[test]
    fn a_byte_order_mark_is_read_past() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(&path, b"\xEF\xBB\xBF{\"zoom_key\": \"X\"}").unwrap();
        let settings = Settings::load(&path);
        assert_eq!(settings.zoom_key, "X");
        assert_eq!(settings.moved_aside_to, None);
    }

    #[test]
    fn an_invalid_file_is_moved_aside_and_said_so() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(&path, b"{\"zoom_key\": \"X\",}").unwrap();
        let settings = Settings::load(&path);
        let moved = settings.moved_aside_to.clone().unwrap();
        assert!(moved.exists());
        assert!(!path.exists());
        assert_eq!(settings.zoom_key, Settings::default().zoom_key);
    }

    #[test]
    fn the_browser_url_is_read_but_not_written() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(&path, br#"{"browser_url": "https://example.com/?token=x"}"#).unwrap();
        let settings = Settings::load(&path);
        assert_eq!(settings.browser_url, "https://example.com/?token=x");
        settings.save(&path).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(!text.contains("browser_url"), "{text}");
    }

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
            browser_toggle_key: "Ctrl+B".into(),
            browser_page_up_key: String::new(),
            browser_page_down_key: "Mouse5".into(),
            browser_play_pause_key: "End".into(),
            browser_seek_back_key: "Home".into(),
            browser_seek_forward_key: "Insert".into(),
            // 書かない（the_browser_url_is_read_but_not_written を参照）。
            browser_url: Settings::default().browser_url,
            browser_rect: Some([1.5, 2.0, 300.0, 168.75]),
            browser_zoom: 0.75,
            browser_opacity: 0.5,
            browser_seek_seconds: 5.0,
            rebinds_kept_in: None,
            moved_aside_to: None,
            unreadable: false,
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
        // 置き換えより前のファイル：オンで、ルールなし。
        fs::write(&path, r#"{"zoom_key": "C"}"#).unwrap();
        let settings = Settings::load(&path);
        assert!(settings.rebinds_enabled);
        assert!(settings.rebinds.is_empty());
        // 名前は、知っていてもいなくても、書かれたとおりに残す。
        fs::write(
            &path,
            r#"{"rebinds_enabled": false, "rebinds": [{"from": "key.keyboard.nope", "to": "x", "note": 1}]}"#,
        )
        .unwrap();
        let settings = Settings::load(&path);
        assert!(!settings.rebinds_enabled);
        assert_eq!(settings.rebinds, [rebind("key.keyboard.nope", "x")]);
        assert_eq!(settings.rebinds_kept_in, None, "nothing dropped");
        // 文字列で書いたスイッチ。
        for (text, enabled) in [("false", false), ("true", true)] {
            fs::write(&path, format!(r#"{{"rebinds_enabled": "{text}"}}"#)).unwrap();
            let settings = Settings::load(&path);
            assert_eq!(settings.rebinds_enabled, enabled, "{text}");
            assert_eq!(settings.rebinds_kept_in, None);
        }
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1, "no copies");
    }

    /// `dir` の中の `settings.rebinds-*.json` の写し。
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
        // ファイルは脇に移さず、ほかの設定も残る。
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
        // 捨てたものはファイルの写しに残り、保存はそれに触れない。
        let copy = settings.rebinds_kept_in.clone().expect("copied");
        assert_eq!(copies(dir.path()), std::slice::from_ref(&copy));
        assert_eq!(fs::read(&copy).unwrap(), original);
        // 同じファイルをまた読んでも（次の起動）、その写しを使う。
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
        // スイッチだけ。
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
        // ウェイポイントのキーより前のファイルは、その既定を得る。
        assert_eq!(settings.waypoint_key, "J");
        assert_eq!(settings.navigate_key, "K");
        // ブラウザより前のものも。
        assert_eq!(settings.browser_toggle_key, "");
        assert_eq!(settings.browser_page_down_key, "PageDown");
        assert_eq!(settings.browser_play_pause_key, "Down");
        assert_eq!(settings.browser_rect, None);
    }

    #[test]
    fn out_of_range_values_are_clamped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(&path, r#"{"zoom_factor": 100}"#).unwrap();
        assert_eq!(Settings::load(&path).zoom_factor, ZOOM_FACTOR_RANGE.1);
        fs::write(&path, r#"{"zoom_factor": 0.1}"#).unwrap();
        assert_eq!(Settings::load(&path).zoom_factor, ZOOM_FACTOR_RANGE.0);
        fs::write(
            &path,
            r#"{"browser_zoom": 9, "browser_opacity": 0, "browser_seek_seconds": 600}"#,
        )
        .unwrap();
        let settings = Settings::load(&path);
        assert_eq!(settings.browser_zoom, BROWSER_ZOOM_RANGE.1);
        assert_eq!(settings.browser_opacity, BROWSER_OPACITY_RANGE.0);
        assert_eq!(settings.browser_seek_seconds, BROWSER_SEEK_RANGE.1);
    }

    #[test]
    fn a_browser_rect_without_a_size_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(&path, r#"{"browser_rect": [10, 20, 300, 200]}"#).unwrap();
        assert_eq!(
            Settings::load(&path).browser_rect,
            Some([10.0, 20.0, 300.0, 200.0])
        );
        fs::write(&path, r#"{"browser_rect": [10, 20, 0, 200]}"#).unwrap();
        assert_eq!(Settings::load(&path).browser_rect, None);
    }

    #[test]
    fn invalid_file_is_moved_aside() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(&path, "{ not json").unwrap();
        let loaded = Settings::load(&path);
        assert!(loaded.moved_aside_to.is_some());
        assert_eq!(
            Settings {
                moved_aside_to: None,
                ..loaded
            },
            Settings::default()
        );
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
