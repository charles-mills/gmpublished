use super::{
    BTreeMap, BspError, ColorRgbExp, DispInfo, Face, HashMap, MapBsp, MapFaceVisibility,
    MapMeshClusterRanges, MapMeshIndexRange, MapMeshSurfaceOwner, MapMeshVisibility,
    MapSurfaceOwner, MapVertex, PendingLightmapBlock, TexInfo, brush_lightmap_uv,
    displacement_lightmap_uv, extract_face_lightmap, is_preview_material_visible,
    normalize_material_name,
};
use crate::math::Vec3;

#[derive(Clone, Debug, PartialEq)]
pub(super) struct BuildMesh {
    pub(super) vertices: Vec<BuildVertex>,
    pub(super) indices: Vec<u32>,
    pub(super) material_index: usize,
    pub(super) partition: GeometryPartition,
    pub(super) entity: bool,
    pub(super) visibility: BuildMeshVisibility,
    pub(super) surface_owners: Vec<MapMeshSurfaceOwner>,
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub(super) enum GeometryPartition {
    #[default]
    Visible,
    Skybox,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) struct BuildMeshKey {
    pub(super) material_index: usize,
    pub(super) partition: GeometryPartition,
    pub(super) entity: bool,
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
        entity: bool,
    ) -> &mut BuildMesh {
        let key = BuildMeshKey {
            material_index,
            partition,
            entity,
        };
        let index = *self.indexes.entry(key).or_insert_with(|| {
            let index = self.meshes.len();
            self.meshes.push(BuildMesh {
                vertices: Vec::new(),
                indices: Vec::new(),
                material_index,
                partition,
                entity,
                visibility: BuildMeshVisibility::default(),
                surface_owners: Vec::new(),
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

#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct BuildMeshVisibility {
    pub(super) always_visible: Vec<MapMeshIndexRange>,
    pub(super) clusters: BTreeMap<u32, Vec<MapMeshIndexRange>>,
}

impl BuildMeshVisibility {
    pub(super) fn push(&mut self, face_visibility: &MapFaceVisibility, range: MapMeshIndexRange) {
        if face_visibility.always_visible {
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

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct BuildVertex {
    pub(super) vertex: MapVertex,
    pub(super) lightmap_block: Option<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct DisplacementBuildVertex {
    pub(super) position: Vec3,
    pub(super) base_position: Vec3,
    pub(super) column: usize,
    pub(super) row: usize,
    pub(super) steps: usize,
    pub(super) alpha: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct DisplacementGridVertex {
    pub(super) position: Vec3,
    pub(super) base_position: Vec3,
    pub(super) alpha: f32,
}

pub(super) struct FaceAppendContext<'a> {
    pub(super) entity: bool,
    pub(super) receiver_owner: MapSurfaceOwner,
    pub(super) receiver_solid: bool,
    pub(super) transform: super::brushes::BrushTransform,
    pub(super) meshes: &'a mut BuildMeshes,
    pub(super) material_names: &'a mut Vec<String>,
    pub(super) material_indexes: &'a mut HashMap<String, usize>,
    pub(super) lightmap_samples: Option<&'a [ColorRgbExp]>,
    pub(super) lightmap_requirements: Option<&'a [super::SourceLightmapRequirement]>,
    pub(super) lightmap_sample_ends: &'a HashMap<i32, usize>,
    pub(super) lightmap_blocks: &'a mut Vec<PendingLightmapBlock>,
    pub(super) lightmap_stats: &'a mut super::MapLightmapStats,
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
    let Some(texinfo) = bsp.face_texinfo(face) else {
        return Ok(());
    };

    // BSP plane numbers already include face.side orientation.
    let normal = bsp.face_plane_normal(face);

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

    let sample_basis = super::lighting::LightmapBasis::from_bumped(texinfo.flags & 0x0800 != 0);
    let output_basis = match context.lightmap_requirements {
        Some(requirements) => match requirements.get(texinfo.texdata as usize) {
            Some(super::SourceLightmapRequirement::None) => None,
            Some(super::SourceLightmapRequirement::Lightmapped) => {
                Some(super::lighting::LightmapBasis::Scalar)
            }
            Some(super::SourceLightmapRequirement::Bumped) => {
                Some(super::lighting::LightmapBasis::Directional)
            }
            None => {
                return Err(BspError::Malformed {
                    message: "face material has no lightmap requirement",
                });
            }
        },
        None => Some(sample_basis),
    };
    let extraction = output_basis.map(|output_basis| {
        if face.light_offset == -1 || context.lightmap_samples.is_none() {
            Ok(None)
        } else {
            context
                .lightmap_samples
                .and_then(|samples| {
                    let end = context.lightmap_sample_ends.get(&face.light_offset)?;
                    samples.get(..*end)
                })
                .ok_or(super::lighting::LightmapExtractionError::InvalidPayload)
                .and_then(|samples| {
                    extract_face_lightmap(face, samples, sample_basis, output_basis)
                })
        }
    });
    if let Some(extraction) = &extraction {
        context.lightmap_stats.record_extraction(extraction);
    } else {
        context.lightmap_stats.unlightmapped_surfaces = context
            .lightmap_stats
            .unlightmapped_surfaces
            .saturating_add(1);
    }
    let lightmap_block = extraction.and_then(Result::ok).flatten().map(|block| {
        let index = context.lightmap_blocks.len();
        context.lightmap_blocks.push(block);
        index
    });
    let mesh = context
        .meshes
        .get_or_insert(material_index, partition, context.entity);
    let index_start = mesh.indices.len();
    let vertex_start = mesh.vertices.len();
    let displacement_projection = if let Some(displacement) = bsp.face_displacement(face) {
        let mut projection = append_displacement(
            bsp,
            face,
            displacement,
            texinfo,
            normal,
            mesh,
            lightmap_block,
        )?;
        projection.base_corners = projection
            .base_corners
            .map(|point| context.transform.point(point));
        for point in std::sync::Arc::make_mut(&mut projection.positions) {
            *point = context.transform.point(*point);
        }
        Some(projection)
    } else {
        append_brush_face(bsp, face, face_index, texinfo, normal, mesh, lightmap_block)?;
        None
    };
    // UV transforms use compiled model space, before entity placement.
    for vertex in &mut mesh.vertices[vertex_start..] {
        vertex.vertex.texture_size = bsp.texinfo_texdata(texinfo).map_or([0; 2], |data| {
            [
                u32::try_from(data.width).unwrap_or(0),
                u32::try_from(data.height).unwrap_or(0),
            ]
        });
        vertex.vertex.position = context.transform.point(vertex.vertex.position);
        vertex.vertex.base_position = context.transform.point(vertex.vertex.base_position);
        vertex.vertex.normal = context.transform.vector(vertex.vertex.normal);
        vertex.vertex.tangent_s = context.transform.vector(vertex.vertex.tangent_s);
        vertex.vertex.tangent_t = context.transform.vector(vertex.vertex.tangent_t);
    }
    let index_count = mesh.indices.len().saturating_sub(index_start);
    if index_count > 0
        && let (Ok(face_id), Ok(start), Ok(count)) = (
            u32::try_from(face_index),
            u32::try_from(index_start),
            u32::try_from(index_count),
        )
    {
        mesh.visibility.push(
            face_visibility,
            MapMeshIndexRange {
                face: face_id,
                start,
                count,
            },
        );
        let positions = bsp.face_vertex_positions(face);
        mesh.surface_owners.push(MapMeshSurfaceOwner {
            index_start: start,
            owner: context.receiver_owner,
            solid: context.receiver_solid,
            allows_decals: texinfo.flags & super::texture_flags::NODECALS == 0,
            plane_normal: context.transform.vector(normal),
            plane_distance: positions.first().map_or(0.0, |position| {
                context
                    .transform
                    .point(*position)
                    .dot(context.transform.vector(normal))
            }),
            displacement: displacement_projection,
            texture_projection: super::MapSurfaceTextureProjection::from_points(
                texinfo.texture_vecs,
                &positions,
            ),
        });
    }

    Ok(())
}

pub(super) fn append_brush_face(
    bsp: &MapBsp,
    face: &Face,
    face_index: usize,
    texinfo: &TexInfo,
    normal: Vec3,
    mesh: &mut BuildMesh,
    lightmap_block: Option<usize>,
) -> Result<(), BspError> {
    let base =
        u32::try_from(mesh.vertices.len()).map_err(|_| BspError::TooLarge { item: "vertices" })?;
    let positions = bsp.face_vertex_positions(face);
    let offset = bsp.face_normal_offsets.get(face_index).copied();
    for (corner, position) in positions.iter().copied().enumerate() {
        let corner_normal = offset
            .and_then(|o| bsp.vertex_normal_indices.get(o + corner))
            .and_then(|i| bsp.vertex_normals.get(usize::from(*i)))
            .copied()
            .filter(|normal| normal.is_finite())
            .unwrap_or(normal);
        let (tangent_s, tangent_t) = super::frames::texture_frame(corner_normal, normal, texinfo);
        mesh.vertices.push(BuildVertex {
            vertex: MapVertex {
                tangent_s,
                tangent_t,
                ..map_vertex(
                    position,
                    corner_normal,
                    texinfo,
                    lightmap_block.map_or([0.0; 2], |_| brush_lightmap_uv(position, texinfo, face)),
                    0.0,
                )
            },
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
    normal: Vec3,
    mesh: &mut BuildMesh,
    lightmap_block: Option<usize>,
) -> Result<super::MapDisplacementProjection, BspError> {
    let fallback;
    let normals = if let Some(normals) = usize::try_from(face.displacement)
        .ok()
        .and_then(|i| bsp.displacement_normals.get(i))
        .filter(|n| !n.normals.is_empty())
    {
        normals
    } else {
        fallback = super::frames::displacement_normals(bsp)?;
        fallback
            .get(usize::try_from(face.displacement).unwrap_or(usize::MAX))
            .ok_or(BspError::Malformed {
                message: "missing displacement normal grid",
            })?
    };
    let (grid, steps) = displacement_build_grid(bsp, face, displacement)?;
    for vertex in displacement_vertices_from_grid(&grid, steps, &displacement.allowed_vertices) {
        let grid_index = vertex.column * (vertex.steps + 1) + vertex.row;
        let vertex_normal = normals.normals.get(grid_index).copied().unwrap_or(normal);
        let stage = normals.stages.get(grid_index).copied().unwrap_or_default();
        let (tangent_s, tangent_t) = super::frames::displacement_frame(
            vertex_normal,
            normal,
            texinfo,
            stage,
            normals.tangent_s.get(grid_index).copied(),
        );
        let index = u32::try_from(mesh.vertices.len())
            .map_err(|_| BspError::TooLarge { item: "vertices" })?;
        mesh.vertices.push(BuildVertex {
            vertex: MapVertex {
                position: vertex.position,
                tangent_s,
                tangent_t,
                opacity: vertex.alpha,
                ..map_vertex(
                    vertex.base_position,
                    vertex_normal,
                    texinfo,
                    lightmap_block.map_or([0.0; 2], |_| {
                        displacement_lightmap_uv(vertex.column, vertex.row, vertex.steps, face)
                    }),
                    vertex.alpha,
                )
            },
            lightmap_block,
        });
        mesh.indices.push(index);
    }
    Ok(super::MapDisplacementProjection {
        base_corners: displacement_corner_positions(bsp, face, displacement)?,
        steps,
        positions: grid.iter().map(|vertex| vertex.position).collect(),
    })
}

pub(super) fn map_vertex(
    position: Vec3,
    normal: Vec3,
    texinfo: &TexInfo,
    lightmap_uv: [f32; 2],
    blend_alpha: f32,
) -> MapVertex {
    let (tangent_s, tangent_t) = super::frames::texture_frame(normal, normal, texinfo);
    MapVertex {
        position,
        base_position: position,
        normal,
        tangent_s,
        tangent_t,
        tex_s: texture_coord(position, texinfo.texture_vecs[0]),
        tex_t: texture_coord(position, texinfo.texture_vecs[1]),
        texture_size: [0; 2],
        lightmap_uv,
        has_lightmap: false,
        lightmap_bump_step: 0.0,
        blend_alpha,
        opacity: 1.0,
    }
}

pub(super) fn texture_coord(position: Vec3, transform: [f32; 4]) -> f32 {
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
    let (grid, steps) = displacement_build_grid(bsp, face, displacement)?;
    Ok(displacement_vertices_from_grid(
        &grid,
        steps,
        &displacement.allowed_vertices,
    ))
}

fn displacement_build_grid(
    bsp: &MapBsp,
    face: &Face,
    displacement: &DispInfo,
) -> Result<(Vec<DisplacementGridVertex>, usize), BspError> {
    if !(2..=4).contains(&displacement.power) {
        return Err(BspError::Malformed {
            message: "unsupported displacement power",
        });
    }
    let power =
        u32::try_from(displacement.power).map_err(|_| BspError::TooLarge { item: "vertices" })?;
    let steps = 2usize
        .checked_pow(power)
        .ok_or(BspError::TooLarge { item: "vertices" })?;
    let side = steps
        .checked_add(1)
        .ok_or(BspError::TooLarge { item: "vertices" })?;
    let grid = displacement_grid(bsp, face, displacement, steps)?;
    if grid.len() != side * side {
        return Err(BspError::Malformed {
            message: "incomplete displacement vertex grid",
        });
    }
    Ok((grid, steps))
}

fn displacement_vertices_from_grid(
    grid: &[DisplacementGridVertex],
    steps: usize,
    allowed: &[u32; 10],
) -> Vec<DisplacementBuildVertex> {
    let side = steps + 1;
    let all_allowed = (0..side * side).all(|index| allowed[index / 32] & (1 << (index % 32)) != 0);
    if all_allowed {
        return tessellate_displacement_grid(grid, steps, side);
    }
    displacement_allowed_triangles(steps, allowed)
        .into_iter()
        .flatten()
        .filter_map(|index| {
            grid.get(index).map(|vertex| DisplacementBuildVertex {
                position: vertex.position,
                base_position: vertex.base_position,
                column: index / side,
                row: index % side,
                steps,
                alpha: vertex.alpha,
            })
        })
        .collect()
}

fn displacement_allowed_triangles(steps: usize, allowed: &[u32; 10]) -> Vec<[usize; 3]> {
    fn active(point: [usize; 2], side: usize, allowed: &[u32; 10]) -> bool {
        let index = point[1] * side + point[0];
        allowed[index / 32] & (1 << (index % 32)) != 0
    }
    fn node(
        center: [usize; 2],
        radius: usize,
        side: usize,
        allowed: &[u32; 10],
        triangles: &mut Vec<[usize; 3]>,
    ) {
        let index = |point: [usize; 2]| point[1] * side + point[0];
        let offset = |delta: [i32; 2], distance: usize| {
            std::array::from_fn(|axis| {
                (center[axis] as i32 + delta[axis] * distance as i32) as usize
            })
        };
        let mut children = [false; 4];
        if radius > 1 {
            for (child, delta) in [[1, 1], [-1, 1], [-1, -1], [1, -1]].into_iter().enumerate() {
                let point = offset(delta, radius / 2);
                children[child] = active(point, side, allowed);
                if children[child] {
                    node(point, radius / 2, side, allowed, triangles);
                }
            }
        }
        // Omit inactive boundary points to match coarser neighboring edges.
        let winding = [
            ([1, -1], Some(3)),
            ([0, -1], None),
            ([-1, -1], Some(2)),
            ([-1, 0], None),
            ([-1, 1], Some(1)),
            ([0, 1], None),
            ([1, 1], Some(0)),
            ([1, 0], None),
            ([1, -1], Some(3)),
        ];
        let mut previous = None;
        for (delta, child) in winding {
            if child.is_some_and(|child| children[child]) {
                previous = None;
                continue;
            }
            let point = offset(delta, radius);
            if active(point, side, allowed) {
                let current = index(point);
                if let Some(previous) = previous {
                    triangles.push([previous, current, index(center)]);
                }
                previous = Some(current);
            }
        }
    }
    let mut triangles = Vec::new();
    node(
        [steps / 2; 2],
        steps / 2,
        steps + 1,
        allowed,
        &mut triangles,
    );
    triangles
}

pub(super) fn tessellate_displacement_grid(
    grid: &[DisplacementGridVertex],
    steps: usize,
    side: usize,
) -> Vec<DisplacementBuildVertex> {
    let index = |column: usize, row: usize| column * side + row;

    let mut vertices = Vec::with_capacity(steps.saturating_mul(steps).saturating_mul(6));
    for column in 0..steps {
        for row in 0..steps {
            let triangles = displacement_cell_triangles(column, row);
            for triangle in triangles {
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
                        base_position: vertex.base_position,
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

pub(super) fn displacement_cell_triangles(column: usize, row: usize) -> [[(usize, usize); 3]; 2] {
    if (column + row).is_multiple_of(2) {
        [
            [(column, row), (column + 1, row), (column + 1, row + 1)],
            [(column, row), (column + 1, row + 1), (column, row + 1)],
        ]
    } else {
        [
            [(column, row), (column + 1, row), (column, row + 1)],
            [(column + 1, row), (column + 1, row + 1), (column, row + 1)],
        ]
    }
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
        ((corner_positions[1] - corner_positions[0]) * step_scale),
        ((corner_positions[2] - corner_positions[3]) * step_scale),
    ];
    let base_positions = (0..=steps).flat_map(move |column| {
        (0..=steps).map(move |row| {
            let edge_positions = [
                (corner_positions[0] + (edge_intervals[0] * (column as f32))),
                (corner_positions[3] + (edge_intervals[1] * (column as f32))),
            ];
            let segment_interval = (edge_positions[1] - edge_positions[0]) * step_scale;
            edge_positions[0] + (segment_interval * (row as f32))
        })
    });

    Ok(bsp
        .displacement_vertices(displacement)
        .zip(base_positions)
        .map(|(displacement_vertex, base_position)| {
            let offset = Vec3::from(displacement_vertex.vector) * displacement_vertex.dist;
            DisplacementGridVertex {
                position: (base_position + offset),
                base_position,
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
) -> Result<[Vec3; 4], BspError> {
    let vertices = bsp.face_vertex_positions(face);
    let mut corners: [Vec3; 4] =
        vertices
            .as_slice()
            .try_into()
            .map_err(|_| BspError::Malformed {
                message: "displacement face is not four-sided",
            })?;
    let start = displacement.start_position;
    let start_index = corners
        .iter()
        .enumerate()
        .min_by(|(_, left), (_, right)| {
            ((**left) - Vec3::from(start))
                .length_squared()
                .total_cmp(&((**right) - Vec3::from(start)).length_squared())
        })
        .map_or(0, |(index, _)| index);
    corners.rotate_left(start_index);
    Ok(corners)
}

pub(super) fn fan_indices(vertex_count: usize) -> Result<Vec<u32>, BspError> {
    if vertex_count < 3 {
        return Ok(Vec::new());
    }

    let mut indices = Vec::with_capacity((vertex_count - 2) * 3);
    for index in 1..vertex_count - 1 {
        indices.push(0);
        indices.push(u32::try_from(index).map_err(|_| BspError::TooLarge { item: "indices" })?);
        indices.push(u32::try_from(index + 1).map_err(|_| BspError::TooLarge { item: "indices" })?);
    }
    Ok(indices)
}

#[cfg(test)]
mod allowed_vertex_tests {
    use super::*;

    #[test]
    fn shared_tessellator_full_allowed_mesh_matches_existing_checkerboard_for_every_power() {
        for power in 2..=4 {
            let steps = 1 << power;
            let side = steps + 1;
            let grid = (0..side)
                .flat_map(|column| {
                    (0..side).map(move |row| DisplacementGridVertex {
                        position: Vec3::new(column as f32, row as f32, 0.0),
                        base_position: Vec3::ZERO,
                        alpha: 0.0,
                    })
                })
                .collect::<Vec<_>>();
            let canonical = |mut triangles: Vec<[usize; 3]>| {
                for t in &mut triangles {
                    t.sort_unstable();
                }
                triangles.sort_unstable();
                triangles
            };
            let expected = tessellate_displacement_grid(&grid, steps, side)
                .chunks_exact(3)
                .map(|t| std::array::from_fn(|i| t[i].column * side + t[i].row))
                .collect();
            let actual = displacement_allowed_triangles(steps, &[u32::MAX; 10]);
            assert_eq!(actual.len(), steps * steps * 2);
            assert_eq!(canonical(actual), canonical(expected));
        }
    }

    #[test]
    fn disallowed_edge_vertex_is_omitted_and_adjacent_segments_merge_into_one_triangle() {
        let mut allowed = [u32::MAX; 10];
        allowed[0] &= !(1 << 9); // Source grid point x4,y1 in a power2 patch.
        let triangles = displacement_allowed_triangles(4, &allowed);
        assert_eq!(triangles.len(), 31);
        assert!(triangles.iter().all(|t| !t.contains(&9)));
        assert!(triangles.contains(&[14, 4, 8]));
        let area: i32 = triangles
            .iter()
            .map(|t| {
                let p = t.map(|i| [(i % 5) as i32, (i / 5) as i32]);
                ((p[1][0] - p[0][0]) * (p[2][1] - p[0][1])
                    - (p[1][1] - p[0][1]) * (p[2][0] - p[0][0]))
                    .abs()
            })
            .sum();
        assert_eq!(area, 32); // Twice the unchanged 4x4 patch area.
    }

    #[test]
    fn collision_and_rendering_ignore_authored_positions_of_disallowed_boundary_vertices() {
        let mut bsp = super::super::tests::displacement_test_bsp(
            2,
            [
                Vec3::ZERO,
                Vec3::new(256.0, 0.0, 0.0),
                Vec3::new(256.0, 256.0, 0.0),
                Vec3::new(0.0, 256.0, 0.0),
            ],
            0,
        );
        bsp.displacements[0].allowed_vertices[0] &= !(1 << 9);
        bsp.displacement_verts[9].dist = 128.0;
        let vertices = displacement_vertices(&bsp, &bsp.faces[0], &bsp.displacements[0]).unwrap();
        assert_eq!(vertices.len(), 31 * 3);
        assert!(vertices.iter().all(|v| v.position[2] == 0.0));
        let collision = super::super::walk::walk_displacements_from_bsp(&bsp);
        assert_eq!(collision.len(), 1);
        assert!(
            collision[0]
                .triangles
                .iter()
                .all(|triangle| triangle.vertices.iter().all(|vertex| vertex[2] == 0.0))
        );
        let mut collision_world = super::super::MapWalkCollision::empty();
        collision_world.displacements = collision;
        let hit = collision_world.trace_aabb(
            Vec3::new(64.0, 252.0, 256.0),
            Vec3::new(64.0, 252.0, -64.0),
            Vec3::ZERO,
        );
        assert!(hit.fraction < 1.0 && !hit.start_solid);
        assert!(hit.end_position[2].abs() < 1e-5);
    }
}
