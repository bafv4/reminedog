//! Agent options: the text after `=` in `-agentpath:C:\x\reminedog.dll=<options>`.
//!
//! Comma-separated `key=value` items with case-insensitive keys:
//! `gamedir=<path>`, `log=off|error|warn|info|debug|trace`, `overlay=on|off|true|false|1|0`
//! and `scale=<0.5..=4>`. Problems produce warnings and leave the default in place.

use std::path::PathBuf;

use log::LevelFilter;

#[derive(Debug, Clone, PartialEq)]
pub struct AgentOptions {
    /// Overrides game directory detection.
    pub game_dir: Option<PathBuf>,
    pub log_level: Option<LevelFilter>,
    /// Draw the in-game overlay (default `true`).
    pub overlay: bool,
    pub ui_scale: Option<f32>,
}

impl Default for AgentOptions {
    fn default() -> Self {
        Self {
            game_dir: None,
            log_level: None,
            overlay: true,
            ui_scale: None,
        }
    }
}

impl AgentOptions {
    /// Same as [`parse`].
    pub fn parse(s: &str) -> (Self, Vec<String>) {
        parse(s)
    }
}

pub const MIN_UI_SCALE: f32 = 0.5;
pub const MAX_UI_SCALE: f32 = 4.0;

/// Parses the agent option string, returning the options and human-readable warnings.
///
/// The `gamedir` value is everything after the first `=` of its item, so it may contain
/// `:`, `\`, `=` and spaces. Since Windows paths may also contain commas, an item without
/// `=` that directly follows `gamedir` is taken as part of the path.
pub fn parse(s: &str) -> (AgentOptions, Vec<String>) {
    let mut options = AgentOptions::default();
    let mut warnings = Vec::new();
    let mut game_dir: Option<String> = None;
    let mut in_game_dir = false;

    for item in s.split(',') {
        if item.trim().is_empty() {
            continue;
        }
        let Some((key, value)) = item.split_once('=') else {
            match game_dir.as_mut() {
                Some(dir) if in_game_dir => {
                    dir.push(',');
                    dir.push_str(item);
                }
                _ => warnings.push(format!(
                    "ignoring option `{}`: expected key=value",
                    item.trim()
                )),
            }
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        let value = value.trim();
        in_game_dir = key == "gamedir";
        match key.as_str() {
            "gamedir" => game_dir = Some(value.to_owned()),
            "log" => match parse_level(value) {
                Some(level) => options.log_level = Some(level),
                None => warnings.push(format!(
                    "invalid log level `{value}`; expected off, error, warn, info, debug or trace"
                )),
            },
            "overlay" => match parse_bool(value) {
                Some(on) => options.overlay = on,
                None => warnings.push(format!(
                    "invalid overlay value `{value}`; expected on, off, true, false, 1 or 0"
                )),
            },
            "scale" => match value.parse::<f32>() {
                Ok(scale) if (MIN_UI_SCALE..=MAX_UI_SCALE).contains(&scale) => {
                    options.ui_scale = Some(scale);
                }
                _ => warnings.push(format!(
                    "invalid scale `{value}`; expected a number from {MIN_UI_SCALE} to {MAX_UI_SCALE}"
                )),
            },
            _ => warnings.push(format!(
                "unknown option `{}`; known options are gamedir, log, overlay and scale",
                key
            )),
        }
    }

    if let Some(dir) = game_dir {
        let dir = dir.trim();
        let dir = dir
            .strip_prefix('"')
            .and_then(|d| d.strip_suffix('"'))
            .unwrap_or(dir);
        if dir.is_empty() {
            warnings.push("empty gamedir ignored".to_owned());
        } else {
            options.game_dir = Some(PathBuf::from(dir));
        }
    }
    (options, warnings)
}

fn parse_level(value: &str) -> Option<LevelFilter> {
    Some(match value.to_ascii_lowercase().as_str() {
        "off" => LevelFilter::Off,
        "error" => LevelFilter::Error,
        "warn" => LevelFilter::Warn,
        "info" => LevelFilter::Info,
        "debug" => LevelFilter::Debug,
        "trace" => LevelFilter::Trace,
        _ => return None,
    })
}

fn parse_bool(value: &str) -> Option<bool> {
    match value.to_ascii_lowercase().as_str() {
        "on" | "true" | "1" => Some(true),
        "off" | "false" | "0" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_ok(s: &str) -> AgentOptions {
        let (options, warnings) = parse(s);
        assert!(warnings.is_empty(), "{s:?}: {warnings:?}");
        options
    }

    #[test]
    fn defaults() {
        let defaults = AgentOptions::default();
        assert!(defaults.overlay);
        assert_eq!(defaults.game_dir, None);
        assert_eq!(defaults.log_level, None);
        assert_eq!(defaults.ui_scale, None);
        assert_eq!(parse(""), (defaults.clone(), vec![]));
        assert_eq!(parse("  "), (defaults.clone(), vec![]));
        assert_eq!(parse(",,"), (defaults, vec![]));
    }

    #[test]
    fn all_keys() {
        let options = parse_ok("gamedir=/home/me/.minecraft,log=debug,overlay=off,scale=1.5");
        assert_eq!(
            options,
            AgentOptions {
                game_dir: Some(PathBuf::from("/home/me/.minecraft")),
                log_level: Some(LevelFilter::Debug),
                overlay: false,
                ui_scale: Some(1.5),
            }
        );
        assert_eq!(
            AgentOptions::parse("log=warn").0.log_level,
            Some(LevelFilter::Warn)
        );
    }

    #[test]
    fn keys_and_values_are_case_insensitive() {
        let options = parse_ok("GameDir=D:\\MC, LOG=Trace ,Overlay=TRUE,SCALE=2");
        assert_eq!(options.game_dir, Some(PathBuf::from("D:\\MC")));
        assert_eq!(options.log_level, Some(LevelFilter::Trace));
        assert!(options.overlay);
        assert_eq!(options.ui_scale, Some(2.0));
    }

    #[test]
    fn windows_paths_with_spaces_colons_and_equals() {
        let options = parse_ok(r"gamedir=C:\Program Files\My Games\.minecraft");
        assert_eq!(
            options.game_dir,
            Some(PathBuf::from(r"C:\Program Files\My Games\.minecraft"))
        );
        let options = parse_ok(r"log=info,gamedir=C:\Users\me\a=b\inst ance,overlay=0");
        assert_eq!(
            options.game_dir,
            Some(PathBuf::from(r"C:\Users\me\a=b\inst ance"))
        );
        assert!(!options.overlay);
        let options = parse_ok(r#"gamedir="C:\Users\me\My Instance""#);
        assert_eq!(
            options.game_dir,
            Some(PathBuf::from(r"C:\Users\me\My Instance"))
        );
        let options = parse_ok(r"gamedir=\\server\share\mc");
        assert_eq!(options.game_dir, Some(PathBuf::from(r"\\server\share\mc")));
    }

    #[test]
    fn commas_inside_gamedir() {
        let options = parse_ok(r"gamedir=C:\Games\Smith, John\mc,log=warn");
        assert_eq!(
            options.game_dir,
            Some(PathBuf::from(r"C:\Games\Smith, John\mc"))
        );
        assert_eq!(options.log_level, Some(LevelFilter::Warn));
        // After another key, a bare item is an error again.
        let (options, warnings) = parse(r"gamedir=C:\a,log=warn,stray");
        assert_eq!(options.game_dir, Some(PathBuf::from(r"C:\a")));
        assert_eq!(warnings.len(), 1, "{warnings:?}");
    }

    #[test]
    fn last_occurrence_wins() {
        let options = parse_ok("log=error,log=trace,gamedir=a,gamedir=b");
        assert_eq!(options.log_level, Some(LevelFilter::Trace));
        assert_eq!(options.game_dir, Some(PathBuf::from("b")));
    }

    #[test]
    fn overlay_values() {
        for (value, expected) in [
            ("on", true),
            ("off", false),
            ("true", true),
            ("false", false),
            ("1", true),
            ("0", false),
            ("OFF", false),
        ] {
            assert_eq!(
                parse_ok(&format!("overlay={value}")).overlay,
                expected,
                "{value}"
            );
        }
    }

    #[test]
    fn log_levels() {
        for (value, expected) in [
            ("off", LevelFilter::Off),
            ("error", LevelFilter::Error),
            ("warn", LevelFilter::Warn),
            ("info", LevelFilter::Info),
            ("debug", LevelFilter::Debug),
            ("trace", LevelFilter::Trace),
        ] {
            assert_eq!(parse_ok(&format!("log={value}")).log_level, Some(expected));
        }
    }

    #[test]
    fn scale_bounds() {
        assert_eq!(parse_ok("scale=0.5").ui_scale, Some(0.5));
        assert_eq!(parse_ok("scale=4").ui_scale, Some(4.0));
        assert_eq!(parse_ok("scale=1.25").ui_scale, Some(1.25));
        for bad in ["0.49", "4.01", "0", "-1", "NaN", "inf", "abc", "", "1.5x"] {
            let (options, warnings) = parse(&format!("scale={bad}"));
            assert_eq!(options.ui_scale, None, "{bad}");
            assert_eq!(warnings.len(), 1, "{bad}: {warnings:?}");
        }
    }

    #[test]
    fn bad_values_warn_and_keep_defaults() {
        let (options, warnings) = parse("log=verbose,overlay=maybe,scale=9,colour=red,justakey");
        assert_eq!(options, AgentOptions::default());
        assert_eq!(warnings.len(), 5, "{warnings:?}");
        assert!(warnings[0].contains("verbose"));
        assert!(warnings[1].contains("maybe"));
        assert!(warnings[2].contains('9'));
        assert!(warnings[3].contains("colour"));
        assert!(warnings[4].contains("justakey"));
    }

    #[test]
    fn empty_gamedir_warns() {
        let (options, warnings) = parse("gamedir=,log=info");
        assert_eq!(options.game_dir, None);
        assert_eq!(options.log_level, Some(LevelFilter::Info));
        assert_eq!(warnings.len(), 1);
        let (options, warnings) = parse("gamedir=\"\"");
        assert_eq!(options.game_dir, None);
        assert_eq!(warnings.len(), 1);
    }

    #[test]
    fn empty_key_is_unknown() {
        let (options, warnings) = parse("=x");
        assert_eq!(options, AgentOptions::default());
        assert_eq!(warnings.len(), 1);
    }
}
