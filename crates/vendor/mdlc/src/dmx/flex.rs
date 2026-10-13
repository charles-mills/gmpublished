use super::*;
use crate::model::{
    CompiledModelDesc, FlexController, FlexDescriptor, FlexOpData, ResolvedFlexOp, ResolvedFlexRule,
};

pub(super) fn read_controls(document: &Document, metadata: &mut Metadata) -> Result<()> {
    let root = node(document, document.root)?;
    let Some(operator) = element(root, "combinationOperator")? else {
        return Ok(());
    };
    let operator = node(document, operator)?;
    metadata.has_combination = true;
    if let Some(rules) = super::flex_rules::read_targets(document, operator)? {
        metadata.explicit_rules = Some(rules);
        for id in elements(operator, "controls")? {
            let control = node(document, id)?;
            metadata.controls.push(Control {
                name: control.name.clone(),
                raw: strings(control, "rawControlNames")?,
                stereo: boolean(control, "stereo", false)?,
                eyelid: boolean(control, "eyelid", false)?,
                eyes_up_down: string(control, "eyesUpDownFlexName", "eyes_updown")?,
                wrinkle_scales: floats(control, "wrinkleScales")?.to_vec(),
                explicit: true,
            });
        }
        return Ok(());
    }
    let mut names = HashSet::new();
    let mut raw_names = HashSet::new();
    for id in elements(operator, "controls")? {
        let n = node(document, id)?;
        if n.name.is_empty() || !names.insert(n.name.clone()) {
            return Err("DMX combination operator has an empty or duplicate control name".into());
        }
        let raw = strings(n, "rawControlNames")?;
        if raw.is_empty() {
            return Err(format!("DMX control {} has no raw controls", n.name));
        }
        for name in &raw {
            if name.is_empty() || name.contains('_') || !raw_names.insert(name.clone()) {
                return Err(format!(
                    "DMX raw control {name:?} is empty, duplicated, or contains the combination separator"
                ));
            }
        }
        let wrinkle_scales = floats(n, "wrinkleScales")?.to_vec();
        if !wrinkle_scales.is_empty() && wrinkle_scales.len() != raw.len() {
            return Err(format!(
                "DMX control {} has mismatched wrinkle scales",
                n.name
            ));
        }
        let eyelid = boolean(n, "eyelid", false)?;
        if eyelid && raw.len() != 2 {
            return Err(format!(
                "DMX eyelid control {} must have two raw states",
                n.name
            ));
        }
        metadata.controls.push(Control {
            name: n.name.clone(),
            raw,
            stereo: boolean(n, "stereo", false)?,
            eyelid,
            eyes_up_down: string(n, "eyesUpDownFlexName", "eyes_updown")?,
            wrinkle_scales,
            explicit: false,
        });
    }
    for id in elements(operator, "dominators")? {
        let n = node(document, id)?;
        let controls = strings(n, "dominators")?;
        let suppressed = strings(n, "suppressed")?;
        if controls.is_empty()
            || suppressed.is_empty()
            || controls
                .iter()
                .chain(&suppressed)
                .any(|name| !raw_names.contains(name))
        {
            return Err(format!(
                "DMX domination rule {} has empty or unknown raw controls",
                n.name
            ));
        }
        metadata.dominators.push(Dominator {
            controls,
            suppressed,
        });
    }
    for id in elements(operator, "targets")? {
        let target = node(document, id)?;
        if target.class != "DmeMesh" && target.class != "DmeFlexRules" {
            return Err(format!(
                "DMX combination target type {} is unsupported",
                target.class
            ));
        }
        for id in elements(target, "deltaStates")? {
            metadata
                .active_shapes
                .push(node(document, id)?.name.clone());
        }
    }
    Ok(())
}

fn descriptor(compiled: &mut CompiledModelDesc, name: &str) -> usize {
    compiled
        .desc
        .flex_descriptors
        .iter()
        .position(|f| f.name.eq_ignore_ascii_case(name))
        .unwrap_or_else(|| {
            let id = compiled.desc.flex_descriptors.len();
            compiled
                .desc
                .flex_descriptors
                .push(FlexDescriptor { name: name.into() });
            id
        })
}

fn controller(compiled: &mut CompiledModelDesc, name: &str, min: f32, max: f32) -> Result<usize> {
    if let Some(id) = compiled
        .desc
        .flex_controllers
        .iter()
        .position(|f| f.name.eq_ignore_ascii_case(name))
    {
        let existing = &compiled.desc.flex_controllers[id];
        if existing.min != min || existing.max != max {
            return Err(format!(
                "DMX controller {name} conflicts with an existing controller range"
            ));
        }
        return Ok(id);
    }
    if compiled.desc.flex_controllers.len() >= 96 {
        return Err("DMX model exceeds Source's 96 flex controllers".into());
    }
    let id = compiled.desc.flex_controllers.len();
    compiled.desc.flex_controllers.push(FlexController {
        name: name.into(),
        kind: name.into(),
        min,
        max,
    });
    Ok(id)
}

fn with_metadata(
    compiled: &mut CompiledModelDesc,
    mut apply: impl FnMut(&mut CompiledModelDesc, usize, usize, &Metadata) -> Result<()>,
) -> Result<()> {
    for bodypart in 0..compiled.bodyparts.len() {
        for model in 0..compiled.bodyparts[bodypart].models.len() {
            if let Some(metadata) = compiled.bodyparts[bodypart].models[model].dmx.take() {
                let result = apply(compiled, bodypart, model, &metadata);
                compiled.bodyparts[bodypart].models[model].dmx = Some(metadata);
                result?;
            }
        }
    }
    Ok(())
}

fn shape_raw<'a>(metadata: &'a Metadata, name: &'a str) -> Option<Vec<(&'a Control, usize)>> {
    name.split('_')
        .map(|raw| {
            metadata.controls.iter().find_map(|control| {
                control
                    .raw
                    .iter()
                    .position(|name| name == raw)
                    .map(|i| (control, i))
            })
        })
        .collect()
}

pub(crate) fn register(compiled: &mut CompiledModelDesc) -> Result<()> {
    with_metadata(compiled, |compiled, bi, mi, metadata| {
        if !metadata.has_combination {
            return Ok(());
        }
        if let Some(rules) = &metadata.explicit_rules {
            super::flex_rules::register(compiled, rules)?;
        }
        for control in metadata.controls.iter().filter(|control| !control.explicit) {
            let min = if control.raw.len() == 2 || control.eyelid {
                -1.
            } else {
                0.
            };
            if control.stereo {
                controller(compiled, &format!("right_{}", control.name), min, 1.)?;
                controller(compiled, &format!("left_{}", control.name), min, 1.)?;
            } else {
                controller(compiled, &control.name, min, 1.)?;
            }
            if control.raw.len() > 2 || control.eyelid {
                controller(compiled, &format!("multi_{}", control.name), -1., 1.)?;
            }
            if control.eyelid {
                controller(compiled, "blink", 0., 1.)?;
            }
        }
        let mut payload = vec![Vec::new(); compiled.bodyparts[bi].models[mi].meshes.len()];
        let mut by_vertex = HashMap::<u32, Vec<(usize, u32)>>::new();
        for (mesh, ids) in compiled.bodyparts[bi].models[mi]
            .mesh_src_index
            .iter()
            .enumerate()
        {
            for (vertex, &id) in ids.iter().enumerate() {
                by_vertex.entry(id).or_default().push((mesh, vertex as u32));
            }
        }
        for shape in &metadata.shapes {
            let stereo = shape_raw(metadata, &shape.name)
                .is_some_and(|controls| controls.iter().any(|(c, _)| c.stereo));
            let left = descriptor(
                compiled,
                &if stereo {
                    format!("{}L", shape.name)
                } else {
                    shape.name.clone()
                },
            );
            let right = if stereo {
                descriptor(compiled, &format!("{}R", shape.name)) as i32
            } else {
                0
            };
            let mut anims = vec![Vec::new(); payload.len()];
            for delta in &shape.vertices {
                if !(0. ..=1.).contains(&delta.speed) || !(0. ..=1.).contains(&delta.balance) {
                    return Err(format!(
                        "DMX shape {} speed or balance is outside 0..1",
                        shape.name
                    ));
                }
                for &(mesh, vertex) in by_vertex.get(&delta.vertex).into_iter().flatten() {
                    anims[mesh].push(crate::flex::ResolvedVertAnim {
                        index: vertex,
                        speed: (delta.speed * 255.) as u8,
                        side: (delta.balance * 255.) as u8,
                        delta: delta.position,
                        ndelta: delta.normal,
                        wrinkle: delta.wrinkle,
                    });
                }
            }
            for (mesh, mut vertices) in anims.into_iter().enumerate() {
                if vertices.is_empty() {
                    continue;
                }
                vertices.sort_by_key(|v| v.index);
                payload[mesh].push(crate::flex::ResolvedFlex {
                    flexdesc: left as i32,
                    targets: [0., 1., 10., 11.],
                    flexpair: right,
                    vertanimtype: u8::from(shape.wrinkle),
                    vertanims: vertices,
                });
            }
        }
        let model = &mut compiled.bodyparts[bi].models[mi];
        if model.mesh_flexes.len() < payload.len() {
            model.mesh_flexes.resize(payload.len(), Vec::new());
        }
        for (mesh, flexes) in payload.into_iter().enumerate() {
            model.mesh_flexes[mesh].extend(flexes);
        }
        Ok(())
    })
}

fn controller_id(compiled: &CompiledModelDesc, name: &str) -> Result<i32> {
    compiled
        .desc
        .flex_controllers
        .iter()
        .position(|f| f.name.eq_ignore_ascii_case(name))
        .map(|i| i as i32)
        .ok_or_else(|| format!("DMX controller {name} was not registered"))
}

fn op(op: i32, d: FlexOpData) -> ResolvedFlexOp {
    ResolvedFlexOp { op, d }
}
fn constant(value: f32) -> ResolvedFlexOp {
    op(1, FlexOpData::Value(value))
}

fn fetch(
    compiled: &CompiledModelDesc,
    control: &Control,
    raw: usize,
    right: bool,
    out: &mut Vec<ResolvedFlexOp>,
) -> Result<()> {
    let name = if control.stereo {
        format!("{}_{}", if right { "right" } else { "left" }, control.name)
    } else {
        control.name.clone()
    };
    let id = controller_id(compiled, &name)?;
    if control.eyelid {
        let eyes = controller_id(compiled, &control.eyes_up_down).unwrap_or(-1);
        out.extend([
            constant(eyes as f32),
            constant(controller_id(compiled, "blink")? as f32),
            constant(id as f32),
            op(
                if raw == 0 { 20 } else { 21 },
                FlexOpData::Index(controller_id(compiled, &format!("multi_{}", control.name))?),
            ),
        ]);
    } else if control.raw.len() == 1 {
        out.push(op(2, FlexOpData::Index(id)));
    } else if control.raw.len() == 2 {
        out.push(op(if raw == 0 { 15 } else { 16 }, FlexOpData::Index(id)));
    } else {
        let count = control.raw.len();
        let step = 2. / (count - 1) as f32;
        let targets = if raw == 0 {
            [-11., -10., -1., -1. + step]
        } else if raw == count - 1 {
            [1. - step, 1., 10., 11.]
        } else {
            let peak = raw as f32 * step - 1.;
            [peak - step, peak, peak, peak + step]
        };
        out.extend(targets.map(constant));
        out.push(constant(
            controller_id(compiled, &format!("multi_{}", control.name))? as f32,
        ));
        out.push(op(17, FlexOpData::Index(id)));
    }
    Ok(())
}

pub(crate) fn resolve(compiled: &mut CompiledModelDesc) -> Result<()> {
    let mut rules = Vec::new();
    let mut uis = Vec::new();
    let mut seen_ui = HashSet::new();
    with_metadata(compiled, |compiled, _, _, metadata| {
        if !metadata.has_combination {
            return Ok(());
        }
        if let Some(rules) = &metadata.explicit_rules {
            super::flex_rules::resolve(compiled, rules)?;
        }
        for control in metadata.controls.iter().filter(|control| !control.explicit) {
            if !seen_ui.insert(control.name.clone()) {
                continue;
            }
            let left = if control.stereo {
                controller_id(compiled, &format!("left_{}", control.name))?
            } else {
                controller_id(compiled, &control.name)?
            };
            let right = if control.stereo {
                Some(controller_id(compiled, &format!("right_{}", control.name))?)
            } else {
                None
            };
            let remap = if control.eyelid {
                3
            } else {
                match control.raw.len() {
                    0 | 1 => 0,
                    2 => 1,
                    _ => 2,
                }
            };
            let multi = if remap >= 2 {
                Some(controller_id(compiled, &format!("multi_{}", control.name))?)
            } else {
                None
            };
            uis.push(crate::model::ResolvedFlexControllerUi {
                name: control.name.clone(),
                fc0: left,
                fc1: right,
                stereo: control.stereo,
                remap_type: remap,
                fc2: multi,
            });
        }
        for shape in &metadata.shapes {
            if !metadata.active_shapes.contains(&shape.name) {
                continue;
            }
            let Some(raw) = shape_raw(metadata, &shape.name) else {
                continue;
            };
            let stereo = raw.iter().any(|(c, _)| c.stereo);
            for right in [false, true] {
                if right && !stereo {
                    continue;
                }
                let name = if stereo {
                    format!("{}{}", shape.name, if right { "R" } else { "L" })
                } else {
                    shape.name.clone()
                };
                let flex = compiled
                    .desc
                    .flex_descriptors
                    .iter()
                    .position(|f| f.name.eq_ignore_ascii_case(&name))
                    .ok_or_else(|| format!("DMX descriptor {name} is missing"))?;
                let mut ops = Vec::new();
                for &(control, index) in &raw {
                    fetch(compiled, control, index, right, &mut ops)?;
                }
                if raw.len() > 1 {
                    ops.push(op(18, FlexOpData::Index(raw.len() as i32)));
                }
                for dominator in &metadata.dominators {
                    if !dominator
                        .suppressed
                        .iter()
                        .all(|name| raw.iter().any(|(c, i)| &c.raw[*i] == name))
                    {
                        continue;
                    }
                    for name in &dominator.controls {
                        let (control, index) = metadata
                            .controls
                            .iter()
                            .find_map(|c| c.raw.iter().position(|n| n == name).map(|i| (c, i)))
                            .ok_or("DMX dominator has an unknown raw control")?;
                        fetch(compiled, control, index, right, &mut ops)?;
                    }
                    ops.push(op(19, FlexOpData::Index(dominator.controls.len() as i32)));
                }
                if !ops.is_empty() {
                    rules.push(ResolvedFlexRule {
                        flex: flex as i32,
                        ops,
                    });
                }
            }
        }
        Ok(())
    })?;
    for ui in uis {
        let controlled = [Some(ui.fc0), ui.fc1, ui.fc2];
        compiled.resolved_flex_controller_ui.retain(|u| {
            u.name != ui.name
                && !controlled.contains(&Some(u.fc0))
                && u.fc1.is_none_or(|id| !controlled.contains(&Some(id)))
        });
        compiled.resolved_flex_controller_ui.push(ui);
    }
    compiled.resolved_flex_rules.extend(rules);
    Ok(())
}

pub(crate) fn remap(model: &mut crate::model::CompiledModel, matrices: &[Matrix3x4]) {
    let Some(metadata) = &mut model.dmx else {
        return;
    };
    let mut weights = HashMap::new();
    for (mesh, ids) in model.meshes.iter().zip(&model.mesh_src_index) {
        for (vertex, &id) in mesh.vertices.iter().zip(ids) {
            weights.entry(id).or_insert(&vertex.bones);
        }
    }
    for shape in &mut metadata.shapes {
        for delta in &mut shape.vertices {
            let Some(bones) = weights.get(&delta.vertex) else {
                continue;
            };
            let mut position = [0.; 3];
            let mut normal = [0.; 3];
            for &[bone, weight] in *bones {
                if let Some(matrix) = matrices.get(bone as usize) {
                    let p = vector(matrix, delta.position, false);
                    let n = vector(matrix, delta.normal, false);
                    for i in 0..3 {
                        position[i] += p[i] * weight;
                        normal[i] += n[i] * weight;
                    }
                }
            }
            delta.position = position;
            delta.normal = normal;
        }
    }
}
