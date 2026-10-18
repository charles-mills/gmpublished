use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use serde_json::Value;

use super::{
    ImageReader, MAX_SOURCE_BYTES, MAX_TEXTURE_DIMENSION, MaterialExportError, Path, RgbaImage,
    case_insensitive_file, fs, io, io_error, linear_to_srgb, model_material_name, read_source,
    safe_relative_path, srgb_to_linear,
};

pub(super) struct SourceMaterial {
    pub name: String,
    pub image: RgbaImage,
    pub normal: Option<RgbaImage>,
    pub texture_flags: u32,
    pub normal_flags: u32,
    pub alpha_mask: bool,
    pub alpha_cutoff: f64,
    pub translucent: bool,
    pub double_sided: bool,
    pub warnings: Vec<String>,
    pub source_vmt: Option<SourceVmt>,
}

pub(super) struct SourceVmt {
    pub shader: String,
    pub parameters: BTreeMap<String, String>,
    pub blocks: BTreeMap<String, Value>,
}

pub(super) fn read_materials(
    path: &Path,
    material_names: &[String],
    material_directories: &[String],
    source_paths: &mut BTreeSet<PathBuf>,
) -> Result<Vec<SourceMaterial>, MaterialExportError> {
    let extension = path.extension().unwrap_or_default().to_string_lossy();
    if extension.eq_ignore_ascii_case("gltf") || extension.eq_ignore_ascii_case("glb") {
        read_gltf(path, material_names, material_directories, source_paths)
    } else if extension.eq_ignore_ascii_case("fbx") {
        read_fbx(path, material_names, material_directories, source_paths)
    } else {
        Ok(Vec::new())
    }
}

fn read_gltf(
    path: &Path,
    material_names: &[String],
    material_directories: &[String],
    source_paths: &mut BTreeSet<PathBuf>,
) -> Result<Vec<SourceMaterial>, MaterialExportError> {
    source_paths.insert(path.to_owned());
    let bytes = read_source(path)?;
    let source = crate::util::gltf::Document::parse(&bytes, bytes.starts_with(b"glTF"))
        .map_err(|reason| decode_error(path, &reason))?;
    let document = &source.json;
    let binary = source.binary;
    let mut buffers = Vec::new();
    if let Some(values) = document["buffers"].as_array() {
        for buffer in values {
            buffers.push(if let Some(uri) = buffer["uri"].as_str() {
                read_uri(path, uri, source_paths)?
            } else {
                binary
                    .ok_or_else(|| {
                        decode_error(path, "glTF buffer has neither URI nor GLB binary data")
                    })?
                    .to_vec()
            });
        }
    }
    let Some(materials) = document["materials"].as_array() else {
        return Ok(Vec::new());
    };
    let mut output = Vec::new();
    let stable_names = mdlc::gltf::stable_source_names(
        materials.iter().map(|material| material["name"].as_str()),
        "material",
    );
    for (material, name) in materials.iter().zip(&stable_names) {
        if !material_names.iter().any(|selected| {
            model_material_name(selected, material_directories).is_ok_and(|selected| {
                model_material_name(name, material_directories).is_ok_and(|name| name == selected)
            })
        }) {
            continue;
        }
        let pbr = &material["pbrMetallicRoughness"];
        let mut source_vmt = read_source_vmt(path, &material["extras"])?;
        let mut factor = color_factor(&pbr["baseColorFactor"]);
        if let Some(source) = source_vmt.as_mut() {
            let original = source_color_factor(&source.parameters);
            if factor
                .iter()
                .zip(original)
                .all(|(current, original)| (current - original).abs() < 1e-6)
            {
                factor = [1.0; 4];
            } else {
                for parameter in ["$color", "$color2", "$alpha"] {
                    source.parameters.remove(parameter);
                }
            }
        }
        let mut image = if let Some(index) = pbr["baseColorTexture"]["index"].as_u64() {
            gltf_texture(path, document, &buffers, index as usize, source_paths)?
        } else {
            RgbaImage::from_pixel(1, 1, image::Rgba([255; 4]))
        };
        let opaque = material["alphaMode"].as_str().unwrap_or("OPAQUE") == "OPAQUE";
        for pixel in image.pixels_mut() {
            for channel in 0..3 {
                pixel[channel] = linear_to_srgb(srgb_to_linear(pixel[channel]) * factor[channel]);
            }
            pixel[3] = if opaque && source_vmt.is_none() {
                255
            } else {
                (f32::from(pixel[3]) * factor[3]).round().clamp(0.0, 255.0) as u8
            };
        }
        let mut normal = material["normalTexture"]["index"]
            .as_u64()
            .map(|index| gltf_texture(path, document, &buffers, index as usize, source_paths))
            .transpose()?;
        let normal_scale = material["normalTexture"]["scale"].as_f64().unwrap_or(1.0) as f32;
        if let Some(normal) = normal.as_mut()
            && normal_scale != 1.0
        {
            for pixel in normal.pixels_mut() {
                let vector = [
                    (f32::from(pixel[0]) / 127.5 - 1.0) * normal_scale,
                    (f32::from(pixel[1]) / 127.5 - 1.0) * normal_scale,
                    f32::from(pixel[2]) / 127.5 - 1.0,
                ];
                let length = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
                if length > f32::EPSILON {
                    for channel in 0..3 {
                        pixel[channel] = ((vector[channel] / length + 1.0) * 127.5)
                            .round()
                            .clamp(0.0, 255.0) as u8;
                    }
                }
            }
        }
        let mut warnings = Vec::new();
        let texture_flags = gltf_texture_flags(
            path,
            document,
            &pbr["baseColorTexture"],
            name,
            &mut warnings,
        )?;
        let normal_flags = gltf_texture_flags(
            path,
            document,
            &material["normalTexture"],
            name,
            &mut warnings,
        )?;
        if pbr["metallicRoughnessTexture"].is_object()
            || pbr["metallicFactor"]
                .as_f64()
                .is_some_and(|value| value != 0.0)
        {
            warnings.push(format!("Material '{name}': Source's VertexLitGeneric shader does not reproduce glTF metallic/roughness shading; base color and normal textures were exported."));
        }
        if normal.is_some() && pbr["baseColorTexture"].is_object() {
            let coordinates = |texture| {
                mdlc::gltf::TextureCoordinates::from_json(texture)
                    .map_err(|error| decode_error(path, &error.to_string()))
            };
            if coordinates(&pbr["baseColorTexture"])? != coordinates(&material["normalTexture"])? {
                normal = None;
                warnings.push(format!("Material '{name}': the normal texture uses different UV coordinates from the base color and was omitted; bake both textures to the same UV mapping to export the normal map."));
            }
        }
        output.push(SourceMaterial {
            name: name.into(),
            image,
            normal,
            texture_flags,
            normal_flags,
            alpha_mask: material["alphaMode"].as_str() == Some("MASK"),
            alpha_cutoff: material["alphaCutoff"]
                .as_f64()
                .unwrap_or(0.5)
                .clamp(0.0, 1.0),
            translucent: material["alphaMode"].as_str() == Some("BLEND"),
            double_sided: material["doubleSided"].as_bool().unwrap_or(false),
            warnings,
            source_vmt,
        });
    }
    Ok(output)
}

fn read_source_vmt(path: &Path, extras: &Value) -> Result<Option<SourceVmt>, MaterialExportError> {
    let Some(shader) = extras["sourceShader"].as_str() else {
        return Ok(None);
    };
    if shader.is_empty() || shader.chars().any(char::is_control) {
        return Err(decode_error(path, "Source material shader is invalid"));
    }
    let object = |key: &str| -> Result<serde_json::Map<String, Value>, MaterialExportError> {
        let value = match &extras[key] {
            Value::String(text) => serde_json::from_str(text)
                .map_err(|error| decode_error(path, &format!("Invalid {key}: {error}")))?,
            value => value.clone(),
        };
        match value {
            Value::Null => Ok(serde_json::Map::new()),
            Value::Object(value) => Ok(value),
            _ => Err(decode_error(path, &format!("{key} must be an object"))),
        }
    };
    let mut parameters = BTreeMap::new();
    for (key, value) in object("sourceParameters")? {
        let value = value
            .as_str()
            .ok_or_else(|| decode_error(path, "Source material parameters must be strings"))?;
        if !key.starts_with('$') || key.chars().chain(value.chars()).any(char::is_control) {
            return Err(decode_error(path, "Source material parameter is invalid"));
        }
        parameters.insert(key.to_ascii_lowercase(), value.to_owned());
    }
    let blocks = object("sourceBlocks")?.into_iter().collect();
    Ok(Some(SourceVmt {
        shader: shader.to_owned(),
        parameters,
        blocks,
    }))
}

fn source_color_factor(parameters: &BTreeMap<String, String>) -> [f32; 4] {
    let color = |key| -> [f32; 3] {
        let Some(value) = parameters.get(key) else {
            return [1.0; 3];
        };
        let divisor = if value.trim_start().starts_with('{') {
            255.0
        } else {
            1.0
        };
        let values: Vec<_> = value
            .trim()
            .trim_matches(['[', ']', '{', '}'])
            .split(|character: char| character.is_ascii_whitespace() || character == ',')
            .filter(|value| !value.is_empty())
            .map(str::parse::<f32>)
            .collect();
        match values.as_slice() {
            [Ok(r), Ok(g), Ok(b)] if [r, g, b].iter().all(|value| value.is_finite()) => {
                [r, g, b].map(|value| (value / divisor).clamp(0.0, 1.0))
            }
            _ => [1.0; 3],
        }
    };
    let first = color("$color");
    let second = color("$color2");
    let alpha = parameters
        .get("$alpha")
        .and_then(|value| value.parse::<f32>().ok())
        .filter(|value| value.is_finite())
        .unwrap_or(1.0)
        .clamp(0.0, 1.0);
    [
        first[0] * second[0],
        first[1] * second[1],
        first[2] * second[2],
        alpha,
    ]
}

fn gltf_texture_flags(
    path: &Path,
    document: &Value,
    texture: &Value,
    name: &str,
    warnings: &mut Vec<String>,
) -> Result<u32, MaterialExportError> {
    let Some(index) = texture["index"].as_u64() else {
        return Ok(0);
    };
    let Some(index) = document["textures"][index as usize]["sampler"].as_u64() else {
        return Ok(0);
    };
    let sampler = document["samplers"]
        .as_array()
        .and_then(|samplers| samplers.get(index as usize))
        .ok_or_else(|| decode_error(path, "glTF texture sampler is missing"))?;
    let mut flags = 0;
    for (axis, clamp) in [("wrapS", 4), ("wrapT", 8)] {
        match sampler[axis].as_u64().unwrap_or(10497) {
            33071 => flags |= clamp,
            10497 => {},
            33648 => warnings.push(format!("Material '{name}': mirrored texture wrapping is not supported by Source VTFs; bake the mirrored pattern into the texture and UVs before export.")),
            _ => return Err(decode_error(path, "glTF texture has an invalid wrapping mode")),
        }
    }
    Ok(flags)
}

fn gltf_texture(
    path: &Path,
    document: &Value,
    buffers: &[Vec<u8>],
    index: usize,
    source_paths: &mut BTreeSet<PathBuf>,
) -> Result<RgbaImage, MaterialExportError> {
    let source = document["textures"][index]["source"]
        .as_u64()
        .ok_or_else(|| decode_error(path, "glTF texture source is missing"))?
        as usize;
    let image = &document["images"][source];
    let bytes = if let Some(uri) = image["uri"].as_str() {
        read_uri(path, uri, source_paths)?
    } else {
        let index = image["bufferView"]
            .as_u64()
            .ok_or_else(|| decode_error(path, "glTF image has neither URI nor bufferView"))?
            as usize;
        let view = &document["bufferViews"][index];
        let buffer = view["buffer"]
            .as_u64()
            .and_then(|index| buffers.get(index as usize))
            .ok_or_else(|| decode_error(path, "glTF image buffer is missing"))?;
        let offset = view["byteOffset"].as_u64().unwrap_or(0) as usize;
        let length = view["byteLength"]
            .as_u64()
            .ok_or_else(|| decode_error(path, "glTF image bufferView length is missing"))?
            as usize;
        let end = offset
            .checked_add(length)
            .ok_or_else(|| decode_error(path, "glTF image bufferView overflows"))?;
        buffer
            .get(offset..end)
            .ok_or_else(|| decode_error(path, "glTF image exceeds its buffer"))?
            .to_vec()
    };
    decode_image(path, &bytes)
}

fn read_uri(
    path: &Path,
    uri: &str,
    source_paths: &mut BTreeSet<PathBuf>,
) -> Result<Vec<u8>, MaterialExportError> {
    if let Some(data) = uri.strip_prefix("data:") {
        let (metadata, value) = data
            .split_once(',')
            .ok_or_else(|| decode_error(path, "Invalid glTF data URI"))?;
        if !metadata.ends_with(";base64") {
            return Err(decode_error(
                path,
                "Only base64 glTF data URIs are supported",
            ));
        }
        let bytes =
            base64::decode(value).map_err(|error| decode_error(path, &error.to_string()))?;
        if bytes.len() as u64 > MAX_SOURCE_BYTES {
            return Err(MaterialExportError::SourceTooLarge(path.to_owned()));
        }
        Ok(bytes)
    } else {
        let root = path.parent().expect("model has parent");
        let relative = safe_relative_path(&decode_uri_path(path, uri)?)?;
        let found = case_insensitive_file(root, Path::new(&relative))?.ok_or_else(|| {
            decode_error(
                path,
                &format!("Referenced texture or buffer was not found: {uri}"),
            )
        })?;
        let found = fs::canonicalize(&found).map_err(|source| io_error(&found, source))?;
        if !found.starts_with(root) {
            return Err(MaterialExportError::UnsafePath(uri.into()));
        }
        source_paths.insert(found.clone());
        read_source(&found)
    }
}

fn decode_uri_path(path: &Path, uri: &str) -> Result<String, MaterialExportError> {
    crate::util::gltf::decode_uri_path(uri).map_err(|reason| decode_error(path, reason))
}

fn read_fbx(
    path: &Path,
    material_names: &[String],
    material_directories: &[String],
    source_paths: &mut BTreeSet<PathBuf>,
) -> Result<Vec<SourceMaterial>, MaterialExportError> {
    source_paths.insert(path.to_owned());
    let length = fs::metadata(path)
        .map_err(|error| io_error(path, error))?
        .len();
    if length > MAX_SOURCE_BYTES {
        return Err(MaterialExportError::SourceTooLarge(path.to_path_buf()));
    }
    let scene = mdlc::fbx::load_scene(path, "materials")
        .map_err(|error| decode_error(path, &error.to_string()))?;
    let mut output = Vec::new();
    for material in &scene.materials {
        let name = material.element.name.as_ref().to_owned();
        if !material_names.iter().any(|selected| {
            model_material_name(selected, material_directories).is_ok_and(|selected| {
                model_material_name(&name, material_directories).is_ok_and(|name| name == selected)
            })
        }) {
            continue;
        }
        let color =
            if material.pbr.base_color.has_value || material.pbr.base_color.texture.is_some() {
                &material.pbr.base_color
            } else {
                &material.fbx.diffuse_color
            };
        let mut warnings = Vec::new();
        let mut image = if let Some(texture) = color.texture.as_ref() {
            fbx_texture(path, texture, source_paths)?.unwrap_or_else(|| {
                    warnings.push(format!("Material '{name}': FBX color texture '{}' was not found. The FBX diffuse color was exported instead; add the texture beside the model, in its textures folder, or in its .fbm folder to restore it.", texture.filename.as_ref()));
                    RgbaImage::from_pixel(1, 1, image::Rgba([255; 4]))
            })
        } else {
            RgbaImage::from_pixel(1, 1, image::Rgba([255; 4]))
        };
        let factor = if color.has_value {
            [
                color.value_vec4.x as f32,
                color.value_vec4.y as f32,
                color.value_vec4.z as f32,
            ]
        } else {
            [1.0; 3]
        };
        let opacity = if material.pbr.opacity.has_value {
            material.pbr.opacity.value_vec4.x as f32
        } else if material.fbx.transparency_factor.has_value {
            1.0 - material.fbx.transparency_factor.value_vec4.x as f32
        } else {
            1.0
        };
        for pixel in image.pixels_mut() {
            for channel in 0..3 {
                pixel[channel] = linear_to_srgb(
                    srgb_to_linear(pixel[channel]) * factor[channel].clamp(0.0, 1.0),
                );
            }
            pixel[3] = (f32::from(pixel[3]) * opacity.clamp(0.0, 1.0)).round() as u8;
        }
        let normal = if let Some(texture) = material.pbr.normal_map.texture.as_ref().or(material
            .fbx
            .normal_map
            .texture
            .as_ref())
        {
            let normal = fbx_texture(path, texture, source_paths)?;
            if normal.is_none() {
                warnings.push(format!("Material '{name}': FBX normal texture '{}' was not found. Its normal map was omitted.", texture.filename.as_ref()));
            }
            normal
        } else {
            None
        };
        output.push(SourceMaterial {
            name,
            image,
            normal,
            texture_flags: color
                .texture
                .as_ref()
                .map_or(0, |texture| fbx_texture_flags(texture)),
            normal_flags: material
                .pbr
                .normal_map
                .texture
                .as_ref()
                .or(material.fbx.normal_map.texture.as_ref())
                .map_or(0, |texture| fbx_texture_flags(texture)),
            alpha_mask: false,
            alpha_cutoff: 0.5,
            translucent: true,
            double_sided: material.features.double_sided.enabled,
            warnings,
            source_vmt: None,
        });
    }
    Ok(output)
}

fn fbx_texture_flags(texture: &ufbx::Texture) -> u32 {
    (u32::from(texture.wrap_u == ufbx::WrapMode::Clamp) * 4)
        | (u32::from(texture.wrap_v == ufbx::WrapMode::Clamp) * 8)
}

fn fbx_texture(
    path: &Path,
    texture: &ufbx::Texture,
    source_paths: &mut BTreeSet<PathBuf>,
) -> Result<Option<RgbaImage>, MaterialExportError> {
    if !texture.content.is_empty() {
        return decode_image(Path::new(texture.filename.as_ref()), &texture.content).map(Some);
    }
    if let Some(video) = texture.video.as_ref()
        && !video.content.is_empty()
    {
        return decode_image(Path::new(video.filename.as_ref()), &video.content).map(Some);
    }
    if let Some((_, found)) = super::resolve_fbx_texture_file(path, texture)? {
        source_paths.insert(found.clone());
        return decode_image(&found, &read_source(&found)?).map(Some);
    }
    Ok(None)
}

fn decode_image(path: &Path, bytes: &[u8]) -> Result<RgbaImage, MaterialExportError> {
    let mut reader = ImageReader::new(io::Cursor::new(bytes));
    if let Ok(format) = image::ImageFormat::from_path(path) {
        reader.set_format(format);
    }
    let mut reader = reader
        .with_guessed_format()
        .map_err(|source| io_error(path, source))?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_TEXTURE_DIMENSION);
    limits.max_image_height = Some(MAX_TEXTURE_DIMENSION);
    limits.max_alloc = Some(MAX_SOURCE_BYTES * 4);
    reader.limits(limits);
    reader
        .decode()
        .map(|image| image.to_rgba8())
        .map_err(|error| decode_error(path, &error.to_string()))
}

fn color_factor(value: &Value) -> [f32; 4] {
    std::array::from_fn(|channel| value[channel].as_f64().unwrap_or(1.0).clamp(0.0, 1.0) as f32)
}

fn decode_error(path: &Path, reason: &str) -> MaterialExportError {
    MaterialExportError::Decode {
        path: path.to_owned(),
        reason: reason.into(),
    }
}
