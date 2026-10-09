//! How far the overlay's own cursor moves for relative mouse motion.
//!
//! With the cursor captured, games read raw mouse counts, which the system's pointer speed
//! and acceleration never touch. The overlay's cursor would then move at a different speed
//! than the real pointer does everywhere else, so it applies the system's settings itself.
//! The Windows rules follow SDL3's `SDL_HINT_MOUSE_RELATIVE_SYSTEM_SCALE` (zlib licence),
//! which approximates Windows' own ballistics per motion event.

/// Gain from mouse counts to pixels.
#[derive(Debug, Clone, PartialEq)]
pub enum PointerSpeed {
    /// Every count moves the pointer this many pixels.
    Linear(f32),
    /// Gain depending on the size of each motion, interpolated between `[size, gain]`
    /// points with increasing sizes (acceleration).
    Curve(Vec<[f32; 2]>),
}

impl Default for PointerSpeed {
    fn default() -> Self {
        Self::RAW
    }
}

/// Multipliers for Windows' pointer speed setting 1–20 (the slider has 11 steps, 1–20 in
/// twos; 10 is the default), when "enhance pointer precision" is off.
const WINDOWS_LINEAR: [f32; 20] = [
    1.0 / 32.0,
    1.0 / 16.0,
    1.0 / 8.0,
    2.0 / 8.0,
    3.0 / 8.0,
    4.0 / 8.0,
    5.0 / 8.0,
    6.0 / 8.0,
    7.0 / 8.0,
    1.0,
    1.25,
    1.5,
    1.75,
    2.0,
    2.25,
    2.5,
    2.75,
    3.0,
    3.25,
    3.5,
];

/// Windows' default acceleration curve (`SmoothMouseXCurve` / `SmoothMouseYCurve` under
/// `HKCU\Control Panel\Mouse`), for when the registry cannot be read.
pub const WINDOWS_DEFAULT_CURVE: ([f32; 5], [f32; 5]) = (
    [0.0, 0.43, 1.25, 3.86, 40.0],
    [0.0, 1.07, 4.14, 18.98, 443.75],
);

impl PointerSpeed {
    /// One pixel per count.
    pub const RAW: Self = Self::Linear(1.0);

    /// The speed Windows gives its pointer: `speed` is `SPI_GETMOUSESPEED` (1–20),
    /// `enhance` whether "enhance pointer precision" is on, and `curve` its acceleration
    /// curve (x: motion, y: pointer movement).
    pub fn windows(speed: u32, enhance: bool, curve: ([f32; 5], [f32; 5])) -> Self {
        let speed = speed.clamp(1, 20);
        if !enhance {
            return Self::Linear(WINDOWS_LINEAR[speed as usize - 1]);
        }
        // SDL3's scaling of the curve to screen pixels at 96 dpi.
        const DISPLAY_FACTOR: f32 = 3.5 * (150.0 / 96.0);
        let scale = speed as f32 / 10.0;
        let (xs, ys) = curve;
        let points: Vec<[f32; 2]> = xs
            .iter()
            .zip(&ys)
            .map(|(&x, &y)| {
                let gain = if x > 0.0 { y / x * scale } else { 0.0 };
                [x, gain / DISPLAY_FACTOR]
            })
            .collect();
        let sane = points.iter().all(|p| p[0].is_finite() && p[1].is_finite())
            && points.windows(2).all(|w| w[0][0] < w[1][0]);
        if sane {
            Self::Curve(points)
        } else {
            Self::windows(speed, true, WINDOWS_DEFAULT_CURVE)
        }
    }

    /// The pixels to move for a motion of `(dx, dy)` counts.
    pub fn apply(&self, dx: f32, dy: f32) -> (f32, f32) {
        let gain = self.gain((dx * dx + dy * dy).sqrt());
        if gain.is_finite() && gain >= 0.0 {
            (dx * gain, dy * gain)
        } else {
            (dx, dy)
        }
    }

    fn gain(&self, size: f32) -> f32 {
        let points = match self {
            Self::Linear(gain) => return *gain,
            Self::Curve(points) => points.as_slice(),
        };
        let (Some(first), Some(last)) = (points.first(), points.last()) else {
            return 1.0;
        };
        // Not a number: no point of the curve to look between (it would index before the
        // first one).
        if size.is_nan() {
            return 1.0;
        }
        if size <= first[0] {
            return first[1];
        }
        if size >= last[0] {
            return last[1];
        }
        let i = points.partition_point(|p| p[0] <= size);
        let ([x0, g0], [x1, g1]) = (points[i - 1], points[i]);
        g0 + (size - x0) / (x1 - x0) * (g1 - g0)
    }
}

/// One of Windows' `SmoothMouse?Curve` registry values: five 8-byte 16.16 fixed-point
/// numbers.
pub fn parse_windows_curve(bytes: &[u8]) -> Option<[f32; 5]> {
    if bytes.len() < 40 {
        return None;
    }
    let mut values = [0.0; 5];
    let (chunks, _) = bytes.as_chunks::<8>();
    for (value, chunk) in values.iter_mut().zip(chunks) {
        let fixed = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        *value = fixed as f32 / 65536.0;
    }
    Some(values)
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_motion_that_is_not_a_number_does_not_panic() {
        let speed = super::PointerSpeed::windows(10, true, super::WINDOWS_DEFAULT_CURVE);
        let (dx, dy) = speed.apply(f32::NAN, 1.0);
        assert!(dx.is_nan() || dx.is_finite());
        assert!(dy.is_finite() || dy.is_nan());
    }

    use super::*;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-3
    }

    #[test]
    fn raw_moves_one_pixel_per_count() {
        assert_eq!(PointerSpeed::RAW.apply(3.0, -4.0), (3.0, -4.0));
        assert_eq!(PointerSpeed::default(), PointerSpeed::RAW);
    }

    #[test]
    fn windows_linear_speeds() {
        let default = PointerSpeed::windows(10, false, WINDOWS_DEFAULT_CURVE);
        assert_eq!(default, PointerSpeed::Linear(1.0));
        assert_eq!(default.apply(5.0, 0.0), (5.0, 0.0));
        let slow = PointerSpeed::windows(6, false, WINDOWS_DEFAULT_CURVE);
        assert_eq!(slow.apply(4.0, -2.0), (2.0, -1.0));
        assert_eq!(
            PointerSpeed::windows(20, false, WINDOWS_DEFAULT_CURVE),
            PointerSpeed::Linear(3.5)
        );
        // Out of range values from a broken setting stay usable.
        assert_eq!(
            PointerSpeed::windows(0, false, WINDOWS_DEFAULT_CURVE),
            PointerSpeed::Linear(1.0 / 32.0)
        );
        assert_eq!(
            PointerSpeed::windows(99, false, WINDOWS_DEFAULT_CURVE),
            PointerSpeed::Linear(3.5)
        );
    }

    #[test]
    fn windows_acceleration_speeds_up_fast_motion() {
        let speed = PointerSpeed::windows(10, true, WINDOWS_DEFAULT_CURVE);
        let (slow, _) = speed.apply(1.0, 0.0);
        let (fast, _) = speed.apply(20.0, 0.0);
        assert!(slow > 0.3 && slow < 1.0, "{slow}");
        assert!(fast / 20.0 > slow, "{fast}");
        // Direction is kept.
        let (x, y) = speed.apply(-3.0, 4.0);
        assert!(x < 0.0 && y > 0.0 && close(x / y, -0.75));
        // The pointer speed setting scales the whole curve.
        let (faster, _) = PointerSpeed::windows(20, true, WINDOWS_DEFAULT_CURVE).apply(1.0, 0.0);
        assert!(close(faster, slow * 2.0), "{faster} {slow}");
    }

    #[test]
    fn curve_is_interpolated_and_clamped() {
        let speed = PointerSpeed::Curve(vec![[0.0, 0.0], [2.0, 1.0], [4.0, 3.0]]);
        assert!(close(speed.gain(1.0), 0.5));
        assert!(close(speed.gain(3.0), 2.0));
        assert!(close(speed.gain(10.0), 3.0));
        assert!(close(speed.gain(0.0), 0.0));
        assert_eq!(PointerSpeed::Curve(vec![]).apply(2.0, 0.0), (2.0, 0.0));
    }

    #[test]
    fn broken_curve_falls_back_to_the_default() {
        let broken = PointerSpeed::windows(10, true, ([0.0, 2.0, 1.0, 3.0, 4.0], [0.0; 5]));
        assert_eq!(
            broken,
            PointerSpeed::windows(10, true, WINDOWS_DEFAULT_CURVE)
        );
    }

    #[test]
    fn parses_the_registry_format() {
        // Windows' default SmoothMouseXCurve.
        let bytes = [
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, //
            0x15, 0x6e, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, //
            0x00, 0x40, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, //
            0x29, 0xdc, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, //
            0x00, 0x00, 0x28, 0x00, 0x00, 0x00, 0x00, 0x00,
        ];
        let curve = parse_windows_curve(&bytes).unwrap();
        for (got, want) in curve.iter().zip(WINDOWS_DEFAULT_CURVE.0) {
            assert!(close(*got, want), "{curve:?}");
        }
        assert_eq!(parse_windows_curve(&bytes[..39]), None);
    }
}
