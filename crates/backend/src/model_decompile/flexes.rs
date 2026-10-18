use std::{fmt::Write as _, sync::atomic::AtomicBool};

use mdlc::{
    flex,
    model::Flex,
    vta::{Vta, VtaVert},
};
use vformats::mdl::{Mdl, Vvd};

#[path = "flexes/rules.rs"]
mod rules;
use rules::{expression, identifier};

#[path = "flexes/weights.rs"]
mod weights;
use weights::{absolute, infer_decay, infer_split};

#[path = "flexes/eyes.rs"]
mod eyes;

use super::{
    DecompileError, ExportFiles, MAX_OUTPUT_BYTES, check_cancelled, invalid, lods, quoted,
};

pub(super) struct Export {
    pub blocks: Vec<Vec<Option<String>>>,
    pub preserved: Vec<usize>,
}

#[derive(Clone, Debug, PartialEq)]
struct Delta {
    mesh: usize,
    index: usize,
    vertex: usize,
    speed: u8,
    side: u8,
    delta: [f32; 3],
    normal: [f32; 3],
}

#[derive(Clone, Debug, PartialEq)]
struct Shape {
    descriptor: usize,
    pair: usize,
    targets: [f32; 4],
    deltas: Vec<Delta>,
}

pub(super) fn write(
    export: &mut ExportFiles<'_>,
    raw: &[u8],
    model: &Mdl,
    vertices: &Vvd,
    lod: &lods::Metadata,
    cancelled: &AtomicBool,
) -> Result<Export, DecompileError> {
    let reader = Reader(raw);
    let descriptors = reader.names(260, 264, 4, 0)?;
    let controller_records = reader.table(268, 272, 20)?;
    let controllers = reader.names(268, 272, 20, 4)?;
    let rule_records = reader.table(276, 280, 12)?;
    let ranges = controller_records
        .iter()
        .map(|record| Ok((reader.float(record + 12)?, reader.float(record + 16)?)))
        .collect::<Result<Vec<_>, DecompileError>>()?;
    validate_names(&descriptors)?;
    validate_names(&controllers)?;
    let mut shared = super::metadata::mouth_options(raw, model)?;
    for name in &descriptors {
        writeln!(shared, "    localvar {}", quoted(name)?).unwrap();
    }
    for (record, name) in controller_records.iter().zip(&controllers) {
        let kind = reader.string(*record, 0)?;
        let min = reader.float(record + 12)?;
        let max = reader.float(record + 16)?;
        if min > max {
            return Err(invalid("A flex controller has reversed bounds"));
        }
        writeln!(
            shared,
            "    flexcontroller {} range {min} {max} {}",
            quoted(kind)?,
            quoted(name)?
        )
        .unwrap();
    }
    validate_ui(&reader, &controllers, &controller_records)?;
    for record in rule_records {
        let descriptor = reader.index(record, descriptors.len())?;
        let ops = reader.relative_table(record, 4, 8, 8)?;
        let expression = expression(&reader, &ops, &descriptors, &controllers, &ranges)?;
        writeln!(
            shared,
            "    %{} = {expression}",
            identifier(&descriptors[descriptor])?
        )
        .unwrap();
    }
    let mut blocks: Vec<Vec<Option<String>>> = model
        .body_parts
        .iter()
        .map(|part| vec![None; part.models.len()])
        .collect();
    let bodies = reader.table(232, 236, 16)?;
    if bodies.len() != model.body_parts.len() {
        return Err(invalid("The flex bodygroup table does not match the model"));
    }
    let mut shared_target = None;
    for (group, (part, body)) in model.body_parts.iter().zip(bodies).enumerate() {
        let models = reader.relative_table(body, 4, 12, 148)?;
        if models.len() != part.models.len() {
            return Err(invalid("The flex model table does not match the bodygroup"));
        }
        for (choice, (source, record)) in part.models.iter().zip(models).enumerate() {
            check_cancelled(cancelled)?;
            if source.meshes.is_empty() {
                continue;
            }
            if part.models.len() == 1 {
                shared_target.get_or_insert((group, choice));
            }
            let counts = &lod.mesh_vertices[group][choice];
            let shapes = read_shapes(&reader, record, source, counts, descriptors.len())?;
            let eyes = eyes::Eyes::read(&reader, record, model, &descriptors)?;
            if shapes.is_empty() && eyes.is_empty() {
                continue;
            }
            if part.models.len() != 1 {
                return Err(invalid(
                    "Facial flexes in a bodygroup with multiple choices cannot yet be represented by the compiler",
                ));
            }
            if model.flags & vformats::mdl::FLAG_STATIC_PROP != 0 {
                return Err(invalid(
                    "Facial flexes on a static prop cannot be exported without changing their coordinate space",
                ));
            }
            let filename = format!("flexes_{group}_{choice}.vta");
            let (block, vta) = reconstruct(
                &filename,
                &shapes,
                &descriptors,
                source,
                vertices,
                counts,
                cancelled,
            )?;
            export.write(&filename, vta.as_bytes())?;
            blocks[group][choice] = Some(eyes.apply(block, &filename, &shapes, &descriptors)?);
        }
    }
    if !shared.is_empty() {
        let (group, choice) = shared_target
            .ok_or_else(|| invalid("Flex declarations need a nonblank single-choice model"))?;
        let block = blocks[group][choice].get_or_insert_with(String::new);
        block.insert_str(0, &shared);
    }
    Ok(Export {
        blocks,
        preserved: vec![260, 268, 276, 292, 384],
    })
}

fn read_shapes(
    reader: &Reader<'_>,
    record: usize,
    source: &vformats::mdl::MdlModel,
    counts: &[usize],
    descriptor_count: usize,
) -> Result<Vec<Shape>, DecompileError> {
    let meshes = reader.relative_table(record, 72, 76, 116)?;
    if meshes.len() != source.meshes.len() || meshes.len() != counts.len() {
        return Err(invalid("The flex mesh table does not match the model"));
    }
    let mut shapes: Vec<Shape> = Vec::new();
    let mut vertex_base = 0usize;
    for (mesh, (record, count)) in meshes.into_iter().zip(counts).enumerate() {
        for record in reader.relative_table(record, 16, 20, 60)? {
            let descriptor = reader.index(record, descriptor_count)?;
            let pair = reader.count(record + 28)?;
            if pair != 0 && pair >= descriptor_count {
                return Err(invalid("A flex pair references an absent descriptor"));
            }
            let targets = [
                reader.float(record + 4)?,
                reader.float(record + 8)?,
                reader.float(record + 12)?,
                reader.float(record + 16)?,
            ];
            if targets.windows(2).any(|window| window[0] > window[1]) {
                return Err(invalid("A flex has reversed activation targets"));
            }
            if reader.byte(record + 32)? != 0 {
                return Err(invalid(
                    "Wrinkle vertex animations cannot yet be represented by the VTA compiler",
                ));
            }
            let key = shapes.iter().position(|shape| {
                shape.descriptor == descriptor && shape.pair == pair && shape.targets == targets
            });
            let shape = if let Some(index) = key {
                &mut shapes[index]
            } else {
                shapes.push(Shape {
                    descriptor,
                    pair,
                    targets,
                    deltas: Vec::new(),
                });
                shapes.last_mut().unwrap()
            };
            if shape.deltas.iter().any(|delta| delta.mesh == mesh) {
                return Err(invalid(
                    "A mesh repeats a flex with identical activation targets",
                ));
            }
            let mut seen = std::collections::HashSet::new();
            for vertex in reader.relative_table(record, 20, 24, 16)? {
                let index = usize::from(reader.u16(vertex)?);
                if index >= *count || !seen.insert(index) {
                    return Err(invalid(
                        "A flex has an invalid or repeated mesh vertex index",
                    ));
                }
                let delta = [
                    reader.half(vertex + 4)?,
                    reader.half(vertex + 6)?,
                    reader.half(vertex + 8)?,
                ];
                let normal = [
                    reader.half(vertex + 10)?,
                    reader.half(vertex + 12)?,
                    reader.half(vertex + 14)?,
                ];
                shape.deltas.push(Delta {
                    mesh,
                    index,
                    vertex: vertex_base + index,
                    speed: reader.byte(vertex + 2)?,
                    side: reader.byte(vertex + 3)?,
                    delta,
                    normal,
                });
            }
        }
        vertex_base = vertex_base
            .checked_add(*count)
            .ok_or(DecompileError::SizeLimit)?;
    }
    Ok(shapes)
}

fn reconstruct(
    filename: &str,
    shapes: &[Shape],
    descriptors: &[String],
    source: &vformats::mdl::MdlModel,
    vertices: &Vvd,
    counts: &[usize],
    cancelled: &AtomicBool,
) -> Result<(String, String), DecompileError> {
    let mut base = Vec::new();
    let mut model_vertices = Vec::new();
    let mut mesh_of_vertex = Vec::new();
    for (mesh, (source_mesh, count)) in source.meshes.iter().zip(counts).enumerate() {
        let offset = usize::try_from(source.vertex_offset)
            .ok()
            .and_then(|start| {
                usize::try_from(source_mesh.vertex_offset)
                    .ok()
                    .and_then(|offset| start.checked_add(offset))
            })
            .ok_or_else(|| invalid("Invalid flex vertex offset"))?;
        let end = offset
            .checked_add(*count)
            .ok_or(DecompileError::SizeLimit)?;
        let slice = vertices
            .vertices
            .get(offset..end)
            .ok_or_else(|| invalid("Flex vertices extend beyond the VVD"))?;
        for (index, vertex) in slice.iter().enumerate() {
            if !vertex
                .position
                .iter()
                .chain(&vertex.normal)
                .all(|value| value.is_finite())
            {
                return Err(invalid("Nonfinite flex base coordinates"));
            }
            base.push(VtaVert {
                index: base.len() as u32,
                pos: vertex.position,
                normal: vertex.normal,
            });
            model_vertices.push(mdlc::model::Vertex {
                pos: vertex.position,
                normal: vertex.normal,
                uv: vertex.uv,
                bones: vec![],
            });
            mesh_of_vertex.push((mesh, index as u32));
        }
    }
    let mut vta = Vta {
        start_frame: 0,
        end_frame: shapes.len() as i32,
        frames: vec![base],
        num_vertices: model_vertices.len(),
        warnings: vec![],
    };
    let mut specs = Vec::new();
    for (index, shape) in shapes.iter().enumerate() {
        check_cancelled(cancelled)?;
        let mut frame = Vec::new();
        for delta in &shape.deltas {
            let original = &vta.frames[0][delta.vertex];
            let mut vertex = *original;
            for axis in 0..3 {
                vertex.pos[axis] = absolute(original.pos[axis], delta.delta[axis])?;
                vertex.normal[axis] = absolute(original.normal[axis], delta.normal[axis])?;
            }
            frame.push(vertex);
        }
        let decay = infer_decay(shape, &vta.frames[0], &mut frame).map_err(|error| {
            invalid(format!("Flex {:?}: {error}", descriptors[shape.descriptor]))
        })?;
        let split = infer_split(shape, &frame).map_err(|error| {
            invalid(format!("Flex {:?}: {error}", descriptors[shape.descriptor]))
        })?;
        vta.frames.push(frame);
        specs.push(Flex {
            vta: filename.to_owned(),
            name: descriptors[shape.descriptor].clone(),
            frame: index as i32 + 1,
            pair: shape.pair != 0,
            split,
            position: shape.targets[1],
            decay,
            targets: Some(shape.targets),
            from_eyelid: false,
        });
    }
    let mapping = flex::build_vanim_map(&vta, &model_vertices);
    let mut block = format!("    flexfile {}\n", quoted(filename)?);
    let mut consumed = vec![false; shapes.len()];
    for (index, (shape, spec)) in shapes.iter().zip(&specs).enumerate() {
        check_cancelled(cancelled)?;
        let resolved = flex::resolve_flex_mapped(
            spec,
            &vta,
            &mapping,
            &mesh_of_vertex,
            (shape.descriptor as i32, shape.pair as i32),
            index,
        )
        .map_err(|error| invalid(error.to_string()))?;
        let count = resolved
            .iter()
            .map(|mesh| {
                mesh.flexes
                    .iter()
                    .map(|shape| shape.vertanims.len())
                    .sum::<usize>()
            })
            .sum::<usize>();
        let payload = resolved
            .iter()
            .enumerate()
            .flat_map(|(mesh, data)| {
                data.flexes.iter().flat_map(move |shape| {
                    shape
                        .vertanims
                        .iter()
                        .map(move |value| ((mesh, value.index as usize), value))
                })
            })
            .collect::<std::collections::HashMap<_, _>>();
        if count != shape.deltas.len()
            || payload.len() != count
            || shape.deltas.iter().any(|delta| {
                !payload
                    .get(&(delta.mesh, delta.index))
                    .is_some_and(|value| {
                        value.speed == delta.speed
                            && value.side == delta.side
                            && same_half(value.delta, delta.delta)
                            && same_half(value.ndelta, delta.normal)
                    })
            })
        {
            return Err(invalid(format!(
                "Flex {:?} cannot be reconstructed without changing its sparse vertex deltas, speed or side weights",
                descriptors[shape.descriptor]
            )));
        }
        if consumed[index] {
            continue;
        }
        let eyelid = shapes.iter().any(|other| {
            other.descriptor == shape.descriptor
                && other.pair == 0
                && other.targets[0] == -11.0
                && other.targets[1] == -10.0
        });
        if !eyelid {
            let mut keys = shapes
                .iter()
                .enumerate()
                .filter_map(|(index, other)| {
                    (other.descriptor == shape.descriptor && other.pair == shape.pair)
                        .then_some(index)
                })
                .collect::<Vec<_>>();
            keys.sort_by(|a, b| shapes[*a].targets[1].total_cmp(&shapes[*b].targets[1]));
            for (ordinal, &key) in keys.iter().enumerate() {
                let current = &shapes[key];
                let position = current.targets[1];
                let expected = [
                    ordinal
                        .checked_sub(1)
                        .map_or(0.0, |previous| shapes[keys[previous]].targets[1]),
                    position,
                    if ordinal + 1 < keys.len() {
                        position
                    } else {
                        10.0
                    },
                    keys.get(ordinal + 1)
                        .map_or(11.0, |next| shapes[*next].targets[1]),
                ];
                if current.targets != expected {
                    return Err(invalid(format!(
                        "Flex {:?} activation targets cannot be reconstructed with consecutive QC positions",
                        descriptors[current.descriptor]
                    )));
                }
                let spec = &specs[key];
                if current.pair == 0 {
                    writeln!(
                        block,
                        "    flex {} frame {} position {} decay {}",
                        quoted(&spec.name)?,
                        spec.frame,
                        spec.position,
                        spec.decay
                    )
                    .unwrap();
                } else {
                    let name = spec
                        .name
                        .strip_suffix('R')
                        .filter(|base| descriptors[current.pair] == format!("{base}L"))
                        .ok_or_else(|| {
                            invalid("A flex pair does not use the standard R/L descriptor names")
                        })?;
                    writeln!(
                        block,
                        "    flexpair {} 1 frame {} position {} split {} decay {}",
                        quoted(name)?,
                        spec.frame,
                        spec.position,
                        spec.split,
                        spec.decay
                    )
                    .unwrap();
                }
                consumed[key] = true;
            }
        } else {
            let lower = shapes.iter().position(|other| {
                other.descriptor == shape.descriptor
                    && other.pair == 0
                    && other.targets[0] == -11.0
                    && other.targets[1] == -10.0
            });
            let upper = shapes.iter().position(|other| {
                other.descriptor == shape.descriptor
                    && other.pair == 0
                    && other.targets[2] == 10.0
                    && other.targets[3] == 11.0
            });
            let (lower, upper) = lower.zip(upper).ok_or_else(|| {
                invalid(
                    "A flex activation window is not representable by standard flex or eyelid QC",
                )
            })?;
            let low = shapes[lower].targets[2];
            let neutral = shapes[lower].targets[3];
            let high = shapes[upper].targets[1];
            let middle = shapes.iter().position(|other| {
                other.descriptor == shape.descriptor
                    && other.pair == 0
                    && other.targets == [low, neutral, neutral, high]
            });
            if shapes[upper].targets[0] != neutral
                || [lower, upper]
                    .into_iter()
                    .chain(middle)
                    .any(|i| specs[i].decay != 0.0 || specs[i].split != 0.0)
            {
                return Err(invalid(
                    "An eyelid cannot be reconstructed with its original activation targets and vertex weights",
                ));
            }
            for suffix in ["lowerer", "neutral", "raiser"] {
                if !descriptors.contains(&format!("{}_{}", spec.name, suffix)) {
                    return Err(invalid(
                        "An eyelid is missing its standard control descriptors",
                    ));
                }
            }
            writeln!(
                block,
                "    eyelid {} {} lowerer {} {low} neutral {} {neutral} raiser {} {high}",
                quoted(&spec.name)?,
                quoted(filename)?,
                specs[lower].frame,
                middle.map_or(0, |i| specs[i].frame),
                specs[upper].frame
            )
            .unwrap();
            consumed[lower] = true;
            consumed[upper] = true;
            if let Some(middle) = middle {
                consumed[middle] = true;
            }
        }
    }
    let mut text = String::from("version 1\nnodes\n0 \"root\" -1\nend\nskeleton\n");
    for index in 0..vta.frames.len() {
        writeln!(text, "time {index}\n0 0 0 0 0 0 0").unwrap();
    }
    text.push_str("end\nvertexanimation\n");
    for (index, frame) in vta.frames.iter().enumerate() {
        check_cancelled(cancelled)?;
        writeln!(text, "time {index}").unwrap();
        for vertex in frame {
            writeln!(
                text,
                "{} {} {} {} {} {} {}",
                vertex.index,
                vertex.pos[0],
                vertex.pos[1],
                vertex.pos[2],
                vertex.normal[0],
                vertex.normal[1],
                vertex.normal[2]
            )
            .unwrap();
        }
        if text.len() > MAX_OUTPUT_BYTES {
            return Err(DecompileError::SizeLimit);
        }
    }
    text.push_str("end\n");
    Ok((block, text))
}

fn same_half(left: [f32; 3], right: [f32; 3]) -> bool {
    left.into_iter().zip(right).all(|(left, right)| {
        left == right
            || mdlc::anim_writer::float_to_half_public(left)
                == mdlc::anim_writer::float_to_half_public(right)
    })
}

fn validate_names(names: &[String]) -> Result<(), DecompileError> {
    let mut seen = std::collections::HashSet::new();
    for name in names {
        quoted(name)?;
        if name.is_empty() || !seen.insert(name.to_ascii_lowercase()) {
            return Err(invalid(
                "Flex names must be nonempty and unique regardless of case",
            ));
        }
    }
    Ok(())
}

fn validate_ui(
    reader: &Reader<'_>,
    controllers: &[String],
    records: &[usize],
) -> Result<(), DecompileError> {
    let ui = reader.table(384, 388, 20)?;
    if ui.is_empty() {
        return Ok(());
    }
    let mut expected = Vec::new();
    let mut index = 0;
    while index < controllers.len() {
        let pair = controllers[index].strip_prefix("right_").and_then(|name| {
            controllers
                .get(index + 1)
                .and_then(|other| other.strip_prefix("left_"))
                .filter(|other| *other == name)
                .map(|_| name)
        });
        if let Some(name) = pair {
            expected.push((name, index + 1, Some(index)));
            index += 2;
        } else {
            expected.push((controllers[index].as_str(), index, None));
            index += 1;
        }
    }
    if ui.len() != expected.len() {
        return Err(invalid(
            "Custom flex controller UI cannot yet be reconstructed as standard QC",
        ));
    }
    for (record, (name, left, right)) in ui.into_iter().zip(expected) {
        let actual_right = if reader.signed(record + 8)? == 0 {
            None
        } else {
            Some(reader.relative(record, 8)?)
        };
        if reader.string(record, 0)? != name
            || reader.relative(record, 4)? != records[left]
            || actual_right != right.map(|index| records[index])
            || reader.signed(record + 12)? != 0
            || reader.byte(record + 16)? != 0
            || reader.byte(record + 17)? != u8::from(right.is_some())
        {
            return Err(invalid(
                "Custom flex controller UI cannot yet be reconstructed as standard QC",
            ));
        }
    }
    Ok(())
}

struct Reader<'a>(&'a [u8]);
impl<'a> Reader<'a> {
    fn byte(&self, offset: usize) -> Result<u8, DecompileError> {
        self.0
            .get(offset)
            .copied()
            .ok_or_else(|| invalid("Truncated flex record"))
    }
    fn u16(&self, offset: usize) -> Result<u16, DecompileError> {
        Ok(u16::from_le_bytes(
            self.0
                .get(offset..offset + 2)
                .ok_or_else(|| invalid("Truncated flex vertex animation"))?
                .try_into()
                .unwrap(),
        ))
    }
    fn signed(&self, offset: usize) -> Result<i32, DecompileError> {
        Ok(i32::from_le_bytes(
            self.0
                .get(offset..offset + 4)
                .ok_or_else(|| invalid("Truncated flex table"))?
                .try_into()
                .unwrap(),
        ))
    }
    fn count(&self, offset: usize) -> Result<usize, DecompileError> {
        let value = usize::try_from(self.signed(offset)?)
            .map_err(|_| invalid("Negative flex table count or index"))?;
        if value > 1_000_000 {
            return Err(DecompileError::SizeLimit);
        }
        Ok(value)
    }
    fn float(&self, offset: usize) -> Result<f32, DecompileError> {
        let value = f32::from_bits(self.signed(offset)? as u32);
        if !value.is_finite() {
            return Err(invalid("Nonfinite flex value"));
        }
        Ok(value)
    }
    fn half(&self, offset: usize) -> Result<f32, DecompileError> {
        let value = half(self.u16(offset)?);
        if !value.is_finite() {
            return Err(invalid("Nonfinite flex vertex delta"));
        }
        Ok(value)
    }
    fn index(&self, offset: usize, count: usize) -> Result<usize, DecompileError> {
        let index = self.count(offset)?;
        if index >= count {
            return Err(invalid(
                "A flex references an absent descriptor or controller",
            ));
        }
        Ok(index)
    }
    fn relative(&self, base: usize, field: usize) -> Result<usize, DecompileError> {
        let start = base
            .checked_add_signed(self.signed(base + field)? as isize)
            .ok_or_else(|| invalid("Invalid flex relative offset"))?;
        if start > self.0.len() {
            return Err(invalid("Flex relative offset extends beyond the MDL"));
        }
        Ok(start)
    }
    fn range(
        &self,
        start: usize,
        count: usize,
        stride: usize,
    ) -> Result<Vec<usize>, DecompileError> {
        let length = count.checked_mul(stride).ok_or(DecompileError::SizeLimit)?;
        if start
            .checked_add(length)
            .is_none_or(|end| end > self.0.len())
        {
            return Err(invalid("Flex table extends beyond the MDL"));
        }
        Ok((0..count).map(|index| start + index * stride).collect())
    }
    fn table(
        &self,
        count: usize,
        offset: usize,
        stride: usize,
    ) -> Result<Vec<usize>, DecompileError> {
        self.range(
            usize::try_from(self.signed(offset)?)
                .map_err(|_| invalid("Negative flex table offset"))?,
            self.count(count)?,
            stride,
        )
    }
    fn relative_table(
        &self,
        base: usize,
        count: usize,
        offset: usize,
        stride: usize,
    ) -> Result<Vec<usize>, DecompileError> {
        self.range(
            self.relative(base, offset)?,
            self.count(base + count)?,
            stride,
        )
    }
    fn string(&self, base: usize, field: usize) -> Result<&'a str, DecompileError> {
        let start = self.relative(base, field)?;
        let bytes = &self.0[start..];
        let end = bytes
            .iter()
            .take(1025)
            .position(|byte| *byte == 0)
            .ok_or_else(|| invalid("Unterminated flex name"))?;
        std::str::from_utf8(&bytes[..end]).map_err(|_| invalid("Invalid UTF-8 flex name"))
    }
    fn names(
        &self,
        count: usize,
        offset: usize,
        stride: usize,
        field: usize,
    ) -> Result<Vec<String>, DecompileError> {
        self.table(count, offset, stride)?
            .into_iter()
            .map(|record| self.string(record, field).map(str::to_owned))
            .collect()
    }
}

fn half(bits: u16) -> f32 {
    let sign = if bits & 0x8000 == 0 { 1.0 } else { -1.0 };
    let exponent = (bits >> 10) & 31;
    let fraction = bits & 1023;
    sign * match exponent {
        0 => f32::from(fraction) * 2.0f32.powi(-24),
        31 => {
            if fraction == 0 {
                f32::INFINITY
            } else {
                f32::NAN
            }
        }
        _ => (1.0 + f32::from(fraction) / 1024.0) * 2.0f32.powi(i32::from(exponent) - 15),
    }
}

#[cfg(test)]
#[path = "flexes_tests.rs"]
mod tests;
