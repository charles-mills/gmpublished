use super::parameters::{scalar, texture_transform};
use super::{
    Arc, ContentPath, DecodedTextureFrames, EffectiveMaterial, Limits, MaterialResolver,
    ResolvedTextureMip, normalize_texture_name, parse_material_color, texture_path, vmt_bool,
};
use vformats::vtf::{Vtf, VtfError, VtfFormat, texture_flags};

const MAX_DECODED_CUBEMAP_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedCubemap {
    /// Native VTF face order; each face has largest-first stored mip levels.
    pub faces: [Vec<ResolvedTextureMip>; 6],
    pub flags: u32,
    pub frame: u32,
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

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(super) struct CubemapCacheKey {
    path: ContentPath,
    frame: u32,
    max_dimension: Option<u32>,
}

#[derive(Debug, thiserror::Error)]
enum CubemapDecodeError {
    #[error(transparent)]
    Vtf(#[from] VtfError),
    #[error("expected a square six-face cubemap")]
    NotCubemap,
    #[error("HDR cubemaps require a floating-point scene mode")]
    UnsupportedHdr,
    #[error("no stored cubemap mip satisfies the dimension limit")]
    DimensionLimit,
    #[error("decoded cubemap size {size} exceeds {max} bytes")]
    DecodedTooLarge { size: u64, max: u64 },
}

fn mip_dimension(base: u32, level: u8) -> u32 {
    base.checked_shr(u32::from(level)).unwrap_or(0).max(1)
}

fn decode_cubemap(
    bytes: &[u8],
    frame: u32,
    max_dimension: Option<u32>,
    limits: &Limits,
) -> Result<ResolvedCubemap, CubemapDecodeError> {
    let vtf = vformats::vtf::parse(bytes, limits)?;
    let (first_mip, _) = cubemap_plan(&vtf, frame, max_dimension, limits)?;
    let mut faces: [Vec<ResolvedTextureMip>; 6] = std::array::from_fn(|_| Vec::new());
    for (face, mips) in faces.iter_mut().enumerate() {
        for mip in first_mip..vtf.mip_count() {
            let image = vtf.decode_rgba(frame, face as u32, mip, limits)?;
            mips.push(ResolvedTextureMip {
                rgba: image.rgba,
                width: image.width,
                height: image.height,
            });
        }
    }
    Ok(ResolvedCubemap {
        faces,
        flags: vtf.flags(),
        frame,
    })
}

fn cubemap_plan(
    vtf: &Vtf<'_>,
    frame: u32,
    max_dimension: Option<u32>,
    limits: &Limits,
) -> Result<(u8, usize), CubemapDecodeError> {
    if vtf.flags() & texture_flags::ENVMAP == 0
        || vtf.face_count() < 6
        || vtf.width() != vtf.height()
    {
        return Err(CubemapDecodeError::NotCubemap);
    }
    if matches!(
        vtf.format(),
        VtfFormat::Rgba16161616F | VtfFormat::Rgba16161616
    ) {
        return Err(CubemapDecodeError::UnsupportedHdr);
    }
    if frame >= vtf.frame_count() {
        return Err(VtfError::OutOfRange.into());
    }
    if u32::from(vtf.mip_count()) > u32::BITS - vtf.width().leading_zeros() {
        return Err(VtfError::BadHeader.into());
    }
    let max = max_dimension.unwrap_or(u32::MAX).max(1);
    let first_mip = (0..vtf.mip_count())
        .find(|&mip| mip_dimension(vtf.width(), mip) <= max)
        .ok_or(CubemapDecodeError::DimensionLimit)?;
    let estimate = (first_mip..vtf.mip_count()).try_fold(0_u64, |sum, mip| {
        let side = u64::from(mip_dimension(vtf.width(), mip));
        side.checked_mul(side)?.checked_mul(4 * 6)?.checked_add(sum)
    });
    let max_bytes = MAX_DECODED_CUBEMAP_BYTES.min(limits.max_entry_bytes);
    let estimate = estimate.unwrap_or(u64::MAX);
    if estimate > max_bytes {
        return Err(CubemapDecodeError::DecodedTooLarge {
            size: estimate,
            max: max_bytes,
        });
    }
    Ok((first_mip, estimate as usize))
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
            .parameter("$envmapmask")
            .and_then(normalize_texture_name)
            .and_then(|name| self.resolve_raw_texture_frames(&name));
        let mask = if vmt_bool(material.parameter("$normalmapalphaenvmapmask")) {
            EnvironmentMapMask::NormalAlpha
        } else if vmt_bool(material.parameter("$basealphaenvmapmask")) {
            EnvironmentMapMask::BaseAlpha
        } else if material.parameter("$envmapmask").is_some() {
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
        let key = CubemapCacheKey {
            path: path.clone(),
            frame,
            max_dimension: self.config.decoded_texture_max_dimension,
        };
        if let Some(cached) = self.shared.cubemap_cache.lock().get(&key).cloned() {
            return Some(cached);
        }
        // The first mounted source owns this name. An invalid packed map cube
        // must not silently fall through to another map/addon's same-name file.
        let bytes = self.entry_bytes_from_sources(&path)?;
        let limits = Limits::default();
        let plan = vformats::vtf::parse(&bytes, &limits)
            .map_err(CubemapDecodeError::from)
            .and_then(|vtf| cubemap_plan(&vtf, frame, key.max_dimension, &limits));
        let (_, byte_len) = match plan {
            Ok(plan) => plan,
            Err(error) => {
                log::warn!("cubemap {name}: {error}");
                return None;
            }
        };
        let mut cache = self.shared.cubemap_cache.lock();
        if let Some(cached) = cache.get(&key) {
            return Some(Arc::clone(cached));
        }
        if let Some(budget) = &self.config.decoded_texture_budget
            && !budget.try_reserve(byte_len)
        {
            log::warn!("cubemap {name}: texture budget exhausted");
            return None;
        }
        let cube = match decode_cubemap(&bytes, frame, key.max_dimension, &limits) {
            Ok(cube) => cube,
            Err(error) => {
                if let Some(budget) = &self.config.decoded_texture_budget {
                    budget
                        .decoded_bytes
                        .fetch_sub(byte_len, std::sync::atomic::Ordering::AcqRel);
                }
                log::warn!("cubemap {name}: {error}");
                return None;
            }
        };
        let cube = Arc::new(cube);
        cache.insert(key, Arc::clone(&cube));
        drop(cache);
        Some(cube)
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
            assert_eq!(cube.flags, texture_flags::ENVMAP);
            for (face, mips) in cube.faces.iter().enumerate() {
                assert_eq!(mips.len(), 3);
                for (mip, image) in mips.iter().enumerate() {
                    assert_eq!((image.width, image.height), (4 >> mip, 4 >> mip));
                    assert_eq!(
                        image.rgba[..4],
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
        assert_eq!(cube.faces[0][0].width, 2);
        assert_eq!(cube.faces[0].len(), 2);
        assert!(matches!(
            decode_cubemap(&bytes, 1, None, &Limits::default()),
            Err(CubemapDecodeError::Vtf(VtfError::OutOfRange))
        ));
        assert!(decode_cubemap(&bytes[..bytes.len() - 1], 0, None, &Limits::default()).is_err());
        let small = Limits {
            max_entry_bytes: 8,
            ..Limits::default()
        };
        assert!(matches!(
            decode_cubemap(&bytes, 0, None, &small),
            Err(CubemapDecodeError::DecodedTooLarge { .. })
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
            &a,
            &first.resolve_cubemap("maps/shared/c0_0_0", 0).unwrap()
        ));
        let b = other.resolve_cubemap("maps/shared/c0_0_0", 0).unwrap();
        assert_ne!(a.faces[0][2], b.faces[0][2]);
        assert!(!Arc::ptr_eq(
            &a,
            &first.resolve_cubemap("maps/shared/c0_0_0", 1).unwrap()
        ));
        let capped = first
            .with_decoded_texture_max_dimension(2)
            .resolve_cubemap("maps/shared/c0_0_0", 0)
            .unwrap();
        assert_eq!(capped.faces[0][0].width, 2);
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
    fn cubemap_budget_reserves_before_decode_and_rolls_back_failed_streams() {
        let path = "materials/maps/shared/c0_0_0.vtf";
        let budget = Arc::new(super::super::DecodedTextureBudget::new(504));
        let resolver =
            MaterialResolver::prop_lighting_fixture(vec![(path, fixture(6, 2, 3))], Vec::new())
                .with_decoded_texture_budget(Arc::clone(&budget));
        assert!(resolver.resolve_cubemap("maps/shared/c0_0_0", 0).is_some());
        assert_eq!(budget.decoded_bytes(), 504);
        assert!(resolver.resolve_cubemap("maps/shared/c0_0_0", 0).is_some());
        assert_eq!(budget.decoded_bytes(), 504);
        assert!(resolver.resolve_cubemap("maps/shared/c0_0_0", 1).is_none());
        assert_eq!(budget.rejected_textures(), 1);
        let mut malformed = fixture(6, 1, 3);
        malformed.pop();
        let budget = Arc::new(super::super::DecodedTextureBudget::new(504));
        let resolver = MaterialResolver::prop_lighting_fixture(vec![(path, malformed)], Vec::new())
            .with_decoded_texture_budget(Arc::clone(&budget));
        assert!(resolver.resolve_cubemap("maps/shared/c0_0_0", 0).is_none());
        assert_eq!(budget.decoded_bytes(), 0);
    }
}
