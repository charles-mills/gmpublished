use std::sync::atomic::AtomicBool;

use serde_json::{Value, json};
use vformats::mdl::Mdl;

use super::{DecompileError, Document, check_cancelled, direction, invalid, position};

struct Delta {
    mesh: usize,
    vertex: usize,
    side: u8,
    position: [f32; 3],
    normal: [f32; 3],
}

pub(super) struct Shape {
    pub name: String,
    pub metadata: Value,
    descriptor: usize,
    pair: usize,
    targets: [f32; 4],
    side: Option<bool>,
    deltas: Vec<Delta>,
}

pub(super) fn metadata(raw: &[u8]) -> Result<Value, DecompileError> {
    let reader = Reader(raw);
    let descriptors = names(&reader)?;
    let controllers = reader
        .limited_table(0, 268, 272, 20, 4096)?
        .into_iter()
        .map(|record| {
            Ok(json!({
                "name": reader.string(record, 4)?,
                "type": reader.string(record, 0)?,
                "minimum": reader.float(record + 12)?,
                "maximum": reader.float(record + 16)?,
            }))
        })
        .collect::<Result<Vec<_>, DecompileError>>()?;
    let mut operation_count = 0usize;
    let rules = reader
        .limited_table(0, 276, 280, 12, 32768)?
        .into_iter()
        .map(|record| {
            let records = reader.limited_table(
                record,
                4,
                8,
                8,
                1_000_000usize.saturating_sub(operation_count),
            )?;
            operation_count += records.len();
            let operations = records
                .into_iter()
                .map(|operation| {
                    Ok(json!({
                        "opcode": reader.count(operation)?,
                        "operandBits": u32::from_le_bytes(reader.bytes(operation + 4)?),
                    }))
                })
                .collect::<Result<Vec<_>, DecompileError>>()?;
            Ok(json!({"descriptor":reader.count(record)?,"operations":operations}))
        })
        .collect::<Result<Vec<_>, DecompileError>>()?;
    Ok(json!({"descriptors":descriptors,"controllers":controllers,"rules":rules}))
}

pub(super) fn read(
    raw: &[u8],
    model: &Mdl,
    group: usize,
    choice: usize,
    counts: &[usize],
    cancelled: &AtomicBool,
) -> Result<Vec<Shape>, DecompileError> {
    let reader = Reader(raw);
    let descriptors = names(&reader)?;
    let body = *reader
        .table(0, 232, 236, 16)?
        .get(group)
        .ok_or_else(|| invalid("The flex bodygroup is absent"))?;
    let model_record = *reader
        .table(body, 4, 12, 148)?
        .get(choice)
        .ok_or_else(|| invalid("The flex bodygroup choice is absent"))?;
    let meshes = reader.table(model_record, 72, 76, 116)?;
    if meshes.len() != counts.len() {
        return Err(invalid("The flex mesh table is inconsistent"));
    }
    let scale = if model.flags & (1 << 21) != 0 {
        reader.float(0x188)?
    } else {
        1.0 / 4096.0
    };
    if scale <= 0.0 {
        return Err(invalid("The flex fixed-point scale is invalid"));
    }
    let mut shapes: Vec<Shape> = Vec::new();
    let mut delta_count = 0usize;
    let mut metadata_count = 0usize;
    for (mesh, (record, &count)) in meshes.into_iter().zip(counts).enumerate() {
        for flex in reader.table(record, 16, 20, 60)? {
            check_cancelled(cancelled)?;
            let descriptor = reader.count(flex)?;
            let pair = reader.count(flex + 28)?;
            if descriptor >= descriptors.len() || (pair != 0 && pair >= descriptors.len()) {
                return Err(invalid("A flex references an absent descriptor"));
            }
            let targets = [
                reader.float(flex + 4)?,
                reader.float(flex + 8)?,
                reader.float(flex + 12)?,
                reader.float(flex + 16)?,
            ];
            if targets.windows(2).any(|pair| pair[0] > pair[1]) {
                return Err(invalid("A flex has reversed activation targets"));
            }
            let kind = reader.byte(flex + 32)?;
            let size = match kind {
                0 => 16,
                1 => 18,
                _ => return Err(invalid("Unknown vertex animation type")),
            };
            let shape = if let Some(index) = shapes.iter().position(|shape| {
                shape.descriptor == descriptor && shape.pair == pair && shape.targets == targets
            }) {
                &mut shapes[index]
            } else {
                if shapes.len() >= 4096 {
                    return Err(DecompileError::SizeLimit);
                }
                shapes.push(Shape {
                    name: descriptors[descriptor].clone(),
                    metadata: json!({
                        "descriptor": descriptor,
                        "pair": pair,
                        "activationTargets": targets,
                        "vertexTiming": [],
                        "wrinkles": [],
                    }),
                    descriptor,
                    pair,
                    targets,
                    side: None,
                    deltas: vec![],
                });
                shapes.last_mut().unwrap()
            };
            if shape.deltas.iter().any(|delta| delta.mesh == mesh) {
                return Err(invalid(
                    "A mesh repeats a flex with identical activation targets",
                ));
            }
            let mut seen = std::collections::HashSet::new();
            let vertices = reader.table(flex, 20, 24, size)?;
            delta_count = delta_count
                .checked_add(vertices.len())
                .filter(|count| *count <= 2_000_000)
                .ok_or(DecompileError::SizeLimit)?;
            for (vertex_index, vertex) in vertices.into_iter().enumerate() {
                if vertex_index.is_multiple_of(4096) {
                    check_cancelled(cancelled)?;
                }
                let index = usize::from(reader.u16(vertex)?);
                if index >= count || !seen.insert(index) {
                    return Err(invalid("A flex references an invalid or repeated vertex"));
                }
                let speed = reader.byte(vertex + 2)?;
                let side = reader.byte(vertex + 3)?;
                if speed != 255 {
                    metadata_count += 1;
                    if metadata_count > 100_000 {
                        return Err(DecompileError::SizeLimit);
                    }
                    shape.metadata["vertexTiming"]
                        .as_array_mut()
                        .unwrap()
                        .push(json!([mesh, index, speed]));
                }
                if kind == 1 {
                    let wrinkle = f32::from(reader.u16(vertex + 16)? as i16) * scale;
                    if !wrinkle.is_finite() {
                        return Err(invalid("A wrinkle delta is nonfinite"));
                    }
                    if wrinkle != 0.0 {
                        metadata_count += 1;
                        if metadata_count > 100_000 {
                            return Err(DecompileError::SizeLimit);
                        }
                        shape.metadata["wrinkles"]
                            .as_array_mut()
                            .unwrap()
                            .push(json!([mesh, index, wrinkle]));
                    }
                }
                shape.deltas.push(Delta {
                    mesh,
                    vertex: index,
                    side,
                    position: [
                        reader.half(vertex + 4)?,
                        reader.half(vertex + 6)?,
                        reader.half(vertex + 8)?,
                    ],
                    normal: [
                        reader.half(vertex + 10)?,
                        reader.half(vertex + 12)?,
                        reader.half(vertex + 14)?,
                    ],
                });
            }
        }
    }
    let mut split = Vec::new();
    for shape in shapes {
        if shape.pair == 0 || shape.pair == shape.descriptor {
            split.push(shape);
            continue;
        }
        for (side, descriptor) in [(false, shape.descriptor), (true, shape.pair)] {
            let mut metadata = shape.metadata.clone();
            metadata["stereoSide"] = json!(if side { "pair" } else { "primary" });
            split.push(Shape {
                name: descriptors[descriptor].clone(),
                metadata,
                descriptor: shape.descriptor,
                pair: shape.pair,
                targets: shape.targets,
                side: Some(side),
                deltas: shape
                    .deltas
                    .iter()
                    .map(|delta| Delta {
                        mesh: delta.mesh,
                        vertex: delta.vertex,
                        side: delta.side,
                        position: delta.position,
                        normal: delta.normal,
                    })
                    .collect(),
            });
        }
    }
    let mut names = std::collections::HashSet::new();
    for (index, shape) in split.iter_mut().enumerate() {
        if !names.insert(shape.name.clone()) {
            shape.name = format!("{} ({index})", shape.name);
            while !names.insert(shape.name.clone()) {
                shape.name.push('_');
            }
        }
    }
    Ok(split)
}

pub(super) fn targets(
    output: &mut Document,
    shapes: &[Shape],
    mesh: usize,
    source_vertices: &[usize],
    cancelled: &AtomicBool,
) -> Result<Vec<Value>, DecompileError> {
    let mut result = Vec::with_capacity(shapes.len());
    let count = source_vertices.len();
    for shape in shapes {
        check_cancelled(cancelled)?;
        if count
            .checked_mul(24)
            .is_none_or(|size| size > super::MAX_OUTPUT_BYTES.saturating_sub(output.bin.len()))
        {
            return Err(DecompileError::SizeLimit);
        }
        let mut positions = vec![[0.0f32; 3]; count];
        let mut normals = positions.clone();
        let deltas = shape
            .deltas
            .iter()
            .filter(|delta| delta.mesh == mesh)
            .map(|delta| (delta.vertex, delta))
            .collect::<std::collections::HashMap<_, _>>();
        for (vertex, source) in source_vertices.iter().enumerate() {
            let Some(delta) = deltas.get(source) else {
                continue;
            };
            let fraction = f32::from(delta.side) / 255.0;
            let weight = match shape.side {
                Some(true) => fraction,
                Some(false) => 1.0 - fraction,
                None => 1.0,
            };
            positions[vertex] = position(delta.position).map(|value| value * weight);
            normals[vertex] = direction(delta.normal).map(|value| value * weight);
        }
        result.push(json!({"POSITION":output.floats(&positions,"VEC3",true,Some(34962))?,"NORMAL":output.floats(&normals,"VEC3",false,Some(34962))?}));
    }
    Ok(result)
}

pub(super) struct Reader<'a>(pub(super) &'a [u8]);

fn names(reader: &Reader<'_>) -> Result<Vec<String>, DecompileError> {
    let records = reader.limited_table(0, 260, 264, 4, 4096)?;
    let names = records
        .into_iter()
        .map(|record| reader.string(record, 0))
        .collect::<Result<Vec<_>, _>>()?;
    if names.iter().any(String::is_empty) {
        return Err(invalid("A flex descriptor has an empty name"));
    }
    Ok(names)
}

impl Reader<'_> {
    pub(super) fn bytes<const N: usize>(&self, offset: usize) -> Result<[u8; N], DecompileError> {
        self.0
            .get(offset..offset.checked_add(N).ok_or(DecompileError::SizeLimit)?)
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or_else(|| invalid("Truncated flex data"))
    }
    fn byte(&self, offset: usize) -> Result<u8, DecompileError> {
        Ok(self.bytes::<1>(offset)?[0])
    }
    fn u16(&self, offset: usize) -> Result<u16, DecompileError> {
        Ok(u16::from_le_bytes(self.bytes(offset)?))
    }
    pub(super) fn count(&self, offset: usize) -> Result<usize, DecompileError> {
        usize::try_from(i32::from_le_bytes(self.bytes(offset)?))
            .map_err(|_| invalid("Negative flex count or offset"))
    }
    pub(super) fn float(&self, offset: usize) -> Result<f32, DecompileError> {
        let value = f32::from_le_bytes(self.bytes(offset)?);
        if !value.is_finite() {
            return Err(invalid("Nonfinite flex data"));
        }
        Ok(value)
    }
    fn half(&self, offset: usize) -> Result<f32, DecompileError> {
        let bits = self.u16(offset)?;
        let sign = if bits & 0x8000 != 0 { -1.0 } else { 1.0 };
        let exponent = (bits >> 10) & 31;
        let fraction = f32::from(bits & 1023);
        let value = if exponent == 0 {
            sign * fraction * 2.0f32.powi(-24)
        } else {
            sign * (1.0 + fraction / 1024.0) * 2.0f32.powi(i32::from(exponent) - 15)
        };
        if exponent == 31 || !value.is_finite() {
            return Err(invalid("A flex delta is nonfinite"));
        }
        Ok(value)
    }
    fn table(
        &self,
        base: usize,
        count: usize,
        offset: usize,
        size: usize,
    ) -> Result<Vec<usize>, DecompileError> {
        let count = self.count(base.checked_add(count).ok_or(DecompileError::SizeLimit)?)?;
        if count > 2_000_000 {
            return Err(DecompileError::SizeLimit);
        }
        if count == 0 {
            return Ok(vec![]);
        }
        let start = self.relative(base, offset)?;
        let end = count
            .checked_mul(size)
            .and_then(|bytes| start.checked_add(bytes))
            .ok_or(DecompileError::SizeLimit)?;
        if end > self.0.len() {
            return Err(invalid("A flex table extends beyond the MDL"));
        }
        Ok((0..count).map(|index| start + index * size).collect())
    }
    pub(super) fn limited_table(
        &self,
        base: usize,
        count: usize,
        offset: usize,
        size: usize,
        limit: usize,
    ) -> Result<Vec<usize>, DecompileError> {
        if self.count(base.checked_add(count).ok_or(DecompileError::SizeLimit)?)? > limit {
            return Err(DecompileError::SizeLimit);
        }
        self.table(base, count, offset, size)
    }
    pub(super) fn string(&self, base: usize, field: usize) -> Result<String, DecompileError> {
        let start = self.relative(base, field)?;
        let bytes = self
            .0
            .get(start..)
            .ok_or_else(|| invalid("A flex name is outside the MDL"))?;
        let end = bytes
            .iter()
            .take(4097)
            .position(|byte| *byte == 0)
            .ok_or_else(|| invalid("Unterminated or oversized flex name"))?;
        let name =
            std::str::from_utf8(&bytes[..end]).map_err(|_| invalid("A flex name is not UTF-8"))?;
        Ok(name.into())
    }
    fn relative(&self, base: usize, field: usize) -> Result<usize, DecompileError> {
        let offset = i32::from_le_bytes(
            self.bytes(base.checked_add(field).ok_or(DecompileError::SizeLimit)?)?,
        );
        base.checked_add_signed(offset as isize)
            .ok_or_else(|| invalid("A flex offset is outside the MDL"))
    }
}
