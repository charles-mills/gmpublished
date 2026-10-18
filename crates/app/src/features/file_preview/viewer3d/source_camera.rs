use super::Vec3;

pub(super) type Matrix4 = [[f32; 4]; 4];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MathError {
    NonFiniteInput,
    SingularMatrix,
    NonFiniteResult,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct SourceCamera {
    pub eye: Vec3,
    pub pitch_degrees: f32,
    pub yaw_degrees: f32,
}

impl SourceCamera {
    pub fn view(self) -> Result<Matrix4, MathError> {
        if !self.eye.is_finite() || !self.pitch_degrees.is_finite() || !self.yaw_degrees.is_finite()
        {
            return Err(MathError::NonFiniteInput);
        }
        let basis = source_multiply(rotation_x(-90.0), rotation_z(90.0));
        let basis = source_multiply(basis, rotation_y(-self.pitch_degrees));
        let basis = source_multiply(basis, rotation_z(-self.yaw_degrees));
        let translation = [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [-self.eye[0], -self.eye[1], -self.eye[2], 1.0],
        ];
        finite_result(source_multiply(basis, translation))
    }
}

pub(super) fn source_multiply(a: Matrix4, b: Matrix4) -> Matrix4 {
    std::array::from_fn(|column| {
        std::array::from_fn(|row| {
            ordered_dot([a[0][row], a[1][row], a[2][row], a[3][row]], b[column])
        })
    })
}

pub(super) fn inverse_general(matrix: Matrix4) -> Result<Matrix4, MathError> {
    if !finite(matrix) {
        return Err(MathError::NonFiniteInput);
    }
    // Eliminate the native row-vector storage directly; transposing changes rounding.
    let mut augmented: [[f32; 8]; 4] = std::array::from_fn(|row| {
        std::array::from_fn(|column| {
            if column < 4 {
                matrix[row][column]
            } else {
                if column - 4 == row { 1.0 } else { 0.0 }
            }
        })
    });
    let mut row_map = [0, 1, 2, 3];
    for pivot_column in 0..4 {
        let mut largest = 0.00001;
        let mut pivot = None;
        for (index, &row) in row_map.iter().enumerate().skip(pivot_column) {
            let value = augmented[row][pivot_column].abs();
            if value > largest {
                largest = value;
                pivot = Some(index);
            }
        }
        row_map.swap(pivot_column, pivot.ok_or(MathError::SingularMatrix)?);
        let pivot_row = row_map[pivot_column];
        let multiplier = 1.0 / augmented[pivot_row][pivot_column];
        for value in &mut augmented[pivot_row] {
            *value *= multiplier;
        }
        augmented[pivot_row][pivot_column] = 1.0;
        let normalized = augmented[pivot_row];
        for &row in &row_map {
            if row == pivot_row {
                continue;
            }
            let multiplier = -augmented[row][pivot_column];
            for (value, pivot_value) in augmented[row].iter_mut().zip(normalized) {
                let product = pivot_value * multiplier;
                *value += product;
            }
            augmented[row][pivot_column] = 0.0;
        }
    }
    finite_result(std::array::from_fn(|row| {
        std::array::from_fn(|column| augmented[row_map[row]][column + 4])
    }))
}

pub(super) fn clip_plane(
    view_projection: Matrix4,
    world_plane: [f32; 4],
) -> Result<[f32; 4], MathError> {
    if !world_plane.into_iter().all(f32::is_finite) {
        return Err(MathError::NonFiniteInput);
    }
    let inverse = inverse_general(view_projection)?;
    let plane = inverse.map(|column| ordered_dot(column, world_plane));
    if plane.into_iter().all(f32::is_finite) {
        Ok(plane)
    } else {
        Err(MathError::NonFiniteResult)
    }
}

fn ordered_dot(a: [f32; 4], b: [f32; 4]) -> f32 {
    let first = a[0] * b[0];
    let second = a[1] * b[1];
    let third = a[2] * b[2];
    let fourth = a[3] * b[3];
    ((first + second) + third) + fourth
}

fn sin_cos(degrees: f32) -> (f32, f32) {
    ((f64::from(degrees) * (std::f64::consts::PI / 180.0)) as f32).sin_cos()
}

fn rotation_x(degrees: f32) -> Matrix4 {
    let (s, c) = sin_cos(degrees);
    [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, c, s, 0.0],
        [0.0, -s, c, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

fn rotation_y(degrees: f32) -> Matrix4 {
    let (s, c) = sin_cos(degrees);
    [
        [c, 0.0, -s, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [s, 0.0, c, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

fn rotation_z(degrees: f32) -> Matrix4 {
    let (s, c) = sin_cos(degrees);
    [
        [c, s, 0.0, 0.0],
        [-s, c, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

fn finite(matrix: Matrix4) -> bool {
    matrix.into_iter().flatten().all(f32::is_finite)
}

fn finite_result(matrix: Matrix4) -> Result<Matrix4, MathError> {
    if finite(matrix) {
        Ok(matrix)
    } else {
        Err(MathError::NonFiniteResult)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bits(matrix: Matrix4) -> [[u32; 4]; 4] {
        matrix.map(|column| column.map(f32::to_bits))
    }

    #[test]
    fn native_view_matrices() {
        let cases = [
            (
                [0.0_f32, 3584.0_f32, 64.0_f32],
                10.0_f32,
                [
                    [0x3f800000, 0xb20266f2, 0xb0368a40, 0x0],
                    [0x30368a40, 0x3e31d0d1, 0xbf7c1c5c, 0x0],
                    [0x320266f2, 0x3f7c1c5c, 0x3e31d0d1, 0x0],
                    [0xb64052b4, 0xc42b587d, 0x455be6ff, 0x3f800000],
                ],
            ),
            (
                [-1428.0_f32, 1869.0_f32, 10_967.2_f32],
                -10.0_f32,
                [
                    [0x3f800000, 0x320266f2, 0xb0368a40, 0x0],
                    [0x30368a40, 0xbe31d0d7, 0xbf7c1c5c, 0x0],
                    [0xb20266f2, 0x3f7c1c5c, 0xbe31d0d7, 0x0],
                    [0x44b28001, 0xc623b024, 0x456a10a6, 0x3f800000],
                ],
            ),
            (
                [-1428.0_f32, 1869.0_f32, 10_995.2_f32],
                10.0_f32,
                [
                    [0x3f800000, 0xb20266f2, 0xb0368a40, 0x0],
                    [0x30368a40, 0x3e31d0d1, 0xbf7c1c5c, 0x0],
                    [0x320266f2, 0x3f7c1c5c, 0x3e31d0d1, 0x0],
                    [0x44b27fff, 0xc62e42d4, 0xc2896180, 0x3f800000],
                ],
            ),
            (
                [0.0_f32, 3584.0_f32, -384.0_f32],
                -10.0_f32,
                [
                    [0x3f800000, 0x320266f2, 0xb0368a40, 0x0],
                    [0x30368a40, 0xbe31d0d7, 0xbf7c1c5c, 0x0],
                    [0xb20266f2, 0x3f7c1c5c, 0xbe31d0d7, 0x0],
                    [0xb6b1a9b2, 0x447a215e, 0x45586deb, 0x3f800000],
                ],
            ),
        ];
        for (eye, pitch_degrees, expected) in cases {
            let camera = SourceCamera {
                eye: Vec3::new(eye[0], eye[1], eye[2]),
                pitch_degrees,
                yaw_degrees: 90.0,
            };
            assert_eq!(bits(camera.view().unwrap()), expected);
        }
    }

    #[test]
    fn native_clip_plane_before_raster_conversion() {
        let view = SourceCamera {
            eye: Vec3::new(-1428.0, 1869.0, 10_967.2),
            pitch_degrees: -10.0,
            yaw_degrees: 90.0,
        }
        .view()
        .unwrap();
        let projection = [
            [f32::from_bits(0x3fadd2c8), 0.0, 0.0, 0.0],
            [0.0, f32::from_bits(0x401a8279), 0.0, 0.0],
            [0.0, 0.0, f32::from_bits(0xbf800128), -1.0],
            [0.0, 0.0, f32::from_bits(0xc0000128), 0.0],
        ];
        let view_projection = source_multiply(projection, view);
        assert_eq!(
            bits(view_projection),
            [
                [0x3fadd2c8, 0x329d68c7, 0x30368be6, 0x30368a40],
                [0x3077e373, 0xbed6a483, 0x3f7c1ea3, 0x3f7c1c5c],
                [0xb23115e0, 0x4018298d, 0x3e31d272, 0x3e31d0d7],
                [0x44f266f2, 0xc6c596c5, 0xc56a32c3, 0xc56a10a6]
            ]
        );
        assert_eq!(
            clip_plane(view_projection, [0.0, 0.0, 1.0, -10_981.2])
                .unwrap()
                .map(f32::to_bits),
            [0x35c30a00, 0x3ed0dafd, 0x40e00c00, 0xc0da8000]
        );
    }

    #[test]
    fn invalid_camera() {
        assert_eq!(
            SourceCamera {
                eye: Vec3::new(f32::NAN, 0.0, 0.0),
                pitch_degrees: 0.0,
                yaw_degrees: 0.0,
            }
            .view(),
            Err(MathError::NonFiniteInput)
        );
    }

    #[test]
    fn inverse_limits_and_pivoting() {
        let identity = [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let mut swapped = identity;
        swapped.swap(0, 2);
        assert_eq!(inverse_general(swapped), Ok(swapped));
        let mut threshold = identity;
        threshold[0][0] = 0.00001;
        assert_eq!(inverse_general(threshold), Err(MathError::SingularMatrix));
        threshold[0][0] = f32::from_bits(0.00001_f32.to_bits() + 1);
        assert!(inverse_general(threshold).is_ok());
        assert_eq!(
            inverse_general([[0.0; 4]; 4]),
            Err(MathError::SingularMatrix)
        );
        let mut invalid = identity;
        invalid[2][3] = f32::INFINITY;
        assert_eq!(inverse_general(invalid), Err(MathError::NonFiniteInput));
        assert_eq!(
            clip_plane(identity, [f32::NAN, 0.0, 1.0, 0.0]),
            Err(MathError::NonFiniteInput)
        );
    }
}
