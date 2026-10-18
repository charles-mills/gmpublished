use crate::model::{Attachment, CompiledModelDesc, STATIC_PROP_BONE};

pub(crate) fn apply(compiled: &mut CompiledModelDesc) -> Result<(), String> {
    if !compiled.desc.model.autocenter {
        return Ok(());
    }
    if !compiled.desc.model.static_prop {
        return Err("$autocenter requires $staticprop".into());
    }
    if compiled
        .desc
        .attachments
        .iter()
        .any(|attachment| attachment.name.eq_ignore_ascii_case("placementOrigin"))
    {
        return Ok(());
    }
    let first_source = compiled.desc.model.first_source.as_deref();
    let source = compiled
        .desc
        .bodyparts
        .iter()
        .zip(&compiled.bodyparts)
        .flat_map(|(description, compiled)| description.models.iter().zip(&compiled.models))
        .find(|(description, compiled)| {
            !compiled.meshes.is_empty()
                && first_source.is_none_or(|source| source == description.smd)
        })
        .map(|(_, model)| model)
        .ok_or("$autocenter requires the first loaded source to be a model with geometry")?;
    let mut min = [f32::INFINITY; 3];
    let mut max = [f32::NEG_INFINITY; 3];
    for vertex in source.meshes.iter().flat_map(|mesh| &mesh.vertices) {
        for axis in 0..3 {
            min[axis] = min[axis].min(vertex.pos[axis]);
            max[axis] = max[axis].max(vertex.pos[axis]);
        }
    }
    let offset = std::array::from_fn(|axis| -(min[axis] + max[axis]) * 0.5);
    if offset.iter().any(|value: &f32| !value.is_finite()) {
        return Err("$autocenter requires finite source bounds".into());
    }
    for model in compiled
        .bodyparts
        .iter_mut()
        .flat_map(|part| &mut part.models)
    {
        for vertex in model.meshes.iter_mut().flat_map(|mesh| &mut mesh.vertices) {
            for (position, offset) in vertex.pos.iter_mut().zip(offset) {
                *position += offset;
            }
        }
        if let Some(lods) = &mut model.lods {
            for vertex in lods.meshes.iter_mut().flat_map(|mesh| &mut mesh.vertices) {
                for (position, offset) in vertex.pos.iter_mut().zip(offset) {
                    *position += offset;
                }
            }
        }
    }
    compiled.desc.model.autocenter_offset = Some(offset);
    compiled.desc.attachments.push(Attachment {
        name: "placementOrigin".into(),
        bone: STATIC_PROP_BONE.into(),
        position: Some(offset),
        rotation: Some([0.0; 3]),
        absolute: false,
        absolute_rotation: None,
        rigid: false,
        flags: Some(0),
        synthetic: false,
        resolved: None,
    });
    Ok(())
}

#[cfg(test)]
#[path = "source_transform_tests.rs"]
mod tests;
