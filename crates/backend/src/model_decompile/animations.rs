use std::fmt::Write as _;
use std::sync::atomic::AtomicBool;

use mdlc::smd::SmdPose;
use vformats::mdl::Mdl;

use super::{DecompileError, ExportFiles, MAX_OUTPUT_BYTES, check_cancelled, invalid, quoted};

#[path = "animations/calcblend.rs"]
mod calcblend;
#[path = "animations/decode.rs"]
mod decode;
#[path = "animations/extras.rs"]
mod extras;
#[path = "animations/flags.rs"]
mod flags;
#[path = "animations/hierarchy.rs"]
mod hierarchy;
#[path = "animations/ik.rs"]
mod ik;
#[path = "animations/movement.rs"]
mod movement;
#[path = "animations/sequences.rs"]
mod sequences;
#[path = "animations/transitions.rs"]
mod transitions;

#[path = "animations/preview.rs"]
pub(super) mod preview;

const MAX_POSES: usize = 2_000_000;

pub(super) struct Animation {
    fps: f32,
    flags: u32,
    frames: Vec<Vec<SmdPose>>,
}

pub(super) struct AnimationReport {
    pub warnings: Vec<String>,
    pub first_sequence_pose: Vec<SmdPose>,
}

pub(super) struct BlenderAnimation {
    pub name: String,
    pub fps: f32,
    pub flags: u32,
    pub frames: Vec<Vec<SmdPose>>,
}

pub(super) fn read_for_blender(
    raw: &[u8],
    ani: Option<&[u8]>,
    model: &Mdl,
    cancelled: &AtomicBool,
) -> Result<(Vec<BlenderAnimation>, Vec<String>), DecompileError> {
    let reader = Reader(raw);
    let records = reader.table(180, 184, 100)?;
    let mut clips = Vec::new();
    let mut warnings = Vec::new();
    let mut poses = 0usize;
    let root_rotation = mdlc::bone_math::angle_quaternion([0.0, 0.0, std::f32::consts::FRAC_PI_2]);
    for record in records {
        check_cancelled(cancelled)?;
        let name = reader.string(record, record + 4)?.to_owned();
        let flags = reader.i32(record + 12)? as u32;
        if flags & 0x0800 != 0 {
            warnings.push(format!(
                "Animation {name:?} is declared in another model; its pose data is not available."
            ));
            continue;
        }
        let count = reader.count(record + 16)?;
        poses = poses
            .checked_add(
                count
                    .checked_mul(model.bones.len())
                    .ok_or(DecompileError::SizeLimit)?,
            )
            .filter(|count| *count <= MAX_POSES)
            .ok_or(DecompileError::SizeLimit)?;
        let mut decoded = match decode::animation(&reader, ani, model, record, cancelled) {
            Ok(decoded) => decoded,
            Err(error @ (DecompileError::Cancelled | DecompileError::SizeLimit)) => {
                return Err(error);
            }
            Err(error) => {
                warnings.push(format!("Animation {name:?} was not exported: {error}"));
                continue;
            }
        };
        if let Err(error) = movement::restore(&reader, model, record, &mut decoded) {
            warnings.push(format!("Animation {name:?} was not exported: {error}"));
            continue;
        }
        if model.flags & vformats::mdl::FLAG_STATIC_PROP == 0 {
            for frame in &mut decoded.frames {
                for (pose, bone) in frame.iter_mut().zip(&model.bones) {
                    if bone.parent < 0 {
                        pose.position = [-pose.position[1], pose.position[0], pose.position[2]];
                        if decoded.flags & 4 == 0 {
                            pose.rotation = mdlc::bone_math::quaternion_angles(
                                mdlc::bone_math::quaternion_mult(
                                    root_rotation,
                                    mdlc::bone_math::angle_quaternion(pose.rotation),
                                ),
                            );
                        }
                    }
                }
            }
        }
        clips.push(BlenderAnimation {
            name,
            fps: decoded.fps,
            flags: decoded.flags,
            frames: decoded.frames,
        });
    }
    Ok((clips, warnings))
}

pub(super) fn write_animations(
    qc: &mut String,
    export: &mut ExportFiles<'_>,
    raw: &[u8],
    ani: Option<&[u8]>,
    model: &Mdl,
    cancelled: &AtomicBool,
) -> Result<AnimationReport, DecompileError> {
    let reader = Reader(raw);
    let records = reader.table(180, 184, 100)?;
    let sequences = reader.table(188, 192, 212)?;
    if records.is_empty() || sequences.is_empty() {
        export.write("idle.smd", super::skeleton_smd(model)?.as_bytes())?;
        qc.push_str("$sequence \"idle\" \"idle.smd\" fps 30\n");
        return Ok(AnimationReport {
            warnings: vec![],
            first_sequence_pose: model
                .bones
                .iter()
                .enumerate()
                .map(|(index, bone)| SmdPose {
                    bone: index as i32,
                    position: bone.position,
                    rotation: bone.rotation,
                })
                .collect(),
        });
    }
    if !matches!(model.version, 44 | 46 | 48 | 49) {
        return Err(invalid("Unsupported MDL animation version"));
    }
    let mut animations = Vec::new();
    let mut poses = 0usize;
    let mut warnings = Vec::new();
    let mut delta_base_written = false;
    for (index, record) in records.into_iter().enumerate() {
        check_cancelled(cancelled)?;
        let frame_count = reader.count(record + 16)?;
        let flags = reader.i32(record + 12)? as u32;
        if flags & 0x0800 != 0 {
            if frame_count != 0 || flags != 0x0800 {
                return Err(invalid(
                    "Declared animation contains local pose data or flags",
                ));
            }
            writeln!(
                qc,
                "$declareanimation {}",
                quoted(reader.string(record, record + 4)?)?
            )
            .unwrap();
            animations.push(Animation {
                fps: 0.0,
                flags,
                frames: Vec::new(),
            });
            continue;
        }
        if frame_count == 0 || frame_count > MAX_POSES || model.bones.is_empty() {
            return Err(invalid("Invalid animation frame or bone count"));
        }
        poses = poses
            .checked_add(
                frame_count
                    .checked_mul(model.bones.len())
                    .ok_or(DecompileError::SizeLimit)?,
            )
            .filter(|count| *count <= MAX_POSES)
            .ok_or(DecompileError::SizeLimit)?;
        let mut animation = decode::animation(&reader, ani, model, record, cancelled)?;
        let ik_options = ik::recover(&reader, model, record, &animation)?;
        let movement_options = movement::restore(&reader, model, record, &mut animation)?;
        let hierarchy_options = hierarchy::recover(&reader, ani, model, record, &animation)?;
        let path = format!("animation_{index}.smd");
        export.write(&path, smd(model, &animation.frames)?.as_bytes())?;
        if animation.flags & 4 != 0 && !delta_base_written {
            let frames = vec![
                model
                    .bones
                    .iter()
                    .enumerate()
                    .map(|(bone, _)| SmdPose {
                        bone: bone as i32,
                        position: [0.0; 3],
                        rotation: [0.0; 3],
                    })
                    .collect(),
            ];
            export.write("animation_delta_base.smd", smd(model, &frames)?.as_bytes())?;
            qc.push_str(
                "$animation \"__decompiled_delta_base\" \"animation_delta_base.smd\" noautoik\n",
            );
            delta_base_written = true;
        }
        write!(
            qc,
            "$animation \"__decompiled_animation_{index}\" \"{path}\" fps {} noautoik",
            animation.fps
        )
        .unwrap();
        if animation.flags & 1 != 0 {
            qc.push_str(" loop");
        }
        if animation.flags & 4 != 0 {
            qc.push_str(" subtract \"__decompiled_delta_base\" 0");
        }
        qc.push_str(&flags::options(animation.flags, model.version));
        qc.push_str(&movement_options);
        qc.push_str(&ik_options);
        qc.push_str(&hierarchy_options);
        qc.push('\n');
        animations.push(animation);
    }
    let first_sequence_pose =
        sequences::write(qc, &reader, model, &sequences, &animations, &mut warnings)?;
    Ok(AnimationReport {
        warnings,
        first_sequence_pose,
    })
}

fn smd(model: &Mdl, frames: &[Vec<SmdPose>]) -> Result<String, DecompileError> {
    let mut text = String::from("version 1\nnodes\n");
    for (index, bone) in model.bones.iter().enumerate() {
        writeln!(text, "{index} {} {}", quoted(&bone.name)?, bone.parent).unwrap();
    }
    text.push_str("end\nskeleton\n");
    for (index, frame) in frames.iter().enumerate() {
        writeln!(text, "time {index}").unwrap();
        for pose in frame {
            writeln!(
                text,
                "{} {} {} {} {} {} {}",
                pose.bone,
                pose.position[0],
                pose.position[1],
                pose.position[2],
                pose.rotation[0],
                pose.rotation[1],
                pose.rotation[2]
            )
            .unwrap();
        }
        if text.len() > MAX_OUTPUT_BYTES {
            return Err(DecompileError::SizeLimit);
        }
    }
    text.push_str("end\n");
    Ok(text)
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn bytes(&self, at: usize, length: usize) -> Result<&'a [u8], DecompileError> {
        self.0
            .get(at..at.checked_add(length).ok_or(DecompileError::SizeLimit)?)
            .ok_or_else(|| invalid("Truncated animation data"))
    }
    fn u8(&self, at: usize) -> Result<u8, DecompileError> {
        Ok(self.bytes(at, 1)?[0])
    }
    fn u16(&self, at: usize) -> Result<u16, DecompileError> {
        Ok(u16::from_le_bytes(self.bytes(at, 2)?.try_into().unwrap()))
    }
    fn i16(&self, at: usize) -> Result<i16, DecompileError> {
        Ok(i16::from_le_bytes(self.bytes(at, 2)?.try_into().unwrap()))
    }
    fn i32(&self, at: usize) -> Result<i32, DecompileError> {
        Ok(i32::from_le_bytes(self.bytes(at, 4)?.try_into().unwrap()))
    }
    fn f32(&self, at: usize) -> Result<f32, DecompileError> {
        let value = f32::from_le_bytes(self.bytes(at, 4)?.try_into().unwrap());
        if !value.is_finite() {
            return Err(invalid("Animation data contains nonfinite values"));
        }
        Ok(value)
    }
    fn count(&self, at: usize) -> Result<usize, DecompileError> {
        usize::try_from(self.i32(at)?).map_err(|_| invalid("Negative animation count or offset"))
    }
    fn relative(&self, base: usize, at: usize) -> Result<usize, DecompileError> {
        let result = (base as i64)
            .checked_add(i64::from(self.i32(at)?))
            .ok_or(DecompileError::SizeLimit)?;
        usize::try_from(result)
            .ok()
            .filter(|offset| *offset <= self.0.len())
            .ok_or_else(|| invalid("Animation offset is outside its file"))
    }
    fn string(&self, base: usize, at: usize) -> Result<&'a str, DecompileError> {
        let start = self.relative(base, at)?;
        let tail = self.bytes(start, self.0.len() - start)?;
        let length = tail
            .iter()
            .take(65_537)
            .position(|byte| *byte == 0)
            .ok_or_else(|| invalid("Unterminated animation name"))?;
        std::str::from_utf8(&tail[..length]).map_err(|_| invalid("Animation name is not UTF-8"))
    }
    fn table(
        &self,
        count_at: usize,
        offset_at: usize,
        stride: usize,
    ) -> Result<Vec<usize>, DecompileError> {
        let count = self.count(count_at)?;
        if count > 65_536 {
            return Err(DecompileError::SizeLimit);
        }
        if count == 0 {
            return Ok(vec![]);
        }
        let base = self.count(offset_at)?;
        self.bytes(
            base,
            count.checked_mul(stride).ok_or(DecompileError::SizeLimit)?,
        )?;
        Ok((0..count).map(|index| base + index * stride).collect())
    }
}

#[cfg(test)]
#[path = "animations/tests.rs"]
mod tests;
