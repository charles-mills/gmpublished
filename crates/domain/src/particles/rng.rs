//! Deterministic random sampling and coherent noise.

use super::*;

/// Scalar linear interpolation, for the noise field's per-component blend.
fn scalar_lerp(start: f32, end: f32, fraction: f32) -> f32 {
    fraction.mul_add(end - start, start)
}

// --- Deterministic RNG ---------------------------------------------------

/// PCG32; deterministic so a restarted preview replays identically.
#[derive(Clone, Debug)]
pub(super) struct Rng {
    state: u64,
}

impl Rng {
    pub(super) fn new(seed: u64) -> Self {
        let mut rng = Self {
            state: seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407),
        };
        rng.next_u32();
        rng
    }

    pub(super) fn next_u32(&mut self) -> u32 {
        let old = self.state;
        self.state = old
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let xorshifted = (((old >> 18) ^ old) >> 27) as u32;
        let rot = (old >> 59) as u32;
        xorshifted.rotate_right(rot)
    }

    /// Uniform in [0, 1).
    pub(super) fn unit(&mut self) -> f32 {
        (self.next_u32() >> 8) as f32 / (1u32 << 24) as f32
    }

    pub(super) fn range(&mut self, min: f32, max: f32) -> f32 {
        min + (max - min) * self.unit()
    }

    /// Source's exponent-biased random: `min + (max-min) * unit^exponent`.
    pub(super) fn range_exp(&mut self, min: f32, max: f32, exponent: f32) -> f32 {
        let t = if exponent == 1.0 {
            self.unit()
        } else {
            self.unit().powf(exponent.max(1e-6))
        };
        min + (max - min) * t
    }

    pub(super) fn range_vec(&mut self, min: Vec3, max: Vec3) -> Vec3 {
        Vec3::new(
            self.range(min[0], max[0]),
            self.range(min[1], max[1]),
            self.range(min[2], max[2]),
        )
    }

    pub(super) fn range_int(&mut self, min: i32, max: i32) -> i32 {
        if max <= min {
            return min;
        }

        // Compute in a wider type: the inclusive i32 domain contains 2^32
        // values, which cannot be represented by either i32 or u32.
        let width = (i64::from(max) - i64::from(min) + 1) as u64;
        let source_width = 1_u64 << 32;
        let unbiased_zone = source_width - source_width % width;

        loop {
            let sample = u64::from(self.next_u32());
            if sample < unbiased_zone {
                // `sample % width` is at most `max - min`, so the sum is
                // proven to remain in the requested i32 interval.
                return (i64::from(min) + (sample % width) as i64) as i32;
            }
        }
    }

    /// Uniform direction, componentwise-scaled by `bias` then renormalized.
    pub(super) fn biased_unit_vector(&mut self, bias: Vec3) -> Vec3 {
        for _ in 0..16 {
            let v = Vec3::new(
                self.range(-1.0, 1.0) * bias[0],
                self.range(-1.0, 1.0) * bias[1],
                self.range(-1.0, 1.0) * bias[2],
            );
            let len = v.length();
            if len > 1e-4 && len <= 1.0 {
                return v * (1.0 / len);
            }
        }
        Vec3::new(0.0, 0.0, 1.0)
    }
}

/// Cheap value noise in [-1, 1]; smooth in its argument. Stands in for
/// Source's Perlin-style curl noise where only "coherent wobble" matters.
pub(super) fn value_noise(x: f32, y: f32, z: f32, w: f32) -> f32 {
    fn hash(mut n: u32) -> f32 {
        n = (n ^ 61) ^ (n >> 16);
        n = n.wrapping_mul(9);
        n ^= n >> 4;
        n = n.wrapping_mul(0x27d4_eb2d);
        n ^= n >> 15;
        (n & 0xffff) as f32 / 32767.5 - 1.0
    }
    fn smooth(t: f32) -> f32 {
        t * t * (3.0 - 2.0 * t)
    }
    let cell = |ix: i32, iy: i32, iz: i32, iw: i32| {
        hash(
            (ix as u32)
                .wrapping_mul(73856093)
                .wrapping_add((iy as u32).wrapping_mul(19349663))
                .wrapping_add((iz as u32).wrapping_mul(83492791))
                .wrapping_add((iw as u32).wrapping_mul(2654435761)),
        )
    };
    let (fx, fy, fz, fw) = (x.floor(), y.floor(), z.floor(), w.floor());
    let (ix, iy, iz, iw) = (fx as i32, fy as i32, fz as i32, fw as i32);
    let (tx, ty, tz, tw) = (
        smooth(x - fx),
        smooth(y - fy),
        smooth(z - fz),
        smooth(w - fw),
    );
    // Bilinear over (x, w) at the two (y, z) corners kept nearest; a full
    // 4D lattice is overkill for a visual wobble source.
    let corner = |dx: i32, dw: i32| {
        let a = cell(ix + dx, iy, iz, iw + dw);
        let b = cell(ix + dx, iy + 1, iz + 1, iw + dw);
        scalar_lerp(a, b, (ty + tz) * 0.5)
    };
    scalar_lerp(
        scalar_lerp(corner(0, 0), corner(1, 0), tx),
        scalar_lerp(corner(0, 1), corner(1, 1), tx),
        tw,
    )
}

/// Stable per-particle random in [min, max]: operators like fade-out draw a
/// random duration per particle that must not change between frames.
pub(super) fn deterministic_range(spawn_index: u32, salt: u32, min: f32, max: f32) -> f32 {
    let mut n = spawn_index
        .wrapping_mul(0x9E37_79B9)
        .wrapping_add(salt.wrapping_mul(0x85EB_CA6B));
    n ^= n >> 13;
    n = n.wrapping_mul(0xC2B2_AE35);
    n ^= n >> 16;
    let t = (n & 0xffff) as f32 / 65535.0;
    min + (max - min) * t
}
