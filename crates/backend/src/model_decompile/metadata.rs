use std::fmt::Write as _;

use mdlc::bone_math;
use vformats::mdl::{self, Mdl};

use super::{DecompileError, ExportFiles, invalid, quoted, safe_model_name};

const MAX_ENTRIES: usize = 65_536;
const MAX_TEXT_BYTES: usize = 1_048_576;
const MAX_NAME_BYTES: usize = 1024;

#[derive(Default)]
pub(super) struct MetadataReport {
    pub preserved: Vec<usize>,
    pub warnings: Vec<String>,
}

pub(super) fn write_metadata(
    qc: &mut String,
    raw: &[u8],
    model: &Mdl,
) -> Result<MetadataReport, DecompileError> {
    super::validate_skeleton(model)?;
    let reader = MetadataReader(raw);
    let mut text = String::new();
    let mut report = MetadataReport::default();
    write_contents(&mut text, &reader, model, &mut report)?;
    let bone_metadata = reader.table(156, 160, 216)?;
    let static_prop = model.flags & mdl::FLAG_STATIC_PROP != 0;
    if static_prop && bone_metadata.len() > 1 {
        report.warnings.push("Joint surface properties on a static prop with multiple bones were not reconstructed because compilation collapses its skeleton.".to_owned());
    } else {
        for (&record, bone) in bone_metadata.iter().zip(&model.bones) {
            if reader.i32(record + 176)? != 0 {
                let name = if static_prop {
                    mdlc::model::STATIC_PROP_BONE
                } else {
                    &bone.name
                };
                writeln!(
                    text,
                    "$jointsurfaceprop {} {}",
                    quoted(name)?,
                    quoted(reader.optional_relative_string(record, 176)?)?
                )
                .unwrap();
            }
        }
    }
    for (flag, command) in [
        (1 << 2, "$opaque"),
        (1 << 3, "$mostlyopaque"),
        (1 << 11, "$noforcedfade"),
        (1 << 12, "$forcephonemecrossfade"),
        (1 << 16, "$ambientboost"),
        (1 << 17, "$donotcastshadows"),
        (1 << 18, "$casttextureshadows"),
    ] {
        if model.flags & flag != 0 {
            writeln!(text, "{command}").unwrap();
        }
    }
    if model.flags & (1 << 13) != 0 {
        let value = *raw
            .get(0x178)
            .ok_or_else(|| invalid("Truncated directional light setting"))?;
        let normalized = if value == 255 {
            1.0
        } else {
            (f64::from(value) + 0.5) / 255.0
        };
        writeln!(text, "$constantdirectionallight {normalized}").unwrap();
    }

    if model.flags & mdl::FLAG_STATIC_PROP == 0 {
        let records = reader.table(156, 160, 216)?;
        if records.len() != model.bones.len() {
            return Err(invalid(
                "The model bone metadata does not match its skeleton",
            ));
        }
        // Explicit bones retain unused animation bones and prevent a second IK realignment pass.
        for (index, bone) in model.bones.iter().enumerate() {
            let parent = if bone.parent == -1 {
                ""
            } else {
                &model.bones[bone.parent as usize].name
            };
            let [roll, pitch, yaw] = bone.rotation.map(f32::to_degrees);
            writeln!(
                text,
                "$definebone {} {} {} {} {} {pitch} {yaw} {roll}",
                quoted(&bone.name)?,
                quoted(parent)?,
                bone.position[0],
                bone.position[1],
                bone.position[2]
            )
            .unwrap();
            if reader.i32(records[index] + 160)? & 0x0010_0000 != 0 {
                let alignment = [
                    reader.float(records[index] + 144)?,
                    reader.float(records[index] + 148)?,
                    reader.float(records[index] + 152)?,
                    reader.float(records[index] + 156)?,
                ];
                let length_squared: f32 = alignment.iter().map(|value| value * value).sum();
                if (length_squared - 1.0).abs() > 0.01 {
                    return Err(invalid(
                        "A fixed-alignment bone quaternion is not normalized",
                    ));
                }
                let bind = bone_math::angle_quaternion(bone.rotation);
                let bind_alignment = alignment
                    .iter()
                    .zip(bind)
                    .all(|(a, b)| (a - b).abs() < 0.00001);
                if bind_alignment {
                    writeln!(text, "$limitrotation {} \"\"", quoted(&bone.name)?).unwrap();
                } else {
                    writeln!(text, "$limitrotation {}", quoted(&bone.name)?).unwrap();
                    report.warnings.push(format!(
                        "Bone {:?} uses a fixed quaternion alignment that will be recalculated from the recovered animations; the original alignment may differ.",
                        bone.name
                    ));
                }
            }
            if reader.i32(records[index] + 160)? & 0x40000 != 0 {
                writeln!(text, "$bonemerge {}", quoted(&bone.name)?).unwrap();
            }
        }
    }
    let root_lod = *raw
        .get(0x179)
        .ok_or_else(|| invalid("Truncated model root LOD settings"))?;
    if root_lod > 7 {
        return Err(invalid("Model root LOD is outside the Source limit"));
    }
    if root_lod != 0 {
        writeln!(text, "$minlod {root_lod}").unwrap();
        report.warnings.push(format!("The compiled model uses minimum LOD {root_lod}; the original higher-detail geometry was stripped and cannot be recovered."));
    }
    let allowed_root_lods = *raw
        .get(0x17a)
        .ok_or_else(|| invalid("Truncated model root LOD settings"))?;
    if allowed_root_lods != 0 {
        writeln!(text, "$allowrootlods {allowed_root_lods}").unwrap();
    }
    let sets = reader.table(172, 176, 12)?;
    let mut set_names = std::collections::HashSet::new();
    for set in sets {
        let name = reader.relative_string(set, 0)?;
        if !set_names.insert(name.to_ascii_lowercase()) {
            return Err(invalid(
                "Duplicate hitbox set names cannot be reconstructed without merging them",
            ));
        }
        writeln!(text, "$hboxset {}", quoted(name)?).unwrap();
        let boxes = reader.relative_table(set, 4, 8, 68)?;
        for hitbox in boxes {
            let bone = reader.bone(model, hitbox)?;
            let group = reader.i32(hitbox + 4)?;
            let min = reader.vector(hitbox + 8)?;
            let max = reader.vector(hitbox + 20)?;
            if (0..3).any(|axis| min[axis] > max[axis]) {
                return Err(invalid("A hitbox has reversed bounds"));
            }
            let name = reader.optional_relative_string(hitbox, 32)?;
            writeln!(
                text,
                "$hbox {group} {} {} {} {} {} {} {} {}",
                quoted(bone)?,
                min[0],
                min[1],
                min[2],
                max[0],
                max[1],
                max[2],
                quoted(name)?
            )
            .unwrap();
        }
    }
    report.preserved.push(172);
    let attachments = reader.table(240, 244, 92)?;
    let header2 = reader.offset(400)?;
    let illumination_attachment = if header2 == 0 {
        0
    } else {
        reader.count(header2.checked_add(8).ok_or(DecompileError::SizeLimit)?)?
    };
    if illumination_attachment > attachments.len() {
        return Err(invalid("Illumination references an absent attachment"));
    }
    let mut illumination_preserved = illumination_attachment == 0;
    for (index, attachment) in attachments.into_iter().enumerate() {
        let name = reader.relative_string(attachment, 0)?;
        let flags = reader.i32(attachment + 4)?;
        if flags & !0x10000 != 0 {
            return Err(invalid("An attachment uses unsupported flags"));
        }
        let bone = reader.bone(model, attachment + 8)?;
        let mut matrix = [0.0; 12];
        for (index, value) in matrix.iter_mut().enumerate() {
            *value = reader.float(attachment + 12 + index * 4)?;
        }
        validate_rotation(&matrix)?;
        if model.flags & mdl::FLAG_STATIC_PROP != 0 {
            // Static-prop compilation left-multiplies attachment matrices by Rz(90°).
            let inverse = bone_math::angle_matrix([0.0, 0.0, -std::f32::consts::FRAC_PI_2]);
            matrix = bone_math::concat(&inverse, &matrix);
        }
        if index + 1 == illumination_attachment && name == "__illumPosition" && flags == 0 {
            let rotation_matches =
                matrix
                    .iter()
                    .zip(&bone_math::IDENTITY)
                    .enumerate()
                    .all(|(component, (a, b))| {
                        [3, 7, 11].contains(&component) || (a - b).abs() < 0.00001
                    });
            let position = reader.vector(92)?;
            let position_matches = position
                .iter()
                .zip([matrix[3], matrix[7], matrix[11]])
                .all(|(a, b)| (a - b).abs() < 0.00001);
            if rotation_matches && position_matches {
                writeln!(
                    text,
                    "$illumposition {} {} {} {}",
                    position[0],
                    position[1],
                    position[2],
                    quoted(bone)?
                )
                .unwrap();
                illumination_preserved = true;
                continue;
            }
        }
        let [roll, pitch, yaw] = bone_math::matrix_angles(&matrix).map(f32::to_degrees);
        write!(
            text,
            "$attachment {} {} {} {} {} rotate {pitch} {yaw} {roll}",
            quoted(name)?,
            quoted(bone)?,
            matrix[3],
            matrix[7],
            matrix[11]
        )
        .unwrap();
        if flags & 0x10000 != 0 {
            text.push_str(" world_align");
        }
        text.push('\n');
    }
    if !illumination_preserved {
        report.warnings.push("Bone-relative illumination was exported as a fixed illumination position; its transformed attachment link could not be reconstructed.".to_owned());
    }
    report.preserved.push(240);
    let chains = reader.table(284, 288, 16)?;
    let mut chain_names = Vec::with_capacity(chains.len());
    for chain in chains {
        let name = reader.relative_string(chain, 0)?;
        if reader.i32(chain + 4)? != 0 {
            return Err(invalid("An IK chain uses an unsupported link type"));
        }
        let links = reader.relative_table(chain, 8, 12, 28)?;
        if links.len() != 3 {
            return Err(invalid("An IK chain does not contain three links"));
        }
        let bones: Vec<_> = links
            .iter()
            .map(|link| reader.offset(*link))
            .collect::<Result<_, _>>()?;
        for &bone in &bones {
            model
                .bones
                .get(bone)
                .ok_or_else(|| invalid("An IK chain references an absent bone"))?;
        }
        if model.bones[bones[2]].parent != bones[1] as i32
            || model.bones[bones[1]].parent != bones[0] as i32
        {
            return Err(invalid("An IK chain differs from the model bone hierarchy"));
        }
        if reader.vector(links[1] + 4)? != [0.0; 3] || reader.vector(links[2] + 4)? != [0.0; 3] {
            return Err(invalid(
                "An IK chain has unsupported secondary knee directions",
            ));
        }
        let knee = reader.vector(links[0] + 4)?;
        writeln!(
            text,
            "$ikchain {} {} knee {} {} {}",
            quoted(name)?,
            quoted(&model.bones[bones[2]].name)?,
            knee[0],
            knee[1],
            knee[2]
        )
        .unwrap();
        chain_names.push(name);
    }
    report.preserved.push(284);
    for lock in reader.table(320, 324, 32)? {
        let chain = reader.offset(lock)?;
        let name = chain_names
            .get(chain)
            .ok_or_else(|| invalid("An IK autoplay lock references an absent chain"))?;
        if reader.i32(lock + 12)? != 0 {
            return Err(invalid("An IK autoplay lock has unsupported flags"));
        }
        writeln!(
            text,
            "$ikautoplaylock {} {} {}",
            quoted(name)?,
            reader.float(lock + 4)?,
            reader.float(lock + 8)?
        )
        .unwrap();
    }
    report.preserved.push(320);
    for parameter in reader.table(300, 304, 20)? {
        let name = reader.relative_string(parameter, 0)?;
        let flags = reader.i32(parameter + 4)?;
        let start = reader.float(parameter + 8)?;
        let end = reader.float(parameter + 12)?;
        let looping = reader.float(parameter + 16)?;
        if flags & !1 != 0 || ((flags & 1 == 0) != (looping == 0.0)) {
            return Err(invalid("A pose parameter has unsupported loop flags"));
        }
        write!(text, "$poseparameter {} {start} {end}", quoted(name)?).unwrap();
        if flags & 1 != 0 {
            write!(text, " loop {looping}").unwrap();
        }
        text.push('\n');
    }
    report.preserved.push(300);
    for included in reader.table(336, 340, 8)? {
        let name = safe_model_name(reader.relative_string(included, 4)?)?;
        let name = name.strip_prefix("models/").unwrap_or(&name);
        writeln!(text, "$includemodel {}", quoted(name)?).unwrap();
    }
    report.preserved.push(336);
    let keyvalue_size = reader.count(316)?;
    if keyvalue_size != 0 {
        if keyvalue_size > MAX_TEXT_BYTES {
            return Err(DecompileError::SizeLimit);
        }
        let start = reader.offset(312)?;
        let end = start
            .checked_add(keyvalue_size)
            .ok_or(DecompileError::SizeLimit)?;
        let data = raw
            .get(start..end)
            .ok_or_else(|| invalid("Truncated model keyvalues"))?;
        let data = data.strip_suffix(&[0]).unwrap_or(data);
        if data.contains(&0) {
            return Err(invalid("Model keyvalues contain embedded nulls"));
        }
        let data =
            std::str::from_utf8(data).map_err(|_| invalid("Model keyvalues are not UTF-8"))?;
        write_keyvalues(&mut text, data)?;
    }
    report.preserved.push(316);
    for controller in reader.table(164, 168, 56)? {
        let bone = reader.bone(model, controller)?;
        let kind = reader.i32(controller + 4)?;
        let axis = match kind & !0x8000 {
            1 => "x",
            2 => "y",
            4 => "z",
            8 => "xr",
            16 => "yr",
            32 => "zr",
            _ => return Err(invalid("A bone controller uses an unsupported axis")),
        };
        let start = reader.float(controller + 8)?;
        let end = reader.float(controller + 12)?;
        let expected_loop =
            kind & 0x38 != 0 && ((start + 360.0) as i32) % 360 == ((end + 360.0) as i32) % 360;
        if (kind & 0x8000 != 0) != expected_loop || reader.i32(controller + 16)? != 0 {
            return Err(invalid(
                "A bone controller has a rest value or looping flags that QC cannot reproduce",
            ));
        }
        let input = reader.i32(controller + 20)?;
        if !(0..=4).contains(&input) {
            return Err(invalid("A bone controller references an absent input"));
        }
        let input = if input == 4 {
            "mouth".to_owned()
        } else {
            input.to_string()
        };
        writeln!(
            text,
            "$controller {input} {} {axis} {start} {end}",
            quoted(bone)?
        )
        .unwrap();
    }
    report.preserved.push(164);
    if text.len() > super::MAX_OUTPUT_BYTES {
        return Err(DecompileError::SizeLimit);
    }
    qc.push_str(&text);
    Ok(report)
}

fn contents_options(mask: i32) -> String {
    let mut options = Vec::new();
    if mask & 8 != 0 {
        options.push("grate");
    } else if mask & 1 == 0 {
        options.push("notsolid");
    } else {
        options.push("solid");
    }
    if mask & 0x2000_0000 != 0 {
        options.push("ladder");
    }
    if mask & 0x0200_0000 != 0 {
        options.push("monster");
    }
    options.join(" ")
}

fn write_contents(
    text: &mut String,
    reader: &MetadataReader<'_>,
    model: &Mdl,
    report: &mut MetadataReport,
) -> Result<(), DecompileError> {
    const KNOWN: i32 = 0x2200_0009;
    let original_global = reader.i32(332)?;
    let global = original_global & KNOWN;
    if global != 1 {
        writeln!(text, "$contents {}", contents_options(global)).unwrap();
        if global & 9 == 9 {
            text.push_str("$contents solid\n");
        }
    }
    if global != original_global {
        report.warnings.push(format!(
            "Model contents {original_global:#010x} includes flags unavailable in standard QC; those flags were omitted and the recovered model uses {global:#010x}."
        ));
    }
    let records = reader.table(156, 160, 216)?;
    if records.len() != model.bones.len() {
        return Err(invalid(
            "The model bone metadata does not match its skeleton",
        ));
    }
    for (&record, bone) in records.iter().zip(&model.bones) {
        if reader.i32(record + 4)? != bone.parent {
            return Err(invalid(
                "The model bone parent metadata does not match its skeleton",
            ));
        }
    }
    let static_prop = model.flags & mdl::FLAG_STATIC_PROP != 0;
    if static_prop && records.len() > 1 {
        for (&record, bone) in records.iter().zip(&model.bones) {
            let original = reader.i32(record + 180)?;
            if original != global {
                report.warnings.push(format!(
                    "Bone {:?} contents {original:#010x} cannot be retained when a static-prop skeleton is collapsed; the recovered bone inherits model contents {global:#010x}.",
                    bone.name
                ));
            }
        }
        return Ok(());
    }
    let mut rebuilt = vec![None; model.bones.len()];
    let mut pending = Vec::new();
    for start in 0..model.bones.len() {
        let mut next = Some(start);
        while let Some(index) = next {
            if rebuilt[index].is_some() {
                break;
            }
            pending.push(index);
            next = usize::try_from(model.bones[index].parent).ok();
        }
        while let Some(index) = pending.pop() {
            let bone = &model.bones[index];
            let inherited = usize::try_from(bone.parent)
                .ok()
                .map_or(global, |parent| rebuilt[parent].unwrap());
            let original = reader.i32(records[index] + 180)?;
            let known = original & KNOWN;
            let mut recovered = inherited;
            if known != inherited && known & 9 != 9 {
                let name = if static_prop {
                    mdlc::model::STATIC_PROP_BONE
                } else {
                    &bone.name
                };
                writeln!(
                    text,
                    "$jointcontents {} {}",
                    quoted(name)?,
                    contents_options(known)
                )
                .unwrap();
                recovered = known;
            }
            if recovered != original {
                let reason = if known != inherited && known & 9 == 9 {
                    "a solid+grate override cannot be expressed by standard QC, so the override was omitted"
                } else {
                    "flags unavailable in standard QC were omitted"
                };
                report.warnings.push(format!(
                    "Bone {:?} contents {original:#010x} could not be reconstructed exactly: {reason}; the recovered bone uses {recovered:#010x}.",
                    bone.name
                ));
            }
            rebuilt[index] = Some(recovered);
        }
    }
    Ok(())
}

pub(super) fn mouth_options(raw: &[u8], model: &Mdl) -> Result<String, DecompileError> {
    let reader = MetadataReader(raw);
    let descriptors = reader.table(260, 264, 4)?;
    let mut text = String::new();
    for (index, mouth) in reader.table(292, 296, 20)?.iter().enumerate() {
        let bone = reader.bone(model, *mouth)?;
        let forward = reader.vector(mouth + 4)?;
        let descriptor = reader.offset(mouth + 16)?;
        let descriptor = *descriptors
            .get(descriptor)
            .ok_or_else(|| invalid("A mouth controller references an absent flex descriptor"))?;
        let name = reader.relative_string(descriptor, 0)?;
        writeln!(
            text,
            "    mouth {index} {} {} {} {} {}",
            quoted(name)?,
            quoted(bone)?,
            forward[0],
            forward[1],
            forward[2]
        )
        .unwrap();
    }
    Ok(text)
}

pub(super) fn write_procedural(
    qc: &mut String,
    export: &mut ExportFiles<'_>,
    raw: &[u8],
    model: &Mdl,
) -> Result<Vec<String>, DecompileError> {
    super::validate_skeleton(model)?;
    let reader = MetadataReader(raw);
    let records = reader.table(156, 160, 216)?;
    if records.len() != model.bones.len() {
        return Err(invalid(
            "The model bone metadata does not match its skeleton",
        ));
    }
    let mut commands = String::new();
    let mut vrd = String::new();
    let mut warnings = Vec::new();
    for (index, record) in records.into_iter().enumerate() {
        let kind = reader.i32(record + 164)?;
        if kind == 0 {
            continue;
        }
        if reader.i32(record + 168)? == 0 {
            return Err(invalid("A procedural bone has no procedure data"));
        }
        let data = reader.relative(record, 168)?;
        let bone = &model.bones[index];
        match kind {
            2 => {
                let control = reader.offset(data)?;
                let control_bone = model.bones.get(control).ok_or_else(|| invalid("A procedural bone references an absent control bone"))?;
                let parent = |parent: i32| if parent == -1 { "" } else { model.bones[parent as usize].name.as_str() };
                writeln!(vrd, "<helper> {} {} {} {}", quoted(&bone.name)?, quoted(parent(bone.parent))?, quoted(parent(control_bone.parent))?, quoted(&control_bone.name)?).unwrap();
                let triggers = reader.relative_table(data, 4, 8, 48)?;
                if triggers.is_empty() {
                    return Err(invalid("A quaternion helper contains no triggers"));
                }
                for trigger in triggers {
                    let inverse = reader.float(trigger)?;
                    if inverse <= 0.0 {
                        return Err(invalid("A quaternion helper has an invalid influence radius"));
                    }
                    let tolerance = inverse.recip().to_degrees();
                    let angles = procedural_quaternion(&reader, trigger + 4)?;
                    let target = procedural_quaternion(&reader, trigger + 32)?;
                    let position = reader.vector(trigger + 20)?;
                    writeln!(vrd, "<trigger> {tolerance} {} {} {} {} {} {} {} {} {}", angles[0], angles[1], angles[2], target[0], target[1], target[2], position[0], position[1], position[2]).unwrap();
                }
            }
            5 => {
                let flags = reader.i32(data)?;
                if flags & !0x7f != 0 || flags & 0x43 == 0 || flags & 3 == 2 && flags & 0x20 == 0 || flags & 3 == 0 && flags & 0x20 != 0 {
                    warnings.push(format!("Procedural jiggle bone {:?} uses flags {flags:#x} that the compiler cannot reproduce.", bone.name));
                    continue;
                }
                write_jiggle(&mut commands, &reader, data, flags, &bone.name)?;
            }
            _ => warnings.push(format!("Procedural bone {:?} uses unsupported procedure type {kind}; its procedural behavior was not reconstructed.", bone.name)),
        }
        if vrd.len() + commands.len() > super::MAX_OUTPUT_BYTES {
            return Err(DecompileError::SizeLimit);
        }
    }
    if !vrd.is_empty() {
        export.write("procedural.vrd", vrd.as_bytes())?;
        commands.push_str("$proceduralbones \"procedural.vrd\"\n");
    }
    qc.push_str(&commands);
    Ok(warnings)
}

fn procedural_quaternion(
    reader: &MetadataReader<'_>,
    at: usize,
) -> Result<[f32; 3], DecompileError> {
    let q = [
        reader.float(at)?,
        reader.float(at + 4)?,
        reader.float(at + 8)?,
        reader.float(at + 12)?,
    ];
    if (q.iter().map(|value| value * value).sum::<f32>() - 1.0).abs() > 0.001 {
        return Err(invalid(
            "A procedural helper contains an invalid quaternion",
        ));
    }
    let mut angles = bone_math::quaternion_angles(q).map(f32::to_degrees);
    let reconstructed = bone_math::angle_quaternion(angles.map(f32::to_radians));
    if reconstructed.iter().zip(q).map(|(a, b)| a * b).sum::<f32>() < 0.0 {
        angles[0] += 360.0;
    }
    Ok(angles)
}

fn write_jiggle(
    text: &mut String,
    reader: &MetadataReader<'_>,
    data: usize,
    flags: i32,
    bone: &str,
) -> Result<(), DecompileError> {
    let mut values = [0.0; 29];
    for (index, value) in values.iter_mut().enumerate() {
        *value = reader.float(data + 4 + index * 4)?;
    }
    writeln!(text, "$jigglebone {}\n{{", quoted(bone)?).unwrap();
    let block = if flags & 1 != 0 {
        "is_flexible"
    } else if flags & 2 != 0 {
        "is_rigid"
    } else {
        "has_base_spring"
    };
    writeln!(text, "    {block}\n    {{").unwrap();
    for (name, index) in [
        ("length", 0),
        ("tip_mass", 1),
        ("yaw_friction", 11),
        ("yaw_bounce", 12),
        ("pitch_friction", 15),
        ("pitch_bounce", 16),
    ] {
        writeln!(text, "        {name} {}", values[index]).unwrap();
    }
    for (flag, name, index, paired) in [
        (0x10, "angle_constraint", 8, false),
        (4, "yaw_constraint", 9, true),
        (8, "pitch_constraint", 13, true),
    ] {
        if flags & flag != 0 {
            write!(text, "        {name} {}", values[index].to_degrees()).unwrap();
            if paired {
                write!(text, " {}", values[index + 1].to_degrees()).unwrap();
            }
            text.push('\n');
        }
    }
    if flags & 1 != 0 {
        for (index, name) in [
            (2, "yaw_stiffness"),
            (3, "yaw_damping"),
            (4, "pitch_stiffness"),
            (5, "pitch_damping"),
            (6, "along_stiffness"),
            (7, "along_damping"),
        ] {
            if !(0.0..=1000.0).contains(&values[index]) {
                return Err(invalid(
                    "A jiggle bone stiffness or damping cannot be reproduced by QC",
                ));
            }
            writeln!(text, "        {name} {}", values[index]).unwrap();
        }
    }
    text.push_str("    }\n");
    if flags & 3 == 3 {
        text.push_str("    is_rigid { }\n");
    }
    if flags & 1 != 0 && flags & 0x20 == 0 {
        text.push_str("    is_flexible { allow_length_flex }\n");
    }
    if flags & 0x40 != 0 {
        if !(0.0..=1000.0).contains(&values[18]) || !(0.0..=1000.0).contains(&values[19]) {
            return Err(invalid(
                "A jiggle base spring stiffness or damping cannot be reproduced by QC",
            ));
        }
        text.push_str("    has_base_spring\n    {\n");
        for (index, name) in [
            (17, "base_mass"),
            (18, "stiffness"),
            (19, "damping"),
            (22, "left_friction"),
            (25, "up_friction"),
            (28, "forward_friction"),
        ] {
            writeln!(text, "        {name} {}", values[index]).unwrap();
        }
        for (index, name) in [
            (20, "left_constraint"),
            (23, "up_constraint"),
            (26, "forward_constraint"),
        ] {
            writeln!(
                text,
                "        {name} {} {}",
                values[index],
                values[index + 1]
            )
            .unwrap();
        }
        text.push_str("    }\n");
    }
    text.push_str("}\n");
    Ok(())
}

struct MetadataReader<'a>(&'a [u8]);

impl<'a> MetadataReader<'a> {
    fn i32(&self, at: usize) -> Result<i32, DecompileError> {
        let bytes = self
            .0
            .get(at..at.checked_add(4).ok_or(DecompileError::SizeLimit)?)
            .ok_or_else(|| invalid("Truncated model metadata"))?;
        Ok(i32::from_le_bytes(bytes.try_into().unwrap()))
    }

    fn count(&self, at: usize) -> Result<usize, DecompileError> {
        usize::try_from(self.i32(at)?).map_err(|_| invalid("Negative model metadata count"))
    }

    fn offset(&self, at: usize) -> Result<usize, DecompileError> {
        usize::try_from(self.i32(at)?).map_err(|_| invalid("Negative model metadata offset"))
    }

    fn float(&self, at: usize) -> Result<f32, DecompileError> {
        let value = f32::from_bits(self.i32(at)? as u32);
        if !value.is_finite() {
            return Err(invalid("Model metadata contains nonfinite coordinates"));
        }
        Ok(value)
    }

    fn vector(&self, at: usize) -> Result<[f32; 3], DecompileError> {
        Ok([self.float(at)?, self.float(at + 4)?, self.float(at + 8)?])
    }

    fn relative(&self, base: usize, field: usize) -> Result<usize, DecompileError> {
        let relative = self.i32(base + field)?;
        base.checked_add_signed(relative as isize)
            .filter(|at| *at < self.0.len())
            .ok_or_else(|| invalid("Invalid relative model metadata offset"))
    }

    fn table(
        &self,
        count: usize,
        offset: usize,
        stride: usize,
    ) -> Result<Vec<usize>, DecompileError> {
        let count = self.count(count)?;
        self.entries(count, self.offset(offset)?, stride)
    }

    fn relative_table(
        &self,
        base: usize,
        count: usize,
        offset: usize,
        stride: usize,
    ) -> Result<Vec<usize>, DecompileError> {
        let count = self.count(base + count)?;
        if count == 0 {
            return Ok(Vec::new());
        }
        self.entries(count, self.relative(base, offset)?, stride)
    }

    fn entries(
        &self,
        count: usize,
        offset: usize,
        stride: usize,
    ) -> Result<Vec<usize>, DecompileError> {
        if count > MAX_ENTRIES {
            return Err(DecompileError::SizeLimit);
        }
        if count == 0 {
            return Ok(Vec::new());
        }
        let end = count
            .checked_mul(stride)
            .and_then(|bytes| offset.checked_add(bytes))
            .ok_or(DecompileError::SizeLimit)?;
        if self.0.get(offset..end).is_none() {
            return Err(invalid("Model metadata table exceeds the file"));
        }
        Ok((0..count).map(|index| offset + index * stride).collect())
    }

    fn relative_string(&self, base: usize, field: usize) -> Result<&'a str, DecompileError> {
        let start = self.relative(base, field)?;
        let tail = &self.0[start..self.0.len().min(start.saturating_add(MAX_NAME_BYTES + 1))];
        let end = tail
            .iter()
            .position(|byte| *byte == 0)
            .ok_or_else(|| invalid("Unterminated model metadata string"))?;
        let text = std::str::from_utf8(&tail[..end])
            .map_err(|_| invalid("Model metadata string is not UTF-8"))?;
        if text.is_empty() {
            return Err(invalid("Empty model metadata name"));
        }
        Ok(text)
    }

    fn optional_relative_string(
        &self,
        base: usize,
        field: usize,
    ) -> Result<&'a str, DecompileError> {
        if self.i32(base + field)? == 0 {
            Ok("")
        } else {
            let start = match self.relative(base, field) {
                Ok(start) => start,
                Err(error) => {
                    // Version 44 compilers left unnamed hitbox names as absolute empty-string offsets.
                    let legacy = usize::try_from(self.i32(base + field)?).ok();
                    if self.i32(4)? == 44 && legacy.is_some_and(|at| self.0.get(at) == Some(&0)) {
                        return Ok("");
                    }
                    return Err(error);
                }
            };
            if self.0.get(start) == Some(&0) {
                Ok("")
            } else {
                self.relative_string(base, field)
            }
        }
    }

    fn bone(&self, model: &'a Mdl, at: usize) -> Result<&'a str, DecompileError> {
        model
            .bones
            .get(self.offset(at)?)
            .map(|bone| bone.name.as_str())
            .ok_or_else(|| invalid("Model metadata references an absent bone"))
    }
}

fn validate_rotation(matrix: &bone_math::Matrix3x4) -> Result<(), DecompileError> {
    let columns = [
        [matrix[0], matrix[4], matrix[8]],
        [matrix[1], matrix[5], matrix[9]],
        [matrix[2], matrix[6], matrix[10]],
    ];
    for (axis, column) in columns.iter().enumerate() {
        for (other, second) in columns.iter().enumerate() {
            let dot = column.iter().zip(second).map(|(a, b)| a * b).sum::<f32>();
            if (dot - if axis == other { 1.0 } else { 0.0 }).abs() > 0.001 {
                return Err(invalid("An attachment matrix is not a rigid rotation"));
            }
        }
    }
    let determinant = columns[0][0]
        * (columns[1][1] * columns[2][2] - columns[1][2] * columns[2][1])
        - columns[1][0] * (columns[0][1] * columns[2][2] - columns[0][2] * columns[2][1])
        + columns[2][0] * (columns[0][1] * columns[1][2] - columns[0][2] * columns[1][1]);
    if (determinant - 1.0).abs() > 0.001 {
        return Err(invalid("An attachment matrix contains a reflection"));
    }
    Ok(())
}

pub(super) fn write_keyvalues(output: &mut String, data: &str) -> Result<(), DecompileError> {
    let tokens = keyvalue_tokens(data)?;
    if tokens.first().map(String::as_str) != Some("mdlkeyvalue")
        || tokens.get(1).map(String::as_str) != Some("{")
        || tokens.last().map(String::as_str) != Some("}")
    {
        return Err(invalid("Model keyvalues lack their expected root block"));
    }
    let mut depth = 0usize;
    let mut text = String::from("$keyvalues\n{\n");
    for token in &tokens[2..tokens.len() - 1] {
        match token.as_str() {
            "{" => {
                depth += 1;
                if depth > 64 {
                    return Err(DecompileError::SizeLimit);
                }
                text.push_str("{\n");
            }
            "}" => {
                depth = depth
                    .checked_sub(1)
                    .ok_or_else(|| invalid("Unbalanced model keyvalues"))?;
                text.push_str("}\n");
            }
            value => {
                if value.chars().any(char::is_control) {
                    return Err(invalid(
                        "Model keyvalues cannot be represented safely in QC",
                    ));
                }
                write!(text, "\"{value}\" ").unwrap();
            }
        }
    }
    if depth != 0 {
        return Err(invalid("Unbalanced model keyvalues"));
    }
    text.push_str("\n}\n");
    output.push_str(&text);
    Ok(())
}

pub(super) fn keyvalue_tokens(data: &str) -> Result<Vec<String>, DecompileError> {
    let mut chars = data.chars().peekable();
    let mut tokens = Vec::new();
    while let Some(character) = chars.next() {
        if character.is_whitespace() {
            continue;
        }
        if character == '/' && chars.peek() == Some(&'/') {
            for ch in chars.by_ref() {
                if ch == '\n' {
                    break;
                }
            }
            continue;
        }
        let token = match character {
            '{' | '}' => character.to_string(),
            '"' => {
                let mut token = String::new();
                let mut closed = false;
                for ch in chars.by_ref() {
                    if ch == '"' {
                        closed = true;
                        break;
                    }
                    token.push(ch);
                }
                if !closed || token.contains(['{', '}']) {
                    return Err(invalid("Unsupported quoted model keyvalue"));
                }
                token
            }
            ch => {
                let mut token = ch.to_string();
                while let Some(&next) = chars.peek() {
                    if next.is_whitespace() || matches!(next, '{' | '}' | '"') {
                        break;
                    }
                    token.push(chars.next().unwrap());
                }
                token
            }
        };
        tokens.push(token);
        if tokens.len() > MAX_ENTRIES {
            return Err(DecompileError::SizeLimit);
        }
    }
    Ok(tokens)
}

#[cfg(test)]
#[path = "metadata/tests.rs"]
mod tests;
