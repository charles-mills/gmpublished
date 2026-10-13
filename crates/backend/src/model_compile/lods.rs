use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use mdlc::model::{LodModel, ModelDesc};
use mdlc::smd::Smd;

use super::{ModelCompileError, ModelCompileRequest};

#[derive(Clone, Debug, PartialEq)]
pub struct ModelLod {
    pub source: PathBuf,
    pub switch_point: f32,
}

pub fn validate_lods(lods: &[ModelLod], bodygroups: bool) -> Result<(), ModelCompileError> {
    if lods.len() >= mdlc::vvd::MAX_NUM_LODS {
        return Err(invalid("use at most seven additional LOD meshes"));
    }
    if bodygroups && !lods.is_empty() {
        return Err(invalid(
            "external LOD authoring cannot be combined with bodygroups; define per-choice replacements in QC",
        ));
    }
    let mut previous = 0.0;
    for lod in lods {
        if !lod.switch_point.is_finite() || lod.switch_point <= previous {
            return Err(invalid(
                "LOD switch thresholds must be positive, finite and strictly increasing",
            ));
        }
        if !lod.source.extension().is_some_and(|extension| {
            ["smd", "fbx", "gltf", "glb"]
                .iter()
                .any(|allowed| extension.eq_ignore_ascii_case(allowed))
        }) {
            return Err(invalid("LOD meshes must be SMD, FBX, glTF or GLB files"));
        }
        previous = lod.switch_point;
    }
    Ok(())
}

pub(super) fn apply(
    desc: &mut ModelDesc,
    request: &ModelCompileRequest,
    source: &Path,
) -> Result<(), ModelCompileError> {
    if request.lods.is_empty() {
        return Ok(());
    }
    let model = &mut desc.bodyparts[0].models[0];
    let options =
        mdlc::compile::SrcOpts::of_model(model).map_err(|error| invalid(&error.to_string()))?;
    let reference = if request.static_prop {
        None
    } else {
        Some(
            mdlc::compile::read_source(source, "LOD reference skeleton", &options)
                .map_err(|error| invalid(&error.to_string()))?,
        )
    };
    for (index, lod) in request.lods.iter().enumerate() {
        let path = std::fs::canonicalize(&lod.source).map_err(|source| ModelCompileError::Io {
            path: lod.source.clone(),
            source,
        })?;
        let geometry = mdlc::compile::read_source(&path, "LOD geometry", &options)
            .map_err(|error| invalid(&error.to_string()))?;
        if let Some(reference) = &reference {
            compatible_skeleton(reference, &geometry)
                .map_err(|error| invalid(&format!("LOD {}: {error}", index + 1)))?;
        } else {
            valid_weights(&geometry)?;
        }
        model.lods.push(LodModel {
            smd: Some(
                path.to_str()
                    .ok_or_else(|| invalid("LOD filename must be UTF-8"))?
                    .into(),
            ),
            switch_point: Some(lod.switch_point),
            reverse: false,
            removed: false,
            removed_materials: Vec::new(),
            material_replacements: Vec::new(),
            bone_tree_collapse: Vec::new(),
            replace_bone: Vec::new(),
            no_facial: true,
        });
    }
    Ok(())
}

fn compatible_skeleton(reference: &Smd, geometry: &Smd) -> Result<(), ModelCompileError> {
    valid_weights(geometry)?;
    let names: HashMap<_, _> = reference
        .nodes
        .iter()
        .map(|node| (node.name.as_str(), node))
        .collect();
    let indices: HashMap<_, _> = geometry
        .nodes
        .iter()
        .map(|node| (node.index, node))
        .collect();
    let base_indices: HashMap<_, _> = reference
        .nodes
        .iter()
        .map(|node| (node.index, node.name.as_str()))
        .collect();
    let reference_poses: HashMap<_, _> = reference
        .frames
        .first()
        .into_iter()
        .flat_map(|frame| &frame.poses)
        .map(|pose| (pose.bone, pose))
        .collect();
    let poses: HashMap<_, _> = geometry
        .frames
        .first()
        .into_iter()
        .flat_map(|frame| &frame.poses)
        .map(|pose| (pose.bone, pose))
        .collect();
    if names.len() != reference.nodes.len() || indices.len() != geometry.nodes.len() {
        return Err(invalid(
            "LOD skeleton bone names and indices must be unique",
        ));
    }
    let mut checked = HashSet::new();
    for link in geometry
        .triangles
        .iter()
        .flat_map(|triangle| &triangle.vertices)
        .flat_map(|vertex| &vertex.links)
        .filter(|link| link.weight > 0.0)
    {
        let mut bone = link.bone;
        let mut chain = HashSet::new();
        while bone >= 0 {
            if !chain.insert(bone) {
                return Err(invalid("LOD skeleton contains a cycle"));
            }
            if !checked.insert(bone) {
                break;
            }
            let node = indices
                .get(&bone)
                .ok_or_else(|| invalid("LOD vertex references an absent bone"))?;
            let base = names.get(node.name.as_str()).ok_or_else(|| {
                invalid(&format!(
                    "weighted bone '{}' is absent from the reference skeleton",
                    node.name
                ))
            })?;
            let parent = indices.get(&node.parent).map(|node| node.name.as_str());
            let base_parent = base_indices.get(&base.parent).copied();
            if parent != base_parent || (node.parent >= 0 && parent.is_none()) {
                return Err(invalid(&format!(
                    "bone '{}' has a different parent",
                    node.name
                )));
            }
            let pose = poses
                .get(&bone)
                .ok_or_else(|| invalid("LOD reference pose is missing"))?;
            let base_pose = reference_poses
                .get(&base.index)
                .ok_or_else(|| invalid("reference skeleton pose is missing"))?;
            if pose
                .position
                .iter()
                .chain(&pose.rotation)
                .chain(&base_pose.position)
                .chain(&base_pose.rotation)
                .any(|value| !value.is_finite())
            {
                return Err(invalid("LOD bind poses must be finite"));
            }
            let rotation = mdlc::bone_math::angle_quaternion(pose.rotation);
            let base_rotation = mdlc::bone_math::angle_quaternion(base_pose.rotation);
            let dot = rotation
                .into_iter()
                .zip(base_rotation)
                .map(|(a, b)| a * b)
                .sum::<f32>()
                .abs();
            if pose
                .position
                .iter()
                .zip(base_pose.position)
                .any(|(a, b)| (a - b).abs() > 0.0001)
                || (1.0 - dot).abs() > 0.00001
            {
                return Err(invalid(&format!(
                    "bone '{}' has a different bind pose",
                    node.name
                )));
            }
            bone = node.parent;
        }
    }
    Ok(())
}

fn valid_weights(geometry: &Smd) -> Result<(), ModelCompileError> {
    let nodes: HashSet<_> = geometry.nodes.iter().map(|node| node.index).collect();
    for vertex in geometry
        .triangles
        .iter()
        .flat_map(|triangle| &triangle.vertices)
    {
        let sum = vertex.links.iter().map(|link| link.weight).sum::<f32>();
        if vertex.links.is_empty()
            || vertex.links.iter().any(|link| {
                !link.weight.is_finite()
                    || link.weight < 0.0
                    || (link.weight > 0.0 && !nodes.contains(&link.bone))
            })
            || !sum.is_finite()
            || sum <= 0.0
        {
            return Err(invalid(
                "LOD vertices require finite positive weights bound to existing bones",
            ));
        }
        let normal = vertex.normal.iter().map(|value| value * value).sum::<f32>();
        if vertex
            .position
            .iter()
            .chain(&vertex.normal)
            .chain(&vertex.uv)
            .any(|value| !value.is_finite())
            || !normal.is_finite()
            || normal <= f32::EPSILON
        {
            return Err(invalid(
                "LOD vertices require finite positions, UVs and usable normals",
            ));
        }
    }
    Ok(())
}

fn invalid(message: &str) -> ModelCompileError {
    ModelCompileError::InvalidOption(message.into())
}

#[cfg(test)]
#[path = "lods_tests.rs"]
pub(super) mod tests;
