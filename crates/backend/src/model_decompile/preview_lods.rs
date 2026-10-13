use super::{DecompileError, invalid, lods};

#[derive(Clone, Debug, PartialEq)]
pub struct ModelLodPreview {
    pub root_lod: usize,
    pub levels: Vec<vformats::mdl::ModelData>,
}

pub fn load_model_lod_preview(
    raw_mdl: &[u8],
    raw_vvd: &[u8],
    raw_vtx: &[u8],
) -> Result<ModelLodPreview, DecompileError> {
    if [raw_mdl, raw_vvd, raw_vtx]
        .iter()
        .any(|bytes| bytes.len() > 128 * 1024 * 1024)
    {
        return Err(DecompileError::SizeLimit);
    }
    let mut header = raw_mdl.to_vec();
    header
        .get_mut(180..184)
        .ok_or_else(|| invalid("Truncated MDL header"))?
        .fill(0);
    let limits = vformats::Limits::default();
    let mut model =
        vformats::mdl::parse_mdl(&header, &limits).map_err(|error| invalid(error.to_string()))?;
    let vertices =
        vformats::mdl::parse_vvd(raw_vvd, &limits).map_err(|error| invalid(error.to_string()))?;
    let mut topology =
        vformats::mdl::parse_vtx(raw_vtx, &limits).map_err(|error| invalid(error.to_string()))?;
    let metadata = lods::Metadata::read(raw_mdl, raw_vvd, raw_vtx, &model, &vertices, &topology)?;
    let root_lod = usize::from(raw_mdl[0x179]);
    let original_materials = model.textures.clone();
    let mut levels = Vec::new();
    let mut vertex_count = 0usize;
    for level in root_lod..metadata.switch_points.len() {
        model.textures.clone_from(&original_materials);
        if metadata.switch_points[level] != -1.0 || metadata.shadow_materials {
            for [source, target] in &metadata.material_replacements[level] {
                for (original, material) in original_materials.iter().zip(&mut model.textures) {
                    if original.eq_ignore_ascii_case(source) {
                        material.clone_from(target);
                    }
                }
            }
        }
        for part in &mut topology.body_parts {
            for choice in &mut part.models {
                choice.lods.swap(0, level);
            }
        }
        // All origMeshVertIDs address the mesh union in the fully fixed-up VVD pool.
        let mut geometry = vformats::mdl::assemble(&model, &vertices, &topology)
            .map_err(|error| invalid(error.to_string()))?
            .model;
        let selected_vertices: usize = geometry.meshes.iter().map(|mesh| mesh.vertices.len()).sum();
        geometry.vertex_count =
            u32::try_from(selected_vertices).map_err(|_| DecompileError::SizeLimit)?;
        vertex_count = vertex_count
            .checked_add(selected_vertices)
            .filter(|count| *count <= 2_000_000)
            .ok_or(DecompileError::SizeLimit)?;
        levels.push(geometry);
        for part in &mut topology.body_parts {
            for choice in &mut part.models {
                choice.lods.swap(0, level);
            }
        }
    }
    Ok(ModelLodPreview { root_lod, levels })
}
