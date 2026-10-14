use std::sync::atomic::AtomicBool;

use serde_json::{Value, json};

use super::super::{LoadedModel, u32_at};
use super::{DecompileError, Document, check_cancelled, flexes::Reader};

pub(super) fn write(
    output: &mut Document,
    loaded: &LoadedModel,
    cancelled: &AtomicBool,
) -> Result<(), DecompileError> {
    let raw = &loaded.raw_mdl;
    let reader = Reader(raw);
    output.extras["sourceBounds"] = json!({
        "eyePosition": vector(&reader, 80)?,
        "illuminationPosition": vector(&reader, 92)?,
        "hullMinimum": vector(&reader, 104)?,
        "hullMaximum": vector(&reader, 116)?,
        "viewMinimum": vector(&reader, 128)?,
        "viewMaximum": vector(&reader, 140)?,
    });
    let mut procedural = false;
    let bones = reader
        .limited_table(0, 156, 160, 216, 65536)?
        .into_iter()
        .enumerate()
        .map(|(index, record)| {
            check_cancelled(cancelled)?;
            let kind = reader.count(record + 164)?;
            procedural |= kind != 0;
            Ok(json!({
                "bone": index,
                "flags": u32::from_le_bytes(reader.bytes(record + 160)?),
                "proceduralType": kind,
                "physicsBone": i32::from_le_bytes(reader.bytes(record + 172)?),
                "surfaceProperty": optional_string(&reader, record, 176)?,
                "contents": u32::from_le_bytes(reader.bytes(record + 180)?),
            }))
        })
        .collect::<Result<Vec<_>, DecompileError>>()?;
    output.extras["sourceBoneMetadata"] = json!(bones);
    let attachments = reader
        .limited_table(0, 240, 244, 92, 4096)?
        .into_iter()
        .map(|record| {
            check_cancelled(cancelled)?;
            let matrix = (0..12)
                .map(|index| reader.float(record + 12 + index * 4))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(json!({
                "name": reader.string(record, 0)?,
                "flags": u32::from_le_bytes(reader.bytes(record + 4)?),
                "bone": reader.count(record + 8)?,
                "localMatrix": matrix,
            }))
        })
        .collect::<Result<Vec<_>, DecompileError>>()?;
    output.extras["sourceAttachments"] = json!(attachments);
    let mut box_count = 0usize;
    let hitboxes = reader
        .limited_table(0, 172, 176, 12, 4096)?
        .into_iter()
        .map(|record| {
            check_cancelled(cancelled)?;
            let records =
                reader.limited_table(record, 4, 8, 68, 65536usize.saturating_sub(box_count))?;
            box_count += records.len();
            let boxes = records
                .into_iter()
                .map(|record| {
                    Ok(json!({
                        "name": optional_string(&reader, record, 32)?,
                        "bone": reader.count(record)?,
                        "group": i32::from_le_bytes(reader.bytes(record + 4)?),
                        "minimum": vector(&reader, record + 8)?,
                        "maximum": vector(&reader, record + 20)?,
                    }))
                })
                .collect::<Result<Vec<_>, DecompileError>>()?;
            Ok(json!({"name":reader.string(record,0)?,"boxes":boxes}))
        })
        .collect::<Result<Vec<_>, DecompileError>>()?;
    output.extras["sourceHitboxSets"] = json!(hitboxes);
    let parameters = reader
        .limited_table(0, 300, 304, 20, 4096)?
        .into_iter()
        .map(|record| {
            Ok(json!({
                "name": reader.string(record, 0)?,
                "flags": u32::from_le_bytes(reader.bytes(record + 4)?),
                "start": reader.float(record + 8)?,
                "end": reader.float(record + 12)?,
                "loop": reader.float(record + 16)?,
            }))
        })
        .collect::<Result<Vec<_>, DecompileError>>()?;
    output.extras["sourcePoseParameters"] = json!(parameters);
    let included = reader
        .limited_table(0, 336, 340, 8, 4096)?
        .into_iter()
        .map(|record| reader.string(record, 4))
        .collect::<Result<Vec<_>, _>>()?;
    output.extras["sourceIncludedModels"] = json!(included);
    if procedural {
        output.warnings.push("Procedural and jiggle bones are exported in their bind pose. Source procedural simulation is not a Blender constraint rig.".into());
    }
    if u32_at(raw, 284).unwrap_or(0) > 0
        || u32_at(raw, 164).unwrap_or(0) > 0
        || u32_at(raw, 300).unwrap_or(0) > 0
    {
        output.warnings.push("Source IK, bone controllers, and pose-parameter blending are not recreated as Blender constraints. Animation clips remain editable.".into());
    }
    if u32_at(raw, 336).unwrap_or(0) > 0 {
        output.warnings.push("Included models are referenced in custom data; their external geometry and animations are not merged into this file.".into());
    }
    let mut has_eyes = false;
    for body in reader.limited_table(0, 232, 236, 16, 65536)? {
        for model in reader.limited_table(body, 4, 12, 148, 65536)? {
            has_eyes |= reader.count(model + 100)? > 0;
        }
    }
    if has_eyes {
        output.warnings.push(
            "Source eyeball tracking and eyelid behavior are not recreated as a Blender eye rig."
                .into(),
        );
    }
    Ok(())
}

fn optional_string(
    reader: &Reader<'_>,
    base: usize,
    field: usize,
) -> Result<String, DecompileError> {
    if i32::from_le_bytes(reader.bytes(base + field)?) == 0 {
        Ok(String::new())
    } else {
        match reader.string(base, field) {
            Ok(name) => Ok(name),
            Err(error) => {
                let version = i32::from_le_bytes(reader.bytes(4)?);
                let absolute =
                    usize::try_from(i32::from_le_bytes(reader.bytes(base + field)?)).ok();
                if version == 44 && absolute.is_some_and(|offset| reader.0.get(offset) == Some(&0))
                {
                    Ok(String::new())
                } else {
                    Err(error)
                }
            }
        }
    }
}

fn vector(reader: &Reader<'_>, offset: usize) -> Result<Value, DecompileError> {
    Ok(json!([
        reader.float(offset)?,
        reader.float(offset + 4)?,
        reader.float(offset + 8)?
    ]))
}
