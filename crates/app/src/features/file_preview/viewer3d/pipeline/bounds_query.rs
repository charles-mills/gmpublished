use super::Uniforms;
use gmpublished_domain::scene::map::{
    MapBounds, MapBoundsCullMask, MapBoundsCulling, MapPreparedBounds,
};

#[derive(Clone, Copy)]
struct HalfSpace {
    coefficients: [f64; 4],
    magnitude: [f64; 4],
    valid: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PlaneRelation {
    Outside,
    Crossing,
    Inside,
}

impl HalfSpace {
    fn new(coefficients: [f64; 4], magnitude: [f64; 4]) -> Self {
        Self {
            coefficients,
            magnitude,
            valid: coefficients
                .into_iter()
                .chain(magnitude)
                .all(f64::is_finite),
        }
    }

    fn classify(self, bounds: MapBounds) -> PlaneRelation {
        if !self.valid {
            return PlaneRelation::Crossing;
        }
        let (maximum, margin) = self.maximum_and_margin(bounds);
        if matches!(
            maximum.partial_cmp(&-margin),
            Some(std::cmp::Ordering::Less)
        ) {
            return PlaneRelation::Outside;
        }
        let minimum = self.coefficients[3]
            + (0..3)
                .map(|axis| {
                    self.coefficients[axis]
                        * f64::from(if self.coefficients[axis] >= 0.0 {
                            bounds.min[axis]
                        } else {
                            bounds.max[axis]
                        })
                })
                .sum::<f64>();
        if matches!(
            minimum.partial_cmp(&margin),
            Some(std::cmp::Ordering::Greater | std::cmp::Ordering::Equal)
        ) {
            PlaneRelation::Inside
        } else {
            PlaneRelation::Crossing
        }
    }
    fn maximum_and_margin(self, bounds: MapBounds) -> (f64, f64) {
        let maximum = self.coefficients[3]
            + (0..3)
                .map(|axis| {
                    self.coefficients[axis]
                        * f64::from(if self.coefficients[axis] >= 0.0 {
                            bounds.max[axis]
                        } else {
                            bounds.min[axis]
                        })
                })
                .sum::<f64>();
        let magnitude = self.magnitude[3]
            + (0..3)
                .map(|axis| {
                    self.magnitude[axis]
                        * f64::from(bounds.min[axis].abs().max(bounds.max[axis].abs()))
                })
                .sum::<f64>();
        // Retain bounds within GPU dot-product rounding error.
        let margin = 16.0 * f64::from(f32::EPSILON) * magnitude;
        (maximum, margin)
    }

    fn intersects(self, bounds: MapBounds) -> bool {
        let (maximum, margin) = self.maximum_and_margin(bounds);
        !matches!(
            maximum.partial_cmp(&-margin),
            Some(std::cmp::Ordering::Less)
        )
    }
}

pub(in super::super) struct BoundsQuery {
    frustum: [HalfSpace; 6],
    clip: HalfSpace,
}

impl BoundsQuery {
    pub fn new(view: &Uniforms) -> Self {
        let row = |axis| view.view_proj.map(|column| f64::from(column[axis]));
        let w = row(3);
        let planes = [(0, 1.0), (0, -1.0), (1, 1.0), (1, -1.0), (2, -1.0)];
        let frustum = std::array::from_fn(|index| {
            if index == 5 {
                let coefficients = row(2);
                HalfSpace::new(coefficients, coefficients.map(f64::abs))
            } else {
                let (axis, sign) = planes[index];
                let axis = row(axis);
                HalfSpace::new(
                    std::array::from_fn(|i| w[i] + sign * axis[i]),
                    std::array::from_fn(|i| w[i].abs() + axis[i].abs()),
                )
            }
        });
        let raster = view.raster_clip;
        let clip = HalfSpace::new(
            raster.view_projection.map(|column| {
                column
                    .into_iter()
                    .zip(raster.plane)
                    .map(|(value, plane)| f64::from(value) * f64::from(plane))
                    .sum()
            }),
            raster.view_projection.map(|column| {
                column
                    .into_iter()
                    .zip(raster.plane)
                    .map(|(value, plane)| f64::from(value).abs() * f64::from(plane).abs())
                    .sum()
            }),
        );
        Self { frustum, clip }
    }

    #[cfg(test)]
    pub fn classify(&self, bounds: MapBounds, mask: MapBoundsCullMask) -> MapBoundsCulling {
        self.classify_prepared(MapPreparedBounds::new(bounds), mask)
    }

    pub fn classify_prepared(
        &self,
        bounds: MapPreparedBounds,
        mask: MapBoundsCullMask,
    ) -> MapBoundsCulling {
        if !bounds.is_valid() {
            return MapBoundsCulling::Visible(MapBoundsCullMask::ALL);
        }
        if mask == MapBoundsCullMask::NONE {
            return MapBoundsCulling::Visible(mask);
        }
        let bounds = bounds.bounds();
        let mut remaining = 0;
        for (index, plane) in self
            .frustum
            .iter()
            .chain(std::iter::once(&self.clip))
            .enumerate()
        {
            let bit = 1 << index;
            if mask.bits() & bit == 0 {
                continue;
            }
            match plane.classify(bounds) {
                PlaneRelation::Outside => return MapBoundsCulling::Outside,
                PlaneRelation::Crossing => remaining |= bit,
                PlaneRelation::Inside => {}
            }
        }
        MapBoundsCulling::Visible(MapBoundsCullMask::from_bits(remaining))
    }

    pub fn intersects(&self, bounds: MapBounds) -> bool {
        if !bounds.min.is_finite()
            || !bounds.max.is_finite()
            || (0..3).any(|axis| bounds.min[axis] > bounds.max[axis])
        {
            return true;
        }
        self.frustum.iter().all(|plane| plane.intersects(bounds)) && self.clip.intersects(bounds)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytemuck::Zeroable;
    use gmpublished_domain::math::Vec3;

    fn corners_intersect(bounds: MapBounds, matrix: [[f32; 4]; 4]) -> bool {
        let corners = std::array::from_fn::<_, 8, _>(|bits| {
            let point = std::array::from_fn::<_, 4, _>(|axis| {
                if axis == 3 {
                    1.0
                } else if bits & (1 << axis) == 0 {
                    bounds.min[axis]
                } else {
                    bounds.max[axis]
                }
            });
            std::array::from_fn::<_, 4, _>(|row| {
                (0..4).map(|col| matrix[col][row] * point[col]).sum::<f32>()
            })
        });
        ![(0, 1.0), (0, -1.0), (1, 1.0), (1, -1.0), (2, 1.0)]
            .iter()
            .any(|&(axis, sign)| corners.iter().all(|p| p[axis] * sign > p[3]))
            && !corners.iter().all(|p| p[2] < 0.0)
    }

    #[test]
    fn half_spaces_never_reject_corner_test_admissions_in_transformed_views() {
        let mut random = 17_u32;
        let mut next = || {
            random = random.wrapping_mul(1664525).wrapping_add(1013904223);
            (random as f64 / u32::MAX as f64 * 2.0 - 1.0) as f32
        };
        for _ in 0..32 {
            let mut view = Uniforms::zeroed();
            view.view_proj = std::array::from_fn(|_| std::array::from_fn(|_| next() * 8.0));
            let query = BoundsQuery::new(&view);
            for _ in 0..128 {
                let center = Vec3::new(next(), next(), next()) * 32768.0;
                let extent = Vec3::new(next().abs(), next().abs(), next().abs()) * 256.0;
                let bounds = MapBounds {
                    min: center - extent,
                    max: center + extent,
                };
                if corners_intersect(bounds, view.view_proj) {
                    assert!(query.intersects(bounds));
                }
            }
        }
    }

    #[test]
    fn clip_half_space_matches_raster_clip_with_rounding_margin() {
        let mut view = Uniforms::zeroed();
        view.raster_clip =
            super::super::view_camera::RasterClip::world_space([1.0, 0.0, 0.0, -2.0]);
        let query = BoundsQuery::new(&view);
        for x in [-100.0, 0.0, 1.9999999, 2.0, 10.0] {
            let bounds = MapBounds {
                min: Vec3::splat(x),
                max: Vec3::splat(x + 0.01),
            };
            assert_eq!(
                query.intersects(bounds),
                view.raster_clip.intersects_bounds(bounds)
            );
        }
    }
}

#[cfg(test)]
mod inherited_tests {
    use super::*;
    use bytemuck::Zeroable;
    use gmpublished_domain::math::Vec3;

    #[test]
    fn inherited_masks_match_full_queries_for_contained_children_and_optical_clipping() {
        let mut view = Uniforms::zeroed();
        view.view_proj = [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        view.raster_clip =
            super::super::view_camera::RasterClip::world_space([1.0, -0.5, 0.0, 0.2]);
        let query = BoundsQuery::new(&view);
        let mut retained = 0;
        for x in -10..10 {
            for y in -10..10 {
                let center = Vec3::new(x as f32 * 0.1, y as f32 * 0.1, 0.5);
                let parent = MapBounds {
                    min: center - Vec3::splat(0.2),
                    max: center + Vec3::splat(0.2),
                };
                let MapBoundsCulling::Visible(mask) =
                    query.classify(parent, MapBoundsCullMask::ALL)
                else {
                    continue;
                };
                for dx in [-0.1, 0.0, 0.1] {
                    let center = center + Vec3::new(dx, 0.0, 0.0);
                    let child = MapBounds {
                        min: center - Vec3::splat(0.05),
                        max: center + Vec3::splat(0.05),
                    };
                    assert_eq!(
                        query.classify(child, mask),
                        query.classify(child, MapBoundsCullMask::ALL)
                    );
                    retained += 1;
                }
            }
        }
        assert!(retained > 300);
    }

    #[test]
    fn rounding_margin_keeps_optical_boundary_plane_active() {
        let mut view = Uniforms::zeroed();
        view.raster_clip =
            super::super::view_camera::RasterClip::world_space([1.0, 0.0, 0.0, -2.0]);
        let query = BoundsQuery::new(&view);
        let bounds = MapBounds {
            min: Vec3::new(2.0, 0.0, 0.0),
            max: Vec3::new(3.0, 1.0, 1.0),
        };
        let MapBoundsCulling::Visible(mask) = query.classify(bounds, MapBoundsCullMask::ALL) else {
            panic!("coplanar bounds rejected")
        };
        const OPTICAL_PLANE: u8 = 1 << 6;
        assert_ne!(mask.bits() & OPTICAL_PLANE, 0);
        let inside = MapBounds {
            min: Vec3::splat(4.0),
            max: Vec3::splat(5.0),
        };
        assert_eq!(
            query.classify(inside, MapBoundsCullMask::ALL),
            MapBoundsCulling::Visible(MapBoundsCullMask::NONE)
        );
    }

    #[test]
    fn invalid_prepared_planes_remain_crossing() {
        let bounds = MapBounds {
            min: Vec3::splat(-2.0),
            max: Vec3::splat(-1.0),
        };
        for plane in [
            HalfSpace::new([f64::NAN, 0.0, 0.0, 0.0], [1.0; 4]),
            HalfSpace::new([1.0, 0.0, 0.0, 0.0], [f64::INFINITY; 4]),
        ] {
            assert!(!plane.valid);
            assert_eq!(plane.classify(bounds), PlaneRelation::Crossing);
            assert!(plane.intersects(bounds));
        }
    }

    #[test]
    fn invalid_bounds_admit_conservatively_and_reset_the_mask() {
        let view = Uniforms::zeroed();
        let query = BoundsQuery::new(&view);
        for bounds in [
            MapBounds {
                min: Vec3::splat(2.0),
                max: Vec3::splat(1.0),
            },
            MapBounds {
                min: Vec3::splat(f32::NAN),
                max: Vec3::splat(1.0),
            },
            MapBounds {
                min: Vec3::splat(0.0),
                max: Vec3::splat(f32::INFINITY),
            },
        ] {
            assert_eq!(
                query.classify(bounds, MapBoundsCullMask::NONE),
                MapBoundsCulling::Visible(MapBoundsCullMask::ALL)
            );
            assert!(query.intersects(bounds));
        }
    }
}
