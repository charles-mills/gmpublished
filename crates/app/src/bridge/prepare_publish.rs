//! Bridge-owned Prepare Publish verification and operational DTOs.
//!
//! The feature state consumes these values but performs no filesystem, image,
//! settings, transaction, or backend work itself. Runners invoke this module
//! on the shared worker runtime and feed the resulting DTOs back to update.

use crate::bridge::tasks::TransactionStatus;
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use crate::bridge::ui_error::UiError;
use crate::bridge::{
    Settings,
    domain::PublishedFileId,
    publish::{
        DEFAULT_WORKSHOP_ICON_FILE_NAME, IconFormat, PublishSelectedPreview, PublishSubmitOutcome,
        PublishSubmitRequest,
    },
    tasks::{ArchiveService, ConfigService, WorkshopSnapshotId},
};
use gmpublished_backend::error_keys as keys;
use gmpublished_backend::{ErrorKey, HasErrorKey};
use iced::widget::image;
use thiserror::Error;

use crate::generation::Generation;
use crate::{
    bridge::gma::{ArchiveEntryPath, GmaMetaEntry, whitelist},
    media::{thumbnail_animation, thumbnail_worker::PreparedAnimation},
    widgets::file_browser,
};

mod content_verification;
mod icon_verification;
mod settings;
mod submission;

#[cfg(test)]
use content_verification::relative_slash_path;
pub use content_verification::{inspect_workshop_snapshot, verify_content_path};
pub use icon_verification::verify_icon_preview;
pub use settings::{apply_ignore_pattern_mutation, ignored_patterns_from_settings};
pub use submission::{
    PublishIconSubmitRequestEnvelope, PublishIconSubmitResult, PublishSubmitContext,
    PublishSubmitRequestEnvelope, PublishSubmitResult, default_icon_path, publish_selected_preview,
};
const WORKSHOP_ICON_MAX_SIZE: u64 = 1_048_576;
const WORKSHOP_ICON_MIN_SIZE: u64 = 16;
const WORKSHOP_ICON_PREVIEW_MAX_EDGE: u32 = 512;
const MAX_WHITELIST_FAILURES: usize = 9;

#[derive(Clone, Copy, Debug)]
enum PathOperation {
    ReadDirectory,
    ReadDirectoryEntry,
    InspectMetadata,
    ReadFile,
}

impl std::fmt::Display for PathOperation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::ReadDirectory => "read directory",
            Self::ReadDirectoryEntry => "read directory entry",
            Self::InspectMetadata => "inspect metadata",
            Self::ReadFile => "read file",
        })
    }
}

/// Rich filesystem failures at the feature boundary. The full operation and
/// source chain are retained for logs; the UI receives the stable localized
/// key plus a path/source detail that does not bake English prose into the
/// translation payload.
#[derive(Debug, Error)]
enum PreparePublishPathError {
    #[error("could not {operation} at {}: {source}", path.display())]
    Io {
        operation: PathOperation,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("expected a regular file at {}", path.display())]
    NotRegularFile { path: PathBuf },
    #[error(
        "path {} escaped content root {}",
        path.display(),
        root.display()
    )]
    OutsideContentRoot { root: PathBuf, path: PathBuf },
}

impl PreparePublishPathError {
    fn io(operation: PathOperation, path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            operation,
            path: path.into(),
            source,
        }
    }

    fn into_ui(self) -> UiError {
        log::warn!("Prepare Publish filesystem operation failed: {self}");
        UiError::from(&self)
    }
}

impl HasErrorKey for PreparePublishPathError {
    fn error_key(&self) -> ErrorKey {
        keys::PATH_IO_ERROR
    }

    fn error_detail(&self) -> Option<String> {
        match self {
            Self::Io { path, source, .. } => Some(format!("{}: {source}", path.display())),
            Self::NotRegularFile { path } | Self::OutsideContentRoot { path, .. } => {
                Some(path.display().to_string())
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContentPathVerificationRequest {
    pub(crate) generation: Generation,
    pub(crate) display_path: String,
    pub(crate) path: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkshopContentRequest {
    pub(crate) request_id: WorkshopSnapshotId,
    pub(crate) workshop_id: PublishedFileId,
    pub(crate) destination: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedContentPath {
    display_path: String,
    path: PathBuf,
    total_size: u64,
    entries: Vec<file_browser::Entry>,
    preview_source: Arc<crate::bridge::archive::PreviewArchiveSource>,
}

impl VerifiedContentPath {
    pub(crate) fn display_path(&self) -> &str {
        &self.display_path
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) const fn total_size(&self) -> u64 {
        self.total_size
    }

    pub(crate) fn entries(&self) -> &[file_browser::Entry] {
        &self.entries
    }

    pub(crate) const fn preview_source(
        &self,
    ) -> &Arc<crate::bridge::archive::PreviewArchiveSource> {
        &self.preview_source
    }
}

/// An unfiltered inventory of content already published to the Workshop.
///
/// This is intentionally distinct from [`VerifiedContentPath`]: snapshot
/// entries are suitable for browsing and previewing, but have not passed the
/// current publish whitelist or ignore policy and therefore cannot be used as
/// a publish source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkshopSnapshotInventory {
    entries: Vec<file_browser::Entry>,
    preview_source: Arc<crate::bridge::archive::PreviewArchiveSource>,
}

impl WorkshopSnapshotInventory {
    pub(crate) fn entries(&self) -> &[file_browser::Entry] {
        &self.entries
    }

    pub(crate) const fn preview_source(
        &self,
    ) -> &Arc<crate::bridge::archive::PreviewArchiveSource> {
        &self.preview_source
    }
}

/// Minimal verified path state retained after the browser tree is built.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedContentPathState {
    pub(crate) display_path: String,
    pub(crate) path: PathBuf,
    pub(crate) total_size: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IconVerificationRequest {
    pub(crate) generation: Generation,
    pub(crate) display_path: String,
    pub(crate) path: PathBuf,
    pub(crate) well_rgb: [u8; 3],
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedIcon {
    pub(crate) display_path: String,
    pub(crate) source_path: PathBuf,
    pub(crate) path: PathBuf,
    pub(crate) format: IconFormat,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) byte_size: u64,
    pub(crate) can_upscale: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct VerifiedIconPreview {
    pub(crate) icon: VerifiedIcon,
    pub(crate) still: image::Handle,
    pub(crate) backdrop: image::Handle,
    pub(crate) animation: Option<thumbnail_animation::Playback>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IgnoredPattern {
    pub(crate) pattern: String,
    pub(crate) default_pattern: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum IgnorePatternMutation {
    Add(String),
    Remove(String),
}

impl IgnorePatternMutation {
    pub(crate) const fn worker_name(&self) -> &'static str {
        match self {
            Self::Add(_) => "prepare-publish-ignore-add",
            Self::Remove(_) => "prepare-publish-ignore-remove",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IgnorePatternMutationResult {
    pub(crate) changed: bool,
    pub(crate) ignored_patterns: Vec<IgnoredPattern>,
    pub(crate) save_error: Option<String>,
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::bridge::tasks::BackendServices;
    use crate::test_support::TestDir;

    /// The verifier indexes an entry by this path; a material referencing the
    /// same name is looked up through `ContentPath`. Both must reduce that one
    /// name identically — two lowercasing rules agree on ASCII and diverge on
    /// everything else, so the entry would exist and never be found.
    #[test]
    fn entry_paths_and_content_paths_reduce_a_name_identically() {
        let root = PathBuf::from("/addon");

        for name in [
            "Materials/Models/THING.VMT",
            "materials/CAFÉ.vmt",
            "a/b/c.txt",
        ] {
            let indexed = relative_slash_path(&root, &root.join(name)).expect("under the root");
            let looked_up = crate::bridge::content_path::ContentPath::new(name)
                .expect("a verified entry path has a canonical form");
            assert_eq!(
                indexed,
                looked_up.as_str(),
                "{name} indexes in a form its own lookup cannot reproduce"
            );
        }
    }

    #[test]
    fn ignored_patterns_keep_user_order_then_defaults_alphabetical() {
        let mut settings = Settings::default();
        settings.backend.ignore_globs = vec!["zzz/*".to_owned(), "aaa.txt".to_owned()];

        let patterns = ignored_patterns_from_settings(&settings);

        let user = patterns
            .iter()
            .take_while(|pattern| !pattern.default_pattern)
            .map(|pattern| pattern.pattern.as_str())
            .collect::<Vec<_>>();
        assert_eq!(user, ["zzz/*", "aaa.txt"]);

        let defaults = patterns
            .iter()
            .skip(user.len())
            .map(|pattern| {
                assert!(pattern.default_pattern);
                pattern.pattern.as_str()
            })
            .collect::<Vec<_>>();
        assert!(!defaults.is_empty());
        let mut sorted = defaults.clone();
        sorted.sort_unstable();
        assert_eq!(defaults, sorted);
    }

    #[test]
    fn verify_content_path_builds_file_browser_entries() {
        let root = TestDir::new("prepare-publish-verify");
        root.file("lua/autorun/init.lua", b"print('ready')");

        let services = BackendServices::for_test();
        let verified = verify_content_path(
            services.config(),
            services.archive(),
            ContentPathVerificationRequest {
                generation: Generation::from_raw(1),
                display_path: root.path_text(),
                path: root.path().to_path_buf(),
            },
        )
        .expect("content path should verify");

        let browser = file_browser::State::from_entries(verified.entries().iter().cloned());
        assert_eq!(verified.total_size(), 14);
        assert_eq!(browser.rows()[0].shortcut_prefix(), Some("lua/"));
        assert_eq!(browser.rows()[0].display_name(), "autorun");
    }

    #[test]
    fn verify_content_path_rejects_relative_paths() {
        let services = BackendServices::for_test();
        let result = verify_content_path(
            services.config(),
            services.archive(),
            ContentPathVerificationRequest {
                generation: Generation::from_raw(1),
                display_path: "relative".to_owned(),
                path: PathBuf::from("relative"),
            },
        );

        assert!(result.is_err());
    }

    #[test]
    fn missing_content_path_preserves_path_and_io_failure() {
        let root = TestDir::new("prepare-publish-missing-content");
        let missing = root.path().join("missing");
        let error = inspect_workshop_snapshot(ContentPathVerificationRequest {
            generation: Generation::from_raw(1),
            display_path: missing.to_string_lossy().into_owned(),
            path: missing.clone(),
        })
        .expect_err("a missing content directory must fail verification");

        assert_eq!(error.key, keys::PATH_IO_ERROR);
        let detail = error.detail.expect("path I/O errors carry context");
        assert!(detail.contains(&missing.to_string_lossy()[..]));
    }

    #[test]
    fn workshop_snapshot_inventory_does_not_apply_publish_ignores() {
        let root = TestDir::new("prepare-publish-workshop-snapshot");
        root.file("lua/autorun/init.lua", b"print('ready')");
        root.file(".git/config", b"published historical file");

        let snapshot = inspect_workshop_snapshot(ContentPathVerificationRequest {
            generation: Generation::from_raw(1),
            display_path: root.path_text(),
            path: root.path().to_path_buf(),
        })
        .expect("Workshop snapshot should ignore local publish filters");

        assert_eq!(snapshot.entries().len(), 2);
    }

    #[test]
    fn verify_icon_preview_maps_png_to_still_handle() {
        let root = TestDir::new("prepare-publish-icon-png");
        let source = root.image("icon.png", ::image::ImageFormat::Png, 32, 48);

        let verified = verify_icon_preview(IconVerificationRequest {
            generation: Generation::from_raw(1),
            display_path: source.to_string_lossy().into_owned(),
            path: source.clone(),
            well_rgb: [0x10, 0x10, 0x10],
        })
        .expect("png icon should verify");

        assert_eq!((verified.icon.width, verified.icon.height), (32, 48));
        assert_eq!(verified.icon.source_path, source);
        assert!(verified.animation.is_none());
    }

    #[test]
    fn missing_icon_preserves_path_and_io_failure() {
        let root = TestDir::new("prepare-publish-missing-icon");
        let missing = root.path().join("missing.png");
        let error = verify_icon_preview(IconVerificationRequest {
            generation: Generation::from_raw(1),
            display_path: missing.to_string_lossy().into_owned(),
            path: missing.clone(),
            well_rgb: [0x10, 0x10, 0x10],
        })
        .expect_err("a missing icon must fail verification");

        assert_eq!(error.key, keys::PATH_IO_ERROR);
        let detail = error.detail.expect("path I/O errors carry context");
        assert!(detail.contains(&missing.to_string_lossy()[..]));
    }

    #[test]
    fn oversized_square_icon_previews_at_display_resolution() {
        let root = TestDir::new("prepare-publish-icon-big");
        let source = root.image("icon.png", ::image::ImageFormat::Png, 1024, 1024);

        let verified = verify_icon_preview(IconVerificationRequest {
            generation: Generation::from_raw(1),
            display_path: source.to_string_lossy().into_owned(),
            path: source.clone(),
            well_rgb: [0x10, 0x10, 0x10],
        })
        .expect("big square icon should verify");

        // Display preview is bounded; submit still reads the original file.
        assert_eq!((verified.icon.width, verified.icon.height), (512, 512));
        assert_eq!(verified.icon.path, source);
        assert_eq!(verified.icon.source_path, source);
    }
}
