use std::collections::{HashMap, HashSet};
use std::path::Path;

use mdlc::fbx::SourcePart;
use mdlc::model::{BodyPart, ModelDesc};

use super::{ModelCompileError, ModelCompileRequest};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelBodygroup {
    pub name: String,
    pub choices: Vec<BodygroupChoice>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BodygroupChoice {
    Hidden,
    Mesh { name: String, parts: Vec<String> },
}

pub(super) fn source_parts(source: &Path) -> Result<Vec<SourcePart>, ModelCompileError> {
    let extension = source.extension().unwrap_or_default().to_string_lossy();
    if extension.eq_ignore_ascii_case("dmx") {
        return mdlc::dmx::mesh_names(source)
            .map(|parts| {
                parts
                    .into_iter()
                    .map(|name| SourcePart {
                        selector: name.clone(),
                        name,
                    })
                    .collect()
            })
            .map_err(ModelCompileError::Description);
    }
    let result = if extension.eq_ignore_ascii_case("fbx") {
        mdlc::fbx::source_parts(source, "model parts")
    } else if extension.eq_ignore_ascii_case("gltf") || extension.eq_ignore_ascii_case("glb") {
        mdlc::gltf::source_parts(source, "model parts")
    } else {
        return Ok(Vec::new());
    };
    result.map_err(|error| ModelCompileError::Description(error.to_string()))
}

pub fn validate_bodygroups(
    groups: &[ModelBodygroup],
    available: &[SourcePart],
) -> Result<(), ModelCompileError> {
    let invalid = |message: &str| ModelCompileError::InvalidOption(message.into());
    let label_valid = |label: &str| {
        !label.is_empty()
            && label.len() <= 63
            && label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    };
    if groups.len() > 32 {
        return Err(invalid("use at most 32 bodygroups"));
    }
    let available: HashSet<_> = available
        .iter()
        .map(|part| part.selector.as_str())
        .collect();
    let mut names = HashSet::from(["body".to_owned()]);
    let mut owners = HashMap::new();
    let mut combinations = 1_i32;
    for (index, group) in groups.iter().enumerate() {
        if !label_valid(&group.name) || !names.insert(group.name.to_ascii_lowercase()) {
            return Err(invalid(
                "bodygroup names must be unique ASCII labels of 1–63 characters; 'body' is reserved",
            ));
        }
        if group.choices.is_empty() || group.choices.len() > 32 {
            return Err(invalid("each bodygroup needs 1–32 choices"));
        }
        combinations = combinations
            .checked_mul(group.choices.len() as i32)
            .ok_or_else(|| invalid("bodygroup combinations exceed Source's body value range"))?;
        let mut choice_names = HashSet::new();
        let mut hidden = false;
        for choice in &group.choices {
            let BodygroupChoice::Mesh { name, parts } = choice else {
                if std::mem::replace(&mut hidden, true) {
                    return Err(invalid("a bodygroup may contain only one hidden choice"));
                }
                continue;
            };
            if !label_valid(name)
                || name.eq_ignore_ascii_case("blank")
                || !choice_names.insert(name.to_ascii_lowercase())
            {
                return Err(invalid(
                    "bodygroup choice names must be unique ASCII labels of 1–63 characters; 'blank' is reserved",
                ));
            }
            if parts.is_empty() {
                return Err(invalid(
                    "select at least one mesh for each bodygroup choice",
                ));
            }
            let mut selected = HashSet::new();
            for part in parts {
                if !available.contains(part.as_str()) || !selected.insert(part) {
                    return Err(invalid(
                        "bodygroup meshes are missing or repeated; import the source again",
                    ));
                }
                if owners
                    .insert(part, index)
                    .is_some_and(|owner| owner != index)
                {
                    return Err(invalid("a mesh can belong to only one bodygroup"));
                }
            }
        }
    }
    Ok(())
}

pub(super) fn apply(
    desc: &mut ModelDesc,
    request: &ModelCompileRequest,
    source: &Path,
) -> Result<(), ModelCompileError> {
    if request.bodygroups.is_empty() {
        return Ok(());
    }
    let available = source_parts(source)?;
    if available.is_empty() {
        return Err(ModelCompileError::InvalidOption(
            "bodygroup authoring requires a DMX, FBX, glTF or GLB source with separate mesh objects"
                .into(),
        ));
    }
    validate_bodygroups(&request.bodygroups, &available)?;
    let selected: HashSet<_> = request
        .bodygroups
        .iter()
        .flat_map(|group| &group.choices)
        .filter_map(|choice| match choice {
            BodygroupChoice::Mesh { parts, .. } => Some(parts),
            BodygroupChoice::Hidden => None,
        })
        .flatten()
        .collect();
    let template = desc.bodyparts[0].models[0].clone();
    let remaining: Vec<_> = available
        .into_iter()
        .map(|part| part.selector)
        .filter(|selector| !selected.contains(selector))
        .collect();
    let mut bodies = Vec::new();
    if !remaining.is_empty() {
        let mut body = template.clone();
        body.src_parts = remaining;
        bodies.push(BodyPart {
            name: "body".into(),
            base: None,
            models: vec![body],
        });
    }
    for group in &request.bodygroups {
        let models = group
            .choices
            .iter()
            .map(|choice| {
                let mut model = template.clone();
                match choice {
                    BodygroupChoice::Hidden => {
                        model.name = Some("blank".into());
                        model.smd.clear();
                        model.src_parts.clear();
                    }
                    BodygroupChoice::Mesh { name, parts } => {
                        model.name = Some(name.clone());
                        model.src_parts.clone_from(parts);
                    }
                }
                model
            })
            .collect();
        bodies.push(BodyPart {
            name: group.name.clone(),
            base: None,
            models,
        });
    }
    desc.bodyparts = bodies;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model_compile::{ModelOutputKind, compile_model, inspect_model};

    fn mesh(name: &str, selector: &str) -> BodygroupChoice {
        BodygroupChoice::Mesh {
            name: name.into(),
            parts: vec![selector.into()],
        }
    }

    fn fixture() -> (tempfile::TempDir, ModelCompileRequest) {
        let root = tempfile::tempdir().unwrap();
        let mut geometry = Vec::new();
        for value in [
            0.0_f32, 0.0, 0.0, 2.0, 0.0, 0.0, 0.0, 3.0, 0.0, 0.0, 0.0, 4.0,
        ] {
            geometry.extend_from_slice(&value.to_le_bytes());
        }
        for index in [0_u16, 2, 1, 0, 1, 3, 0, 3, 2, 1, 2, 3] {
            geometry.extend_from_slice(&index.to_le_bytes());
        }
        let document = serde_json::json!({
            "asset": {"version": "2.0"},
            "buffers": [{"uri": format!("data:application/octet-stream;base64,{}",base64::encode(&geometry)),"byteLength":geometry.len()}],
            "bufferViews": [{"buffer":0,"byteOffset":0,"byteLength":48},{"buffer":0,"byteOffset":48,"byteLength":24}],
            "accessors": [{"bufferView":0,"componentType":5126,"count":4,"type":"VEC3","min":[0,0,0],"max":[2,3,4]}, {"bufferView":1,"componentType":5123,"count":12,"type":"SCALAR"}],
            "meshes": [{"name":"shared","primitives":[{"attributes":{"POSITION":0},"indices":1}]}],
            "nodes": [{"name":"Base","mesh":0},{"name":"Hat","mesh":0,"translation":[10,0,0]},{"name":"Hat","mesh":0,"translation":[100,0,0]}],
            "scenes": [{"nodes":[0,1,2]}],"scene":0
        });
        let source = root.path().join("prop.gltf");
        std::fs::write(&source, serde_json::to_vec(&document).unwrap()).unwrap();
        let parts = source_parts(&source).unwrap();
        let mut request = ModelCompileRequest::new(source, root.path().join("original"));
        request.export_materials = false;
        request.generate_collision = true;
        request.static_prop = true;
        request.bodygroups = vec![ModelBodygroup {
            name: "hat".into(),
            choices: vec![
                mesh("small", &parts[1].selector),
                mesh("large", &parts[2].selector),
                BodygroupChoice::Hidden,
            ],
        }];
        (root, request)
    }

    fn file(output: &super::super::ModelCompileOutput, kind: ModelOutputKind) -> Vec<u8> {
        std::fs::read(
            &output
                .files
                .iter()
                .find(|file| file.kind == kind)
                .unwrap()
                .path,
        )
        .unwrap()
    }

    #[test]
    fn duplicate_named_instances_become_distinct_choices_with_default_collision() {
        let (_root, request) = fixture();
        let inspection = inspect_model(&request.source).unwrap();
        for (index, part) in inspection.parts.iter().enumerate() {
            assert!(part.selector.starts_with(&format!("@node:{index}:")));
        }
        assert_eq!(inspection.parts[1].name, inspection.parts[2].name);
        let output = compile_model(&request).unwrap();
        let mdl = vformats::mdl::parse_mdl(
            &file(&output, ModelOutputKind::Mdl),
            &vformats::Limits::default(),
        )
        .unwrap();
        assert_eq!(mdl.body_parts.len(), 2);
        assert_eq!(
            mdl.body_parts[1]
                .models
                .iter()
                .map(|model| model.name.as_str())
                .collect::<Vec<_>>(),
            ["small", "large", "blank"]
        );
        assert_eq!(
            mdl.body_parts[1].models[0].vertex_offset - mdl.body_parts[0].models[0].vertex_offset,
            12
        );
        assert_eq!(
            mdl.body_parts[1].models[1].vertex_offset - mdl.body_parts[1].models[0].vertex_offset,
            12
        );
        assert!(mdl.body_parts[1].models[2].meshes.is_empty());
        let physics = file(&output, ModelOutputKind::Phy);
        let phy = vformats::phy::parse(&physics, &vformats::Limits::default()).unwrap();
        let points: Vec<_> = phy
            .solids
            .iter()
            .flat_map(|solid| &solid.ledges)
            .flat_map(|ledge| &ledge.vertices)
            .collect();
        assert!(
            points
                .iter()
                .any(|point| point.iter().any(|value| value.abs() > 9.9 / 0.0254))
        );
        assert!(
            points
                .iter()
                .all(|point| point.iter().all(|value| value.abs() < 20.0 / 0.0254))
        );
    }

    #[test]
    fn hidden_defaults_do_not_expand_collision_and_empty_defaults_fail_explicitly() {
        let (_root, mut request) = fixture();
        request.bodygroups[0]
            .choices
            .insert(0, BodygroupChoice::Hidden);
        request.bodygroups[0].choices.pop();
        let output = compile_model(&request).unwrap();
        let physics = file(&output, ModelOutputKind::Phy);
        let phy = vformats::phy::parse(&physics, &vformats::Limits::default()).unwrap();
        assert!(
            phy.solids
                .iter()
                .flat_map(|solid| &solid.ledges)
                .flat_map(|ledge| &ledge.vertices)
                .all(|point| point.iter().all(|value| value.abs() < 5.0 / 0.0254))
        );
        let BodygroupChoice::Mesh { parts, .. } = &mut request.bodygroups[0].choices[1] else {
            unreachable!()
        };
        parts.push(source_parts(&request.source).unwrap()[0].selector.clone());
        request.destination.push("empty");
        let error = compile_model(&request).unwrap_err().to_string();
        assert!(error.contains("default bodygroup choices"), "{error}");
    }

    #[test]
    fn portable_qc_preserves_choices_and_geometry_after_original_is_removed() {
        for static_prop in [false, true] {
            let (root, mut request) = fixture();
            request.static_prop = static_prop;
            request.generate_collision = static_prop;
            if static_prop {
                request.orientation = [17.0, -31.0, 22.0];
                request.scale = 1.7;
            }
            let original = compile_model(&request).unwrap();
            let original_geometry = super::super::lods::tests::compiled(&request);
            let qc = root.path().join("saved.qc");
            crate::model_project::save_model_qc(&request, &qc).unwrap();
            std::fs::remove_file(&request.source).unwrap();
            let mut saved = ModelCompileRequest::new(qc, root.path().join("saved"));
            saved.export_materials = false;
            let restored = compile_model(&saved).unwrap();
            super::super::lods::tests::assert_geometry(
                &original_geometry,
                &super::super::lods::tests::compiled(&saved),
            );
            let limits = vformats::Limits::default();
            let a =
                vformats::mdl::parse_mdl(&file(&original, ModelOutputKind::Mdl), &limits).unwrap();
            let b =
                vformats::mdl::parse_mdl(&file(&restored, ModelOutputKind::Mdl), &limits).unwrap();
            assert_eq!(a.body_parts, b.body_parts);
            let a =
                vformats::mdl::parse_vvd(&file(&original, ModelOutputKind::Vvd), &limits).unwrap();
            let b =
                vformats::mdl::parse_vvd(&file(&restored, ModelOutputKind::Vvd), &limits).unwrap();
            assert_eq!(a.vertices.len(), b.vertices.len());
            let mut remaining = b.vertices;
            for vertex in &a.vertices {
                let index = remaining
                    .iter()
                    .position(|other| {
                        vertex
                            .position
                            .into_iter()
                            .chain(vertex.normal)
                            .chain(vertex.uv)
                            .zip(
                                other
                                    .position
                                    .into_iter()
                                    .chain(other.normal)
                                    .chain(other.uv),
                            )
                            .all(|(a, b)| (a - b).abs() < 0.0001)
                            && vertex.bones == other.bones
                            && vertex.weights == other.weights
                    })
                    .expect("saved QC must preserve each weighted vertex");
                remaining.remove(index);
            }
            assert!(remaining.is_empty());
        }
    }

    #[test]
    fn source_object_changes_require_reimport_instead_of_rebinding_saved_indices() {
        let (_root, request) = fixture();
        let mut document: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&request.source).unwrap()).unwrap();
        document["nodes"][1]["name"] = "Replacement".into();
        std::fs::write(&request.source, serde_json::to_vec(&document).unwrap()).unwrap();
        let error = compile_model(&request).unwrap_err().to_string();
        assert!(error.contains("import the source again"), "{error}");
    }

    #[test]
    fn validation_rejects_missing_parts_conflicting_groups_and_body_value_overflow() {
        let (_root, request) = fixture();
        let parts = source_parts(&request.source).unwrap();
        let mut groups = request.bodygroups;
        groups.push(ModelBodygroup {
            name: "other".into(),
            choices: vec![mesh("copy", &parts[1].selector)],
        });
        assert!(validate_bodygroups(&groups, &parts).is_err());
        groups[1].choices = vec![mesh("missing", "@node:99")];
        assert!(validate_bodygroups(&groups, &parts).is_err());
        groups[1].choices = vec![mesh("base", &parts[0].selector)];
        assert!(validate_bodygroups(&groups, &parts).is_ok());
        groups[1].name = "HAT".into();
        assert!(validate_bodygroups(&groups, &parts).is_err());
        let overflow: Vec<_> = (0..31)
            .map(|index| ModelBodygroup {
                name: format!("group{index}"),
                choices: vec![
                    BodygroupChoice::Hidden,
                    mesh("part", &format!("@node:{index}")),
                ],
            })
            .collect();
        let available: Vec<_> = (0..31)
            .map(|index| SourcePart {
                selector: format!("@node:{index}"),
                name: format!("Part{index}"),
            })
            .collect();
        assert!(
            validate_bodygroups(&overflow, &available)
                .unwrap_err()
                .to_string()
                .contains("body value")
        );
    }
}
