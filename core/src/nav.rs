//! ウェイポイントへの距離と方角、オーバーワールドとネザーの座標の変換。
//!
//! Minecraft の yaw の決まり：0 は +Z（南）、90 は -X（西）、180/-180 は -Z（北）、-90 は
//! +X（東）を向く。右に回ると yaw が増える。

use std::borrow::Cow;
use std::fmt;

use crate::location::{Location, OVERWORLD, THE_NETHER};

/// 8 方位の方角。
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

    /// Minecraft の yaw が指す方角。
    pub fn from_yaw(yaw: f32) -> Cardinal {
        // 方位角：0 = 北、時計回り。yaw を半回転ずらしたもの。
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

/// プレイヤーから見た目標の位置。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bearing {
    pub horizontal_distance: f64,
    pub distance_3d: f64,
    /// 目標の高さからプレイヤーの高さを引いたもの。
    pub dy: f64,
    /// 目標を向くのに要る yaw。(-180, 180] の中。
    pub target_yaw: f32,
    /// `target_yaw` からプレイヤーの yaw を引き、(-180, 180] に収めたもの。正なら右に回る。
    pub relative_yaw: f32,
    pub cardinal: Cardinal,
}

/// プレイヤーの位置から `to`（`[x, y, z]`）への距離と方角を求める。
///
/// 目標が真上か真下にあると方角は決まらないので、プレイヤー自身の yaw を使う
/// （`relative_yaw` は 0 になる）。
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
    // 狭めると -179.99999999 などが -180.0 に丸まりうるので、f32 でもう一度収める。
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

/// 水平の座標を、オーバーワールドとネザーの間で変換する（1:8）。
///
/// 同じディメンションならそのまま返し、ほかの組なら `None`。namespace のないディメンションの id
/// は `minecraft:` とみなす。
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

/// プレイヤーの位置からウェイポイントへの行き方。
#[derive(Debug, Clone, PartialEq)]
pub enum Guide {
    /// 同じディメンションか、オーバーワールドとネザーの間で変換したもの（`converted` = プレイヤーの
    /// ディメンションでの目標の x,z）。
    Bearing {
        bearing: Bearing,
        converted: Option<(f64, f64)>,
    },
    /// ウェイポイントが、プレイヤーのものへの座標の変換がないディメンションにある。
    OtherDimension,
}

/// `to`（`to_dimension` の `[x, y, z]`）のウェイポイントが、プレイヤーから見てどこにあるか。
/// オーバーワールドとネザーのもう一方にあるウェイポイントは、プレイヤーのディメンションに変換
/// する（[`convert_xz`]）。Y は変換しない。
pub fn guide(from: &Location, to_dimension: &str, to: [f64; 3]) -> Guide {
    let here = from.dimension_or_overworld();
    match convert_xz(to[0], to[2], to_dimension, here) {
        Some((x, z)) => Guide::Bearing {
            bearing: bearing(from, [x, to[1], z]),
            converted: (with_namespace(to_dimension) != with_namespace(here)).then_some((x, z)),
        },
        None => Guide::OtherDimension,
    }
}

fn with_namespace(id: &str) -> Cow<'_, str> {
    if id.contains(':') {
        Cow::Borrowed(id)
    } else {
        Cow::Owned(format!("minecraft:{id}"))
    }
}

/// 度の角度を (-180, 180] に収める。有限でない値は NaN。`-0.0` は `0.0` になる。
pub fn wrap_degrees(a: f32) -> f32 {
    let r = a % 360.0;
    // どちらの調整も正確（Sterbenz の補題）なので、結果が -180 になることはない。
    if r > 180.0 {
        r - 360.0
    } else if r <= -180.0 {
        r + 360.0
    } else {
        // +0.0 を足すと -0.0 が 0.0 になる。そうしないと "-0" と表示される。
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
    use crate::location::THE_END;

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
        // `-0` はオーバーレイで "-0°" と表示されてしまう。
        for a in [-0.0, 0.0, 360.0, -360.0, -720.0] {
            assert!(wrap_degrees(a).is_sign_positive(), "{a}");
        }
        // 同じ x で真南：atan2(-0.0, dz) は -0.0。
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
        // 南を向いている。西（yaw 90）の目標には右に回る。
        let facing_south = at(0.0, 64.0, 0.0, 0.0);
        assert_close(
            bearing(&facing_south, [-10.0, 64.0, 0.0]).relative_yaw,
            90.0,
        );
        assert_close(
            bearing(&facing_south, [10.0, 64.0, 0.0]).relative_yaw,
            -90.0,
        );
        // 真後ろは +180。-180 にはならない。
        assert_eq!(
            bearing(&facing_south, [0.0, 64.0, -10.0]).relative_yaw,
            180.0
        );
        // 東を向いて目標は南：右に 90 回る。
        let facing_east = at(0.0, 64.0, 0.0, -90.0);
        assert_close(bearing(&facing_east, [0.0, 64.0, 10.0]).relative_yaw, 90.0);
    }

    #[test]
    fn relative_yaw_across_the_wrap_boundary() {
        // 目標は北の少し東（yaw -179）、プレイヤーは北の少し西（yaw 179）。
        let target = [0.017_452_406, 64.0, -1.0]; // tan(1 度)
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
        // 古い版は、720 + 90 のように積み重なった yaw を報告する。
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
        // 負のゼロで、方角が -180 に裏返ってはならない。
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

    fn in_dimension(dimension: Option<&str>, x: f64, y: f64, z: f64, yaw: f32) -> Location {
        Location {
            dimension: dimension.map(str::to_owned),
            ..at(x, y, z, yaw)
        }
    }

    #[test]
    fn guide_in_the_same_dimension() {
        let player = at(0.0, 64.0, 0.0, 0.0);
        assert_eq!(
            guide(&player, OVERWORLD, [30.0, 70.0, 40.0]),
            Guide::Bearing {
                bearing: bearing(&player, [30.0, 70.0, 40.0]),
                converted: None,
            }
        );
        // namespace のない id と、ディメンションのない F3+C の古い形（オーバーワールド）。
        let old_form = in_dimension(None, 0.0, 64.0, 0.0, 0.0);
        assert_eq!(
            guide(&old_form, "overworld", [3.0, 64.0, 4.0]),
            Guide::Bearing {
                bearing: bearing(&old_form, [3.0, 64.0, 4.0]),
                converted: None,
            }
        );
        let end = in_dimension(Some(THE_END), 0.0, 64.0, 0.0, 0.0);
        assert!(matches!(
            guide(&end, "the_end", [0.0, 64.0, 5.0]),
            Guide::Bearing {
                converted: None,
                ..
            }
        ));
    }

    #[test]
    fn guide_converts_between_overworld_and_nether() {
        // ネザーでは、オーバーワールドの (800, 64, -80) のウェイポイントは (100, -10)。
        let player = in_dimension(Some(THE_NETHER), 100.0, 40.0, 0.0, 0.0);
        let Guide::Bearing {
            bearing: b,
            converted,
        } = guide(&player, OVERWORLD, [800.0, 64.0, -80.0])
        else {
            panic!("expected a bearing");
        };
        assert_eq!(converted, Some((100.0, -10.0)));
        assert_eq!(b, bearing(&player, [100.0, 64.0, -10.0]));
        assert_eq!(b.horizontal_distance, 10.0);
        assert_eq!(b.cardinal, Cardinal::N);
        // Y は変換しない。
        assert_eq!(b.dy, 24.0);

        // オーバーワールドでは、ネザーのウェイポイントを 8 倍する。
        let player = at(0.0, 64.0, 0.0, 0.0);
        let Guide::Bearing {
            bearing: b,
            converted,
        } = guide(&player, THE_NETHER, [-2.0, 100.0, 1.5])
        else {
            panic!("expected a bearing");
        };
        assert_eq!(converted, Some((-16.0, 12.0)));
        assert_eq!(b.horizontal_distance, 20.0);
        assert_eq!(b.dy, 36.0);
    }

    #[test]
    fn guide_to_other_dimensions() {
        let overworld = at(0.0, 64.0, 0.0, 0.0);
        assert_eq!(
            guide(&overworld, THE_END, [0.0, 64.0, 0.0]),
            Guide::OtherDimension
        );
        assert_eq!(
            guide(&overworld, "mod:dim", [0.0, 64.0, 0.0]),
            Guide::OtherDimension
        );
        let end = in_dimension(Some(THE_END), 0.0, 64.0, 0.0, 0.0);
        assert_eq!(
            guide(&end, OVERWORLD, [0.0, 64.0, 0.0]),
            Guide::OtherDimension
        );
        let nether = in_dimension(Some(THE_NETHER), 0.0, 64.0, 0.0, 0.0);
        assert_eq!(
            guide(&nether, THE_END, [0.0, 64.0, 0.0]),
            Guide::OtherDimension
        );
    }
}
