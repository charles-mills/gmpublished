use vformats::Limits;
pub use vformats::mdl::{MdlError, MeshData, ModelAssemblyOptions, ModelVertex, VertexCompression};
use vformats::mdl::{ModelData, assemble_lossy_with_options, parse_mdl, parse_vtx, parse_vvd};

pub fn load_model(mdl: &[u8], vvd: &[u8], vtx: &[u8]) -> Result<ModelData, MdlError> {
    load_model_with_options(mdl, vvd, vtx, ModelAssemblyOptions::default())
}

pub fn load_model_with_options(
    mdl: &[u8],
    vvd: &[u8],
    vtx: &[u8],
    options: ModelAssemblyOptions,
) -> Result<ModelData, MdlError> {
    let limits = Limits::default();
    let mdl = parse_mdl(mdl, &limits)?;
    let vvd = parse_vvd(vvd, &limits)?;
    let vtx = parse_vtx(vtx, &limits)?;
    // Tolerate checksum and mesh-count mismatches so stale companion files
    // still produce a best-effort preview.
    let read = assemble_lossy_with_options(&mdl, &vvd, &vtx, options)?;
    if read.stats.total() > 0 {
        log::debug!(
            "model assembly sanitized structures: {:?}",
            read.stats.sanitized
        );
    }
    Ok(read.model)
}
