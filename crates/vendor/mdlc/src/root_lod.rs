use crate::{pipeline::PipelineOutput, vvd::Vvd};

type Result<T> = std::result::Result<T, String>;

fn i32_at(bytes: &[u8], offset: usize) -> Result<i32> {
    let value = bytes
        .get(offset..offset.checked_add(4).ok_or("LOD offset overflow")?)
        .ok_or("Truncated LOD record")?;
    Ok(i32::from_le_bytes(value.try_into().unwrap()))
}

fn count(bytes: &[u8], offset: usize) -> Result<usize> {
    usize::try_from(i32_at(bytes, offset)?).map_err(|_| "Negative LOD count".into())
}

fn relative(bytes: &[u8], base: usize, field: usize) -> Result<usize> {
    base.checked_add_signed(i32_at(bytes, base + field)? as isize)
        .filter(|&offset| offset <= bytes.len())
        .ok_or_else(|| "LOD offset is outside the file".into())
}

fn put(bytes: &mut [u8], offset: usize, value: usize) -> Result<()> {
    let value = i32::try_from(value).map_err(|_| "LOD value exceeds the file format")?;
    bytes
        .get_mut(offset..offset + 4)
        .ok_or("Truncated LOD record")?
        .copy_from_slice(&value.to_le_bytes());
    Ok(())
}

fn link(bytes: &mut [u8], base: usize, field: usize, target: usize) -> Result<()> {
    let delta = i32::try_from(target as i64 - base as i64)
        .map_err(|_| "LOD relative offset exceeds the file format")?;
    bytes
        .get_mut(base + field..base + field + 4)
        .ok_or("Truncated LOD record")?
        .copy_from_slice(&delta.to_le_bytes());
    Ok(())
}

fn copy_records(
    source: &[u8],
    target: &mut Vec<u8>,
    offset: usize,
    count: usize,
    size: usize,
) -> Result<usize> {
    let length = count.checked_mul(size).ok_or("LOD array size overflow")?;
    let end = offset
        .checked_add(length)
        .ok_or("LOD array offset overflow")?;
    let records = source.get(offset..end).ok_or("Truncated LOD array")?;
    let result = target.len();
    target.extend_from_slice(records);
    Ok(result)
}

fn copy_array(
    source: &[u8],
    target: &mut Vec<u8>,
    old: usize,
    new: usize,
    count_field: usize,
    offset_field: usize,
    size: usize,
) -> Result<(usize, usize, usize)> {
    let length = count(source, old + count_field)?;
    let start = if length == 0 {
        0
    } else {
        relative(source, old, offset_field)?
    };
    let copied = copy_records(source, target, start, length, size)?;
    link(target, new, offset_field, copied)?;
    Ok((start, copied, length))
}

fn strip_vtx(source: &[u8], root: usize) -> Result<Vec<u8>> {
    let mut target = source.get(..36).ok_or("Truncated VTX header")?.to_vec();
    let (old_lists, new_lists, levels) = copy_array(source, &mut target, 0, 0, 20, 24, 8)?;
    for level in root..levels {
        let old = old_lists + level * 8;
        let new = new_lists + level * 8;
        let (old_records, new_records, length) =
            copy_array(source, &mut target, old, new, 0, 4, 6)?;
        for index in 0..length {
            let old_record = old_records + index * 6;
            let new_record = new_records + index * 6;
            let string = relative(source, old_record, 2)?;
            let end = source[string..]
                .iter()
                .position(|&byte| byte == 0)
                .ok_or("Unterminated LOD material name")?;
            let copied = copy_records(source, &mut target, string, end + 1, 1)?;
            link(&mut target, new_record, 2, copied)?;
        }
    }
    let root_list = new_lists + root * 8;
    for level in 0..root {
        let new = new_lists + level * 8;
        let length = count(&target, root_list)?;
        let records = relative(&target, root_list, 4)?;
        put(&mut target, new, length)?;
        link(&mut target, new, 4, records)?;
    }
    let (old_parts, new_parts, parts) = copy_array(source, &mut target, 0, 0, 28, 32, 8)?;
    for part in 0..parts {
        let (old_models, new_models, models) = copy_array(
            source,
            &mut target,
            old_parts + part * 8,
            new_parts + part * 8,
            0,
            4,
            8,
        )?;
        for model in 0..models {
            let (old_lods, new_lods, lods) = copy_array(
                source,
                &mut target,
                old_models + model * 8,
                new_models + model * 8,
                0,
                4,
                12,
            )?;
            if lods != levels {
                return Err("VTX model LOD count differs from its header".into());
            }
            for level in root..lods {
                let (old_meshes, new_meshes, meshes) = copy_array(
                    source,
                    &mut target,
                    old_lods + level * 12,
                    new_lods + level * 12,
                    0,
                    4,
                    9,
                )?;
                for mesh in 0..meshes {
                    let (old_groups, new_groups, groups) = copy_array(
                        source,
                        &mut target,
                        old_meshes + mesh * 9,
                        new_meshes + mesh * 9,
                        0,
                        4,
                        25,
                    )?;
                    for group in 0..groups {
                        let old = old_groups + group * 25;
                        let new = new_groups + group * 25;
                        copy_array(source, &mut target, old, new, 0, 4, 9)?;
                        copy_array(source, &mut target, old, new, 8, 12, 2)?;
                        let (old_strips, new_strips, strips) =
                            copy_array(source, &mut target, old, new, 16, 20, 27)?;
                        for strip in 0..strips {
                            copy_array(
                                source,
                                &mut target,
                                old_strips + strip * 27,
                                new_strips + strip * 27,
                                19,
                                23,
                                8,
                            )?;
                        }
                    }
                }
            }
            let root_lod = new_lods + root * 12;
            for level in 0..root {
                let new = new_lods + level * 12;
                let meshes = count(&target, root_lod)?;
                let records = relative(&target, root_lod, 4)?;
                put(&mut target, new, meshes)?;
                link(&mut target, new, 4, records)?;
            }
        }
    }
    Ok(target)
}

fn strip_mdl(bytes: &mut [u8], root: usize) -> Result<()> {
    let parts = count(bytes, 0xe8)?;
    let parts_offset = relative(bytes, 0, 0xec)?;
    let mut total_vertices = 0;
    for part in 0..parts {
        let record = parts_offset + part * 16;
        let models = count(bytes, record + 4)?;
        let model_offset = relative(bytes, record, 12)?;
        for model in 0..models {
            let record = model_offset + model * 148;
            let meshes = count(bytes, record + 72)?;
            let mesh_offset = relative(bytes, record, 76)?;
            let mut model_vertices = 0;
            for mesh in 0..meshes {
                let record = mesh_offset + mesh * 116;
                let vertices = count(bytes, record + 52 + root * 4)?;
                put(bytes, record + 8, vertices)?;
                put(bytes, record + 12, model_vertices)?;
                model_vertices += vertices;
                let flexes = count(bytes, record + 16)?;
                if flexes == 0 {
                    continue;
                }
                let flex_offset = relative(bytes, record, 20)?;
                for flex in 0..flexes {
                    let record = flex_offset + flex * 60;
                    let animations = count(bytes, record + 20)?;
                    let animation_offset = relative(bytes, record, 24)?;
                    let stride = match bytes.get(record + 32) {
                        Some(0) => 16,
                        Some(1) => 18,
                        _ => return Err("Unknown flex vertex animation format".into()),
                    };
                    let end = animation_offset
                        .checked_add(
                            animations
                                .checked_mul(stride)
                                .ok_or("Flex array overflow")?,
                        )
                        .ok_or("Flex array overflow")?;
                    if end > bytes.len() {
                        return Err("Truncated flex vertex animations".into());
                    }
                    let mut retained = 0;
                    for animation in 0..animations {
                        let offset = animation_offset + animation * stride;
                        let vertex =
                            u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap())
                                as usize;
                        if vertex < vertices {
                            bytes.copy_within(
                                offset..offset + stride,
                                animation_offset + retained * stride,
                            );
                            retained += 1;
                        }
                    }
                    put(bytes, record + 20, retained)?;
                }
            }
            put(bytes, record + 80, model_vertices)?;
            put(bytes, record + 84, total_vertices * 48)?;
            put(bytes, record + 88, total_vertices * 16)?;
            total_vertices += model_vertices;
        }
    }
    bytes[0x179] = root as u8;
    if bytes[0x17a] != 0 && bytes[0x17a] as usize <= root {
        bytes[0x17a] = root as u8 + 1;
    }
    Ok(())
}

pub(crate) fn apply(output: &mut PipelineOutput) -> Result<()> {
    let requested = output.compiled.desc.model.minimum_lod as usize;
    if requested == 0 {
        return Ok(());
    }
    let mut vvd = Vvd::parse(&output.vvd).map_err(|error| error.to_string())?;
    let levels = usize::try_from(vvd.header.num_lods).map_err(|_| "Invalid VVD LOD count")?;
    if !(1..=8).contains(&levels) || count(&output.vtx, 20)? != levels {
        return Err("MDL/VVD/VTX LOD counts disagree".into());
    }
    let root = requested.min(levels - 1);
    if root == 0 {
        return Ok(());
    }
    let retained = usize::try_from(vvd.header.num_lod_vertexes[root])
        .map_err(|_| "Negative root LOD vertex count")?;
    vvd.vertices.truncate(retained);
    vvd.tangents.truncate(retained);
    vvd.header.num_lod_vertexes[..root].fill(retained as i32);
    // Retain surviving fixups so later root LOD loads still reorder every mesh correctly.
    vvd.fixups.retain(|fixup| fixup.lod >= root as i32);
    vvd.header.num_fixups = vvd.fixups.len() as i32;
    vvd.recompute_offsets();
    let vvd_bytes = vvd.to_bytes().map_err(|error| error.to_string())?;
    crate::vvd::check_invariants(&vvd, vvd_bytes.len()).map_err(|error| error.to_string())?;
    let vtx = strip_vtx(&output.vtx, root)?;
    strip_mdl(&mut output.mdl, root)?;
    output.vvd = vvd_bytes;
    output.vtx = vtx;
    Ok(())
}

#[cfg(test)]
#[path = "root_lod_tests.rs"]
mod tests;
