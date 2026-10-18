use crate::bridge::materials::{MaterialResolver, SpriteSheetBudget, TextureWorkingBudget};
use std::sync::{Arc, OnceLock};

const DEFAULT_BYTES: usize = 1024 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct TextureResourcePolicy {
    pub source_discovery_limits: crate::bridge::materials::SourceDiscoveryLimits,
    pub archive_index_limits: vformats::ArchiveIndexLimits,
    pub archive_index_working_budget: Arc<TextureWorkingBudget>,
    pub resource_resident_bytes: usize,
    pub source_decoder_bytes: usize,
    pub sprite_sheet_frames: usize,
    pub sprite_sheet_metadata_bytes: usize,
    pub working_budget: Arc<TextureWorkingBudget>,
    pub source_working_budget: Arc<TextureWorkingBudget>,
}

impl Default for TextureResourcePolicy {
    fn default() -> Self {
        static PAYLOAD: OnceLock<Arc<TextureWorkingBudget>> = OnceLock::new();
        static SOURCE: OnceLock<Arc<TextureWorkingBudget>> = OnceLock::new();
        Self {
            source_discovery_limits: Default::default(),
            archive_index_limits: vformats::ArchiveIndexLimits::default(),
            archive_index_working_budget: MaterialResolver::default_archive_index_working_budget(),
            resource_resident_bytes: DEFAULT_BYTES,
            source_decoder_bytes: DEFAULT_BYTES,
            sprite_sheet_frames: crate::bridge::materials::DEFAULT_SPRITE_SHEET_FRAMES,
            sprite_sheet_metadata_bytes:
                crate::bridge::materials::DEFAULT_SPRITE_SHEET_METADATA_BYTES,
            working_budget: Arc::clone(
                PAYLOAD.get_or_init(|| Arc::new(TextureWorkingBudget::new(DEFAULT_BYTES))),
            ),
            source_working_budget: Arc::clone(
                SOURCE.get_or_init(|| Arc::new(TextureWorkingBudget::new(DEFAULT_BYTES))),
            ),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct MapTextureResidency {
    pub max_dimension: Option<u32>,
    pub resources: TextureResourcePolicy,
}

pub(super) struct TexturePreparationResources {
    pub resolver: MaterialResolver,
    pub sheet_budget: Arc<SpriteSheetBudget>,
}

impl TexturePreparationResources {
    pub fn for_archive_cancellable(
        archive: Arc<crate::bridge::archive::PreviewArchiveSource>,
        gmod_dir: Option<std::path::PathBuf>,
        policy: &TextureResourcePolicy,
        cancellation: crate::preparation::PreparationCancellation,
    ) -> Result<Self, crate::preparation::Cancelled> {
        let resolver = MaterialResolver::new_with_source_policy_cancellable(
            archive,
            gmod_dir,
            policy.archive_index_limits,
            Arc::clone(&policy.archive_index_working_budget),
            policy.source_discovery_limits,
            cancellation,
        )?;
        Ok(Self::new(&resolver, policy))
    }

    pub fn for_map_cancellable(
        archive: Arc<crate::bridge::archive::PreviewArchiveSource>,
        gmod_dir: Option<std::path::PathBuf>,
        pakfile: gmpublished_domain::scene::map::MapPakFile,
        policy: &TextureResourcePolicy,
        cancellation: crate::preparation::PreparationCancellation,
    ) -> Result<Self, crate::preparation::Cancelled> {
        let resolver = MaterialResolver::with_pakfile_source_policy_cancellable(
            archive,
            gmod_dir,
            pakfile,
            policy.archive_index_limits,
            Arc::clone(&policy.archive_index_working_budget),
            policy.source_discovery_limits,
            cancellation,
        )?;
        Ok(Self::new(&resolver, policy))
    }

    pub fn new(resolver: &MaterialResolver, policy: &TextureResourcePolicy) -> Self {
        let sheet_budget = Arc::new(SpriteSheetBudget::new(
            policy.sprite_sheet_frames,
            policy.sprite_sheet_metadata_bytes,
        ));
        let resolver = resolver
            .with_texture_resource_limit(policy.resource_resident_bytes)
            .with_texture_decoder_limit(policy.source_decoder_bytes)
            .with_texture_source_working_budget(Arc::clone(&policy.source_working_budget))
            .with_texture_working_budget(Arc::clone(&policy.working_budget))
            .with_sprite_sheet_budget(Arc::clone(&sheet_budget));
        Self {
            resolver,
            sheet_budget,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_previews_share_separate_source_and_payload_working_gates() {
        let first = TextureResourcePolicy::default();
        let second = TextureResourcePolicy::default();
        assert!(Arc::ptr_eq(&first.working_budget, &second.working_budget));
        assert!(Arc::ptr_eq(
            &first.source_working_budget,
            &second.source_working_budget
        ));
        assert!(Arc::ptr_eq(
            &first.archive_index_working_budget,
            &second.archive_index_working_budget
        ));
        assert!(!Arc::ptr_eq(
            &first.working_budget,
            &first.source_working_budget
        ));
        assert_eq!(first.working_budget.capacity(), DEFAULT_BYTES);
        assert_eq!(first.source_working_budget.capacity(), DEFAULT_BYTES);
        assert!(!Arc::ptr_eq(
            &first.archive_index_working_budget,
            &first.source_working_budget
        ));
        assert!(!Arc::ptr_eq(
            &first.archive_index_working_budget,
            &first.working_budget
        ));
        assert_eq!(first.archive_index_working_budget.capacity(), DEFAULT_BYTES);
    }

    #[test]
    fn scene_admission_is_independent_from_shared_working_capacity() {
        let policy = TextureResourcePolicy::default();
        let first = TexturePreparationResources::new(
            &MaterialResolver::prop_lighting_fixture(Vec::new(), Vec::new()),
            &policy,
        );
        let second = TexturePreparationResources::new(
            &MaterialResolver::prop_lighting_fixture(Vec::new(), Vec::new()),
            &policy,
        );
        assert!(!Arc::ptr_eq(&first.sheet_budget, &second.sheet_budget));
    }
}
