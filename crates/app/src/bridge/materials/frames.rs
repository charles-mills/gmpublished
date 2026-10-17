#[cfg(any(test, feature = "map-capture"))]
use super::ResidentTextureMip;
#[cfg(test)]
use super::ResolvedTextureMip;

use super::{
    Arc, MaterialResolver, ResidentTexture, TextureAssetError, TextureLoadOutcome, TextureMetadata,
    texture_path,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecodedTextureFrames {
    resident: super::TextureResidency,
    source_metadata: Option<Arc<TextureMetadata>>,
    load_outcome: TextureLoadOutcome,
}

impl DecodedTextureFrames {
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
    fn unavailable(metadata: Option<Arc<TextureMetadata>>, outcome: TextureLoadOutcome) -> Self {
        let placeholder = super::ResolvedTexture::unavailable(metadata.clone(), outcome);
        Self {
            resident: placeholder.resident,
            source_metadata: metadata,
            load_outcome: outcome,
        }
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
        (self.resident.id().frames.end - self.resident.id().frames.start) as usize
    }
    #[cfg(any(test, feature = "map-capture"))]
    pub(crate) fn frame(&self, index: usize) -> Option<&[ResidentTextureMip]> {
        let frame = self
            .resident
            .id()
            .frames
            .start
            .checked_add(u32::try_from(index).ok()?)?;
        Some(&self.resident()?.layer(frame, 0)?.mips)
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
        let selected = match self.selected_texture_asset(&path) {
            Ok(selected) => selected,
            Err(super::TextureSourceError::SourceUnavailable(_)) => {
                return Some(Arc::new(DecodedTextureFrames::unavailable(
                    None,
                    TextureLoadOutcome::SourceUnavailable,
                )));
            }
            Err(super::TextureSourceError::Preparation { .. }) => {
                return Some(Arc::new(DecodedTextureFrames::unavailable(
                    None,
                    TextureLoadOutcome::Unplanned,
                )));
            }
            Err(
                super::TextureSourceError::EncodedBudget { metadata, .. }
                | super::TextureSourceError::WorkingBudget { metadata, .. },
            ) => {
                return Some(Arc::new(DecodedTextureFrames::unavailable(
                    metadata,
                    TextureLoadOutcome::SourceBudgetRejected,
                )));
            }
            Err(super::TextureSourceError::Missing) => return None,
            Err(error) => {
                log::warn!("material data texture {path}: {error}");
                return None;
            }
        };
        let source_metadata = selected.asset.metadata_arc();
        let (resident, load_outcome) = match self.resident_texture(
            &selected,
            0..1,
            !self.bc_textures_enabled(),
        ) {
            Ok(resident) => {
                let outcome = resident.load_outcome();
                (resident, outcome)
            }
            Err(error) => {
                log::warn!("material data texture {path}: {error}");
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
                    | TextureAssetError::Vtf(vformats::vtf::VtfError::DecodedTooLarge { .. }) => {
                        TextureLoadOutcome::QualityRejected
                    }
                    TextureAssetError::UnplannedRequest | TextureAssetError::ClosedPlan => {
                        TextureLoadOutcome::Unplanned
                    }
                    _ => TextureLoadOutcome::Unsupported,
                };
                let placeholder = super::ResolvedTexture::unavailable(
                    Some(Arc::clone(&source_metadata)),
                    outcome,
                );
                (placeholder.resident, outcome)
            }
        };
        Some(Arc::new(DecodedTextureFrames {
            resident,
            source_metadata: Some(source_metadata),
            load_outcome,
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
