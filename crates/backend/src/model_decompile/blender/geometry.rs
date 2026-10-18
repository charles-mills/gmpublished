use std::sync::atomic::AtomicBool;

use serde_json::{Value, json};
use vformats::mdl::{self, Mdl};

use super::super::{LoadedModel, bodygroup_name, lods};
use super::{
    DecompileError, Document, check_cancelled, direction, flexes, invalid, position, rotation,
};

pub(super) fn write(
    output: &mut Document,
    loaded: &LoadedModel,
    lods: &lods::Metadata,
    cancelled: &AtomicBool,
) -> Result<(), DecompileError> {
    let model = &loaded.model;
    let flex_metadata = flexes::metadata(&loaded.raw_mdl)?;
    output.extras["sourceFacialSetup"] = flex_metadata;
    skeleton(output, model)?;
    let skinned = !output.skins.is_empty();
    let skin_count = if model.skin_reference_count == 0 {
        0
    } else {
        if !model
            .skin_table
            .len()
            .is_multiple_of(model.skin_reference_count)
        {
            return Err(invalid("The skin table has an incomplete family"));
        }
        model.skin_table.len() / model.skin_reference_count
    };
    if skin_count > 1 {
        output.variants = (0..skin_count)
            .map(|skin| json!({"name":format!("Skin {skin}")}))
            .collect();
    }
    let mut choices: Vec<Vec<Vec<Option<usize>>>> = Vec::new();
    let mut has_flexes = false;
    for (group, (part, vtx_part)) in model
        .body_parts
        .iter()
        .zip(&loaded.topology.body_parts)
        .enumerate()
    {
        let group_name =
            bodygroup_name(&loaded.raw_mdl, group).unwrap_or_else(|| format!("Bodygroup {group}"));
        let mut group_choices = Vec::new();
        for (choice, (source, topology)) in part.models.iter().zip(&vtx_part.models).enumerate() {
            check_cancelled(cancelled)?;
            let shapes = flexes::read(
                &loaded.raw_mdl,
                model,
                group,
                choice,
                &lods.mesh_vertices[group][choice],
                cancelled,
            )?;
            has_flexes |= !shapes.is_empty();
            let mut levels = Vec::new();
            for (level, lod) in topology.lods.iter().enumerate() {
                let mut primitives = Vec::new();
                for (mesh_index, (mesh, topology)) in
                    source.meshes.iter().zip(&lod.meshes).enumerate()
                {
                    check_cancelled(cancelled)?;
                    let count = lods.mesh_vertices[group][choice][mesh_index];
                    let mut indices = Vec::new();
                    for strip_group in &topology.strip_groups {
                        for strip in &strip_group.strips {
                            check_cancelled(cancelled)?;
                            if !matches!(strip.flags & 3, 1 | 2) {
                                return Err(invalid("Unsupported VTX strip topology"));
                            }
                            for (triangle_index, pool) in
                                strip.triangle_index_positions().enumerate()
                            {
                                if triangle_index.is_multiple_of(4096) {
                                    check_cancelled(cancelled)?;
                                }
                                let vertex = strip_group
                                    .indices
                                    .get(pool)
                                    .and_then(|index| strip_group.vertices.get(usize::from(*index)))
                                    .ok_or_else(|| {
                                        invalid("A triangle references an absent strip vertex")
                                    })?;
                                let index = usize::from(vertex.original_mesh_vertex_id);
                                if index >= count {
                                    return Err(invalid(
                                        "A triangle references a vertex outside its mesh",
                                    ));
                                }
                                if indices.len() >= super::MAX_OUTPUT_BYTES / 4 {
                                    return Err(DecompileError::SizeLimit);
                                }
                                indices.push(index as u32);
                            }
                        }
                    }
                    if !indices.len().is_multiple_of(3) {
                        return Err(invalid("A VTX triangle list is incomplete"));
                    }
                    if indices.is_empty() {
                        continue;
                    }
                    let mut source_vertices = Vec::new();
                    let mut remap = vec![u32::MAX; count];
                    for index in &mut indices {
                        let original = *index as usize;
                        if remap[original] == u32::MAX {
                            remap[original] = source_vertices.len() as u32;
                            source_vertices.push(original);
                        }
                        *index = remap[original];
                    }
                    // Blender discards repeated faces even when their winding differs.
                    let mut faces = std::collections::HashSet::new();
                    for (index, triangle) in indices.chunks_exact_mut(3).enumerate() {
                        if index.is_multiple_of(4096) {
                            check_cancelled(cancelled)?;
                        }
                        let mut key = [triangle[0], triangle[1], triangle[2]];
                        key.sort_unstable();
                        if !faces.insert(key) {
                            for index in triangle {
                                if source_vertices.len() >= super::MAX_OUTPUT_BYTES / 72 {
                                    return Err(DecompileError::SizeLimit);
                                }
                                let original = source_vertices[*index as usize];
                                *index = source_vertices.len() as u32;
                                source_vertices.push(original);
                            }
                        }
                    }
                    let start = i64::from(source.vertex_offset) + i64::from(mesh.vertex_offset);
                    let start = usize::try_from(start)
                        .map_err(|_| invalid("Negative model vertex offset"))?;
                    let end = start.checked_add(count).ok_or(DecompileError::SizeLimit)?;
                    let vertex_pool = loaded
                        .vertices
                        .vertices
                        .get(start..end)
                        .ok_or_else(|| invalid("Mesh vertices extend beyond the VVD"))?;
                    let vertices = source_vertices
                        .iter()
                        .map(|&index| &vertex_pool[index])
                        .collect::<Vec<_>>();
                    let positions = vertices
                        .iter()
                        .map(|vertex| position(vertex.position))
                        .collect::<Vec<_>>();
                    let normals = vertices
                        .iter()
                        .map(|vertex| direction(vertex.normal))
                        .collect::<Vec<_>>();
                    let uv = vertices.iter().map(|vertex| vertex.uv).collect::<Vec<_>>();
                    let mut attributes = json!({
                        "POSITION": output.floats(&positions,"VEC3",true,Some(34962))?,
                        "NORMAL": output.floats(&normals,"VEC3",false,Some(34962))?,
                        "TEXCOORD_0": output.floats(&uv,"VEC2",false,Some(34962))?,
                    });
                    if let Some(tangent_pool) = loaded.vertices.tangents.get(start..end) {
                        let tangents = source_vertices
                            .iter()
                            .map(|&index| tangent_pool[index])
                            .collect::<Vec<_>>();
                        if tangents.iter().all(|tangent| {
                            tangent.iter().all(|value| value.is_finite())
                                && (tangent[3].abs() - 1.0).abs() < 0.0001
                        }) {
                            let tangents = tangents
                                .iter()
                                .map(|tangent| {
                                    let [x, y, z] = direction([tangent[0], tangent[1], tangent[2]]);
                                    [x, y, z, tangent[3]]
                                })
                                .collect::<Vec<_>>();
                            attributes["TANGENT"] =
                                json!(output.floats(&tangents, "VEC4", false, Some(34962))?);
                        } else if !output
                            .warnings
                            .iter()
                            .any(|warning| warning.starts_with("Invalid tangent frames"))
                        {
                            output.warnings.push("Invalid tangent frames were omitted so Blender can recalculate them.".into());
                        }
                    }
                    if skinned {
                        let mut joints = Vec::with_capacity(vertices.len() * 8);
                        let mut weights = Vec::with_capacity(vertices.len());
                        for vertex in &vertices {
                            let count = usize::from(vertex.bone_count);
                            if !(1..=3).contains(&count) {
                                return Err(invalid("A skinned vertex has an invalid bone count"));
                            }
                            let mut value = [0.0f32; 4];
                            for (slot, weight) in value.iter_mut().enumerate() {
                                let joint = if slot < count {
                                    u16::from(vertex.bones[slot])
                                } else {
                                    0
                                };
                                if usize::from(joint) >= model.bones.len() {
                                    return Err(invalid("A vertex references an absent bone"));
                                }
                                joints.extend_from_slice(&joint.to_le_bytes());
                                if slot < count {
                                    *weight = vertex.weights[slot];
                                }
                            }
                            let sum = value.iter().sum::<f32>();
                            if value
                                .iter()
                                .any(|weight| !weight.is_finite() || *weight < 0.0)
                                || sum <= 0.0
                                || (sum - 1.0).abs() > 0.01
                            {
                                return Err(invalid("A vertex has invalid bone weights"));
                            }
                            weights.push(value.map(|weight| weight / sum));
                        }
                        attributes["JOINTS_0"] =
                            json!(output.integers(&joints, 5123, vertices.len(), "VEC4", 34962)?);
                        attributes["WEIGHTS_0"] =
                            json!(output.floats(&weights, "VEC4", false, Some(34962))?);
                    }
                    let index_bytes = indices
                        .iter()
                        .flat_map(|index| index.to_le_bytes())
                        .collect::<Vec<_>>();
                    let mut primitive = json!({"attributes":attributes,"indices":output.integers(&index_bytes,5125,indices.len(),"SCALAR",34963)?,"mode":4});
                    material(
                        output,
                        &mut primitive,
                        model,
                        mesh.material,
                        level,
                        lods,
                        skin_count,
                    )?;
                    if level == 0 && !shapes.is_empty() {
                        primitive["targets"] = json!(flexes::targets(
                            output,
                            &shapes,
                            mesh_index,
                            &source_vertices,
                            cancelled
                        )?);
                        primitive["extras"] =
                            json!({"sourceMesh":mesh_index,"sourceVertexIndices":source_vertices});
                    }
                    primitives.push(primitive);
                }
                if primitives.is_empty() {
                    levels.push(None);
                    continue;
                }
                let name = format!(
                    "{group_name} / {} / LOD {level}",
                    if source.name.is_empty() {
                        format!("Choice {choice}")
                    } else {
                        source.name.clone()
                    }
                );
                let mesh_index = output.meshes.len();
                let mut mesh = json!({"name":name,"primitives":primitives});
                if level == 0 && !shapes.is_empty() {
                    mesh["weights"] = json!(vec![0.0; shapes.len()]);
                    mesh["extras"] = json!({"targetNames":shapes.iter().map(|shape| &shape.name).collect::<Vec<_>>(),"sourceFlexes":shapes.iter().map(|shape| &shape.metadata).collect::<Vec<_>>()});
                }
                output.meshes.push(mesh);
                let node_index = output.nodes.len();
                let mut node = json!({"name":name,"mesh":mesh_index,"extras":{"sourceBodygroup":group,"sourceBodygroupName":group_name,"sourceChoice":choice,"sourceLOD":level,"sourceLODSwitch":lods.switch_points[level]}});
                if skinned {
                    node["skin"] = json!(0);
                }
                output.nodes.push(node);
                levels.push(Some(node_index));
            }
            group_choices.push(levels);
        }
        choices.push(group_choices);
    }
    let nodes_for = |level: usize, alternative: Option<(usize, usize)>| {
        let mut nodes = vec![0usize];
        for (group, choices) in choices.iter().enumerate() {
            let choice = alternative
                .filter(|(selected, _)| *selected == group)
                .map_or(0, |(_, choice)| choice);
            if let Some(node) = choices
                .get(choice)
                .and_then(|levels| levels.get(level))
                .copied()
                .flatten()
            {
                nodes.push(node);
            }
        }
        nodes
    };
    for level in 0..lods.switch_points.len() {
        let nodes = nodes_for(level, None);
        if level == 0 && nodes.len() == 1 && output.meshes.is_empty() {
            return Err(invalid("The model contains no exportable render geometry"));
        }
        output.scenes.push(json!({"name":if level == 0 { "Model".into() } else { format!("LOD {level}") },"nodes":nodes,"extras":{"sourceLOD":level}}));
    }
    for (group, choices) in choices.iter().enumerate() {
        let group_name =
            bodygroup_name(&loaded.raw_mdl, group).unwrap_or_else(|| format!("Bodygroup {group}"));
        for choice in 1..choices.len() {
            for level in 0..lods.switch_points.len() {
                if output.scenes.len() >= 4096 {
                    return Err(DecompileError::SizeLimit);
                }
                output.scenes.push(json!({"name":format!("{group_name} / Choice {choice} / LOD {level}"),"nodes":nodes_for(level,Some((group,choice))),"extras":{"sourceBodygroup":group,"sourceChoice":choice,"sourceLOD":level}}));
            }
        }
    }
    if choices.iter().any(|choices| choices.len() > 1) || lods.switch_points.len() > 1 {
        output.warnings.push("Bodygroup choices and LODs are exported as separate scenes, imported as collections in Blender. Enable the collection you want to edit.".into());
    }
    if has_flexes {
        output.warnings.push("Facial flexes are editable Blender shape keys. Source controller rules, activation ranges, vertex timing, and wrinkle values are preserved as custom data; their engine behavior is not a Blender facial rig.".into());
    }
    Ok(())
}

fn skeleton(output: &mut Document, model: &Mdl) -> Result<(), DecompileError> {
    let mut children = vec![Vec::new(); model.bones.len() + 1];
    for (index, bone) in model.bones.iter().enumerate() {
        let parent = if bone.parent < 0 {
            0
        } else {
            bone.parent as usize + 1
        };
        children[parent].push(index + 1);
    }
    let mut root = json!({"name":"Armature"});
    if !children[0].is_empty() {
        root["children"] = json!(children[0]);
    }
    output.nodes.push(root);
    for (index, bone) in model.bones.iter().enumerate() {
        let length = bone
            .quaternion
            .iter()
            .map(|value| value * value)
            .sum::<f32>()
            .sqrt();
        if !length.is_finite() || length <= 0.0 {
            return Err(invalid("A bone has an invalid bind quaternion"));
        }
        let mut node = json!({"name":bone.name,"translation":position(bone.position),"rotation":rotation(bone.quaternion.map(|value| value / length)),"extras":{"sourceBone":index}});
        if !children[index + 1].is_empty() {
            node["children"] = json!(children[index + 1]);
        }
        output.nodes.push(node);
    }
    if model.bones.is_empty() || model.flags & mdl::FLAG_STATIC_PROP != 0 {
        return Ok(());
    }
    let matrices = model
        .bones
        .iter()
        .map(|bone| {
            let map = [1, 2, 0];
            let mut matrix = [0.0f32; 16];
            for row in 0..3 {
                for col in 0..3 {
                    matrix[col * 4 + row] = bone.pose_to_bone[map[row]][map[col]];
                }
                matrix[12 + row] = bone.pose_to_bone[map[row]][3] * super::INCHES_TO_METERS;
            }
            matrix[15] = 1.0;
            matrix
        })
        .collect::<Vec<_>>();
    let accessor = output.floats(&matrices, "MAT4", false, None)?;
    output.skins.push(json!({"name":"Armature","inverseBindMatrices":accessor,"skeleton":0,"joints":(1..=model.bones.len()).collect::<Vec<_>>() }));
    Ok(())
}

fn material(
    output: &Document,
    primitive: &mut Value,
    model: &Mdl,
    slot: i32,
    level: usize,
    lods: &lods::Metadata,
    skin_count: usize,
) -> Result<(), DecompileError> {
    let slot = usize::try_from(slot).map_err(|_| invalid("Negative material slot"))?;
    let resolve = |skin: usize| -> Result<usize, DecompileError> {
        let index = if model.skin_reference_count == 0 {
            slot
        } else {
            if slot >= model.skin_reference_count {
                return Err(invalid("A material slot exceeds the skin table"));
            }
            usize::from(
                *model
                    .skin_table
                    .get(skin * model.skin_reference_count + slot)
                    .ok_or_else(|| invalid("The skin table is incomplete"))?,
            )
        };
        let name = model
            .textures
            .get(index)
            .ok_or_else(|| invalid("A mesh references an absent material"))?;
        let name = lods.material_replacements[level]
            .iter()
            .find(|pair| pair[0].eq_ignore_ascii_case(name))
            .map_or(name, |pair| &pair[1]);
        output
            .material_names
            .iter()
            .position(|material| material == name)
            .ok_or_else(|| invalid("A LOD references an absent material"))
    };
    primitive["material"] = json!(resolve(0)?);
    if skin_count > 1 {
        primitive["extensions"] = json!({"KHR_materials_variants":{"mappings":(0..skin_count).map(|skin| Ok(json!({"material":resolve(skin)?,"variants":[skin]}))).collect::<Result<Vec<_>,DecompileError>>()?}});
    }
    Ok(())
}
