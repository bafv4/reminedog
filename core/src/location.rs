//! Parsing of the command Minecraft copies to the clipboard on F3+C.
//!
//! Since 1.13 the game formats it with `Locale.ROOT` and `%.2f`:
//! `/execute in minecraft:overworld run tp @s 123.45 64.00 -987.65 -179.90 12.30`.

use std::borrow::Cow;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

pub const OVERWORLD: &str = "minecraft:overworld";
pub const THE_NETHER: &str = "minecraft:the_nether";
pub const THE_END: &str = "minecraft:the_end";

/// A player position and view direction as reported by F3+C.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Location {
    /// Dimension id such as `minecraft:overworld`; `None` for the bare `/tp @s ...` form.
    pub dimension: Option<String>,
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub yaw: f32,
    pub pitch: f32,
}

impl Location {
    /// The dimension id, assuming the overworld when F3+C did not name one.
    pub fn dimension_or_overworld(&self) -> &str {
        self.dimension.as_deref().unwrap_or(OVERWORLD)
    }
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ParseError {
    #[error("clipboard text is empty")]
    Empty,
    #[error("not an F3+C teleport command")]
    NotTeleport,
    #[error("invalid dimension id `{0}`")]
    InvalidDimension(String),
    #[error("expected 5 numbers (x y z yaw pitch), found {0}")]
    ArgumentCount(usize),
    #[error("invalid number `{0}`")]
    InvalidNumber(String),
}

/// Parses F3+C clipboard text into a [`Location`].
///
/// Accepts `[/]execute in <dimension> run tp @s x y z yaw pitch` and the older
/// `[/]tp @s x y z yaw pitch`. Anything else, including other single-line commands,
/// is rejected so that unrelated clipboard contents never produce a waypoint.
pub fn parse_f3c(text: &str) -> Result<Location, ParseError> {
    let text = text.trim();
    if text.is_empty() {
        return Err(ParseError::Empty);
    }
    // F3+C output is always a single line.
    if text.contains(['\r', '\n']) {
        return Err(ParseError::NotTeleport);
    }
    let text = text.strip_prefix('/').unwrap_or(text);
    let tokens: Vec<&str> = text.split_whitespace().collect();
    let (dimension, numbers) = match tokens.as_slice() {
        ["execute", "in", dimension, "run", "tp", "@s", rest @ ..] => {
            (Some(parse_dimension(dimension)?), rest)
        }
        ["tp", "@s", rest @ ..] => (None, rest),
        _ => return Err(ParseError::NotTeleport),
    };
    let [x, y, z, yaw, pitch] = numbers else {
        return Err(ParseError::ArgumentCount(numbers.len()));
    };
    Ok(Location {
        dimension,
        x: parse_number(x, |v: &f64| v.is_finite())?,
        y: parse_number(y, |v: &f64| v.is_finite())?,
        z: parse_number(z, |v: &f64| v.is_finite())?,
        yaw: parse_number(yaw, |v: &f32| v.is_finite())?,
        pitch: parse_number(pitch, |v: &f32| v.is_finite())?,
    })
}

/// Validates a resource location (`namespace:path`); a missing namespace means `minecraft`.
fn parse_dimension(id: &str) -> Result<String, ParseError> {
    let (namespace, path) = id.split_once(':').unwrap_or(("minecraft", id));
    let namespace_ok = !namespace.is_empty()
        && namespace
            .bytes()
            .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-' | b'.'));
    let path_ok = !path.is_empty()
        && path
            .bytes()
            .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-' | b'.' | b'/'));
    if namespace_ok && path_ok {
        Ok(format!("{namespace}:{path}"))
    } else {
        Err(ParseError::InvalidDimension(id.to_owned()))
    }
}

fn parse_number<T: FromStr>(token: &str, finite: impl Fn(&T) -> bool) -> Result<T, ParseError> {
    normalize_number(token)
        .and_then(|n| n.parse::<T>().ok())
        .filter(finite)
        .ok_or_else(|| ParseError::InvalidNumber(token.to_owned()))
}

/// Accepts `-?digits([.,]digits)?` and returns it with a `.` decimal separator.
///
/// This deliberately rejects exponents, `NaN`, `Infinity` and a leading `+`, none of
/// which `%.2f` produces. A `,` separator is tolerated in case a locale leaks in.
fn normalize_number(token: &str) -> Option<Cow<'_, str>> {
    let unsigned = token.strip_prefix('-').unwrap_or(token);
    let (int, frac) = match unsigned.split_once(['.', ',']) {
        Some((int, frac)) => (int, Some(frac)),
        None => (unsigned, None),
    };
    let all_digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if !all_digits(int) || !frac.is_none_or(all_digits) {
        return None;
    }
    Some(if token.contains(',') {
        Cow::Owned(token.replace(',', "."))
    } else {
        Cow::Borrowed(token)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loc(dimension: Option<&str>, x: f64, y: f64, z: f64, yaw: f32, pitch: f32) -> Location {
        Location {
            dimension: dimension.map(str::to_owned),
            x,
            y,
            z,
            yaw,
            pitch,
        }
    }

    #[test]
    fn canonical() {
        assert_eq!(
            parse_f3c(
                "/execute in minecraft:overworld run tp @s 123.45 64.00 -987.65 -179.90 12.30"
            ),
            Ok(loc(Some(OVERWORLD), 123.45, 64.0, -987.65, -179.9, 12.3))
        );
    }

    #[test]
    fn other_vanilla_dimensions() {
        let nether =
            parse_f3c("/execute in minecraft:the_nether run tp @s 1.00 2.00 3.00 4.00 5.00");
        assert_eq!(nether.unwrap().dimension.as_deref(), Some(THE_NETHER));
        let end = parse_f3c("/execute in minecraft:the_end run tp @s 1.00 2.00 3.00 4.00 5.00");
        assert_eq!(end.unwrap().dimension.as_deref(), Some(THE_END));
    }

    #[test]
    fn without_leading_slash_and_with_surrounding_whitespace() {
        let expected = loc(Some(OVERWORLD), 1.5, 2.0, -3.25, 90.0, -45.0);
        for text in [
            "execute in minecraft:overworld run tp @s 1.50 2.00 -3.25 90.00 -45.00",
            "  /execute in minecraft:overworld run tp @s 1.50 2.00 -3.25 90.00 -45.00\r\n",
            "\t/execute in minecraft:overworld run tp @s 1.50 2.00 -3.25 90.00 -45.00\n",
            "/execute  in minecraft:overworld\trun tp @s 1.50 2.00 -3.25 90.00 -45.00",
        ] {
            assert_eq!(parse_f3c(text), Ok(expected.clone()), "{text:?}");
        }
    }

    #[test]
    fn modded_dimensions() {
        let parsed = parse_f3c(
            "/execute in twilightforest:twilight_forest run tp @s 1.00 2.00 3.00 0.00 0.00",
        )
        .unwrap();
        assert_eq!(
            parsed.dimension.as_deref(),
            Some("twilightforest:twilight_forest")
        );
        let parsed =
            parse_f3c("/execute in my-mod.v2:some/deep/path_1 run tp @s 1.00 2.00 3.00 0.00 0.00")
                .unwrap();
        assert_eq!(
            parsed.dimension.as_deref(),
            Some("my-mod.v2:some/deep/path_1")
        );
    }

    #[test]
    fn dimension_without_namespace_defaults_to_minecraft() {
        let parsed = parse_f3c("/execute in the_nether run tp @s 1 2 3 4 5").unwrap();
        assert_eq!(parsed.dimension.as_deref(), Some(THE_NETHER));
    }

    #[test]
    fn invalid_dimensions() {
        for dim in [
            "Minecraft:Overworld",
            "minecraft:",
            ":overworld",
            "minecraft:over world",
            "a:b:c",
            "minecraft:ov\u{e9}rworld",
            "@s",
        ] {
            let text = format!("/execute in {dim} run tp @s 1 2 3 4 5");
            assert!(
                matches!(
                    parse_f3c(&text),
                    Err(ParseError::InvalidDimension(_)) | Err(ParseError::NotTeleport)
                ),
                "{text:?} -> {:?}",
                parse_f3c(&text)
            );
        }
        assert_eq!(
            parse_f3c("/execute in minecraft:Overworld run tp @s 1 2 3 4 5"),
            Err(ParseError::InvalidDimension("minecraft:Overworld".into()))
        );
    }

    #[test]
    fn legacy_tp_form() {
        assert_eq!(
            parse_f3c("/tp @s 10.00 70.00 -20.00 0.00 90.00"),
            Ok(loc(None, 10.0, 70.0, -20.0, 0.0, 90.0))
        );
        assert_eq!(
            parse_f3c("tp @s 10.00 70.00 -20.00 0.00 90.00")
                .unwrap()
                .dimension_or_overworld(),
            OVERWORLD
        );
    }

    #[test]
    fn negative_zero() {
        let parsed =
            parse_f3c("/execute in minecraft:overworld run tp @s -0.00 64.00 -0.00 -0.00 0.00")
                .unwrap();
        assert_eq!(parsed.x, 0.0);
        assert!(parsed.x.is_sign_negative());
        assert!(parsed.z.is_sign_negative());
        assert!(parsed.yaw.is_sign_negative());
    }

    #[test]
    fn integers_without_decimals() {
        assert_eq!(
            parse_f3c("/execute in minecraft:overworld run tp @s 100 64 -200 180 0"),
            Ok(loc(Some(OVERWORLD), 100.0, 64.0, -200.0, 180.0, 0.0))
        );
    }

    #[test]
    fn comma_decimal_separator() {
        assert_eq!(
            parse_f3c(
                "/execute in minecraft:overworld run tp @s 123,45 64,00 -987,65 -179,90 12,30"
            ),
            Ok(loc(Some(OVERWORLD), 123.45, 64.0, -987.65, -179.9, 12.3))
        );
    }

    #[test]
    fn large_magnitudes() {
        let parsed = parse_f3c(
            "/execute in minecraft:overworld run tp @s 30000000.00 -64.00 -29999999.99 12345.67 -90.00",
        )
        .unwrap();
        assert_eq!(parsed.x, 30_000_000.0);
        assert_eq!(parsed.z, -29_999_999.99);
        assert_eq!(parsed.yaw, 12345.67);
        // Beyond the world border, but still what the game reported.
        let parsed = parse_f3c("/tp @s 1000000000000.00 0.00 0.00 0.00 0.00").unwrap();
        assert_eq!(parsed.x, 1e12);
    }

    #[test]
    fn rejects_non_finite() {
        for text in [
            "/tp @s NaN 64.00 0.00 0.00 0.00",
            "/tp @s 0.00 Infinity 0.00 0.00 0.00",
            "/tp @s 0.00 64.00 -Infinity 0.00 0.00",
            "/tp @s 0.00 64.00 0.00 inf 0.00",
            "/tp @s 0.00 64.00 0.00 0.00 nan",
        ] {
            assert!(
                matches!(parse_f3c(text), Err(ParseError::InvalidNumber(_))),
                "{text:?}"
            );
        }
        // Overflows to infinity once parsed.
        let huge = format!("/tp @s 1{} 64.00 0.00 0.00 0.00", "0".repeat(400));
        assert!(matches!(
            parse_f3c(&huge),
            Err(ParseError::InvalidNumber(_))
        ));
        let huge_yaw = format!("/tp @s 0.00 64.00 0.00 1{}.00 0.00", "0".repeat(40));
        assert!(matches!(
            parse_f3c(&huge_yaw),
            Err(ParseError::InvalidNumber(_))
        ));
    }

    #[test]
    fn rejects_malformed_numbers() {
        for bad in [
            "1.2.3", "1e5", "+1.00", ".5", "5.", "--1", "-", "1,2,3", "0x10", "1_000", "１２",
        ] {
            let text = format!("/tp @s {bad} 64.00 0.00 0.00 0.00");
            assert_eq!(
                parse_f3c(&text),
                Err(ParseError::InvalidNumber(bad.to_owned())),
                "{text:?}"
            );
        }
    }

    #[test]
    fn rejects_wrong_token_counts() {
        assert_eq!(
            parse_f3c("/execute in minecraft:overworld run tp @s 1.00 2.00 3.00 4.00"),
            Err(ParseError::ArgumentCount(4))
        );
        assert_eq!(
            parse_f3c("/execute in minecraft:overworld run tp @s 1.00 2.00 3.00 4.00 5.00 6.00"),
            Err(ParseError::ArgumentCount(6))
        );
        assert_eq!(parse_f3c("/tp @s"), Err(ParseError::ArgumentCount(0)));
        assert_eq!(
            parse_f3c("/execute in minecraft:overworld run tp @s"),
            Err(ParseError::ArgumentCount(0))
        );
    }

    #[test]
    fn rejects_empty() {
        assert_eq!(parse_f3c(""), Err(ParseError::Empty));
        assert_eq!(parse_f3c(" \r\n\t"), Err(ParseError::Empty));
    }

    #[test]
    fn rejects_other_clipboard_contents() {
        for text in [
            "/",
            "hello world",
            "123.45 64.00 -987.65 -179.90 12.30",
            // Shift+F3+C and F3+I copy other commands.
            "/setblock 10 64 -20 minecraft:stone",
            "/summon minecraft:pig 1.00 2.00 3.00 {}",
            "/execute in minecraft:overworld run tp @p 1.00 2.00 3.00 4.00 5.00",
            "/execute in minecraft:overworld run teleport @s 1.00 2.00 3.00 4.00 5.00",
            "/execute in minecraft:overworld tp @s 1.00 2.00 3.00 4.00 5.00",
            "/execute at @s run tp @s 1.00 2.00 3.00 4.00 5.00",
            "/tp @p 1.00 2.00 3.00 4.00 5.00",
            "/tp Steve 1.00 2.00 3.00 4.00 5.00",
            "/TP @s 1.00 2.00 3.00 4.00 5.00",
            "//tp @s 1.00 2.00 3.00 4.00 5.00",
            "please /tp @s 1.00 2.00 3.00 4.00 5.00",
            // Two lines where each half would be suspicious on its own.
            "/tp @s 1.00 2.00 3.00\n4.00 5.00",
            "/tp @s 1.00 2.00 3.00 4.00 5.00\n/tp @s 1.00 2.00 3.00 4.00 5.00",
        ] {
            assert!(parse_f3c(text).is_err(), "{text:?} parsed");
        }
        assert_eq!(parse_f3c("hello world"), Err(ParseError::NotTeleport));
    }

    #[test]
    fn serde_round_trip() {
        let original = loc(Some(OVERWORLD), 1.5, 2.0, -3.0, 4.5, -5.5);
        let json = serde_json::to_string(&original).unwrap();
        assert_eq!(serde_json::from_str::<Location>(&json).unwrap(), original);
        let legacy: Location =
            serde_json::from_str(r#"{"x":1.0,"y":2.0,"z":3.0,"yaw":0.0,"pitch":0.0}"#).unwrap();
        assert_eq!(legacy.dimension, None);
    }
}
