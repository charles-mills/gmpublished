//! Geometry assembly: `.mdl` + `.vvd` + `.vtx` into render-ready meshes.
//!
//! - Triangle winding is flipped once here: strip decoding yields
//!   triangles wound opposite to brush geometry under a Cw-front
//!   convention.
//! - Static props keep stored vertices: every bone skins as identity.
//! - Skinning uses `pose_chain * pose_to_bone`, with a near-identity
//!   snap so pose==bind models keep exact stored vertices.
//! - A single-frame first animation is the rest pose and overrides
//!   bind locals per bone. A rest-pose channel missing its rotation
//!   or position falls back to the bone's BIND values.
//! - Malformed per-vertex data degrades to the stored vertex and is
//!   counted in [`MdlStats`].

use std::collections::BTreeMap;

use super::MdlError;
use super::studio::{FLAG_STATIC_PROP, Mdl, MdlAnimation};
use super::vtx::Vtx;
use super::vvd::{Vvd, VvdVertex};
use crate::math::sqrt_f32;

/// Assembled model geometry (LOD 0).
#[derive(Clone, Debug, PartialEq)]
pub struct ModelData {
    /// Render meshes, bodypart-major order.
    pub meshes: Vec<MeshData>,
    /// Material names from the `.mdl`.
    pub material_names: Vec<String>,
    /// Material search directories (`$cdmaterials`).
    pub material_dirs: Vec<String>,
    /// Skin tables mapping material slots per skin family.
    pub skin_tables: Vec<Vec<u16>>,
    /// Choice count per bodygroup, in bodypart order.
    pub bodygroups: Vec<usize>,
    /// Axis-aligned bounds over the skinned vertices.
    pub bounds_min: [f32; 3],
    /// See [`bounds_min`](Self::bounds_min).
    pub bounds_max: [f32; 3],
    /// Skeleton bone count.
    pub bone_count: u32,
    /// Declared sequence count.
    pub sequence_count: u32,
    /// Skinned vertex-pool size.
    pub vertex_count: u32,
    /// Total triangles across meshes.
    pub triangle_count: u32,
}

/// One renderable mesh.
#[derive(Clone, Debug, PartialEq)]
pub struct MeshData {
    /// Deduplicated vertices, first-use order.
    pub vertices: Vec<ModelVertex>,
    /// Triangle indices into [`vertices`](Self::vertices), winding
    /// already flipped for a Cw-front convention.
    pub indices: Vec<u32>,
    /// Index into [`ModelData::material_names`] (pre-skin-table).
    pub material_index: usize,
    /// Which bodygroup this mesh belongs to; a mesh renders only when
    /// its choice is the group's selected one.
    pub bodygroup: usize,
    /// The choice within the bodygroup.
    pub bodygroup_choice: usize,
}

/// One skinned vertex.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ModelVertex {
    /// Skinned model-space position.
    pub position: [f32; 3],
    /// Skinned unit normal (zero when unrecoverable).
    pub normal: [f32; 3],
    /// Texture coordinates.
    pub uv: [f32; 2],
}

/// Assembly result: geometry plus lossy-degradation accounting.
#[derive(Clone, Debug, PartialEq)]
pub struct ModelRead {
    /// The assembled geometry.
    pub model: ModelData,
    /// What was sanitized along the way.
    pub stats: MdlStats,
}

/// Counts of sanitized structures (the strict errors are in
/// [`MdlError`]; these are the degrade-and-continue cases).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MdlStats {
    /// Occurrences per degradation reason.
    pub sanitized: BTreeMap<AssemblySkip, usize>,
}

impl MdlStats {
    fn note(&mut self, reason: AssemblySkip) {
        *self.sanitized.entry(reason).or_default() += 1;
    }

    /// Total degradations of any kind.
    #[must_use]
    pub fn total(&self) -> usize {
        self.sanitized.values().sum()
    }
}

/// A degrade-and-continue reason recorded during assembly. Inspect
/// [`MdlStats::sanitized`] when an assembled model looks wrong — these
/// say what was repaired and roughly where to look.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum AssemblySkip {
    /// A rest ($sequence "idle"-style) animation spans several frames;
    /// only frame 0 feeds the rest pose.
    MultiFrameRestAnimation,
    /// A rest animation addresses a bone the model does not have.
    RestBoneOutOfRange,
    /// A rest animation channel held non-finite values; the bind pose
    /// was used instead.
    RestPoseInvalid,
    /// A rest channel was only partially present; missing components
    /// fell back to the bind pose.
    RestChannelUsedBindFallback,
    /// A bone's parent index is out of range; treated as a root bone.
    InvalidBoneParent,
    /// A bone's bind-pose matrix was non-finite; replaced with identity.
    InvalidBindPose,
    /// A pose-to-bone matrix came out non-finite; replaced with identity.
    NonFinitePoseToBone,
    /// A final skinning matrix came out non-finite; replaced with identity.
    NonFiniteSkinningMatrix,
    /// A vertex position was non-finite; zeroed.
    VertexInvalidPosition,
    /// A vertex normal was non-finite or unnormalizable; defaulted.
    VertexInvalidNormal,
    /// A vertex declared more weights than the format allows; clamped.
    VertexInvalidWeightCount,
    /// A vertex weight was non-finite or negative; zeroed.
    VertexInvalidWeight,
    /// A vertex bone index was out of range; remapped to bone 0.
    VertexInvalidBoneIndex,
    /// A skinned vertex came out non-finite; the unskinned position won.
    VertexNonFiniteSkinning,
    /// All of a vertex's weights were zero; skinning was skipped.
    VertexZeroWeightSum,
    /// The `.mdl`/`.vvd`/`.vtx` trio's checksums disagreed; assembly
    /// proceeded anyway ([`assemble_lossy`] only — [`assemble`] errors).
    ChecksumMismatch,
    /// The `.mdl`'s mesh count and the `.vtx`'s LOD-0 mesh count
    /// disagreed; assembly zip-truncated to the shorter list
    /// ([`assemble_lossy`] only — [`assemble`] errors).
    MeshCountMismatch,
}

/// Assemble LOD-0 render geometry from the three parsed files.
///
/// Inputs are trusted to the extent [`parse_mdl`](super::parse_mdl) /
/// [`parse_vvd`](super::parse_vvd) / [`parse_vtx`](super::parse_vtx)
/// validated them (their `Limits` bound every table); cross-file
/// disagreements degrade per [`AssemblySkip`] or fail with a typed
/// error, never panic.
///
/// Errors if the trio's checksums disagree or their mesh counts don't
/// match: a `.mdl` is only meaningfully paired with the `.vvd`/`.vtx`
/// that shipped alongside it, and zipping mismatched mesh lists
/// silently pairs unrelated triangles with unrelated vertices. Use
/// [`assemble_lossy`] to render a best-effort mismatched trio instead.
pub fn assemble(mdl: &Mdl, vvd: &Vvd, vtx: &Vtx) -> Result<ModelRead, MdlError> {
    assemble_impl(mdl, vvd, vtx, true)
}

/// Same as [`assemble`], but a checksum or mesh-count mismatch is
/// recorded in the returned [`MdlStats`] (see
/// [`AssemblySkip::ChecksumMismatch`] and
/// [`AssemblySkip::MeshCountMismatch`]) instead of failing; a mismatched
/// mesh count still zip-truncates to the shorter list.
pub fn assemble_lossy(mdl: &Mdl, vvd: &Vvd, vtx: &Vtx) -> Result<ModelRead, MdlError> {
    assemble_impl(mdl, vvd, vtx, false)
}

fn assemble_impl(mdl: &Mdl, vvd: &Vvd, vtx: &Vtx, strict: bool) -> Result<ModelRead, MdlError> {
    let mut stats = MdlStats::default();
    let static_prop = mdl.flags & FLAG_STATIC_PROP != 0;

    if mdl.checksum != vvd.checksum || mdl.checksum != vtx.checksum {
        if strict {
            return Err(MdlError::ChecksumMismatch {
                mdl: mdl.checksum,
                vvd: vvd.checksum,
                vtx: vtx.checksum,
            });
        }
        stats.note(AssemblySkip::ChecksumMismatch);
    }

    let geometry = prepare_model_geometry(mdl, vvd, static_prop, &mut stats);
    let skin_tables = skin_tables(mdl);
    let bodygroups: Vec<usize> = mdl
        .body_parts
        .iter()
        .map(|part| part.models.len())
        .collect();

    // Pair mdl meshes with vtx LOD-0 meshes, both flattened
    // bodypart-major; a count mismatch either errors (strict) or
    // zip-truncates to the shorter list (lossy, predecessor behavior).
    let mdl_meshes: Vec<_> = mdl
        .body_parts
        .iter()
        .enumerate()
        .flat_map(|(group, part)| {
            part.models
                .iter()
                .enumerate()
                .flat_map(move |(choice, model)| {
                    model
                        .meshes
                        .iter()
                        .map(move |mesh| (group, choice, model.vertex_offset, mesh))
                })
        })
        .collect();
    let vtx_meshes: Vec<_> = vtx
        .body_parts
        .iter()
        .flat_map(|part| part.models.iter())
        .flat_map(|model| model.lods.first())
        .flat_map(|lod| lod.meshes.iter())
        .collect();

    if mdl_meshes.len() != vtx_meshes.len() {
        if strict {
            return Err(MdlError::MeshCountMismatch {
                mdl: mdl_meshes.len(),
                vtx: vtx_meshes.len(),
            });
        }
        stats.note(AssemblySkip::MeshCountMismatch);
    }

    let mut meshes = Vec::new();
    for ((group, choice, model_offset, mdl_mesh), vtx_mesh) in
        mdl_meshes.into_iter().zip(vtx_meshes)
    {
        let material_index = usize::try_from(mdl_mesh.material)
            .ok()
            .filter(|index| *index < mdl.textures.len())
            .ok_or(MdlError::MaterialIndex {
                index: mdl_mesh.material,
            })?;

        let mut local_indices = BTreeMap::<usize, u32>::new();
        let mut vertices = Vec::new();
        let mut indices = Vec::new();
        let base_offset = vertex_base_offset(model_offset, mdl_mesh.vertex_offset)?;

        for strip_group in &vtx_mesh.strip_groups {
            for strip in &strip_group.strips {
                for position in strip.triangle_index_positions() {
                    // parse_vtx validates strip ranges against the pool,
                    // but `Vtx` is freely constructible: check anyway.
                    let pool_value = strip_group
                        .indices
                        .get(position)
                        .copied()
                        .map(usize::from)
                        .ok_or(MdlError::Corrupt {
                            part: "vtx strip index range",
                        })?;
                    let vertex = strip_group
                        .vertices
                        .get(pool_value)
                        .ok_or(MdlError::Corrupt {
                            part: "vtx strip vertex id",
                        })?;
                    let source_index = base_offset
                        .checked_add(usize::from(vertex.original_mesh_vertex_id))
                        .ok_or(MdlError::Corrupt {
                            part: "vertex index overflow",
                        })?;

                    let local_index =
                        if let Some(index) = local_indices.get(&source_index).copied() {
                            index
                        } else {
                            let source_vertex = geometry.vertices.get(source_index).ok_or(
                                MdlError::VertexIndex {
                                    index: source_index,
                                },
                            )?;
                            let local_index = u32::try_from(vertices.len())
                                .map_err(|_| MdlError::TooManyVertices)?;
                            vertices.push(*source_vertex);
                            local_indices.insert(source_index, local_index);
                            local_index
                        };
                    indices.push(local_index);
                }
            }
        }
        flip_triangle_winding(&mut indices);
        meshes.push(MeshData {
            vertices,
            indices,
            material_index,
            bodygroup: group,
            bodygroup_choice: choice,
        });
    }

    let triangle_count = count_to_u32(meshes.iter().map(|mesh| mesh.indices.len() / 3).sum());
    let vertex_count = count_to_u32(geometry.vertices.len());

    Ok(ModelRead {
        model: ModelData {
            meshes,
            material_names: mdl.textures.clone(),
            material_dirs: mdl.texture_paths.clone(),
            skin_tables,
            bodygroups,
            bounds_min: geometry.bounds.0,
            bounds_max: geometry.bounds.1,
            bone_count: count_to_u32(mdl.bones.len()),
            sequence_count: mdl.sequence_count,
            vertex_count,
            triangle_count,
        },
        stats,
    })
}

fn vertex_base_offset(model_offset: i32, mesh_offset: i32) -> Result<usize, MdlError> {
    let base = i64::from(model_offset) + i64::from(mesh_offset);
    usize::try_from(base).map_err(|_| MdlError::Corrupt {
        part: "mdl mesh vertex offset",
    })
}

fn flip_triangle_winding(indices: &mut [u32]) {
    for triangle in indices.chunks_exact_mut(3) {
        triangle.swap(1, 2);
    }
}

struct PreparedModelGeometry {
    vertices: Vec<ModelVertex>,
    bounds: ([f32; 3], [f32; 3]),
}

/// Column-major 4x4 affine transform. Source stores `pose_to_bone` as a
/// row-major 3x4 matrix; convert at that boundary and keep all internal
/// affine math in this one convention.
type Mat4 = [[f32; 4]; 4];

const MAT4_IDENTITY: Mat4 = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
];

const IDENTITY_SNAP_EPSILON: f32 = 1.0e-3;
const QUAT_UNIT_EPSILON: f32 = 1.0e-3;

/// Unit quaternion as `[w, x, y, z]` (internal convention; the studio
/// file stores `[x, y, z, w]`).
type Quat = [f32; 4];

#[derive(Clone, Copy, Debug)]
struct LocalBonePose {
    position: [f32; 3],
    rotation: Quat,
}

fn prepare_model_geometry(
    mdl: &Mdl,
    vvd: &Vvd,
    static_prop: bool,
    stats: &mut MdlStats,
) -> PreparedModelGeometry {
    let rest_overrides = rest_animation_local_overrides(mdl, stats);
    let bone_transforms = pose_to_bone_skinning_matrices(mdl, static_prop, &rest_overrides, stats);
    let vertices: Vec<ModelVertex> = vvd
        .vertices
        .iter()
        .map(|vertex| model_vertex(&bone_transforms, vertex, stats))
        .collect();
    let bounds = bounds_from_vertices(&vertices);
    PreparedModelGeometry { vertices, bounds }
}

fn stored_quat_to_wxyz(quat: [f32; 4]) -> Quat {
    [quat[3], quat[0], quat[1], quat[2]]
}

fn rest_animation_local_overrides(mdl: &Mdl, stats: &mut MdlStats) -> Vec<Option<LocalBonePose>> {
    let mut overrides = vec![None; mdl.bones.len()];
    let Some(desc) = mdl.local_animations.first() else {
        return overrides;
    };
    if desc.frame_count != 1 {
        stats.note(AssemblySkip::MultiFrameRestAnimation);
        return overrides;
    }

    for animation in &desc.animations {
        let bone_index = usize::from(animation.bone);
        if bone_index >= overrides.len() {
            stats.note(AssemblySkip::RestBoneOutOfRange);
            continue;
        }
        let (position, rotation) = rest_channel_pose(&mdl.bones[bone_index], animation, stats);
        if !array_is_finite(position) || !quat_is_valid_unit(rotation) {
            stats.note(AssemblySkip::RestPoseInvalid);
            continue;
        }
        overrides[bone_index] = Some(LocalBonePose { position, rotation });
    }
    overrides
}

/// Deliberate divergence: a channel missing rotation or position uses
/// the bone's bind value for the missing component.
fn rest_channel_pose(
    bone: &super::studio::MdlBone,
    animation: &MdlAnimation,
    stats: &mut MdlStats,
) -> ([f32; 3], Quat) {
    let position = if animation.has_position() {
        animation.position(0)
    } else {
        stats.note(AssemblySkip::RestChannelUsedBindFallback);
        bone.position
    };
    let rotation = if animation.has_rotation() {
        stored_quat_to_wxyz(animation.rotation(0))
    } else {
        stats.note(AssemblySkip::RestChannelUsedBindFallback);
        stored_quat_to_wxyz(bone.quaternion)
    };
    (position, rotation)
}

fn pose_to_bone_skinning_matrices(
    mdl: &Mdl,
    static_prop: bool,
    rest_overrides: &[Option<LocalBonePose>],
    stats: &mut MdlStats,
) -> Vec<Mat4> {
    let bones = &mdl.bones;
    if bones.is_empty() {
        return Vec::new();
    }
    if static_prop {
        // Source static props ignore bone posing; preserve stored
        // model-space vertices via identity transforms.
        return vec![MAT4_IDENTITY; bones.len()];
    }

    let mut pose_chains: Vec<Mat4> = Vec::with_capacity(bones.len());
    let mut skinning_transforms: Vec<Mat4> = Vec::with_capacity(bones.len());
    for (index, bone) in bones.iter().enumerate() {
        let local = if let Some(Some(rest_pose)) = rest_overrides.get(index) {
            local_pose_matrix(*rest_pose)
        } else {
            local_bind_pose_matrix(bone, stats)
        };

        let pose_chain = usize::try_from(bone.parent)
            .ok()
            .filter(|parent| *parent < pose_chains.len())
            .map_or_else(
                || {
                    if bone.parent >= 0 {
                        stats.note(AssemblySkip::InvalidBoneParent);
                    }
                    local
                },
                |parent| mat4_mul(pose_chains[parent], local),
            );
        pose_chains.push(pose_chain);

        let pose_to_bone = mat4_from_pose_to_bone(bone.pose_to_bone).unwrap_or_else(|| {
            stats.note(AssemblySkip::NonFinitePoseToBone);
            MAT4_IDENTITY
        });
        let transform = mat4_mul(pose_chain, pose_to_bone);
        skinning_transforms.push(if matrix_is_finite(&transform) {
            snap_near_identity(transform)
        } else {
            stats.note(AssemblySkip::NonFiniteSkinningMatrix);
            MAT4_IDENTITY
        });
    }
    skinning_transforms
}

fn local_bind_pose_matrix(bone: &super::studio::MdlBone, stats: &mut MdlStats) -> Mat4 {
    let rotation = stored_quat_to_wxyz(bone.quaternion);
    if array_is_finite(bone.position) && quat_is_valid_unit(rotation) {
        local_pose_matrix(LocalBonePose {
            position: bone.position,
            rotation,
        })
    } else {
        stats.note(AssemblySkip::InvalidBindPose);
        MAT4_IDENTITY
    }
}

fn local_pose_matrix(pose: LocalBonePose) -> Mat4 {
    mat4_mul(
        mat4_from_translation(pose.position),
        mat4_from_quat(pose.rotation),
    )
}

/// Row-major stored 3x4 to column-major 4x4.
fn mat4_from_pose_to_bone(rows: [[f32; 4]; 3]) -> Option<Mat4> {
    if rows.iter().flatten().any(|cell| !cell.is_finite()) {
        return None;
    }
    Some([
        [rows[0][0], rows[1][0], rows[2][0], 0.0],
        [rows[0][1], rows[1][1], rows[2][1], 0.0],
        [rows[0][2], rows[1][2], rows[2][2], 0.0],
        [rows[0][3], rows[1][3], rows[2][3], 1.0],
    ])
}

fn mat4_mul(a: Mat4, b: Mat4) -> Mat4 {
    let mut out = [[0.0; 4]; 4];
    for (column, out_column) in out.iter_mut().enumerate() {
        for (row, out_cell) in out_column.iter_mut().enumerate() {
            *out_cell = (0..4).map(|k| a[k][row] * b[column][k]).sum();
        }
    }
    out
}

fn mat4_from_translation(translation: [f32; 3]) -> Mat4 {
    let mut out = MAT4_IDENTITY;
    out[3][..3].copy_from_slice(&translation);
    out
}

fn mat4_from_quat(quat: Quat) -> Mat4 {
    let [w, x, y, z] = quat;
    let (x2, y2, z2) = (x + x, y + y, z + z);
    let (xx, yy, zz) = (x * x2, y * y2, z * z2);
    let (xy, yz, xz) = (x * y2, y * z2, x * z2);
    let (wx, wy, wz) = (w * x2, w * y2, w * z2);
    [
        [1.0 - yy - zz, xy + wz, xz - wy, 0.0],
        [xy - wz, 1.0 - xx - zz, yz + wx, 0.0],
        [xz + wy, yz - wx, 1.0 - xx - yy, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

fn mat4_transform_point(matrix: Mat4, point: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|row| {
        matrix[0][row] * point[0]
            + matrix[1][row] * point[1]
            + matrix[2][row] * point[2]
            + matrix[3][row]
    })
}

fn mat4_transform_direction(matrix: Mat4, direction: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|row| {
        matrix[0][row] * direction[0]
            + matrix[1][row] * direction[1]
            + matrix[2][row] * direction[2]
    })
}

fn model_vertex(bone_transforms: &[Mat4], vertex: &VvdVertex, stats: &mut MdlStats) -> ModelVertex {
    let (position, normal) = skin_model_vertex(bone_transforms, vertex, stats);
    ModelVertex {
        position,
        normal,
        uv: vertex.uv,
    }
}

fn skin_model_vertex(
    bone_transforms: &[Mat4],
    vertex: &VvdVertex,
    stats: &mut MdlStats,
) -> ([f32; 3], [f32; 3]) {
    if !array_is_finite(vertex.position) {
        stats.note(AssemblySkip::VertexInvalidPosition);
        return ([0.0; 3], safe_normal(vertex.normal));
    }
    if !array_is_finite(vertex.normal) {
        stats.note(AssemblySkip::VertexInvalidNormal);
        return (sanitize_vector(vertex.position), [0.0; 3]);
    }

    let count = usize::from(vertex.bone_count);
    if count == 0 || count > 3 {
        stats.note(AssemblySkip::VertexInvalidWeightCount);
        return stored_vertex(vertex);
    }

    let mut position = [0.0; 3];
    let mut normal = [0.0; 3];
    let mut weight_sum = 0.0_f32;
    for slot in 0..count {
        let weight = vertex.weights[slot];
        if !weight.is_finite() || weight < 0.0 {
            stats.note(AssemblySkip::VertexInvalidWeight);
            return stored_vertex(vertex);
        }
        if weight <= f32::EPSILON {
            continue;
        }
        let bone_index = usize::from(vertex.bones[slot]);
        let Some(transform) = bone_transforms.get(bone_index).copied() else {
            stats.note(AssemblySkip::VertexInvalidBoneIndex);
            return stored_vertex(vertex);
        };
        let skinned_position = sanitize_vector(mat4_transform_point(transform, vertex.position));
        let skinned_normal = normalize_vector(sanitize_vector(mat4_transform_direction(
            transform,
            vertex.normal,
        )));
        if !array_is_finite(skinned_position) || !array_is_finite(skinned_normal) {
            stats.note(AssemblySkip::VertexNonFiniteSkinning);
            return stored_vertex(vertex);
        }
        for axis in 0..3 {
            position[axis] += skinned_position[axis] * weight;
            normal[axis] += skinned_normal[axis] * weight;
        }
        weight_sum += weight;
    }

    if !weight_sum.is_finite() || weight_sum <= f32::EPSILON {
        stats.note(AssemblySkip::VertexZeroWeightSum);
        return stored_vertex(vertex);
    }

    for axis in 0..3 {
        position[axis] /= weight_sum;
        normal[axis] /= weight_sum;
    }
    (position, normalize_vector(normal))
}

fn stored_vertex(vertex: &VvdVertex) -> ([f32; 3], [f32; 3]) {
    (sanitize_vector(vertex.position), safe_normal(vertex.normal))
}

fn safe_normal(normal: [f32; 3]) -> [f32; 3] {
    if array_is_finite(normal) {
        normalize_vector(normal)
    } else {
        [0.0; 3]
    }
}

fn quat_is_valid_unit(quat: Quat) -> bool {
    quat.iter().all(|cell| cell.is_finite())
        && (quat.iter().map(|cell| cell * cell).sum::<f32>() - 1.0).abs() <= QUAT_UNIT_EPSILON
}

fn matrix_is_finite(matrix: &Mat4) -> bool {
    matrix.iter().flatten().all(|cell| cell.is_finite())
}

fn snap_near_identity(matrix: Mat4) -> Mat4 {
    let mut deviation = 0.0_f32;
    for column in 0..4 {
        for row in 0..4 {
            deviation = deviation.max((matrix[column][row] - MAT4_IDENTITY[column][row]).abs());
        }
    }
    if deviation < IDENTITY_SNAP_EPSILON {
        MAT4_IDENTITY
    } else {
        matrix
    }
}

fn array_is_finite(vector: [f32; 3]) -> bool {
    vector.into_iter().all(f32::is_finite)
}

fn sanitize_vector(vector: [f32; 3]) -> [f32; 3] {
    if array_is_finite(vector) {
        vector
    } else {
        [0.0; 3]
    }
}

fn normalize_vector(vector: [f32; 3]) -> [f32; 3] {
    let length = sqrt_f32(vector[0] * vector[0] + vector[1] * vector[1] + vector[2] * vector[2]);
    if length <= f32::EPSILON {
        [0.0; 3]
    } else {
        [vector[0] / length, vector[1] / length, vector[2] / length]
    }
}

fn skin_tables(mdl: &Mdl) -> Vec<Vec<u16>> {
    let material_count = mdl.textures.len();
    let identity = || {
        (0..material_count)
            .map(material_index_to_u16)
            .collect::<Vec<_>>()
    };
    if mdl.skin_reference_count == 0 || mdl.skin_table.is_empty() {
        return vec![identity()];
    }
    mdl.skin_table
        .chunks(mdl.skin_reference_count)
        .map(|family| {
            (0..material_count)
                .map(|index| {
                    family
                        .get(index)
                        .map_or_else(|| material_index_to_u16(index), |mapped| *mapped)
                })
                .collect()
        })
        .collect()
}

fn material_index_to_u16(index: usize) -> u16 {
    u16::try_from(index).unwrap_or(u16::MAX)
}

fn bounds_from_vertices(vertices: &[ModelVertex]) -> ([f32; 3], [f32; 3]) {
    let Some(first) = vertices.first() else {
        return ([0.0; 3], [0.0; 3]);
    };
    let mut min = first.position;
    let mut max = first.position;
    for vertex in &vertices[1..] {
        for axis in 0..3 {
            min[axis] = min[axis].min(vertex.position[axis]);
            max[axis] = max[axis].max(vertex.position[axis]);
        }
    }
    (min, max)
}

fn count_to_u32(count: usize) -> u32 {
    u32::try_from(count).unwrap_or(u32::MAX)
}

#[cfg(test)]
#[path = "assembly_tests.rs"]
mod tests;
