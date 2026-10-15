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
use super::vertex_compression::{
    VertexCompression, source_short2_weights, source_ubyte4_tangent, source_ubyte4_vector,
};
use super::vtx::Vtx;
use super::vvd::{Vvd, VvdVertex};

/// Model assembly's physical vertex-stream choices.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ModelAssemblyOptions {
    /// Applied before bone blending; authored source frames remain available.
    pub vertex_compression: VertexCompression,
}

/// Authored VVD frame, in original materialized LOD-0 vertex-pool order.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ModelSourceVertexFrame {
    /// Raw model-space authored position and unmodified skinning channels.
    pub position: [f32; 3],
    /// Raw VVD weights, before physical-stream quantization.
    pub weights: [f32; 3],
    /// Raw authored bone indices for the three weight slots.
    pub bones: [u8; 3],
    /// Authored number of populated skinning slots.
    pub bone_count: u8,
    /// Raw model-space authored normal.
    pub normal: [f32; 3],
    /// Raw authored tangent XYZ and independent handedness W.
    pub tangent: Option<[f32; 4]>,
}

/// Assembled model geometry (LOD 0).
#[derive(Clone, Debug, PartialEq)]
pub struct ModelData {
    /// Physical stream representation used for assembled vertices.
    pub vertex_compression: VertexCompression,
    /// Authored frames retained independently of physical stream encoding.
    pub source_frames: Vec<ModelSourceVertexFrame>,
    /// Authored model-space illumination position, before instance placement.
    pub illumination_position: [f32; 3],
    /// Authored model-space hull minimum; independent of assembled bounds.
    pub hull_min: [f32; 3],
    /// Authored model-space hull maximum.
    pub hull_max: [f32; 3],
    /// Optional zero-based authored illumination attachment selector.
    pub illumination_attachment_index: Option<usize>,
    /// Selected attachment position after rest bone posing, before entity placement.
    /// `None` with a selector indicates an unresolved virtual-model attachment.
    pub illumination_attachment_position: Option<[f32; 3]>,
    /// Local attachments assembled with the same runtime bone pose as geometry.
    pub attachments: Vec<ModelAttachment>,
    /// Raw authored studio-header flags, independent of runtime compact flags.
    pub mdl_flags: u32,
    /// Checksum of the source `.mdl`, used to validate compiled prop lighting.
    pub checksum: u32,
    /// Whether the source model companions agree sufficiently to attach VHV data.
    pub lighting_compatible: bool,
    /// Hardware strip-group directory in bodypart/model/LOD/mesh/group order.
    pub lighting_meshes: Vec<ModelLightingMesh>,
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

/// Bone-posed attachment before entity placement or model scaling.
#[derive(Clone, Debug, PartialEq)]
pub struct ModelAttachment {
    /// Authored record, including bone-local matrix and flags.
    pub source: super::MdlAttachment,
    /// Row-major model-space matrix; world-align flags remain for placement.
    pub model: [[f32; 4]; 3],
}

/// One renderable mesh.
#[derive(Clone, Debug, PartialEq)]
pub struct MeshData {
    /// Deduplicated vertices, first-use order.
    pub vertices: Vec<ModelVertex>,
    /// VHV hardware vertex identity, parallel to [`vertices`](Self::vertices).
    pub vertex_lighting: Vec<ModelLightingVertex>,
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

/// One VHV hardware mesh, corresponding to one VTX strip group.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModelLightingMesh {
    /// Source model level of detail.
    pub lod: u32,
    /// Full strip-group vertex-pool length, including unused vertices.
    pub vertex_count: usize,
}

/// A render vertex's location in the compiled hardware lighting streams.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModelLightingVertex {
    /// Index into [`ModelData::lighting_meshes`] and the VHV mesh directory.
    pub mesh: usize,
    /// Index into that strip group's original vertex pool.
    pub vertex: usize,
}

/// One skinned vertex.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ModelVertex {
    /// Skinned model-space position.
    pub position: [f32; 3],
    /// Skinned normal (zero when unrecoverable).
    pub normal: [f32; 3],
    /// Authored VVD S tangent transformed by the vertex's bone blend.
    pub tangent_s: [f32; 3],
    /// Binormal reconstructed from normal, tangent and authored handedness.
    pub tangent_t: [f32; 3],
    /// Authored tangent handedness; retained independently of degenerate frames.
    pub tangent_sign: f32,
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
    /// A vertex has no usable authored VVD tangent; its tangent frame is zero.
    VertexMissingTangent,
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
    assemble_impl(mdl, vvd, vtx, true, ModelAssemblyOptions::default())
}

/// Same as [`assemble`], but a checksum or mesh-count mismatch is
/// recorded in the returned [`MdlStats`] (see
/// [`AssemblySkip::ChecksumMismatch`] and
/// [`AssemblySkip::MeshCountMismatch`]) instead of failing; a mismatched
/// mesh count still zip-truncates to the shorter list.
pub fn assemble_lossy(mdl: &Mdl, vvd: &Vvd, vtx: &Vtx) -> Result<ModelRead, MdlError> {
    assemble_lossy_with_options(mdl, vvd, vtx, ModelAssemblyOptions::default())
}

/// Best-effort assembly with an explicit physical stream representation.
pub fn assemble_lossy_with_options(
    mdl: &Mdl,
    vvd: &Vvd,
    vtx: &Vtx,
    options: ModelAssemblyOptions,
) -> Result<ModelRead, MdlError> {
    assemble_impl(mdl, vvd, vtx, false, options)
}

fn assemble_impl(
    mdl: &Mdl,
    vvd: &Vvd,
    vtx: &Vtx,
    strict: bool,
    options: ModelAssemblyOptions,
) -> Result<ModelRead, MdlError> {
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

    let geometry = prepare_model_geometry_with_options(mdl, vvd, static_prop, &mut stats, options)?;
    let skin_tables = skin_tables(mdl);
    let bodygroups: Vec<usize> = mdl
        .body_parts
        .iter()
        .map(|part| part.models.len())
        .collect();

    // Pair mdl meshes with vtx LOD-0 meshes, both flattened
    // bodypart-major; a count mismatch either errors (strict) or
    // zip-truncates to the shorter list (lossy).
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
    let mut lighting_meshes = Vec::new();
    let mut vtx_meshes = Vec::new();
    for part in &vtx.body_parts {
        for model in &part.models {
            for (lod_index, lod) in model.lods.iter().enumerate() {
                for mesh in &lod.meshes {
                    let lighting_base = lighting_meshes.len();
                    lighting_meshes.extend(mesh.strip_groups.iter().map(|group| {
                        ModelLightingMesh {
                            lod: count_to_u32(lod_index),
                            vertex_count: group.vertices.len(),
                        }
                    }));
                    if lod_index == 0 {
                        vtx_meshes.push((mesh, lighting_base));
                    }
                }
            }
        }
    }
    let lighting_compatible = mdl.checksum == vvd.checksum
        && mdl.checksum == vtx.checksum
        && mdl_meshes.len() == vtx_meshes.len();

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
    for ((group, choice, model_offset, mdl_mesh), (vtx_mesh, lighting_base)) in
        mdl_meshes.into_iter().zip(vtx_meshes)
    {
        let material_index = usize::try_from(mdl_mesh.material)
            .ok()
            .filter(|index| *index < mdl.textures.len())
            .ok_or(MdlError::MaterialIndex {
                index: mdl_mesh.material,
            })?;

        let mut local_indices = BTreeMap::<(usize, usize), u32>::new();
        let mut vertices = Vec::new();
        let mut vertex_lighting = Vec::new();
        let mut indices = Vec::new();
        let base_offset = vertex_base_offset(model_offset, mdl_mesh.vertex_offset)?;

        for (strip_group_index, strip_group) in vtx_mesh.strip_groups.iter().enumerate() {
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

                    let hardware_vertex = (strip_group_index, pool_value);
                    let local_index =
                        if let Some(index) = local_indices.get(&hardware_vertex).copied() {
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
                            vertex_lighting.push(ModelLightingVertex {
                                mesh: lighting_base + strip_group_index,
                                vertex: pool_value,
                            });
                            local_indices.insert(hardware_vertex, local_index);
                            local_index
                        };
                    indices.push(local_index);
                }
            }
        }
        flip_triangle_winding(&mut indices);
        meshes.push(MeshData {
            vertices,
            vertex_lighting,
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
            vertex_compression: options.vertex_compression,
            source_frames: vvd
                .vertices
                .iter()
                .enumerate()
                .map(|(index, vertex)| ModelSourceVertexFrame {
                    position: vertex.position,
                    weights: vertex.weights,
                    bones: vertex.bones,
                    bone_count: vertex.bone_count,
                    normal: vertex.normal,
                    tangent: vvd.tangents.get(index).copied(),
                })
                .collect(),
            illumination_position: mdl.illumination_position,
            hull_min: mdl.hull_min,
            hull_max: mdl.hull_max,
            illumination_attachment_index: mdl.illumination_attachment_index,
            illumination_attachment_position: mdl
                .illumination_attachment_index
                .and_then(|index| geometry.attachments.get(index))
                .map(|attachment| attachment.model.map(|row| row[3])),
            attachments: geometry.attachments,
            mdl_flags: mdl.flags,
            checksum: mdl.checksum,
            lighting_compatible,
            lighting_meshes,
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
    attachments: Vec<ModelAttachment>,
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

#[cfg(test)]
fn prepare_model_geometry(
    mdl: &Mdl,
    vvd: &Vvd,
    static_prop: bool,
    stats: &mut MdlStats,
) -> PreparedModelGeometry {
    prepare_model_geometry_with_options(
        mdl,
        vvd,
        static_prop,
        stats,
        ModelAssemblyOptions::default(),
    )
    .expect("valid fixture model")
}

fn prepare_model_geometry_with_options(
    mdl: &Mdl,
    vvd: &Vvd,
    static_prop: bool,
    stats: &mut MdlStats,
    options: ModelAssemblyOptions,
) -> Result<PreparedModelGeometry, MdlError> {
    let bones = mdl.runtime_bones()?;
    let rest_overrides = rest_animation_local_overrides(mdl, &bones, stats);
    let pose_chains = if !static_prop || !mdl.attachments.is_empty() {
        bone_pose_chains(&bones, &rest_overrides, stats)
    } else {
        Vec::new()
    };
    let bone_transforms = skinning_matrices(&bones, static_prop, &pose_chains, stats);
    let attachments = mdl
        .attachments
        .iter()
        .map(|source| {
            let bone = pose_chains
                .get(source.local_bone)
                .copied()
                .ok_or(MdlError::Corrupt {
                    part: "mdl attachment bone",
                })?;
            let local = mat4_from_pose_to_bone(source.local).ok_or(MdlError::Corrupt {
                part: "mdl attachment matrix",
            })?;
            let model = mat4_mul(bone, local);
            if !matrix_is_finite(&model) {
                return Err(MdlError::Corrupt {
                    part: "mdl posed attachment matrix",
                });
            }
            Ok(ModelAttachment {
                source: source.clone(),
                model: std::array::from_fn(|row| std::array::from_fn(|column| model[column][row])),
            })
        })
        .collect::<Result<Vec<_>, MdlError>>()?;
    let vertices: Vec<ModelVertex> = vvd
        .vertices
        .iter()
        .enumerate()
        .map(|(index, vertex)| {
            let mut physical = *vertex;
            let mut tangent = vvd.tangents.get(index).copied();
            if matches!(
                options.vertex_compression,
                VertexCompression::SourceUbyte4 | VertexCompression::SourceHardware
            ) {
                physical.normal = source_ubyte4_vector(vertex.normal).unwrap_or(vertex.normal);
                tangent = tangent.map(|frame| source_ubyte4_tangent(frame).unwrap_or(frame));
            }
            let blend = (options.vertex_compression == VertexCompression::SourceHardware)
                .then(|| source_hardware_blend(&bone_transforms, &physical))
                .flatten();
            model_vertex(&bone_transforms, &physical, tangent, blend, stats)
        })
        .collect();
    let bounds = bounds_from_vertices(&vertices);
    Ok(PreparedModelGeometry {
        vertices,
        attachments,
        bounds,
    })
}

fn stored_quat_to_wxyz(quat: [f32; 4]) -> Quat {
    [quat[3], quat[0], quat[1], quat[2]]
}

fn rest_animation_local_overrides(
    mdl: &Mdl,
    bones: &[super::studio::MdlBone],
    stats: &mut MdlStats,
) -> Vec<Option<LocalBonePose>> {
    let mut overrides = vec![None; bones.len()];
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
        let (position, rotation) = rest_channel_pose(&bones[bone_index], animation, stats);
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

#[cfg(test)]
fn pose_to_bone_skinning_matrices(
    bones: &[super::studio::MdlBone],
    static_prop: bool,
    rest_overrides: &[Option<LocalBonePose>],
    stats: &mut MdlStats,
) -> Vec<Mat4> {
    let poses = if static_prop {
        Vec::new()
    } else {
        bone_pose_chains(bones, rest_overrides, stats)
    };
    skinning_matrices(bones, static_prop, &poses, stats)
}

fn bone_pose_chains(
    bones: &[super::studio::MdlBone],
    rest_overrides: &[Option<LocalBonePose>],
    stats: &mut MdlStats,
) -> Vec<Mat4> {
    let mut pose_chains: Vec<Mat4> = Vec::with_capacity(bones.len());
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
    }
    pose_chains
}

fn skinning_matrices(
    bones: &[super::studio::MdlBone],
    static_prop: bool,
    pose_chains: &[Mat4],
    stats: &mut MdlStats,
) -> Vec<Mat4> {
    if static_prop {
        return vec![MAT4_IDENTITY; bones.len()];
    }
    bones
        .iter()
        .zip(pose_chains)
        .map(|(bone, pose_chain)| {
            let pose_to_bone = mat4_from_pose_to_bone(bone.pose_to_bone).unwrap_or_else(|| {
                stats.note(AssemblySkip::NonFinitePoseToBone);
                MAT4_IDENTITY
            });
            let transform = mat4_mul(*pose_chain, pose_to_bone);
            if matrix_is_finite(&transform) {
                snap_near_identity(transform)
            } else {
                stats.note(AssemblySkip::NonFiniteSkinningMatrix);
                MAT4_IDENTITY
            }
        })
        .collect()
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

fn model_vertex(
    bone_transforms: &[Mat4],
    vertex: &VvdVertex,
    tangent: Option<[f32; 4]>,
    blend: Option<Mat4>,
    stats: &mut MdlStats,
) -> ModelVertex {
    let (position, normal) = blend.map_or_else(
        || skin_model_vertex(bone_transforms, vertex, stats),
        |blend| {
            (
                mat4_transform_point(blend, vertex.position),
                mat4_transform_direction(blend, vertex.normal),
            )
        },
    );
    let tangent_s = tangent
        .filter(|t| {
            t.iter().all(|x| x.is_finite()) && t[3] != 0.0 && t[..3].iter().any(|x| *x != 0.0)
        })
        .map_or_else(
            || {
                stats.note(AssemblySkip::VertexMissingTangent);
                [0.0; 3]
            },
            |t| {
                blend.map_or_else(
                    || skin_direction(bone_transforms, vertex, [t[0], t[1], t[2]]),
                    |blend| mat4_transform_direction(blend, [t[0], t[1], t[2]]),
                )
            },
        );
    let handedness = tangent.map_or(0.0, |t| if t[3].is_finite() { t[3] } else { 0.0 });
    let tangent_t = [
        (normal[1] * tangent_s[2] - normal[2] * tangent_s[1]) * handedness,
        (normal[2] * tangent_s[0] - normal[0] * tangent_s[2]) * handedness,
        (normal[0] * tangent_s[1] - normal[1] * tangent_s[0]) * handedness,
    ];
    ModelVertex {
        position,
        normal,
        tangent_s,
        tangent_t,
        tangent_sign: handedness,
        uv: vertex.uv,
    }
}

/// Mirror the compiled Source VS: decode two physical weights, reconstruct
/// the third, blend the three matrices, then transform all vertex channels.
fn source_hardware_blend(bones: &[Mat4], vertex: &VvdVertex) -> Option<Mat4> {
    if !(1..=3).contains(&vertex.bone_count)
        || !array_is_finite(vertex.position)
        || !array_is_finite(vertex.normal)
        || vertex.weights.iter().sum::<f32>() <= f32::EPSILON
    {
        return None;
    }
    let weights = source_short2_weights(vertex.weights)?;
    let mut matrices = [MAT4_IDENTITY; 3];
    for slot in 0..3 {
        if weights[slot] != 0.0 {
            matrices[slot] = *bones.get(usize::from(vertex.bones[slot]))?;
            if matrices[slot]
                .iter()
                .flatten()
                .any(|value| !value.is_finite())
            {
                return None;
            }
        }
    }
    Some(std::array::from_fn(|column| {
        std::array::from_fn(|row| {
            matrices[0][column][row] * weights[0]
                + matrices[1][column][row] * weights[1]
                + matrices[2][column][row] * weights[2]
        })
    }))
}

fn skin_direction(bone_transforms: &[Mat4], vertex: &VvdVertex, direction: [f32; 3]) -> [f32; 3] {
    let count = usize::from(vertex.bone_count);
    if count == 0 || count > 3 {
        return direction;
    }
    let mut result = [0.0; 3];
    let mut total = 0.0;
    for slot in 0..count {
        let weight = vertex.weights[slot];
        if !weight.is_finite() || weight < 0.0 {
            return direction;
        }
        if weight <= f32::EPSILON {
            continue;
        }
        let Some(transform) = bone_transforms
            .get(usize::from(vertex.bones[slot]))
            .copied()
        else {
            return direction;
        };
        let transformed = mat4_transform_direction(transform, direction);
        if !array_is_finite(transformed) {
            return direction;
        }
        for axis in 0..3 {
            result[axis] += transformed[axis] * weight;
        }
        total += weight;
    }
    if !total.is_finite() || total <= f32::EPSILON || !array_is_finite(result) {
        direction
    } else {
        result
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
        let skinned_normal = mat4_transform_direction(transform, vertex.normal);
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
    if !array_is_finite(position) || !array_is_finite(normal) {
        stats.note(AssemblySkip::VertexNonFiniteSkinning);
        return stored_vertex(vertex);
    }

    (position, normal)
}

fn stored_vertex(vertex: &VvdVertex) -> ([f32; 3], [f32; 3]) {
    (sanitize_vector(vertex.position), safe_normal(vertex.normal))
}

fn safe_normal(normal: [f32; 3]) -> [f32; 3] {
    if array_is_finite(normal) {
        normal
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

#[cfg(test)]
mod tangent_frame_tests {
    use super::*;

    #[test]
    fn linear_pose_arrays_drive_geometry_while_full_bones_remain_raw() {
        let mut mdl = super::tests::mdl_with_for_compression_test();
        let full = mdl.bones.clone();
        let half = std::f32::consts::FRAC_PI_4;
        let identity = [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
        ];
        let mut inverse = identity;
        inverse[0][3] = -2.0;
        mdl.linear_bones = Some(super::super::MdlLinearBones {
            flags: vec![0; 2],
            parents: vec![-1; 2],
            positions: vec![[10.0, 0.0, 0.0], [0.0; 3]],
            quaternions: vec![[0.0, 0.0, half.sin(), half.cos()], [0.0, 0.0, 0.0, 1.0]],
            rotations: vec![[0.0; 3]; 2],
            pose_to_bone: vec![inverse, identity],
            position_scales: vec![[0.0; 3]; 2],
            rotation_scales: vec![[0.0; 3]; 2],
            quaternion_alignment: vec![[0.0, 0.0, 0.0, 1.0]; 2],
        });
        let vvd = Vvd {
            checksum: 0,
            vertices: vec![VvdVertex {
                weights: [1.0, 0.0, 0.0],
                bones: [0; 3],
                bone_count: 1,
                position: [3.0, 0.0, 0.0],
                normal: [1.0, 0.0, 0.0],
                uv: [0.0; 2],
            }],
            tangents: vec![[0.0, 1.0, 0.0, -1.0]],
        };
        let geometry = prepare_model_geometry_with_options(
            &mdl,
            &vvd,
            false,
            &mut MdlStats::default(),
            Default::default(),
        )
        .unwrap();
        for (actual, expected) in geometry.vertices[0]
            .position
            .into_iter()
            .zip([10.0, 1.0, 0.0])
        {
            assert!((actual - expected).abs() < 1e-5);
        }
        for (actual, expected) in geometry.vertices[0].normal.into_iter().zip([0.0, 1.0, 0.0]) {
            assert!((actual - expected).abs() < 1e-5);
        }
        assert_eq!(mdl.bones, full);
        mdl.linear_bones.as_mut().unwrap().positions.pop();
        assert!(matches!(
            prepare_model_geometry_with_options(
                &mdl,
                &vvd,
                false,
                &mut MdlStats::default(),
                Default::default()
            ),
            Err(MdlError::Corrupt { .. })
        ));
    }

    #[test]
    fn source_hardware_stream_quantizes_weights_before_sharing_one_blended_frame() {
        let mut scale = MAT4_IDENTITY;
        for (axis, column) in scale.iter_mut().take(3).enumerate() {
            column[axis] = 2.0;
        }
        scale[3] = [10.0, 20.0, 30.0, 1.0];
        let mut rotated = MAT4_IDENTITY;
        rotated[0] = [0.0, 1.0, 0.0, 0.0];
        rotated[1] = [-1.0, 0.0, 0.0, 0.0];
        rotated[3] = [-10.0, -20.0, -30.0, 1.0];
        let vertex = VvdVertex {
            weights: [0.3, 0.4, 0.3],
            bones: [0, 1, 2],
            bone_count: 3,
            position: [3.0, -2.0, 4.0],
            normal: [1.0, 0.0, 0.0],
            uv: [0.25, 0.75],
        };
        let matrices = [MAT4_IDENTITY, scale, rotated];
        let blend = source_hardware_blend(&matrices, &vertex).unwrap();
        let first = 9830.0 / 32768.0;
        let second = 13108.0 / 32768.0;
        let third = first;
        let diagonal = first + 2.0 * second;
        let mut stats = MdlStats::default();
        let result = model_vertex(
            &matrices,
            &vertex,
            Some([0.0, 1.0, 0.0, -1.0]),
            Some(blend),
            &mut stats,
        );
        assert_eq!(result.normal, [diagonal, third, 0.0]);
        assert_eq!(result.tangent_s, [-third, diagonal, 0.0]);
        assert_eq!(result.tangent_sign, -1.0);
        assert_eq!(
            result.position,
            [
                3.0 * diagonal + 2.0 * third + 10.0 * (second - third),
                3.0 * third - 2.0 * diagonal + 20.0 * (second - third),
                4.0 * (1.0 + second) + 30.0 * (second - third)
            ]
        );
        assert_eq!(vertex.weights, [0.3, 0.4, 0.3]);
        assert_ne!(
            result.normal,
            skin_direction(&matrices, &vertex, vertex.normal)
        );
        assert_eq!(result.uv, vertex.uv);
    }

    #[test]
    fn source_hardware_blends_matrices_before_large_origin_position_arithmetic() {
        let matrix = |rotation, position| {
            local_pose_matrix(LocalBonePose {
                position,
                rotation: stored_quat_to_wxyz(rotation),
            })
        };
        let a = matrix(
            [0.0, 0.0, 0.14943813, 0.9887711],
            [-2000.17, 3000.8, -100.3],
        );
        let b = matrix(
            [0.0, 0.0, 0.7512804, 0.65998316],
            [-1500.95, 3300.45, -700.33],
        );
        let c = matrix(
            [0.0, 0.43496552, 0.0, 0.90044713],
            [-3200.1, 2000.33, 500.88],
        );
        let vertex = VvdVertex {
            weights: [0.25, 0.5, 0.25],
            bones: [0, 1, 2],
            bone_count: 3,
            position: [105.7, 33.9, -150.33],
            normal: [0.0, 0.0, 1.0],
            uv: [0.0; 2],
        };
        let matrices = [a, b, c];
        let actual = mat4_transform_point(
            source_hardware_blend(&matrices, &vertex).unwrap(),
            vertex.position,
        );
        let previous = skin_model_vertex(&matrices, &vertex, &mut MdlStats::default()).0;
        assert_ne!(
            actual, previous,
            "post-transform blending loses the Source arithmetic order"
        );
        assert!(
            actual
                .into_iter()
                .zip(previous)
                .all(|(a, b)| (a - b).abs() < 0.001)
        );
    }

    #[test]
    fn physical_stream_compression_precedes_nonidentity_bone_blending() {
        let mut mdl = super::tests::mdl_with_for_compression_test();
        mdl.illumination_position = [1.0, 2.0, 3.0];
        let original = VvdVertex {
            weights: [0.25, 0.75, 0.0],
            bones: [0, 1, 0],
            bone_count: 2,
            position: [0.0; 3],
            normal: [0.2, 0.3, 0.5],
            uv: [0.0; 2],
        };
        let vvd = Vvd {
            checksum: 0,
            vertices: vec![original],
            tangents: vec![[0.2, 0.3, 0.5, -1.0]],
        };
        let options = ModelAssemblyOptions {
            vertex_compression: VertexCompression::SourceUbyte4,
        };
        let result = prepare_model_geometry_with_options(
            &mdl,
            &vvd,
            false,
            &mut MdlStats::default(),
            options,
        )
        .unwrap();
        let encoded = source_ubyte4_vector(original.normal).unwrap();
        let expected = [
            0.25 * encoded[0] - 0.75 * encoded[1],
            0.25 * encoded[1] + 0.75 * encoded[0],
            encoded[2],
        ];
        for (actual, expected) in result.vertices[0].normal.into_iter().zip(expected) {
            assert!((actual - expected).abs() < 1e-5);
        }
        assert_eq!(vvd.vertices[0].normal, original.normal);
        assert_eq!(vvd.tangents[0], [0.2, 0.3, 0.5, -1.0]);
        assert_eq!(result.vertices[0].tangent_sign, -1.0);
    }

    #[test]
    fn skinned_normal_and_tangent_share_linear_bone_blend_and_authored_handedness() {
        let a = MAT4_IDENTITY;
        let mut b = MAT4_IDENTITY;
        b[0][0] = 0.0;
        b[0][1] = 1.0;
        b[1][0] = -1.0;
        b[1][1] = 0.0;
        let vertex = VvdVertex {
            weights: [0.25, 0.75, 0.0],
            bones: [0, 1, 0],
            bone_count: 2,
            position: [0.0; 3],
            normal: [1.0, 0.0, 0.0],
            uv: [0.0; 2],
        };
        let mut stats = MdlStats::default();
        let result = model_vertex(
            &[a, b],
            &vertex,
            Some([0.0, 1.0, 0.0, -1.0]),
            None,
            &mut stats,
        );
        assert_eq!(result.normal, [0.25, 0.75, 0.0]);
        assert_eq!(result.tangent_s, [-0.75, 0.25, 0.0]);
        assert_eq!(result.tangent_t, [0.0, 0.0, -0.625]);
        assert_eq!(result.tangent_sign, -1.0);
        assert!(stats.sanitized.is_empty());
    }

    #[test]
    fn missing_vvd_tangent_has_zero_frame_and_explicit_degradation() {
        let vertex = VvdVertex {
            weights: [1.0, 0.0, 0.0],
            bones: [0; 3],
            bone_count: 1,
            position: [0.0; 3],
            normal: [0.0, 0.0, 1.0],
            uv: [0.0; 2],
        };
        let mut stats = MdlStats::default();
        let result = model_vertex(&[MAT4_IDENTITY], &vertex, None, None, &mut stats);
        assert_eq!(result.tangent_s, [0.0; 3]);
        assert_eq!(result.tangent_t, [0.0; 3]);
        assert_eq!(result.tangent_sign, 0.0);
        assert!(
            stats
                .sanitized
                .contains_key(&AssemblySkip::VertexMissingTangent)
        );
    }

    #[test]
    fn authored_handedness_survives_a_degenerate_skinned_frame() {
        let vertex = VvdVertex {
            weights: [1.0, 0.0, 0.0],
            bones: [0; 3],
            bone_count: 1,
            position: [0.0; 3],
            normal: [1.0, 0.0, 0.0],
            uv: [0.0; 2],
        };
        let result = model_vertex(
            &[MAT4_IDENTITY],
            &vertex,
            Some([1.0, 0.0, 0.0, -1.0]),
            None,
            &mut MdlStats::default(),
        );
        assert_eq!(result.tangent_t, [0.0; 3]);
        assert_eq!(result.tangent_sign, -1.0);
    }
}
