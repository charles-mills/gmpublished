//! Workshop metadata resolution and patches for search rows.

use super::*;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MetadataCompletion {
    pub(crate) changed: bool,
    pub(crate) stale_ids: Vec<PublishedFileId>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MetadataResolution {
    pub(crate) patches: Vec<MetadataPatch>,
    pub(crate) stale_ids: Vec<PublishedFileId>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MetadataPatch {
    pub(super) workshop_id: PublishedFileId,
    pub(super) preview_url: Option<String>,
}

impl MetadataPatch {
    fn from_metadata(metadata: &WorkshopMetadata) -> Self {
        Self {
            workshop_id: metadata.id,
            preview_url: metadata
                .preview_url
                .as_deref()
                .map(str::trim)
                .filter(|url| !url.is_empty())
                .map(str::to_owned),
        }
    }

    #[cfg(test)]
    pub(crate) fn for_test(workshop_id: PublishedFileId, preview_url: Option<&str>) -> Self {
        Self {
            workshop_id,
            preview_url: preview_url.map(str::to_owned),
        }
    }
}

pub fn resolve_metadata(
    metadata: &[WorkshopMetadata],
    stale_ids: Vec<PublishedFileId>,
) -> MetadataResolution {
    MetadataResolution {
        patches: metadata.iter().map(MetadataPatch::from_metadata).collect(),
        stale_ids,
    }
}

pub fn refresh_metadata(metadata: &[WorkshopMetadata]) -> Vec<MetadataPatch> {
    metadata.iter().map(MetadataPatch::from_metadata).collect()
}
