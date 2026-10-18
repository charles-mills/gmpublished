use std::{collections::HashMap, fs, path::Path};

use mdlc::{compile::SrcOpts, fbx::ForcedAxis, model::ModelDesc};
use serde::{Deserialize, Serialize};

use super::{ModelCollision, ModelCompileError, ModelCompileRequest, bodygroups, lods};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SourceAuthoring {
    pub version: u32,
    pub description: ModelDesc,
    pub meshes: Vec<MeshBinding>,
    pub animations: Vec<AnimationBinding>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MeshBinding {
    pub bodygroup: usize,
    pub choice: usize,
    pub levels: Vec<Option<String>>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AnimationBinding {
    pub name: String,
    pub clip: String,
}

fn metadata(source: &Path) -> Result<Option<SourceAuthoring>, ModelCompileError> {
    if !source.extension().is_some_and(|extension| {
        extension.eq_ignore_ascii_case("gltf") || extension.eq_ignore_ascii_case("glb")
    }) {
        return Ok(None);
    }
    let mut found = None;
    for text in mdlc::gltf::source_metadata(source, "Source authoring settings")
        .map_err(|error| invalid(&error.to_string()))?
    {
        if text.len() > 8 * 1024 * 1024 {
            return Err(invalid("Source authoring settings exceed the 8 MiB limit"));
        }
        let value: serde_json::Value = serde_json::from_str(&text)
            .map_err(|error| invalid(&format!("Invalid Source authoring metadata: {error}")))?;
        if let Some(error) = value["sourceAuthoringError"].as_str() {
            return Err(invalid(&format!(
                "This Blender export cannot restore Source authoring settings: {error}"
            )));
        }
        let Some(value) = value.get("sourceAuthoring") else {
            continue;
        };
        let authoring: SourceAuthoring = serde_json::from_value(value.clone())
            .map_err(|error| invalid(&format!("Invalid Source authoring description: {error}")))?;
        if authoring.version != 1 {
            return Err(invalid("Unsupported Source authoring description version"));
        }
        if found.is_some() {
            return Err(invalid(
                "The source contains multiple Source authoring descriptions; export one model at a time",
            ));
        }
        found = Some(authoring);
    }
    Ok(found)
}

fn selectors(source: &Path) -> Result<HashMap<String, String>, ModelCompileError> {
    let mut selectors = HashMap::new();
    for part in bodygroups::source_parts(source)? {
        if selectors.insert(part.name.clone(), part.selector).is_some() {
            return Err(invalid(&format!(
                "Source mesh name {:?} is ambiguous; keep exported model object names unique",
                part.name
            )));
        }
    }
    Ok(selectors)
}

fn selector<'a>(
    selectors: &'a HashMap<String, String>,
    name: &str,
) -> Result<&'a str, ModelCompileError> {
    selectors.get(name).map(String::as_str).ok_or_else(|| invalid(&format!("Source mesh {name:?} is missing; include its Blender collection when exporting and preserve the object name")))
}

pub(super) fn restore(
    request: &ModelCompileRequest,
    source: &Path,
) -> Result<Option<ModelDesc>, ModelCompileError> {
    let Some(authoring) = metadata(source)? else {
        return Ok(None);
    };
    let selectors = selectors(source)?;
    let filename = source
        .to_str()
        .ok_or_else(|| invalid("Mesh filename must be UTF-8"))?;
    let mut desc = authoring.description;
    desc.model.source_rotation = Some([0.0; 3]);
    if desc
        .bodyparts
        .iter()
        .flat_map(|body| &body.models)
        .any(|model| !model.flexes.is_empty())
    {
        return Err(invalid(
            "Recompiling preserved Source facial flex activation and timing from Blender is not yet supported; the original QC and VTA sources are required",
        ));
    }
    for binding in &authoring.meshes {
        let model = desc
            .bodyparts
            .get_mut(binding.bodygroup)
            .and_then(|body| body.models.get_mut(binding.choice))
            .ok_or_else(|| invalid("Invalid Source bodygroup binding"))?;
        let name = binding
            .levels
            .first()
            .and_then(Option::as_deref)
            .ok_or_else(|| invalid("A Source bodygroup has no exported base mesh"))?;
        model.smd = filename.into();
        model.src_parts = vec![selector(&selectors, name)?.into()];
        model.src_axis = Some("auto".into());
        model.input_scale = Some(request.scale);
        model.src_scale = None;
        for (level, lod) in model.lods.iter_mut().enumerate() {
            if lod.removed {
                continue;
            }
            let name = binding
                .levels
                .get(level + 1)
                .and_then(Option::as_deref)
                .ok_or_else(|| invalid("An exported Source LOD mesh is missing"))?;
            selector(&selectors, name)?;
            lod.smd = Some(filename.into());
        }
    }
    let actions = mdlc::gltf::animation_names(source, "Source animations")
        .map_err(|error| invalid(&error.to_string()))?;
    for binding in &authoring.animations {
        if !actions.contains(&binding.clip) {
            return Err(invalid(&format!(
                "Source animation {:?} is missing; export all animation actions from Blender",
                binding.clip
            )));
        }
        let animation = desc
            .animations
            .iter_mut()
            .find(|animation| animation.name == binding.name)
            .ok_or_else(|| invalid("Invalid Source animation binding"))?;
        animation.smd = filename.into();
        animation.src_stack = Some(binding.clip.clone());
        animation.src_fps = animation.fps;
        animation.scale = Some(request.scale);
        animation.rotation = Some([0.0; 3]);
    }
    // Blender clips contain absolute poses, including additive clips baked onto the bind pose.
    for animation in &mut desc.animations {
        if animation.name == "__decompiled_delta_base" {
            animation.smd = filename.into();
            animation.scale = Some(request.scale);
            animation.rotation = Some([0.0; 3]);
        }
    }
    if desc.animations.is_empty() {
        for sequence in &mut desc.sequences {
            if !sequence.forward_declared {
                sequence.smd = filename.into();
                sequence.src_stack = actions.first().cloned();
                sequence.input_scale = Some(request.scale);
                sequence.rotation = Some([0.0; 3]);
            }
        }
    }
    if desc.physics.smd.is_some() {
        if super::collision_parts(source)?.is_empty() {
            return Err(invalid(
                "The Source collision meshes are missing; include the Collision collection in the Blender export",
            ));
        }
        desc.physics.smd = Some(filename.into());
        desc.physics.input_scale = Some(request.scale);
    }
    if let Some(static_prop) = request.static_prop_override {
        desc.model.static_prop = static_prop;
    } else if request.static_prop {
        desc.model.static_prop = true;
    }
    if let Some(surface) = &request.surface_property {
        desc.model.surface_prop = Some(surface.clone());
    }
    scale_settings(&mut desc, request.scale);
    bodygroups::apply(&mut desc, request, source)?;
    lods::apply(&mut desc, request, source)?;
    apply_animation_settings(&mut desc, request, source)?;
    match request.collision {
        ModelCollision::None => desc.physics = mdlc::model::Physics::default(),
        ModelCollision::Automatic(_) | ModelCollision::Custom { .. } => {
            if desc.physics.joints {
                return Err(invalid(
                    "Keep authored collision joints when compiling a ragdoll",
                ));
            }
            desc.physics.smd = None;
            desc.physics.mass = Some(request.collision_mass);
        }
        ModelCollision::Existing => {}
    }
    Ok(Some(desc))
}

fn scale_settings(desc: &mut ModelDesc, scale: f32) {
    if scale == 1.0 {
        return;
    }
    let scale_optional = |value: &mut Option<[f32; 3]>| {
        if let Some(value) = value {
            *value = value.map(|axis| axis * scale);
        }
    };
    for value in [
        &mut desc.model.eye_position,
        &mut desc.model.illum_position,
        &mut desc.model.hull_min,
        &mut desc.model.hull_max,
        &mut desc.model.view_bb_min,
        &mut desc.model.view_bb_max,
    ] {
        scale_optional(value);
    }
    for bone in &mut desc.bones {
        scale_optional(&mut bone.position);
        scale_optional(&mut bone.realign_position);
    }
    for attachment in &mut desc.attachments {
        scale_optional(&mut attachment.position);
    }
    for set in std::iter::once(&mut desc.hitboxes).chain(&mut desc.additional_hitbox_sets) {
        for hitbox in &mut set.boxes {
            hitbox.bbmin = hitbox.bbmin.map(|axis| axis * scale);
            hitbox.bbmax = hitbox.bbmax.map(|axis| axis * scale);
        }
    }
    scale_optional(&mut desc.physics.mass_center);
}

pub(super) fn restore_animation_labels(
    compiled: &mut mdlc::model::CompiledModelDesc,
    source: &Path,
) -> Result<(), ModelCompileError> {
    let Some(authoring) = metadata(source)? else {
        return Ok(());
    };
    for binding in authoring.animations {
        if let Some(animation) = compiled
            .animations
            .iter_mut()
            .find(|animation| animation.name == binding.name)
        {
            animation.name = binding.clip;
        }
    }
    Ok(())
}

pub(crate) fn model_settings(
    source: &Path,
) -> Result<Option<super::ImportedModelSettings>, ModelCompileError> {
    Ok(
        metadata(source)?.map(|authoring| super::ImportedModelSettings {
            model_name: authoring.description.model.name,
            static_prop: authoring.description.model.static_prop,
        }),
    )
}

pub(super) fn inspection_animations(
    desc: &ModelDesc,
    source: &Path,
) -> Result<Vec<super::AnimationSettings>, ModelCompileError> {
    desc.sequences
        .iter()
        .filter(|sequence| !sequence.forward_declared)
        .map(|sequence| {
            let animation = desc.animations.iter().find(|animation| {
                animation.name == sequence.smd
                    || sequence
                        .blends
                        .first()
                        .is_some_and(|name| name == &animation.name)
            });
            let fps = sequence
                .fps
                .or_else(|| animation.and_then(|animation| animation.fps))
                .unwrap_or(30.0);
            let stack = animation
                .and_then(|animation| animation.src_stack.as_deref())
                .or(sequence.src_stack.as_deref());
            let options = SrcOpts {
                fbx: mdlc::fbx::FbxOpts {
                    axis: Some(ForcedAxis::Auto),
                    ..Default::default()
                },
                ..Default::default()
            }
            .with_anim(stack, Some(fps));
            let frames = mdlc::compile::read_source_frames(source, &sequence.name, &options)
                .map_err(|error| invalid(&error.to_string()))?;
            let frame_count = frames.frames.len();
            Ok(super::AnimationSettings {
                source: Some(sequence.name.clone()),
                included: true,
                name: sequence.name.clone(),
                fps,
                looping: sequence.looping,
                frame_count,
                activity: sequence.activity.clone(),
                activity_weight: sequence.activity_weight,
                events: sequence
                    .events
                    .iter()
                    .map(|event| super::AnimationEvent {
                        frame: (event.cycle * frame_count.saturating_sub(1) as f32).round() as u32,
                        event: if event.name.is_empty() {
                            event.event.to_string()
                        } else {
                            event.name.clone()
                        },
                        options: event.options.clone(),
                    })
                    .collect(),
            })
        })
        .collect()
}

fn apply_animation_settings(
    desc: &mut ModelDesc,
    request: &ModelCompileRequest,
    source: &Path,
) -> Result<(), ModelCompileError> {
    if request.animation_settings.is_empty() {
        return Ok(());
    }
    if desc.model.static_prop {
        return Err(invalid("Animation settings require an animated model"));
    }
    super::validate_animation_settings(&request.animation_settings)?;
    let originals = inspection_animations(desc, source)?;
    if originals.len() != request.animation_settings.len()
        || originals.iter().any(|original| {
            !request
                .animation_settings
                .iter()
                .any(|setting| setting.source == original.source)
        })
    {
        return Err(invalid(
            "Source sequences changed; import the model again before exporting",
        ));
    }
    for original in &originals {
        let setting = request
            .animation_settings
            .iter()
            .find(|setting| setting.source == original.source)
            .unwrap();
        if setting == original {
            continue;
        }
        let sequence = desc
            .sequences
            .iter_mut()
            .find(|sequence| Some(&sequence.name) == original.source.as_ref())
            .unwrap();
        if setting.name != original.name {
            return Err(invalid(
                "Renaming a preserved Source sequence requires editing its QC so sequence references remain valid",
            ));
        }
        if setting.frame_count != 0 && setting.frame_count != original.frame_count {
            return Err(invalid(
                "Source animation length changed; import the model again before exporting",
            ));
        }
        sequence.fps = Some(setting.fps);
        sequence.looping = setting.looping;
        sequence.activity.clone_from(&setting.activity);
        sequence.activity_weight = setting.activity_weight;
        sequence.events = setting
            .events
            .iter()
            .map(|event| {
                let numeric = event.event.parse::<i32>().ok();
                mdlc::model::SequenceEvent {
                    cycle: event.frame as f32
                        / original.frame_count.saturating_sub(1).max(1) as f32,
                    event_type: if numeric.is_some() { 0 } else { 1024 },
                    event: numeric.unwrap_or(0),
                    name: if numeric.is_some() {
                        String::new()
                    } else {
                        event.event.clone()
                    },
                    options: event.options.clone(),
                }
            })
            .collect();
    }
    let excluded: Vec<_> = request
        .animation_settings
        .iter()
        .filter(|setting| !setting.included)
        .filter_map(|setting| setting.source.as_deref())
        .collect();
    if !excluded.is_empty() {
        return Err(invalid(
            "Removing preserved Source sequences requires editing their QC so sequence references remain valid",
        ));
    }
    Ok(())
}

pub(crate) fn prepare_assets(
    desc: &mut ModelDesc,
    source: &Path,
) -> Result<Option<tempfile::TempDir>, ModelCompileError> {
    let Some(authoring) = metadata(source)? else {
        return Ok(None);
    };
    let selectors = selectors(source)?;
    let temporary = tempfile::tempdir().map_err(|error| super::ModelCompileError::Io {
        path: source.into(),
        source: error,
    })?;
    for binding in &authoring.meshes {
        let Some(model) = desc
            .bodyparts
            .get_mut(binding.bodygroup)
            .and_then(|body| body.models.get_mut(binding.choice))
        else {
            continue;
        };
        for (level, lod) in model.lods.iter_mut().enumerate() {
            if lod.removed || lod.smd.as_deref() != source.to_str() {
                continue;
            }
            let name = binding
                .levels
                .get(level + 1)
                .and_then(Option::as_deref)
                .ok_or_else(|| invalid("An exported Source LOD mesh is missing"))?;
            let options = SrcOpts {
                fbx: mdlc::fbx::FbxOpts {
                    parts: vec![selector(&selectors, name)?.into()],
                    axis: Some(ForcedAxis::Auto),
                    ..Default::default()
                },
                ..Default::default()
            };
            let geometry = mdlc::compile::read_source(source, "Source LOD", &options)
                .map_err(|error| invalid(&error.to_string()))?;
            let path = temporary.path().join(format!(
                "lod_{}_{}_{}.smd",
                binding.bodygroup, binding.choice, level
            ));
            write_smd(&path, &geometry)?;
            lod.smd = Some(path.to_string_lossy().into_owned());
        }
    }
    if desc.physics.smd.as_deref() == source.to_str() {
        let mut geometry = collision_source(source)?;
        if desc.physics.joints {
            joint_collision_axes(&mut geometry)?;
        } else if !desc.model.static_prop {
            for vertex in geometry
                .triangles
                .iter_mut()
                .flat_map(|triangle| &mut triangle.vertices)
            {
                let [x, y, z] = vertex.position;
                vertex.position = [y, -x, z];
            }
            desc.physics.assume_world_space = true;
        }
        let path = temporary.path().join("collision.smd");
        write_smd(&path, &geometry)?;
        desc.physics.smd = Some(path.to_string_lossy().into_owned());
    }
    for animation in &mut desc.animations {
        if animation.name != "__decompiled_delta_base" {
            continue;
        }
        let options = SrcOpts {
            fbx: mdlc::fbx::FbxOpts {
                axis: Some(ForcedAxis::Auto),
                ..Default::default()
            },
            ..Default::default()
        };
        let mut geometry =
            mdlc::compile::read_source(source, "Source additive bind pose", &options)
                .map_err(|error| invalid(&error.to_string()))?;
        geometry.triangles.clear();
        let path = temporary.path().join("delta_base.smd");
        write_smd(&path, &geometry)?;
        animation.smd = path.to_string_lossy().into_owned();
    }
    Ok(Some(temporary))
}

fn joint_collision_axes(geometry: &mut mdlc::smd::Smd) -> Result<(), ModelCompileError> {
    let frame = geometry
        .reference_frame()
        .ok_or_else(|| invalid("Collision skeleton has no bind pose"))?;
    let mut positions = vec![[0.0; 3]; geometry.nodes.len()];
    let mut rotations = positions.clone();
    for pose in &frame.poses {
        let bone = usize::try_from(pose.bone)
            .ok()
            .filter(|&index| index < positions.len())
            .ok_or_else(|| invalid("Collision bind pose references an absent bone"))?;
        positions[bone] = pose.position;
        rotations[bone] = pose.rotation;
    }
    let parents = geometry
        .nodes
        .iter()
        .map(|node| node.parent)
        .collect::<Vec<_>>();
    let worlds = mdlc::bone_math::compute_world(&positions, &rotations, &parents);
    for vertex in geometry
        .triangles
        .iter_mut()
        .flat_map(|triangle| &mut triangle.vertices)
    {
        if vertex.links.len() != 1 || (vertex.links[0].weight - 1.0).abs() > 0.0001 {
            return Err(invalid(
                "Each authored ragdoll collision vertex must be bound rigidly to one bone",
            ));
        }
        let world = usize::try_from(vertex.links[0].bone)
            .ok()
            .and_then(|index| worlds.get(index))
            .ok_or_else(|| invalid("Collision vertex references an absent bone"))?;
        let local = transform(&mdlc::bone_math::invert(world), vertex.position);
        vertex.position = transform(world, [local[1], -local[0], local[2]]);
    }
    Ok(())
}

fn transform(matrix: &mdlc::bone_math::Matrix3x4, point: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|row| {
        matrix[row * 4] * point[0]
            + matrix[row * 4 + 1] * point[1]
            + matrix[row * 4 + 2] * point[2]
            + matrix[row * 4 + 3]
    })
}

fn collision_source(source: &Path) -> Result<mdlc::smd::Smd, ModelCompileError> {
    let parts = super::collision_parts(source)?;
    let mut options = SrcOpts {
        fbx: mdlc::fbx::FbxOpts {
            parts: parts.clone(),
            axis: Some(ForcedAxis::Auto),
            ..Default::default()
        },
        ..Default::default()
    };
    let mut geometry = mdlc::compile::read_source(source, "Source collision", &options)
        .map_err(|error| invalid(&error.to_string()))?;
    let nodes: HashMap<_, _> = geometry
        .nodes
        .iter()
        .map(|node| (node.name.clone(), node.index))
        .collect();
    geometry.triangles.clear();
    for (piece, part) in parts.iter().enumerate() {
        options.fbx.parts = vec![part.clone()];
        let piece_geometry = mdlc::compile::read_source(source, "Source collision piece", &options)
            .map_err(|error| invalid(&error.to_string()))?;
        let remap: HashMap<_, _> = piece_geometry
            .nodes
            .iter()
            .map(|node| {
                nodes
                    .get(&node.name)
                    .copied()
                    .map(|index| (node.index, index))
                    .ok_or_else(|| invalid("A collision piece has an inconsistent skeleton"))
            })
            .collect::<Result<_, _>>()?;
        // A distinct smooth normal keeps touching convex shells separate during Source welding.
        let height = 1.0 - 2.0 * (piece as f32 + 0.5) / parts.len().max(1) as f32;
        let radius = (1.0 - height * height).sqrt();
        let angle = piece as f32 * std::f32::consts::PI * (3.0 - 5.0_f32.sqrt());
        let normal = [radius * angle.cos(), radius * angle.sin(), height];
        for mut triangle in piece_geometry.triangles {
            for vertex in &mut triangle.vertices {
                vertex.normal = normal;
                vertex.parent_bone = *remap
                    .get(&vertex.parent_bone)
                    .ok_or_else(|| invalid("A collision vertex references an absent bone"))?;
                for link in &mut vertex.links {
                    link.bone = *remap
                        .get(&link.bone)
                        .ok_or_else(|| invalid("A collision weight references an absent bone"))?;
                }
            }
            geometry.triangles.push(triangle);
        }
    }
    Ok(geometry)
}

fn write_smd(path: &Path, source: &mdlc::smd::Smd) -> Result<(), ModelCompileError> {
    fs::write(path, crate::model_project::source_smd(source)?).map_err(|source| {
        ModelCompileError::Io {
            path: path.into(),
            source,
        }
    })
}

fn invalid(message: &str) -> ModelCompileError {
    ModelCompileError::InvalidOption(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    const RIG: &str = "version 1\nnodes\n0 \"root\" -1\n1 \"child\" 0\nend\nskeleton\ntime 0\n0 1 2 3 0.2 0.3 0.4\n1 0 0 5 0.1 0 -0.2\ntime 1\n0 4 5 6 0.2 0.3 0.8\n1 0 0 8 0.3 0 -0.2\nend\ntriangles\npaint\n0 2 3 4 0 0 1 0.1 0.2 1 0 1\n1 6 3 4 0 0 1 0.8 0.2 1 1 1\n0 2 7 4 0 0 1 0.1 0.9 2 0 0.25 1 0.75\nend\n";

    fn exported_fixture() -> (
        tempfile::TempDir,
        std::path::PathBuf,
        mdlc::pipeline::PipelineOutput,
    ) {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("rig.smd"), RIG).unwrap();
        let qc = "$modelname models/test.mdl\n$body body rig.smd\n$surfaceprop metal\n$attachment muzzle child 1 2 3 rotate 10 20 30\n$hboxset shots\n$hbox 2 child -2 -3 -4 5 6 7\n$sequence walk rig.smd fps 24 activity ACT_IDLE 2 { event 5004 1 \"fire\" }\n";
        let description = mdlc::qc::parse_qc_str(qc, root.path()).unwrap();
        let original = mdlc::pipeline::build(
            &description,
            root.path(),
            mdlc::pipeline::PipelineOptions::default(),
        )
        .unwrap();
        mdlc::pipeline::write_files(&original, root.path()).unwrap();
        let exported = crate::model_decompile::export_blender_model(
            &root.path().join("models/test.mdl"),
            &root.path().join("blender"),
        )
        .unwrap();
        (root, exported.model_path, original)
    }

    #[test]
    fn exported_model_restores_source_sequences_events_and_named_authoring_settings() {
        let (root, source, original) = exported_fixture();
        let inspected = super::super::inspect_model(&source).unwrap();
        let settings = inspected.source_model.unwrap();
        assert_eq!(settings.model_name, "models/test.mdl");
        assert!(!settings.static_prop);
        assert_eq!(inspected.animations.len(), 1);
        let animation = &inspected.animations[0];
        assert_eq!(animation.name, "walk");
        assert_eq!(animation.fps, 24.0);
        assert_eq!(animation.frame_count, 2);
        assert_eq!(animation.activity.as_deref(), Some("ACT_IDLE"));
        assert_eq!(animation.events[0].event, "5004");
        assert_eq!(animation.events[0].frame, 1);
        let mut request = ModelCompileRequest::new(source.clone(), root.path().join("compiled"));
        request.export_materials = false;
        request.animation_settings = inspected.animations;
        let description =
            super::super::load_description(&request, &source, source.parent().unwrap()).unwrap();
        assert_eq!(description.attachments[0].bone, "child");
        assert_eq!(description.hitboxes.boxes[0].bone, "child");
        assert_eq!(description.hitboxes.boxes[0].group, Some(2));
        assert_eq!(description.model.surface_prop.as_deref(), Some("metal"));
        let output =
            super::super::compile_model_with_cancel(&request, &AtomicBool::new(false)).unwrap();
        assert_eq!(output.triangles, original.compiled.total_triangles());
        let bytes = fs::read(root.path().join("compiled/models/test.mdl")).unwrap();
        let model = vformats::mdl::parse_mdl(&bytes, &vformats::Limits::default()).unwrap();
        let original_model =
            vformats::mdl::parse_mdl(&original.mdl, &vformats::Limits::default()).unwrap();
        assert_eq!(
            model.local_animations[0].name,
            original_model.local_animations[0].name
        );
        assert_eq!(
            model
                .bones
                .iter()
                .map(|bone| bone.name.as_str())
                .collect::<Vec<_>>(),
            ["root", "child"]
        );
        let original_vertices =
            vformats::mdl::parse_vvd(&original.vvd, &vformats::Limits::default()).unwrap();
        let rebuilt_vertices = vformats::mdl::parse_vvd(
            &fs::read(root.path().join("compiled/models/test.vvd")).unwrap(),
            &vformats::Limits::default(),
        )
        .unwrap();
        assert_eq!(
            original_vertices.vertices.len(),
            rebuilt_vertices.vertices.len()
        );
        for expected in &original_vertices.vertices {
            let actual = rebuilt_vertices
                .vertices
                .iter()
                .find(|actual| {
                    actual
                        .position
                        .iter()
                        .zip(expected.position)
                        .all(|(actual, expected)| (actual - expected).abs() < 0.001)
                        && actual
                            .uv
                            .iter()
                            .zip(expected.uv)
                            .all(|(actual, expected)| (actual - expected).abs() < 0.0001)
                })
                .expect("round trip must preserve each original vertex and its UV");
            assert_eq!(actual.bone_count, expected.bone_count);
            for influence in 0..usize::from(expected.bone_count) {
                let expected_bone =
                    &original.compiled.desc.bones[usize::from(expected.bones[influence])].name;
                assert!(
                    (0..usize::from(actual.bone_count)).any(|index| {
                        model.bones[usize::from(actual.bones[index])].name == *expected_bone
                            && (actual.weights[index] - expected.weights[influence]).abs() < 0.0001
                    }),
                    "round trip changed a named bone influence"
                );
            }
        }
        let metadata = metadata(&source).unwrap().unwrap();
        assert_eq!(metadata.description.attachments[0].name, "muzzle");
    }

    #[test]
    fn missing_authored_mesh_fails_instead_of_merging_other_bodygroups() {
        let (_root, source, _) = exported_fixture();
        let mut authoring = metadata(&source).unwrap().unwrap();
        authoring.meshes[0].levels[0] = Some("missing mesh".into());
        let json = serde_json::json!({
            "asset":{"version":"2.0"},
            "nodes":[{"extras":{"gmpublishedSourceMetadata":serde_json::json!({"sourceAuthoring":authoring}).to_string()}}],
            "scenes":[{"nodes":[0]}],"scene":0
        });
        let path = source.with_file_name("missing.gltf");
        fs::write(&path, serde_json::to_vec(&json).unwrap()).unwrap();
        let request = ModelCompileRequest::new(path.clone(), path.with_extension("output"));
        let error = restore(&request, &path).unwrap_err().to_string();
        assert!(error.contains("missing mesh"), "{error}");
        assert!(error.contains("is missing"), "{error}");
    }

    #[test]
    fn authored_collision_round_trip_preserves_prop_pieces_and_rotated_ragdoll_bindings() {
        for joints in [false, true] {
            let (root, _, original) = exported_fixture();
            let mdl = root.path().join("models/test.mdl");
            let original_mdl = fs::read(&mdl).unwrap();
            let model =
                vformats::mdl::parse_mdl(&original_mdl, &vformats::Limits::default()).unwrap();
            let hull = mdlc::phy::PhyHull {
                vertices: vec![
                    [0.0, 0.0, 0.0],
                    [4.0, 0.0, 0.0],
                    [0.0, 7.0, 0.0],
                    [0.0, 0.0, 9.0],
                ],
                faces: vec![[0, 2, 1], [0, 1, 3], [0, 3, 2], [1, 2, 3]],
            };
            let params = mdlc::phy::PhyParams::new("collision", model.checksum);
            let original_phy = if joints {
                mdlc::phy::write_phy_multi(
                    &[vec![hull.clone()], vec![hull]],
                    &[
                        mdlc::phy::PhySolid::ragdoll(0, "root", None),
                        mdlc::phy::PhySolid::ragdoll(1, "child", Some("root".into())),
                    ],
                    &params,
                )
                .unwrap()
            } else {
                let mut touching = hull.clone();
                for point in &mut touching.vertices {
                    point[0] -= 4.0;
                }
                mdlc::phy::write_phy_multi(
                    &[vec![hull, touching]],
                    &[mdlc::phy::PhySolid::prop("collision")],
                    &params,
                )
                .unwrap()
            };
            fs::write(mdl.with_extension("phy"), &original_phy).unwrap();
            let exported = crate::model_decompile::export_blender_model(
                &mdl,
                &root.path().join("collision-blender"),
            )
            .unwrap();
            let mut request = ModelCompileRequest::new(
                exported.model_path,
                root.path().join("collision-compiled"),
            );
            request.export_materials = false;
            let compiled = super::super::compile_model(&request).unwrap();
            assert_eq!(compiled.triangles, original.compiled.total_triangles());
            let compiled_mdl =
                fs::read(root.path().join("collision-compiled/models/test.mdl")).unwrap();
            let compiled_phy =
                fs::read(root.path().join("collision-compiled/models/test.phy")).unwrap();
            let expected =
                crate::model_decompile::load_collision_meshes(&original_mdl, &original_phy)
                    .unwrap();
            let actual =
                crate::model_decompile::load_collision_meshes(&compiled_mdl, &compiled_phy)
                    .unwrap();
            assert_eq!(
                expected.len(),
                actual.len(),
                "changed convex piece count, joints={joints}"
            );
            for expected_piece in expected {
                assert!(
                    actual.iter().any(|actual_piece| {
                        actual_piece.bone == expected_piece.bone
                            && actual_piece.vertices.len() == expected_piece.vertices.len()
                            && expected_piece.vertices.iter().all(|expected| {
                                actual_piece.vertices.iter().any(|actual| {
                                    actual
                                        .iter()
                                        .zip(expected)
                                        .all(|(actual, expected)| (actual - expected).abs() < 0.002)
                                })
                            })
                    }),
                    "changed collision coordinates or binding, joints={joints}: {expected_piece:?}"
                );
            }
        }
    }
}
