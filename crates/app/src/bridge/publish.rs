use gmpublished_backend::error_key::keys;

use crate::bridge::ui_error::UiError;
use std::path::{Path, PathBuf};

use super::domain::PublishedFileId;

pub const PUBLISH_PROCESSING_ICON: &str = "PUBLISH_PROCESSING_ICON";
pub const PUBLISH_PACKING: &str = "PUBLISH_PACKING";
pub const DEFAULT_WORKSHOP_ICON_FILE_NAME: &str = "gmpublished_default_icon.png";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IconFormat {
    Png,
    Jpeg,
    Gif,
}

impl TryFrom<&Path> for IconFormat {
    type Error = UiError;

    fn try_from(path: &Path) -> Result<Self, Self::Error> {
        match path
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str()
        {
            "png" => Ok(Self::Png),
            "jpg" | "jpeg" => Ok(Self::Jpeg),
            "gif" => Ok(Self::Gif),
            _ => Err(UiError::new(keys::ICON_INVALID_FORMAT)),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PublishSubmitMode {
    New,
    Update { workshop_id: PublishedFileId },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PublishSelectedPreview {
    Source { path: PathBuf, upscale: bool },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PublishSubmitPreview {
    Default(PathBuf),
    Selected(PublishSelectedPreview),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublishSubmitRequest {
    pub(crate) mode: PublishSubmitMode,
    pub(crate) content_source_path: PathBuf,
    pub(crate) title: String,
    pub(crate) addon_type: String,
    pub(crate) tags: Vec<String>,
    pub(crate) changelog: Option<String>,
    pub(crate) preview: Option<PublishSubmitPreview>,
    pub(crate) ignore_globs: Vec<String>,
    pub(crate) total_size: u64,
    pub(crate) temp_dir: PathBuf,
}

impl PublishSubmitRequest {
    pub(crate) const fn initial_status(&self) -> &'static str {
        match &self.preview {
            Some(PublishSubmitPreview::Selected(_)) => PUBLISH_PROCESSING_ICON,
            Some(PublishSubmitPreview::Default(_)) | None => PUBLISH_PACKING,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PublishSubmitOutcome {
    pub(crate) published_file_id: PublishedFileId,
    pub(crate) legal_agreement_required: bool,
}
