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

const DEFAULT_CUBEMAP: &str = "engine/defaultcubemap";

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum CubemapFailure {
    #[error(transparent)]
    SourceUnavailable(super::SourceDiscoveryFailure),
    #[error("cubemap source is absent")]
    Missing,
    #[error("cubemap has an invalid VTF payload: {0}")]
    InvalidVtf(VtfError),
    #[error("expected a square six-face cubemap")]
    NotCubemap,
    #[error("HDR cubemaps require a floating-point scene mode")]
    UnsupportedHdr,
    #[error("cubemap frame {requested} exceeds frame count {count}")]
    Frame { requested: u32, count: u32 },
    #[error("invalid cubemap path")]
    Path,
    #[error("cubemap source read failed: {0}")]
    SourceRead(Arc<str>),
    #[error(transparent)]
    Texture(TextureAssetError),
    #[error("default cubemap failed after {authored}: {fallback}")]
    DefaultUnavailable {
        authored: Box<Self>,
        fallback: Box<Self>,
    },
}

impl CubemapFailure {
    fn substitutes_default(&self) -> bool {
        matches!(self, Self::Missing)
            || matches!(self, Self::InvalidVtf(error) if super::native_invalid_texture(error))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CubemapResolution {
    Authored,
    Default { authored_failure: CubemapFailure },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedCubemap {
    resident: super::TextureResidency,
    source_metadata: Option<Arc<TextureMetadata>>,
    load_outcome: TextureLoadOutcome,
    pub frame: u32,
    pub resolution: super::CubemapResolution,
    pub effective_path: Option<String>,
    pub failure: Option<super::CubemapFailure>,
}

impl ResolvedCubemap {
    #[cfg(test)]
    pub(crate) fn read_for_test(&self) -> &Arc<ResidentTexture> {
        self.resident.read_for_test()
    }

    pub(crate) fn on_demand(
        &self,
    ) -> Option<(
        &Arc<super::ReloadableTextureSource>,
        &super::TextureFrameRequest,
    )> {
        self.resident.on_demand()
    }
    pub(crate) fn resident(&self) -> Option<&Arc<ResidentTexture>> {
        self.resident.resident()
    }
    #[cfg(test)]
    pub(crate) fn face(&self, face: u32) -> Option<&[ResidentTextureMip]> {
        Some(&self.read_for_test().layer(self.frame, face)?.mips)
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
            resolution: CubemapResolution::Authored,
            effective_path: None,
            failure: None,
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
    let source = super::ReloadableTextureSource::inspect(
        super::texture_asset::TextureOrigin {
            path: super::ContentPath::new("materials/fixture.vtf").unwrap(),
            tier: super::ContentSourceTier::Addon,
        },
        Arc::new(super::reloadable::PreloadSource(Arc::from(bytes))),
        *limits,
        &Default::default(),
        &Arc::new(super::TextureWorkingBudget::new(usize::MAX)),
    )?;
    validate_cubemap(source.metadata(), frame)?;
    let resolver = MaterialResolver::prop_lighting_fixture(Vec::new(), Vec::new());
    let resolver = max_dimension.map_or_else(
        || resolver.variant(resolver.config.clone()),
        |max| resolver.with_decoded_texture_max_dimension(max),
    );
    let resolver = resolver
        .with_texture_resource_limit(usize::try_from(limits.max_entry_bytes).unwrap_or(usize::MAX));
    let residency = resolver.reloadable_residency(Arc::clone(&source), frame, 0..6, true)?;
    let resident = Arc::clone(residency.read_for_test());
    Ok(ResolvedCubemap {
        resident: super::TextureResidency::Resident(resident),
        source_metadata: Some(source.metadata_arc()),
        load_outcome: TextureLoadOutcome::Loaded,
        frame,
        resolution: CubemapResolution::Authored,
        effective_path: None,
        failure: None,
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
            EnvironmentMapSource::Named(
                self.resolve_named_cubemap(&path, frame)
                    .inspect_err(|error| log::warn!("cubemap {path}: {error}"))
                    .ok()?,
            )
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
        self.load_cubemap(name, frame)
            .inspect_err(|error| log::warn!("cubemap {name}: {error}"))
            .ok()
            .map(Arc::new)
    }

    fn resolve_named_cubemap(
        &self,
        name: &str,
        frame: u32,
    ) -> Result<Arc<ResolvedCubemap>, CubemapFailure> {
        match self.load_cubemap(name, frame) {
            Ok(cube) => Ok(Arc::new(cube)),
            Err(authored) if authored.substitutes_default() && name != DEFAULT_CUBEMAP => {
                let mut cube = self.load_cubemap(DEFAULT_CUBEMAP, 0).map_err(|fallback| {
                    CubemapFailure::DefaultUnavailable {
                        authored: Box::new(authored.clone()),
                        fallback: Box::new(fallback),
                    }
                })?;
                cube.resolution = CubemapResolution::Default {
                    authored_failure: authored,
                };
                Ok(Arc::new(cube))
            }
            Err(error) => Err(error),
        }
    }

    fn load_cubemap(&self, name: &str, frame: u32) -> Result<ResolvedCubemap, CubemapFailure> {
        let normalized = normalize_texture_name(name).ok_or(CubemapFailure::Path)?;
        if std::path::Path::new(&normalized)
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("hdr"))
        {
            return Err(CubemapFailure::UnsupportedHdr);
        }
        let path = texture_path(&normalized).ok_or(CubemapFailure::Path)?;
        let selected = match self.selected_reloadable_texture(&path) {
            Ok(selected) => selected,
            Err(super::TextureSourceError::SourceUnavailable(error)) => {
                let mut cube =
                    ResolvedCubemap::unavailable(None, TextureLoadOutcome::SourceUnavailable);
                cube.effective_path = Some(normalized);
                cube.failure = Some(CubemapFailure::SourceUnavailable(error));
                return Ok(cube);
            }
            Err(super::TextureSourceError::Preparation { error, .. }) => {
                let mut cube = ResolvedCubemap::unavailable(None, TextureLoadOutcome::Unplanned);
                cube.effective_path = Some(normalized);
                cube.failure = Some(CubemapFailure::Texture(error));
                return Ok(cube);
            }
            Err(super::TextureSourceError::WorkingBudget {
                metadata, error, ..
            }) => {
                let mut cube = ResolvedCubemap::unavailable(
                    metadata,
                    TextureLoadOutcome::SourceBudgetRejected,
                );
                cube.effective_path = Some(normalized);
                cube.failure = Some(CubemapFailure::Texture(error));
                return Ok(cube);
            }
            Err(super::TextureSourceError::Missing) => return Err(CubemapFailure::Missing),
            Err(super::TextureSourceError::Read { message, .. }) => {
                return Err(CubemapFailure::SourceRead(message));
            }
            Err(super::TextureSourceError::Invalid { error, .. }) => {
                return Err(match error {
                    TextureAssetError::Vtf(error) => CubemapFailure::InvalidVtf(error),
                    error => CubemapFailure::Texture(error),
                });
            }
        };
        let source_metadata = selected.metadata_arc();
        validate_cubemap(&source_metadata, frame).map_err(|error| match error {
            CubemapDecodeError::NotCubemap => CubemapFailure::NotCubemap,
            CubemapDecodeError::UnsupportedHdr => CubemapFailure::UnsupportedHdr,
            CubemapDecodeError::Texture(_) => CubemapFailure::Frame {
                requested: frame,
                count: source_metadata.frame_count,
            },
        })?;
        let (resident, load_outcome, frame, failure) =
            match self.reloadable_residency(selected, frame, 0..6, !self.bc_textures_enabled()) {
                Ok(resident) => {
                    let outcome = resident.load_outcome();
                    (resident, outcome, frame, None)
                }
                Err(error) => {
                    if let TextureAssetError::Vtf(vtf) = &error
                        && super::native_invalid_texture(vtf)
                    {
                        return Err(CubemapFailure::InvalidVtf(vtf.clone()));
                    }
                    let outcome = match &error {
                        TextureAssetError::DimensionLimit(_)
                        | TextureAssetError::WorkingLimit { .. }
                        | TextureAssetError::ImageCount(_)
                        | TextureAssetError::Vtf(VtfError::DecodedTooLarge { .. }) => {
                            TextureLoadOutcome::QualityRejected
                        }
                        _ => TextureLoadOutcome::Unsupported,
                    };
                    let fallback =
                        ResolvedCubemap::unavailable(Some(Arc::clone(&source_metadata)), outcome);
                    (
                        fallback.resident,
                        outcome,
                        0,
                        Some(CubemapFailure::Texture(error)),
                    )
                }
            };
        Ok(ResolvedCubemap {
            resident,
            source_metadata: Some(source_metadata),
            load_outcome,
            frame,
            resolution: CubemapResolution::Authored,
            effective_path: Some(normalized),
            failure,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::fixture_vtf_bytes;

    pub(super) fn fixture(faces: u8, frames: u16, mip_count: u8) -> Vec<u8> {
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
        bytes.resize(80, 0);
        bytes[8..12].copy_from_slice(&(if faces == 7 { 2_u32 } else { 5_u32 }).to_le_bytes());
        bytes[12..16].copy_from_slice(&80_u32.to_le_bytes());
        bytes[63..65].copy_from_slice(&1_u16.to_le_bytes());
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
    fn legacy_sentinel_keeps_the_stored_sphere_out_of_the_resident_cube() {
        let mut bytes = fixture(7, 2, 3);
        bytes[26..28].copy_from_slice(&u16::MAX.to_le_bytes());
        let cube = decode_cubemap(&bytes, 1, None, &Limits::default()).unwrap();
        let metadata = cube.source_metadata().unwrap();
        assert_eq!(metadata.face_count, 7);
        assert_eq!(metadata.first_frame, u16::MAX);
        let resident = cube.resident().unwrap();
        assert_eq!(resident.layers().len(), 12);
        assert_eq!(resident.byte_len(), 1008);
        assert!(resident.layer_index(1, 6).is_none());
        assert_eq!(
            cube.face(5).unwrap()[0].bytes.as_slice()[..4],
            [100, 70, 0, 42]
        );
        let mut truncated = fixture(6, 1, 3);
        truncated[8..12].copy_from_slice(&2_u32.to_le_bytes());
        assert!(matches!(
            decode_cubemap(&truncated, 0, None, &Limits::default()),
            Err(CubemapDecodeError::Texture(TextureAssetError::Vtf(
                VtfError::Truncated { .. }
            )))
        ));
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
        // A floating-point RGBA image uses eight bytes per texel. Keep this
        // fixture structurally valid so it exercises HDR policy, not truncation.
        let header = u32::from_le_bytes(hdr[12..16].try_into().unwrap()) as usize;
        hdr.resize(header + (hdr.len() - header) * 2, 0);
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
        let image = vformats::vtf::parse(&other_bytes, &Limits::default())
            .unwrap()
            .stored_image(0, 0, 2)
            .unwrap();
        other_bytes[image.byte_range.start..image.byte_range.start + 4].copy_from_slice(&[199; 4]);
        let other = MaterialResolver::prop_lighting_fixture(vec![(path, other_bytes)], Vec::new());
        let a = first.resolve_cubemap("maps/shared/c0_0_0", 0).unwrap();
        assert!(Arc::ptr_eq(
            a.on_demand().unwrap().0,
            first
                .resolve_cubemap("maps/shared/c0_0_0", 0)
                .unwrap()
                .on_demand()
                .unwrap()
                .0
        ));
        let b = other.resolve_cubemap("maps/shared/c0_0_0", 0).unwrap();
        assert_ne!(a.face(0).unwrap()[2], b.face(0).unwrap()[2]);
        assert!(Arc::ptr_eq(
            a.on_demand().unwrap().0,
            first
                .resolve_cubemap("maps/shared/c0_0_0", 1)
                .unwrap()
                .on_demand()
                .unwrap()
                .0
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
}

#[cfg(test)]
#[path = "cubemap/named_tests.rs"]
mod named_tests;
