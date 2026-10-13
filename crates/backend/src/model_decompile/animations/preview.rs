use std::{collections::BTreeMap, sync::atomic::AtomicBool};

use mdlc::bone_math::{self, Matrix3x4};
use vformats::mdl::{Mdl, VvdVertex};

use super::{DecompileError, MAX_POSES, Reader, decode, invalid};

#[path = "preview_blend.rs"]
mod pose_blend;
pub use pose_blend::PreviewPoseParameter;

#[derive(Clone, Debug, PartialEq)]
pub struct PreviewPose {
    pub sequence: usize,
    pub cycle: f32,
    pub parameters: Vec<f32>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PreviewSequence {
    pub name: String,
    pub duration: f32,
    pub looping: bool,
    pub unavailable: Option<String>,
    pub parameters: Vec<usize>,
    cells: Vec<usize>,
    grid: [usize; 2],
    axes: [Option<pose_blend::Axis>; 2],
    weights: Vec<f32>,
}

#[derive(Clone, Debug, PartialEq)]
struct Pose {
    position: [f32; 3],
    rotation: [f32; 4],
}

#[derive(Clone, Debug, PartialEq)]
pub struct ModelAnimationPreview {
    pub sequences: Vec<PreviewSequence>,
    pub parameters: Vec<PreviewPoseParameter>,
    rates: Vec<f32>,
    bones: Vec<vformats::mdl::MdlBone>,
    order: Vec<usize>,
    alignments: Vec<Option<[f32; 4]>>,
    animations: Vec<Option<Vec<Vec<Pose>>>>,
    root_lod: usize,
    meshes: Vec<Vec<Vec<VvdVertex>>>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AnimatedVertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
}

impl ModelAnimationPreview {
    pub fn evaluate(&self, sequence: usize, cycle: f32) -> Option<Vec<Vec<AnimatedVertex>>> {
        self.evaluate_with_parameters(sequence, cycle, &[])
    }

    pub fn duration(&self, index: usize, values: &[f32]) -> Option<f32> {
        let sequence = self.sequences.get(index)?;
        if sequence.unavailable.is_some() {
            return None;
        }
        let ([x, y], [s, t]) = self.blend_setting(sequence, values)?;
        let mut rate = 0.0;
        for (dx, dy, weight) in [
            (0, 0, (1.0 - s) * (1.0 - t)),
            (1, 0, s * (1.0 - t)),
            (0, 1, (1.0 - s) * t),
            (1, 1, s * t),
        ] {
            if weight > 0.0 {
                let cell = sequence.cells[((y + dy).min(sequence.grid[1] - 1)) * sequence.grid[0]
                    + (x + dx).min(sequence.grid[0] - 1)];
                rate += self.rates[cell] * weight;
            }
        }
        let duration = if rate == 0.0 { 0.0 } else { 1.0 / rate };
        duration.is_finite().then_some(duration)
    }

    fn blend_setting(
        &self,
        sequence: &PreviewSequence,
        values: &[f32],
    ) -> Option<([usize; 2], [f32; 2])> {
        let mut index = [0; 2];
        let mut fraction = [0.0; 2];
        for axis in 0..2 {
            if let Some(setting) = &sequence.axes[axis] {
                (index[axis], fraction[axis]) = setting.sample(&self.parameters, values)?;
            }
        }
        Some((index, fraction))
    }

    fn sample(&self, animation: usize, cycle: f32) -> Option<Vec<Pose>> {
        let frames = self.animations.get(animation)?.as_ref()?;
        let frame = cycle * (frames.len() - 1) as f32;
        let first = frame.floor() as usize;
        let second = (first + 1).min(frames.len() - 1);
        let fraction = frame - first as f32;
        Some(
            frames[first]
                .iter()
                .zip(&frames[second])
                .enumerate()
                .map(|(bone, (a, b))| {
                    let mut rotation = blend(a.rotation, b.rotation, fraction);
                    if self.alignments[bone].is_some_and(|alignment| dot(alignment, rotation) < 0.0)
                    {
                        rotation = rotation.map(|value| -value);
                    }
                    Pose {
                        position: std::array::from_fn(|axis| {
                            a.position[axis] + (b.position[axis] - a.position[axis]) * fraction
                        }),
                        rotation,
                    }
                })
                .collect(),
        )
    }

    pub fn evaluate_with_parameters(
        &self,
        sequence: usize,
        cycle: f32,
        values: &[f32],
    ) -> Option<Vec<Vec<AnimatedVertex>>> {
        self.evaluate_lod(sequence, cycle, values, self.root_lod)
    }

    pub fn evaluate_lod(
        &self,
        sequence: usize,
        cycle: f32,
        values: &[f32],
        lod: usize,
    ) -> Option<Vec<Vec<AnimatedVertex>>> {
        let meshes = self.meshes.get(lod.checked_sub(self.root_lod)?)?;
        if !cycle.is_finite() || values.iter().any(|value| !value.is_finite()) {
            return None;
        }
        let sequence = self.sequences.get(sequence)?;
        if sequence.unavailable.is_some() {
            return None;
        }
        let cycle = if sequence.looping {
            cycle.rem_euclid(1.0)
        } else {
            cycle.clamp(0.0, 1.0)
        };
        let (index, [s, t]) = self.blend_setting(sequence, values)?;
        let corner = |x: usize, y: usize| {
            sequence.cells
                [y.min(sequence.grid[1] - 1) * sequence.grid[0] + x.min(sequence.grid[0] - 1)]
        };
        let [x, y] = index;
        let corners: Vec<(usize, f32)> = if !(0.001..=0.999).contains(&s) {
            let x = x + usize::from(s > 0.999);
            if !(0.001..=0.999).contains(&t) {
                vec![(corner(x, y + usize::from(t > 0.999)), 1.0)]
            } else {
                vec![(corner(x, y), 1.0 - t), (corner(x, y + 1), t)]
            }
        } else if !(0.001..=0.999).contains(&t) {
            let y = y + usize::from(t > 0.999);
            vec![(corner(x, y), 1.0 - s), (corner(x + 1, y), s)]
        } else {
            pose_blend::triangle(index, [s, t])
                .into_iter()
                .filter(|(_, weight)| *weight > 0.0)
                .map(|([x, y], weight)| (corner(x, y), weight))
                .collect()
        };
        let (first, mut total) = *corners.first()?;
        let mut poses = self.sample(first, cycle)?;
        for &(animation, weight) in &corners[1..] {
            let other = self.sample(animation, cycle)?;
            let fraction = weight / (total + weight);
            for (bone, (pose, other)) in poses.iter_mut().zip(other).enumerate() {
                pose.position = std::array::from_fn(|axis| {
                    pose.position[axis] * (1.0 - fraction) + other.position[axis] * fraction
                });
                pose.rotation = blend_aligned(
                    pose.rotation,
                    other.rotation,
                    fraction,
                    self.alignments[bone].is_none(),
                );
            }
            total += weight;
        }
        let mut world = vec![bone_math::IDENTITY; self.bones.len()];
        let mut skin = world.clone();
        for &index in &self.order {
            let bone = &self.bones[index];
            let pose = &poses[index];
            let weight = sequence.weights[index];
            let position = std::array::from_fn(|axis| {
                let sampled = pose.position[axis];
                bone.position[axis] + (sampled - bone.position[axis]) * weight
            });
            let mut sampled = pose.rotation;
            if self.alignments[index].is_some_and(|alignment| dot(alignment, sampled) < 0.0) {
                sampled = sampled.map(|value| -value);
            }
            let rotation = if weight <= 0.0 {
                bone.quaternion
            } else {
                slerp(
                    sampled,
                    bone.quaternion,
                    1.0 - weight,
                    self.alignments[index].is_none(),
                )
            };
            let local = bone_math::quaternion_local_transform(rotation, position);
            world[index] = if bone.parent < 0 {
                local
            } else {
                bone_math::concat(&world[bone.parent as usize], &local)
            };
            let inverse_bind: Matrix3x4 = std::array::from_fn(|i| bone.pose_to_bone[i / 4][i % 4]);
            skin[index] = bone_math::concat(&world[index], &inverse_bind);
        }
        if skin.iter().flatten().any(|value| !value.is_finite()) {
            return None;
        }
        meshes
            .iter()
            .map(|mesh| {
                mesh.iter()
                    .map(|vertex| skin_vertex(vertex, &skin))
                    .collect()
            })
            .collect()
    }

    pub fn mesh_vertex_counts(&self) -> impl Iterator<Item = usize> + '_ {
        self.mesh_vertex_counts_lod(self.root_lod)
    }

    pub fn mesh_vertex_counts_lod(&self, lod: usize) -> impl Iterator<Item = usize> + '_ {
        lod.checked_sub(self.root_lod)
            .and_then(|index| self.meshes.get(index))
            .into_iter()
            .flatten()
            .map(Vec::len)
    }
}

fn blend(a: [f32; 4], b: [f32; 4], fraction: f32) -> [f32; 4] {
    blend_aligned(a, b, fraction, true)
}

fn blend_aligned(a: [f32; 4], mut b: [f32; 4], fraction: f32, align: bool) -> [f32; 4] {
    if align && dot(a, b) < 0.0 {
        b = b.map(|value| -value);
    }
    bone_math::quaternion_normalize(std::array::from_fn(|axis| {
        a[axis] + (b[axis] - a[axis]) * fraction
    }))
}

fn slerp(a: [f32; 4], mut b: [f32; 4], fraction: f32, align: bool) -> [f32; 4] {
    let mut cosine = dot(a, b);
    if align && cosine < 0.0 {
        b = b.map(|value| -value);
        cosine = -cosine;
    }
    if 1.0 + cosine <= 0.000001 {
        let mut result = [-b[1], b[0], -b[3], b[2]];
        let left = ((1.0 - fraction) * std::f32::consts::FRAC_PI_2).sin();
        let right = (fraction * std::f32::consts::FRAC_PI_2).sin();
        for axis in 0..3 {
            result[axis] = a[axis] * left + result[axis] * right;
        }
        return result;
    }
    let (left, right) = if 1.0 - cosine > 0.000001 {
        let angle = cosine.clamp(-1.0, 1.0).acos();
        (
            ((1.0 - fraction) * angle).sin() / angle.sin(),
            (fraction * angle).sin() / angle.sin(),
        )
    } else {
        (1.0 - fraction, fraction)
    };
    std::array::from_fn(|axis| a[axis] * left + b[axis] * right)
}

fn dot(a: [f32; 4], b: [f32; 4]) -> f32 {
    a.into_iter().zip(b).map(|(a, b)| a * b).sum()
}

fn skin_vertex(vertex: &VvdVertex, matrices: &[Matrix3x4]) -> Option<AnimatedVertex> {
    let mut position = [0.0; 3];
    let mut normal = [0.0; 3];
    for slot in 0..usize::from(vertex.bone_count) {
        let matrix = matrices.get(usize::from(vertex.bones[slot]))?;
        let weight = vertex.weights[slot];
        for axis in 0..3 {
            let row = axis * 4;
            let rotate =
                |v: [f32; 3]| matrix[row] * v[0] + matrix[row + 1] * v[1] + matrix[row + 2] * v[2];
            position[axis] += (rotate(vertex.position) + matrix[row + 3]) * weight;
            normal[axis] += rotate(vertex.normal) * weight;
        }
    }
    let length = normal.iter().map(|value| value * value).sum::<f32>().sqrt();
    if position.iter().any(|v| !v.is_finite()) || !length.is_finite() || length <= f32::EPSILON {
        return None;
    }
    Some(AnimatedVertex {
        position,
        normal: normal.map(|value| value / length),
    })
}

pub fn load_animation_preview(
    raw: &[u8],
    vvd: &[u8],
    vtx: &[u8],
    ani: Option<&[u8]>,
) -> Result<ModelAnimationPreview, DecompileError> {
    if raw.len() > 128 * 1024 * 1024 || ani.is_some_and(|ani| ani.len() > 128 * 1024 * 1024) {
        return Err(DecompileError::SizeLimit);
    }
    let mut geometry_header = raw.to_vec();
    geometry_header
        .get_mut(180..184)
        .ok_or_else(|| invalid("Truncated MDL header"))?
        .fill(0);
    let limits = vformats::Limits::default();
    let model = vformats::mdl::parse_mdl(&geometry_header, &limits)
        .map_err(|error| invalid(error.to_string()))?;
    let raw_vvd = vvd;
    let raw_vtx = vtx;
    let vvd = vformats::mdl::parse_vvd(vvd, &limits).map_err(|error| invalid(error.to_string()))?;
    let vtx = vformats::mdl::parse_vtx(vtx, &limits).map_err(|error| invalid(error.to_string()))?;
    if model.checksum != vvd.checksum || model.checksum != vtx.checksum {
        return Err(invalid(
            "Animation preview companion checksums do not match",
        ));
    }
    if model.bones.len() > 256 || model.bones.is_empty() {
        return Err(invalid("Animation preview requires 1–256 bones"));
    }
    let order = bone_order(&model)?;
    let reader = Reader(raw);
    if let Some(ani) = ani {
        let data = Reader(ani);
        if ani.get(..4) != Some(&mdlc::ani_writer::ANI_ID.to_le_bytes())
            || !matches!(data.i32(4)?, 48 | 49)
            || data.count(76)? != ani.len()
            || ![0, model.checksum].contains(&(data.i32(8)? as u32))
        {
            return Err(invalid("The ANI file does not match the MDL"));
        }
    }
    let alignments = reader
        .table(156, 160, 216)?
        .into_iter()
        .map(|record| {
            if reader.i32(record + 160)? & 0x0010_0000 == 0 {
                return Ok(None);
            }
            Ok(Some([
                reader.f32(record + 144)?,
                reader.f32(record + 148)?,
                reader.f32(record + 152)?,
                reader.f32(record + 156)?,
            ]))
        })
        .collect::<Result<Vec<_>, DecompileError>>()?;
    let records = reader.table(180, 184, 100)?;
    let sequence_records = reader.table(188, 192, 212)?;
    if records.len() > 4096 || sequence_records.len() > 4096 {
        return Err(DecompileError::SizeLimit);
    }
    let parameters = pose_blend::parameters(&reader)?;
    let mut rates = vec![0.0; records.len()];
    let mut animations = vec![None; records.len()];
    let mut sequences = Vec::new();
    let mut poses = 0usize;
    for record in sequence_records {
        let name = reader.string(record, record + 4)?.to_owned();
        let flags = reader.i32(record + 12)?;
        let looping = flags & 1 != 0;
        let mut sequence = PreviewSequence {
            name,
            duration: 0.0,
            looping,
            unavailable: None,
            parameters: Vec::new(),
            cells: Vec::new(),
            grid: [1, 1],
            axes: [None, None],
            weights: Vec::new(),
        };
        let result = (|| {
            if model.flags & vformats::mdl::FLAG_STATIC_PROP != 0 {
                return Err(invalid("Static props do not have skeletal playback"));
            }
            let world_flag = if model.version <= 48 { 0x4000 } else { 0x2000 };
            if flags & (4 | 16 | 0x800 | world_flag) != 0 {
                return Err(invalid(
                    "Delta, post, world-space and declared sequences require layered playback",
                ));
            }
            let count = reader.count(record + 56)?;
            let grid = [reader.count(record + 68)?, reader.count(record + 72)?];
            if count == 0
                || count > 4096
                || grid.contains(&0)
                || grid[0].checked_mul(grid[1]) != Some(count)
            {
                return Err(invalid("Invalid sequence blend grid"));
            }
            sequence.grid = grid;
            for axis in 0..2 {
                sequence.axes[axis] = pose_blend::axis(&reader, record, axis, grid, &parameters)?;
                if let Some(axis) = &sequence.axes[axis]
                    && !sequence.parameters.contains(&axis.parameter)
                {
                    sequence.parameters.push(axis.parameter);
                }
            }
            let cycle_pose = if model.version <= 48 { 0x0080 } else { 0x0200 };
            if flags & cycle_pose != 0 {
                return Err(invalid(
                    "Cycle-pose sequences require runtime cycle evaluation",
                ));
            }
            if reader.count(record + 148)? != 0 || reader.count(record + 164)? != 0 {
                return Err(invalid(
                    "Sequence layers or IK locks require runtime evaluation",
                ));
            }
            if reader.count(320)? != 0 {
                return Err(invalid("Autoplay IK locks require runtime evaluation"));
            }
            if reader.count(164)? != 0 {
                return Err(invalid("Bone controllers require runtime evaluation"));
            }
            let bones = reader.table(156, 160, 216)?;
            for bone in bones {
                if reader.i32(bone + 164)? != 0 {
                    return Err(invalid("Procedural bones require runtime evaluation"));
                }
            }
            let table = reader.relative(record, record + 60)?;
            let weights = reader.relative(record, record + 156)?;
            for bone in 0..model.bones.len() {
                let weight = reader.f32(weights + bone * 4)?;
                if !(0.0..=1.0).contains(&weight) {
                    return Err(invalid("Invalid sequence bone weight"));
                }
                sequence.weights.push(weight);
            }
            for cell in 0..sequence.grid[0] * sequence.grid[1] {
                let index = usize::try_from(reader.i16(table + cell * 2)?)
                    .map_err(|_| invalid("Invalid sequence animation index"))?;
                let animation_record = *records
                    .get(index)
                    .ok_or_else(|| invalid("Sequence references an absent animation"))?;
                if reader.i32(animation_record + 12)? & (4 | 16) != 0 {
                    return Err(invalid(
                        "Delta and post animations require layered playback",
                    ));
                }
                if reader.count(animation_record + 60)? != 0
                    || reader.count(animation_record + 72)? != 0
                {
                    return Err(invalid(
                        "Animation IK or hierarchy changes require runtime evaluation",
                    ));
                }
                let count = reader.count(animation_record + 16)?;
                let fps = reader.f32(animation_record + 8)?;
                if count == 0 || fps <= 0.0 {
                    return Err(invalid("Invalid animation timing"));
                }
                rates[index] = if count <= 1 {
                    0.0
                } else {
                    fps / (count - 1) as f32
                };
                if !rates[index].is_finite() || (count > 1 && !(1.0 / rates[index]).is_finite()) {
                    return Err(invalid("Animation duration exceeds the preview range"));
                }
                if animations[index].is_none() {
                    poses = poses
                        .checked_add(
                            count
                                .checked_mul(model.bones.len())
                                .ok_or(DecompileError::SizeLimit)?,
                        )
                        .filter(|count| *count <= MAX_POSES)
                        .ok_or(DecompileError::SizeLimit)?;
                    let decoded = decode::animation(
                        &reader,
                        ani,
                        &model,
                        animation_record,
                        &AtomicBool::new(false),
                    )?;
                    let root_rotation =
                        bone_math::angle_quaternion([0.0, 0.0, std::f32::consts::FRAC_PI_2]);
                    let frames = decoded
                        .frames
                        .into_iter()
                        .map(|frame| {
                            frame
                                .into_iter()
                                .zip(&model.bones)
                                .map(|(pose, bone)| {
                                    let mut rotation = bone_math::angle_quaternion(pose.rotation);
                                    let mut position = pose.position;
                                    if bone.parent < 0 {
                                        position = [-position[1], position[0], position[2]];
                                        rotation =
                                            bone_math::quaternion_mult(root_rotation, rotation);
                                    }
                                    Pose { position, rotation }
                                })
                                .collect()
                        })
                        .collect();
                    animations[index] = Some(frames);
                }
                sequence.cells.push(index);
            }

            Ok(())
        })();
        if let Err(error) = result {
            sequence.unavailable = Some(error.to_string());
        }
        sequences.push(sequence);
    }
    let mdl_meshes: Vec<_> = model
        .body_parts
        .iter()
        .flat_map(|part| &part.models)
        .flat_map(|model| {
            model
                .meshes
                .iter()
                .map(move |mesh| (model.vertex_offset, mesh))
        })
        .collect();
    let metadata = super::super::lods::Metadata::read(raw, raw_vvd, raw_vtx, &model, &vvd, &vtx)?;
    let root_lod = usize::from(raw[0x179]);
    let mut all_meshes = Vec::new();
    let mut vertices = 0usize;
    for level in root_lod..metadata.switch_points.len() {
        let vtx_meshes: Vec<_> = vtx
            .body_parts
            .iter()
            .flat_map(|part| &part.models)
            .flat_map(|model| model.lods.get(level))
            .flat_map(|lod| &lod.meshes)
            .collect();
        if mdl_meshes.len() != vtx_meshes.len() {
            return Err(invalid("Animation preview mesh tables do not match"));
        }
        let mut meshes = Vec::new();
        for ((model_offset, mesh), vtx_mesh) in mdl_meshes.iter().copied().zip(vtx_meshes) {
            let base = usize::try_from(i64::from(model_offset) + i64::from(mesh.vertex_offset))
                .map_err(|_| invalid("Invalid mesh vertex offset"))?;
            let mut seen = BTreeMap::new();
            let mut binding = Vec::new();
            for group in &vtx_mesh.strip_groups {
                for strip in &group.strips {
                    for position in strip.triangle_index_positions() {
                        let vertex = group
                            .indices
                            .get(position)
                            .and_then(|index| group.vertices.get(usize::from(*index)))
                            .ok_or_else(|| invalid("Invalid strip vertex"))?;
                        let source = base
                            .checked_add(usize::from(vertex.original_mesh_vertex_id))
                            .ok_or(DecompileError::SizeLimit)?;
                        if seen.insert(source, ()).is_none() {
                            let vertex = vvd
                                .vertices
                                .get(source)
                                .ok_or_else(|| invalid("Invalid VVD vertex"))?;
                            if vertex.bone_count == 0
                                || vertex.bone_count > 3
                                || !vertex
                                    .position
                                    .iter()
                                    .chain(&vertex.normal)
                                    .all(|value| value.is_finite())
                            {
                                return Err(invalid("Invalid animated vertex"));
                            }
                            for slot in 0..usize::from(vertex.bone_count) {
                                if usize::from(vertex.bones[slot]) >= model.bones.len()
                                    || !vertex.weights[slot].is_finite()
                                    || vertex.weights[slot] < 0.0
                                {
                                    return Err(invalid("Invalid animated vertex weight"));
                                }
                            }
                            let sum = vertex.weights[..usize::from(vertex.bone_count)]
                                .iter()
                                .sum::<f32>();
                            if sum <= 0.0
                                || !sum.is_finite()
                                || vertex.normal.iter().map(|value| value * value).sum::<f32>()
                                    <= f32::EPSILON
                            {
                                return Err(invalid(
                                    "Animated vertex has no usable weights or normal",
                                ));
                            }
                            vertices += 1;
                            if vertices > 2_000_000 {
                                return Err(DecompileError::SizeLimit);
                            }
                            binding.push(*vertex);
                        }
                    }
                }
            }
            meshes.push(binding);
        }
        all_meshes.push(meshes);
    }
    let mut preview = ModelAnimationPreview {
        sequences,
        parameters,
        rates,
        bones: model.bones,
        order,
        alignments,
        animations,
        root_lod,
        meshes: all_meshes,
    };
    for index in 0..preview.sequences.len() {
        if let Some(duration) = preview.duration(index, &[]) {
            preview.sequences[index].duration = duration;
        }
    }
    Ok(preview)
}

fn bone_order(model: &Mdl) -> Result<Vec<usize>, DecompileError> {
    let mut order = Vec::new();
    let mut states = vec![0; model.bones.len()];
    fn visit(
        index: usize,
        model: &Mdl,
        states: &mut [u8],
        order: &mut Vec<usize>,
    ) -> Result<(), DecompileError> {
        if states[index] == 2 {
            return Ok(());
        }
        if states[index] == 1 {
            return Err(invalid("Cyclic bone hierarchy"));
        }
        states[index] = 1;
        let bone = &model.bones[index];
        if bone.parent < -1
            || !bone
                .position
                .iter()
                .chain(&bone.quaternion)
                .chain(bone.pose_to_bone.iter().flatten())
                .all(|value| value.is_finite())
            || dot(bone.quaternion, bone.quaternion) < 0.5
        {
            return Err(invalid("Invalid animation bind transform"));
        }
        if bone.parent >= 0 {
            let parent = usize::try_from(bone.parent)
                .ok()
                .filter(|parent| *parent < model.bones.len())
                .ok_or_else(|| invalid("Invalid bone parent"))?;
            visit(parent, model, states, order)?;
        }
        states[index] = 2;
        order.push(index);
        Ok(())
    }
    for index in 0..model.bones.len() {
        visit(index, model, &mut states, &mut order)?;
    }
    Ok(order)
}

#[cfg(test)]
#[path = "preview_tests.rs"]
mod tests;
