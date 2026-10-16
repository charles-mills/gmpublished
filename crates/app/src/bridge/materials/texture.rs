use super::normalize_archive_path;
use std::collections::BTreeSet;
use vformats::Limits;

pub fn decode_vtf_rgba(bytes: &[u8]) -> Result<vformats::vtf::RgbaImage, vformats::vtf::VtfError> {
    vformats::vtf::parse(bytes, &Limits::default())?.decode_rgba(0, 0, 0, &Limits::default())
}

pub(super) fn push_matching_path(
    path: &str,
    matches: &impl Fn(&str) -> bool,
    seen: &mut BTreeSet<String>,
    out: &mut Vec<String>,
) {
    let Some(path) = normalize_archive_path(path) else {
        return;
    };
    if matches(&path) && seen.insert(path.clone()) {
        out.push(path);
    }
}

#[cfg(test)]
pub(super) fn resolved_bc_texture(
    bytes: &[u8],
    max_dimension: Option<u32>,
) -> Option<super::ResolvedTexture> {
    let limits = Limits::default();
    let asset =
        super::texture_asset::TextureAsset::from_bytes(std::sync::Arc::from(bytes), &limits)
            .ok()?;
    let first = asset.first_mip(max_dimension).ok()?;
    let format = asset.metadata().format?.bc()?;
    let end = if asset.metadata().flags.unwrap_or(0) & vformats::vtf::texture_flags::NOMIP != 0 {
        first + 1
    } else {
        asset.metadata().mip_count
    };
    let resident = asset
        .materialize(
            0..asset.metadata().frame_count,
            0..1,
            first..end,
            super::TextureStorage::Bc(format),
            &limits,
        )
        .ok()?;
    Some(super::ResolvedTexture::from_resident(
        super::TextureResidency::Resident(resident),
        asset.metadata_arc(),
    ))
}
