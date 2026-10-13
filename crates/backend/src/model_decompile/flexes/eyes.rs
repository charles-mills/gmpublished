use std::fmt::Write as _;

use mdlc::bone_math;
use vformats::mdl::Mdl;

use super::{DecompileError, Reader, Shape, invalid, quoted};

pub(super) struct Eyes {
    options: String,
    lids: Vec<Lid>,
}

struct Lid {
    eye: String,
    descriptor: usize,
    targets: [f32; 3],
}

impl Eyes {
    pub(super) fn read(
        reader: &Reader<'_>,
        record: usize,
        model: &Mdl,
        descriptors: &[String],
    ) -> Result<Self, DecompileError> {
        let eyes = reader.relative_table(record, 100, 104, 172)?;
        let meshes = reader.relative_table(record, 72, 76, 116)?;
        let bones = reader.table(156, 160, 216)?;
        let mut result = Self {
            options: String::new(),
            lids: Vec::new(),
        };
        for (index, eye) in eyes.into_iter().enumerate() {
            let name = format!("eye_{index}");
            let bone = reader.index(eye + 4, model.bones.len())?;
            let mut pose_to_bone = [0.0; 12];
            for (index, value) in pose_to_bone.iter_mut().enumerate() {
                *value = reader.float(bones[bone] + 96 + index * 4)?;
            }
            let up = vector(reader, eye + 28)?;
            let forward = vector(reader, eye + 40)?;
            let expected_up = normalize([pose_to_bone[2], pose_to_bone[6], pose_to_bone[10]])?;
            let expected_forward =
                normalize([-pose_to_bone[1], -pose_to_bone[5], -pose_to_bone[9]])?;
            if (0..3).any(|axis| {
                (up[axis] - expected_up[axis]).abs() > 0.001
                    || (forward[axis] - expected_forward[axis]).abs() > 0.001
            }) {
                return Err(invalid(
                    "An eyeball uses an orientation that cannot be represented by standard eyeball QC",
                ));
            }
            let world = bone_math::invert(&pose_to_bone);
            let local = vector(reader, eye + 8)?;
            let origin = std::array::from_fn::<_, 3, _>(|axis| {
                world[axis * 4] * local[0]
                    + world[axis * 4 + 1] * local[1]
                    + world[axis * 4 + 2] * local[2]
                    + world[axis * 4 + 3]
            });
            let matching = meshes
                .iter()
                .copied()
                .filter(|mesh| {
                    reader.signed(mesh + 24).ok() == Some(1)
                        && reader.signed(mesh + 28).ok() == Some(index as i32)
                })
                .collect::<Vec<_>>();
            if matching.len() != 1 {
                return Err(invalid(
                    "An eyeball does not reference exactly one material mesh",
                ));
            }
            let material = reader.index(matching[0], model.textures.len())?;
            let iris = reader.float(eye + 60)?;
            let radius = reader.float(eye + 24)?;
            if radius <= 0.0
                || iris <= 0.0
                || reader.byte(eye + 140)? != 0
                || reader.signed(eye + 52)? != 0
            {
                return Err(invalid(
                    "An eyeball uses unsupported dimensions, texture metadata or non-FACS controls",
                ));
            }
            let zangle = reader.float(eye + 20)?.atan().to_degrees();
            writeln!(
                result.options,
                "    eyeball {} {} {} {} {} {} {} {zangle} {} {}",
                quoted(&name)?,
                quoted(&model.bones[bone].name)?,
                origin[0],
                origin[1],
                origin[2],
                quoted(&model.textures[material])?,
                radius * 2.0,
                quoted(&model.textures[material])?,
                1.0 / iris
            )
            .unwrap();
            for (controls, targets, descriptor, prefix) in
                [(68, 92, 116, b'u'), (80, 104, 120, b'l')]
            {
                let descriptor = reader.index(eye + descriptor, descriptors.len())?;
                let indices = [
                    reader.index(eye + controls, descriptors.len())?,
                    reader.index(eye + controls + 4, descriptors.len())?,
                    reader.index(eye + controls + 8, descriptors.len())?,
                ];
                let targets = vector(reader, eye + targets)?;
                if descriptors[descriptor] == "dummy_eyelid"
                    && indices == [descriptor; 3]
                    && targets == [-1.0, 0.0, 1.0]
                {
                    continue;
                }
                if descriptors[descriptor].as_bytes().first() != Some(&prefix)
                    || indices.iter().zip(["lowerer", "neutral", "raiser"]).any(
                        |(index, suffix)| {
                            descriptors[*index] != format!("{}_{}", descriptors[descriptor], suffix)
                        },
                    )
                    || targets.iter().any(|target| target.abs() > radius)
                {
                    return Err(invalid(
                        "An eyeball eyelid has control links that cannot be represented by standard eyelid QC",
                    ));
                }
                result.lids.push(Lid {
                    eye: name.clone(),
                    descriptor,
                    targets,
                });
            }
        }
        Ok(result)
    }

    pub(super) fn is_empty(&self) -> bool {
        self.options.is_empty()
    }

    pub(super) fn apply(
        self,
        mut block: String,
        filename: &str,
        shapes: &[Shape],
        descriptors: &[String],
    ) -> Result<String, DecompileError> {
        let mut bound = String::new();
        let mut templates = std::collections::HashMap::new();
        for lid in self.lids {
            let name = &descriptors[lid.descriptor];
            let key = format!("    eyelid {} ", quoted(name)?);
            let line = block
                .lines()
                .find(|line| line.starts_with(&key))
                .map(str::to_owned);
            let template = if let Some(line) = line {
                let windows = [
                    [-11.0, -10.0, lid.targets[0], lid.targets[1]],
                    [
                        lid.targets[0],
                        lid.targets[1],
                        lid.targets[1],
                        lid.targets[2],
                    ],
                    [lid.targets[1], lid.targets[2], 10.0, 11.0],
                ];
                if shapes
                    .iter()
                    .filter(|shape| shape.descriptor == lid.descriptor)
                    .any(|shape| !windows.contains(&shape.targets))
                {
                    return Err(invalid(
                        "An eyeball eyelid targets disagree with its vertex animation windows",
                    ));
                }
                block = block
                    .lines()
                    .filter(|candidate| !candidate.starts_with(&key))
                    .map(|line| format!("{line}\n"))
                    .collect();
                templates.insert(lid.descriptor, lid.targets);
                line
            } else {
                if let Some(targets) = templates.get(&lid.descriptor)
                    && *targets != lid.targets
                {
                    return Err(invalid(
                        "Two eyeballs share an eyelid with different activation targets",
                    ));
                }
                format!(
                    "    eyelid {} {} lowerer 0 {} neutral 0 {} raiser 0 {}",
                    quoted(name)?,
                    quoted(filename)?,
                    lid.targets[0],
                    lid.targets[1],
                    lid.targets[2]
                )
            };
            writeln!(bound, "{template} eyeball {}", quoted(&lid.eye)?).unwrap();
        }
        block.push_str(&self.options);
        block.push_str(&bound);
        Ok(block)
    }
}

fn vector(reader: &Reader<'_>, offset: usize) -> Result<[f32; 3], DecompileError> {
    Ok([
        reader.float(offset)?,
        reader.float(offset + 4)?,
        reader.float(offset + 8)?,
    ])
}

fn normalize(value: [f32; 3]) -> Result<[f32; 3], DecompileError> {
    let length = value.iter().map(|value| value * value).sum::<f32>().sqrt();
    if !length.is_finite() || (length - 1.0).abs() > 0.001 {
        return Err(invalid(
            "An eyeball references a nonorthogonal bone transform",
        ));
    }
    Ok(value.map(|value| value / length))
}
