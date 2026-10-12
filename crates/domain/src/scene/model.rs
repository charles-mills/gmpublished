use vformats::Limits;
pub use vformats::mdl::{MdlError, MeshData, ModelVertex};
use vformats::mdl::{ModelData, assemble_lossy, parse_mdl, parse_vtx, parse_vvd};

pub fn load_model(mdl: &[u8], vvd: &[u8], vtx: &[u8]) -> Result<ModelData, MdlError> {
    let limits = Limits::default();
    let mdl = parse_mdl(mdl, &limits)?;
    let vvd = parse_vvd(vvd, &limits)?;
    let vtx = parse_vtx(vtx, &limits)?;
    // Tolerate checksum and mesh-count mismatches so stale companion files
    // still produce a best-effort preview.
    let read = assemble_lossy(&mdl, &vvd, &vtx)?;
    if read.stats.total() > 0 {
        log::debug!(
            "model assembly sanitized structures: {:?}",
            read.stats.sanitized
        );
    }
    Ok(read.model)
}
