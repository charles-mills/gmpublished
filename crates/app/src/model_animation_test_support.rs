use std::{fs, sync::Arc};

use crate::bridge::materials::RenderMode;
use crate::media::preview_model::{MaterialSlot, ModelData, ModelPreview, ModelStats, RenderScene};
use gmpublished_backend::model_compile::{ModelCompileRequest, ModelOutputKind, compile_model};

pub fn animated_model() -> Arc<ModelPreview> {
    model(false, false, 0.0)
}

pub fn blended_model() -> Arc<ModelPreview> {
    model(true, false, 0.0)
}

pub fn lod_blended_model() -> Arc<ModelPreview> {
    model(true, true, 0.0)
}

pub fn off_origin_model() -> Arc<ModelPreview> {
    model(false, false, 100.0)
}

fn model(blended: bool, lods: bool, offset: f32) -> Arc<ModelPreview> {
    let project = tempfile::tempdir().unwrap();
    let source = include_str!("../../vendor/mdlc/tests/fixtures/rig.smd");
    let (header, triangles) = source.split_once("triangles\n").unwrap();
    let triangles = triangles
        .lines()
        .map(|line| {
            let mut fields: Vec<_> = line.split_whitespace().map(str::to_owned).collect();
            if fields.len() >= 9 {
                fields[1] = (fields[1].parse::<f32>().unwrap() + offset).to_string();
            }
            fields.join(" ")
        })
        .collect::<Vec<_>>()
        .join("\n");
    let rig = format!("{header}triangles\n{triangles}\n");
    fs::write(project.path().join("rig.smd"), &rig).unwrap();
    let (_, frames) = rig.split_once("skeleton\n").unwrap();
    let reference = frames.split_once("end\n").unwrap().0;
    let mut motion = String::from(rig.split_once("triangles\n").unwrap().0);
    motion.truncate(motion.rfind("end\n").unwrap());
    motion.push_str(
        &reference
            .replace("time 0", "time 1")
            .replace("0 0 0 0 0 0 0", "0 12 0 0 0 0 0"),
    );
    motion.push_str("end\n");
    fs::write(project.path().join("motion.smd"), motion).unwrap();
    let qc = project.path().join("source.qc");
    let source = if blended {
        "$modelname models/preview.mdl\n$body body rig.smd\n$poseparameter aim -1 1\n$animation rest rig.smd noautoik\n$animation moving motion.smd fps 2 noautoik\n$sequence blend { rest moving blendwidth 2 blend aim -1 1 }\n$sequence loop motion.smd fps 2 loop noautoik\n"
    } else {
        "$modelname models/preview.mdl\n$body body rig.smd\n$sequence move motion.smd fps 2 noautoik\n$sequence loop motion.smd fps 2 loop noautoik\n"
    };
    let source = if lods {
        let (skeleton, triangles) = rig.split_once("triangles\n").unwrap();
        let triangle = triangles.lines().take(4).collect::<Vec<_>>().join("\n");
        fs::write(
            project.path().join("low.smd"),
            format!("{skeleton}triangles\n{triangle}\nend\n"),
        )
        .unwrap();
        format!("{source}$lod 25 {{ replacemodel rig.smd low.smd }}\n")
    } else {
        source.to_owned()
    };
    fs::write(&qc, source).unwrap();
    let mut request = ModelCompileRequest::new(qc, project.path().join("output"));
    request.export_materials = false;
    let output = std::thread::spawn(move || compile_model(&request))
        .join()
        .unwrap()
        .unwrap();
    let bytes = |kind| {
        fs::read(
            &output
                .files
                .iter()
                .find(|file| file.kind == kind)
                .unwrap()
                .path,
        )
        .unwrap()
    };
    let mdl = bytes(ModelOutputKind::Mdl);
    let vvd = bytes(ModelOutputKind::Vvd);
    let vtx = bytes(ModelOutputKind::Vtx);
    let geometry =
        gmpublished_backend::model_decompile::load_model_lod_preview(&mdl, &vvd, &vtx).unwrap();
    let root_lod = geometry.root_lod;
    let models: Vec<_> = geometry.levels.into_iter().map(ModelData::from).collect();
    let bodygroups = models[0].bodygroups.clone();
    let animation =
        gmpublished_backend::model_decompile::load_animation_preview(&mdl, &vvd, &vtx, None)
            .unwrap();
    assert!(
        animation
            .sequences
            .iter()
            .all(|sequence| sequence.unavailable.is_none()),
        "{:?}",
        animation.sequences
    );
    let scenes: Vec<_> = models
        .into_iter()
        .map(|model| {
            let materials = model
                .material_names
                .iter()
                .map(|name| MaterialSlot {
                    tint_base: None,
                    name: name.clone(),
                    texture: None,
                    texture2: None,
                    force_opaque: true,
                    render_mode: RenderMode::Opaque,
                })
                .collect();
            Arc::new(RenderScene {
                meshes: model.meshes,
                materials,
                phy_debug_meshes: vec![],
                bounds_min: model.bounds_min,
                bounds_max: model.bounds_max,
                stats: ModelStats {
                    bone_count: model.bone_count,
                    sequence_count: model.sequence_count,
                    vertex_count: model.vertex_count,
                    triangle_count: model.triangle_count,
                    mesh_count: 1,
                    material_count: 1,
                    resolved_material_count: 0,
                },
            })
        })
        .collect();
    Arc::new(ModelPreview {
        pose_cache: Default::default(),
        root_lod,
        selected_lod: root_lod,
        animation: Some(Ok(Arc::new(animation))),
        skin_tables: vec![vec![0], vec![0]],
        bodygroups,
        scene: Arc::clone(&scenes[0]),
        lods: scenes,
    })
}
