use super::reflections::Plane;

pub(super) const WATER_CLIP_SPREAD: f32 = 2.0;

#[derive(Clone, Copy, Debug)]
pub(super) enum WorldWaterPass {
    Main,
    Reflection,
    Refraction,
    Intersection,
}

pub(super) fn world_plane(physical: Plane, pass: WorldWaterPass) -> Plane {
    match pass {
        WorldWaterPass::Main | WorldWaterPass::Reflection => Plane {
            distance: physical.distance - WATER_CLIP_SPREAD,
            ..physical
        },
        WorldWaterPass::Refraction => Plane {
            normal: -physical.normal,
            distance: -physical.distance - WATER_CLIP_SPREAD,
        },
        WorldWaterPass::Intersection => Plane {
            normal: -physical.normal,
            distance: -physical.distance + WATER_CLIP_SPREAD,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::super::{
        FlyCamera, Rectangle, Vec3,
        view_camera::{RasterClip, ViewCamera},
    };
    use super::*;

    fn surface(normal_z: f32) -> Plane {
        Plane {
            normal: Vec3::new(0.0, 0.0, normal_z),
            distance: -160.0 * normal_z,
        }
    }

    #[test]
    fn source_world_passes_retain_the_installed_height_regions() {
        let point = |z| Vec3::new(1024.0, 2304.0, z);
        for (normal, pass, retained, rejected) in [
            (1.0, WorldWaterPass::Main, -161.0, -163.0),
            (1.0, WorldWaterPass::Reflection, -161.0, -163.0),
            (1.0, WorldWaterPass::Refraction, -159.0, -157.0),
            (1.0, WorldWaterPass::Intersection, -163.0, -161.0),
            (-1.0, WorldWaterPass::Main, -159.0, -157.0),
            (-1.0, WorldWaterPass::Refraction, -161.0, -163.0),
        ] {
            let physical = surface(normal);
            let clip = world_plane(physical, pass);
            assert!(
                clip.signed_distance(point(retained)) > 0.0,
                "{normal} {pass:?}"
            );
            assert!(
                clip.signed_distance(point(rejected)) < 0.0,
                "{normal} {pass:?}"
            );
            let water_distance = clip.signed_distance(point(-160.0));
            assert_eq!(
                water_distance,
                if matches!(pass, WorldWaterPass::Intersection) {
                    -2.0
                } else {
                    2.0
                }
            );
            assert_eq!(physical.signed_distance(point(-160.0)), 0.0);
        }
    }

    #[test]
    fn source_transformed_planes_keep_water_vertices_inside_at_near_and_deep_poses() {
        let camera = FlyCamera {
            yaw: 90_f32.to_radians(),
            pitch: -15_f32.to_radians(),
            ..FlyCamera::default()
        };
        let bounds = Rectangle::new(iced::Point::ORIGIN, iced::Size::new(1280.0, 720.0));
        for eye_z in [-159.75, -159.875, -160.0, -160.125, -160.25, -400.0] {
            let camera = ViewCamera::fly(
                &camera,
                bounds,
                Vec3::new(1024.0, 2304.0, eye_z),
                4.0,
                42_370.14,
            );
            for normal in [1.0, -1.0] {
                for pass in [
                    WorldWaterPass::Main,
                    WorldWaterPass::Reflection,
                    WorldWaterPass::Refraction,
                    WorldWaterPass::Intersection,
                ] {
                    let physical = surface(normal);
                    let plane = world_plane(physical, pass);
                    let raster = RasterClip::new(
                        camera,
                        [
                            plane.normal[0],
                            plane.normal[1],
                            plane.normal[2],
                            -plane.distance,
                        ],
                    )
                    .unwrap();
                    for point in [[512.0, 1792.0, -160.0, 1.0], [1536.0, 3072.0, -160.0, 1.0]] {
                        let clip_position: [f32; 4] = std::array::from_fn(|row| {
                            (0..4)
                                .map(|col| raster.view_projection[col][row] * point[col])
                                .sum()
                        });
                        let distance: f32 = clip_position
                            .into_iter()
                            .zip(raster.plane)
                            .map(|(position, plane)| position * plane)
                            .sum();
                        assert!(
                            if matches!(pass, WorldWaterPass::Intersection) {
                                distance < 0.0
                            } else {
                                distance > 0.0
                            },
                            "eye={eye_z} normal={normal} pass={pass:?} distance={distance}"
                        );
                    }
                }
            }
        }
    }
}
