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
    let source = super::ReloadableTextureSource::inspect(
        super::texture_asset::TextureOrigin {
            path: super::ContentPath::new("materials/fixture.vtf").unwrap(),
            tier: super::ContentSourceTier::Addon,
        },
        std::sync::Arc::new(super::reloadable::PreloadSource(std::sync::Arc::from(
            bytes,
        ))),
        Limits::default(),
        &Default::default(),
        &std::sync::Arc::new(super::TextureWorkingBudget::new(usize::MAX)),
    )
    .ok()?;
    source.metadata().format?.bc()?;
    let resolver = super::MaterialResolver::prop_lighting_fixture(Vec::new(), Vec::new());
    let resolver = max_dimension.map_or_else(
        || resolver.variant(resolver.config.clone()),
        |max| resolver.with_decoded_texture_max_dimension(max),
    );
    let residency = resolver
        .reloadable_residency(std::sync::Arc::clone(&source), 0, 0..1, false)
        .ok()?;
    Some(super::ResolvedTexture::from_resident(
        super::TextureResidency::Resident(std::sync::Arc::clone(residency.read_for_test())),
        source.metadata_arc(),
    ))
}
