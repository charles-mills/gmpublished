use crate::{
    model::{AnimCmd, ModelDesc},
    smd::Smd,
};

pub(crate) fn name(value: &mut String, renames: &[[String; 2]]) {
    if let Some([_, to]) = renames
        .iter()
        .find(|[from, _]| from.eq_ignore_ascii_case(value))
    {
        value.clone_from(to);
    }
}

pub(crate) fn source(source: &mut Smd, renames: &[[String; 2]]) -> Result<(), String> {
    if renames.is_empty() {
        return Ok(());
    }
    let original: Vec<_> = source.nodes.iter().map(|node| node.name.clone()).collect();
    for node in &mut source.nodes {
        name(&mut node.name, renames);
    }
    let mut seen = std::collections::HashMap::<String, usize>::new();
    for (index, node) in source.nodes.iter().enumerate() {
        if let Some(other) = seen.insert(node.name.to_ascii_lowercase(), index)
            && !original[index].eq_ignore_ascii_case(&original[other])
        {
            return Err(format!(
                "Bone renaming maps both {:?} and {:?} to {:?}",
                original[other], original[index], node.name
            ));
        }
    }
    Ok(())
}

pub(crate) fn description(desc: &mut ModelDesc) -> Result<(), String> {
    let renames = desc.model.bone_renames.clone();
    if renames
        .iter()
        .flatten()
        .any(|name| name.is_empty() || name.contains('\0'))
    {
        return Err("Bone rename names must be nonempty and contain no null bytes".into());
    }
    let rename = |value: &mut String| name(value, &renames);
    if let Some(root) = &mut desc.model.root_bone {
        rename(root);
    }
    for (bone, _) in &mut desc.model.joint_contents {
        rename(bone);
    }
    for limit in &mut desc.model.rotation_limits {
        rename(&mut limit.bone);
    }
    for forced in &mut desc.model.forced_realign {
        rename(&mut forced.bone);
    }
    for pair in &mut desc.model.bone_hierarchy {
        rename(&mut pair[0]);
        rename(&mut pair[1]);
    }
    for bone in &mut desc.bones {
        rename(&mut bone.name);
        if let Some(parent) = &mut bone.parent {
            rename(parent);
        }
    }
    for set in std::iter::once(&mut desc.hitboxes).chain(&mut desc.additional_hitbox_sets) {
        for hitbox in &mut set.boxes {
            rename(&mut hitbox.bone);
        }
    }
    for attachment in &mut desc.attachments {
        rename(&mut attachment.bone);
    }
    for controller in &mut desc.bonecontrollers {
        rename(&mut controller.bone);
    }
    for chain in &mut desc.ikchains {
        rename(&mut chain.bone);
    }
    for jiggle in &mut desc.jiggle_bones {
        rename(&mut jiggle.bone);
    }
    for procedural in &mut desc.quat_interp_bones {
        rename(&mut procedural.bone);
        rename(&mut procedural.control);
    }
    for mouth in &mut desc.mouths {
        rename(&mut mouth.bone);
    }
    for weight in &mut desc.default_weight_list {
        rename(&mut weight.bone);
    }
    for list in &mut desc.weight_lists {
        for weight in &mut list.bones {
            rename(&mut weight.bone);
        }
    }
    for list in &mut desc.cmd_lists {
        commands(&mut list.cmds, &renames);
    }
    for sequence in &mut desc.sequences {
        commands(&mut sequence.cmds, &renames);
        for rule in &mut sequence.ik_rules {
            if let Some(bone) = &mut rule.bone {
                rename(bone);
            }
        }
    }
    for animation in &mut desc.animations {
        commands(&mut animation.cmds, &renames);
        for rule in &mut animation.ik_rules {
            if let Some(bone) = &mut rule.bone {
                rename(bone);
            }
        }
    }
    for part in &mut desc.bodyparts {
        for model in &mut part.models {
            for eye in &mut model.eyeballs {
                rename(&mut eye.bone);
            }
            for lod in &mut model.lods {
                for bone in &mut lod.bone_tree_collapse {
                    rename(bone);
                }
                for pair in &mut lod.replace_bone {
                    for bone in pair {
                        rename(bone);
                    }
                }
            }
        }
    }
    if let Some(root) = &mut desc.physics.root_bone {
        rename(root);
    }
    for joint in &mut desc.physics.joint_overrides {
        rename(&mut joint.bone);
    }
    for constraint in &mut desc.physics.constraints {
        rename(&mut constraint.bone);
    }
    for pair in desc
        .physics
        .collision_pairs
        .iter_mut()
        .chain(&mut desc.physics.merge)
    {
        rename(&mut pair.a);
        rename(&mut pair.b);
    }
    for bone in &mut desc.physics.skip {
        rename(bone);
    }
    Ok(())
}

fn commands(commands: &mut [AnimCmd], renames: &[[String; 2]]) {
    for command in commands {
        match command {
            AnimCmd::Align {
                bone: Some(bone), ..
            }
            | AnimCmd::CounterRotate { bone, .. } => name(bone, renames),
            AnimCmd::LocalHierarchy { bone, parent, .. } => {
                name(bone, renames);
                name(parent, renames);
            }
            AnimCmd::IkRule { rule } | AnimCmd::IkFixup { rule } => {
                if let Some(bone) = &mut rule.bone {
                    name(bone, renames);
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{pipeline, qc};
    use std::{
        fs,
        path::PathBuf,
        sync::atomic::{AtomicUsize, Ordering},
    };

    struct Directory(PathBuf);

    impl Directory {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "mdlc-bone-rename-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn renamed(source: &str) -> String {
        source
            .split_inclusive(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_')
            .map(|token| {
                let end = token
                    .trim_end_matches(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_')
                    .len();
                let replacement = match &token[..end] {
                    "root" => "pelvis",
                    "mid" => "elbow",
                    "tip" => "hand",
                    value => value,
                };
                format!("{replacement}{}", &token[end..])
            })
            .collect()
    }

    #[test]
    fn renamed_sources_and_qc_produce_identical_model_and_collision_files() {
        let original = Directory::new();
        let manual = Directory::new();
        for (name, contents) in [
            ("rig.smd", include_str!("../tests/fixtures/rig.smd")),
            ("rig_lod.smd", include_str!("../tests/fixtures/rig_lod.smd")),
            (
                "animation.smd",
                include_str!("../tests/fixtures/animation.smd"),
            ),
        ] {
            fs::write(original.0.join(name), contents).unwrap();
            fs::write(manual.0.join(name), renamed(contents)).unwrap();
        }
        let text = r#"$modelname models/tests/renamed.mdl
$surfaceprop metal
$definebone root "" 0 0 0 0 0 0
$definebone mid root 0 0 10 0 0 0
$body body rig.smd
$bonemerge mid
$lod 15 { replacemodel rig.smd rig_lod.smd replacebone tip mid }
$weightlist upper mid 0.5 tip 0.75
$sequence move animation.smd { fps 20 weightlist upper }
$hboxset damage
$hbox 2 mid -2 -3 -4 5 6 7 body
$attachment muzzle tip 1 2 3 rotate 15 25 35
$illumposition 1 2 3 tip
$controller 0 root x -2 5
$ikchain arm tip knee 1 0 0
$model face rig.smd { mouth 0 open mid 1 0.5 -0.25 }
$collisionjoints rig.smd {
$mass 42
$rootbone root
$jointdamping mid 0.3
$jointrotdamping tip 0.7
$jointinertia mid 3
$jointmassbias mid 2
$jointsurfaceprop mid wood
$jointconstrain mid x limit -35 45 7.5
$jointcollide root mid
}
"#;
        let input =
            format!("$renamebone ROOT pelvis\n$renamebone mid elbow\n$renamebone tip hand\n{text}");
        let from = qc::parse_qc_str(&input, &original.0).unwrap();
        let target = qc::parse_qc_str(&renamed(text), &manual.0).unwrap();
        let a = pipeline::build(&from, &original.0, pipeline::PipelineOptions::default()).unwrap();
        let b = pipeline::build(&target, &manual.0, pipeline::PipelineOptions::default()).unwrap();
        assert_eq!(a.mdl, b.mdl, "MDL");
        assert_eq!(a.vvd, b.vvd, "VVD");
        assert_eq!(a.vtx, b.vtx, "VTX");
        assert!(a.phy.is_some());
        assert_eq!(a.phy, b.phy, "PHY");
        assert_eq!(a.ani, b.ani, "ANI");
    }

    #[test]
    fn rename_matching_uses_first_case_insensitive_mapping_without_recursion() {
        let mappings = [
            ["ROOT".into(), "mid".into()],
            ["root".into(), "other".into()],
            ["mid".into(), "tip".into()],
        ];
        let mut value = "root".to_owned();
        name(&mut value, &mappings);
        assert_eq!(value, "mid");
        let mut source = crate::smd::parse_smd(include_str!("../tests/fixtures/rig.smd")).unwrap();
        source.nodes.pop();
        super::source(&mut source, &mappings).unwrap();
        assert_eq!(source.nodes[0].name, "mid");
        assert_eq!(source.nodes[1].name, "tip");
    }

    #[test]
    fn colliding_renames_are_rejected_before_bone_weights_are_merged() {
        let mut source = crate::smd::parse_smd(include_str!("../tests/fixtures/rig.smd")).unwrap();
        let error = super::source(&mut source, &[["root".into(), "mid".into()]]).unwrap_err();
        assert!(error.contains("both"));
        assert!(error.contains("root"));
        let dir = Directory::new();
        fs::write(
            dir.0.join("rig.smd"),
            include_str!("../tests/fixtures/rig.smd"),
        )
        .unwrap();
        assert!(
            qc::parse_qc_str(
                "$modelname model.mdl\n$body body rig.smd\n$renamebone root mid\n",
                &dir.0
            )
            .is_err()
        );
    }
}
