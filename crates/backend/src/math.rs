//! Vector maths shared by the scene, particle and viewer code.
//!
//! [`Vec3`] is `repr(transparent)` over `[f32; 3]` and `Pod`, so it is the
//! same bytes as the array it wraps and crosses the wgpu and steamworks
//! boundaries unchanged. Being a distinct type is what lets the arithmetic be
//! written as arithmetic — `start + (end - start) * t` rather than
//! `add(start, scale(sub(end, start), t))` — and stops a position being passed
//! where a normal belongs.

use std::ops::{Add, AddAssign, Div, Index, IndexMut, Mul, MulAssign, Neg, Sub, SubAssign};

/// A 3-component vector: a position, direction, normal or extent.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Vec3([f32; 3]);

impl Vec3 {
    pub const ZERO: Self = Self([0.0; 3]);

    #[must_use]
    pub const fn new(x: f32, y: f32, z: f32) -> Self {
        Self([x, y, z])
    }

    #[must_use]
    pub const fn splat(value: f32) -> Self {
        Self([value; 3])
    }

    #[must_use]
    pub const fn to_array(self) -> [f32; 3] {
        self.0
    }

    #[must_use]
    pub const fn as_array(&self) -> &[f32; 3] {
        &self.0
    }

    #[must_use]
    pub const fn x(self) -> f32 {
        self.0[0]
    }

    #[must_use]
    pub const fn y(self) -> f32 {
        self.0[1]
    }

    #[must_use]
    pub const fn z(self) -> f32 {
        self.0[2]
    }

    /// Applies `f` to each component, mirroring `[f32; 3]::map`.
    #[must_use]
    pub fn map(self, f: impl FnMut(f32) -> f32) -> Self {
        Self(self.0.map(f))
    }

    /// Whether every component is finite. Parsed and decoded vectors reach
    /// this crate from map files, so a NaN is data, not a bug.
    #[must_use]
    pub fn is_finite(self) -> bool {
        self.0.iter().all(|component| component.is_finite())
    }

    #[must_use]
    pub fn dot(self, other: Self) -> f32 {
        self.0[0] * other.0[0] + self.0[1] * other.0[1] + self.0[2] * other.0[2]
    }

    /// [`Self::dot`] with this vector's components taken as magnitudes —
    /// Source's plane/AABB overlap test, where the box extent projects onto
    /// the plane normal regardless of which way the normal points.
    #[must_use]
    pub fn dot_abs(self, other: Self) -> f32 {
        self.0[0].abs() * other.0[0] + self.0[1].abs() * other.0[1] + self.0[2].abs() * other.0[2]
    }

    #[must_use]
    pub fn cross(self, other: Self) -> Self {
        Self([
            self.0[1] * other.0[2] - self.0[2] * other.0[1],
            self.0[2] * other.0[0] - self.0[0] * other.0[2],
            self.0[0] * other.0[1] - self.0[1] * other.0[0],
        ])
    }

    #[must_use]
    pub fn length_squared(self) -> f32 {
        self.dot(self)
    }

    #[must_use]
    pub fn length(self) -> f32 {
        self.length_squared().sqrt()
    }

    #[must_use]
    pub fn distance_squared(self, other: Self) -> f32 {
        (self - other).length_squared()
    }

    #[must_use]
    pub fn distance(self, other: Self) -> f32 {
        self.distance_squared(other).sqrt()
    }

    /// `None` for a vector too short to have a direction.
    ///
    /// Fallible because callers disagree on the degenerate case: a camera
    /// basis wants a usable axis, a surface normal wants zero.
    #[must_use]
    pub fn normalize(self) -> Option<Self> {
        let length = self.length();
        (length > f32::EPSILON).then(|| self / length)
    }

    /// A direction, or the zero vector when there is none. Prefer
    /// [`Self::normalize`] where the caller can say something better than zero.
    #[must_use]
    pub fn normalize_or_zero(self) -> Self {
        self.normalize().unwrap_or(Self::ZERO)
    }

    #[must_use]
    pub fn lerp(self, end: Self, fraction: f32) -> Self {
        self + (end - self) * fraction
    }

    #[must_use]
    pub fn rotate_x(self, radians: f32) -> Self {
        let (sin, cos) = radians.sin_cos();
        Self([
            self.0[0],
            self.0[1] * cos - self.0[2] * sin,
            self.0[1] * sin + self.0[2] * cos,
        ])
    }

    #[must_use]
    pub fn rotate_y(self, radians: f32) -> Self {
        let (sin, cos) = radians.sin_cos();
        Self([
            self.0[0] * cos + self.0[2] * sin,
            self.0[1],
            -self.0[0] * sin + self.0[2] * cos,
        ])
    }

    #[must_use]
    pub fn rotate_z(self, radians: f32) -> Self {
        let (sin, cos) = radians.sin_cos();
        Self([
            self.0[0] * cos - self.0[1] * sin,
            self.0[0] * sin + self.0[1] * cos,
            self.0[2],
        ])
    }

    /// Applies a Source entity's `angles` to this vector, in Source's
    /// roll-pitch-yaw order. Applying the three rotations in any other order
    /// gives a different result for anything but a single-axis angle.
    #[must_use]
    pub fn rotate_source(self, angles: Self) -> Self {
        let QAngle { pitch, yaw, roll } = QAngle::from_source_degrees(angles);
        self.rotate_x(roll).rotate_y(pitch).rotate_z(yaw)
    }
}

impl IntoIterator for Vec3 {
    type Item = f32;
    type IntoIter = std::array::IntoIter<f32, 3>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl From<[f32; 3]> for Vec3 {
    fn from(array: [f32; 3]) -> Self {
        Self(array)
    }
}

impl From<Vec3> for [f32; 3] {
    fn from(vector: Vec3) -> Self {
        vector.0
    }
}

impl Index<usize> for Vec3 {
    type Output = f32;

    fn index(&self, axis: usize) -> &f32 {
        &self.0[axis]
    }
}

impl IndexMut<usize> for Vec3 {
    fn index_mut(&mut self, axis: usize) -> &mut f32 {
        &mut self.0[axis]
    }
}

impl Add for Vec3 {
    type Output = Self;

    fn add(self, other: Self) -> Self {
        Self([
            self.0[0] + other.0[0],
            self.0[1] + other.0[1],
            self.0[2] + other.0[2],
        ])
    }
}

impl Sub for Vec3 {
    type Output = Self;

    fn sub(self, other: Self) -> Self {
        Self([
            self.0[0] - other.0[0],
            self.0[1] - other.0[1],
            self.0[2] - other.0[2],
        ])
    }
}

impl Neg for Vec3 {
    type Output = Self;

    fn neg(self) -> Self {
        Self([-self.0[0], -self.0[1], -self.0[2]])
    }
}

impl Mul<f32> for Vec3 {
    type Output = Self;

    fn mul(self, scalar: f32) -> Self {
        Self([self.0[0] * scalar, self.0[1] * scalar, self.0[2] * scalar])
    }
}

impl Div<f32> for Vec3 {
    type Output = Self;

    fn div(self, scalar: f32) -> Self {
        Self([self.0[0] / scalar, self.0[1] / scalar, self.0[2] / scalar])
    }
}

impl AddAssign for Vec3 {
    fn add_assign(&mut self, other: Self) {
        *self = *self + other;
    }
}

impl SubAssign for Vec3 {
    fn sub_assign(&mut self, other: Self) {
        *self = *self - other;
    }
}

impl MulAssign<f32> for Vec3 {
    fn mul_assign(&mut self, scalar: f32) {
        *self = *self * scalar;
    }
}

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
    pub fn from_source_degrees(angles: Vec3) -> Self {
        Self {
            pitch: angles[0].to_radians(),
            yaw: angles[1].to_radians(),
            roll: angles[2].to_radians(),
        }
    }
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
        assert_eq!(Vec3::ZERO.normalize(), None);
        assert_eq!(Vec3::new(f32::EPSILON, 0.0, 0.0).normalize(), None);
        assert_eq!(
            Vec3::new(0.0, 5.0, 0.0).normalize(),
            Some(Vec3::new(0.0, 1.0, 0.0))
        );
    }

    /// `repr(transparent)` is what lets a `Vec3` reach wgpu and steamworks as
    /// the array they expect. A layout change would be silent at every call
    /// site and wrong on the GPU.
    #[test]
    fn a_vector_is_the_bytes_of_the_array_it_wraps() {
        use std::mem::{align_of, size_of};

        assert_eq!(size_of::<Vec3>(), size_of::<[f32; 3]>());
        assert_eq!(align_of::<Vec3>(), align_of::<[f32; 3]>());

        let source = [1.0_f32, -2.5, 3.25];
        assert_eq!(bytemuck::cast::<[f32; 3], Vec3>(source), Vec3::from(source));
        assert_eq!(Vec3::from(source).to_array(), source);
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
            Vec3::new(1.0, 0.0, 0.0).rotate_source(Vec3::new(0.0, 90.0, 0.0)),
            Vec3::new(0.0, 1.0, 0.0),
        );
        // 90° pitch alone takes +X to -Z (Source pitches nose-down).
        assert_close(
            Vec3::new(1.0, 0.0, 0.0).rotate_source(Vec3::new(90.0, 0.0, 0.0)),
            Vec3::new(0.0, 0.0, -1.0),
        );
        // 90° roll alone leaves +X fixed and takes +Y to +Z.
        assert_close(
            Vec3::new(0.0, 1.0, 0.0).rotate_source(Vec3::new(0.0, 0.0, 90.0)),
            Vec3::new(0.0, 0.0, 1.0),
        );

        // Roll-then-pitch-then-yaw: +X rolls to +X, pitches to -Z, and yaw
        // leaves -Z alone. Applying the same angles in the reverse order lands
        // somewhere else entirely, which is the whole point.
        let combined = Vec3::splat(90.0);
        let x_axis = Vec3::new(1.0, 0.0, 0.0);
        assert_close(x_axis.rotate_source(combined), Vec3::new(0.0, 0.0, -1.0));
        let right_angle = 90_f32.to_radians();
        assert_ne!(
            x_axis.rotate_source(combined),
            x_axis
                .rotate_z(right_angle)
                .rotate_y(right_angle)
                .rotate_x(right_angle)
        );
    }

    #[track_caller]
    fn assert_close(actual: Vec3, expected: Vec3) {
        for axis in 0..3 {
            assert!(
                (actual[axis] - expected[axis]).abs() < 1e-5,
                "{actual:?} != {expected:?}"
            );
        }
    }
}
