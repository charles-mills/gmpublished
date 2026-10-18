#[cfg(test)]
use super::{ResidentTextureMip, ResolvedTextureMip, TextureAssetError};

use super::{
    Arc, MaterialResolver, ResidentTexture, TextureLoadOutcome, TextureMetadata, texture_path,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecodedTextureFrames {
    resident: super::TextureResidency,
    source_metadata: Option<Arc<TextureMetadata>>,
    load_outcome: TextureLoadOutcome,
}

impl DecodedTextureFrames {
    #[cfg(test)]
    pub(crate) fn read_for_test(&self) -> &Arc<ResidentTexture> {
        self.resident.read_for_test()
    }

    #[cfg(test)]
    pub(crate) fn from_rgba(
        flags: u32,
        frames: Vec<Vec<ResolvedTextureMip>>,
    ) -> Result<Self, TextureAssetError> {
        let resident =
            super::texture_asset::generated_rgba("material-data-fallback", Some(flags), frames)?;
        let source_metadata = resident.metadata_arc();
        Ok(Self {
            resident: super::TextureResidency::Resident(resident),
            source_metadata: Some(source_metadata),
            load_outcome: TextureLoadOutcome::Generated,
        })
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
    pub(crate) fn flags(&self) -> u32 {
        self.source_metadata()
            .and_then(|metadata| metadata.flags)
            .unwrap_or(0)
    }
    pub(crate) fn frame_count(&self) -> usize {
        if self.on_demand().is_some() {
            self.source_metadata()
                .map_or(0, |metadata| metadata.frame_count as usize)
        } else {
            (self.resident.id().frames.end - self.resident.id().frames.start) as usize
        }
    }
    #[cfg(test)]
    pub(crate) fn frame(&self, index: usize) -> Option<&[ResidentTextureMip]> {
        let frame = self
            .resident
            .id()
            .frames
            .start
            .checked_add(u32::try_from(index).ok()?)?;
        Some(&self.read_for_test().layer(frame, 0)?.mips)
    }
    pub(crate) fn source_metadata(&self) -> Option<&TextureMetadata> {
        self.source_metadata.as_deref()
    }
    pub(crate) fn load_outcome(&self) -> TextureLoadOutcome {
        self.load_outcome
    }
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
        let source =
            self.resolve_on_demand_texture(&path, super::TextureFailurePolicy::Unverified)?;
        Some(Arc::new(DecodedTextureFrames {
            resident: source.resident.clone(),
            source_metadata: source.source_metadata.clone(),
            load_outcome: source.load_outcome,
        }))
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
