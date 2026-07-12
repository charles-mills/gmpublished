//! Model loading: the wire formats (`.mdl`/`.vvd`/`.vtx`) and geometry
//! assembly live in [`vformats::mdl`]; this is the one-shot the app
//! consumes, plus degradation-stats logging.

use vformats::Limits;
pub use vformats::mdl::{MdlError, MeshData, ModelVertex};
use vformats::mdl::{ModelData, assemble_lossy, parse_mdl, parse_vtx, parse_vvd};

pub fn load_model(mdl: &[u8], vvd: &[u8], vtx: &[u8]) -> Result<ModelData, MdlError> {
    let limits = Limits::default();
    let mdl = parse_mdl(mdl, &limits)?;
    let vvd = parse_vvd(vvd, &limits)?;
    let vtx = parse_vtx(vtx, &limits)?;
    // Renderer-feeding format: a checksum or mesh-count mismatch
    // between the trio (e.g. a workshop addon shipping stale sibling
    // files) degrades and renders best-effort rather than blocking the
    // whole model, matching every other assembly anomaly here.
    let read = assemble_lossy(&mdl, &vvd, &vtx)?;
    if read.stats.total() > 0 {
        log::debug!(
            "model assembly sanitized structures: {:?}",
            read.stats.sanitized
        );
    }
    Ok(read.model)
}
