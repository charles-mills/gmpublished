use crate::{
    bone_math as math,
    compile::{bone_parents, resolve_bone_pose},
    model::{CompiledModelDesc, RealignedBones},
    smd::SmdPose,
};

type Pose = ([f32; 3], [f32; 3]);

fn world(poses: &[Pose], parents: &[i32]) -> Vec<math::Matrix3x4> {
    math::compute_world(
        &poses.iter().map(|pose| pose.0).collect::<Vec<_>>(),
        &poses.iter().map(|pose| pose.1).collect::<Vec<_>>(),
        parents,
    )
}

fn local(matrix: &math::Matrix3x4) -> Pose {
    (
        [matrix[3], matrix[7], matrix[11]],
        math::matrix_angles(matrix),
    )
}

pub(crate) fn tag_alignments(compiled: &mut CompiledModelDesc) -> Result<(), String> {
    for (name, _) in &compiled.desc.model.screen_alignments {
        if !compiled
            .desc
            .bones
            .iter()
            .any(|bone| bone.name.eq_ignore_ascii_case(name))
        {
            return Err(format!("Unknown screen-aligned bone {name:?}"));
        }
    }
    Ok(())
}

pub(crate) fn center(compiled: &mut CompiledModelDesc) -> Result<(), String> {
    if !compiled.desc.model.center_bones_on_vertices {
        return Ok(());
    }
    let count = compiled.desc.bones.len();
    let parents = bone_parents(&compiled.desc);
    let original: Vec<_> = (0..count)
        .map(|index| resolve_bone_pose(&compiled.desc, compiled, index))
        .collect();
    let old_world = world(&original, &parents);
    let mut minimum = vec![[f32::INFINITY; 3]; count];
    let mut maximum = vec![[f32::NEG_INFINITY; 3]; count];
    for model in compiled.bodyparts.iter().flat_map(|part| &part.models) {
        let vertices: Box<dyn Iterator<Item = &crate::model::Vertex> + '_> = match &model.lods {
            Some(lods) => Box::new(lods.meshes.iter().flat_map(|mesh| &mesh.vertices)),
            None => Box::new(model.meshes.iter().flat_map(|mesh| &mesh.vertices)),
        };
        for vertex in vertices {
            for &[bone, weight] in &vertex.bones {
                if weight <= 0.0 {
                    continue;
                }
                let index = bone as usize;
                if index >= count {
                    return Err("Vertex references an invalid bone while centering".into());
                }
                for axis in 0..3 {
                    minimum[index][axis] = minimum[index][axis].min(vertex.pos[axis]);
                    maximum[index][axis] = maximum[index][axis].max(vertex.pos[axis]);
                }
            }
        }
    }
    for parent in (0..count).rev() {
        if minimum[parent][0] <= maximum[parent][0] {
            continue;
        }
        for (child, &ancestor) in parents.iter().enumerate().skip(parent + 1) {
            if ancestor == parent as i32 {
                for axis in 0..3 {
                    minimum[parent][axis] = minimum[parent][axis].min(minimum[child][axis]);
                    maximum[parent][axis] = maximum[parent][axis].max(maximum[child][axis]);
                }
            }
        }
    }
    let mut new_world = old_world.clone();
    for index in 0..count {
        if minimum[index][0] > maximum[index][0] {
            continue;
        }
        for axis in 0..3 {
            new_world[index][axis * 4 + 3] = (minimum[index][axis] + maximum[index][axis]) * 0.5;
        }
    }
    let prior = compiled
        .realigned
        .as_ref()
        .map(|realigned| realigned.src_realign.clone())
        .unwrap_or_else(|| vec![math::IDENTITY; count]);
    let corrections: Vec<_> = (0..count)
        .map(|index| math::concat(&math::invert(&old_world[index]), &new_world[index]))
        .collect();
    let poses = (0..count)
        .map(|index| {
            local(&match usize::try_from(parents[index]).ok() {
                Some(parent) => math::concat(&math::invert(&new_world[parent]), &new_world[index]),
                None => new_world[index],
            })
        })
        .collect();
    compiled.realigned = Some(RealignedBones {
        poses,
        src_realign: prior
            .iter()
            .zip(&corrections)
            .map(|(prior, correction)| math::concat(prior, correction))
            .collect(),
    });
    Ok(())
}

fn reparent_frame(frame: &mut Vec<SmdPose>, parents: &[i32], retained: &[usize], remap: &[usize]) {
    let poses: Vec<_> = frame
        .iter()
        .map(|pose| (pose.position, pose.rotation))
        .collect();
    let matrices = world(&poses, parents);
    *frame = retained
        .iter()
        .enumerate()
        .map(|(new, &old)| {
            let original_parent = usize::try_from(parents[old]).ok();
            let mut parent = original_parent;
            while let Some(index) = parent {
                if remap[index] != usize::MAX {
                    break;
                }
                parent = usize::try_from(parents[index]).ok();
            }
            let (position, rotation) = if parent == original_parent {
                poses[old]
            } else {
                local(&match parent {
                    Some(parent) => math::concat(&math::invert(&matrices[parent]), &matrices[old]),
                    None => matrices[old],
                })
            };
            SmdPose {
                bone: new as i32,
                position,
                rotation,
            }
        })
        .collect();
}

pub(crate) fn collapse(compiled: &mut CompiledModelDesc) -> Result<(), String> {
    let desc = &compiled.desc;
    if desc.model.static_prop || !desc.model.collapse_bones {
        return Ok(());
    }
    let count = desc.bones.len();
    let parents = bone_parents(desc);
    let lookup = |name: &str| {
        desc.bones
            .iter()
            .position(|bone| bone.name.eq_ignore_ascii_case(name))
    };
    let mut protected = vec![false; count];
    for index in 0..count {
        protected[index] =
            parents[index] < 0 || desc.bones[index].is_pre_aligned() || desc.bones[index].bonemerge;
    }
    for name in desc
        .hitbox_sets()
        .flat_map(|set| set.boxes.iter().map(|item| item.bone.as_str()))
        .chain(
            desc.bodyparts
                .iter()
                .flat_map(|part| &part.models)
                .flat_map(|model| &model.eyeballs)
                .map(|eye| eye.bone.as_str()),
        )
        .chain(
            desc.physics
                .joint_overrides
                .iter()
                .map(|joint| joint.bone.as_str()),
        )
        .chain(
            desc.model
                .bone_flex_drivers
                .iter()
                .map(|item| item.bone.as_str()),
        )
        .chain(desc.model.hitgroups.iter().map(|item| item.0.as_str()))
        .chain(
            desc.model
                .rotation_limits
                .iter()
                .map(|item| item.bone.as_str()),
        )
        .chain(desc.model.root_bone.as_deref())
        .chain(desc.bonecontrollers.iter().map(|item| item.bone.as_str()))
        .chain(desc.mouths.iter().map(|item| item.bone.as_str()))
        .chain(desc.jiggle_bones.iter().map(|item| item.bone.as_str()))
        .chain(
            desc.quat_interp_bones
                .iter()
                .flat_map(|item| [item.bone.as_str(), item.control.as_str()]),
        )
        .chain(
            desc.model
                .screen_alignments
                .iter()
                .map(|item| item.0.as_str()),
        )
        .chain(
            desc.model
                .forced_realign
                .iter()
                .map(|item| item.bone.as_str()),
        )
    {
        if let Some(index) = lookup(name) {
            protected[index] = true;
        }
    }
    for chain in &desc.ikchains {
        let mut index = lookup(&chain.bone);
        for _ in 0..3 {
            if let Some(bone) = index {
                protected[bone] = true;
                index = usize::try_from(parents[bone]).ok();
            }
        }
    }
    for hierarchy in compiled
        .animations
        .iter()
        .flat_map(|animation| &animation.local_hierarchy)
    {
        if let Ok(index) = usize::try_from(hierarchy.bone) {
            protected[index] = true;
        }
        if let Ok(index) = usize::try_from(hierarchy.parent) {
            protected[index] = true;
        }
    }
    let mut animated = vec![false; count];
    for animation in &compiled.animations {
        let frames = animation
            .pre_subtract_frames
            .as_ref()
            .unwrap_or(&animation.frames);
        let Some(first) = frames.first() else {
            continue;
        };
        for frame in frames.iter().skip(1) {
            for (index, (start, pose)) in first.iter().zip(frame).enumerate() {
                let different_position = start
                    .position
                    .iter()
                    .zip(pose.position)
                    .any(|(a, b)| (a - b).abs() > 0.01);
                let different_rotation = start.rotation.iter().zip(pose.rotation).any(|(a, b)| {
                    let delta = (a - b).abs().rem_euclid(std::f32::consts::TAU);
                    delta.min(std::f32::consts::TAU - delta) > 0.01
                });
                animated[index] |= different_position || different_rotation;
            }
        }
    }
    // A delta keeps its original local basis when a static parent is retained.
    if compiled.animations.iter().any(|animation| animation.delta) {
        for index in 0..count {
            if animated[index] {
                let mut parent = usize::try_from(parents[index]).ok();
                while let Some(ancestor) = parent {
                    protected[ancestor] = true;
                    parent = usize::try_from(parents[ancestor]).ok();
                }
            }
        }
    }
    let strongly_protected = protected.clone();
    for attachment in &desc.attachments {
        if let Some(index) = lookup(&attachment.bone) {
            protected[index] = true;
        }
    }
    let forced: Vec<_> = desc
        .model
        .always_collapse
        .iter()
        .map(|name| lookup(name).ok_or_else(|| format!("Unknown bone {name:?} in $alwayscollapse")))
        .collect::<Result<_, _>>()?;
    for &index in &forced {
        let disposable = desc
            .jiggle_bones
            .iter()
            .any(|bone| bone.bone.eq_ignore_ascii_case(&desc.bones[index].name))
            || desc
                .quat_interp_bones
                .iter()
                .any(|bone| bone.bone.eq_ignore_ascii_case(&desc.bones[index].name))
            || desc
                .model
                .bone_flex_drivers
                .iter()
                .any(|driver| driver.bone.eq_ignore_ascii_case(&desc.bones[index].name));
        let ik_ancestor = desc.ikchains.iter().any(|chain| {
            let tip = lookup(&chain.bone);
            let parent = tip.and_then(|bone| usize::try_from(parents[bone]).ok());
            let grandparent = parent.and_then(|bone| usize::try_from(parents[bone]).ok());
            tip != Some(index) && (parent == Some(index) || grandparent == Some(index))
        });
        let has_required_reference = desc
            .hitbox_sets()
            .flat_map(|set| &set.boxes)
            .any(|item| item.bone.eq_ignore_ascii_case(&desc.bones[index].name))
            || desc
                .bonecontrollers
                .iter()
                .any(|item| item.bone.eq_ignore_ascii_case(&desc.bones[index].name))
            || desc
                .ikchains
                .iter()
                .any(|chain| lookup(&chain.bone) == Some(index))
            || desc.quat_interp_bones.iter().any(|item| {
                item.control.eq_ignore_ascii_case(&desc.bones[index].name)
                    && !forced.contains(&lookup(&item.bone).unwrap_or(usize::MAX))
            })
            || desc
                .model
                .hitgroups
                .iter()
                .any(|item| item.0.eq_ignore_ascii_case(&desc.bones[index].name))
            || desc
                .model
                .screen_alignments
                .iter()
                .any(|item| item.0.eq_ignore_ascii_case(&desc.bones[index].name))
            || desc
                .model
                .forced_realign
                .iter()
                .any(|item| item.bone.eq_ignore_ascii_case(&desc.bones[index].name))
            || desc
                .model
                .rotation_limits
                .iter()
                .any(|item| item.bone.eq_ignore_ascii_case(&desc.bones[index].name))
            || desc
                .mouths
                .iter()
                .any(|item| item.bone.eq_ignore_ascii_case(&desc.bones[index].name))
            || desc
                .bodyparts
                .iter()
                .flat_map(|part| &part.models)
                .flat_map(|model| &model.eyeballs)
                .any(|item| item.bone.eq_ignore_ascii_case(&desc.bones[index].name))
            || desc
                .physics
                .joint_overrides
                .iter()
                .any(|item| item.bone.eq_ignore_ascii_case(&desc.bones[index].name))
            || compiled
                .animations
                .iter()
                .flat_map(|animation| &animation.local_hierarchy)
                .any(|item| item.bone == index as i32 || item.parent == index as i32);
        if strongly_protected[index]
            && !desc.bones[index].is_pre_aligned()
            && (!(disposable || ik_ancestor || parents[index] < 0) || has_required_reference)
        {
            return Err(format!(
                "Cannot collapse referenced bone {:?}",
                desc.bones[index].name
            ));
        }
        if (!strongly_protected[index]
            || ((disposable || ik_ancestor || parents[index] < 0) && !has_required_reference))
            && !desc.bones[index].is_pre_aligned()
        {
            protected[index] = false;
        }
    }
    for chain in &desc.ikchains {
        let mut bone = lookup(&chain.bone);
        let mut remaining = 0;
        while let Some(index) = bone {
            if !forced.contains(&index) || desc.bones[index].is_pre_aligned() {
                protected[index] = true;
                remaining += 1;
                if remaining == 3 {
                    break;
                }
            }
            bone = usize::try_from(parents[index]).ok();
        }
        if remaining < 3 {
            return Err(format!(
                "IK chain {:?} has fewer than three bones after collapsing",
                chain.name
            ));
        }
    }
    let mut used = protected.clone();
    for model in compiled.bodyparts.iter().flat_map(|part| &part.models) {
        for vertex in model.meshes.iter().flat_map(|mesh| &mesh.vertices).chain(
            model
                .lods
                .iter()
                .flat_map(|lods| &lods.meshes)
                .flat_map(|mesh| &mesh.vertices),
        ) {
            for &[bone, weight] in &vertex.bones {
                if weight > 0.0 {
                    used[bone as usize] = true;
                }
            }
        }
    }
    if !desc.model.collapse_bones_aggressive {
        for index in (0..count).rev() {
            if used[index] && parents[index] >= 0 {
                used[parents[index] as usize] = true;
            }
        }
    }
    let retained: Vec<_> = (0..count)
        .filter(|&index| {
            protected[index] || (used[index] && animated[index] && !forced.contains(&index))
        })
        .collect();
    if retained.len() == count {
        return Ok(());
    }
    if retained.is_empty() {
        return Err("Bone collapsing removed every bone in the model".into());
    }
    let mut remap = vec![usize::MAX; count];
    for (new, &old) in retained.iter().enumerate() {
        remap[old] = new;
    }
    let representative: Vec<_> = (0..count)
        .map(|mut index| {
            while remap[index] == usize::MAX {
                let Ok(parent) = usize::try_from(parents[index]) else {
                    return 0;
                };
                index = parent;
            }
            remap[index]
        })
        .collect();
    let mut bind: Vec<_> = (0..count)
        .map(|index| {
            let (position, rotation) = resolve_bone_pose(desc, compiled, index);
            SmdPose {
                bone: index as i32,
                position,
                rotation,
            }
        })
        .collect();
    let original_bind = world(
        &bind
            .iter()
            .map(|pose| (pose.position, pose.rotation))
            .collect::<Vec<_>>(),
        &parents,
    );
    let original_names: Vec<_> = desc.bones.iter().map(|bone| bone.name.clone()).collect();
    let original_bind_poses = bind.clone();
    reparent_frame(&mut bind, &parents, &retained, &remap);
    let surfaces: Vec<_> = crate::contents::bone_surface_props(desc)?
        .into_iter()
        .map(|value| value.map(str::to_owned))
        .collect();
    let contents = crate::contents::bone_contents(desc)?;
    let mut bones: Vec<_> = retained
        .iter()
        .map(|&index| desc.bones[index].clone())
        .collect();
    for (new, &old) in retained.iter().enumerate() {
        let mut parent = usize::try_from(parents[old]).ok();
        while let Some(index) = parent {
            if remap[index] != usize::MAX {
                break;
            }
            parent = usize::try_from(parents[index]).ok();
        }
        bones[new].parent = parent.map(|index| desc.bones[index].name.clone());
        bones[new].surface_prop = surfaces[old].clone();
        if parent != usize::try_from(parents[old]).ok() {
            bones[new].position = Some(bind[new].position);
            bones[new].rotation = Some(bind[new].rotation.map(f32::to_degrees));
        }
    }
    for model in compiled
        .bodyparts
        .iter_mut()
        .flat_map(|part| &mut part.models)
    {
        if model.poses.is_empty() && model.meshes.is_empty() {
            continue;
        }
        let source_poses: Vec<_> = (0..count)
            .map(|index| {
                model
                    .poses
                    .iter()
                    .find(|pose| pose.bone == index as i32)
                    .copied()
                    .unwrap_or(SmdPose {
                        bone: index as i32,
                        position: [0.0; 3],
                        rotation: [0.0; 3],
                    })
            })
            .collect();
        model.poses = source_poses;
        reparent_frame(&mut model.poses, &parents, &retained, &remap);
        model.source_world = retained
            .iter()
            .map(|&old| model.source_world.get(old).copied().flatten())
            .collect();
        let mut vertices: Vec<_> = model
            .meshes
            .iter_mut()
            .flat_map(|mesh| &mut mesh.vertices)
            .collect();
        if let Some(lods) = &mut model.lods {
            vertices.extend(lods.meshes.iter_mut().flat_map(|mesh| &mut mesh.vertices));
            let mut usage = vec![0; retained.len()];
            for (old, &flags) in lods.bone_lod_usage.iter().enumerate() {
                usage[representative[old]] |= flags;
            }
            lods.bone_lod_usage = usage;
        }
        for vertex in vertices {
            let mut weights: Vec<[f32; 2]> = Vec::new();
            for [bone, weight] in &vertex.bones {
                let index = representative[*bone as usize] as f32;
                if let Some(existing) = weights.iter_mut().find(|entry| entry[0] == index) {
                    existing[1] += weight;
                } else {
                    weights.push([index, *weight]);
                }
            }
            vertex.bones = weights;
        }
    }
    for animation in &mut compiled.animations {
        if animation.delta {
            reparent_delta_frames(
                &mut animation.frames,
                animation.pre_subtract_frames.as_deref(),
                &animation.weights,
                &original_bind_poses,
                &parents,
                &retained,
                &remap,
            );
        } else {
            reparent_frame_set(&mut animation.frames, &parents, &retained, &remap);
        }
        if let Some(frames) = &mut animation.pre_subtract_frames {
            reparent_frame_set(frames, &parents, &retained, &remap);
        }
        if let Some(frames) = &mut animation.source_frames {
            reparent_frame_set(frames, &parents, &retained, &remap);
        }
        animation.weights = retained.iter().map(|&old| animation.weights[old]).collect();
        for hierarchy in &mut animation.local_hierarchy {
            hierarchy.bone = remap[hierarchy.bone as usize] as i32;
            if hierarchy.parent >= 0 {
                hierarchy.parent = remap[hierarchy.parent as usize] as i32;
            }
        }
    }
    for sequence in &mut compiled.sequences {
        if sequence.delta {
            reparent_delta_frames(
                &mut sequence.frames,
                sequence.pre_subtract_frames.as_deref(),
                &sequence.weights,
                &original_bind_poses,
                &parents,
                &retained,
                &remap,
            );
        } else {
            reparent_frame_set(&mut sequence.frames, &parents, &retained, &remap);
        }
        if let Some(frames) = &mut sequence.pre_subtract_frames {
            reparent_frame_set(frames, &parents, &retained, &remap);
        }
        sequence.weights = retained.iter().map(|&old| sequence.weights[old]).collect();
    }
    for attachment in &mut compiled.desc.attachments {
        let old = attachment
            .resolved
            .as_ref()
            .map(|resolved| resolved.0)
            .or_else(|| {
                original_names
                    .iter()
                    .position(|name| name.eq_ignore_ascii_case(&attachment.bone))
            });
        let Some(old) = old else {
            continue;
        };
        if old >= count {
            return Err("Attachment references an invalid bone while collapsing".into());
        }
        let new = representative[old];
        if remap[old] != usize::MAX {
            if let Some(resolved) = &mut attachment.resolved {
                resolved.0 = new;
            }
            continue;
        }
        let ancestor = retained[new];
        let correction = if attachment.absolute {
            math::IDENTITY
        } else {
            let parent = math::concat(&math::invert(&original_bind[ancestor]), &original_bind[old]);
            math::concat(
                &parent,
                &attachment
                    .resolved
                    .map_or(math::IDENTITY, |resolved| resolved.1),
            )
        };
        attachment.bone = original_names[ancestor].clone();
        attachment.resolved = Some((new, correction));
    }
    if let Some(realigned) = &mut compiled.realigned {
        realigned.poses = bind
            .iter()
            .map(|pose| (pose.position, pose.rotation))
            .collect();
        realigned.src_realign = retained
            .iter()
            .map(|&old| realigned.src_realign[old])
            .collect();
    }
    compiled.desc.model.joint_contents = retained
        .iter()
        .map(|&old| (compiled.desc.bones[old].name.clone(), contents[old]))
        .collect();
    let names: std::collections::HashSet<_> = bones
        .iter()
        .map(|bone| bone.name.to_ascii_lowercase())
        .collect();
    compiled
        .desc
        .default_weight_list
        .retain(|entry| names.contains(&entry.bone.to_ascii_lowercase()));
    for list in &mut compiled.desc.weight_lists {
        list.bones
            .retain(|entry| names.contains(&entry.bone.to_ascii_lowercase()));
    }
    compiled.desc.jiggle_bones.retain(|bone| {
        let retained = names.contains(&bone.bone.to_ascii_lowercase());
        if !retained {
            log::info!("Jiggle bone {:?} was removed by $alwayscollapse", bone.bone);
        }
        retained
    });
    compiled.desc.quat_interp_bones.retain(|bone| {
        let retained = names.contains(&bone.bone.to_ascii_lowercase());
        if !retained {
            log::info!(
                "Procedural bone {:?} was removed by $alwayscollapse",
                bone.bone
            );
        }
        retained
    });
    compiled.desc.model.bone_flex_drivers.retain(|driver| {
        let retained = names.contains(&driver.bone.to_ascii_lowercase());
        if !retained {
            crate::warning!(
                "Bone flex driver {:?} was removed by $alwayscollapse",
                driver.bone
            );
        }
        retained
    });
    if let Some(root) = &mut compiled.desc.model.root_bone
        && !names.contains(&root.to_ascii_lowercase())
    {
        *root = bones[0].name.clone();
    }
    compiled.desc.bones = bones;
    Ok(())
}

fn reparent_frame_set(
    frames: &mut [Vec<SmdPose>],
    parents: &[i32],
    retained: &[usize],
    remap: &[usize],
) {
    for frame in frames {
        if !frame.is_empty() {
            reparent_frame(frame, parents, retained, remap);
        }
    }
}

pub(crate) fn lock_lengths(
    compiled: &mut CompiledModelDesc,
    lengths: &[[f32; 3]],
) -> Result<(), String> {
    if !compiled.desc.model.lock_bone_lengths {
        return Ok(());
    }
    let parents = bone_parents(&compiled.desc);
    let chains: Vec<_> = compiled
        .desc
        .ikchains
        .iter()
        .map(|chain| {
            let tip = compiled
                .desc
                .bones
                .iter()
                .position(|bone| bone.name.eq_ignore_ascii_case(&chain.bone))
                .ok_or("Unknown IK bone while locking lengths")?;
            let mid = usize::try_from(parents[tip])
                .map_err(|_| "IK chain requires three bones while locking lengths")?;
            let root = usize::try_from(parents[mid])
                .map_err(|_| "IK chain requires three bones while locking lengths")?;
            Ok([root, mid, tip])
        })
        .collect::<Result<Vec<_>, String>>()?;
    for animation in &mut compiled.animations {
        if animation.delta {
            continue;
        }
        for frame in &mut animation.frames {
            let original = crate::compile::frame_worlds(&compiled.desc, &parents, frame);
            for (index, pose) in frame.iter_mut().enumerate() {
                if parents[index] >= 0 {
                    pose.position = lengths[index];
                }
            }
            let mut locked = crate::compile::frame_worlds(&compiled.desc, &parents, frame);
            for &[root, mid, tip] in &chains {
                if animation.weights[tip] <= 0.0 {
                    continue;
                }
                let target = [original[tip][3], original[tip][7], original[tip][11]];
                crate::ik_fixup::solve(&mut locked, [root, mid, tip], target)?;
                for bone in [root, mid, tip] {
                    let mut matrix = if parents[bone] >= 0 {
                        math::concat(
                            &math::invert(&locked[parents[bone] as usize]),
                            &locked[bone],
                        )
                    } else {
                        locked[bone]
                    };
                    if parents[bone] < 0 && !compiled.desc.model.static_prop {
                        matrix = math::concat(
                            &math::angle_matrix([0.0, 0.0, -std::f32::consts::FRAC_PI_2]),
                            &matrix,
                        );
                    }
                    (frame[bone].position, frame[bone].rotation) = local(&matrix);
                }
            }
        }
    }
    for sequence in &mut compiled.sequences {
        if let Some(&index) = sequence.cells.first() {
            sequence
                .frames
                .clone_from(&compiled.animations[index].frames);
        }
    }
    Ok(())
}

fn vertex_count(model: &crate::model::CompiledModel) -> usize {
    model.lods.as_ref().map_or_else(
        || model.meshes.iter().map(|mesh| mesh.vertices.len()).sum(),
        |lods| lods.meshes.iter().map(|mesh| mesh.vertices.len()).sum(),
    )
}

pub(crate) fn vertex_limit(
    model: &crate::model::CompiledModel,
    requested: Option<[usize; 2]>,
) -> usize {
    requested
        .filter(|limits| vertex_count(model) >= limits[0])
        .map_or(crate::mdl_writer::MAXSTUDIOVERTS_PER_MESH, |limits| {
            limits[1]
        })
}

pub(crate) fn split_models(compiled: &mut CompiledModelDesc) {
    let Some([threshold, target]) = compiled.desc.model.max_vertices else {
        return;
    };
    let mut additions = Vec::new();
    for (body_index, part) in compiled.bodyparts.iter_mut().enumerate() {
        let mut extra_parts: Vec<crate::model::CompiledBodyPart> = Vec::new();
        let blank_models: Vec<_> = part
            .models
            .iter()
            .map(|_| crate::model::CompiledModel {
                dmx: None,
                smd_path: Default::default(),
                name: "blank".into(),
                poses: Vec::new(),
                source_world: Vec::new(),
                meshes: Vec::new(),
                lods: None,
                eyeballs: Vec::new(),
                mesh_flexes: Vec::new(),
                mesh_src_index: Vec::new(),
                shape_keys: Vec::new(),
            })
            .collect();
        for (model_index, model) in part.models.iter_mut().enumerate() {
            if vertex_count(model) < threshold {
                continue;
            }
            let mut chunks: Vec<Vec<usize>> = vec![Vec::new()];
            let mut vertices = 0;
            for (index, mesh) in model.meshes.iter().enumerate() {
                let count = model.lods.as_ref().map_or(mesh.vertices.len(), |lods| {
                    lods.meshes[index].vertices.len()
                });
                if vertices + count > target && !chunks.last().unwrap().is_empty() {
                    chunks.push(Vec::new());
                    vertices = 0;
                }
                chunks.last_mut().unwrap().push(index);
                vertices += count;
            }
            if chunks.len() <= 1 {
                continue;
            }
            let source = model.clone();
            let template = crate::model::CompiledModel {
                dmx: None,
                smd_path: source.smd_path.clone(),
                name: source.name.clone(),
                poses: source.poses.clone(),
                source_world: source.source_world.clone(),
                meshes: Vec::new(),
                lods: source.lods.as_ref().map(|lods| crate::model::ModelLods {
                    meshes: Vec::new(),
                    num_lods: lods.num_lods,
                    switch_points: lods.switch_points.clone(),
                    bone_lod_usage: lods.bone_lod_usage.clone(),
                    no_facial: lods.no_facial.clone(),
                }),
                eyeballs: source.eyeballs.clone(),
                mesh_flexes: Vec::new(),
                mesh_src_index: Vec::new(),
                shape_keys: Vec::new(),
            };
            for (chunk_index, meshes) in chunks.iter().enumerate() {
                let mut chunk = template.clone();
                chunk.meshes = meshes
                    .iter()
                    .map(|&index| source.meshes[index].clone())
                    .collect();
                chunk.mesh_flexes = meshes
                    .iter()
                    .map(|&index| source.mesh_flexes.get(index).cloned().unwrap_or_default())
                    .collect();
                chunk.mesh_src_index.clear();
                chunk.shape_keys.clear();
                if let Some(lods) = &mut chunk.lods {
                    lods.meshes = meshes
                        .iter()
                        .map(|&index| source.lods.as_ref().unwrap().meshes[index].clone())
                        .collect();
                }
                if chunk_index == 0 {
                    *model = chunk;
                } else {
                    if extra_parts.len() < chunk_index {
                        extra_parts.push(crate::model::CompiledBodyPart {
                            name: format!("{}_clamped{}", part.name, chunk_index),
                            base: part.base,
                            models: blank_models.clone(),
                        });
                    }
                    extra_parts[chunk_index - 1].models[model_index] = chunk;
                }
            }
        }
        additions.extend(extra_parts.into_iter().map(|part| (body_index, part)));
    }
    for (body, part) in additions {
        let mut description = compiled.desc.bodyparts[body].clone();
        description.name = part.name.clone();
        description.base = Some(part.base);
        for (description, model) in description.models.iter_mut().zip(&part.models) {
            if model.meshes.is_empty() {
                description.smd.clear();
                description.name = Some("blank".into());
                description.lods.clear();
                description.eyeballs.clear();
                description.flexes.clear();
            }
        }
        compiled.desc.bodyparts.push(description);
        compiled.bodyparts.push(part);
    }
}

#[cfg(test)]
#[path = "bone_optimization_tests.rs"]
mod tests;

pub(crate) fn transform_frame(
    frame: &mut [SmdPose],
    parents: &[i32],
    corrections: &[math::Matrix3x4],
) {
    if frame.len() != parents.len() {
        return;
    }
    let poses: Vec<_> = frame
        .iter()
        .map(|pose| (pose.position, pose.rotation))
        .collect();
    let matrices = world(&poses, parents);
    let transformed: Vec<_> = matrices
        .iter()
        .zip(corrections)
        .map(|(matrix, correction)| math::concat(matrix, correction))
        .collect();
    for (index, pose) in frame.iter_mut().enumerate() {
        let matrix = match usize::try_from(parents[index]).ok() {
            Some(parent) => math::concat(&math::invert(&transformed[parent]), &transformed[index]),
            None => transformed[index],
        };
        (pose.position, pose.rotation) = local(&matrix);
    }
}

pub(crate) fn realign_frames(compiled: &mut CompiledModelDesc) {
    let Some(realigned) = &compiled.realigned else {
        return;
    };
    let corrections = realigned.src_realign.clone();
    let parents = bone_parents(&compiled.desc);
    let new_bind = world(&realigned.poses, &parents);
    let old_bind: Vec<_> = new_bind
        .iter()
        .zip(&corrections)
        .map(|(matrix, correction)| math::concat(matrix, &math::invert(correction)))
        .collect();
    let base: Vec<_> = old_bind
        .iter()
        .enumerate()
        .map(|(index, matrix)| {
            let matrix = match usize::try_from(parents[index]).ok() {
                Some(parent) => math::concat(&math::invert(&old_bind[parent]), matrix),
                None => *matrix,
            };
            let (position, rotation) = local(&matrix);
            SmdPose {
                bone: index as i32,
                position,
                rotation,
            }
        })
        .collect();
    for frames in compiled
        .animations
        .iter_mut()
        .filter_map(|animation| animation.source_frames.as_mut())
    {
        for frame in frames {
            transform_frame(frame, &parents, &corrections);
        }
    }
    for (frames, original, delta, weights) in compiled
        .animations
        .iter_mut()
        .map(|animation| {
            (
                &mut animation.frames,
                &mut animation.pre_subtract_frames,
                animation.delta,
                &animation.weights,
            )
        })
        .chain(compiled.sequences.iter_mut().map(|sequence| {
            (
                &mut sequence.frames,
                &mut sequence.pre_subtract_frames,
                sequence.delta,
                &sequence.weights,
            )
        }))
    {
        for (index, frame) in frames.iter_mut().enumerate() {
            if frame.len() != parents.len() {
                continue;
            }
            if !delta {
                transform_frame(frame, &parents, &corrections);
                continue;
            }
            let mut reference = base.clone();
            let mut absolute = original
                .as_ref()
                .and_then(|frames| frames.get(index))
                .cloned();
            if let Some(absolute) = &absolute {
                for bone in 0..parents.len() {
                    if weights.get(bone).copied().unwrap_or(1.0) <= 0.0 {
                        continue;
                    }
                    let q = math::angle_quaternion(frame[bone].rotation);
                    reference[bone].rotation = math::quaternion_angles(math::quaternion_mult(
                        math::angle_quaternion(absolute[bone].rotation),
                        [-q[0], -q[1], -q[2], q[3]],
                    ));
                    reference[bone].position = std::array::from_fn(|axis| {
                        absolute[bone].position[axis] - frame[bone].position[axis]
                    });
                }
            } else {
                absolute = Some(
                    base.iter()
                        .zip(frame.iter())
                        .map(|(reference, delta)| {
                            let mut pose = *reference;
                            pose.position = std::array::from_fn(|axis| {
                                reference.position[axis] + delta.position[axis]
                            });
                            pose.rotation = math::quaternion_angles(math::quaternion_mult(
                                math::angle_quaternion(reference.rotation),
                                math::angle_quaternion(delta.rotation),
                            ));
                            pose
                        })
                        .collect(),
                );
            }
            let mut absolute = absolute.unwrap();
            transform_frame(&mut absolute, &parents, &corrections);
            transform_frame(&mut reference, &parents, &corrections);
            for (bone, pose) in frame.iter_mut().enumerate() {
                if weights.get(bone).copied().unwrap_or(1.0) <= 0.0 {
                    *pose = absolute[bone];
                    continue;
                }
                let q = math::angle_quaternion(reference[bone].rotation);
                pose.rotation = math::quaternion_angles(math::quaternion_mult(
                    [-q[0], -q[1], -q[2], q[3]],
                    math::angle_quaternion(absolute[bone].rotation),
                ));
                pose.position = std::array::from_fn(|axis| {
                    absolute[bone].position[axis] - reference[bone].position[axis]
                });
            }
        }
        if let Some(frames) = original {
            for frame in frames {
                transform_frame(frame, &parents, &corrections);
            }
        }
    }
}

fn reparent_delta_frames(
    frames: &mut [Vec<SmdPose>],
    originals: Option<&[Vec<SmdPose>]>,
    weights: &[f32],
    base: &[SmdPose],
    parents: &[i32],
    retained: &[usize],
    remap: &[usize],
) {
    for (frame_index, frame) in frames.iter_mut().enumerate() {
        if frame.len() != parents.len() {
            continue;
        }
        let original = originals.and_then(|frames| frames.get(frame_index));
        let mut reference = base.to_vec();
        let mut absolute = original.cloned().unwrap_or_else(|| base.to_vec());
        for index in 0..parents.len() {
            if weights.get(index).copied().unwrap_or(1.0) <= 0.0 {
                absolute[index] = frame[index];
                continue;
            }
            let delta_rotation = math::angle_quaternion(frame[index].rotation);
            if original.is_some() {
                reference[index].position = std::array::from_fn(|axis| {
                    absolute[index].position[axis] - frame[index].position[axis]
                });
                reference[index].rotation = math::quaternion_angles(math::quaternion_mult(
                    math::angle_quaternion(absolute[index].rotation),
                    [
                        -delta_rotation[0],
                        -delta_rotation[1],
                        -delta_rotation[2],
                        delta_rotation[3],
                    ],
                ));
            } else {
                absolute[index].position = std::array::from_fn(|axis| {
                    base[index].position[axis] + frame[index].position[axis]
                });
                absolute[index].rotation = math::quaternion_angles(math::quaternion_mult(
                    math::angle_quaternion(base[index].rotation),
                    delta_rotation,
                ));
            }
        }
        reparent_frame(&mut absolute, parents, retained, remap);
        reparent_frame(&mut reference, parents, retained, remap);
        *frame = absolute.clone();
        for (new, &old) in retained.iter().enumerate() {
            if weights.get(old).copied().unwrap_or(1.0) <= 0.0 {
                continue;
            }
            frame[new].position = std::array::from_fn(|axis| {
                absolute[new].position[axis] - reference[new].position[axis]
            });
            let q = math::angle_quaternion(reference[new].rotation);
            frame[new].rotation = math::quaternion_angles(math::quaternion_mult(
                [-q[0], -q[1], -q[2], q[3]],
                math::angle_quaternion(absolute[new].rotation),
            ));
        }
    }
}
