use super::{Animation, DecompileError, Reader, invalid};
use crate::model_decompile::quoted;
use mdlc::bone_math as math;
use std::fmt::Write as _;
use vformats::mdl::Mdl;

pub(super) fn recover(
    reader: &Reader<'_>,
    ani: Option<&[u8]>,
    model: &Mdl,
    record: usize,
    animation: &Animation,
) -> Result<String, DecompileError> {
    let count = reader.count(record + 72)?;
    if count == 0 {
        return Ok(String::new());
    }
    if count > 1024 || animation.frames.len() < 2 {
        return Err(invalid("Invalid local hierarchy count or animation length"));
    }
    if animation.flags & 4 != 0 {
        return Err(invalid(
            "Delta animation local hierarchy cannot be reconstructed from its source poses",
        ));
    }
    let block = reader.i32(record + 52)?;
    let offset = reader.count(record + 76)?;
    if offset == 0 {
        return Err(invalid("Local hierarchy table is absent"));
    }
    let (bytes, table) = if block == 0 {
        (
            reader.0,
            record
                .checked_add(offset)
                .ok_or(DecompileError::SizeLimit)?,
        )
    } else if block > 0 && (block as usize) < reader.count(352)? {
        let blocks = reader.count(356)?;
        let entry = blocks
            .checked_add(
                (block as usize)
                    .checked_mul(8)
                    .ok_or(DecompileError::SizeLimit)?,
            )
            .ok_or(DecompileError::SizeLimit)?;
        let data = ani
            .and_then(|data| data.get(reader.count(entry).ok()?..reader.count(entry + 4).ok()?))
            .ok_or_else(|| {
                invalid("Matching external animation block is required for local hierarchy")
            })?;
        (data, offset)
    } else {
        return Err(invalid(
            "Local hierarchy references an unavailable animation block",
        ));
    };
    let data = Reader(bytes);
    data.bytes(table, count * 48)?;
    let parents: Vec<_> = model.bones.iter().map(|bone| bone.parent).collect();
    let mut source = animation.frames.clone();
    let root = math::angle_matrix([0.0, 0.0, std::f32::consts::FRAC_PI_2]);
    for frame in &mut source {
        for (bone, pose) in frame.iter_mut().enumerate() {
            if parents[bone] < 0 {
                let matrix =
                    math::concat(&root, &math::local_transform(pose.position, pose.rotation));
                pose.position = [matrix[3], matrix[7], matrix[11]];
                pose.rotation = math::matrix_angles(&matrix);
            }
        }
    }
    let mut result = String::new();
    let last = (source.len() - 1) as i32;
    let mut work = 0_usize;
    for index in 0..count {
        let at = table + index * 48;
        let bone = data.count(at)?;
        let parent = data.i32(at + 4)?;
        if bone >= model.bones.len() || parent < -1 || parent >= model.bones.len() as i32 {
            return Err(invalid("Local hierarchy references an absent bone"));
        }
        let mut range = [0; 4];
        for (phase, frame) in range.iter_mut().enumerate() {
            let value = data.f32(at + 8 + phase * 4)? * last as f32;
            if value < 0.0 || value > last as f32 * 4.0 || (value - value.round()).abs() > 0.001 {
                return Err(invalid(
                    "Local hierarchy time range is not representable in QC",
                ));
            }
            *frame = value.round() as i32;
        }
        if range.windows(2).any(|window| window[0] > window[1])
            || range[0] > last
            || data.i32(at + 24)? != range[0]
        {
            return Err(invalid("Invalid local hierarchy time range or start frame"));
        }
        let samples = (range[3] - range[0] + 1 + if range[3] > last { 2 } else { 0 }) as usize;
        work = samples
            .checked_mul(model.bones.len())
            .and_then(|samples| work.checked_add(samples))
            .filter(|work| *work <= super::MAX_POSES)
            .ok_or(DecompileError::SizeLimit)?;
        if data.i32(at + 28)? <= 0 {
            return Err(invalid("Compressed local hierarchy payload is absent"));
        }
        let compressed = data.relative(at, at + 28)?;
        data.bytes(compressed, 36)?;
        let mut channels = Vec::with_capacity(6);
        let mut scales = [0.0; 6];
        for (channel, scale) in scales.iter_mut().enumerate() {
            *scale = data.f32(compressed + channel * 4)?;
            let offset = data.i16(compressed + 24 + channel * 2)?;
            if *scale < 0.0 || offset < 0 || (offset > 0 && offset < 36) {
                return Err(invalid("Invalid compressed local hierarchy channel"));
            }
            let mut values = Vec::with_capacity(samples);
            if offset == 0 {
                values.resize(samples, 0.0);
            } else {
                let mut cursor = compressed + offset as usize;
                while values.len() < samples {
                    let valid = data.u8(cursor)? as usize;
                    let total = data.u8(cursor + 1)? as usize;
                    if valid == 0 || total < valid {
                        return Err(invalid("Malformed local hierarchy sample run"));
                    }
                    data.bytes(cursor + 2, valid * 2)?;
                    for local in 0..total.min(samples - values.len()) {
                        let value =
                            f32::from(data.i16(cursor + 2 + local.min(valid - 1) * 2)?) * *scale;
                        if !value.is_finite() {
                            return Err(invalid("Local hierarchy sample overflow"));
                        }
                        values.push(value);
                    }
                    cursor += 2 + valid * 2;
                }
            }
            channels.push(values);
        }
        for (sample, _) in channels[0].iter().enumerate().take(samples) {
            let frame = range[0] as usize + sample;
            let frame = if animation.flags & 1 != 0 {
                frame % source.len()
            } else {
                frame.min(source.len() - 1)
            };
            let frame = &source[frame];
            let world = math::compute_world(
                &frame.iter().map(|pose| pose.position).collect::<Vec<_>>(),
                &frame.iter().map(|pose| pose.rotation).collect::<Vec<_>>(),
                &parents,
            );
            let candidate = if parent < 0 {
                world[bone]
            } else {
                math::concat(&math::invert(&world[parent as usize]), &world[bone])
            };
            for (axis, value) in [candidate[3], candidate[7], candidate[11]]
                .iter()
                .enumerate()
            {
                if (*value - channels[axis][sample]).abs() > scales[axis] * 2.0 + 0.003 {
                    return Err(invalid(
                        "Local hierarchy source poses differ from the exported animation; independent raw-source overrides cannot be represented by standard QC",
                    ));
                }
            }
            let actual = math::angle_quaternion([
                channels[3][sample],
                channels[4][sample],
                channels[5][sample],
            ]);
            let candidate = math::matrix_quaternion(&candidate);
            let angle = 2.0
                * actual
                    .iter()
                    .zip(candidate)
                    .map(|(a, b)| a * b)
                    .sum::<f32>()
                    .abs()
                    .clamp(-1.0, 1.0)
                    .acos();
            if angle > scales[3..].iter().sum::<f32>() * 2.0 + 0.001 {
                return Err(invalid(format!(
                    "Local hierarchy source rotations differ at rule {index}, sample {sample}, angle {angle}, scales {:?}",
                    &scales[3..]
                )));
            }
        }
        let parent = if parent < 0 {
            ""
        } else {
            model.bones[parent as usize].name.as_str()
        };
        let mut qc_range = range;
        for phase in (1..4).rev() {
            if range[phase] > last {
                qc_range[phase] -= last;
            }
        }
        write!(
            result,
            " localhierarchy {} {} range {} {} {} {}",
            quoted(&model.bones[bone].name)?,
            quoted(parent)?,
            qc_range[0],
            qc_range[1],
            qc_range[2],
            qc_range[3]
        )
        .unwrap();
    }
    Ok(result)
}
