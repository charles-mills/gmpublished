//! Fixtures shared by the viewer's test modules.

use super::{MapPreview, ModelPreview, RenderScene};
use crate::media::preview_model::ModelStats;
use gmpublished_domain::math::Vec3;
use std::sync::Arc;

fn empty_render_scene(bounds_min: Vec3, bounds_max: Vec3) -> Arc<RenderScene> {
    Arc::new(RenderScene {
        meshes: Vec::new(),
        materials: Vec::new(),
        phy_debug_meshes: Vec::new(),
        stats: ModelStats {
            bone_count: 0,
            sequence_count: 0,
            vertex_count: 0,
            triangle_count: 0,
            mesh_count: 0,
            material_count: 0,
            resolved_material_count: 0,
        },
        bounds_min,
        bounds_max,
    })
}

pub(super) fn empty_model_preview(bounds_min: Vec3, bounds_max: Vec3) -> ModelPreview {
    ModelPreview {
        scene: empty_render_scene(bounds_min, bounds_max),
        skin_tables: vec![Vec::new()],
        bodygroups: Vec::new(),
    }
}

pub(super) fn empty_preview(bounds_min: Vec3, bounds_max: Vec3) -> MapPreview {
    MapPreview {
        scene: empty_render_scene(bounds_min, bounds_max),
        mesh_visibility: Vec::new(),
        map_skybox_meshes: Vec::new(),
        lightmap: None,
        skybox: None,
        detail_sprites: Vec::new(),
        map_skybox_detail_sprites: Vec::new(),
        overlays: Vec::new(),
        map_skybox_overlays: Vec::new(),
        doors: Vec::new(),
        visibility: None,
        walk_collision: None,
    }
}

use super::super::state::MovementMode;
use super::camera::FlyCamera;
use gmpublished_domain::scene::map::MapWalkCollision;

pub(super) fn floor_scene() -> MapPreview {
    let mut scene = empty_preview(Vec3::splat(0.0), Vec3::splat(1024.0));
    scene.walk_collision = Some(MapWalkCollision::solid_box_for_tests(
        Vec3::new(-4096.0, -4096.0, -64.0),
        Vec3::new(4096.0, 4096.0, 0.0),
    ));
    scene
}

pub(super) fn deep_water_scene() -> MapPreview {
    let mut scene = empty_preview(
        Vec3::new(-512.0, -512.0, -320.0),
        Vec3::new(512.0, 512.0, 256.0),
    );
    scene.walk_collision = Some(
        MapWalkCollision::solid_box_for_tests(
            Vec3::new(-4096.0, -4096.0, -320.0),
            Vec3::new(4096.0, 4096.0, -256.0),
        )
        .with_water_box_for_tests(
            Vec3::new(-4096.0, -4096.0, -256.0),
            Vec3::new(4096.0, 4096.0, 100.0),
        ),
    );
    scene
}

pub(super) fn walk_camera(position: Vec3, grounded: bool) -> FlyCamera {
    FlyCamera {
        content_id: Some(1),
        position: Some(position),
        mode: MovementMode::Walk,
        grounded,
        ..FlyCamera::default()
    }
}

pub(super) fn horizontal_distance_from(position: Vec3, origin: Vec3) -> f32 {
    ((position[0] - origin[0]).powi(2) + (position[1] - origin[1]).powi(2)).sqrt()
}
