use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{Cursor, Read},
    path::{Component, Path, PathBuf},
    sync::atomic::AtomicBool,
};

use image::{ImageReader, RgbaImage};
use serde_json::{Value, json};
use vformats::{
    Limits,
    keyvalues::{KvDocument, KvValue},
    mdl::Mdl,
};

use super::super::{DecompileError, check_cancelled, material_source_root};

const MAX_SOURCE_BYTES: u64 = 128 * 1024 * 1024;
const MAX_OUTPUT_BYTES: usize = 512 * 1024 * 1024;
const MAX_MATERIAL_BYTES: usize = 32 * 1024 * 1024;
const MAX_DIMENSION: u32 = 8192;
const MAX_MATERIALS: usize = 4096;
const IMAGE_EXTENSIONS: &[&str] = &["vtf", "png", "tga", "jpg", "jpeg", "bmp", "gif"];

pub(super) struct MaterialSet {
    pub(super) materials: Vec<Value>,
    pub(super) images: Vec<(String, Vec<u8>)>,
    pub(super) warnings: Vec<String>,
}

pub(super) fn prepare(
    model: &Mdl,
    path: &Path,
    read: &mut dyn FnMut(&str) -> Option<Vec<u8>>,
    cancelled: &AtomicBool,
) -> Result<MaterialSet, DecompileError> {
    check_cancelled(cancelled)?;
    if model.textures.len() > MAX_MATERIALS || model.texture_paths.len() > MAX_MATERIALS {
        return Err(DecompileError::SizeLimit);
    }
    let root = material_source_root(path);
    let mut resolver = Resolver {
        root: fs::canonicalize(&root).unwrap_or(root),
        read,
        cancelled,
        images: Vec::new(),
        image_indices: BTreeMap::new(),
        warnings: BTreeSet::new(),
        image_bytes: 0,
        material_bytes: 0,
        material_reads: 0,
        dependency_lookups: 0,
        size_exceeded: false,
    };
    let mut materials = Vec::with_capacity(model.textures.len());
    for name in &model.textures {
        check_cancelled(cancelled)?;
        materials.push(resolver.material(name, &model.texture_paths));
        check_cancelled(cancelled)?;
        if resolver.size_exceeded {
            return Err(DecompileError::SizeLimit);
        }
    }
    check_cancelled(cancelled)?;
    Ok(MaterialSet {
        materials,
        images: resolver.images,
        warnings: resolver.warnings.into_iter().collect(),
    })
}

struct Resolver<'a> {
    root: PathBuf,
    read: &'a mut dyn FnMut(&str) -> Option<Vec<u8>>,
    cancelled: &'a AtomicBool,
    images: Vec<(String, Vec<u8>)>,
    image_indices: BTreeMap<String, Option<usize>>,
    warnings: BTreeSet<String>,
    image_bytes: usize,
    material_bytes: usize,
    material_reads: usize,
    dependency_lookups: usize,
    size_exceeded: bool,
}

#[derive(Default)]
struct EffectiveMaterial {
    shader: String,
    parameters: BTreeMap<String, String>,
    blocks: BTreeMap<String, Value>,
}

impl Resolver<'_> {
    fn check_cancelled(&self) -> Result<(), String> {
        check_cancelled(self.cancelled).map_err(|error| error.to_string())
    }

    fn material(&mut self, name: &str, directories: &[String]) -> Value {
        let mut material = json!({
            "name": name,
            "pbrMetallicRoughness": {"metallicFactor": 0.0, "roughnessFactor": 1.0},
        });
        let Some(name) = material_name(name) else {
            self.warnings.insert(format!(
                "Material path '{name}' is invalid; its texture was omitted."
            ));
            return material;
        };
        let mut candidates = Vec::new();
        for directory in directories {
            let directory = directory.replace('\\', "/");
            let directory = directory.trim_matches('/');
            let candidate = if directory.is_empty() {
                name.clone()
            } else {
                format!("{directory}/{name}")
            };
            if let Some(candidate) = material_name(&candidate)
                && !candidates.contains(&candidate)
            {
                candidates.push(candidate);
            }
        }
        if !candidates.contains(&name) {
            candidates.push(name.clone());
        }
        let mut resolved = None;
        for candidate in &candidates {
            match self.document(candidate, &mut Vec::new()) {
                Ok(Some(document)) => {
                    resolved = Some((candidate.clone(), document));
                    break;
                }
                Ok(None) => {}
                Err(error) => {
                    self.warnings
                        .insert(format!("Material '{candidate}' could not be read: {error}"));
                }
            }
        }
        let Some((source, document)) = resolved else {
            for candidate in &candidates {
                if let Some(index) = self.texture(candidate) {
                    material["pbrMetallicRoughness"]["baseColorTexture"] = json!({"index": index});
                    self.warnings.insert(format!("Material '{name}' has no VMT; its matching texture was exported with default shading."));
                    return material;
                }
            }
            self.warnings.insert(format!(
                "Material '{name}' was not found; its geometry was exported without a texture."
            ));
            return material;
        };
        let parameters = &document.parameters;
        material["extras"] = json!({
            "sourceMaterial": format!("materials/{source}.vmt"),
            "sourceShader": document.shader,
            "sourceParameters": parameters,
            "sourceBlocks": document.blocks,
        });
        if let Some(base) = parameters.get("$basetexture") {
            if let Some(index) = self.texture(base) {
                material["pbrMetallicRoughness"]["baseColorTexture"] = json!({"index": index});
            } else {
                self.warnings.insert(format!(
                    "Material '{name}': base texture '{base}' was omitted."
                ));
            }
        }
        if let Some(normal) = parameters
            .get("$bumpmap")
            .or_else(|| parameters.get("$normalmap"))
            && !flag(parameters, "$ssbump")
        {
            if let Some(index) = self.texture(normal) {
                // Source and glTF both use cross(normal, tangent) * handedness; preserved VVD UVs need no green-channel inversion.
                material["normalTexture"] = json!({"index": index});
            } else {
                self.warnings.insert(format!(
                    "Material '{name}': normal texture '{normal}' was omitted."
                ));
            }
        }
        let base_color = parameters
            .get("$color")
            .and_then(|value| color(value))
            .unwrap_or([1.0; 3]);
        let color2 = parameters
            .get("$color2")
            .and_then(|value| color(value))
            .unwrap_or([1.0; 3]);
        let alpha = number(parameters, "$alpha").unwrap_or(1.0).clamp(0.0, 1.0);
        material["pbrMetallicRoughness"]["baseColorFactor"] = json!([
            (base_color[0] * color2[0]).clamp(0.0, 1.0),
            (base_color[1] * color2[1]).clamp(0.0, 1.0),
            (base_color[2] * color2[2]).clamp(0.0, 1.0),
            alpha,
        ]);
        if flag(parameters, "$alphatest") {
            material["alphaMode"] = json!("MASK");
            material["alphaCutoff"] = json!(
                number(parameters, "$alphatestreference")
                    .unwrap_or(0.5)
                    .clamp(0.0, 1.0)
            );
        } else if flag(parameters, "$translucent") || alpha < 1.0 {
            material["alphaMode"] = json!("BLEND");
        }
        if flag(parameters, "$nocull") {
            material["doubleSided"] = json!(true);
        }
        if flag(parameters, "$phong") {
            let exponent = number(parameters, "$phongexponent").unwrap_or(5.0).max(0.0);
            material["pbrMetallicRoughness"]["roughnessFactor"] =
                json!((2.0 / (exponent + 2.0)).sqrt().clamp(0.0, 1.0));
        }
        if document.shader.eq_ignore_ascii_case("UnlitGeneric") {
            material["extensions"] = json!({"KHR_materials_unlit": {}});
        } else if !document.shader.eq_ignore_ascii_case("VertexLitGeneric") {
            self.warnings.insert(format!("Material '{name}' uses shader '{}'; Blender uses an approximate surface shader. The original parameters are retained in the material metadata.", document.shader));
        }
        let unsupported: Vec<_> = parameters
            .keys()
            .filter(|key| {
                matches!(
                    key.as_str(),
                    "$additive"
                        | "$detail"
                        | "$basetexture2"
                        | "$bumpmap2"
                        | "$envmap"
                        | "$envmapmask"
                        | "$selfillum"
                        | "$selfillummask"
                        | "$phongexponenttexture"
                        | "$lightwarptexture"
                        | "$basetexturetransform"
                        | "$bumptransform"
                        | "$normalmapalphaenvmapmask"
                        | "$basealphaenvmapmask"
                        | "$blendmodulatetexture"
                        | "$ssbump"
                        | "$rimlight"
                ) && (!matches!(
                    key.as_str(),
                    "$additive"
                        | "$selfillum"
                        | "$normalmapalphaenvmapmask"
                        | "$basealphaenvmapmask"
                        | "$ssbump"
                        | "$rimlight"
                ) || flag(parameters, key))
            })
            .cloned()
            .chain(document.blocks.keys().cloned())
            .collect();
        if !unsupported.is_empty() {
            self.warnings.insert(format!("Material '{name}': these Source effects are not reproduced in Blender: {}. Their original parameters are retained in the material metadata.", unsupported.join(", ")));
        }
        material
    }

    fn document(
        &mut self,
        name: &str,
        chain: &mut Vec<String>,
    ) -> Result<Option<EffectiveMaterial>, String> {
        if chain.len() >= 32 || chain.iter().any(|value| value == name) {
            return Err("material include cycle or depth limit exceeded".into());
        }
        let Some((_, bytes)) = self.find(name, &["vmt"])? else {
            return Ok(None);
        };
        self.material_reads += 1;
        self.material_bytes = self.material_bytes.saturating_add(bytes.len());
        if self.material_reads > MAX_MATERIALS || self.material_bytes > MAX_MATERIAL_BYTES {
            self.size_exceeded = true;
            return Err("material size limit exceeded".into());
        }
        let text = std::str::from_utf8(&bytes).map_err(|error| error.to_string())?;
        let limits = Limits {
            max_input_bytes: MAX_MATERIAL_BYTES as u64,
            max_entries: 65_536,
            max_kv_depth: 32,
            ..Limits::default()
        };
        let document = vformats::vmt::parse(text, &limits).map_err(|error| error.to_string())?;
        let mut material = if document.is_patch() {
            let include = document
                .value("include")
                .and_then(material_name)
                .ok_or("material patch has no valid include")?;
            chain.push(name.to_owned());
            let inherited = self.document(&include, chain);
            chain.pop();
            inherited?.ok_or_else(|| format!("included material '{include}' was not found"))?
        } else {
            EffectiveMaterial {
                shader: document.shader.to_string(),
                ..EffectiveMaterial::default()
            }
        };
        for pair in &document.kv.pairs {
            match &pair.value {
                KvValue::String(value) if pair.key.starts_with('$') => {
                    material
                        .parameters
                        .insert(pair.key.to_ascii_lowercase(), value.to_string());
                }
                KvValue::Block(block)
                    if !matches!(pair.key.to_ascii_lowercase().as_str(), "insert" | "replace") =>
                {
                    material
                        .blocks
                        .insert(pair.key.to_string(), block_metadata(block));
                }
                _ => {}
            }
        }
        if let Some(patch) = document.patch() {
            for value in patch.overrides {
                material
                    .parameters
                    .insert(value.key.to_ascii_lowercase(), value.value.to_string());
            }
            for group in ["insert", "replace"] {
                for block in document.kv.blocks(group) {
                    for pair in &block.pairs {
                        if let KvValue::Block(block) = &pair.value {
                            material
                                .blocks
                                .insert(pair.key.to_string(), block_metadata(block));
                        }
                    }
                }
            }
        }
        Ok(Some(material))
    }

    fn texture(&mut self, name: &str) -> Option<usize> {
        let Some(name) = material_name(name) else {
            self.warnings.insert(format!(
                "Texture path '{name}' is invalid; the texture was omitted."
            ));
            return None;
        };
        if let Some(index) = self.image_indices.get(&name) {
            return *index;
        }
        let index = match self.load_image(&name) {
            Ok(index) => index,
            Err(error) => {
                self.warnings
                    .insert(format!("Texture '{name}' could not be exported: {error}"));
                None
            }
        };
        self.image_indices.insert(name, index);
        index
    }

    fn load_image(&mut self, name: &str) -> Result<Option<usize>, String> {
        let Some((filename, bytes)) = self.find(name, IMAGE_EXTENSIONS)? else {
            return Ok(None);
        };
        let limits = Limits {
            max_input_bytes: MAX_SOURCE_BYTES,
            max_entry_bytes: u64::from(MAX_DIMENSION).pow(2) * 4,
            ..Limits::default()
        };
        let image = if bytes.starts_with(b"VTF\0") {
            let texture =
                vformats::vtf::parse(&bytes, &limits).map_err(|error| error.to_string())?;
            check_dimensions(texture.width(), texture.height())?;
            if texture.frame_count() > 1 || texture.face_count() > 1 {
                self.warnings.insert(format!("Texture '{name}' contains multiple frames or cubemap faces; only its first frame and face were exported."));
            }
            self.check_cancelled()?;
            let image = texture
                .decode_rgba(0, 0, 0, &limits)
                .map_err(|error| error.to_string())?;
            RgbaImage::from_raw(image.width, image.height, image.rgba)
                .ok_or("invalid RGBA image")?
        } else {
            let mut reader = ImageReader::new(Cursor::new(bytes))
                .with_guessed_format()
                .map_err(|error| error.to_string())?;
            if reader.format().is_none()
                && let Some(format) = Path::new(&filename)
                    .extension()
                    .and_then(image::ImageFormat::from_extension)
            {
                reader.set_format(format);
            }
            let mut limits = image::Limits::default();
            limits.max_image_width = Some(MAX_DIMENSION);
            limits.max_image_height = Some(MAX_DIMENSION);
            limits.max_alloc = Some(u64::from(MAX_DIMENSION).pow(2) * 4);
            reader.limits(limits);
            self.check_cancelled()?;
            let image = reader
                .decode()
                .map_err(|error| error.to_string())?
                .to_rgba8();
            check_dimensions(image.width(), image.height())?;
            image
        };
        self.check_cancelled()?;
        let mut output = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut output, image::ImageFormat::Png)
            .map_err(|error| error.to_string())?;
        self.check_cancelled()?;
        let bytes = output.into_inner();
        self.image_bytes = self.image_bytes.saturating_add(bytes.len());
        if self.image_bytes > MAX_OUTPUT_BYTES {
            self.size_exceeded = true;
            return Err("texture output size limit exceeded".into());
        }
        let index = self.images.len();
        self.images.push((name.to_owned(), bytes));
        Ok(Some(index))
    }

    fn find(
        &mut self,
        name: &str,
        extensions: &[&str],
    ) -> Result<Option<(String, Vec<u8>)>, String> {
        self.check_cancelled()?;
        self.dependency_lookups += 1;
        if self.dependency_lookups > 16_384 {
            self.size_exceeded = true;
            return Err("material dependency lookup limit exceeded".into());
        }
        let stem = Path::new(name);
        let known_extension = stem.extension().is_some_and(|extension| {
            extensions
                .iter()
                .any(|candidate| extension.eq_ignore_ascii_case(candidate))
        });
        let mut filenames = Vec::new();
        for stem in [name, name.rsplit('/').next().unwrap_or(name)] {
            if known_extension {
                filenames.push(stem.to_owned());
            } else {
                filenames.extend(
                    extensions
                        .iter()
                        .map(|extension| format!("{stem}.{extension}")),
                );
            }
        }
        filenames.dedup();
        for filename in &filenames {
            for directory in ["materials", "textures", ""] {
                self.check_cancelled()?;
                let relative = Path::new(directory).join(filename);
                if let Some(path) = local_file(&self.root, &relative)? {
                    self.check_cancelled()?;
                    let file = fs::File::open(path).map_err(|error| error.to_string())?;
                    let mut bytes = Vec::new();
                    file.take(MAX_SOURCE_BYTES + 1)
                        .read_to_end(&mut bytes)
                        .map_err(|error| error.to_string())?;
                    self.check_cancelled()?;
                    if bytes.len() as u64 > MAX_SOURCE_BYTES {
                        return Err("file exceeds the 128 MiB input limit".into());
                    }
                    return Ok(Some((filename.clone(), bytes)));
                }
            }
        }
        for filename in filenames {
            self.check_cancelled()?;
            let bytes = (self.read)(&format!("materials/{filename}"));
            self.check_cancelled()?;
            if let Some(bytes) = bytes {
                if bytes.len() as u64 > MAX_SOURCE_BYTES {
                    return Err("file exceeds the 128 MiB input limit".into());
                }
                return Ok(Some((filename, bytes)));
            }
        }
        Ok(None)
    }
}

fn material_name(name: &str) -> Option<String> {
    let mut name = name.trim().replace('\\', "/").to_ascii_lowercase();
    if name.starts_with("materials/") {
        name.replace_range(.."materials/".len(), "");
    }
    if [".vmt", ".vtf"]
        .iter()
        .any(|extension| name.ends_with(extension))
    {
        name.truncate(name.len() - 4);
    }
    if name.is_empty()
        || name.contains([':', '\0', '\r', '\n'])
        || name
            .split('/')
            .any(|part| part.is_empty() || matches!(part, "." | ".."))
    {
        None
    } else {
        Some(name)
    }
}

fn local_file(root: &Path, relative: &Path) -> Result<Option<PathBuf>, String> {
    let mut current = root.to_owned();
    for component in relative.components() {
        let Component::Normal(name) = component else {
            return Ok(None);
        };
        let exact = current.join(name);
        if exact.exists() {
            current = exact;
        } else {
            let entries = match fs::read_dir(&current) {
                Ok(entries) => entries,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(error.to_string()),
            };
            let mut matched = None;
            for (index, entry) in entries.enumerate() {
                if index >= 65_536 {
                    return Err("material directory entry limit exceeded".into());
                }
                let entry = entry.map_err(|error| error.to_string())?;
                if entry
                    .file_name()
                    .to_string_lossy()
                    .eq_ignore_ascii_case(&name.to_string_lossy())
                {
                    if matched.is_some() {
                        return Err("material path is ambiguous ignoring case".into());
                    }
                    matched = Some(entry.path());
                }
            }
            let Some(path) = matched else { return Ok(None) };
            current = path;
        }
        let real = fs::canonicalize(&current).map_err(|error| error.to_string())?;
        if !real.starts_with(root) {
            return Err("material symlink points outside the model directory".into());
        }
    }
    Ok(current.is_file().then_some(current))
}

fn number(parameters: &BTreeMap<String, String>, key: &str) -> Option<f64> {
    parameters
        .get(key)?
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite())
}

fn block_metadata(block: &KvDocument<'_>) -> Value {
    let mut parameters = serde_json::Map::new();
    for pair in &block.pairs {
        let value = match &pair.value {
            KvValue::String(value) => json!(value),
            KvValue::Block(block) => block_metadata(block),
        };
        parameters.insert(pair.key.to_string(), value);
    }
    Value::Object(parameters)
}

fn flag(parameters: &BTreeMap<String, String>, key: &str) -> bool {
    parameters
        .get(key)
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("true"))
        || number(parameters, key).is_some_and(|value| value != 0.0)
}

fn color(value: &str) -> Option<[f64; 3]> {
    let divisor = if value.trim_start().starts_with('{') {
        255.0
    } else {
        1.0
    };
    let values = value
        .trim()
        .trim_matches(['[', ']', '{', '}'])
        .split(|character: char| character.is_ascii_whitespace() || character == ',')
        .filter(|value| !value.is_empty())
        .map(str::parse::<f64>)
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    let values: [f64; 3] = values.try_into().ok()?;
    values
        .iter()
        .all(|value| value.is_finite())
        .then(|| values.map(|value| (value / divisor).clamp(0.0, 1.0)))
}

fn check_dimensions(width: u32, height: u32) -> Result<(), String> {
    if width == 0 || height == 0 || width > MAX_DIMENSION || height > MAX_DIMENSION {
        Err("texture dimensions must be between 1 and 8192 pixels".into())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(names: &[&str], directories: &[&str]) -> Mdl {
        Mdl {
            name: "models/test.mdl".into(),
            checksum: 0,
            version: 48,
            flags: 0,
            illumination_position: [0.0; 3],
            hull_min: [0.0; 3],
            hull_max: [0.0; 3],
            view_min: [0.0; 3],
            view_max: [0.0; 3],
            attachments: Vec::new(),
            illumination_attachment_index: None,
            bones: Vec::new(),
            linear_bones: None,
            textures: names.iter().map(|value| (*value).to_owned()).collect(),
            texture_paths: directories
                .iter()
                .map(|value| (*value).to_owned())
                .collect(),
            skin_reference_count: 0,
            skin_table: Vec::new(),
            body_parts: Vec::new(),
            local_animations: Vec::new(),
            sequences: Vec::new(),
            included_model_count: 0,
            bone_controller_count: 0,
            flex_descriptor_count: 0,
            sequence_count: 0,
        }
    }

    fn png(pixels: &RgbaImage) -> Vec<u8> {
        let mut output = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(pixels.clone())
            .write_to(&mut output, image::ImageFormat::Png)
            .unwrap();
        output.into_inner()
    }

    fn vtf() -> Vec<u8> {
        let mut bytes = vec![0; 80];
        bytes[..4].copy_from_slice(b"VTF\0");
        bytes[4..8].copy_from_slice(&7_u32.to_le_bytes());
        bytes[8..12].copy_from_slice(&2_u32.to_le_bytes());
        bytes[12..16].copy_from_slice(&80_u32.to_le_bytes());
        bytes[16..18].copy_from_slice(&2_u16.to_le_bytes());
        bytes[18..20].copy_from_slice(&2_u16.to_le_bytes());
        bytes[24..26].copy_from_slice(&2_u16.to_le_bytes());
        bytes[48..52].copy_from_slice(&1_f32.to_le_bytes());
        bytes[56] = 2;
        bytes[57..61].copy_from_slice(&u32::MAX.to_le_bytes());
        bytes[63..65].copy_from_slice(&1_u16.to_le_bytes());
        bytes.extend_from_slice(&[16, 32, 48, 255, 8, 16, 24, 255]);
        bytes.extend_from_slice(&[
            255, 0, 0, 0, 0, 255, 0, 64, 0, 0, 255, 128, 255, 255, 255, 255,
        ]);
        bytes.extend_from_slice(&[0; 16]);
        bytes
    }

    #[test]
    fn patch_inheritance_exports_embedded_color_alpha_and_unchanged_normal_pixels() {
        let normal = RgbaImage::from_pixel(2, 1, image::Rgba([128, 64, 240, 255]));
        let files = BTreeMap::from([
            ("materials/cars/paint.vmt", br#"Patch { include "materials/shared/base.vmt" replace { "$color2" "{128 255 64}" "$alphatestreference" ".3" } }"#.to_vec()),
            ("materials/shared/base.vmt", br#"VertexLitGeneric { "$basetexture" "cars/color" "$bumpmap" "cars/normal" "$alphatest" "true" "$nocull" "1" "$phong" "1" "$phongexponent" "30" }"#.to_vec()),
            ("materials/cars/color.vtf", vtf()),
            ("materials/cars/normal.png", png(&normal)),
        ]);
        let mut read = |path: &str| files.get(path).cloned();
        let result = prepare(
            &model(&["paint", "paint"], &["cars/"]),
            Path::new("/no-such-model/test.mdl"),
            &mut read,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(result.materials.len(), 2);
        assert_eq!(result.images.len(), 2);
        let material = &result.materials[0];
        assert_eq!(material["extras"]["sourceShader"], "VertexLitGeneric");
        assert_eq!(
            material["pbrMetallicRoughness"]["baseColorTexture"]["index"],
            0
        );
        assert_eq!(material["normalTexture"]["index"], 1);
        assert_eq!(material["alphaMode"], "MASK");
        assert_eq!(material["alphaCutoff"], 0.3);
        assert_eq!(material["doubleSided"], true);
        assert_eq!(material["pbrMetallicRoughness"]["roughnessFactor"], 0.25);
        assert_eq!(
            material["pbrMetallicRoughness"]["baseColorFactor"],
            json!([128.0 / 255.0, 1.0, 64.0 / 255.0, 1.0])
        );
        let base = image::load_from_memory(&result.images[0].1)
            .unwrap()
            .to_rgba8();
        assert_eq!(base.dimensions(), (2, 2));
        assert_eq!(base.get_pixel(0, 0).0, [255, 0, 0, 0]);
        assert_eq!(base.get_pixel(1, 0).0, [0, 255, 0, 64]);
        assert_eq!(base.get_pixel(0, 1).0, [0, 0, 255, 128]);
        assert_eq!(
            image::load_from_memory(&result.images[1].1)
                .unwrap()
                .to_rgba8(),
            normal
        );
        assert_eq!(result.warnings.len(), 1);
        assert!(result.warnings[0].contains("first frame and face"));
    }

    #[test]
    fn local_materials_win_over_game_assets_and_match_case_insensitively() {
        let directory = tempfile::tempdir().unwrap();
        let material_directory = directory.path().join("Materials/Cars");
        fs::create_dir_all(&material_directory).unwrap();
        fs::create_dir_all(directory.path().join("models")).unwrap();
        fs::write(
            material_directory.join("Paint.VMT"),
            br#"UnlitGeneric { "$basetexture" "cars/color" "$translucent" "1" "$alpha" ".4" }"#,
        )
        .unwrap();
        fs::write(
            material_directory.join("Color.PNG"),
            png(&RgbaImage::from_pixel(1, 1, image::Rgba([8, 16, 32, 255]))),
        )
        .unwrap();
        let mut read = |_: &str| panic!("local materials must take priority");
        let result = prepare(
            &model(&["paint"], &["cars/"]),
            &directory.path().join("models/test.mdl"),
            &mut read,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(result.materials[0]["alphaMode"], "BLEND");
        assert_eq!(
            result.materials[0]["pbrMetallicRoughness"]["baseColorFactor"][3],
            0.4
        );
        assert!(result.materials[0]["extensions"]["KHR_materials_unlit"].is_object());
        assert!(result.warnings.is_empty());
        assert_eq!(
            image::load_from_memory(&result.images[0].1)
                .unwrap()
                .to_rgba8()
                .get_pixel(0, 0)
                .0,
            [8, 16, 32, 255]
        );
    }

    #[test]
    fn missing_cyclic_and_unsafe_materials_preserve_all_material_slots() {
        let files = BTreeMap::from([
            ("materials/a.vmt", br#"Patch { include "b.vmt" }"#.to_vec()),
            ("materials/b.vmt", br#"Patch { include "a.vmt" }"#.to_vec()),
            (
                "materials/bad_texture.vmt",
                br#"VertexLitGeneric { "$basetexture" "../outside" "$bumpmap" "broken" }"#.to_vec(),
            ),
            ("materials/broken.vtf", b"VTF\0broken".to_vec()),
        ]);
        let mut requests = Vec::new();
        let mut read = |path: &str| {
            requests.push(path.to_owned());
            files.get(path).cloned()
        };
        let result = prepare(
            &model(&["a", "bad_texture", "missing", "../escape"], &[]),
            Path::new("/no-such-model/test.mdl"),
            &mut read,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(
            result
                .materials
                .iter()
                .map(|material| material["name"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["a", "bad_texture", "missing", "../escape"]
        );
        assert!(result.images.is_empty());
        assert!(
            result
                .warnings
                .iter()
                .any(|warning| warning.contains("include cycle"))
        );
        assert!(
            result
                .warnings
                .iter()
                .any(|warning| warning.contains("Texture path '../outside'"))
        );
        assert!(
            result
                .warnings
                .iter()
                .any(|warning| warning.contains("Texture 'broken' could not be exported"))
        );
        assert!(requests.iter().all(|path| !path.contains("..")));
    }

    #[test]
    fn tga_texture_names_supply_format_and_preserve_pixel_orientation() {
        let pixels = RgbaImage::from_fn(2, 2, |x, y| {
            image::Rgba([x as u8 * 128, y as u8 * 128, 240, 255])
        });
        let mut output = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(pixels.clone())
            .write_to(&mut output, image::ImageFormat::Tga)
            .unwrap();
        let tga = output.into_inner();
        let mut read = |path: &str| match path {
            "materials/paint.vmt" => Some(
                br#"VertexLitGeneric { "$basetexture" "color.tga" "$selfillum" "0" }"#.to_vec(),
            ),
            "materials/color.tga" => Some(tga.clone()),
            _ => None,
        };
        let result = prepare(
            &model(&["paint"], &[]),
            Path::new("/no-such-model/test.mdl"),
            &mut read,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(result.warnings.is_empty());
        assert_eq!(result.images.len(), 1);
        assert_eq!(
            image::load_from_memory(&result.images[0].1)
                .unwrap()
                .to_rgba8(),
            pixels
        );
    }

    #[test]
    fn unsupported_self_shadowed_normals_and_oversized_textures_are_omitted() {
        let image = RgbaImage::from_pixel(MAX_DIMENSION + 1, 1, image::Rgba([255; 4]));
        let files = BTreeMap::from([
            ("materials/paint.vmt", br#"VertexLitGeneric { "$basetexture" "too_large" "$bumpmap" "ssbump" "$ssbump" "1" Proxies { AnimatedTexture { animatedTextureVar "$basetexture" animatedTextureFrameNumVar "$frame" animatedTextureFrameRate "4" } } }"#.to_vec()),
            ("materials/too_large.png", png(&image)),
        ]);
        let mut requests = Vec::new();
        let mut read = |path: &str| {
            requests.push(path.to_owned());
            files.get(path).cloned()
        };
        let result = prepare(
            &model(&["paint"], &[]),
            Path::new("/no-such-model/test.mdl"),
            &mut read,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(result.images.is_empty());
        assert!(result.materials[0]["normalTexture"].is_null());
        assert_eq!(
            result.materials[0]["extras"]["sourceBlocks"]["Proxies"]["AnimatedTexture"]["animatedTextureFrameRate"],
            "4"
        );
        assert!(requests.iter().all(|path| !path.contains("ssbump.")));
        assert!(
            result
                .warnings
                .iter()
                .any(|warning| warning.contains("$ssbump, Proxies"))
        );
        assert!(
            result
                .warnings
                .iter()
                .any(|warning| warning.contains("Texture 'too_large' could not be exported"))
        );
    }

    #[test]
    fn cancellation_during_dependency_read_stops_before_texture_decode() {
        let cancelled = AtomicBool::new(false);
        let mut requests = Vec::new();
        let mut read = |path: &str| {
            requests.push(path.to_owned());
            cancelled.store(true, std::sync::atomic::Ordering::Relaxed);
            Some(br#"VertexLitGeneric { "$basetexture" "color" }"#.to_vec())
        };
        assert!(matches!(
            prepare(
                &model(&["paint", "second"], &[]),
                Path::new("/no-such-model/test.mdl"),
                &mut read,
                &cancelled,
            ),
            Err(DecompileError::Cancelled)
        ));
        assert_eq!(requests, ["materials/paint.vmt"]);
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_dependencies_outside_model_root_are_omitted() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(
            outside.path().join("paint.vmt"),
            br#"UnlitGeneric { "$basetexture" "outside" }"#,
        )
        .unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("materials")).unwrap();
        let mut read = |_: &str| None;
        let result = prepare(
            &model(&["paint"], &[]),
            &root.path().join("test.mdl"),
            &mut read,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(result.images.is_empty());
        assert!(
            result
                .warnings
                .iter()
                .any(|warning| warning.contains("outside the model directory"))
        );
    }
}
