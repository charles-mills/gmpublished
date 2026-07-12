//! Source engine particle file (.pcf) parser.
//!
//! PCF files are binary DMX documents whose root holds an array of
//! `DmeParticleSystemDefinition` elements. Each definition carries scalar
//! base properties plus operator lists (emitters, initializers, operators,
//! renderers, forces, constraints) and child-system references. The DMX
//! layer here is deliberately generic; the PCF layer extracts only the
//! particle schema.

use std::collections::HashMap;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum PcfError {
    #[error("ERR_PCF_NOT_DMX")]
    NotDmx,
    #[error("ERR_PCF_TEXT_ENCODING_UNSUPPORTED")]
    TextEncodingUnsupported,
    #[error("ERR_PCF_UNSUPPORTED_ENCODING_VERSION: {0}")]
    UnsupportedEncodingVersion(u32),
    #[error("ERR_PCF_TRUNCATED")]
    Truncated,
    #[error("ERR_PCF_MALFORMED: {0}")]
    Malformed(&'static str),
}

/// A parsed particle attribute value. DMX arrays that particle definitions
/// never use in practice are preserved generically so unknown operators can
/// still be listed by name.
#[derive(Debug, Clone, PartialEq)]
pub enum PcfValue {
    Int(i32),
    Float(f32),
    Bool(bool),
    String(String),
    Binary(Vec<u8>),
    /// Seconds. Stored in DMX as integer ten-thousandths.
    Time(f32),
    Color([u8; 4]),
    Vector2([f32; 2]),
    Vector3([f32; 3]),
    Vector4([f32; 4]),
    Angle([f32; 3]),
    Quaternion([f32; 4]),
    Matrix(Box<[f32; 16]>),
    Array(Vec<Self>),
}

/// Attribute bag with Source-style case-insensitive lookup.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PcfAttributes {
    entries: Vec<(String, PcfValue)>,
}

impl PcfAttributes {
    /// Appends an attribute; later entries never shadow earlier ones because
    /// lookup is first-match, mirroring Source.
    pub fn push(&mut self, name: impl Into<String>, value: PcfValue) {
        self.entries.push((name.into(), value));
    }

    pub fn get(&self, name: &str) -> Option<&PcfValue> {
        self.entries
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value)
    }

    pub fn get_float(&self, name: &str) -> Option<f32> {
        match self.get(name)? {
            PcfValue::Float(value) | PcfValue::Time(value) => Some(*value),
            PcfValue::Int(value) => Some(*value as f32),
            _ => None,
        }
    }

    pub fn get_int(&self, name: &str) -> Option<i32> {
        match self.get(name)? {
            PcfValue::Int(value) => Some(*value),
            PcfValue::Float(value) => Some(*value as i32),
            _ => None,
        }
    }

    pub fn get_bool(&self, name: &str) -> Option<bool> {
        match self.get(name)? {
            PcfValue::Bool(value) => Some(*value),
            PcfValue::Int(value) => Some(*value != 0),
            _ => None,
        }
    }

    pub fn get_string(&self, name: &str) -> Option<&str> {
        match self.get(name)? {
            PcfValue::String(value) => Some(value.as_str()),
            _ => None,
        }
    }

    pub fn get_vector3(&self, name: &str) -> Option<[f32; 3]> {
        match self.get(name)? {
            PcfValue::Vector3(value) | PcfValue::Angle(value) => Some(*value),
            _ => None,
        }
    }

    pub fn get_color(&self, name: &str) -> Option<[u8; 4]> {
        match self.get(name)? {
            PcfValue::Color(value) => Some(*value),
            _ => None,
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &PcfValue)> {
        self.entries
            .iter()
            .map(|(key, value)| (key.as_str(), value))
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// One operator instance: the Source function name ("emit_continuously",
/// "alpha_fade_out_random", ...) plus its parameter attributes.
#[derive(Debug, Clone, PartialEq)]
pub struct PcfFunction {
    pub name: String,
    pub attributes: PcfAttributes,
}

/// Reference to a child system spawned alongside a parent.
#[derive(Debug, Clone, PartialEq)]
pub struct PcfChild {
    pub name: String,
    /// Index into [`PcfFile::systems`] when the child definition lives in the
    /// same file (the overwhelmingly common case).
    pub system_index: Option<usize>,
    pub delay: f32,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct PcfSystem {
    pub name: String,
    /// Base properties: max_particles, material, radius, color, ...
    pub attributes: PcfAttributes,
    pub emitters: Vec<PcfFunction>,
    pub initializers: Vec<PcfFunction>,
    pub operators: Vec<PcfFunction>,
    pub renderers: Vec<PcfFunction>,
    pub forces: Vec<PcfFunction>,
    pub constraints: Vec<PcfFunction>,
    pub children: Vec<PcfChild>,
}

impl PcfSystem {
    pub fn material(&self) -> Option<&str> {
        self.attributes.get_string("material")
    }

    pub fn max_particles(&self) -> u32 {
        self.attributes
            .get_int("max_particles")
            .map_or(1000, |value| value.clamp(1, 50_000) as u32)
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct PcfFile {
    pub encoding_version: u32,
    pub format_version: u32,
    pub systems: Vec<PcfSystem>,
}

pub fn parse_pcf(bytes: &[u8]) -> Result<PcfFile, PcfError> {
    let document = DmxDocument::parse(bytes)?;
    Ok(extract_particle_systems(&document))
}

// --- DMX binary layer ---------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum DmxValue {
    /// Index into the document element list; `None` for null references.
    Element(Option<u32>),
    Value(PcfValue),
    ElementArray(Vec<Option<u32>>),
}

#[derive(Debug, Clone)]
struct DmxElement {
    type_name: String,
    name: String,
    attributes: Vec<(String, DmxValue)>,
}

struct DmxDocument {
    encoding_version: u32,
    format_version: u32,
    elements: Vec<DmxElement>,
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, len: usize) -> Result<&'a [u8], PcfError> {
        let end = self.pos.checked_add(len).ok_or(PcfError::Truncated)?;
        let slice = self.bytes.get(self.pos..end).ok_or(PcfError::Truncated)?;
        self.pos = end;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8, PcfError> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, PcfError> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }

    fn i32(&mut self) -> Result<i32, PcfError> {
        Ok(i32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn u32(&mut self) -> Result<u32, PcfError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn f32(&mut self) -> Result<f32, PcfError> {
        Ok(f32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn f32s<const N: usize>(&mut self) -> Result<[f32; N], PcfError> {
        let mut out = [0.0; N];
        for value in &mut out {
            *value = self.f32()?;
        }
        Ok(out)
    }

    /// Null-terminated string; lossy UTF-8 keeps odd editor exports readable.
    fn cstr(&mut self) -> Result<String, PcfError> {
        let start = self.pos;
        let nul = self.bytes[start..]
            .iter()
            .position(|&byte| byte == 0)
            .ok_or(PcfError::Truncated)?;
        let slice = &self.bytes[start..start + nul];
        self.pos = start + nul + 1;
        Ok(String::from_utf8_lossy(slice).into_owned())
    }

    /// Upper bound for count-prefixed reads: each item consumes at least
    /// `min_item_bytes`, so a count beyond that is a corrupt file.
    fn check_count(&self, count: u32, min_item_bytes: usize) -> Result<usize, PcfError> {
        let remaining = self.bytes.len() - self.pos;
        let count = count as usize;
        if min_item_bytes != 0 && count > remaining / min_item_bytes {
            return Err(PcfError::Malformed("count exceeds remaining bytes"));
        }
        Ok(count)
    }
}

struct StringDict {
    strings: Vec<String>,
    wide_index: bool,
}

impl StringDict {
    fn read(reader: &mut Reader<'_>, encoding_version: u32) -> Result<Option<Self>, PcfError> {
        if encoding_version < 2 {
            return Ok(None);
        }
        let count = if encoding_version >= 4 {
            reader.u32()?
        } else {
            u32::from(reader.u16()?)
        };
        let count = reader.check_count(count, 1)?;
        let mut strings = Vec::with_capacity(count);
        for _ in 0..count {
            strings.push(reader.cstr()?);
        }
        Ok(Some(Self {
            strings,
            wide_index: encoding_version >= 5,
        }))
    }

    fn get(&self, reader: &mut Reader<'_>) -> Result<String, PcfError> {
        let index = if self.wide_index {
            reader.u32()? as usize
        } else {
            usize::from(reader.u16()?)
        };
        self.strings
            .get(index)
            .cloned()
            .ok_or(PcfError::Malformed("string table index out of range"))
    }
}

impl DmxDocument {
    fn parse(bytes: &[u8]) -> Result<Self, PcfError> {
        let mut reader = Reader { bytes, pos: 0 };
        let header = reader.cstr()?;
        let (encoding, encoding_version, format_version) = parse_header(&header)?;
        if encoding != "binary" {
            return Err(PcfError::TextEncodingUnsupported);
        }
        if !(1..=5).contains(&encoding_version) {
            return Err(PcfError::UnsupportedEncodingVersion(encoding_version));
        }

        let dict = StringDict::read(&mut reader, encoding_version)?;
        let read_dict_string =
            |reader: &mut Reader<'_>, dict: Option<&StringDict>| -> Result<String, PcfError> {
                match dict {
                    Some(dict) => dict.get(reader),
                    None => reader.cstr(),
                }
            };

        let element_count = reader.u32()?;
        // Type + name references plus a 16-byte GUID per element header.
        let element_count = reader.check_count(element_count, 18)?;
        let mut elements = Vec::with_capacity(element_count);
        for _ in 0..element_count {
            let type_name = read_dict_string(&mut reader, dict.as_ref())?;
            let name = if encoding_version >= 4 {
                read_dict_string(&mut reader, dict.as_ref())?
            } else {
                reader.cstr()?
            };
            reader.take(16)?; // GUID, unused
            elements.push(DmxElement {
                type_name,
                name,
                attributes: Vec::new(),
            });
        }

        for element in &mut elements {
            let attribute_count = reader.u32()?;
            let attribute_count = reader.check_count(attribute_count, 3)?;
            let mut attributes = Vec::with_capacity(attribute_count);
            for _ in 0..attribute_count {
                let name = read_dict_string(&mut reader, dict.as_ref())?;
                let type_id = reader.u8()?;
                let value = read_value(&mut reader, dict.as_ref(), encoding_version, type_id)?;
                attributes.push((name, value));
            }
            element.attributes = attributes;
        }

        Ok(Self {
            encoding_version,
            format_version,
            elements,
        })
    }
}

fn parse_header(header: &str) -> Result<(String, u32, u32), PcfError> {
    // `<!-- dmx encoding binary 2 format pcf 1 -->`
    let tokens: Vec<&str> = header.split_whitespace().collect();
    if tokens.first() != Some(&"<!--") || tokens.get(1) != Some(&"dmx") {
        return Err(PcfError::NotDmx);
    }
    let position = |keyword: &str| tokens.iter().position(|token| *token == keyword);
    let encoding_at = position("encoding").ok_or(PcfError::NotDmx)?;
    let format_at = position("format").ok_or(PcfError::NotDmx)?;
    let encoding = tokens
        .get(encoding_at + 1)
        .ok_or(PcfError::NotDmx)?
        .to_string();
    let encoding_version = tokens
        .get(encoding_at + 2)
        .and_then(|token| token.parse().ok())
        .ok_or(PcfError::NotDmx)?;
    let format_version = tokens
        .get(format_at + 2)
        .and_then(|token| token.parse().ok())
        .unwrap_or(0);
    Ok((encoding, encoding_version, format_version))
}

fn element_ref(reader: &mut Reader<'_>) -> Result<Option<u32>, PcfError> {
    let index = reader.i32()?;
    match index {
        -1 => Ok(None),
        // -2 is an external stub referenced by a GUID string; unused by PCF.
        -2 => {
            reader.cstr()?;
            Ok(None)
        }
        index if index >= 0 => Ok(Some(index as u32)),
        _ => Err(PcfError::Malformed("negative element reference")),
    }
}

fn read_value(
    reader: &mut Reader<'_>,
    dict: Option<&StringDict>,
    encoding_version: u32,
    type_id: u8,
) -> Result<DmxValue, PcfError> {
    const ARRAY_BASE: u8 = 14;
    if type_id == 0 || type_id > ARRAY_BASE * 2 {
        return Err(PcfError::Malformed("unknown attribute type"));
    }
    if type_id > ARRAY_BASE {
        let scalar_id = type_id - ARRAY_BASE;
        let count = reader.u32()?;
        let count = reader.check_count(count, 1)?;
        if scalar_id == 1 {
            let mut refs = Vec::with_capacity(count);
            for _ in 0..count {
                refs.push(element_ref(reader)?);
            }
            return Ok(DmxValue::ElementArray(refs));
        }
        let mut values = Vec::with_capacity(count);
        for _ in 0..count {
            // Array string entries are always inline, independent of the
            // dictionary rules for scalar strings.
            values.push(read_scalar(
                reader,
                dict,
                encoding_version,
                scalar_id,
                true,
            )?);
        }
        return Ok(DmxValue::Value(PcfValue::Array(values)));
    }
    if type_id == 1 {
        return Ok(DmxValue::Element(element_ref(reader)?));
    }
    Ok(DmxValue::Value(read_scalar(
        reader,
        dict,
        encoding_version,
        type_id,
        false,
    )?))
}

fn read_scalar(
    reader: &mut Reader<'_>,
    dict: Option<&StringDict>,
    encoding_version: u32,
    type_id: u8,
    in_array: bool,
) -> Result<PcfValue, PcfError> {
    Ok(match type_id {
        2 => PcfValue::Int(reader.i32()?),
        3 => PcfValue::Float(reader.f32()?),
        4 => PcfValue::Bool(reader.u8()? != 0),
        5 => {
            let use_dict = encoding_version >= 4 && !in_array;
            match (use_dict, dict) {
                (true, Some(dict)) => PcfValue::String(dict.get(reader)?),
                _ => PcfValue::String(reader.cstr()?),
            }
        }
        6 => {
            let len = reader.u32()?;
            let len = reader.check_count(len, 1)?;
            PcfValue::Binary(reader.take(len)?.to_vec())
        }
        // Type 7 changed meaning across encoding versions: a 16-byte object
        // id in v1/v2 files, a fixed-point time in v3+.
        7 if encoding_version < 3 => {
            let guid = reader.take(16)?;
            PcfValue::Binary(guid.to_vec())
        }
        7 => PcfValue::Time(reader.i32()? as f32 / 10_000.0),
        8 => {
            let rgba = reader.take(4)?;
            PcfValue::Color([rgba[0], rgba[1], rgba[2], rgba[3]])
        }
        9 => PcfValue::Vector2(reader.f32s()?),
        10 => PcfValue::Vector3(reader.f32s()?),
        11 => PcfValue::Vector4(reader.f32s()?),
        12 => PcfValue::Angle(reader.f32s()?),
        13 => PcfValue::Quaternion(reader.f32s()?),
        14 => PcfValue::Matrix(Box::new(reader.f32s()?)),
        _ => return Err(PcfError::Malformed("unknown attribute type")),
    })
}

// --- PCF layer -----------------------------------------------------------

const OPERATOR_LISTS: [&str; 6] = [
    "emitters",
    "initializers",
    "operators",
    "renderers",
    "forces",
    "constraints",
];

fn extract_particle_systems(document: &DmxDocument) -> PcfFile {
    // Prefer the root's authoritative definition list; fall back to a type
    // scan for files whose root element is missing or oddly named.
    let definition_indices: Vec<u32> = document
        .elements
        .first()
        .and_then(|root| {
            root.attributes.iter().find_map(|(name, value)| {
                match (
                    name.eq_ignore_ascii_case("particleSystemDefinitions"),
                    value,
                ) {
                    (true, DmxValue::ElementArray(refs)) => {
                        Some(refs.iter().copied().flatten().collect())
                    }
                    _ => None,
                }
            })
        })
        .unwrap_or_else(|| {
            document
                .elements
                .iter()
                .enumerate()
                .filter(|(_, element)| {
                    element
                        .type_name
                        .eq_ignore_ascii_case("DmeParticleSystemDefinition")
                })
                .map(|(index, _)| index as u32)
                .collect()
        });

    let system_index_by_element: HashMap<u32, usize> = definition_indices
        .iter()
        .enumerate()
        .map(|(system_index, element_index)| (*element_index, system_index))
        .collect();

    let systems = definition_indices
        .iter()
        .filter_map(|&element_index| {
            let element = document.elements.get(element_index as usize)?;
            Some(extract_system(document, element, &system_index_by_element))
        })
        .collect();

    PcfFile {
        encoding_version: document.encoding_version,
        format_version: document.format_version,
        systems,
    }
}

fn extract_system(
    document: &DmxDocument,
    element: &DmxElement,
    system_index_by_element: &HashMap<u32, usize>,
) -> PcfSystem {
    let mut system = PcfSystem {
        name: element.name.clone(),
        ..PcfSystem::default()
    };

    for (name, value) in &element.attributes {
        match value {
            DmxValue::ElementArray(refs) => {
                if name.eq_ignore_ascii_case("children") {
                    system.children = refs
                        .iter()
                        .copied()
                        .flatten()
                        .filter_map(|child_ref| {
                            extract_child(document, child_ref, system_index_by_element)
                        })
                        .collect();
                } else if let Some(list) = OPERATOR_LISTS
                    .iter()
                    .position(|list| name.eq_ignore_ascii_case(list))
                {
                    let functions = refs
                        .iter()
                        .copied()
                        .flatten()
                        .filter_map(|function_ref| {
                            let function = document.elements.get(function_ref as usize)?;
                            Some(PcfFunction {
                                name: function.name.clone(),
                                attributes: plain_attributes(&function.attributes),
                            })
                        })
                        .collect();
                    match list {
                        0 => system.emitters = functions,
                        1 => system.initializers = functions,
                        2 => system.operators = functions,
                        3 => system.renderers = functions,
                        4 => system.forces = functions,
                        _ => system.constraints = functions,
                    }
                }
            }
            DmxValue::Element(_) => {}
            DmxValue::Value(value) => {
                system
                    .attributes
                    .entries
                    .push((name.clone(), value.clone()));
            }
        }
    }

    system
}

fn extract_child(
    document: &DmxDocument,
    child_ref: u32,
    system_index_by_element: &HashMap<u32, usize>,
) -> Option<PcfChild> {
    let element = document.elements.get(child_ref as usize)?;
    // Children are usually wrapped in a DmeParticleChild that points at the
    // definition; some exporters reference the definition directly.
    if system_index_by_element.contains_key(&child_ref) {
        return Some(PcfChild {
            name: element.name.clone(),
            system_index: system_index_by_element.get(&child_ref).copied(),
            delay: 0.0,
        });
    }
    let attributes = plain_attributes(&element.attributes);
    let delay = attributes.get_float("delay").unwrap_or(0.0);
    let child_definition = element.attributes.iter().find_map(|(name, value)| {
        match (name.eq_ignore_ascii_case("child"), value) {
            (true, DmxValue::Element(reference)) => Some(*reference),
            _ => None,
        }
    })??;
    let definition = document.elements.get(child_definition as usize)?;
    Some(PcfChild {
        name: definition.name.clone(),
        system_index: system_index_by_element.get(&child_definition).copied(),
        delay,
    })
}

fn plain_attributes(attributes: &[(String, DmxValue)]) -> PcfAttributes {
    PcfAttributes {
        entries: attributes
            .iter()
            .filter_map(|(name, value)| match value {
                DmxValue::Value(value) => Some((name.clone(), value.clone())),
                _ => None,
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal binary DMX writer covering the layouts the parser reads.
    struct Writer {
        bytes: Vec<u8>,
        encoding_version: u32,
        strings: Vec<String>,
    }

    impl Writer {
        fn new(encoding_version: u32, format_version: u32) -> Self {
            let header = format!(
                "<!-- dmx encoding binary {encoding_version} format pcf {format_version} -->\n"
            );
            let mut bytes = header.into_bytes();
            bytes.push(0);
            Self {
                bytes,
                encoding_version,
                strings: Vec::new(),
            }
        }

        fn intern(&mut self, value: &str) -> u32 {
            if let Some(index) = self.strings.iter().position(|entry| entry == value) {
                return index as u32;
            }
            self.strings.push(value.to_owned());
            (self.strings.len() - 1) as u32
        }

        fn write_dict(&mut self) {
            if self.encoding_version < 2 {
                return;
            }
            if self.encoding_version >= 4 {
                self.bytes
                    .extend_from_slice(&(self.strings.len() as u32).to_le_bytes());
            } else {
                self.bytes
                    .extend_from_slice(&(self.strings.len() as u16).to_le_bytes());
            }
            for entry in &self.strings {
                self.bytes.extend_from_slice(entry.as_bytes());
                self.bytes.push(0);
            }
        }

        fn dict_ref(&mut self, index: u32) {
            if self.encoding_version >= 5 {
                self.bytes.extend_from_slice(&index.to_le_bytes());
            } else {
                self.bytes.extend_from_slice(&(index as u16).to_le_bytes());
            }
        }

        /// Names are dictionary references from v2 up, inline before that.
        fn dict_string(&mut self, value: &str) {
            if self.encoding_version < 2 {
                self.cstr(value);
            } else {
                let index = self.intern(value);
                self.dict_ref(index);
            }
        }

        fn cstr(&mut self, value: &str) {
            self.bytes.extend_from_slice(value.as_bytes());
            self.bytes.push(0);
        }

        fn i32(&mut self, value: i32) {
            self.bytes.extend_from_slice(&value.to_le_bytes());
        }

        fn f32(&mut self, value: f32) {
            self.bytes.extend_from_slice(&value.to_le_bytes());
        }
    }

    struct TestAttr {
        name: &'static str,
        write: fn(&mut Writer),
        type_id: u8,
    }

    struct TestElement {
        type_name: &'static str,
        name: &'static str,
        attrs: Vec<TestAttr>,
    }

    fn build(encoding_version: u32, elements: &[TestElement], value_strings: &[&str]) -> Vec<u8> {
        let mut writer = Writer::new(encoding_version, 1);
        // The dictionary is serialized before the body, so everything the
        // body references — including string values written by closures —
        // must be interned first.
        if encoding_version >= 2 {
            for element in elements {
                writer.intern(element.type_name);
                if encoding_version >= 4 {
                    writer.intern(element.name);
                }
                for attr in &element.attrs {
                    writer.intern(attr.name);
                }
            }
            if encoding_version >= 4 {
                for value in value_strings {
                    writer.intern(value);
                }
            }
        }
        writer.write_dict();

        writer.i32(elements.len() as i32);
        for element in elements {
            writer.dict_string(element.type_name);
            if encoding_version >= 4 {
                writer.dict_string(element.name);
            } else {
                writer.cstr(element.name);
            }
            writer.bytes.extend_from_slice(&[0u8; 16]);
        }
        for element in elements {
            writer.i32(element.attrs.len() as i32);
            for attr in &element.attrs {
                writer.dict_string(attr.name);
                writer.bytes.push(attr.type_id);
                (attr.write)(&mut writer);
            }
        }
        writer.bytes
    }

    fn particle_fixture(encoding_version: u32) -> Vec<u8> {
        build(
            encoding_version,
            &[
                TestElement {
                    type_name: "DmElement",
                    name: "untitled",
                    attrs: vec![TestAttr {
                        name: "particleSystemDefinitions",
                        type_id: 15,
                        write: |writer| {
                            writer.i32(2);
                            writer.i32(1);
                            writer.i32(4);
                        },
                    }],
                },
                TestElement {
                    type_name: "DmeParticleSystemDefinition",
                    name: "explosion_core",
                    attrs: vec![
                        TestAttr {
                            name: "max_particles",
                            type_id: 2,
                            write: |writer| writer.i32(64),
                        },
                        TestAttr {
                            name: "radius",
                            type_id: 3,
                            write: |writer| writer.f32(5.5),
                        },
                        TestAttr {
                            name: "color",
                            type_id: 8,
                            write: |writer| {
                                writer.bytes.extend_from_slice(&[255, 128, 0, 255]);
                            },
                        },
                        TestAttr {
                            name: "emitters",
                            type_id: 15,
                            write: |writer| {
                                writer.i32(1);
                                writer.i32(2);
                            },
                        },
                        TestAttr {
                            name: "children",
                            type_id: 15,
                            write: |writer| {
                                writer.i32(1);
                                writer.i32(3);
                            },
                        },
                    ],
                },
                TestElement {
                    type_name: "DmeParticleOperator",
                    name: "emit_continuously",
                    attrs: vec![TestAttr {
                        name: "emission_rate",
                        type_id: 3,
                        write: |writer| writer.f32(120.0),
                    }],
                },
                TestElement {
                    type_name: "DmeParticleChild",
                    name: "child ref",
                    attrs: vec![
                        TestAttr {
                            name: "child",
                            type_id: 1,
                            write: |writer| writer.i32(4),
                        },
                        TestAttr {
                            name: "delay",
                            type_id: 3,
                            write: |writer| writer.f32(0.25),
                        },
                    ],
                },
                TestElement {
                    type_name: "DmeParticleSystemDefinition",
                    name: "explosion_sparks",
                    attrs: vec![],
                },
            ],
            &[],
        )
    }

    fn assert_fixture_parses(encoding_version: u32) {
        let bytes = particle_fixture(encoding_version);
        let file = parse_pcf(&bytes).expect("fixture parses");
        assert_eq!(file.encoding_version, encoding_version);
        assert_eq!(file.systems.len(), 2);

        let core = &file.systems[0];
        assert_eq!(core.name, "explosion_core");
        assert_eq!(core.attributes.get_int("max_particles"), Some(64));
        assert_eq!(core.attributes.get_float("radius"), Some(5.5));
        assert_eq!(core.attributes.get_color("color"), Some([255, 128, 0, 255]));
        assert_eq!(core.emitters.len(), 1);
        assert_eq!(core.emitters[0].name, "emit_continuously");
        assert_eq!(
            core.emitters[0].attributes.get_float("emission_rate"),
            Some(120.0)
        );
        assert_eq!(core.children.len(), 1);
        assert_eq!(core.children[0].name, "explosion_sparks");
        assert_eq!(core.children[0].system_index, Some(1));
        assert_eq!(core.children[0].delay, 0.25);

        assert_eq!(file.systems[1].name, "explosion_sparks");
    }

    #[test]
    fn parses_binary_v1_fixture() {
        assert_fixture_parses(1);
    }

    #[test]
    fn parses_binary_v2_fixture() {
        assert_fixture_parses(2);
    }

    #[test]
    fn parses_binary_v4_fixture() {
        assert_fixture_parses(4);
    }

    #[test]
    fn parses_binary_v5_fixture() {
        assert_fixture_parses(5);
    }

    #[test]
    fn rejects_text_encoding() {
        let mut bytes = b"<!-- dmx encoding keyvalues2 1 format pcf 1 -->\n".to_vec();
        bytes.push(0);
        assert!(matches!(
            parse_pcf(&bytes),
            Err(PcfError::TextEncodingUnsupported)
        ));
    }

    #[test]
    fn rejects_non_dmx() {
        assert!(matches!(parse_pcf(b"GMAD\x00"), Err(PcfError::NotDmx)));
        // No terminator at all reads as a truncated header.
        assert!(matches!(parse_pcf(b"GMAD\x03"), Err(PcfError::Truncated)));
        assert!(matches!(parse_pcf(b""), Err(PcfError::Truncated)));
    }

    #[test]
    fn survives_truncation_everywhere() {
        let bytes = particle_fixture(2);
        for len in 0..bytes.len() {
            // Any prefix must fail cleanly, never panic.
            let _ = parse_pcf(&bytes[..len]);
        }
    }

    #[test]
    fn rejects_absurd_counts() {
        let mut writer = Writer::new(1, 1);
        writer.i32(i32::MAX);
        assert!(parse_pcf(&writer.bytes).is_err());
    }

    #[test]
    fn string_scalar_uses_dict_only_in_v4_plus() {
        for encoding_version in [2, 5] {
            let bytes = build(
                encoding_version,
                &[
                    TestElement {
                        type_name: "DmElement",
                        name: "root",
                        attrs: vec![TestAttr {
                            name: "particleSystemDefinitions",
                            type_id: 15,
                            write: |writer| {
                                writer.i32(1);
                                writer.i32(1);
                            },
                        }],
                    },
                    TestElement {
                        type_name: "DmeParticleSystemDefinition",
                        name: "system",
                        attrs: vec![TestAttr {
                            name: "material",
                            type_id: 5,
                            write: |writer| {
                                if writer.encoding_version >= 4 {
                                    let index = writer.intern("particle/wisp01.vmt");
                                    writer.dict_ref(index);
                                } else {
                                    writer.cstr("particle/wisp01.vmt");
                                }
                            },
                        }],
                    },
                ],
                &["particle/wisp01.vmt"],
            );
            let file = parse_pcf(&bytes).expect("fixture parses");
            assert_eq!(
                file.systems[0].material(),
                Some("particle/wisp01.vmt"),
                "encoding v{encoding_version}"
            );
        }
    }
}
