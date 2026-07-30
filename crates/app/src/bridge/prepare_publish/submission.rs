//! Publish submission state, envelopes, and request construction.

use super::{
    DEFAULT_WORKSHOP_ICON_FILE_NAME, Generation, Path, PathBuf, PublishSelectedPreview,
    PublishSubmitOutcome, PublishSubmitRequest, PublishedFileId, TransactionStatus, VerifiedIcon,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublishSubmitContext {
    pub(crate) ignore_globs: Vec<String>,
    pub(crate) temp_dir: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublishSubmitRequestEnvelope {
    pub(crate) generation: Generation,
    pub(crate) request: PublishSubmitRequest,
}

impl PublishSubmitRequestEnvelope {
    pub(crate) const fn initial_status(&self) -> TransactionStatus {
        self.request.initial_status()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PublishSubmitResult {
    pub(crate) published_file_id: PublishedFileId,
    pub(crate) legal_agreement_required: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublishIconSubmitRequestEnvelope {
    pub(crate) generation: Generation,
    pub(crate) icon_source_path: PathBuf,
    pub(crate) upscale: bool,
    pub(crate) workshop_id: PublishedFileId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PublishIconSubmitResult {
    pub(crate) legal_agreement_required: bool,
}

pub fn default_icon_path(temp_dir: &Path) -> PathBuf {
    temp_dir.join(DEFAULT_WORKSHOP_ICON_FILE_NAME)
}

pub fn publish_selected_preview(icon: &VerifiedIcon, upscale_icon: bool) -> PublishSelectedPreview {
    PublishSelectedPreview {
        path: icon.path.clone(),
        upscale: upscale_icon && icon.can_upscale,
    }
}

impl From<PublishSubmitOutcome> for PublishSubmitResult {
    fn from(outcome: PublishSubmitOutcome) -> Self {
        Self {
            published_file_id: outcome.published_file_id,
            legal_agreement_required: outcome.legal_agreement_required,
        }
    }
}
