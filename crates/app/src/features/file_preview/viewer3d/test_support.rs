//! Fixtures shared by the viewer's test modules.

use super::super::model::ModelStats;
use super::ModelPreview;

pub(super) fn empty_preview(bounds_min: [f32; 3], bounds_max: [f32; 3]) -> ModelPreview {
    ModelPreview {
        meshes: Vec::new(),
        mesh_visibility: Vec::new(),
        map_skybox_meshes: Vec::new(),
        materials: Vec::new(),
        lightmap: None,
        skybox: None,
        detail_sprites: Vec::new(),
        map_skybox_detail_sprites: Vec::new(),
        overlays: Vec::new(),
        map_skybox_overlays: Vec::new(),
        doors: Vec::new(),
        phy_debug_meshes: Vec::new(),
        skin_tables: vec![Vec::new()],
        bodygroups: Vec::new(),
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
        visibility: None,
        walk_collision: None,
    }
}
