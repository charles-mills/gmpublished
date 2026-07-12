use super::{
    BTreeMap, BspError, ColorRgbExp, DispInfo, Face, HashMap, MapBsp, MapFaceVisibility,
    MapMeshClusterRanges, MapMeshIndexRange, MapMeshVisibility, MapVertex, PendingLightmapBlock,
    TexInfo, add, brush_lightmap_uv, displacement_lightmap_uv, extract_face_lightmap, face_normal,
    fan_indices, is_preview_material_visible, is_water_underside_face, length_squared, mul,
    normalize_material_name, sub,
};

#[derive(Debug, Clone, PartialEq)]
pub(super) struct BuildMesh {
    pub(super) vertices: Vec<BuildVertex>,
    pub(super) indices: Vec<u32>,
    pub(super) material_index: usize,
    pub(super) partition: GeometryPartition,
    pub(super) visibility: BuildMeshVisibility,
}

#[derive(Debug, Default, Clone, Copy, Eq, PartialEq, Hash)]
pub(super) enum GeometryPartition {
    #[default]
    Visible,
    Skybox,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub(super) struct BuildMeshKey {
    pub(super) material_index: usize,
    pub(super) partition: GeometryPartition,
}

#[derive(Debug, Default)]
pub(super) struct BuildMeshes {
    pub(super) meshes: Vec<BuildMesh>,
    pub(super) indexes: HashMap<BuildMeshKey, usize>,
}

impl BuildMeshes {
    pub(super) fn get_or_insert(
        &mut self,
        material_index: usize,
        partition: GeometryPartition,
    ) -> &mut BuildMesh {
        let key = BuildMeshKey {
            material_index,
            partition,
        };
        let index = *self.indexes.entry(key).or_insert_with(|| {
            let index = self.meshes.len();
            self.meshes.push(BuildMesh {
                vertices: Vec::new(),
                indices: Vec::new(),
                material_index,
                partition,
                visibility: BuildMeshVisibility::default(),
            });
            index
        });
        &mut self.meshes[index]
    }

    pub(super) fn iter_mut(&mut self) -> impl Iterator<Item = &mut BuildMesh> {
        self.meshes.iter_mut()
    }

    pub(super) fn into_inner(self) -> Vec<BuildMesh> {
        self.meshes
    }
}

#[derive(Debug, Default, Clone, PartialEq)]
pub(super) struct BuildMeshVisibility {
    pub(super) always_visible: Vec<MapMeshIndexRange>,
    pub(super) clusters: BTreeMap<u32, Vec<MapMeshIndexRange>>,
}

impl BuildMeshVisibility {
    pub(super) fn push(&mut self, face_visibility: &MapFaceVisibility, range: MapMeshIndexRange) {
        if face_visibility.always_visible || face_visibility.clusters.is_empty() {
            self.always_visible.push(range);
        }
        for cluster in &face_visibility.clusters {
            self.clusters.entry(*cluster).or_default().push(range);
        }
    }

    pub(super) fn into_map_visibility(self) -> MapMeshVisibility {
        MapMeshVisibility {
            always_visible: self.always_visible,
            clusters: self
                .clusters
                .into_iter()
                .map(|(cluster, ranges)| MapMeshClusterRanges { cluster, ranges })
                .collect(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct BuildVertex {
    pub(super) vertex: MapVertex,
    pub(super) lightmap_block: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct DisplacementBuildVertex {
    pub(super) position: [f32; 3],
    pub(super) column: usize,
    pub(super) row: usize,
    pub(super) steps: usize,
    pub(super) alpha: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct DisplacementGridVertex {
    pub(super) position: [f32; 3],
    pub(super) alpha: f32,
}

pub(super) struct FaceAppendContext<'a> {
    pub(super) meshes: &'a mut BuildMeshes,
    pub(super) material_names: &'a mut Vec<String>,
    pub(super) material_indexes: &'a mut HashMap<String, usize>,
    pub(super) lightmap_samples: Option<&'a [ColorRgbExp]>,
    pub(super) lightmap_blocks: &'a mut Vec<PendingLightmapBlock>,
}

pub(super) fn append_face(
    bsp: &MapBsp,
    face: &Face,
    face_index: usize,
    partition: GeometryPartition,
    face_visibility: &MapFaceVisibility,
    context: &mut FaceAppendContext<'_>,
) -> Result<(), BspError> {
    if face.edge_count < 3 || !bsp.face_is_visible(face) {
        return Ok(());
    }
    // `face_is_visible` only returns true when the texinfo lookup hits.
    let Some(texinfo) = bsp.face_texinfo(face) else {
        return Ok(());
    };

    let normal = face_normal(bsp, face);
    if is_water_underside_face(texinfo, normal) {
        return Ok(());
    }

    let texture_name = bsp.texinfo_name(texinfo);
    let Some(material_name) = normalize_material_name(texture_name) else {
        return Ok(());
    };
    if !is_preview_material_visible(&material_name) {
        return Ok(());
    }

    let material_index = if let Some(index) = context.material_indexes.get(&material_name).copied()
    {
        index
    } else {
        let index = context.material_names.len();
        context.material_names.push(material_name.clone());
        context.material_indexes.insert(material_name, index);
        index
    };

    let lightmap_block = context.lightmap_samples.and_then(|samples| {
        let block = extract_face_lightmap(face, samples)?;
        let index = context.lightmap_blocks.len();
        context.lightmap_blocks.push(block);
        Some(index)
    });
    let mesh = context.meshes.get_or_insert(material_index, partition);
    let index_start = mesh.indices.len();
    if let Some(displacement) = bsp.face_displacement(face) {
        append_displacement(
            bsp,
            face,
            displacement,
            texinfo,
            normal,
            mesh,
            lightmap_block,
        )?;
    } else {
        append_brush_face(bsp, face, texinfo, normal, mesh, lightmap_block)?;
    }
    let index_count = mesh.indices.len().saturating_sub(index_start);
    if index_count > 0
        && let (Ok(face), Ok(start), Ok(count)) = (
            u32::try_from(face_index),
            u32::try_from(index_start),
            u32::try_from(index_count),
        )
    {
        mesh.visibility
            .push(face_visibility, MapMeshIndexRange { face, start, count });
    }

    Ok(())
}

pub(super) fn append_brush_face(
    bsp: &MapBsp,
    face: &Face,
    texinfo: &TexInfo,
    normal: [f32; 3],
    mesh: &mut BuildMesh,
    lightmap_block: Option<usize>,
) -> Result<(), BspError> {
    let base =
        u32::try_from(mesh.vertices.len()).map_err(|_| BspError::TooLarge { item: "vertices" })?;
    let positions = bsp.face_vertex_positions(face);
    for position in positions.iter().copied() {
        mesh.vertices.push(BuildVertex {
            vertex: map_vertex(
                position,
                normal,
                texinfo,
                lightmap_block.map_or([0.0; 2], |_| brush_lightmap_uv(position, texinfo, face)),
                0.0,
            ),
            lightmap_block,
        });
    }
    for index in fan_indices(positions.len())? {
        mesh.indices.push(base + index);
    }
    Ok(())
}

pub(super) fn append_displacement(
    bsp: &MapBsp,
    face: &Face,
    displacement: &DispInfo,
    texinfo: &TexInfo,
    normal: [f32; 3],
    mesh: &mut BuildMesh,
    lightmap_block: Option<usize>,
) -> Result<(), BspError> {
    for vertex in displacement_vertices(bsp, face, displacement)? {
        let index = u32::try_from(mesh.vertices.len())
            .map_err(|_| BspError::TooLarge { item: "vertices" })?;
        mesh.vertices.push(BuildVertex {
            vertex: map_vertex(
                vertex.position,
                normal,
                texinfo,
                lightmap_block.map_or([0.0; 2], |_| {
                    displacement_lightmap_uv(vertex.column, vertex.row, vertex.steps, face)
                }),
                vertex.alpha,
            ),
            lightmap_block,
        });
        mesh.indices.push(index);
    }
    Ok(())
}

pub(super) fn map_vertex(
    position: [f32; 3],
    normal: [f32; 3],
    texinfo: &TexInfo,
    lightmap_uv: [f32; 2],
    blend_alpha: f32,
) -> MapVertex {
    MapVertex {
        position,
        normal,
        tex_s: texture_coord(position, texinfo.texture_vecs[0]),
        tex_t: texture_coord(position, texinfo.texture_vecs[1]),
        lightmap_uv,
        blend_alpha,
    }
}

pub(super) fn texture_coord(position: [f32; 3], transform: [f32; 4]) -> f32 {
    position[0] * transform[0]
        + position[1] * transform[1]
        + position[2] * transform[2]
        + transform[3]
}

pub(super) fn displacement_vertices(
    bsp: &MapBsp,
    face: &Face,
    displacement: &DispInfo,
) -> Result<Vec<DisplacementBuildVertex>, BspError> {
    let power =
        u32::try_from(displacement.power).map_err(|_| BspError::TooLarge { item: "vertices" })?;
    let steps = 2usize
        .checked_pow(power)
        .ok_or(BspError::TooLarge { item: "vertices" })?;
    let side = steps
        .checked_add(1)
        .ok_or(BspError::TooLarge { item: "vertices" })?;
    let grid = displacement_grid(bsp, face, displacement, steps)?;
    Ok(tessellate_displacement_grid(&grid, steps, side))
}

pub(super) fn tessellate_displacement_grid(
    grid: &[DisplacementGridVertex],
    steps: usize,
    side: usize,
) -> Vec<DisplacementBuildVertex> {
    // displacement_grid lays vertices out column-major (column is the outer
    // loop). Reading it back row-major silently transposes the grid: the
    // surface shape survives (same vertex set, still a valid tessellation)
    // but every triangle's winding reverses relative to its base face, so
    // backface culling removes displacements viewed from the front.
    let index = |column: usize, row: usize| column * side + row;

    let mut vertices = Vec::with_capacity(steps.saturating_mul(steps).saturating_mul(6));
    for column in 0..steps {
        for row in 0..steps {
            for triangle in [
                [(column, row), (column + 1, row), (column, row + 1)],
                [(column + 1, row), (column + 1, row + 1), (column, row + 1)],
            ] {
                let mut triangle_vertices = Vec::with_capacity(3);
                for (vertex_column, vertex_row) in triangle {
                    let Some(vertex) = grid.get(index(vertex_column, vertex_row)).copied() else {
                        triangle_vertices.clear();
                        break;
                    };
                    triangle_vertices.push((vertex_column, vertex_row, vertex));
                }
                if triangle_vertices.len() != 3 {
                    continue;
                }
                for (vertex_column, vertex_row, vertex) in triangle_vertices {
                    vertices.push(DisplacementBuildVertex {
                        position: vertex.position,
                        column: vertex_column,
                        row: vertex_row,
                        steps,
                        alpha: vertex.alpha,
                    });
                }
            }
        }
    }
    vertices
}

pub(super) fn displacement_grid(
    bsp: &MapBsp,
    face: &Face,
    displacement: &DispInfo,
    steps: usize,
) -> Result<Vec<DisplacementGridVertex>, BspError> {
    let corner_positions = displacement_corner_positions(bsp, face, displacement)?;
    let step_scale = 1.0 / steps.max(1) as f32;
    let edge_intervals = [
        mul(sub(corner_positions[1], corner_positions[0]), step_scale),
        mul(sub(corner_positions[2], corner_positions[3]), step_scale),
    ];
    let base_positions = (0..=steps).flat_map(move |column| {
        (0..=steps).map(move |row| {
            let edge_positions = [
                add(corner_positions[0], mul(edge_intervals[0], column as f32)),
                add(corner_positions[3], mul(edge_intervals[1], column as f32)),
            ];
            let segment_interval = mul(sub(edge_positions[1], edge_positions[0]), step_scale);
            add(edge_positions[0], mul(segment_interval, row as f32))
        })
    });

    Ok(bsp
        .displacement_vertices(displacement)
        .zip(base_positions)
        .map(|(displacement_vertex, base_position)| {
            let offset = mul(displacement_vertex.vector, displacement_vertex.dist);
            DisplacementGridVertex {
                position: add(base_position, offset),
                alpha: displacement_blend_alpha(displacement_vertex.alpha),
            }
        })
        .collect())
}

pub(super) fn displacement_blend_alpha(alpha: f32) -> f32 {
    (alpha / 255.0).clamp(0.0, 1.0)
}

pub(super) fn displacement_corner_positions(
    bsp: &MapBsp,
    face: &Face,
    displacement: &DispInfo,
) -> Result<[[f32; 3]; 4], BspError> {
    let vertices = bsp.face_vertex_positions(face);
    let mut corners: [[f32; 3]; 4] =
        vertices
            .as_slice()
            .try_into()
            .map_err(|_| BspError::Decode {
                message: "displacement face is not four-sided".to_owned(),
            })?;
    let start = displacement.start_position;
    let start_index = corners
        .iter()
        .enumerate()
        .min_by(|(_, left), (_, right)| {
            length_squared(sub(**left, start)).total_cmp(&length_squared(sub(**right, start)))
        })
        .map_or(0, |(index, _)| index);
    corners.rotate_left(start_index);
    Ok(corners)
}
