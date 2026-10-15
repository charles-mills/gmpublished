//! Source hardware model-vertex normal/tangent stream encoding.

/// Physical vertex-stream representation used before bone blending.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum VertexCompression {
    /// Preserve full-precision authored vectors.
    #[default]
    None,
    /// Source's combined normal/tangent UBYTE4 octant projection.
    SourceUbyte4,
    /// Source hardware UBYTE4 frames, SHORT2 weights and matrix-first skinning.
    /// Explicit candidate until native physical-stream validation is complete.
    SourceHardware,
}

/// Apply Source's UBYTE4 pack/decompress round trip to one vector.
///
/// The authored normal and tangent use the same octant projection. Tangent
/// handedness occupies a separate sign bit and remains independent of XYZ.
/// Invalid or zero vectors cannot be encoded and return `None`.
/// https://github.com/ValveSoftware/source-sdk-2013/blob/master/src/public/mathlib/mathlib.h
/// https://github.com/ValveSoftware/source-sdk-2013/blob/master/src/materialsystem/stdshaders/common_vs_fxc.h
#[must_use]
pub fn source_ubyte4_vector(vector: [f32; 3]) -> Option<[f32; 3]> {
    roundtrip(vector, 1.0)
}

/// Encode/decompress authored tangent XYZ/W using its actual W sign bit.
///
/// W affects the unsigned-byte truncation of packed Y, so encoding XYZ with
/// the normal's positive sign and restoring W afterward is not equivalent.
#[must_use]
pub fn source_ubyte4_tangent(tangent: [f32; 4]) -> Option<[f32; 4]> {
    if tangent[3] != 1.0 && tangent[3] != -1.0 {
        return None;
    }
    let xyz = roundtrip([tangent[0], tangent[1], tangent[2]], tangent[3])?;
    Some([xyz[0], xyz[1], xyz[2], tangent[3]])
}

/// Source's compressed SHORT2 bone-weight stream and shader decode.
///
/// The vertex builder truncates all three weights to 15 fractional bits,
/// assigns the remaining units to weight one, and stores the first two as
/// signed shorts minus one. The shader adds one, divides by 32768 exactly,
/// and reconstructs the third weight from the first two.
///
/// https://github.com/ValveSoftware/source-sdk-2013/blob/master/src/public/materialsystem/imesh.h
/// https://github.com/ValveSoftware/source-sdk-2013/blob/master/src/materialsystem/stdshaders/common_vs_fxc.h
#[must_use]
pub fn source_short2_weights(weights: [f32; 3]) -> Option<[f32; 3]> {
    if weights
        .iter()
        .any(|weight| !weight.is_finite() || !(0.0..=1.0).contains(weight))
    {
        return None;
    }
    // Float2Int is a truncating cast on Source's PC path, including ties.
    let quantized = weights.map(|weight| (weight * 32768.0) as u32);
    let sum: u32 = quantized.iter().sum();
    if sum > 32768 {
        return None;
    }
    let first = quantized[0] as f32 / 32768.0;
    let second = (quantized[1] + 32768 - sum) as f32 / 32768.0;
    Some([first, second, 1.0 - (first + second)])
}

fn roundtrip(vector: [f32; 3], sign: f32) -> Option<[f32; 3]> {
    if vector.iter().any(|v| !v.is_finite()) {
        return None;
    }
    let [nx, ny, nz] = vector;
    let xs = if nx < 0.0 { -1.0 } else { 1.0 };
    let ys = if ny < 0.0 { -1.0 } else { 1.0 };
    let zs = if nz < 0.0 { -1.0 } else { 1.0 };
    let xb = 0.5 * (1.0 - xs);
    let yb = 0.5 * (1.0 - ys);
    let zb = 0.5 * (1.0 - zs);
    let denominator = nx * xs + ny * ys + nz * zs;
    if !denominator.is_finite() || denominator <= 0.0 {
        return None;
    }
    // Preserve the native float arithmetic and final unsigned-byte truncation.
    let mut x = nx * xs / denominator * 63.0;
    let mut y = ny * ys / denominator * 63.0;
    x = (x * xs - xb + 64.0) * zs - zb + 128.0;
    y = (y * ys - yb + 64.0) * sign - 0.5 * (1.0 - sign) + 128.0;
    let encoded = [x as u8, y as u8];
    // Shader _DecompressUByte4Normal. Tangent's independent W sign does not
    // change the recovered XYZ; the full packed pair shares this projection.
    let packed = [f32::from(encoded[0]) - 128.0, f32::from(encoded[1]) - 128.0];
    let zbit = f32::from(packed[0] < 0.0);
    let tbit = f32::from(packed[1] < 0.0);
    let abs = [packed[0].abs() - zbit, packed[1].abs() - tbit];
    let signs = [f32::from(abs[0] < 64.0), f32::from(abs[1] < 64.0)];
    let xy = [
        ((abs[0] - 64.0).abs() - signs[0]) / 63.0,
        ((abs[1] - 64.0).abs() - signs[1]) / 63.0,
    ];
    let z = 1.0 - xy[0] - xy[1];
    let length = (xy[0] * xy[0] + xy[1] * xy[1] + z * z).sqrt();
    Some([
        xy[0] / length * (1.0 - 2.0 * signs[0]),
        xy[1] / length * (1.0 - 2.0 * signs[1]),
        z / length * (1.0 - 2.0 * zbit),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_weights_preserve_mesh_boundary_normalization_and_truncate_ties() {
        for weights in [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.5, 0.5, 0.0]] {
            assert_eq!(source_short2_weights(weights), Some(weights));
        }
        let first = 1000.5 / 32768.0;
        let third = 2000.5 / 32768.0;
        let physical = source_short2_weights([first, 1.0 - first - third, third]).unwrap();
        assert_eq!(physical[0], 1000.0 / 32768.0);
        assert_eq!(physical[2], 2000.0 / 32768.0);
        assert_eq!(physical.into_iter().sum::<f32>(), 1.0);
        assert_eq!(
            source_short2_weights([0.5, 0.49, 0.0]),
            Some([0.5, 0.5, 0.0])
        );
    }

    #[test]
    fn short_weights_reject_invalid_stream_values() {
        for weights in [
            [f32::NAN, 0.0, 1.0],
            [-0.1, 0.5, 0.6],
            [1.1, 0.0, 0.0],
            [0.8, 0.8, 0.0],
        ] {
            assert!(source_short2_weights(weights).is_none());
        }
    }

    #[test]
    fn axes_and_octant_projection_preserve_signs_and_unit_length() {
        for axis in [
            [1.0, 0.0, 0.0],
            [-1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, -1.0, 0.0],
            [0.0, 0.0, 1.0],
            [0.0, 0.0, -1.0],
        ] {
            assert_eq!(source_ubyte4_vector(axis), Some(axis));
        }
        for vector in [
            [0.2, 0.3, 0.5],
            [-0.2, 0.3, 0.5],
            [0.2, -0.3, -0.5],
            [-0.2, -0.3, -0.5],
        ] {
            let result = source_ubyte4_vector(vector).unwrap();
            assert!((result.iter().map(|x| x * x).sum::<f32>() - 1.0).abs() < 1e-6);
            for (raw, packed) in vector.into_iter().zip(result) {
                assert_eq!(raw.signum(), packed.signum());
            }
        }
    }

    #[test]
    fn zero_and_nonfinite_vectors_are_not_encodable() {
        for vector in [[0.0; 3], [f32::NAN, 0.0, 1.0], [f32::INFINITY, 0.0, 1.0]] {
            assert_eq!(source_ubyte4_vector(vector), None);
        }
    }

    #[test]
    fn authored_negative_w_changes_unsigned_y_truncation() {
        let positive = source_ubyte4_tangent([0.2, 0.3, 0.5, 1.0]).unwrap();
        let negative = source_ubyte4_tangent([0.2, 0.3, 0.5, -1.0]).unwrap();
        // Native packed bytes are (204,210) vs (204,44), which recover
        // projected octant numerators (12,18,33) vs (12,19,32).
        for (actual, numerators) in [
            (positive, [12.0_f32, 18.0, 33.0]),
            (negative, [12.0, 19.0, 32.0]),
        ] {
            let length = numerators.iter().map(|v| v * v).sum::<f32>().sqrt();
            for axis in 0..3 {
                assert!((actual[axis] - numerators[axis] / length).abs() < 1e-6);
            }
        }
        assert_eq!(positive[3], 1.0);
        assert_eq!(negative[3], -1.0);
        assert_ne!(positive[1], negative[1]);
    }
}
