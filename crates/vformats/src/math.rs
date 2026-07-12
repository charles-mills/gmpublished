//! Crate-internal float helpers shared by the binary formats.
//!
//! `sqrt_f32` uses a Newton iteration (ULP-tested against std) rather
//! than the intrinsic, so it stays available uniformly across every
//! caller; `sin_f32`/`cos_f32` delegate straight to the intrinsics.

/// `f32` square root. Newton's method with a bit-level initial guess:
/// ~1 ULP, plenty for normalization gates and quaternion reconstruction.
#[cfg(any(feature = "phy", feature = "mdl", test))]
pub fn sqrt_f32(v: f32) -> f32 {
    if v == 0.0 {
        return 0.0;
    }
    if v.is_nan() || v < 0.0 {
        return f32::NAN;
    }
    if v.is_infinite() {
        return f32::INFINITY;
    }
    let mut x = f32::from_bits((v.to_bits() >> 1) + 0x1FC0_0000);
    for _ in 0..4 {
        x = 0.5 * (x + v / x);
    }
    x
}

/// IEEE half-precision to `f32`.
#[cfg(any(feature = "vtf", feature = "mdl", test))]
pub fn half_to_f32(v: u16) -> f32 {
    let sign = if v & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exponent = ((v >> 10) & 0x1F) as i32;
    let mantissa = (v & 0x3FF) as f32;
    match exponent {
        0 => sign * mantissa * (1.0 / 1024.0) * (1.0 / 16384.0),
        31 => {
            if mantissa == 0.0 {
                sign * f32::INFINITY
            } else {
                f32::NAN
            }
        }
        _ => sign * (1.0 + mantissa / 1024.0) * exp2(exponent - 15),
    }
}

/// Exact for the half-float normal exponent range (-14..=15): the
/// biased exponent always lands strictly inside (0, 255).
#[cfg(any(feature = "vtf", feature = "mdl", test))]
fn exp2(e: i32) -> f32 {
    let biased = u32::try_from(e + 127).expect("half-float exponent is positive after bias");
    f32::from_bits(biased << 23)
}

#[cfg(feature = "mdl")]
pub fn sin_f32(v: f32) -> f32 {
    v.sin()
}

#[cfg(feature = "mdl")]
pub fn cos_f32(v: f32) -> f32 {
    v.cos()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newton_sqrt_stays_within_one_ulp_of_std() {
        let mut v = 1.0e-12_f32;
        while v < 1.0e12 {
            let ours = sqrt_f32(v);
            let std = v.sqrt();
            let ulps = (ours.to_bits() as i64 - std.to_bits() as i64).unsigned_abs();
            assert!(ulps <= 1, "sqrt({v}) = {ours}, std {std}, {ulps} ulps");
            v *= 1.7;
        }
        assert_eq!(sqrt_f32(0.0), 0.0);
        assert!(sqrt_f32(-1.0).is_nan());
        assert!(sqrt_f32(f32::NAN).is_nan());
        assert_eq!(sqrt_f32(f32::INFINITY), f32::INFINITY);
    }

    #[test]
    fn half_conversion_hits_known_values() {
        assert_eq!(half_to_f32(0x3C00), 1.0);
        assert_eq!(half_to_f32(0x0000), 0.0);
        assert_eq!(half_to_f32(0x3800), 0.5);
        assert_eq!(half_to_f32(0x4000), 2.0);
        assert_eq!(half_to_f32(0xBC00), -1.0);
        assert!(half_to_f32(0x7FFF).is_nan());
        assert_eq!(half_to_f32(0x7C00), f32::INFINITY);
    }
}
