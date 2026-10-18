use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use image::{ImageReader, RgbaImage};
use thiserror::Error;

const MAX_SOURCE_BYTES: u64 = 128 * 1024 * 1024;
const MAX_OUTPUT_BYTES: usize = 512 * 1024 * 1024;
const MAX_TEXTURE_DIMENSION: u32 = 8192;
const IMAGE_EXTENSIONS: &[&str] = &["png", "tga", "jpg", "jpeg", "bmp", "gif"];

#[path = "model_materials/source.rs"]
mod source;

#[derive(Debug, Error)]
pub enum MaterialExportError {
    #[error("Unsafe material path: {0}")]
    UnsafePath(String),
    #[error("Material file exceeds the 128 MiB limit: {}", .0.display())]
    SourceTooLarge(PathBuf),
    #[error("Material export exceeds the 512 MiB limit")]
    SizeLimit,
    #[error("Cannot read or write {}: {source}", path.display())]
    Io { path: PathBuf, source: io::Error },
    #[error("Cannot decode {}: {reason}", path.display())]
    Decode { path: PathBuf, reason: String },
    #[error("Texture dimensions must be between 1 and 8192 pixels: {width} × {height}")]
    Dimensions { width: u32, height: u32 },
    #[error("Material export would overwrite {}", .0.display())]
    AlreadyExists(PathBuf),
    #[error("Conflicting material outputs: {}", .0.display())]
    Conflict(PathBuf),
}

#[derive(Debug, Clone)]
pub struct MaterialAsset {
    pub relative_path: PathBuf,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Default)]
pub struct PreparedMaterials {
    pub assets: Vec<MaterialAsset>,
    pub warnings: Vec<String>,
    /// Canonical input files consumed while resolving and preparing these assets.
    pub(crate) source_paths: BTreeSet<PathBuf>,
}

impl PreparedMaterials {
    pub fn write_to(&self, root: &Path) -> Result<(), MaterialExportError> {
        check_asset_budget(&self.assets)?;
        let canonical = fs::canonicalize(root).map_err(|source| io_error(root, source))?;
        let mut seen = BTreeSet::new();
        for asset in &self.assets {
            safe_relative_path(&asset.relative_path.to_string_lossy().replace('\\', "/"))?;
            if !seen.insert(asset.relative_path.clone()) {
                return Err(MaterialExportError::Conflict(asset.relative_path.clone()));
            }
            let path = root.join(&asset.relative_path);
            if path
                .try_exists()
                .map_err(|source| io_error(&path, source))?
            {
                return Err(MaterialExportError::AlreadyExists(path));
            }
            let mut ancestor = path.parent();
            while let Some(parent) = ancestor {
                if parent.exists() {
                    let real =
                        fs::canonicalize(parent).map_err(|source| io_error(parent, source))?;
                    if !real.starts_with(&canonical) {
                        return Err(MaterialExportError::UnsafePath(path.display().to_string()));
                    }
                    break;
                }
                ancestor = parent.parent();
            }
        }
        for asset in &self.assets {
            let path = root.join(&asset.relative_path);
            let parent = path.parent().expect("material asset has parent");
            fs::create_dir_all(parent).map_err(|source| io_error(parent, source))?;
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .map_err(|source| io_error(&path, source))?;
            file.write_all(&asset.bytes)
                .map_err(|source| io_error(&path, source))?;
        }
        Ok(())
    }

    pub fn write_decompiled_to(&self, root: &Path) -> Result<Vec<String>, MaterialExportError> {
        let mut assets = self.assets.clone();
        let mut warnings = self.warnings.clone();
        for asset in &self.assets {
            if asset
                .relative_path
                .extension()
                .is_none_or(|ext| ext != "vtf")
            {
                continue;
            }
            let decode = vformats::vtf::parse(&asset.bytes, &vformats::Limits::default())
                .and_then(|vtf| {
                    if vtf.frame_count() > 1 || vtf.face_count() > 1 {
                        warnings.push(format!("{} contains multiple frames or cubemap faces; its PNG contains only the first frame and face. The original VTF was preserved.", asset.relative_path.display()));
                    }
                    vtf.decode_rgba(0, 0, 0, &vformats::Limits::default())
                });
            match decode {
                Ok(image) => {
                    let mut bytes = io::Cursor::new(Vec::new());
                    let pixels = RgbaImage::from_raw(image.width, image.height, image.rgba)
                        .expect("VTF decoder provides RGBA pixels");
                    image::DynamicImage::ImageRgba8(pixels)
                        .write_to(&mut bytes, image::ImageFormat::Png)
                        .map_err(|error| MaterialExportError::Decode {
                            path: asset.relative_path.clone(),
                            reason: error.to_string(),
                        })?;
                    assets.push(MaterialAsset {
                        relative_path: asset.relative_path.with_extension("png"),
                        bytes: bytes.into_inner(),
                    });
                }
                Err(error) => warnings.push(format!(
                    "Could not convert {} to PNG: {error}",
                    asset.relative_path.display()
                )),
            }
        }
        Self {
            assets,
            ..Self::default()
        }
        .write_to(root)?;
        Ok(warnings)
    }
}

/// Material files used by geometry, authored skins and independent LOD replacements.
pub(crate) fn source_material_names(
    desc: &mdlc::model::ModelDesc,
    variants: &[SkinVariant],
) -> Vec<String> {
    let mut seen = BTreeSet::new();
    desc.materials
        .textures
        .iter()
        .map(|texture| &texture.name)
        .chain(
            desc.bodyparts
                .iter()
                .flat_map(|body| &body.models)
                .flat_map(|model| &model.lods)
                .flat_map(|lod| &lod.material_replacements)
                .map(|pair| &pair[1]),
        )
        .filter(|name| {
            !variants
                .iter()
                .any(|variant| variant.name.eq_ignore_ascii_case(name))
        })
        .filter(|name| {
            seen.insert(
                mdlc::mdl_writer::normalize_texture_name(name, &desc.materials.search_paths)
                    .replace('\\', "/")
                    .to_ascii_lowercase(),
            )
        })
        .cloned()
        .collect()
}

pub fn prepare_materials(
    material_names: &[String],
    material_directories: &[String],
    source_root: &Path,
) -> Result<PreparedMaterials, MaterialExportError> {
    let source_root =
        fs::canonicalize(source_root).map_err(|source| io_error(source_root, source))?;
    let mut builder = MaterialBuilder {
        source_root,
        files: BTreeMap::new(),
        visited_materials: BTreeSet::new(),
        source_paths: BTreeSet::new(),
        warnings: Vec::new(),
    };
    let mut directories = material_directories
        .iter()
        .map(|directory| normalize_material_directory(directory))
        .collect::<Result<Vec<_>, _>>()?;
    if directories.is_empty() {
        directories.push(String::new());
    }
    for name in material_names {
        let name = model_material_name(name, material_directories)?;
        if name == "debug/debugempty" {
            builder
                .warnings
                .push("The model contains an untextured material slot.".into());
            continue;
        }
        let mut found = false;
        for directory in &directories {
            let path = if directory.is_empty() {
                name.clone()
            } else {
                format!("{directory}/{name}")
            };
            if builder.copy_material(&path, 0)? {
                found = true;
                break;
            }
        }
        if !found {
            builder.warnings.push(format!(
                "No material or texture was found for '{name}'. Put its VMT/VTF or matching image beside the model or in its materials/textures folder."
            ));
        }
    }
    Ok(PreparedMaterials {
        assets: builder
            .files
            .into_iter()
            .map(|(relative_path, bytes)| MaterialAsset {
                relative_path,
                bytes,
            })
            .collect(),
        warnings: builder.warnings,
        source_paths: builder.source_paths,
    })
}

pub fn prepare_materials_for_source(
    material_names: &[String],
    material_directories: &[String],
    source_file: &Path,
) -> Result<PreparedMaterials, MaterialExportError> {
    prepare_materials_for_source_inner(material_names, material_directories, source_file)
        .map(|(prepared, _)| prepared)
}

fn prepare_materials_for_source_inner(
    material_names: &[String],
    material_directories: &[String],
    source_file: &Path,
) -> Result<(PreparedMaterials, BTreeSet<PathBuf>), MaterialExportError> {
    let source_file =
        fs::canonicalize(source_file).map_err(|error| io_error(source_file, error))?;
    let root = source_file.parent().expect("source file has parent");
    let mut prepared = prepare_materials(material_names, material_directories, root)?;
    let imported = source::read_materials(
        &source_file,
        material_names,
        material_directories,
        &mut prepared.source_paths,
    )?;
    let directory =
        normalize_material_directory(material_directories.first().map_or("", String::as_str))?;
    let mut imported_names = BTreeSet::new();
    let mut overrides = BTreeSet::new();
    for material in imported {
        let clean = model_material_name(&material.name, material_directories)?;
        if !material_names.iter().any(|name| {
            model_material_name(name, material_directories).is_ok_and(|name| name == clean)
        }) {
            continue;
        }
        let name = if directory.is_empty() {
            clean.clone()
        } else {
            format!("{directory}/{clean}")
        };
        let path = PathBuf::from("materials").join(format!("{name}.vmt"));
        if !imported_names.insert(name.clone()) {
            return Err(MaterialExportError::Conflict(path));
        }
        if material.source_vmt.is_some() {
            for generated in [
                path.clone(),
                path.with_extension("vtf"),
                PathBuf::from("materials").join(format!("{name}_normal.vtf")),
            ] {
                prepared
                    .assets
                    .retain(|asset| asset.relative_path != generated);
                overrides.insert(generated);
            }
        } else if prepared
            .assets
            .iter()
            .any(|asset| asset.relative_path == path)
        {
            continue;
        }
        let mut image = material.image;
        if !image.width().is_power_of_two() || !image.height().is_power_of_two() {
            let (width, height) = image.dimensions();
            image = image::imageops::resize(
                &image,
                width.next_power_of_two(),
                height.next_power_of_two(),
                image::imageops::FilterType::Lanczos3,
            );
            prepared.warnings.push(format!("Resized texture for '{clean}' from {width}×{height} to {}×{} for Source texture compatibility.", image.width(), image.height()));
        }
        let alpha = image.pixels().any(|pixel| pixel[3] != 255);
        let mut source_vmt = material.source_vmt.unwrap_or_else(|| source::SourceVmt {
            shader: "VertexLitGeneric".into(),
            parameters: BTreeMap::new(),
            blocks: BTreeMap::new(),
        });
        source_vmt
            .parameters
            .insert("$basetexture".into(), name.clone());
        for parameter in [
            "$alphatest",
            "$alphatestreference",
            "$translucent",
            "$nocull",
        ] {
            source_vmt.parameters.remove(parameter);
        }
        if material.alpha_mask {
            source_vmt
                .parameters
                .insert("$alphatest".into(), "1".into());
            source_vmt.parameters.insert(
                "$alphatestreference".into(),
                material.alpha_cutoff.to_string(),
            );
        } else if material.translucent && (alpha || overrides.contains(&path)) {
            source_vmt
                .parameters
                .insert("$translucent".into(), "1".into());
        }
        if material.double_sided {
            source_vmt.parameters.insert("$nocull".into(), "1".into());
        }
        let ssbump = source_vmt.parameters.get("$ssbump").is_some_and(|value| {
            value.trim().eq_ignore_ascii_case("true")
                || value.parse::<f64>().is_ok_and(|value| value != 0.0)
        });
        if !ssbump {
            source_vmt.parameters.remove("$bumpmap");
            source_vmt.parameters.remove("$normalmap");
        }
        prepared.assets.push(MaterialAsset {
            relative_path: path.with_extension("vtf"),
            bytes: encode_vtf_with_flags(&image, false, material.texture_flags)?,
        });
        if let Some(normal) = material.normal {
            let normal_name = format!("{name}_normal");
            let normal_path = PathBuf::from("materials").join(format!("{normal_name}.vtf"));
            if prepared
                .assets
                .iter()
                .any(|asset| asset.relative_path == normal_path)
            {
                return Err(MaterialExportError::Conflict(normal_path));
            }
            let normal = image::imageops::resize(
                &normal,
                normal.width().next_power_of_two(),
                normal.height().next_power_of_two(),
                image::imageops::FilterType::Triangle,
            );
            prepared.assets.push(MaterialAsset {
                relative_path: normal_path,
                bytes: encode_vtf_with_flags(&normal, true, material.normal_flags)?,
            });
            source_vmt.parameters.insert("$bumpmap".into(), normal_name);
            source_vmt.parameters.remove("$ssbump");
        }
        let vmt = render_source_vmt(&source_vmt, &source_file)?;
        let document =
            vformats::vmt::parse(&vmt, &vformats::Limits::default()).map_err(|error| {
                MaterialExportError::Decode {
                    path: source_file.clone(),
                    reason: error.to_string(),
                }
            })?;
        let mut dependencies = Vec::new();
        texture_dependencies(&document.kv, &mut dependencies);
        let mut builder = MaterialBuilder {
            source_root: root.to_owned(),
            files: BTreeMap::new(),
            visited_materials: BTreeSet::new(),
            source_paths: BTreeSet::new(),
            warnings: Vec::new(),
        };
        for (key, texture) in dependencies {
            if texture.eq_ignore_ascii_case("env_cubemap") || texture.starts_with(['_', '!']) {
                continue;
            }
            let texture_name = material_name(&texture)?;
            let texture_path = PathBuf::from("materials").join(format!("{texture_name}.vtf"));
            if prepared
                .assets
                .iter()
                .any(|asset| asset.relative_path == texture_path)
            {
                continue;
            }
            if !builder.copy_texture(
                &texture_name,
                key.contains("bump") || key.contains("normal"),
            )? {
                prepared.warnings.push(format!("Texture '{texture}' referenced by '{name}' was not found. Its Source material parameter was preserved; provide the original texture or mount its game content."));
            }
        }
        for (relative_path, bytes) in builder.files {
            prepared.assets.push(MaterialAsset {
                relative_path,
                bytes,
            });
        }
        prepared.warnings.extend(builder.warnings);
        prepared.source_paths.extend(builder.source_paths);
        prepared.assets.push(MaterialAsset {
            relative_path: path,
            bytes: vmt.into_bytes(),
        });
        prepared.warnings.retain(|warning| {
            !warning.starts_with(&format!("No material or texture was found for '{clean}'."))
        });
        prepared.warnings.extend(material.warnings);
        check_asset_budget(&prepared.assets)?;
    }
    prepared
        .assets
        .sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
    Ok((prepared, overrides))
}

fn render_source_vmt(
    source: &source::SourceVmt,
    path: &Path,
) -> Result<String, MaterialExportError> {
    fn quoted(value: &str, path: &Path) -> Result<String, MaterialExportError> {
        let quote = if !value.contains('"') {
            '"'
        } else if !value.contains('\'') {
            '\''
        } else {
            return Err(MaterialExportError::Decode {
                path: path.to_owned(),
                reason: "Source material values cannot contain both quote delimiters".into(),
            });
        };
        Ok(format!("{quote}{value}{quote}"))
    }
    fn block(
        output: &mut String,
        values: &serde_json::Map<String, serde_json::Value>,
        depth: usize,
        path: &Path,
    ) -> Result<(), MaterialExportError> {
        if depth > 32 {
            return Err(MaterialExportError::Decode {
                path: path.to_owned(),
                reason: "Source material block nesting exceeds 32".into(),
            });
        }
        for (key, value) in values {
            if key.chars().any(char::is_control) {
                return Err(MaterialExportError::Decode {
                    path: path.to_owned(),
                    reason: "Source material block key is invalid".into(),
                });
            }
            output.push_str(&"\t".repeat(depth));
            output.push_str(&quoted(key, path)?);
            match value {
                serde_json::Value::String(value) if !value.chars().any(char::is_control) => {
                    output.push(' ');
                    output.push_str(&quoted(value, path)?);
                    output.push('\n');
                }
                serde_json::Value::Object(values) => {
                    output.push('\n');
                    output.push_str(&"\t".repeat(depth));
                    output.push_str("{\n");
                    block(output, values, depth + 1, path)?;
                    output.push_str(&"\t".repeat(depth));
                    output.push_str("}\n");
                }
                _ => {
                    return Err(MaterialExportError::Decode {
                        path: path.to_owned(),
                        reason: "Source material blocks must contain strings or objects".into(),
                    });
                }
            }
        }
        Ok(())
    }
    let mut output = format!("{}\n{{\n", quoted(&source.shader, path)?);
    for (key, value) in &source.parameters {
        use std::fmt::Write as _;
        writeln!(output, "\t{} {}", quoted(key, path)?, quoted(value, path)?)
            .expect("string write succeeds");
    }
    let blocks = source
        .blocks
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    block(&mut output, &blocks, 1, path)?;
    output.push_str("}\n");
    Ok(output)
}

pub fn prepare_materials_for_sources(
    material_names: &[String],
    material_directories: &[String],
    source_root: &Path,
    source_files: &[PathBuf],
) -> Result<PreparedMaterials, MaterialExportError> {
    let mut prepared = prepare_materials(material_names, material_directories, source_root)?;
    let mut preferred: BTreeSet<_> = prepared
        .assets
        .iter()
        .map(|asset| asset.relative_path.clone())
        .collect();
    let mut assets: BTreeMap<_, _> = prepared
        .assets
        .into_iter()
        .map(|asset| (asset.relative_path, asset.bytes))
        .collect();
    let mut visited = BTreeSet::new();
    for path in source_files {
        let extension = path.extension().unwrap_or_default().to_string_lossy();
        if !["gltf", "glb", "fbx"]
            .iter()
            .any(|value| extension.eq_ignore_ascii_case(value))
        {
            continue;
        }
        let path = fs::canonicalize(path).map_err(|source| io_error(path, source))?;
        if !visited.insert(path.clone()) {
            continue;
        }
        let (imported, overrides) =
            prepare_materials_for_source_inner(material_names, material_directories, &path)?;
        prepared.source_paths.extend(imported.source_paths);
        for asset in imported.assets {
            if overrides.contains(&asset.relative_path) && preferred.remove(&asset.relative_path) {
                assets.insert(asset.relative_path, asset.bytes);
            } else if let Some(existing) = assets.get(&asset.relative_path) {
                if existing != &asset.bytes && !preferred.contains(&asset.relative_path) {
                    return Err(MaterialExportError::Conflict(asset.relative_path));
                }
            } else {
                assets.insert(asset.relative_path, asset.bytes);
            }
        }
        for warning in imported.warnings {
            if !warning.starts_with("No material or texture was found for '")
                && !prepared.warnings.contains(&warning)
            {
                prepared.warnings.push(warning);
            }
        }
    }
    for name in material_names {
        let name = model_material_name(name, material_directories)?;
        let suffix = PathBuf::from(format!("{name}.vmt"));
        if assets.keys().any(|path| path.ends_with(&suffix)) {
            prepared.warnings.retain(|warning| {
                !warning.starts_with(&format!("No material or texture was found for '{name}'."))
            });
        }
    }
    prepared.assets = assets
        .into_iter()
        .map(|(relative_path, bytes)| MaterialAsset {
            relative_path,
            bytes,
        })
        .collect();
    Ok(prepared)
}

struct MaterialBuilder {
    source_root: PathBuf,
    files: BTreeMap<PathBuf, Vec<u8>>,
    visited_materials: BTreeSet<String>,
    source_paths: BTreeSet<PathBuf>,
    warnings: Vec<String>,
}

impl MaterialBuilder {
    fn insert(
        &mut self,
        name: &str,
        extension: &str,
        bytes: Vec<u8>,
    ) -> Result<(), MaterialExportError> {
        let path = PathBuf::from("materials").join(format!("{name}.{extension}"));
        if let Some(existing) = self.files.get(&path) {
            if existing != &bytes {
                return Err(MaterialExportError::Conflict(path));
            }
        } else {
            self.files
                .values()
                .try_fold(bytes.len(), |total, bytes| total.checked_add(bytes.len()))
                .filter(|total| *total <= MAX_OUTPUT_BYTES)
                .ok_or(MaterialExportError::SizeLimit)?;
            self.files.insert(path, bytes);
        }
        Ok(())
    }

    fn copy_material(&mut self, name: &str, depth: usize) -> Result<bool, MaterialExportError> {
        if self.visited_materials.contains(name) {
            return Ok(true);
        }
        if depth >= 32 {
            self.warnings
                .push(format!("Material include depth exceeded for '{name}'."));
            return Ok(false);
        }
        if let Some(path) = self.find_file(name, &["vmt"])? {
            self.source_paths.insert(path.clone());
            self.visited_materials.insert(name.to_owned());
            let bytes = read_source(&path)?;
            let text =
                std::str::from_utf8(&bytes).map_err(|error| MaterialExportError::Decode {
                    path: path.clone(),
                    reason: error.to_string(),
                })?;
            let document =
                vformats::vmt::parse(text, &vformats::Limits::default()).map_err(|error| {
                    MaterialExportError::Decode {
                        path: path.clone(),
                        reason: error.to_string(),
                    }
                })?;
            let mut dependencies = Vec::new();
            texture_dependencies(&document.kv, &mut dependencies);
            let include = document.patch().map(|patch| patch.include);
            for (key, value) in dependencies {
                if value.eq_ignore_ascii_case("env_cubemap")
                    || value.starts_with('_')
                    || value.starts_with('!')
                {
                    continue;
                }
                let texture = material_name(&value)?;
                if !self.copy_texture(&texture, key.contains("bump") || key.contains("normal"))? {
                    self.warnings.push(format!(
                        "Texture '{value}' referenced by '{name}' was not found."
                    ));
                }
            }
            if let Some(include) = include {
                let include = material_name(&include)?;
                if !self.copy_material(&include, depth + 1)? {
                    self.warnings.push(format!(
                        "Included material '{include}' referenced by '{name}' was not found."
                    ));
                }
            }
            self.insert(name, "vmt", bytes)?;
            return Ok(true);
        }
        if self.copy_texture(name, false)? {
            self.visited_materials.insert(name.to_owned());
            let bytes = self
                .files
                .get(&PathBuf::from("materials").join(format!("{name}.vtf")))
                .expect("copy_texture adds VTF");
            let alpha = vformats::vtf::parse(bytes, &vformats::Limits::default())
                .is_ok_and(|vtf| vtf.flags() & 0x3000 != 0);
            let alpha_parameter = if alpha {
                "\n\t\"$translucent\" \"1\""
            } else {
                ""
            };
            self.insert(
                name,
                "vmt",
                format!(
                    "\"VertexLitGeneric\"\n{{\n\t\"$basetexture\" \"{name}\"{alpha_parameter}\n}}\n"
                )
                .into_bytes(),
            )?;
            return Ok(true);
        }
        Ok(false)
    }

    fn copy_texture(&mut self, name: &str, normal: bool) -> Result<bool, MaterialExportError> {
        let destination = PathBuf::from("materials").join(format!("{name}.vtf"));
        if self.files.contains_key(&destination) {
            return Ok(true);
        }
        if let Some(path) = self.find_file(name, &["vtf"])? {
            self.source_paths.insert(path.clone());
            let bytes = read_source(&path)?;
            let texture =
                vformats::vtf::parse(&bytes, &vformats::Limits::default()).map_err(|error| {
                    MaterialExportError::Decode {
                        path: path.clone(),
                        reason: error.to_string(),
                    }
                })?;
            if let Err(error) = texture.decode_rgba(0, 0, 0, &vformats::Limits::default()) {
                if matches!(error, vformats::vtf::VtfError::UnsupportedFormat(_)) {
                    self.warnings.push(format!("{} uses an image format that cannot be previewed or converted to PNG. The original VTF was preserved.", path.display()));
                } else {
                    return Err(MaterialExportError::Decode {
                        path,
                        reason: error.to_string(),
                    });
                }
            }
            self.insert(name, "vtf", bytes)?;
            return Ok(true);
        }
        let Some(path) = self.find_file(name, IMAGE_EXTENSIONS)? else {
            return Ok(false);
        };
        self.source_paths.insert(path.clone());
        if fs::metadata(&path)
            .map_err(|source| io_error(&path, source))?
            .len()
            > MAX_SOURCE_BYTES
        {
            return Err(MaterialExportError::SourceTooLarge(path));
        }
        let reader = ImageReader::open(&path).map_err(|source| io_error(&path, source))?;
        let mut reader = reader
            .with_guessed_format()
            .map_err(|source| io_error(&path, source))?;
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(MAX_TEXTURE_DIMENSION);
        limits.max_image_height = Some(MAX_TEXTURE_DIMENSION);
        limits.max_alloc = Some(MAX_SOURCE_BYTES * 4);
        reader.limits(limits);
        let mut image = reader
            .decode()
            .map_err(|error| MaterialExportError::Decode {
                path: path.clone(),
                reason: error.to_string(),
            })?
            .to_rgba8();
        let (width, height) = image.dimensions();
        if !width.is_power_of_two() || !height.is_power_of_two() {
            image = image::imageops::resize(
                &image,
                width.next_power_of_two(),
                height.next_power_of_two(),
                image::imageops::FilterType::Lanczos3,
            );
            self.warnings.push(format!(
                "Resized '{}' from {width}×{height} to {}×{} for Source texture compatibility.",
                path.display(),
                image.width(),
                image.height()
            ));
        }
        self.insert(name, "vtf", encode_vtf(&image, normal)?)?;
        Ok(true)
    }

    fn find_file(
        &self,
        name: &str,
        extensions: &[&str],
    ) -> Result<Option<PathBuf>, MaterialExportError> {
        let basename = name.rsplit('/').next().unwrap_or(name);
        let mut candidates = Vec::new();
        for stem in [name, basename] {
            for directory in ["materials", "textures", ""] {
                if Path::new(stem).extension().is_some_and(|extension| {
                    extensions
                        .iter()
                        .any(|candidate| extension.eq_ignore_ascii_case(candidate))
                }) {
                    let relative = Path::new(directory).join(stem);
                    if !candidates.contains(&relative) {
                        candidates.push(relative);
                    }
                }
                for extension in extensions {
                    let relative = Path::new(directory).join(format!("{stem}.{extension}"));
                    if !candidates.contains(&relative) {
                        candidates.push(relative);
                    }
                }
            }
        }
        for relative in candidates {
            let Some(path) = case_insensitive_file(&self.source_root, &relative)? else {
                continue;
            };
            let real = fs::canonicalize(&path).map_err(|source| io_error(&path, source))?;
            if !real.starts_with(&self.source_root) {
                return Err(MaterialExportError::UnsafePath(path.display().to_string()));
            }
            if real.is_file() {
                return Ok(Some(real));
            }
        }
        Ok(None)
    }
}

fn texture_dependencies(
    block: &vformats::keyvalues::KvDocument<'_>,
    output: &mut Vec<(String, String)>,
) {
    for pair in &block.pairs {
        match &pair.value {
            vformats::keyvalues::KvValue::Block(block) => texture_dependencies(block, output),
            vformats::keyvalues::KvValue::String(value) => {
                let key = pair.key.to_ascii_lowercase();
                if matches!(
                    key.as_str(),
                    "$basetexture"
                        | "$basetexture2"
                        | "$bumpmap"
                        | "$bumpmap2"
                        | "$normalmap"
                        | "$detail"
                        | "$envmap"
                        | "$envmapmask"
                        | "$selfillummask"
                        | "$phongexponenttexture"
                        | "$lightwarptexture"
                        | "$blendmodulatetexture"
                ) {
                    output.push((key, value.to_string()));
                }
            }
        }
    }
}

fn case_insensitive_file(
    root: &Path,
    relative: &Path,
) -> Result<Option<PathBuf>, MaterialExportError> {
    use crate::util::case_path::LookupError;
    match crate::util::case_path::find_file(root, relative, false, None) {
        Ok(path) => Ok(path),
        Err(LookupError::InvalidPath(_)) => Ok(None),
        Err(LookupError::Io { path, source }) => Err(io_error(&path, source)),
        Err(LookupError::Ambiguous(path)) => Err(MaterialExportError::Conflict(path)),
        Err(LookupError::OutsideRoot(path)) => {
            Err(MaterialExportError::UnsafePath(path.display().to_string()))
        }
        Err(LookupError::EntryLimit) => unreachable!("material lookups have no directory limit"),
    }
}

fn material_name(name: &str) -> Result<String, MaterialExportError> {
    let mut clean = name.replace('\\', "/");
    for extension in ["vmt", "vtf"] {
        if clean
            .to_ascii_lowercase()
            .ends_with(&format!(".{extension}"))
        {
            clean.truncate(clean.len() - extension.len() - 1);
            break;
        }
    }
    safe_relative_path(&clean)
}

fn model_material_name(name: &str, directories: &[String]) -> Result<String, MaterialExportError> {
    let directories = directories
        .iter()
        .map(|directory| normalize_material_directory(directory))
        .collect::<Result<Vec<_>, _>>()?;
    let name = name.replace('\\', "/").to_ascii_lowercase();
    material_name(&mdlc::mdl_writer::normalize_texture_name(
        &name,
        &directories,
    ))
}

fn normalize_material_directory(directory: &str) -> Result<String, MaterialExportError> {
    let clean = directory.replace('\\', "/");
    if clean.trim_matches('/').is_empty() {
        Ok(String::new())
    } else {
        safe_relative_path(clean.trim_end_matches('/'))
    }
}

fn safe_relative_path(name: &str) -> Result<String, MaterialExportError> {
    if name.is_empty()
        || name.starts_with('/')
        || name.contains([':', '"', '\0', '\r', '\n', '{', '}'])
        || name
            .split('/')
            .any(|component| component.is_empty() || component == "." || component == "..")
    {
        return Err(MaterialExportError::UnsafePath(name.to_owned()));
    }
    Ok(name.to_ascii_lowercase())
}

/// Resolve external FBX textures inside the source directory, retaining the selected
/// relative path for portable copies and the canonical file for input protection.
pub(crate) fn resolve_fbx_texture_file(
    source: &Path,
    texture: &ufbx::Texture,
) -> Result<Option<(PathBuf, PathBuf)>, MaterialExportError> {
    if !texture.content.is_empty()
        || texture
            .video
            .as_ref()
            .is_some_and(|video| !video.content.is_empty())
    {
        return Ok(None);
    }
    let parent = source.parent().expect("model has parent");
    let root = fs::canonicalize(parent).map_err(|error| io_error(parent, error))?;
    let embedded = format!(
        "{}.fbm",
        source.file_stem().unwrap_or_default().to_string_lossy()
    );
    let mut escaped_reference = None;
    for name in [
        texture.relative_filename.as_ref(),
        texture.filename.as_ref(),
    ] {
        if name.is_empty() {
            continue;
        }
        let name = name.replace('\\', "/");
        if name.split('/').any(|component| component == "..") {
            escaped_reference.get_or_insert_with(|| name.clone());
        }
        let basename = name.rsplit('/').next().unwrap();
        for candidate in [
            name.clone(),
            basename.into(),
            format!("textures/{basename}"),
            format!("{embedded}/{basename}"),
            format!("{embedded}/{name}"),
        ] {
            // Exporter machine paths may be stale or escape the model folder.
            // Ignore those candidates while retaining safe in-folder fallbacks.
            let Ok(relative) = safe_relative_path(&candidate) else {
                continue;
            };
            let Some(found) = case_insensitive_file(&root, Path::new(&relative))? else {
                continue;
            };
            let canonical = fs::canonicalize(&found).map_err(|error| io_error(&found, error))?;
            if !canonical.starts_with(&root) {
                return Err(MaterialExportError::UnsafePath(candidate));
            }
            if canonical.is_file() {
                return Ok(Some((relative.into(), canonical)));
            }
        }
    }
    escaped_reference.map_or(Ok(None), |reference| {
        Err(MaterialExportError::UnsafePath(reference))
    })
}

fn read_source(path: &Path) -> Result<Vec<u8>, MaterialExportError> {
    if fs::metadata(path)
        .map_err(|source| io_error(path, source))?
        .len()
        > MAX_SOURCE_BYTES
    {
        return Err(MaterialExportError::SourceTooLarge(path.to_owned()));
    }
    fs::read(path).map_err(|source| io_error(path, source))
}

fn io_error(path: &Path, source: io::Error) -> MaterialExportError {
    MaterialExportError::Io {
        path: path.to_owned(),
        source,
    }
}

fn check_asset_budget(assets: &[MaterialAsset]) -> Result<(), MaterialExportError> {
    assets
        .iter()
        .try_fold(0_usize, |total, asset| total.checked_add(asset.bytes.len()))
        .filter(|total| *total <= MAX_OUTPUT_BYTES)
        .ok_or(MaterialExportError::SizeLimit)?;
    Ok(())
}

fn encode_vtf(image: &RgbaImage, normal: bool) -> Result<Vec<u8>, MaterialExportError> {
    encode_vtf_with_flags(image, normal, 0)
}

fn encode_vtf_with_flags(
    image: &RgbaImage,
    normal: bool,
    sampling_flags: u32,
) -> Result<Vec<u8>, MaterialExportError> {
    let (width, height) = image.dimensions();
    if width == 0 || height == 0 || width > MAX_TEXTURE_DIMENSION || height > MAX_TEXTURE_DIMENSION
    {
        return Err(MaterialExportError::Dimensions { width, height });
    }
    let mut levels = Vec::new();
    if width > 1 || height > 1 {
        levels.push(downsample(image, normal));
    }
    while levels
        .last()
        .is_some_and(|level| level.width() > 1 || level.height() > 1)
    {
        let previous = levels.last().expect("base mip exists");
        levels.push(downsample(previous, normal));
    }
    let alpha = image.pixels().any(|pixel| pixel[3] != 255);
    let flags: u32 =
        0x10 | sampling_flags | if normal { 0x80 } else { 0 } | if alpha { 0x2000 } else { 0 };
    let mut bytes = Vec::with_capacity(
        80 + image.as_raw().len()
            + levels
                .iter()
                .map(|level| level.as_raw().len())
                .sum::<usize>(),
    );
    bytes.resize(80, 0_u8);
    bytes[0..4].copy_from_slice(b"VTF\0");
    bytes[4..8].copy_from_slice(&7_u32.to_le_bytes());
    bytes[8..12].copy_from_slice(&2_u32.to_le_bytes());
    bytes[12..16].copy_from_slice(&80_u32.to_le_bytes());
    bytes[16..18].copy_from_slice(&(width as u16).to_le_bytes());
    bytes[18..20].copy_from_slice(&(height as u16).to_le_bytes());
    bytes[20..24].copy_from_slice(&flags.to_le_bytes());
    bytes[24..26].copy_from_slice(&1_u16.to_le_bytes());
    for channel in 0..3 {
        let average = image
            .pixels()
            .map(|pixel| f64::from(srgb_to_linear(pixel[channel])))
            .sum::<f64>()
            / f64::from(width * height);
        let offset = 32 + channel * 4;
        bytes[offset..offset + 4].copy_from_slice(&(average as f32).to_le_bytes());
    }
    bytes[48..52].copy_from_slice(&1_f32.to_le_bytes());
    bytes[52..56].copy_from_slice(&0_u32.to_le_bytes());
    bytes[56] = (levels.len() + 1) as u8;
    bytes[57..61].copy_from_slice(&u32::MAX.to_le_bytes());
    bytes[63..65].copy_from_slice(&1_u16.to_le_bytes());
    for level in levels.iter().rev() {
        bytes.extend_from_slice(level.as_raw());
    }
    bytes.extend_from_slice(image.as_raw());
    Ok(bytes)
}

fn downsample(image: &RgbaImage, normal: bool) -> RgbaImage {
    let (width, height) = image.dimensions();
    RgbaImage::from_fn((width / 2).max(1), (height / 2).max(1), |x, y| {
        let mut rgb = [0_f32; 3];
        let mut alpha = 0_f32;
        let mut count = 0_f32;
        for sy in y * 2..(y * 2 + 2).min(height) {
            for sx in x * 2..(x * 2 + 2).min(width) {
                let pixel = image.get_pixel(sx, sy);
                for channel in 0..3 {
                    rgb[channel] += if normal {
                        f32::from(pixel[channel]) / 127.5 - 1.0
                    } else {
                        srgb_to_linear(pixel[channel])
                    };
                }
                alpha += f32::from(pixel[3]);
                count += 1.0;
            }
        }
        let mut output = [0_u8; 4];
        if normal {
            let length = rgb.iter().map(|v| v * v).sum::<f32>().sqrt();
            for channel in 0..3 {
                output[channel] = if length > f32::EPSILON {
                    ((rgb[channel] / length + 1.0) * 127.5)
                        .round()
                        .clamp(0.0, 255.0) as u8
                } else {
                    [128, 128, 255][channel]
                };
            }
        } else {
            for channel in 0..3 {
                output[channel] = linear_to_srgb(rgb[channel] / count);
            }
        }
        output[3] = (alpha / count).round().clamp(0.0, 255.0) as u8;
        image::Rgba(output)
    })
}

fn srgb_to_linear(value: u8) -> f32 {
    let value = f32::from(value) / 255.0;
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(value: f32) -> u8 {
    let value = if value <= 0.0031308 {
        value * 12.92
    } else {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    };
    (value * 255.0).round().clamp(0.0, 255.0) as u8
}

#[cfg(test)]
#[path = "model_materials/tests.rs"]
mod tests;

pub(crate) struct SkinVariant {
    pub name: String,
    pub base: String,
    pub replacement: crate::model_project::SkinMaterial,
}

pub(crate) fn validate_material_name(name: &str) -> Result<(), MaterialExportError> {
    material_name(name).map(|_| ())
}

pub(crate) fn prepare_skin_materials(
    prepared: &mut PreparedMaterials,
    variants: &[SkinVariant],
    directories: &[String],
    source_root: &Path,
) -> Result<(), MaterialExportError> {
    use crate::model_project::SkinMaterial;
    let directory = normalize_material_directory(directories.first().map_or("", String::as_str))?;
    for variant in variants {
        let clean = model_material_name(&variant.name, directories)?;
        let name = if directory.is_empty() {
            clean.clone()
        } else {
            format!("{directory}/{clean}")
        };
        let vmt_path = PathBuf::from("materials").join(format!("{name}.vmt"));
        if prepared
            .assets
            .iter()
            .any(|asset| asset.relative_path == vmt_path)
        {
            return Err(MaterialExportError::Conflict(vmt_path));
        }
        match &variant.replacement {
            SkinMaterial::Tint { color } => {
                let inherited = prepared_material(prepared, &variant.base, directories)?;
                let color = color.map(srgb_to_linear);
                let value = format!("[{} {} {}]", color[0], color[1], color[2]);
                let vmt = format!(
                    "\"Patch\"\n{{\n\t\"include\" \"materials/{inherited}.vmt\"\n\t\"insert\" {{ \"$color2\" \"{value}\" }}\n\t\"replace\" {{ \"$color2\" \"{value}\" }}\n}}\n"
                );
                prepared.assets.push(MaterialAsset {
                    relative_path: vmt_path,
                    bytes: vmt.into_bytes(),
                });
            }
            SkinMaterial::Texture { path } => {
                let path = if path.is_absolute() {
                    path.clone()
                } else {
                    source_root.join(path)
                };
                let path = fs::canonicalize(&path).map_err(|error| io_error(&path, error))?;
                prepared.source_paths.insert(path.clone());
                let bytes = read_source(&path)?;
                let mut original_vtf = None;
                let mut image = if bytes.starts_with(b"VTF\0") {
                    let limits = vformats::Limits::default();
                    let vtf = vformats::vtf::parse(&bytes, &limits).map_err(|error| {
                        MaterialExportError::Decode {
                            path: path.clone(),
                            reason: error.to_string(),
                        }
                    })?;
                    if vtf.width() > MAX_TEXTURE_DIMENSION || vtf.height() > MAX_TEXTURE_DIMENSION {
                        return Err(MaterialExportError::Dimensions {
                            width: vtf.width(),
                            height: vtf.height(),
                        });
                    }
                    let decoded = vtf.decode_rgba(0, 0, 0, &limits).map_err(|error| {
                        MaterialExportError::Decode {
                            path: path.clone(),
                            reason: error.to_string(),
                        }
                    })?;
                    let image = RgbaImage::from_raw(decoded.width, decoded.height, decoded.rgba)
                        .expect("VTF decoder provides RGBA pixels");
                    original_vtf = Some(bytes);
                    image
                } else {
                    let mut reader = ImageReader::new(io::Cursor::new(bytes))
                        .with_guessed_format()
                        .map_err(|error| io_error(&path, error))?;
                    let mut limits = image::Limits::default();
                    limits.max_image_width = Some(MAX_TEXTURE_DIMENSION);
                    limits.max_image_height = Some(MAX_TEXTURE_DIMENSION);
                    limits.max_alloc = Some(MAX_SOURCE_BYTES);
                    reader.limits(limits);
                    reader
                        .decode()
                        .map_err(|error| MaterialExportError::Decode {
                            path: path.clone(),
                            reason: error.to_string(),
                        })?
                        .into_rgba8()
                };
                let (width, height) = image.dimensions();
                if width == 0
                    || height == 0
                    || width > MAX_TEXTURE_DIMENSION
                    || height > MAX_TEXTURE_DIMENSION
                {
                    return Err(MaterialExportError::Dimensions { width, height });
                }
                if !width.is_power_of_two() || !height.is_power_of_two() {
                    original_vtf = None;
                    image = image::imageops::resize(
                        &image,
                        width.next_power_of_two(),
                        height.next_power_of_two(),
                        image::imageops::FilterType::Lanczos3,
                    );
                    prepared.warnings.push(format!("Resized skin texture '{}' from {width}×{height} to {}×{} for Source texture compatibility.", path.display(), image.width(), image.height()));
                }
                let alpha = if image.pixels().any(|pixel| pixel[3] != 255) {
                    "\t\"$translucent\" \"1\"\n"
                } else {
                    ""
                };
                let vmt = prepared_material(prepared, &variant.base, directories).map_or_else(
                    |_| format!("\"VertexLitGeneric\"\n{{\n\t\"$basetexture\" \"{name}\"\n{alpha}}}\n"),
                    |inherited| format!("\"Patch\"\n{{\n\t\"include\" \"materials/{inherited}.vmt\"\n\t\"insert\" {{ \"$basetexture\" \"{name}\" {alpha} }}\n\t\"replace\" {{ \"$basetexture\" \"{name}\" {alpha} }}\n}}\n"),
                );
                let texture_path = vmt_path.with_extension("vtf");
                if prepared
                    .assets
                    .iter()
                    .any(|asset| asset.relative_path == texture_path)
                {
                    return Err(MaterialExportError::Conflict(texture_path));
                }
                prepared.assets.push(MaterialAsset {
                    relative_path: texture_path,
                    bytes: original_vtf.map_or_else(|| encode_vtf(&image, false), Ok)?,
                });
                prepared.assets.push(MaterialAsset {
                    relative_path: vmt_path,
                    bytes: vmt.into_bytes(),
                });
            }
            SkinMaterial::Material { .. } => {
                unreachable!("existing material references have no generated assets")
            }
        }
        prepared.warnings.retain(|warning| {
            !warning.starts_with(&format!("No material or texture was found for '{clean}'."))
        });
        check_asset_budget(&prepared.assets)?;
    }
    prepared
        .assets
        .sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
    Ok(())
}

pub(crate) fn require_prepared_material(
    prepared: &PreparedMaterials,
    name: &str,
    directories: &[String],
) -> Result<(), MaterialExportError> {
    prepared_material(prepared, name, directories).map(|_| ())
}

fn prepared_material(
    prepared: &PreparedMaterials,
    name: &str,
    directories: &[String],
) -> Result<String, MaterialExportError> {
    let clean = model_material_name(name, directories)?;
    let directories = directories
        .iter()
        .map(|directory| normalize_material_directory(directory))
        .collect::<Result<Vec<_>, _>>()?;
    directories
        .iter()
        .map(String::as_str)
        .chain(std::iter::once(""))
        .find_map(|directory| {
            let directory = directory.trim_matches('/');
            let name = if directory.is_empty() {
                clean.clone()
            } else {
                format!("{directory}/{clean}")
            };
            let path = PathBuf::from("materials").join(format!("{name}.vmt"));
            prepared
                .assets
                .iter()
                .any(|asset| asset.relative_path == path)
                .then_some(name)
        })
        .ok_or_else(|| MaterialExportError::Decode {
            path: PathBuf::from(name),
            reason: "skin replacement material was not found; choose an existing material or image"
                .into(),
        })
}
