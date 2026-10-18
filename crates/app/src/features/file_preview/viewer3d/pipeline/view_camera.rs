use super::super::source_camera::{self, MathError, Matrix4, SourceCamera};
use super::{FlyCamera, Rectangle, Vec3};

const SKY_NEAR: f32 = 2.0;
const SKY_FAR: f32 = f32::from_bits(0x475d_b3d7);

#[derive(Clone, Copy, Debug)]
enum ViewTransform {
    Camera(SourceCamera),
    Matrix(Matrix4),
}

#[derive(Clone, Copy, Debug)]
pub(in super::super) struct ViewCamera {
    view: ViewTransform,
    field_of_view: f32,
    aspect: f32,
    pub near: f32,
    pub far: f32,
}

impl ViewCamera {
    pub fn fly(camera: &FlyCamera, bounds: Rectangle, eye: Vec3, near: f32, far: f32) -> Self {
        Self {
            view: ViewTransform::Camera(SourceCamera {
                eye,
                pitch_degrees: (-camera.pitch).to_degrees(),
                yaw_degrees: camera.yaw.to_degrees(),
            }),
            field_of_view: super::super::FOV_Y,
            aspect: (bounds.width / bounds.height.max(1.0)).max(0.1),
            near,
            far,
        }
    }

    pub fn skybox(self, origin: Vec3, scale: f32) -> Self {
        let ViewTransform::Camera(mut camera) = self.view else {
            unreachable!("skybox placement precedes optical reflection");
        };
        camera.eye = super::super::skybox_eye(camera.eye, origin, scale);
        Self {
            view: ViewTransform::Camera(camera),
            near: SKY_NEAR,
            far: SKY_FAR,
            ..self
        }
    }

    pub fn reflected(
        self,
        plane: super::reflections::Plane,
        water: bool,
    ) -> Result<Self, MathError> {
        let view = if water && let ViewTransform::Camera(mut camera) = self.view {
            camera.eye = plane.reflect_point(camera.eye);
            camera.pitch_degrees = -camera.pitch_degrees;
            ViewTransform::Camera(camera)
        } else {
            ViewTransform::Matrix(source_camera::source_multiply(self.view()?, plane.matrix()))
        };
        Ok(Self { view, ..self })
    }

    pub fn view(self) -> Result<Matrix4, MathError> {
        match self.view {
            ViewTransform::Camera(camera) => camera.view(),
            ViewTransform::Matrix(matrix) => Ok(matrix),
        }
    }

    pub fn projection(self) -> Matrix4 {
        let scale = 1.0 / (self.field_of_view * 0.5).tan();
        let range = f64::from(self.near) - f64::from(self.far);
        [
            [scale / self.aspect, 0.0, 0.0, 0.0],
            [0.0, scale, 0.0, 0.0],
            [0.0, 0.0, (f64::from(self.far) / range) as f32, -1.0],
            [
                0.0,
                0.0,
                (f64::from(self.near) * f64::from(self.far) / range) as f32,
                0.0,
            ],
        ]
    }

    pub fn raster_projection(self) -> Matrix4 {
        let mut projection = self.projection();
        let range = self.far - self.near;
        projection[2][2] = self.near / range;
        projection[3][2] = self.far * self.near / range;
        projection
    }

    pub fn view_projection(self) -> Result<Matrix4, MathError> {
        Ok(source_camera::source_multiply(
            self.projection(),
            self.view()?,
        ))
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, bytemuck::Pod, bytemuck::Zeroable)]
pub(in super::super) struct RasterClip {
    pub view_projection: Matrix4,
    pub plane: [f32; 4],
}

impl RasterClip {
    pub fn world_space(plane: [f32; 4]) -> Self {
        Self {
            view_projection: [
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
                [0.0, 0.0, 0.0, 1.0],
            ],
            plane,
        }
    }

    #[cfg(test)]
    pub fn intersects_bounds(self, bounds: gmpublished_domain::scene::map::MapBounds) -> bool {
        let absolute_corner = [
            f64::from(bounds.min[0].abs().max(bounds.max[0].abs())),
            f64::from(bounds.min[1].abs().max(bounds.max[1].abs())),
            f64::from(bounds.min[2].abs().max(bounds.max[2].abs())),
            1.0,
        ];
        let coefficients: [f64; 4] = self.view_projection.map(|column| {
            column
                .into_iter()
                .zip(self.plane)
                .map(|(value, plane)| f64::from(value) * f64::from(plane))
                .sum()
        });
        let maximum = coefficients[3]
            + (0..3)
                .map(|axis| {
                    coefficients[axis]
                        * f64::from(if coefficients[axis] >= 0.0 {
                            bounds.max[axis]
                        } else {
                            bounds.min[axis]
                        })
                })
                .sum::<f64>();
        let magnitude = (0..4)
            .map(|row| {
                f64::from(self.plane[row]).abs()
                    * (0..4)
                        .map(|column| {
                            f64::from(self.view_projection[column][row]).abs()
                                * absolute_corner[column]
                        })
                        .sum::<f64>()
            })
            .sum::<f64>();
        // Keep rounding in both GPU dot products inside the culling bound.
        maximum >= -16.0 * f64::from(f32::EPSILON) * magnitude
    }

    pub fn new(camera: ViewCamera, world_plane: [f32; 4]) -> Result<Self, MathError> {
        if world_plane == [0.0; 4] {
            return Ok(Self::default());
        }
        let view_projection = camera.view_projection()?;
        let plane = source_camera::clip_plane(view_projection, world_plane)?;
        Ok(Self {
            view_projection,
            plane,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounds_culling_uses_the_raster_halfspace_conservatively() {
        use gmpublished_domain::scene::map::MapBounds;
        let clip = RasterClip::world_space([0.0, 0.0, 1.0, -20.0]);
        let bounds = |low, high| MapBounds {
            min: Vec3::new(-10.0, -10.0, low),
            max: Vec3::new(10.0, 10.0, high),
        };
        assert!(clip.intersects_bounds(bounds(20.0, 30.0)));
        assert!(clip.intersects_bounds(bounds(10.0, 20.0)));
        assert!(!clip.intersects_bounds(bounds(10.0, 19.0)));
        assert!(RasterClip::default().intersects_bounds(bounds(-100.0, -50.0)));
        assert!(
            !RasterClip::world_space([0.0, 0.0, 0.0, -1.0])
                .intersects_bounds(bounds(-100.0, 100.0))
        );
    }

    #[test]
    fn disabled_clipping_avoids_an_inverse_and_world_fallback_preserves_halfspace() {
        let clip = RasterClip::world_space([0.0, 0.0, 1.0, -20.0]);
        assert_eq!(clip.view_projection[3], [0.0, 0.0, 0.0, 1.0]);
        assert_eq!(clip.plane, [0.0, 0.0, 1.0, -20.0]);
        let invalid = ViewCamera {
            view: ViewTransform::Matrix([[0.0; 4]; 4]),
            field_of_view: super::super::super::FOV_Y,
            aspect: 1.0,
            near: 2.0,
            far: 1000.0,
        };
        assert_eq!(RasterClip::new(invalid, [0.0; 4]).unwrap().plane, [0.0; 4]);
        assert!(RasterClip::new(invalid, [0.0, 0.0, 1.0, 0.0]).is_err());
    }
}
