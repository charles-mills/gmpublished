use crate::{
    bone_math as math,
    model::Bone,
    smd::{Smd, SmdPose},
};

type Pose = ([f32; 3], [f32; 3]);

fn parents(
    names: &[String],
    original: &[i32],
    overrides: &[[String; 2]],
    require_child: bool,
) -> Result<Vec<i32>, String> {
    let mut result = original.to_vec();
    for [child, parent] in overrides {
        let Some(index) = names
            .iter()
            .position(|name| name.eq_ignore_ascii_case(child))
        else {
            if require_child {
                return Err(format!("Unknown child bone {child:?} in hierarchy"));
            }
            continue;
        };
        result[index] = if parent.is_empty() {
            -1
        } else {
            names
                .iter()
                .position(|name| name.eq_ignore_ascii_case(parent))
                .ok_or_else(|| {
                    format!("Unknown parent bone {parent:?} for {child:?} in hierarchy")
                })? as i32
        };
    }
    world(&vec![([0.0; 3], [0.0; 3]); names.len()], &result)?;
    Ok(result)
}

fn world(poses: &[Pose], parents: &[i32]) -> Result<Vec<math::Matrix3x4>, String> {
    let mut output = vec![None; poses.len()];
    let mut visiting = vec![false; poses.len()];
    for start in 0..poses.len() {
        let mut path = Vec::new();
        let mut at = Some(start);
        while let Some(index) = at {
            if index >= poses.len() {
                return Err("Hierarchy parent index is out of bounds".into());
            }
            if output[index].is_some() {
                break;
            }
            if visiting[index] {
                return Err("Circular bone hierarchy".into());
            }
            visiting[index] = true;
            path.push(index);
            at = usize::try_from(parents[index]).ok();
        }
        for index in path.into_iter().rev() {
            let local = math::local_transform(poses[index].0, poses[index].1);
            output[index] = Some(match usize::try_from(parents[index]).ok() {
                Some(parent) => math::concat(&output[parent].unwrap(), &local),
                None => local,
            });
            visiting[index] = false;
        }
    }
    Ok(output.into_iter().map(Option::unwrap).collect())
}

fn local(world: &[math::Matrix3x4], index: usize, parent: i32) -> Pose {
    let matrix = match usize::try_from(parent).ok() {
        Some(parent) => math::concat(&math::invert(&world[parent]), &world[index]),
        None => world[index],
    };
    (
        [matrix[3], matrix[7], matrix[11]],
        math::matrix_angles(&matrix),
    )
}

pub(crate) fn source(source: &mut Smd, overrides: &[[String; 2]]) -> Result<(), String> {
    if overrides.is_empty() {
        return Ok(());
    }
    let names: Vec<_> = source.nodes.iter().map(|node| node.name.clone()).collect();
    let original: Vec<_> = source.nodes.iter().map(|node| node.parent).collect();
    let changed = parents(&names, &original, overrides, false)?;
    if changed == original {
        return Ok(());
    }
    let mut previous = vec![([0.0; 3], [0.0; 3]); names.len()];
    for frame in &mut source.frames {
        for pose in &frame.poses {
            if let Ok(index) = usize::try_from(pose.bone)
                && let Some(slot) = previous.get_mut(index)
            {
                *slot = (pose.position, pose.rotation);
            }
        }
        let world = world(&previous, &original)?;
        frame.poses = previous
            .iter()
            .enumerate()
            .map(|(index, pose)| {
                let (position, rotation) = if original[index] == changed[index] {
                    *pose
                } else {
                    local(&world, index, changed[index])
                };
                SmdPose {
                    bone: index as i32,
                    position,
                    rotation,
                }
            })
            .collect();
    }
    for (node, parent) in source.nodes.iter_mut().zip(changed) {
        node.parent = parent;
    }
    let mut order = Vec::with_capacity(names.len());
    let mut placed = vec![false; names.len()];
    for start in 0..names.len() {
        let mut path = Vec::new();
        let mut at = Some(start);
        while let Some(index) = at {
            if placed[index] {
                break;
            }
            path.push(index);
            at = usize::try_from(source.nodes[index].parent).ok();
        }
        for index in path.into_iter().rev() {
            placed[index] = true;
            order.push(index);
        }
    }
    let mut remap = vec![0; names.len()];
    for (new, old) in order.iter().enumerate() {
        remap[*old] = new as i32;
    }
    source.nodes = order
        .iter()
        .map(|old| {
            let mut node = source.nodes[*old].clone();
            node.index = remap[*old];
            if node.parent >= 0 {
                node.parent = remap[node.parent as usize];
            }
            node
        })
        .collect();
    for frame in &mut source.frames {
        for pose in &mut frame.poses {
            pose.bone = remap[pose.bone as usize];
        }
        frame.poses.sort_by_key(|pose| pose.bone);
    }
    for triangle in &mut source.triangles {
        for vertex in &mut triangle.vertices {
            if vertex.parent_bone >= 0 {
                vertex.parent_bone = *remap
                    .get(vertex.parent_bone as usize)
                    .ok_or_else(|| "Source vertex parent bone is out of bounds".to_owned())?;
            }
            for link in &mut vertex.links {
                link.bone = *usize::try_from(link.bone)
                    .ok()
                    .and_then(|index| remap.get(index))
                    .ok_or_else(|| "Source vertex weight bone is out of bounds".to_owned())?;
            }
        }
    }
    Ok(())
}

pub(crate) fn description(
    bones: &mut [Bone],
    overrides: &[[String; 2]],
    reference: impl Fn(&str) -> Option<Pose>,
) -> Result<(), String> {
    if overrides.is_empty() {
        return Ok(());
    }
    let names: Vec<_> = bones.iter().map(|bone| bone.name.clone()).collect();
    let original: Vec<_> = bones
        .iter()
        .map(|bone| match &bone.parent {
            Some(parent) => names
                .iter()
                .position(|name| name.eq_ignore_ascii_case(parent))
                .map(|index| index as i32)
                .ok_or_else(|| format!("Unknown parent bone {parent:?}")),
            None => Ok(-1),
        })
        .collect::<Result<_, _>>()?;
    let changed = parents(&names, &original, overrides, true)?;
    let poses: Vec<_> = bones
        .iter()
        .map(|bone| {
            let fallback = reference(&bone.name).unwrap_or(([0.0; 3], [0.0; 3]));
            (
                bone.position.unwrap_or(fallback.0),
                bone.rotation
                    .map(|value| value.map(f32::to_radians))
                    .unwrap_or(fallback.1),
            )
        })
        .collect();
    let world = world(&poses, &original)?;
    for (index, bone) in bones.iter_mut().enumerate() {
        if original[index] == changed[index] {
            continue;
        }
        let (position, rotation) = local(&world, index, changed[index]);
        let pre_aligned = bone.is_pre_aligned();
        bone.parent = usize::try_from(changed[index])
            .ok()
            .map(|parent| names[parent].clone());
        bone.position = Some(position);
        bone.rotation = Some(rotation.map(f32::to_degrees));
        bone.pre_aligned = Some(pre_aligned);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        compile::{bone_parents, resolve_bone_pose},
        pipeline, qc,
    };
    use std::{
        fs,
        sync::atomic::{AtomicUsize, Ordering},
    };

    fn assert_close(a: &[f32], b: &[f32]) {
        assert_eq!(a.len(), b.len());
        for (a, b) in a.iter().zip(b) {
            assert!((a - b).abs() < 0.0002, "{a} != {b}");
        }
    }

    #[test]
    fn hierarchy_preserves_bind_and_animation_world_poses_after_parent_reordering() {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "mdlc-hierarchy-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        let rig = include_str!("../tests/fixtures/rig.smd")
            .replace("0 0 0 0 0 0 0\n", "0 4 7 -2 0.1 -0.2 0.3\n")
            .replace("1 0 0 10 0 0 0\n", "1 2 3 10 -0.2 0.15 0.4\n");
        fs::write(dir.join("rig.smd"), &rig).unwrap();
        fs::write(
            dir.join("animation.smd"),
            include_str!("../tests/fixtures/animation.smd")
                .replace("0 0 0 0 0 0 0\n", "0 4 7 -2 0.1 -0.2 0.3\n")
                .replace("1 0 0 10 0 0 0\n", "1 2 3 10 -0.2 0.15 0.4\n")
                .replace("1 0 0 11 0 0 0\n", "1 2 4 11 -0.1 0.2 0.5\n")
                .replace("1 0 0 12 0 0 0\n", "1 3 4 12 0.2 0.3 0.6\n"),
        )
        .unwrap();
        let text = "$modelname models/tests/hierarchy.mdl\n$body body rig.smd\n$sequence move animation.smd\n$attachment marker tip 1 2 3\n$collisionjoints rig.smd { $mass 42 }\n";
        let build = |extra: &str| {
            let desc = qc::parse_qc_str(&format!("{text}{extra}"), &dir).unwrap();
            pipeline::build(&desc, &dir, pipeline::PipelineOptions::default()).unwrap()
        };
        let original_output = build("");
        let original = &original_output.compiled;
        for changes in [
            "$hierarchy tip root\n",
            "$heirarchy tip \"\"\n$hierarchy mid tip\n",
        ] {
            let changed_output = build(changes);
            let changed = &changed_output.compiled;
            let physics = changed_output.phy.as_ref().unwrap();
            let layout = crate::phy::check_invariants(physics).unwrap();
            assert_eq!(layout.solid_count, 3);
            let physics =
                std::str::from_utf8(&physics[layout.solids_end..physics.len() - 1]).unwrap();
            assert!(physics.contains("\"name\" \"tip\""));
            let poses = |compiled: &crate::model::CompiledModelDesc| {
                let locals = (0..compiled.desc.bones.len())
                    .map(|index| resolve_bone_pose(&compiled.desc, compiled, index))
                    .collect::<Vec<_>>();
                world(&locals, &bone_parents(&compiled.desc)).unwrap()
            };
            let before = poses(original);
            let after = poses(changed);
            for (index, bone) in original.desc.bones.iter().enumerate() {
                let target = changed
                    .desc
                    .bones
                    .iter()
                    .position(|target| target.name == bone.name)
                    .unwrap();
                assert_close(&before[index], &after[target]);
            }
            for (a, b) in original.animations[0]
                .frames
                .iter()
                .zip(&changed.animations[0].frames)
            {
                let frame = |compiled: &crate::model::CompiledModelDesc, poses: &[SmdPose]| {
                    world(
                        &poses
                            .iter()
                            .map(|pose| (pose.position, pose.rotation))
                            .collect::<Vec<_>>(),
                        &bone_parents(&compiled.desc),
                    )
                    .unwrap()
                };
                let before = frame(original, a);
                let after = frame(changed, b);
                for (index, bone) in original.desc.bones.iter().enumerate() {
                    let target = changed
                        .desc
                        .bones
                        .iter()
                        .position(|target| target.name == bone.name)
                        .unwrap();
                    assert_close(&before[index], &after[target]);
                }
            }
            for (a, b) in original.bodyparts[0].models[0].meshes[0]
                .vertices
                .iter()
                .zip(&changed.bodyparts[0].models[0].meshes[0].vertices)
            {
                assert_close(&a.pos, &b.pos);
                assert_close(&a.normal, &b.normal);
            }
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn hierarchy_rejects_cycles_unknown_children_and_unavailable_source_parents() {
        let mut source = crate::smd::parse_smd(include_str!("../tests/fixtures/rig.smd")).unwrap();
        assert!(
            super::source(&mut source, &[["root".into(), "tip".into()]])
                .unwrap_err()
                .contains("Circular")
        );
        assert!(
            super::source(&mut source, &[["tip".into(), "missing".into()]])
                .unwrap_err()
                .contains("Unknown parent")
        );
        let mut bones = vec![Bone {
            name: "root".into(),
            parent: None,
            position: None,
            rotation: None,
            flags: None,
            surface_prop: None,
            bonemerge: false,
            pre_aligned: None,
            realign_position: None,
            realign_rotation: None,
        }];
        assert!(
            description(&mut bones, &[["missing".into(), "root".into()]], |_| None)
                .unwrap_err()
                .contains("Unknown child")
        );
    }
}
