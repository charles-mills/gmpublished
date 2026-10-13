use super::*;

pub(super) struct Metadata {
    pub switch_points: Vec<f32>,
    pub material_replacements: Vec<Vec<[String; 2]>>,
    pub shadow_materials: bool,
    pub no_facial: Vec<bool>,
    pub mesh_vertices: Vec<Vec<Vec<usize>>>,
}

impl Metadata {
    pub(super) fn read(
        raw_mdl: &[u8],
        raw_vvd: &[u8],
        raw_vtx: &[u8],
        model: &Mdl,
        vertices: &Vvd,
        topology: &mdl::Vtx,
    ) -> Result<Self, DecompileError> {
        let levels = integer(raw_vtx, 20)?;
        if !(1..=8).contains(&levels) || integer(raw_vvd, 12)? != levels {
            return Err(invalid(
                "The VVD and VTX LOD counts are invalid or do not match",
            ));
        }
        validate_fixups(raw_vvd, levels, vertices.vertices.len())?;
        if model.body_parts.len() != topology.body_parts.len() {
            return Err(invalid("The MDL and VTX bodygroups do not match"));
        }
        let body_offset = integer(raw_vtx, 32)?;
        let mdl_body_offset = integer(raw_mdl, 236)?;
        let shadow = model.flags & 0x40 != 0;
        let root_lod = raw_mdl
            .get(0x179)
            .copied()
            .ok_or_else(|| invalid("Truncated model root LOD settings"))?
            as usize;
        if root_lod >= levels {
            return Err(invalid("Model root LOD exceeds the available LOD levels"));
        }
        let material_replacements = read_material_replacements(raw_vtx, levels, root_lod, model)?;
        let mut switch_points = Vec::new();
        let mut mesh_vertices = Vec::new();
        let mut no_facial = vec![true; levels];
        let mut has_flexes = false;
        for (group, (part, vtx_part)) in model
            .body_parts
            .iter()
            .zip(&topology.body_parts)
            .enumerate()
        {
            if part.models.len() != vtx_part.models.len() {
                return Err(invalid("The MDL and VTX bodygroup choices do not match"));
            }
            let vtx_body = record(body_offset, group, 8)?;
            let models_offset = relative(raw_vtx, vtx_body, 4)?;
            let mdl_body = record(mdl_body_offset, group, 16)?;
            let mdl_models = relative(raw_mdl, mdl_body, 12)?;
            let mut choices = Vec::new();
            for (choice, (source, vtx)) in part.models.iter().zip(&vtx_part.models).enumerate() {
                if vtx.lods.len() != levels {
                    return Err(invalid("A bodygroup choice has an inconsistent LOD count"));
                }
                let vtx_model = record(models_offset, choice, 8)?;
                let lod_offset = relative(raw_vtx, vtx_model, 4)?;
                let mut points = Vec::with_capacity(levels);
                for (level, lod) in vtx.lods.iter().enumerate() {
                    if lod.meshes.len() != source.meshes.len() {
                        return Err(invalid("The MDL and VTX LOD mesh counts do not match"));
                    }
                    let lod_base = record(lod_offset, level, 12)?;
                    let meshes_base = relative(raw_vtx, lod_base, 4)?;
                    for (mesh_index, mesh) in lod.meshes.iter().enumerate() {
                        let mesh_base = record(meshes_base, mesh_index, 9)?;
                        let groups_base = relative(raw_vtx, mesh_base, 4)?;
                        for group_index in 0..mesh.strip_groups.len() {
                            let group_base = record(groups_base, group_index, 25)?;
                            let flags = raw_vtx
                                .get(group_base + 24)
                                .ok_or_else(|| invalid("Truncated VTX strip group flags"))?;
                            if flags & 5 != 0 {
                                no_facial[level] = false;
                            }
                        }
                    }
                    let offset = lod_base + 8;
                    let point = f32::from_bits(
                        u32_at(raw_vtx, offset)
                            .ok_or_else(|| invalid("Truncated LOD switch point"))?,
                    );
                    if !point.is_finite()
                        || (point < 0.0 && !(shadow && point == -1.0 && level == levels - 1))
                        || (shadow && level == levels - 1 && point != -1.0)
                        || (level == 0 && point != 0.0)
                    {
                        return Err(invalid(
                            "A LOD switch point is invalid or disagrees with the shadow LOD flag",
                        ));
                    }
                    points.push(point);
                }
                if switch_points.is_empty() {
                    switch_points = points;
                } else if switch_points != points {
                    return Err(invalid(
                        "Bodygroup choices have inconsistent LOD switch points",
                    ));
                }
                let mdl_model = record(mdl_models, choice, 148)?;
                let mesh_offset = relative(raw_mdl, mdl_model, 76)?;
                let model_vertices = integer(raw_mdl, mdl_model + 80)?;
                let model_start = usize::try_from(source.vertex_offset)
                    .map_err(|_| invalid("Negative model vertex offset"))?;
                let model_end = model_start
                    .checked_add(model_vertices)
                    .ok_or(DecompileError::SizeLimit)?;
                if model_end > vertices.vertices.len() {
                    return Err(invalid("An MDL model extends beyond the VVD vertex array"));
                }
                let mut counts = Vec::new();
                for (mesh_index, mesh) in source.meshes.iter().enumerate() {
                    let mesh_base = record(mesh_offset, mesh_index, 116)?;
                    let count = integer(raw_mdl, mesh_base + 8)?;
                    has_flexes |= integer(raw_mdl, mesh_base + 16)? > 0;
                    let start = usize::try_from(mesh.vertex_offset)
                        .map_err(|_| invalid("Negative mesh vertex offset"))?;
                    if start
                        .checked_add(count)
                        .is_none_or(|end| end > model_vertices)
                    {
                        return Err(invalid("An MDL mesh extends beyond its model vertex range"));
                    }
                    counts.push(count);
                }
                choices.push(counts);
            }
            mesh_vertices.push(choices);
        }
        if !has_flexes {
            no_facial.fill(false);
        }
        Ok(Self {
            no_facial,
            material_replacements,
            shadow_materials: model.flags & 0x100 != 0,
            switch_points,
            mesh_vertices,
        })
    }
}

pub(super) fn write(
    qc: &mut String,
    export: &mut ExportFiles<'_>,
    model: &Mdl,
    vertices: &Vvd,
    topology: &mdl::Vtx,
    metadata: &Metadata,
    cancelled: &AtomicBool,
) -> Result<(), DecompileError> {
    for (level, point) in metadata.switch_points.iter().enumerate().skip(1) {
        if *point == -1.0 {
            qc.push_str("$shadowlod\n{\n");
            if metadata.shadow_materials {
                qc.push_str("    use_shadowlod_materials\n");
            }
        } else {
            writeln!(qc, "$lod {point}\n{{").unwrap();
        }
        if metadata.no_facial[level] {
            qc.push_str("    nofacial\n");
        }
        for [source, target] in &metadata.material_replacements[level] {
            writeln!(
                qc,
                "    replacematerial {} {}",
                quoted(source)?,
                quoted(target)?
            )
            .unwrap();
        }
        for (group, (part, vtx_part)) in model
            .body_parts
            .iter()
            .zip(&topology.body_parts)
            .enumerate()
        {
            for (choice, (source, vtx)) in part.models.iter().zip(&vtx_part.models).enumerate() {
                check_cancelled(cancelled)?;
                if source.meshes.is_empty() {
                    continue;
                }
                if vtx.lods[level]
                    .meshes
                    .iter()
                    .flat_map(|mesh| &mesh.strip_groups)
                    .flat_map(|group| &group.strips)
                    .all(|strip| strip.index_count == 0)
                {
                    writeln!(qc, "    removemodel \"reference_{group}_{choice}.smd\"").unwrap();
                    continue;
                }
                let filename = format!("lod_{level}_{group}_{choice}.smd");
                // origMeshVertID addresses the mesh's LOD union. Loading root LOD 0
                // applies every VVD fixup and retains those indices for every LOD.
                let smd = reference_smd(
                    model,
                    source,
                    &vtx.lods[level],
                    vertices,
                    &metadata.mesh_vertices[group][choice],
                    cancelled,
                )?;
                export.write(&filename, smd.as_bytes())?;
                writeln!(
                    qc,
                    "    replacemodel \"reference_{group}_{choice}.smd\" \"{filename}\""
                )
                .unwrap();
            }
        }
        qc.push_str("}\n");
    }
    Ok(())
}

fn read_material_replacements(
    bytes: &[u8],
    levels: usize,
    root_lod: usize,
    model: &Mdl,
) -> Result<Vec<Vec<[String; 2]>>, DecompileError> {
    let mut output = vec![Vec::new(); levels];
    let base = integer(bytes, 24)?;
    if base == 0 {
        return Ok(output);
    }
    for (level, replacements) in output.iter_mut().enumerate() {
        let header = record(base, level, 8)?;
        let count = integer(bytes, header)?;
        if count > 1_000_000 {
            return Err(DecompileError::SizeLimit);
        }
        if count == 0 {
            continue;
        }
        let records = relative(bytes, header, 4)?;
        let mut seen = HashSet::new();
        for index in 0..count {
            let record = record(records, index, 6)?;
            let slot = bytes
                .get(record..record + 2)
                .map(|field| i16::from_le_bytes(field.try_into().unwrap()))
                .and_then(|slot| usize::try_from(slot).ok())
                .ok_or_else(|| invalid("Invalid LOD material replacement slot"))?;
            let source = model
                .textures
                .get(slot)
                .ok_or_else(|| invalid("A LOD replacement references an absent material"))?;
            if !seen.insert(slot) {
                return Err(invalid("Duplicate LOD material replacement slot"));
            }
            let offset = u32_at(bytes, record + 2)
                .ok_or_else(|| invalid("Truncated LOD material name offset"))?
                as i32;
            let start = usize::try_from(record as i64 + i64::from(offset))
                .map_err(|_| invalid("Invalid LOD material name offset"))?;
            let tail = bytes
                .get(start..)
                .ok_or_else(|| invalid("A LOD material name exceeds the VTX file"))?;
            let length = tail
                .iter()
                .position(|byte| *byte == 0)
                .ok_or_else(|| invalid("Unterminated LOD material name"))?;
            let target = std::str::from_utf8(&tail[..length])
                .map_err(|_| invalid("A LOD material name is not UTF-8"))?;
            validate_material(source)?;
            validate_material(target)?;
            if level == 0 && root_lod == 0 {
                return Err(invalid(
                    "LOD 0 material replacements cannot be represented in QC",
                ));
            }
            replacements.push([source.clone(), target.to_owned()]);
        }
    }
    if root_lod > 0
        && output[..root_lod]
            .iter()
            .any(|replacements| *replacements != output[root_lod])
    {
        return Err(invalid(
            "Stripped LOD material replacements do not match the minimum LOD",
        ));
    }
    Ok(output)
}

fn validate_fixups(
    bytes: &[u8],
    levels: usize,
    parsed_vertices: usize,
) -> Result<(), DecompileError> {
    let counts: Vec<_> = (0..levels)
        .map(|level| integer(bytes, 16 + level * 4))
        .collect::<Result<_, _>>()?;
    if counts[0] != parsed_vertices || counts.windows(2).any(|pair| pair[1] > pair[0]) {
        return Err(invalid("Invalid VVD LOD vertex counts"));
    }
    let fixup_count = integer(bytes, 48)?;
    if fixup_count == 0 {
        return Ok(());
    }
    let fixup_offset = integer(bytes, 52)?;
    let mut covered = vec![false; parsed_vertices];
    let mut per_lod = vec![0usize; levels];
    for index in 0..fixup_count {
        let base = record(fixup_offset, index, 12)?;
        let lod = integer(bytes, base)?;
        let source = integer(bytes, base + 4)?;
        let count = integer(bytes, base + 8)?;
        if lod >= levels {
            return Err(invalid("A VVD fixup references an absent LOD"));
        }
        let end = source.checked_add(count).ok_or(DecompileError::SizeLimit)?;
        let range = covered
            .get_mut(source..end)
            .ok_or_else(|| invalid("A VVD fixup exceeds the vertex array"))?;
        if range.iter().any(|covered| *covered) {
            return Err(invalid("VVD fixup vertex ranges overlap"));
        }
        range.fill(true);
        for total in per_lod.iter_mut().take(lod + 1) {
            *total = total.checked_add(count).ok_or(DecompileError::SizeLimit)?;
        }
    }
    if covered.iter().any(|covered| !covered) || per_lod != counts {
        return Err(invalid(
            "VVD fixups do not cover the declared LOD vertex arrays",
        ));
    }
    Ok(())
}

fn integer(bytes: &[u8], offset: usize) -> Result<usize, DecompileError> {
    let value =
        u32_at(bytes, offset).ok_or_else(|| invalid("Truncated model geometry metadata"))? as i32;
    usize::try_from(value).map_err(|_| {
        invalid(format!(
            "Negative model geometry count or offset at {offset}: {value}"
        ))
    })
}

fn relative(bytes: &[u8], base: usize, field: usize) -> Result<usize, DecompileError> {
    base.checked_add(integer(bytes, base + field)?)
        .ok_or(DecompileError::SizeLimit)
}

fn record(base: usize, index: usize, stride: usize) -> Result<usize, DecompileError> {
    index
        .checked_mul(stride)
        .and_then(|offset| base.checked_add(offset))
        .ok_or(DecompileError::SizeLimit)
}
