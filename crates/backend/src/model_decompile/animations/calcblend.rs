use std::{collections::HashSet, fmt::Write as _};

use mdlc::bone_math::{self, Matrix3x4};
use vformats::mdl::Mdl;

use super::{Animation, DecompileError, Reader, invalid, quoted};

pub(super) struct Axis {
    pub parameter: String,
    pub keys: Vec<f32>,
}

pub(super) struct Recovery {
    pub axes: [Option<String>; 2],
    pub options: String,
}

struct Attachment<'a> {
    name: &'a str,
    bone: usize,
    local: Matrix3x4,
}

pub(super) fn recover(
    reader: &Reader<'_>,
    model: &Mdl,
    animations: &[Animation],
    cells: &[usize],
    grid: [usize; 2],
    axes: &[Option<Axis>; 2],
) -> Result<Recovery, DecompileError> {
    let attachments = reader.table(240, 244, 92)?;
    let attachments = attachments
        .into_iter()
        .map(|record| {
            let name = reader.string(record, record)?;
            let bone = reader.count(record + 8)?;
            if bone >= model.bones.len() {
                return Err(invalid("Calcblend attachment references an absent bone"));
            }
            let mut local = [0.0; 12];
            for (component, value) in local.iter_mut().enumerate() {
                *value = reader.f32(record + 12 + component * 4)?;
            }
            Ok(Attachment { name, bone, local })
        })
        .collect::<Result<Vec<_>, DecompileError>>()?;
    let reference_worlds = animations
        .iter()
        .map(|animation| worlds(model, animation, &animations[0]))
        .collect::<Vec<_>>();
    let has_delta = cells.iter().any(|cell| animations[*cell].flags & 4 != 0);
    let mut seen = HashSet::new();
    let references = std::iter::once(0)
        .chain(cells.iter().copied())
        .chain(0..animations.len())
        .filter(|index| seen.insert(*index))
        .collect::<Vec<_>>();
    let mut budget = 2_000_000usize;
    let mut seen_cells = HashSet::new();
    let centers = std::iter::once(None)
        .chain(
            cells
                .iter()
                .enumerate()
                .filter_map(|(index, cell)| seen_cells.insert(*cell).then_some(Some(index))),
        )
        .collect::<Vec<_>>();
    for reference in references {
        let mut seen_comp = HashSet::new();
        for comp in std::iter::once(reference)
            .chain((0..animations.len()).filter(|_| has_delta))
            .filter(|index| seen_comp.insert(*index))
        {
            budget = budget
                .checked_sub(model.bones.len() * animations.len())
                .ok_or(DecompileError::SizeLimit)?;
            let worlds = animations
                .iter()
                .map(|animation| worlds(model, animation, &animations[comp]))
                .collect::<Vec<_>>();
            for &center in &centers {
                let mut recovered: [Option<String>; 2] = [None, None];
                for axis in 0..2 {
                    let Some(expected) = &axes[axis] else {
                        continue;
                    };
                    let other = center.map_or(grid[1 - axis] / 2, |center| {
                        if axis == 0 {
                            center / grid[0]
                        } else {
                            center % grid[0]
                        }
                    });
                    for attachment in &attachments {
                        let reference_world = reference_worlds[reference][attachment.bone];
                        let inverse = bone_math::invert(&bone_math::concat(
                            &reference_world,
                            &attachment.local,
                        ));
                        let mut values = Vec::with_capacity(grid[axis]);
                        for key in 0..grid[axis] {
                            budget = budget.checked_sub(1).ok_or(DecompileError::SizeLimit)?;
                            let cell = if axis == 0 {
                                other * grid[0] + key
                            } else {
                                key * grid[0] + other
                            };
                            let world = worlds[cells[cell]][attachment.bone];
                            let relative = bone_math::concat(
                                &inverse,
                                &bone_math::concat(&world, &attachment.local),
                            );
                            let angles = bone_math::matrix_angles(&relative).map(f32::to_degrees);
                            if !relative
                                .iter()
                                .chain(&angles)
                                .all(|value| value.is_finite())
                            {
                                return Err(invalid(
                                    "Calcblend attachment reconstruction overflowed",
                                ));
                            }
                            values.push([
                                relative[3],
                                relative[7],
                                relative[11],
                                angles[0],
                                angles[1],
                                angles[2],
                            ]);
                        }
                        for (control, name) in ["X", "Y", "Z", "XR", "YR", "ZR"].iter().enumerate()
                        {
                            if values.iter().zip(&expected.keys).all(|(actual, expected)| {
                                (actual[control] - expected).abs()
                                    <= 0.001 + expected.abs() * f32::EPSILON * 8.0
                            }) {
                                recovered[axis] = Some(format!(
                                    "    calcblend {} {} {name}\n",
                                    quoted(&expected.parameter)?,
                                    quoted(attachment.name)?
                                ));
                                break;
                            }
                        }
                        if recovered[axis].is_some() {
                            break;
                        }
                    }
                    if recovered[axis].is_none() {
                        break;
                    }
                }
                if axes
                    .iter()
                    .zip(&recovered)
                    .all(|(axis, recovery)| axis.is_none() || recovery.is_some())
                {
                    let mut options =
                        format!("    blendref \"__decompiled_animation_{reference}\"\n");
                    if has_delta {
                        writeln!(options, "    blendcomp \"__decompiled_animation_{comp}\"")
                            .unwrap();
                    }
                    if let Some(center) = center {
                        writeln!(
                            options,
                            "    blendcenter \"__decompiled_animation_{}\"",
                            cells[center]
                        )
                        .unwrap();
                    }
                    return Ok(Recovery {
                        axes: recovered,
                        options,
                    });
                }
            }
        }
    }
    Err(invalid(
        "Nonlinear sequence blend keys cannot be reconstructed from the model's attachments and animations",
    ))
}

fn worlds(model: &Mdl, animation: &Animation, base: &Animation) -> Vec<Matrix3x4> {
    let root = bone_math::angle_matrix([0.0, 0.0, std::f32::consts::FRAC_PI_2]);
    let mut worlds = Vec::with_capacity(model.bones.len());
    for ((pose, base), bone) in animation.frames[0]
        .iter()
        .zip(&base.frames[0])
        .zip(&model.bones)
    {
        let local = if animation.flags & 4 == 0 {
            bone_math::local_transform(pose.position, pose.rotation)
        } else {
            let rotation = bone_math::quaternion_ma(
                bone_math::angle_quaternion(base.rotation),
                1.0,
                bone_math::angle_quaternion(pose.rotation),
            );
            let position = std::array::from_fn(|axis| base.position[axis] + pose.position[axis]);
            bone_math::quaternion_local_transform(rotation, position)
        };
        let world = if bone.parent < 0 {
            bone_math::concat(&root, &local)
        } else {
            bone_math::concat(&worlds[bone.parent as usize], &local)
        };
        worlds.push(world);
    }
    worlds
}

#[cfg(test)]
#[path = "calcblend_tests.rs"]
mod tests;
