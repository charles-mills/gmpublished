use std::fmt::Write as _;

use vformats::mdl::Mdl;

use super::{Animation, DecompileError, Reader, invalid, quoted};

pub(super) fn write(
    qc: &mut String,
    reader: &Reader<'_>,
    model: &Mdl,
    records: &[usize],
    animations: &[Animation],
    warnings: &mut Vec<String>,
) -> Result<Vec<mdlc::smd::SmdPose>, DecompileError> {
    qc.push_str(&super::transitions::prelude(reader, records)?);
    let mut first_sequence_pose = None;
    for (index, &record) in records.iter().enumerate() {
        let name = reader.string(record, record + 4)?;
        let stored_flags = reader.i32(record + 12)? as u32;
        let flags = if model.version <= 48 {
            (stored_flags & !(0x0080 | 0x0100 | 0x0200 | 0x1000 | 0x2000 | 0x4000))
                | if stored_flags & 0x0100 != 0 {
                    0x0080
                } else {
                    0
                }
                | if stored_flags & 0x0200 != 0 {
                    0x1000
                } else {
                    0
                }
                | if stored_flags & 0x4000 != 0 {
                    0x2000
                } else {
                    0
                }
        } else {
            stored_flags
        };
        if flags & 0x800 != 0 {
            writeln!(qc, "$declaresequence {}", quoted(name)?).unwrap();
            continue;
        }
        let count = reader.count(record + 56)?;
        let grid = [reader.count(record + 68)?, reader.count(record + 72)?];
        if count == 0 || count > 4096 || grid[0].checked_mul(grid[1]) != Some(count) {
            return Err(invalid("Invalid sequence blend grid"));
        }
        let table = reader.relative(record, record + 60)?;
        let mut cells = Vec::with_capacity(count);
        for cell in 0..count {
            let animation = usize::try_from(reader.i16(table + cell * 2)?)
                .ok()
                .filter(|animation| *animation < animations.len())
                .ok_or_else(|| invalid("Sequence references an absent animation"))?;
            cells.push(animation);
        }
        let frame_count = animations[cells[0]].frames.len();
        if cells.iter().any(|cell| animations[*cell].frames.is_empty()) {
            return Err(invalid(
                "Local sequence references an external animation declaration",
            ));
        }
        first_sequence_pose.get_or_insert_with(|| animations[cells[0]].frames[0].clone());
        let weights = reader.relative(record, record + 156)?;
        write!(qc, "$weightlist \"__decompiled_weights_{index}\" {{").unwrap();
        for (bone, entry) in model.bones.iter().enumerate() {
            let weight = reader.f32(weights + bone * 4)?;
            if !(0.0..=1.0).contains(&weight) {
                return Err(invalid("Sequence contains an invalid bone weight"));
            }
            write!(qc, " {} {weight}", quoted(&entry.name)?).unwrap();
        }
        qc.push_str(" }\n");
        writeln!(qc, "$sequence {} {{", quoted(name)?).unwrap();
        qc.push_str(&super::transitions::sequence(reader, record)?);
        for cell in &cells {
            writeln!(qc, "    \"__decompiled_animation_{cell}\"").unwrap();
        }
        writeln!(qc, "    weightlist \"__decompiled_weights_{index}\"").unwrap();
        if count > 1 {
            writeln!(qc, "    blendwidth {}", grid[0]).unwrap();
        }
        let mut blend_options: [Option<String>; 2] = [None, None];
        let mut nonlinear = [None, None];
        for axis in 0..2 {
            let parameter = reader.i32(record + 76 + axis * 4)?;
            if parameter < 0 {
                if grid[axis] > 1 {
                    return Err(invalid("Sequence blend axis has no pose parameter"));
                }
                continue;
            }
            let parameters = reader.table(300, 304, 20)?;
            let parameter = parameters
                .get(parameter as usize)
                .ok_or_else(|| invalid("Sequence references an absent pose parameter"))?;
            let parameter_name = reader.string(*parameter, *parameter)?;
            let start = reader.f32(record + 84 + axis * 4)?;
            let end = reader.f32(record + 92 + axis * 4)?;
            if count > 1 && reader.i32(record + 160)? != 0 {
                let keys = reader.relative(record, record + 160)?;
                let first = if axis == 0 { 0 } else { grid[0] };
                let mut actual_keys = Vec::with_capacity(grid[axis]);
                let mut differs = false;
                for key in 0..grid[axis] {
                    let expected = if grid[axis] == 1 {
                        start
                    } else {
                        start + (end - start) * key as f32 / (grid[axis] - 1) as f32
                    };
                    let actual = reader.f32(keys + (first + key) * 4)?;
                    actual_keys.push(actual);
                    differs |=
                        (actual - expected).abs() > 0.0001 + expected.abs() * f32::EPSILON * 8.0;
                }
                if differs {
                    if (actual_keys[0] - start).abs() > 0.0001 + start.abs() * f32::EPSILON * 8.0
                        || (actual_keys.last().unwrap() - end).abs()
                            > 0.0001 + end.abs() * f32::EPSILON * 8.0
                    {
                        return Err(invalid(
                            "Sequence blend key endpoints do not match their range",
                        ));
                    }
                    nonlinear[axis] = Some(super::calcblend::Axis {
                        parameter: parameter_name.into(),
                        keys: actual_keys,
                    });
                }
            }
            blend_options[axis] = Some(format!(
                "    blend {} {start} {end}\n",
                quoted(parameter_name)?
            ));
        }
        if nonlinear.iter().any(Option::is_some) {
            let recovered =
                super::calcblend::recover(reader, model, animations, &cells, grid, &nonlinear)?;
            for (option, recovered) in blend_options.iter_mut().zip(recovered.axes) {
                if recovered.is_some() {
                    *option = recovered;
                }
            }
            qc.push_str(&recovered.options);
        }
        for option in blend_options.iter().flatten() {
            qc.push_str(option);
        }
        if flags & 1 != 0 {
            qc.push_str("    loop\n");
        }
        for (bit, keyword) in [
            (2, "snap"),
            (4, "delta"),
            (8, "autoplay"),
            (16, "post"),
            (32, "allzeros"),
            (128, "realtime"),
            (4096, "local"),
            (1024, "hidden"),
            (8192, "worldspace"),
        ] {
            if flags & bit == 0 {
                continue;
            }
            if keyword == "delta" {
                qc.push_str(if flags & 16 != 0 {
                    "    delta\n"
                } else {
                    "    predelta\n"
                });
            } else if keyword == "allzeros" {
                warnings.push(format!(
                    "Sequence '{name}' flag {keyword} was not reconstructed."
                ));
            } else {
                writeln!(qc, "    {keyword}").unwrap();
            }
        }
        let activity = reader.string(record, record + 8)?;
        if !activity.is_empty() {
            writeln!(
                qc,
                "    activity {} {}",
                quoted(activity)?,
                reader.i32(record + 20)?
            )
            .unwrap();
        }
        writeln!(
            qc,
            "    fadein {}\n    fadeout {}",
            reader.f32(record + 104)?,
            reader.f32(record + 108)?
        )
        .unwrap();
        let event_count = reader.count(record + 24)?;
        if event_count > 65_536 {
            return Err(DecompileError::SizeLimit);
        }
        let events = reader.relative(record, record + 28)?;
        for event in 0..event_count {
            let event = events + event * 80;
            let cycle = reader.f32(event)?;
            if !(0.0..=1.0).contains(&cycle) {
                return Err(invalid("Sequence event cycle is outside its animation"));
            }
            let frame = cycle * frame_count.saturating_sub(1) as f32;
            if (frame - frame.round()).abs() > 0.001 {
                return Err(invalid(
                    "Sequence event does not lie on an exportable source frame",
                ));
            }
            let kind = reader.i32(event + 8)?;
            let name = if kind & 1024 != 0 {
                quoted(reader.string(event, event + 76)?)?
            } else {
                let number = reader.i32(event + 4)?;
                if number < 0 {
                    return Err(invalid("Negative animation event identifier"));
                }
                number.to_string()
            };
            let options = reader.bytes(event + 12, 64)?;
            let length = options
                .iter()
                .position(|byte| *byte == 0)
                .unwrap_or(options.len());
            let options = std::str::from_utf8(&options[..length])
                .map_err(|_| invalid("Animation event options are not UTF-8"))?;
            writeln!(
                qc,
                "    event {name} {} {}",
                frame.round() as usize,
                quoted(options)?
            )
            .unwrap();
        }
        super::extras::write(qc, reader, records, record, frame_count)?;
        if model.version <= 48 && stored_flags & 0x80 != 0 {
            let index = reader.count(record + 180)?;
            let parameters = reader.table(300, 304, 20)?;
            let parameter = *parameters
                .get(index)
                .ok_or_else(|| invalid("Sequence cycle references an absent pose parameter"))?;
            writeln!(
                qc,
                "    cyclepose {}",
                quoted(reader.string(parameter, parameter)?)?
            )
            .unwrap();
        }
        qc.push_str("}\n");
    }
    Ok(first_sequence_pose.unwrap_or_default())
}
