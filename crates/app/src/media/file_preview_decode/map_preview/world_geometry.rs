//! World geometry and BSP-to-preview projections.

use super::{
    MapFog, MapSkyCamera, MapSpawn, MaterialSlot, MeshData, ModelVertex, Vec3, material_dimensions,
    normalize_map_uv, srgb_byte_to_linear,
};

pub(in super::super) fn map_fog_to_preview(fog: gmpublished_domain::scene::map::MapFog) -> MapFog {
    MapFog {
        color_linear: Vec3::from(fog.color_srgb.map(srgb_byte_to_linear)),
        start: fog.start,
        end: fog.end,
        max_density: fog.max_density,
    }
}

pub(in super::super) fn map_sky_camera_to_preview(
    camera: gmpublished_domain::scene::map::MapSkyCamera,
) -> MapSkyCamera {
    MapSkyCamera {
        origin: camera.origin,
        scale: camera.scale,
        fog: camera.fog.map(map_fog_to_preview),
    }
}

pub(in super::super) fn map_spawn_to_preview(
    spawn: gmpublished_domain::scene::map::MapPlayerStart,
) -> MapSpawn {
    MapSpawn {
        origin: spawn.origin,
        angles: spawn.angles,
    }
}

pub(in super::super) fn map_mesh_to_model_mesh(
    mesh: &gmpublished_domain::scene::map::MapMesh,
    materials: &[MaterialSlot],
) -> MeshData {
    let (width, height) = material_dimensions(materials, mesh.material_index);
    MeshData {
        vertices: mesh
            .vertices
            .iter()
            .map(|vertex| ModelVertex {
                position: vertex.position,
                normal: vertex.normal,
                uv: normalize_map_uv(vertex.tex_s, vertex.tex_t, width, height),
                lightmap_uv: vertex.lightmap_uv,
                color: Vec3::splat(1.0),
                blend_alpha: vertex.blend_alpha,
            })
            .collect(),
        indices: mesh.indices.clone(),
        material_index: mesh.material_index,
        bodygroup: 0,
        bodygroup_choice: 0,
    }
}

pub(in super::super) fn bounds_from_model_meshes(meshes: &[MeshData]) -> Option<(Vec3, Vec3)> {
    let mut positions = meshes
        .iter()
        .flat_map(|mesh| mesh.vertices.iter())
        .map(|vertex| vertex.position);
    let first = positions.next()?;
    let mut min = first;
    let mut max = first;
    for position in positions {
        for axis in 0..3 {
            min[axis] = min[axis].min(position[axis]);
            max[axis] = max[axis].max(position[axis]);
        }
    }
    Some((min, max))
}
