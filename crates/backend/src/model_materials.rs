use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};

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
            warnings: Vec::new(),
        }
        .write_to(root)?;
        Ok(warnings)
    }
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
        warnings: Vec::new(),
    };
    let mut directories = Vec::new();
    for directory in material_directories {
        let clean = directory.replace('\\', "/");
        if clean.trim_matches('/').is_empty() {
            directories.push(String::new());
        } else {
            directories.push(safe_relative_path(clean.trim_end_matches('/'))?);
        }
    }
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
    })
}

pub fn prepare_materials_for_source(
    material_names: &[String],
    material_directories: &[String],
    source_file: &Path,
) -> Result<PreparedMaterials, MaterialExportError> {
    let source_file =
        fs::canonicalize(source_file).map_err(|error| io_error(source_file, error))?;
    let root = source_file.parent().expect("source file has parent");
    let mut prepared = prepare_materials(material_names, material_directories, root)?;
    let imported = source::read_materials(&source_file, material_names, material_directories)?;
    let directory = material_directories
        .first()
        .map_or(Ok(String::new()), |directory| {
            let clean = directory.replace('\\', "/");
            if clean.trim_matches('/').is_empty() {
                Ok(String::new())
            } else {
                safe_relative_path(clean.trim_end_matches('/'))
            }
        })?;
    let mut imported_names = BTreeSet::new();
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
        if prepared
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
        let mut vmt = format!("\"VertexLitGeneric\"\n{{\n\t\"$basetexture\" \"{name}\"\n");
        if material.alpha_mask {
            use std::fmt::Write as _;
            writeln!(
                vmt,
                "\t\"$alphatest\" \"1\"\n\t\"$alphatestreference\" \"{}\"",
                material.alpha_cutoff
            )
            .expect("string write succeeds");
        } else if material.translucent && alpha {
            vmt.push_str("\t\"$translucent\" \"1\"\n");
        }
        if material.double_sided {
            vmt.push_str("\t\"$nocull\" \"1\"\n");
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
            use std::fmt::Write as _;
            writeln!(vmt, "\t\"$bumpmap\" \"{normal_name}\"").expect("string write succeeds");
        }
        vmt.push_str("}\n");
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
    Ok(prepared)
}

pub fn prepare_materials_for_sources(
    material_names: &[String],
    material_directories: &[String],
    source_root: &Path,
    source_files: &[PathBuf],
) -> Result<PreparedMaterials, MaterialExportError> {
    let mut prepared = prepare_materials(material_names, material_directories, source_root)?;
    let preferred: BTreeSet<_> = prepared
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
        let imported = prepare_materials_for_source(material_names, material_directories, &path)?;
        for asset in imported.assets {
            if let Some(existing) = assets.get(&asset.relative_path) {
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
    let mut current = root.to_owned();
    for component in relative.components() {
        let Component::Normal(name) = component else {
            return Ok(None);
        };
        let exact = current.join(name);
        if exact.exists() {
            current = exact;
            continue;
        }
        if !current.is_dir() {
            return Ok(None);
        }
        let entries = fs::read_dir(&current).map_err(|source| io_error(&current, source))?;
        let mut matched = None;
        for entry in entries {
            let entry = entry.map_err(|source| io_error(&current, source))?;
            if entry
                .file_name()
                .to_string_lossy()
                .eq_ignore_ascii_case(&name.to_string_lossy())
            {
                if matched.is_some() {
                    return Err(MaterialExportError::Conflict(exact));
                }
                matched = Some(entry.path());
            }
        }
        let Some(path) = matched else { return Ok(None) };
        current = path;
    }
    Ok(Some(current))
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
    material_name(&mdlc::mdl_writer::normalize_texture_name(name, directories))
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
    let directory = directories
        .first()
        .map_or("", String::as_str)
        .trim_matches('/');
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
