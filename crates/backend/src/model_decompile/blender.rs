use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};

use serde_json::{Value, json};
use vformats::mdl::Mdl;

use super::{DecompileError, MAX_OUTPUT_BYTES, check_cancelled, invalid, io_error};

#[path = "blender/animation.rs"]
mod animation;
#[path = "blender/flexes.rs"]
mod flexes;
#[path = "blender/geometry.rs"]
mod geometry;
#[path = "blender/materials.rs"]
mod materials;
#[path = "blender/metadata.rs"]
mod metadata;

#[cfg(test)]
#[path = "blender/tests.rs"]
mod tests;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlenderExportReport {
    pub output_directory: PathBuf,
    pub model_path: PathBuf,
    pub files: Vec<PathBuf>,
    pub bytes_written: u64,
    pub warnings: Vec<String>,
}

pub fn export_blender_model(
    path: &Path,
    destination: &Path,
) -> Result<BlenderExportReport, DecompileError> {
    export_blender_model_with_cancel(path, destination, &AtomicBool::new(false), |_| None)
}

pub fn export_blender_model_with_cancel(
    path: &Path,
    destination: &Path,
    cancelled: &AtomicBool,
    mut read_material: impl FnMut(&str) -> Option<Vec<u8>>,
) -> Result<BlenderExportReport, DecompileError> {
    check_cancelled(cancelled)?;
    if fs::symlink_metadata(destination).is_ok() {
        return Err(DecompileError::DestinationExists(destination.to_path_buf()));
    }
    let name = destination
        .file_name()
        .ok_or_else(|| invalid("Choose a new destination directory"))?;
    let parent = destination
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let parent = fs::canonicalize(parent).map_err(|error| io_error(parent, error))?;
    let destination = parent.join(name);
    let loaded = super::load_model(path, cancelled)?;
    let model = &loaded.model;
    let lods = super::lods::Metadata::read(
        &loaded.raw_mdl,
        &loaded.raw_vvd,
        &loaded.raw_vtx,
        model,
        &loaded.vertices,
        &loaded.topology,
    )?;
    let mut material_model = model.clone();
    for replacement in lods.material_replacements.iter().flatten() {
        if !material_model.textures.contains(&replacement[1]) {
            material_model.textures.push(replacement[1].clone());
        }
    }
    let material_set = materials::prepare(&material_model, path, &mut read_material, cancelled)?;
    check_cancelled(cancelled)?;
    let mut output = Document::new(model);
    output.material_names = material_model.textures;
    output.materials = material_set.materials;
    output.warnings = material_set.warnings;
    for (name, bytes) in material_set.images {
        check_cancelled(cancelled)?;
        let view = output.view(&bytes, None)?;
        output
            .images
            .push(json!({"name":name,"bufferView":view,"mimeType":"image/png"}));
    }
    geometry::write(&mut output, &loaded, &lods, cancelled)?;
    animation::write(&mut output, &loaded, cancelled)?;
    match metadata::write(&mut output, &loaded, path, cancelled) {
        Ok(()) => {}
        Err(error @ (DecompileError::Cancelled | DecompileError::SizeLimit)) => return Err(error),
        Err(error) => output.warnings.push(format!(
            "Some Source authoring settings could not be preserved as custom data: {error}"
        )),
    }
    check_cancelled(cancelled)?;
    let bytes = output.finish()?;
    let temporary = tempfile::Builder::new()
        .prefix(".blender-export-")
        .tempdir_in(&parent)
        .map_err(|error| io_error(&parent, error))?;
    let temporary_path = temporary.path().join("model.glb");
    fs::write(&temporary_path, &bytes).map_err(|error| io_error(&temporary_path, error))?;
    check_cancelled(cancelled)?;
    if fs::symlink_metadata(&destination).is_ok() {
        return Err(DecompileError::DestinationExists(destination));
    }
    fs::rename(temporary.path(), &destination).map_err(|error| io_error(&destination, error))?;
    let model_path = destination.join("model.glb");
    Ok(BlenderExportReport {
        output_directory: destination,
        files: vec![model_path.clone()],
        model_path,
        bytes_written: bytes.len() as u64,
        warnings: output.warnings,
    })
}

const INCHES_TO_METERS: f32 = 0.0254;

fn direction([x, y, z]: [f32; 3]) -> [f32; 3] {
    [y, z, x]
}
fn position(value: [f32; 3]) -> [f32; 3] {
    direction(value).map(|value| value * INCHES_TO_METERS)
}
fn rotation([x, y, z, w]: [f32; 4]) -> [f32; 4] {
    [y, z, x, w]
}

struct Document {
    bin: Vec<u8>,
    views: Vec<Value>,
    accessors: Vec<Value>,
    nodes: Vec<Value>,
    meshes: Vec<Value>,
    materials: Vec<Value>,
    material_names: Vec<String>,
    images: Vec<Value>,
    skins: Vec<Value>,
    scenes: Vec<Value>,
    animations: Vec<Value>,
    variants: Vec<Value>,
    extras: Value,
    warnings: Vec<String>,
}

impl Document {
    fn new(model: &Mdl) -> Self {
        Self {
            bin: vec![],
            views: vec![],
            accessors: vec![],
            nodes: vec![],
            meshes: vec![],
            materials: vec![],
            material_names: vec![],
            images: vec![],
            skins: vec![],
            scenes: vec![],
            animations: vec![],
            variants: vec![],
            warnings: vec![],
            extras: json!({"sourceModel":model.name,"sourceVersion":model.version,"sourceFlags":model.flags,"sourceUnits":"inches","metersPerSourceUnit":INCHES_TO_METERS,"sourceToGltfAxes":[[0,1,0],[0,0,1],[1,0,0]],"sourceSkinTable":model.skin_table,"sourceSkinReferenceCount":model.skin_reference_count}),
        }
    }

    fn view(&mut self, bytes: &[u8], target: Option<u32>) -> Result<usize, DecompileError> {
        let padding = (4 - self.bin.len() % 4) % 4;
        let end = self
            .bin
            .len()
            .checked_add(padding)
            .and_then(|len| len.checked_add(bytes.len()))
            .filter(|len| *len <= MAX_OUTPUT_BYTES)
            .ok_or(DecompileError::SizeLimit)?;
        self.bin.resize(self.bin.len() + padding, 0);
        let mut value = json!({"buffer":0,"byteOffset":self.bin.len(),"byteLength":bytes.len()});
        if let Some(target) = target {
            value["target"] = json!(target);
        }
        let index = self.views.len();
        self.views.push(value);
        self.bin.extend_from_slice(bytes);
        debug_assert_eq!(self.bin.len(), end);
        Ok(index)
    }

    fn floats<const N: usize>(
        &mut self,
        values: &[[f32; N]],
        kind: &str,
        bounds: bool,
        target: Option<u32>,
    ) -> Result<usize, DecompileError> {
        if values.is_empty() {
            return Err(invalid("An exported accessor is empty"));
        }
        let size = values
            .len()
            .checked_mul(N * 4)
            .filter(|size| *size <= MAX_OUTPUT_BYTES.saturating_sub(self.bin.len()))
            .ok_or(DecompileError::SizeLimit)?;
        let mut bytes = Vec::with_capacity(size);
        let mut min = [f32::INFINITY; N];
        let mut max = [f32::NEG_INFINITY; N];
        for value in values {
            for (axis, component) in value.iter().copied().enumerate() {
                if !component.is_finite() {
                    return Err(invalid("An exported coordinate is nonfinite"));
                }
                min[axis] = min[axis].min(component);
                max[axis] = max[axis].max(component);
                bytes.extend_from_slice(&component.to_le_bytes());
            }
        }
        let view = self.view(&bytes, target)?;
        let mut value =
            json!({"bufferView":view,"componentType":5126,"count":values.len(),"type":kind});
        if bounds {
            value["min"] = json!(min.as_slice());
            value["max"] = json!(max.as_slice());
        }
        let index = self.accessors.len();
        self.accessors.push(value);
        Ok(index)
    }

    fn integers(
        &mut self,
        bytes: &[u8],
        component: u32,
        count: usize,
        kind: &str,
        target: u32,
    ) -> Result<usize, DecompileError> {
        let view = self.view(bytes, Some(target))?;
        let index = self.accessors.len();
        self.accessors
            .push(json!({"bufferView":view,"componentType":component,"count":count,"type":kind}));
        Ok(index)
    }

    fn finish(&mut self) -> Result<Vec<u8>, DecompileError> {
        self.nodes[0]["extras"] = json!({"gmpublishedSourceMetadata":serde_json::to_string(&self.extras).map_err(|error|invalid(error.to_string()))?});
        let mut document = json!({
            "asset":{"version":"2.0","generator":"gmpublished"},
            "buffers":[{"byteLength":self.bin.len()}],
            "bufferViews":self.views,"accessors":self.accessors,
            "nodes":self.nodes,"meshes":self.meshes,"scenes":self.scenes,"scene":0,
        });
        if !self.materials.is_empty() {
            document["materials"] = json!(self.materials);
        }
        if !self.images.is_empty() {
            document["images"] = json!(self.images);
            document["textures"] = json!(
                (0..self.images.len())
                    .map(|source| json!({"source":source}))
                    .collect::<Vec<_>>()
            );
        }
        if !self.skins.is_empty() {
            document["skins"] = json!(self.skins);
        }
        if !self.animations.is_empty() {
            document["animations"] = json!(self.animations);
        }
        let mut extensions = std::collections::BTreeSet::new();
        for material in &self.materials {
            if let Some(used) = material.get("extensions").and_then(Value::as_object) {
                extensions.extend(used.keys().cloned());
            }
        }
        if !self.variants.is_empty() {
            extensions.insert("KHR_materials_variants".into());
            document["extensions"] = json!({"KHR_materials_variants":{"variants":self.variants}});
        }
        if !extensions.is_empty() {
            document["extensionsUsed"] = json!(extensions);
        }
        let mut json = serde_json::to_vec(&document).map_err(|error| invalid(error.to_string()))?;
        while !json.len().is_multiple_of(4) {
            json.push(b' ');
        }
        let mut bin = std::mem::take(&mut self.bin);
        while !bin.len().is_multiple_of(4) {
            bin.push(0);
        }
        let length = 28usize
            .checked_add(json.len())
            .and_then(|length| length.checked_add(bin.len()))
            .filter(|length| *length <= MAX_OUTPUT_BYTES)
            .ok_or(DecompileError::SizeLimit)?;
        let mut bytes = Vec::with_capacity(length);
        for value in [0x46546c67, 2, length as u32, json.len() as u32, 0x4e4f534a] {
            bytes.extend_from_slice(&u32::to_le_bytes(value));
        }
        bytes.extend_from_slice(&json);
        bytes.extend_from_slice(&(bin.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&0x004e4942u32.to_le_bytes());
        bytes.extend_from_slice(&bin);
        Ok(bytes)
    }
}
