//! Vector maths shared by the scene, particle and viewer code.
//!
//! On bare `[f32; 3]`, not a newtype: these arrays cross the wgpu and
//! steamworks boundaries as-is.

/// A Source engine QAngle: the `[pitch, yaw, roll]` degree triple that map
/// entities and static props store their orientation as.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct QAngle {
    pub pitch: f32,
    pub yaw: f32,
    pub roll: f32,
}

impl QAngle {
    /// Reads the `[pitch, yaw, roll]` degree layout, yielding radians.
    #[must_use]
    pub fn from_source_degrees(angles: [f32; 3]) -> Self {
        Self {
            pitch: angles[0].to_radians(),
            yaw: angles[1].to_radians(),
            roll: angles[2].to_radians(),
        }
    }
}

#[must_use]
pub fn add(left: [f32; 3], right: [f32; 3]) -> [f32; 3] {
    [left[0] + right[0], left[1] + right[1], left[2] + right[2]]
}

#[must_use]
pub fn sub(left: [f32; 3], right: [f32; 3]) -> [f32; 3] {
    [left[0] - right[0], left[1] - right[1], left[2] - right[2]]
}

#[must_use]
pub fn scale(vector: [f32; 3], scalar: f32) -> [f32; 3] {
    [vector[0] * scalar, vector[1] * scalar, vector[2] * scalar]
}

#[must_use]
pub fn lerp(start: [f32; 3], end: [f32; 3], fraction: f32) -> [f32; 3] {
    add(start, scale(sub(end, start), fraction))
}

#[must_use]
pub fn cross(left: [f32; 3], right: [f32; 3]) -> [f32; 3] {
    [
        left[1] * right[2] - left[2] * right[1],
        left[2] * right[0] - left[0] * right[2],
        left[0] * right[1] - left[1] * right[0],
    ]
}

#[must_use]
pub fn dot(left: [f32; 3], right: [f32; 3]) -> f32 {
    left[0] * right[0] + left[1] * right[1] + left[2] * right[2]
}

/// `dot` with the left operand's components taken as magnitudes — Source's
/// plane/AABB overlap test, where the box extent projects onto the plane
/// normal regardless of which way the normal points.
#[must_use]
pub fn dot_abs(left: [f32; 3], right: [f32; 3]) -> f32 {
    left[0].abs() * right[0] + left[1].abs() * right[1] + left[2].abs() * right[2]
}

#[must_use]
pub fn length_squared(vector: [f32; 3]) -> f32 {
    dot(vector, vector)
}

#[must_use]
pub fn length(vector: [f32; 3]) -> f32 {
    length_squared(vector).sqrt()
}

#[must_use]
pub fn distance_squared(left: [f32; 3], right: [f32; 3]) -> f32 {
    length_squared(sub(left, right))
}

#[must_use]
pub fn distance(left: [f32; 3], right: [f32; 3]) -> f32 {
    distance_squared(left, right).sqrt()
}

/// `None` for a vector too short to have a direction.
///
/// Fallible because callers disagree on the degenerate case: a camera basis
/// wants a usable axis, a surface normal wants zero.
#[must_use]
pub fn normalize(vector: [f32; 3]) -> Option<[f32; 3]> {
    let length = length(vector);
    (length > f32::EPSILON).then(|| scale(vector, 1.0 / length))
}

/// A direction, or the zero vector when there is none. Prefer [`normalize`]
/// where the caller can say something better than zero.
#[must_use]
pub fn normalize_or_zero(vector: [f32; 3]) -> [f32; 3] {
    normalize(vector).unwrap_or([0.0; 3])
}

#[must_use]
pub fn rotate_x(vector: [f32; 3], radians: f32) -> [f32; 3] {
    let (sin, cos) = radians.sin_cos();
    [
        vector[0],
        vector[1] * cos - vector[2] * sin,
        vector[1] * sin + vector[2] * cos,
    ]
}

#[must_use]
pub fn rotate_y(vector: [f32; 3], radians: f32) -> [f32; 3] {
    let (sin, cos) = radians.sin_cos();
    [
        vector[0] * cos + vector[2] * sin,
        vector[1],
        -vector[0] * sin + vector[2] * cos,
    ]
}

#[must_use]
pub fn rotate_z(vector: [f32; 3], radians: f32) -> [f32; 3] {
    let (sin, cos) = radians.sin_cos();
    [
        vector[0] * cos - vector[1] * sin,
        vector[0] * sin + vector[1] * cos,
        vector[2],
    ]
}

/// Applies a Source entity's `angles` to a vector, in Source's roll-pitch-yaw
/// order. Applying the three rotations in any other order gives a different
/// result for anything but a single-axis angle.
#[must_use]
pub fn rotate_source_vector(vector: [f32; 3], angles: [f32; 3]) -> [f32; 3] {
    let QAngle { pitch, yaw, roll } = QAngle::from_source_degrees(angles);
    rotate_z(rotate_y(rotate_x(vector, roll), pitch), yaw)
}

/// Source's `SimpleSpline` ease. Clamped: the curve turns back on itself
/// outside 0..=1, so an unclamped `t` of 1.5 eases backwards.
#[must_use]
pub fn simple_spline(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The degenerate case is the whole reason this returns `Option`.
    #[test]
    fn normalize_reports_a_vector_with_no_direction_rather_than_choosing_one() {
        assert_eq!(normalize([0.0; 3]), None);
        assert_eq!(normalize([f32::EPSILON, 0.0, 0.0]), None);
        assert_eq!(normalize([0.0, 5.0, 0.0]), Some([0.0, 1.0, 0.0]));
    }

    /// Outside 0..=1 the raw polynomial reverses direction, so an unclamped
    /// spline eases backwards rather than saturating.
    #[test]
    fn simple_spline_saturates_outside_the_unit_interval() {
        assert_eq!(simple_spline(-1.0), 0.0);
        assert_eq!(simple_spline(0.0), 0.0);
        assert_eq!(simple_spline(1.0), 1.0);
        assert_eq!(simple_spline(2.0), 1.0);
        assert!((simple_spline(0.5) - 0.5).abs() < f32::EPSILON);
    }

    /// Source applies roll, then pitch, then yaw. Any other order agrees when
    /// at most one angle is non-zero, so a swap is invisible on the
    /// single-axis cases and wrong on every real prop. Values are hand-derived
    /// rather than composed from the same helpers.
    #[test]
    fn source_rotation_order_is_roll_pitch_yaw() {
        // 90° yaw alone takes +X to +Y.
        assert_close(
            rotate_source_vector([1.0, 0.0, 0.0], [0.0, 90.0, 0.0]),
            [0.0, 1.0, 0.0],
        );
        // 90° pitch alone takes +X to -Z (Source pitches nose-down).
        assert_close(
            rotate_source_vector([1.0, 0.0, 0.0], [90.0, 0.0, 0.0]),
            [0.0, 0.0, -1.0],
        );
        // 90° roll alone leaves +X fixed and takes +Y to +Z.
        assert_close(
            rotate_source_vector([0.0, 1.0, 0.0], [0.0, 0.0, 90.0]),
            [0.0, 0.0, 1.0],
        );

        // Roll-then-pitch-then-yaw: +X rolls to +X, pitches to -Z, and yaw
        // leaves -Z alone. Applying the same angles in the reverse order lands
        // somewhere else entirely, which is the whole point.
        let combined = [90.0, 90.0, 90.0];
        assert_close(
            rotate_source_vector([1.0, 0.0, 0.0], combined),
            [0.0, 0.0, -1.0],
        );
        assert_ne!(
            rotate_source_vector([1.0, 0.0, 0.0], combined),
            rotate_x(
                rotate_y(
                    rotate_z([1.0, 0.0, 0.0], 90_f32.to_radians()),
                    90_f32.to_radians()
                ),
                90_f32.to_radians()
            )
        );
    }

    #[track_caller]
    fn assert_close(actual: [f32; 3], expected: [f32; 3]) {
        for axis in 0..3 {
            assert!(
                (actual[axis] - expected[axis]).abs() < 1e-5,
                "{actual:?} != {expected:?}"
            );
        }
    }
}
