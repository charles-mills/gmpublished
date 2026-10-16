#[cfg(test)]
use super::ResidentTextureMip;
use super::parameters::{scalar, texture_transform};
use super::{
    Arc, DecodedTextureFrames, EffectiveMaterial, MaterialResolver, ResidentTexture,
    ResolvedTextureMip, TextureAssetError, TextureLoadOutcome, TextureMetadata,
    normalize_texture_name, parse_material_color, texture_path, vmt_bool,
};

use vformats::vtf::{VtfError, VtfFormat, texture_flags};

#[cfg(test)]
use vformats::Limits;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedCubemap {
    resident: super::TextureResidency,
    source_metadata: Option<Arc<TextureMetadata>>,
    load_outcome: TextureLoadOutcome,
    pub frame: u32,
}

impl ResolvedCubemap {
    pub(crate) fn resident(&self) -> Option<&Arc<ResidentTexture>> {
        self.resident.resident()
    }
    #[cfg(test)]
    pub(crate) fn face(&self, face: u32) -> Option<&[ResidentTextureMip]> {
        Some(&self.resident()?.layer(self.frame, face)?.mips)
    }
    pub(crate) fn source_metadata(&self) -> Option<&TextureMetadata> {
        self.source_metadata.as_deref()
    }
    pub(crate) fn load_outcome(&self) -> TextureLoadOutcome {
        self.load_outcome
    }
    fn unavailable(metadata: Option<Arc<TextureMetadata>>, outcome: TextureLoadOutcome) -> Self {
        let mut cube = Self::from_rgba(
            0,
            0,
            std::array::from_fn(|_| {
                vec![ResolvedTextureMip {
                    rgba: vec![0, 0, 0, 255],
                    width: 1,
                    height: 1,
                }]
            }),
        )
        .expect("fallback cube has valid dimensions");
        cube.source_metadata = metadata;
        cube.load_outcome = outcome;
        cube
    }
    pub(crate) fn from_rgba(
        flags: u32,
        frame: u32,
        faces: [Vec<ResolvedTextureMip>; 6],
    ) -> Result<Self, TextureAssetError> {
        let first = faces[0].first().ok_or(TextureAssetError::Selection)?;
        let count = frame.checked_add(1).ok_or(TextureAssetError::Selection)?;
        let metadata = super::texture_asset::generated_metadata(
            [first.width, first.height],
            Some(flags),
            count,
            6,
            u8::try_from(faces[0].len()).map_err(|_| TextureAssetError::Selection)?,
        );
        let layers = faces
            .into_iter()
            .enumerate()
            .map(|(face, mips)| super::ResidentTextureLayer {
                frame,
                face: face as u32,
                mips: mips
                    .into_iter()
                    .enumerate()
                    .map(|(level, mip)| super::ResidentTextureMip {
                        source_mip: level as u8,
                        dimensions: [mip.width, mip.height],
                        bytes: super::TextureBytes::owned(mip.rgba),
                    })
                    .collect(),
            })
            .collect();
        let resident = ResidentTexture::generated(
            "material-cube-fallback",
            metadata,
            frame..count,
            0..6,
            layers,
            super::TextureStorage::Rgba8,
        )?;
        let source_metadata = resident.metadata_arc();
        Ok(Self {
            resident: super::TextureResidency::Resident(resident),
            source_metadata: Some(source_metadata),
            load_outcome: TextureLoadOutcome::Generated,
            frame,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EnvironmentMapSource {
    Named(Arc<ResolvedCubemap>),
    LocalProbe,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EnvironmentMapMask {
    None,
    BaseAlpha,
    NormalAlpha,
    Texture,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EnvironmentMapMaterial {
    pub source: EnvironmentMapSource,
    pub path: String,
    pub frame: u32,
    /// Authored values; effective colour conversion belongs to the family.
    pub tint: [f32; 3],
    pub contrast: f32,
    pub saturation: f32,
    pub mask: EnvironmentMapMask,
    pub mask_texture: Option<Arc<DecodedTextureFrames>>,
    pub mask_frame: u32,
    pub mask_transform: [[f32; 4]; 2],
    pub fresnel: f32,
}

#[derive(Debug, thiserror::Error)]
enum CubemapDecodeError {
    #[error(transparent)]
    Texture(#[from] TextureAssetError),
    #[error("expected a square six-face cubemap")]
    NotCubemap,
    #[error("HDR cubemaps require a floating-point scene mode")]
    UnsupportedHdr,
}

fn validate_cubemap(metadata: &TextureMetadata, frame: u32) -> Result<(), CubemapDecodeError> {
    if metadata.flags.unwrap_or(0) & texture_flags::ENVMAP == 0
        || metadata.face_count < 6
        || metadata.original_dimensions[0] != metadata.original_dimensions[1]
    {
        return Err(CubemapDecodeError::NotCubemap);
    }
    if matches!(
        metadata.format,
        Some(VtfFormat::Rgba16161616F | VtfFormat::Rgba16161616)
    ) {
        return Err(CubemapDecodeError::UnsupportedHdr);
    }
    if frame >= metadata.frame_count {
        return Err(TextureAssetError::Vtf(VtfError::OutOfRange).into());
    }
    Ok(())
}

#[cfg(test)]
fn decode_cubemap(
    bytes: &[u8],
    frame: u32,
    max_dimension: Option<u32>,
    limits: &Limits,
) -> Result<ResolvedCubemap, CubemapDecodeError> {
    let asset = super::texture_asset::TextureAsset::from_bytes(Arc::from(bytes), limits)?;
    validate_cubemap(asset.metadata(), frame)?;
    let first = asset.first_mip(max_dimension)?;
    let end = if asset.metadata().flags.unwrap_or(0) & texture_flags::NOMIP != 0 {
        first + 1
    } else {
        asset.metadata().mip_count
    };
    let resident = asset.materialize(
        0..asset.metadata().frame_count,
        0..6,
        first..end,
        super::TextureStorage::Rgba8,
        limits,
    )?;
    Ok(ResolvedCubemap {
        resident: super::TextureResidency::Resident(resident),
        source_metadata: Some(asset.metadata_arc()),
        load_outcome: TextureLoadOutcome::Loaded,
        frame,
    })
}

fn authored_tint(value: Option<&str>) -> [f32; 3] {
    value
        .and_then(|value| {
            parse_material_color(value)
                .map(gmpublished_domain::math::Vec3::to_array)
                .or_else(|| {
                    value
                        .parse::<f32>()
                        .ok()
                        .filter(|number| number.is_finite())
                        .map(|number| [number; 3])
                })
        })
        .unwrap_or([1.0; 3])
}

impl MaterialResolver {
    pub(super) fn resolve_environment_map(
        &self,
        material: &EffectiveMaterial,
    ) -> Option<Arc<EnvironmentMapMaterial>> {
        let path = normalize_texture_name(material.parameter("$envmap")?)?;
        let frame = scalar(material.parameter("$envmapframe"), 0.0).max(0.0) as u32;
        let source = if path == "env_cubemap" {
            EnvironmentMapSource::LocalProbe
        } else {
            EnvironmentMapSource::Named(self.resolve_cubemap(&path, frame)?)
        };
        let mask_texture = material
            .effective_envmap_mask()
            .and_then(normalize_texture_name)
            .and_then(|name| self.resolve_raw_texture_frames(&name));
        let mask = if vmt_bool(material.parameter("$normalmapalphaenvmapmask")) {
            EnvironmentMapMask::NormalAlpha
        } else if vmt_bool(material.parameter("$basealphaenvmapmask")) {
            EnvironmentMapMask::BaseAlpha
        } else if material.effective_envmap_mask().is_some() {
            EnvironmentMapMask::Texture
        } else {
            EnvironmentMapMask::None
        };
        Some(Arc::new(EnvironmentMapMaterial {
            source,
            path,
            frame,
            tint: authored_tint(material.parameter("$envmaptint")),
            contrast: scalar(material.parameter("$envmapcontrast"), 0.0),
            saturation: scalar(material.parameter("$envmapsaturation"), 1.0),
            mask,
            mask_texture,
            mask_frame: scalar(material.parameter("$envmapmaskframe"), 0.0).max(0.0) as u32,
            mask_transform: texture_transform(material.parameter("$envmapmasktransform")),
            fresnel: scalar(material.parameter("$fresnelreflection"), 1.0),
        }))
    }

    pub(crate) fn resolve_cubemap(&self, name: &str, frame: u32) -> Option<Arc<ResolvedCubemap>> {
        let normalized = normalize_texture_name(name)?;
        if std::path::Path::new(&normalized)
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("hdr"))
        {
            log::warn!(
                "cubemap {name}: HDR resource requires a coherent floating-point scene mode"
            );
            return None;
        }
        let path = texture_path(&normalized)?;
        let selected = match self.selected_texture_asset(&path) {
            Ok(selected) => selected,
            Err(super::TextureSourceError::Preparation { .. }) => {
                return Some(Arc::new(ResolvedCubemap::unavailable(
                    None,
                    TextureLoadOutcome::Unplanned,
                )));
            }
            Err(
                super::TextureSourceError::EncodedBudget { metadata, .. }
                | super::TextureSourceError::WorkingBudget { metadata, .. },
            ) => {
                return Some(Arc::new(ResolvedCubemap::unavailable(
                    metadata,
                    TextureLoadOutcome::SourceBudgetRejected,
                )));
            }
            Err(error) => {
                log::warn!("cubemap {name}: {error}");
                return None;
            }
        };
        let source_metadata = selected.asset.metadata_arc();
        if let Err(error) = validate_cubemap(&source_metadata, frame) {
            log::warn!("cubemap {name}: {error}");
            return None;
        }
        let (resident, load_outcome, frame) = match self.resident_texture(
            &selected,
            0..6,
            !self.bc_textures_enabled(),
        ) {
            Ok(resident) => {
                let outcome = resident.load_outcome();
                (resident, outcome, frame)
            }
            Err(error) => {
                log::warn!("cubemap {name}: {error}");
                if matches!(&error,TextureAssetError::Vtf(error) if super::native_invalid_texture(error))
                {
                    return None;
                }
                let outcome = match error {
                    TextureAssetError::DimensionLimit(_)
                    | TextureAssetError::Budget
                    | TextureAssetError::ResidentImageBudget { .. }
                    | TextureAssetError::WorkingLimit { .. }
                    | TextureAssetError::ImageCount(_)
                    | TextureAssetError::Vtf(VtfError::DecodedTooLarge { .. }) => {
                        TextureLoadOutcome::QualityRejected
                    }
                    TextureAssetError::UnplannedRequest | TextureAssetError::ClosedPlan => {
                        TextureLoadOutcome::Unplanned
                    }
                    _ => TextureLoadOutcome::Unsupported,
                };
                let fallback =
                    ResolvedCubemap::unavailable(Some(Arc::clone(&source_metadata)), outcome);
                (fallback.resident, outcome, 0)
            }
        };
        Some(Arc::new(ResolvedCubemap {
            resident,
            source_metadata: Some(source_metadata),
            load_outcome,
            frame,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::fixture_vtf_bytes;

    fn fixture(faces: u8, frames: u16, mip_count: u8) -> Vec<u8> {
        let mut payload = Vec::new();
        for mip in (0..mip_count).rev() {
            let side = 4_usize >> mip;
            for frame in 0..frames {
                for face in 0..faces {
                    for _ in 0..side * side {
                        payload.extend_from_slice(&[
                            face * 20,
                            frame as u8 * 70,
                            mip * 30,
                            37 + face,
                        ]);
                    }
                }
            }
        }
        let placeholder = vec![&[][..]; usize::from(mip_count)];
        let mut bytes = fixture_vtf_bytes(4, 4, VtfFormat::Rgba8888, &placeholder);
        bytes[20..24].copy_from_slice(&texture_flags::ENVMAP.to_le_bytes());
        bytes[24..26].copy_from_slice(&frames.to_le_bytes());
        bytes[26..28].copy_from_slice(&(if faces == 7 { 0_u16 } else { u16::MAX }).to_le_bytes());
        bytes.extend(payload);
        bytes
    }

    #[test]
    fn cubemap_frame_face_mip_order_and_alpha_are_independent() {
        for count in [6, 7] {
            let cube = decode_cubemap(&fixture(count, 2, 3), 1, None, &Limits::default()).unwrap();
            assert_eq!(cube.frame, 1);
            assert_eq!(
                cube.source_metadata().unwrap().flags,
                Some(texture_flags::ENVMAP)
            );
            for face in 0..6 {
                let mips = cube.face(face).unwrap();
                assert_eq!(mips.len(), 3);
                for (mip, image) in mips.iter().enumerate() {
                    assert_eq!(
                        (image.dimensions[0], image.dimensions[1]),
                        (4 >> mip, 4 >> mip)
                    );
                    assert_eq!(
                        image.bytes.as_slice()[..4],
                        [face as u8 * 20, 70, mip as u8 * 30, 37 + face as u8]
                    );
                }
            }
        }
    }

    #[test]
    fn cubemap_limits_select_stored_mips_and_reject_malformed_resources() {
        let bytes = fixture(6, 1, 3);
        let cube = decode_cubemap(&bytes, 0, Some(2), &Limits::default()).unwrap();
        assert_eq!(cube.face(0).unwrap()[0].dimensions[0], 2);
        assert_eq!(cube.face(0).unwrap().len(), 2);
        assert!(matches!(
            decode_cubemap(&bytes, 1, None, &Limits::default()),
            Err(CubemapDecodeError::Texture(TextureAssetError::Vtf(
                VtfError::OutOfRange
            )))
        ));
        assert!(decode_cubemap(&bytes[..bytes.len() - 1], 0, None, &Limits::default()).is_err());
        let small = Limits {
            max_entry_bytes: 8,
            ..Limits::default()
        };
        assert!(matches!(
            decode_cubemap(&bytes, 0, None, &small),
            Err(CubemapDecodeError::Texture(TextureAssetError::Vtf(
                VtfError::DecodedTooLarge { .. }
            )))
        ));
        let mut noncube = bytes.clone();
        noncube[20..24].copy_from_slice(&0_u32.to_le_bytes());
        assert!(matches!(
            decode_cubemap(&noncube, 0, None, &Limits::default()),
            Err(CubemapDecodeError::NotCubemap)
        ));
        let mut hdr = bytes;
        hdr[52..56].copy_from_slice(&24_u32.to_le_bytes());
        assert!(matches!(
            decode_cubemap(&hdr, 0, None, &Limits::default()),
            Err(CubemapDecodeError::UnsupportedHdr)
        ));
    }

    #[test]
    fn cube_resolution_is_map_scoped_and_caches_frame_and_decode_policy() {
        let path = "materials/maps/shared/c0_0_0.vtf";
        let first =
            MaterialResolver::prop_lighting_fixture(vec![(path, fixture(6, 2, 3))], Vec::new());
        let mut other_bytes = fixture(6, 2, 3);
        other_bytes[64..68].copy_from_slice(&[199; 4]);
        let other = MaterialResolver::prop_lighting_fixture(vec![(path, other_bytes)], Vec::new());
        let a = first.resolve_cubemap("maps/shared/c0_0_0", 0).unwrap();
        assert!(Arc::ptr_eq(
            a.resident().unwrap(),
            first
                .resolve_cubemap("maps/shared/c0_0_0", 0)
                .unwrap()
                .resident()
                .unwrap()
        ));
        let b = other.resolve_cubemap("maps/shared/c0_0_0", 0).unwrap();
        assert_ne!(a.face(0).unwrap()[2], b.face(0).unwrap()[2]);
        assert!(Arc::ptr_eq(
            a.resident().unwrap(),
            first
                .resolve_cubemap("maps/shared/c0_0_0", 1)
                .unwrap()
                .resident()
                .unwrap()
        ));
        let capped = first
            .with_decoded_texture_max_dimension(2)
            .resolve_cubemap("maps/shared/c0_0_0", 0)
            .unwrap();
        assert_eq!(capped.face(0).unwrap()[0].dimensions[0], 2);
        let invalid = MaterialResolver::prop_lighting_fixture(
            vec![(path, vec![0])],
            vec![(path, fixture(6, 1, 3))],
        );
        assert!(invalid.resolve_cubemap("maps/shared/c0_0_0", 0).is_none());
    }

    #[test]
    fn authored_environment_state_retains_local_probe_and_raw_tint() {
        let resolver = MaterialResolver::prop_lighting_fixture(Vec::new(), Vec::new());
        let document = vformats::vmt::parse(r#"VertexLitGeneric { "$envmap" "env_cubemap" "$envmapframe" "2" "$envmapmaskframe" "3" "$envmaptint" "[0.5 1 2]" "$envmapcontrast" "0.3" "$basealphaenvmapmask" "1" "$envmapmasktransform" "center 0 0 scale 2 3 rotate 0 translate 0.25 0.5" }"#, &Limits::default()).unwrap();
        let state = resolver
            .resolve_environment_map(&EffectiveMaterial::from_document(&document))
            .unwrap();
        assert!(matches!(state.source, EnvironmentMapSource::LocalProbe));
        assert_eq!(state.tint, [0.5, 1.0, 2.0]);
        assert_eq!(state.frame, 2);
        assert_eq!(state.mask_frame, 3);
        assert_eq!(state.mask, EnvironmentMapMask::BaseAlpha);
        assert_eq!(
            state.mask_transform,
            [[2.0, 0.0, 0.0, 0.25], [0.0, 3.0, 0.0, 0.5]]
        );
        assert_eq!(state.contrast, 0.3);
        assert_eq!(state.saturation, 1.0);
        assert_eq!(state.fresnel, 1.0);
    }

    #[test]
    fn cubemap_budget_charges_shared_frames_once_and_keeps_quality_separate() {
        let path = "materials/maps/shared/c0_0_0.vtf";
        let budget = Arc::new(super::super::DecodedTextureBudget::new(1008));
        let resolver =
            MaterialResolver::prop_lighting_fixture(vec![(path, fixture(6, 2, 3))], Vec::new())
                .with_decoded_texture_budget(Arc::clone(&budget));
        let first = resolver.resolve_cubemap("maps/shared/c0_0_0", 0).unwrap();
        let second = resolver.resolve_cubemap("maps/shared/c0_0_0", 1).unwrap();
        assert!(Arc::ptr_eq(
            first.resident().unwrap(),
            second.resident().unwrap()
        ));
        assert_eq!(budget.decoded_bytes(), 1008);
        assert_eq!(budget.rejected_textures(), 0);
        let small = Arc::new(super::super::DecodedTextureBudget::new(504));
        let rejected = resolver
            .with_decoded_texture_budget(Arc::clone(&small))
            .resolve_cubemap("maps/shared/c0_0_0", 0)
            .unwrap();
        assert_eq!(rejected.load_outcome(), TextureLoadOutcome::QualityRejected);
        assert_eq!(rejected.source_metadata().unwrap().frame_count, 2);
        assert_eq!(small.decoded_bytes(), 0);
        assert_eq!(small.rejected_textures(), 1);
        let mut malformed = fixture(6, 1, 3);
        malformed.pop();
        let failed = Arc::new(super::super::DecodedTextureBudget::new(504));
        let resolver = MaterialResolver::prop_lighting_fixture(vec![(path, malformed)], Vec::new())
            .with_decoded_texture_budget(Arc::clone(&failed));
        assert!(resolver.resolve_cubemap("maps/shared/c0_0_0", 0).is_none());
        assert_eq!(failed.decoded_bytes(), 0);
    }
}
