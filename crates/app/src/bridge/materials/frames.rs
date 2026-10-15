use super::{Arc, ContentPath, Limits, MaterialResolver, ResolvedTextureMip, texture_path};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecodedTextureFrames {
    pub flags: u32,
    /// Unconverted RGBA bytes and stored mip chains, one layer per animation frame.
    pub frames: Vec<Vec<ResolvedTextureMip>>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(super) struct DecodedFramesKey {
    path: ContentPath,
    max_dimension: Option<u32>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum TextureReadTransfer {
    Unmodified,
    SrgbToLinear,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TextureRole {
    Base {
        family: super::SurfaceFamily,
    },
    Normal,
    Detail {
        family: super::SurfaceFamily,
        mode: Option<u32>,
    },
    BlendModulation,
    EmissionMask,
    EnvironmentMask,
    PhongExponent,
    PhongWarp,
}

impl TextureRole {
    pub const fn read_transfer(self) -> TextureReadTransfer {
        let srgb = match self {
            Self::Base {
                family: super::SurfaceFamily::DecalModulate,
            } => false,
            Self::Base { .. } => true,
            Self::Detail {
                family: super::SurfaceFamily::VertexLit | super::SurfaceFamily::Unlit,
                mode: Some(mode),
            } => mode != 0,
            Self::Detail { mode: Some(1), .. } => true,
            _ => false,
        };
        if srgb {
            TextureReadTransfer::SrgbToLinear
        } else {
            TextureReadTransfer::Unmodified
        }
    }
}

impl MaterialResolver {
    pub(super) fn resolve_raw_texture_frames(
        &self,
        name: &str,
    ) -> Option<Arc<DecodedTextureFrames>> {
        let path = texture_path(name)?;
        let key = DecodedFramesKey {
            path: path.clone(),
            max_dimension: self.config.decoded_texture_max_dimension,
        };
        if let Some(decoded) = self.shared.frame_texture_cache.lock().get(&key).cloned() {
            return Some(decoded);
        }
        self.find_entry_by_source(std::slice::from_ref(&path), |_, bytes| {
            let limits = Limits::default();
            let vtf = vformats::vtf::parse(&bytes, &limits).ok()?;
            if vtf.frame_count() == 0 || vtf.frame_count() > 128 {
                log::warn!("decoded texture {name}: invalid or excessive animation frame count");
                return None;
            }
            let max = self
                .config
                .decoded_texture_max_dimension
                .unwrap_or(u32::MAX)
                .max(1);
            let mut first_mip = 0;
            while (vtf.width() >> first_mip).max(vtf.height() >> first_mip) > max
                && first_mip + 1 < vtf.mip_count()
            {
                first_mip += 1;
            }
            if (vtf.width() >> first_mip).max(vtf.height() >> first_mip) > max {
                log::warn!(
                    "decoded texture {name}: no stored mip satisfies the texture dimension limit"
                );
                return None;
            }
            let estimate = (first_mip..vtf.mip_count())
                .map(|mip| {
                    u64::from((vtf.width() >> mip).max(1))
                        * u64::from((vtf.height() >> mip).max(1))
                        * 4
                        * u64::from(vtf.frame_count())
                })
                .sum::<u64>();
            if estimate > 64 * 1024 * 1024 {
                log::warn!("decoded texture {name}: decoded animation exceeds 64 MiB");
                return None;
            }
            let mut frames = Vec::new();
            let mut byte_len = 0_usize;
            for frame in 0..vtf.frame_count() {
                let mut mips = Vec::new();
                for mip in first_mip..vtf.mip_count() {
                    let image = vtf.decode_rgba(frame, 0, mip, &limits).ok()?;
                    byte_len = byte_len.checked_add(image.rgba.len())?;
                    mips.push(ResolvedTextureMip {
                        rgba: image.rgba,
                        width: image.width,
                        height: image.height,
                    });
                }
                frames.push(mips);
            }
            let mut cache = self.shared.frame_texture_cache.lock();
            if let Some(cached) = cache.get(&key) {
                return Some(Arc::clone(cached));
            }
            if let Some(budget) = &self.config.decoded_texture_budget
                && !budget.try_reserve(byte_len)
            {
                log::warn!("decoded texture {name}: texture budget exhausted");
                return None;
            }
            let decoded = Arc::new(DecodedTextureFrames {
                flags: vtf.flags(),
                frames,
            });
            cache.insert(key.clone(), Arc::clone(&decoded));
            drop(cache);
            Some(decoded)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::materials::SurfaceFamily;

    #[test]
    fn detail_transfer_depends_on_effective_shader_family_and_mode() {
        for family in [
            SurfaceFamily::Lightmapped,
            SurfaceFamily::Transition,
            SurfaceFamily::VertexLit,
            SurfaceFamily::Unlit,
        ] {
            for mode in [None, Some(0), Some(1), Some(2), Some(9), Some(10), Some(11)] {
                let model = matches!(family, SurfaceFamily::VertexLit | SurfaceFamily::Unlit);
                let expected = if mode.is_some_and(|mode| if model { mode != 0 } else { mode == 1 })
                {
                    TextureReadTransfer::SrgbToLinear
                } else {
                    TextureReadTransfer::Unmodified
                };
                assert_eq!(
                    TextureRole::Detail { family, mode }.read_transfer(),
                    expected
                );
            }
        }
    }

    #[test]
    fn data_roles_preserve_authored_channel_bytes() {
        for role in [
            TextureRole::Normal,
            TextureRole::BlendModulation,
            TextureRole::EmissionMask,
            TextureRole::EnvironmentMask,
            TextureRole::PhongExponent,
            TextureRole::PhongWarp,
        ] {
            assert_eq!(role.read_transfer(), TextureReadTransfer::Unmodified);
        }
    }
}
