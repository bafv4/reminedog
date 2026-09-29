//! Distance and direction to a waypoint, and overworld/nether coordinate conversion.
//!
//! Minecraft's yaw convention: 0 faces +Z (south), 90 faces -X (west), 180/-180 faces -Z
//! (north) and -90 faces +X (east). Turning right increases yaw.

use std::borrow::Cow;
use std::fmt;

use crate::location::{Location, OVERWORLD, THE_NETHER};

/// Eight-point compass direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Cardinal {
    N,
    NE,
    E,
    SE,
    S,
    SW,
    W,
    NW,
}

impl Cardinal {
    const CLOCKWISE_FROM_NORTH: [Cardinal; 8] = [
        Cardinal::N,
        Cardinal::NE,
        Cardinal::E,
        Cardinal::SE,
        Cardinal::S,
        Cardinal::SW,
        Cardinal::W,
        Cardinal::NW,
    ];

    /// The direction a Minecraft yaw points to.
    pub fn from_yaw(yaw: f32) -> Cardinal {
        // Compass bearing: 0 = north, clockwise, which is yaw shifted by half a turn.
        let compass = (f64::from(yaw) + 180.0).rem_euclid(360.0);
        let sector = ((compass + 22.5) / 45.0).floor() as usize % 8;
        Self::CLOCKWISE_FROM_NORTH[sector]
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Cardinal::N => "N",
            Cardinal::NE => "NE",
            Cardinal::E => "E",
            Cardinal::SE => "SE",
            Cardinal::S => "S",
            Cardinal::SW => "SW",
            Cardinal::W => "W",
            Cardinal::NW => "NW",
        }
    }
}

impl fmt::Display for Cardinal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Where a target lies relative to the player.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bearing {
    pub horizontal_distance: f64,
    pub distance_3d: f64,
    /// Target height minus player height.
    pub dy: f64,
    /// Yaw the player would need to face the target, in (-180, 180].
    pub target_yaw: f32,
    /// `target_yaw` minus the player's yaw, wrapped to (-180, 180]; positive = turn right.
    pub relative_yaw: f32,
    pub cardinal: Cardinal,
}

/// Computes distance and direction from the player's location to `to` (`[x, y, z]`).
///
/// When the target is straight above or below, the direction is undefined and the
/// player's own yaw is used (so `relative_yaw` is 0).
pub fn bearing(from: &Location, to: [f64; 3]) -> Bearing {
    let dx = to[0] - from.x;
    let dy = to[1] - from.y;
    let dz = to[2] - from.z;
    let horizontal_distance = dx.hypot(dz);
    let distance_3d = horizontal_distance.hypot(dy);
    let player_yaw = wrap_degrees_f64(f64::from(from.yaw));
    let target_yaw = if horizontal_distance > 0.0 {
        wrap_degrees_f64((-dx).atan2(dz).to_degrees())
    } else {
        player_yaw
    };
    // Narrowing can round e.g. -179.99999999 to -180.0, so wrap again in f32.
    let relative_yaw = wrap_degrees(wrap_degrees_f64(target_yaw - player_yaw) as f32);
    let target_yaw = wrap_degrees(target_yaw as f32);
    Bearing {
        horizontal_distance,
        distance_3d,
        dy,
        target_yaw,
        relative_yaw,
        cardinal: Cardinal::from_yaw(target_yaw),
    }
}

/// Converts horizontal coordinates between the overworld and the nether (1:8).
///
/// Returns them unchanged for the same dimension and `None` for any other pair.
/// Dimension ids without a namespace are taken as `minecraft:`.
pub fn convert_xz(x: f64, z: f64, from_dim: &str, to_dim: &str) -> Option<(f64, f64)> {
    let from = with_namespace(from_dim);
    let to = with_namespace(to_dim);
    if from == to {
        return Some((x, z));
    }
    match (from.as_ref(), to.as_ref()) {
        (OVERWORLD, THE_NETHER) => Some((x / 8.0, z / 8.0)),
        (THE_NETHER, OVERWORLD) => Some((x * 8.0, z * 8.0)),
        _ => None,
    }
}

fn with_namespace(id: &str) -> Cow<'_, str> {
    if id.contains(':') {
        Cow::Borrowed(id)
    } else {
        Cow::Owned(format!("minecraft:{id}"))
    }
}

/// Wraps an angle in degrees to (-180, 180]. Non-finite input yields NaN; `-0.0` becomes `0.0`.
pub fn wrap_degrees(a: f32) -> f32 {
    let r = a % 360.0;
    // Both adjustments are exact (Sterbenz), so the result never lands on -180.
    if r > 180.0 {
        r - 360.0
    } else if r <= -180.0 {
        r + 360.0
    } else {
        // Adding +0.0 turns -0.0 into 0.0, which would otherwise be displayed as "-0".
        r + 0.0
    }
}

fn wrap_degrees_f64(a: f64) -> f64 {
    let r = a % 360.0;
    if r > 180.0 {
        r - 360.0
    } else if r <= -180.0 {
        r + 360.0
    } else {
        r + 0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(x: f64, y: f64, z: f64, yaw: f32) -> Location {
        Location {
            dimension: Some(OVERWORLD.into()),
            x,
            y,
            z,
            yaw,
            pitch: 0.0,
        }
    }

    fn assert_close(actual: f32, expected: f32) {
        assert!((actual - expected).abs() < 1e-3, "{actual} != {expected}");
    }

    #[test]
    fn wrap_degrees_range() {
        let cases = [
            (0.0, 0.0),
            (90.0, 90.0),
            (180.0, 180.0),
            (-180.0, 180.0),
            (-179.9, -179.9),
            (180.1, -179.9),
            (270.0, -90.0),
            (-270.0, 90.0),
            (360.0, 0.0),
            (540.0, 180.0),
            (-540.0, 180.0),
            (720.0 + 45.0, 45.0),
            (-3600.0 - 45.0, -45.0),
        ];
        for (input, expected) in cases {
            assert_close(wrap_degrees(input), expected);
        }
        assert_eq!(wrap_degrees(-180.0), 180.0);
        assert_eq!(wrap_degrees(180.0), 180.0);
        assert!(wrap_degrees(f32::NAN).is_nan());
        assert!(wrap_degrees(f32::INFINITY).is_nan());
    }

    #[test]
    fn wrap_degrees_never_returns_minus_180() {
        let mut a = -1000.0f32;
        while a < 1000.0 {
            let w = wrap_degrees(a);
            assert!(w > -180.0 && w <= 180.0, "{a} -> {w}");
            a += 0.37;
        }
        for bits_off in 0..64 {
            let a = f32::from_bits((-180.0f32).to_bits() - bits_off);
            let w = wrap_degrees(a);
            assert!(w > -180.0 && w <= 180.0, "{a} -> {w}");
            let a = f32::from_bits((-180.0f32).to_bits() + bits_off);
            let w = wrap_degrees(a);
            assert!(w > -180.0 && w <= 180.0, "{a} -> {w}");
        }
    }

    #[test]
    fn no_negative_zero_angles() {
        // `-0` would be shown as "-0°" in the overlay.
        for a in [-0.0, 0.0, 360.0, -360.0, -720.0] {
            assert!(wrap_degrees(a).is_sign_positive(), "{a}");
        }
        // Due south on the same x: atan2(-0.0, dz) is -0.0.
        let b = bearing(&at(0.0, 64.0, 0.0, 0.0), [0.0, 64.0, 10.0]);
        assert!(b.target_yaw.is_sign_positive());
        assert!(b.relative_yaw.is_sign_positive());
        let b = bearing(&at(5.0, 64.0, 0.0, -0.0), [5.0, 64.0, 10.0]);
        assert!(b.relative_yaw.is_sign_positive());
    }

    #[test]
    fn four_cardinal_directions() {
        let player = at(0.0, 64.0, 0.0, 0.0);
        let south = bearing(&player, [0.0, 64.0, 10.0]);
        assert_close(south.target_yaw, 0.0);
        assert_eq!(south.cardinal, Cardinal::S);
        let west = bearing(&player, [-10.0, 64.0, 0.0]);
        assert_close(west.target_yaw, 90.0);
        assert_eq!(west.cardinal, Cardinal::W);
        let north = bearing(&player, [0.0, 64.0, -10.0]);
        assert_eq!(north.target_yaw, 180.0);
        assert_eq!(north.cardinal, Cardinal::N);
        let east = bearing(&player, [10.0, 64.0, 0.0]);
        assert_close(east.target_yaw, -90.0);
        assert_eq!(east.cardinal, Cardinal::E);
    }

    #[test]
    fn diagonals() {
        let player = at(0.0, 64.0, 0.0, 0.0);
        let cases = [
            ([10.0, -10.0], -135.0, Cardinal::NE),
            ([10.0, 10.0], -45.0, Cardinal::SE),
            ([-10.0, 10.0], 45.0, Cardinal::SW),
            ([-10.0, -10.0], 135.0, Cardinal::NW),
        ];
        for ([x, z], yaw, cardinal) in cases {
            let b = bearing(&player, [x, 64.0, z]);
            assert_close(b.target_yaw, yaw);
            assert_eq!(b.cardinal, cardinal, "{x},{z}");
        }
    }

    #[test]
    fn relative_yaw_turn_direction() {
        // Facing south; a target to the west (yaw 90) needs a right turn.
        let facing_south = at(0.0, 64.0, 0.0, 0.0);
        assert_close(
            bearing(&facing_south, [-10.0, 64.0, 0.0]).relative_yaw,
            90.0,
        );
        assert_close(
            bearing(&facing_south, [10.0, 64.0, 0.0]).relative_yaw,
            -90.0,
        );
        // Straight behind is +180, never -180.
        assert_eq!(
            bearing(&facing_south, [0.0, 64.0, -10.0]).relative_yaw,
            180.0
        );
        // Facing east, target south: turn right 90.
        let facing_east = at(0.0, 64.0, 0.0, -90.0);
        assert_close(bearing(&facing_east, [0.0, 64.0, 10.0]).relative_yaw, 90.0);
    }

    #[test]
    fn relative_yaw_across_the_wrap_boundary() {
        // Target just east of north (yaw -179), player just west of north (yaw 179).
        let target = [0.017_452_406, 64.0, -1.0]; // tan(1 degree)
        let player = at(0.0, 64.0, 0.0, 179.0);
        let b = bearing(&player, target);
        assert_close(b.target_yaw, -179.0);
        assert_close(b.relative_yaw, 2.0);
        let player = at(0.0, 64.0, 0.0, -179.0);
        let b = bearing(&player, [-0.017_452_406, 64.0, -1.0]);
        assert_close(b.target_yaw, 179.0);
        assert_close(b.relative_yaw, -2.0);
    }

    #[test]
    fn unwrapped_player_yaw() {
        // Older versions report accumulated yaw such as 720 + 90.
        let player = at(0.0, 64.0, 0.0, 810.0);
        let b = bearing(&player, [-10.0, 64.0, 0.0]);
        assert_close(b.relative_yaw, 0.0);
        let player = at(0.0, 64.0, 0.0, -12345.0);
        let b = bearing(&player, [0.0, 64.0, 10.0]);
        assert_close(b.relative_yaw, wrap_degrees(12345.0));
        assert!(b.relative_yaw > -180.0 && b.relative_yaw <= 180.0);
    }

    #[test]
    fn distances() {
        let player = at(100.0, 64.0, 100.0, 0.0);
        let b = bearing(&player, [103.0, 76.0, 104.0]);
        assert_eq!(b.horizontal_distance, 5.0);
        assert_eq!(b.distance_3d, 13.0);
        assert_eq!(b.dy, 12.0);
        let below = bearing(&player, [103.0, 52.0, 104.0]);
        assert_eq!(below.dy, -12.0);
        assert_eq!(below.distance_3d, 13.0);
    }

    #[test]
    fn target_straight_above_uses_player_yaw() {
        let player = at(5.0, 64.0, 5.0, 33.0);
        let b = bearing(&player, [5.0, 80.0, 5.0]);
        assert_eq!(b.horizontal_distance, 0.0);
        assert_eq!(b.distance_3d, 16.0);
        assert_close(b.target_yaw, 33.0);
        assert_eq!(b.relative_yaw, 0.0);
        // Negative zeros must not flip the direction to -180.
        let player = at(0.0, 64.0, 0.0, -180.0);
        let b = bearing(&player, [-0.0, 64.0, -0.0]);
        assert_eq!(b.target_yaw, 180.0);
        assert_eq!(b.relative_yaw, 0.0);
    }

    #[test]
    fn cardinal_sector_boundaries() {
        assert_eq!(Cardinal::from_yaw(180.0), Cardinal::N);
        assert_eq!(Cardinal::from_yaw(-180.0), Cardinal::N);
        assert_eq!(Cardinal::from_yaw(157.6), Cardinal::N);
        assert_eq!(Cardinal::from_yaw(157.4), Cardinal::NW);
        assert_eq!(Cardinal::from_yaw(-157.4), Cardinal::NE);
        assert_eq!(Cardinal::from_yaw(-90.0), Cardinal::E);
        assert_eq!(Cardinal::from_yaw(0.0), Cardinal::S);
        assert_eq!(Cardinal::from_yaw(22.4), Cardinal::S);
        assert_eq!(Cardinal::from_yaw(22.6), Cardinal::SW);
        assert_eq!(Cardinal::from_yaw(90.0), Cardinal::W);
        assert_eq!(Cardinal::from_yaw(450.0), Cardinal::W);
        assert_eq!(Cardinal::NW.to_string(), "NW");
    }

    #[test]
    fn nether_conversion() {
        assert_eq!(
            convert_xz(800.0, -80.0, OVERWORLD, THE_NETHER),
            Some((100.0, -10.0))
        );
        assert_eq!(
            convert_xz(100.0, -10.0, THE_NETHER, OVERWORLD),
            Some((800.0, -80.0))
        );
        assert_eq!(
            convert_xz(12.0, 3.0, "overworld", "the_nether"),
            Some((1.5, 0.375))
        );
        assert_eq!(convert_xz(1.0, 2.0, OVERWORLD, OVERWORLD), Some((1.0, 2.0)));
        assert_eq!(
            convert_xz(1.0, 2.0, "minecraft:the_end", "the_end"),
            Some((1.0, 2.0))
        );
        assert_eq!(convert_xz(1.0, 2.0, "mod:dim", "mod:dim"), Some((1.0, 2.0)));
        assert_eq!(convert_xz(1.0, 2.0, OVERWORLD, "minecraft:the_end"), None);
        assert_eq!(convert_xz(1.0, 2.0, "minecraft:the_end", THE_NETHER), None);
        assert_eq!(convert_xz(1.0, 2.0, "mod:dim", OVERWORLD), None);
    }
}
