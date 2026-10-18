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
        radial: fog.radial,
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
    let mapping_dimensions = material_dimensions(materials, mesh.material_index);
    MeshData {
        geometry: crate::media::preview_model::MeshGeometry::Brush,
        vertices: mesh
            .vertices
            .iter()
            .map(|vertex| ModelVertex {
                lighting_context: 0,
                surface_mark: 0,
                position: vertex.position,
                normal: vertex.normal,
                tangent_s: vertex.tangent_s,
                tangent_t: vertex.tangent_t,
                tangent_sign: vertex
                    .normal
                    .cross(vertex.tangent_s)
                    .dot(vertex.tangent_t)
                    .signum(),
                uv: normalize_map_uv(
                    vertex.tex_s,
                    vertex.tex_t,
                    mapping_dimensions.map_or(vertex.texture_size[0], |size| size.0),
                    mapping_dimensions.map_or(vertex.texture_size[1], |size| size.1),
                ),
                lightmap_uv: vertex.lightmap_uv,
                lightmap_bump_step: vertex.lightmap_bump_step,
                lightmap_page: vertex.lightmap_page.map_or(0, |page| page.0 + 1),
                color: Vec3::splat(1.0),
                blend_alpha: vertex.blend_alpha,
                opacity: vertex.opacity,
                illumination: Vec3::splat(1.0),
                lighting_mode: if vertex.lightmap_page.is_some() {
                    crate::media::preview_model::vertex_lighting::LIGHTMAP
                } else {
                    crate::media::preview_model::vertex_lighting::WORLD_CONSTANT
                },
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
