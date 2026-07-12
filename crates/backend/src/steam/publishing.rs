use crate::{
    GMOD_APP_ID, Transaction,
    appdata::AppData,
    gma::{GMAEntry, GMAFile, GMAMetadata, whitelist::AddonWhitelist},
    transactions::Transactions,
};
use image::{DynamicImage, ImageError, ImageFormat};
use path_slash::PathExt;
use std::{
    fs::File,
    io::BufReader,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    time::{Duration, SystemTime},
};
use steamworks::{PublishedFileId, SteamError};
use walkdir::WalkDir;

#[cfg(not(target_os = "windows"))]
use std::collections::HashSet;

#[derive(Debug, thiserror::Error)]
pub enum PublishError {
    #[error("ERR_WHITELIST:{}", .0.join("\n"))]
    NotWhitelisted(Vec<String>),
    #[error("ERR_NO_ENTRIES")]
    NoEntries,
    #[error("ERR_DUPLICATE_ENTRIES:{0}")]
    DuplicateEntry(String),
    #[error("ERR_INVALID_CONTENT_PATH")]
    InvalidContentPath,
    #[error("ERR_MULTIPLE_GMAS")]
    MultipleGMAs,
    #[error("ERR_ICON_TOO_LARGE")]
    IconTooLarge,
    #[error("ERR_ICON_TOO_SMALL")]
    IconTooSmall,
    #[error("ERR_ICON_INVALID_FORMAT")]
    IconInvalidFormat,
    #[error("ERR_IO_ERROR")]
    IOError(#[source] Option<std::sync::Arc<std::io::Error>>),
    #[error("ERR_STEAM_ERROR:{0}")]
    SteamError(SteamError),
    #[error("ERR_IMAGE_ERROR:{0}")]
    ImageError(#[source] ImageError),
    #[error("ERR_CANCELLED")]
    Cancelled,
    #[error(transparent)]
    Gma(#[from] crate::gma::GMAError),
}
impl crate::error_key::HasErrorKey for PublishError {
    fn error_key(&self) -> crate::error_key::ErrorKey {
        use crate::error_key::keys;
        match self {
            Self::NotWhitelisted(_) => keys::WHITELIST,
            Self::NoEntries => keys::NO_ENTRIES,
            Self::DuplicateEntry(_) => keys::DUPLICATE_ENTRIES,
            Self::InvalidContentPath => keys::INVALID_CONTENT_PATH,
            Self::MultipleGMAs => keys::MULTIPLE_GMAS,
            Self::IconTooLarge => keys::ICON_TOO_LARGE,
            Self::IconTooSmall => keys::ICON_TOO_SMALL,
            Self::IconInvalidFormat => keys::ICON_INVALID_FORMAT,
            Self::IOError(_) => keys::IO_ERROR,
            Self::SteamError(_) => keys::STEAM_ERROR,
            Self::ImageError(_) => keys::IMAGE_ERROR,
            Self::Cancelled => keys::CANCELLED,
            Self::Gma(error) => error.error_key(),
        }
    }

    fn error_detail(&self) -> Option<String> {
        match self {
            Self::NotWhitelisted(failed) => Some(failed.join("\n")),
            Self::DuplicateEntry(path) => Some(path.clone()),
            Self::IOError(Some(source)) => Some(source.to_string()),
            Self::SteamError(error) => Some(error.to_string()),
            Self::ImageError(error) => Some(error.to_string()),
            Self::Gma(error) => error.error_detail(),
            _ => None,
        }
    }
}
impl serde::Serialize for PublishError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}
impl From<SteamError> for PublishError {
    fn from(error: SteamError) -> Self {
        Self::SteamError(error)
    }
}
impl From<ImageError> for PublishError {
    fn from(error: ImageError) -> Self {
        Self::ImageError(error)
    }
}
impl From<std::io::Error> for PublishError {
    fn from(error: std::io::Error) -> Self {
        Self::IOError(Some(std::sync::Arc::new(error)))
    }
}

use super::Steam;
pub struct ContentPath(PathBuf);
impl AsRef<Path> for ContentPath {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}
impl From<ContentPath> for PathBuf {
    fn from(val: ContentPath) -> Self {
        val.0
    }
}
impl ContentPath {
    pub fn new(path: &Path) -> Result<Self, PublishError> {
        if !path.is_dir() {
            return Err(PublishError::InvalidContentPath);
        }

        let mut gma_path: Option<PathBuf> = None;
        for path in path.read_dir()?.filter_map(|entry| {
            entry.ok().and_then(|entry| {
                let path = entry.path();
                let extension = path.extension()?;
                if extension == "gma" { Some(path) } else { None }
            })
        }) {
            if gma_path.is_some() {
                return Err(PublishError::MultipleGMAs);
            }
            gma_path = Some(path);
        }

        gma_path.map(ContentPath).ok_or(PublishError::NoEntries)
    }
}

const WORKSHOP_ICON_MAX_SIZE: u64 = 1048576;
const WORKSHOP_ICON_MIN_SIZE: u64 = 16;
const WORKSHOP_DEFAULT_ICON: &[u8] = include_bytes!("../../assets/gmpublisher_default_icon.png");
const WORKSHOP_DEFAULT_DESCRIPTION: &str =
    "Uploaded with [url=https://github.com/charles-mills/gmpublished]gmpublished[/url]";
const WORKSHOP_LEGAL_AGREEMENT_URL: &str =
    "https://steamcommunity.com/workshop/workshoplegalagreement";

/// A suffix that's unique across concurrent publishes in this process and
/// across restarts (a crashed run's temp names never repeat), so per-run
/// temp files and directories never collide.
fn unique_temp_suffix() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{nanos}_{counter}")
}

/// Directory this operation packs its GMA into, uniquely named and confined
/// to the configured temp dir (never a parent of it) so a user-configured
/// temp path is always respected.
fn publish_temp_dir(app_data: &AppData) -> PathBuf {
    let mut dir = app_data.temp_dir();
    dir.push(format!("gmpublisher_publishing_{}", unique_temp_suffix()));
    dir
}

/// Deletes the whole per-publish temp directory (the packed GMA, and any
/// debris from an interrupted pack) once the flow ends, success or failure.
struct PublishDirGuard(PathBuf);
impl Drop for PublishDirGuard {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.0) {
            log::debug!(
                "Failed to remove temporary publish directory {}: {error}",
                self.0.display()
            );
        }
    }
}

fn publish_tags(mut tags: Vec<String>, addon_type: String) -> Vec<String> {
    tags.reserve(2);
    tags.push("Addon".to_string());
    tags.push(addon_type);
    tags
}

fn publish_update_status_key(processed: steamworks::UpdateStatus) -> Option<&'static str> {
    Some(match processed {
        steamworks::UpdateStatus::Invalid => return None,
        steamworks::UpdateStatus::PreparingConfig => "PUBLISH_PREPARING_CONFIG",
        steamworks::UpdateStatus::PreparingContent => "PUBLISH_PREPARING_CONTENT",
        steamworks::UpdateStatus::UploadingContent => "PUBLISH_UPLOADING_CONTENT",
        steamworks::UpdateStatus::UploadingPreviewFile => "PUBLISH_UPLOADING_PREVIEW_FILE",
        steamworks::UpdateStatus::CommittingChanges => "PUBLISH_COMMITTING_CHANGES",
    })
}

/// Polls a Steam UGC item-update handle until its submit callback resolves,
/// reporting progress on `transaction` along the way. Progress resets on
/// every phase change (or while the total is unknown) since a byte count
/// from the previous phase would otherwise show; once a phase is underway
/// its own byte-level progress is reported normally.
fn pump_publish_progress(
    update_handle: &steamworks::UpdateWatchHandle,
    transaction: &Transaction,
    result_rx: &mpsc::Receiver<Result<(PublishedFileId, bool), SteamError>>,
) -> Result<Result<(PublishedFileId, bool), SteamError>, PublishError> {
    let mut last_processed = None;
    loop {
        let (processed, progress, total) = update_handle.progress();
        if let Some(status) = publish_update_status_key(processed) {
            transaction.status(status);
        }
        if total == 0 || last_processed != Some(processed) {
            transaction.progress_reset();
        } else {
            transaction.data(crate::transactions::TransactionPayload::TotalBytes(total));
            transaction.progress(progress as f64 / total as f64);
        }
        last_processed = Some(processed);

        match result_rx.recv_timeout(Duration::from_millis(50)) {
            Ok(result) => return Ok(result),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(fail_publish_callback_channel(transaction));
            }
        }
    }
}

pub enum WorkshopIcon {
    Custom {
        image: DynamicImage,
        path: PathBuf,
        format: ImageFormat,
        width: u32,
        height: u32,
        upscale: bool,
    },
    Default,
}
impl WorkshopIcon {
    pub fn can_upscale(width: u32, height: u32, format: ImageFormat) -> bool {
        !matches!(format, ImageFormat::Gif) && ((width < 512 || height < 512) || (width != height))
    }
}
/// The default icon could not be written to disk; carries the path that was
/// attempted so the caller can report it.
#[derive(Debug)]
struct PreviewIconWriteFailed(PathBuf);

/// The preview file path handed to Steam for an upload. A temp file this
/// resolved (an upscaled copy or the bundled default icon) is deleted once
/// Steam is done reading it, on drop; a user-supplied icon path is left
/// untouched.
struct PreviewPath {
    path: PathBuf,
    owned_temp: Option<PathBuf>,
}
impl PreviewPath {
    fn borrowed(path: PathBuf) -> Self {
        Self {
            path,
            owned_temp: None,
        }
    }

    fn owned(path: PathBuf) -> Self {
        Self {
            owned_temp: Some(path.clone()),
            path,
        }
    }
}
impl AsRef<Path> for PreviewPath {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}
impl Drop for PreviewPath {
    fn drop(&mut self) {
        if let Some(temp) = self.owned_temp.take() {
            let _ = std::fs::remove_file(&temp);
        }
    }
}

impl WorkshopIcon {
    /// Materializes the icon into a preview file path for the Steam upload.
    ///
    /// A custom icon never fails here (a failed upscale falls back to the
    /// original path); only writing the default icon can fail. Every
    /// materialized temp file is uniquely named, so concurrent publishes and
    /// crashed prior runs never collide.
    fn into_preview_path(self, app_data: &AppData) -> Result<PreviewPath, PreviewIconWriteFailed> {
        match self {
            Self::Custom {
                path,
                image,
                width,
                height,
                upscale,
                format,
            } => {
                if upscale && Self::can_upscale(width, height, format) {
                    let format_extension = match format {
                        ImageFormat::Png => "png",
                        ImageFormat::Jpeg => "jpg",
                        _ => unreachable!(),
                    };

                    let mut temp_img = app_data.temp_dir();
                    temp_img.push(format!(
                        "gmpublisher_upscaled_icon_{}.{format_extension}",
                        unique_temp_suffix()
                    ));

                    let image =
                        image.resize_exact(512, 512, image::imageops::FilterType::CatmullRom);
                    match image.save_with_format(&temp_img, format) {
                        Ok(_) => Ok(PreviewPath::owned(temp_img)),
                        Err(_) => Ok(PreviewPath::borrowed(path)),
                    }
                } else {
                    Ok(PreviewPath::borrowed(path))
                }
            }
            Self::Default => {
                let mut path = app_data.temp_dir();
                path.push(format!(
                    "gmpublisher_default_icon_{}.png",
                    unique_temp_suffix()
                ));
                if let Err(error) = std::fs::write(&path, WORKSHOP_DEFAULT_ICON) {
                    log::error!(
                        "Failed to write default icon to {}: {error}",
                        path.display()
                    );
                    return Err(PreviewIconWriteFailed(path));
                }
                Ok(PreviewPath::owned(path))
            }
        }
    }

    /// [`Self::into_preview_path`], reporting a write failure on `transaction`
    /// and converting it to the [`PublishError`] the publish flow returns.
    fn resolve_preview_path(
        self,
        app_data: &AppData,
        transaction: &Transaction,
    ) -> Result<PreviewPath, PublishError> {
        self.into_preview_path(app_data)
            .map_err(|PreviewIconWriteFailed(path)| fail_publish_preview_path(transaction, path))
    }
}
impl WorkshopIcon {
    pub fn new<P: AsRef<Path>>(path: P, upscale: bool) -> Result<Self, PublishError> {
        let path = path.as_ref();

        let len = path.metadata()?.len();
        if len > WORKSHOP_ICON_MAX_SIZE {
            return Err(PublishError::IconTooLarge);
        } else if len < WORKSHOP_ICON_MIN_SIZE {
            return Err(PublishError::IconTooSmall);
        }

        let file_extension = path
            .extension()
            .and_then(|x| x.to_str())
            .unwrap_or("jpg")
            .to_ascii_lowercase();
        let image_format = match file_extension.as_str() {
            "png" => ImageFormat::Png,
            "gif" => ImageFormat::Gif,
            "jpeg" | "jpg" => ImageFormat::Jpeg,
            _ => return Err(PublishError::IconInvalidFormat),
        };

        let image = image::load(BufReader::new(File::open(path)?), image_format)?;
        Ok(Self::Custom {
            path: path.to_path_buf(),
            width: image.width(),
            height: image.height(),
            format: image_format,
            upscale,
            image,
        })
    }
}

pub enum WorkshopUpdateType {
    Creation {
        title: String,
        path: ContentPath,
        tags: Vec<String>,
        addon_type: String,
        preview: WorkshopIcon,
    },
    Update {
        path: ContentPath,
        tags: Vec<String>,
        addon_type: String,
        preview: Option<WorkshopIcon>,
        changes: Option<String>,
    },
}

#[derive(Debug, Clone)]
pub struct PublishSettingsSnapshot {
    pub temp: Option<PathBuf>,
    pub ignore_globs: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct PublishSubmission {
    pub content_path_src: PathBuf,
    pub icon_path: Option<PathBuf>,
    pub title: String,
    pub tags: Vec<String>,
    pub addon_type: String,
    pub upscale: bool,
    pub update_id: Option<u64>,
    pub changes: Option<String>,
    pub settings: Option<PublishSettingsSnapshot>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct PublishSubmissionOutcome {
    pub published_file_id: u64,
    pub legal_agreement_required: bool,
}

/// Outcome of creating a new Workshop item (see [`Steam::publish`]): the
/// freshly created id alongside whether Steam requires the legal agreement
/// to be accepted before the item is visible.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct PublishOutcome {
    pub id: PublishedFileId,
    pub legal_agreement_required: bool,
}

impl Steam {
    pub fn update(
        &self,
        id: PublishedFileId,
        details: WorkshopUpdateType,
        transaction: &Transaction,
        app_data: &AppData,
    ) -> Result<bool, PublishError> {
        use WorkshopUpdateType::{Creation, Update};

        let (result_tx, result_rx) = mpsc::channel();
        // The preview file must outlive the upload itself, not just the
        // builder call that hands Steam its path, so it's threaded out of
        // the match and only dropped (deleting an owned temp) after the
        // upload has finished below.
        let (update_handle, _preview) = match details {
            Creation {
                title,
                path,
                tags,
                addon_type,
                preview,
            } => {
                let tags = publish_tags(tags, addon_type);

                let preview_path = preview.resolve_preview_path(app_data, transaction)?;

                let handle = self
                    .client()
                    .expect("reached only through app-layer entry points that already checked steam_connected()")
                    .ugc()
                    .start_item_update(GMOD_APP_ID, id)
                    .content_path(path.as_ref())
                    .title(&title)
                    .preview_path(preview_path.as_ref())
                    .tags(tags, false)
                    .description(WORKSHOP_DEFAULT_DESCRIPTION)
                    .submit(None, move |result| {
                        let _ = result_tx.send(result);
                    });
                (handle, Some(preview_path))
            }

            Update {
                path,
                tags,
                addon_type,
                preview,
                changes,
            } => {
                let tags = publish_tags(tags, addon_type);

                let preview_path = preview
                    .map(|icon| icon.resolve_preview_path(app_data, transaction))
                    .transpose()?;

                let update = self
                    .client()
                    .expect("reached only through app-layer entry points that already checked steam_connected()")
                    .ugc()
                    .start_item_update(GMOD_APP_ID, id);
                let handle = match preview_path.as_ref() {
                    Some(preview_path) => update.preview_path(preview_path.as_ref()),
                    None => update,
                }
                .content_path(path.as_ref())
                .tags(tags, false)
                .submit(changes.as_deref(), move |result| {
                    let _ = result_tx.send(result);
                });
                (handle, preview_path)
            }
        };

        let result = pump_publish_progress(&update_handle, transaction, &result_rx)?;

        match result {
            Ok((_, legal_agreement)) => {
                transaction.progress(1.);
                Ok(legal_agreement)
            }
            Err(error) => Err(PublishError::SteamError(error)),
        }
    }

    /// Creates a new Workshop item and uploads `details` as its first
    /// revision. On failure after the item has been created, the item is
    /// deleted so a failed publish never leaves an empty orphan behind.
    pub fn publish(
        &self,
        details: WorkshopUpdateType,
        transaction: &Transaction,
        app_data: &AppData,
    ) -> Result<PublishOutcome, PublishError> {
        debug_assert!(matches!(details, WorkshopUpdateType::Creation { .. }));

        let (published_tx, published_rx) = mpsc::channel();
        self.client()
            .expect("reached only through app-layer entry points that already checked steam_connected()")
            .ugc()
            .create_item(
                GMOD_APP_ID,
                steamworks::FileType::Community,
                move |result| {
                    let _ = published_tx.send(result);
                },
            );

        let id = match published_rx.recv() {
            Ok(Ok((id, _))) => id,
            Ok(Err(error)) => return Err(PublishError::SteamError(error)),
            Err(mpsc::RecvError) => return Err(fail_publish_callback_channel(transaction)),
        };

        match self.update(id, details, transaction, app_data) {
            Ok(legal_agreement_required) => Ok(PublishOutcome {
                id,
                legal_agreement_required,
            }),
            Err(error) => {
                self.client()
                    .expect("reached only through app-layer entry points that already checked steam_connected()")
                    .ugc()
                    .delete_item(id, |_| {});
                Err(error)
            }
        }
    }

    pub fn update_icon(
        &self,
        addon_id: PublishedFileId,
        icon: WorkshopIcon,
        transaction: &Transaction,
        app_data: &AppData,
    ) -> Result<bool, PublishError> {
        let preview_path = icon.resolve_preview_path(app_data, transaction)?;

        let (result_tx, result_rx) = mpsc::channel();
        let update_handle = self
            .client()
            .expect("reached only through app-layer entry points that already checked steam_connected()")
            .ugc()
            .start_item_update(GMOD_APP_ID, addon_id)
            .preview_path(preview_path.as_ref())
            .submit(None, move |result| {
                let _ = result_tx.send(result);
            });

        let result = pump_publish_progress(&update_handle, transaction, &result_rx)?;

        match result {
            Ok((_, legal_agreement)) => {
                transaction.progress(1.);
                Ok(legal_agreement)
            }
            Err(error) => Err(PublishError::SteamError(error)),
        }
    }
}

pub fn submit_with_transaction(
    submission: PublishSubmission,
    transaction: &Transaction,
    app_data: &AppData,
    steam: &Steam,
    whitelist: &AddonWhitelist,
) -> Result<PublishSubmissionOutcome, PublishError> {
    let PublishSubmission {
        content_path_src,
        icon_path,
        title,
        tags,
        addon_type,
        upscale,
        update_id,
        changes,
        settings,
    } = submission;
    let update_id = update_id.map(PublishedFileId);

    if let Some(settings) = settings
        && let Err(error) = apply_publish_settings(&settings, app_data)
    {
        return emit_publish_error(transaction, error);
    }

    // `Some` only for a custom icon; `None` means "keep the existing preview"
    // when updating, or "use the default icon" when creating (resolved at the
    // `WorkshopUpdateType` construction below, where each branch knows which).
    let custom_icon = match icon_path {
        Some(icon_path) => {
            transaction.status("PUBLISH_PROCESSING_ICON");

            match WorkshopIcon::new(icon_path, upscale) {
                Ok(icon) => Some(icon),
                Err(error) => return emit_publish_error(transaction, error),
            }
        }
        None => None,
    };

    transaction.status("PUBLISH_PACKING");

    let publish_dir = publish_temp_dir(app_data);
    if let Err(error) = std::fs::create_dir_all(&publish_dir) {
        return emit_publish_error(transaction, error.into());
    }
    let _publish_dir_guard = PublishDirGuard(publish_dir.clone());

    {
        let gma = GMAFile {
            path: publish_dir.join("gmpublisher.gma"),
            size: 0,
            id: None,
            metadata: GMAMetadata::Standard {
                title: title.clone(),
                addon_type: addon_type.clone(),
                tags: tags.clone(),
                ignore: app_data.publish_ignore_globs_snapshot(),
            },
            version: 3,
            extracted_name: String::new(),
            modified: None,
        };

        if let Err(error) = gma.create(&content_path_src, transaction, whitelist) {
            if !transaction.aborted() {
                return emit_publish_error(transaction, error.into());
            }
            return Err(PublishError::Cancelled);
        }
    }

    let content_path = match ContentPath::new(&publish_dir) {
        Ok(content_path) => content_path,
        Err(error) => return emit_publish_error(transaction, error),
    };

    transaction.status("PUBLISH_STARTING");

    let outcome = if let Some(id) = update_id {
        steam
            .update(
                id,
                WorkshopUpdateType::Update {
                    path: content_path,
                    tags,
                    addon_type,
                    preview: custom_icon,
                    changes,
                },
                transaction,
                app_data,
            )
            .map(|legal_agreement_required| PublishOutcome {
                id,
                legal_agreement_required,
            })
    } else {
        steam.publish(
            WorkshopUpdateType::Creation {
                title,
                path: content_path,
                tags,
                addon_type,
                preview: custom_icon.unwrap_or(WorkshopIcon::Default),
            },
            transaction,
            app_data,
        )
    };

    match outcome {
        Ok(PublishOutcome {
            id,
            legal_agreement_required,
        }) => {
            transaction.finished(crate::transactions::TransactionPayload::None);
            Ok(PublishSubmissionOutcome {
                published_file_id: id.0,
                legal_agreement_required,
            })
        }
        Err(error) => {
            transaction.error(&error);
            Err(error)
        }
    }
}

fn apply_publish_settings(
    settings: &PublishSettingsSnapshot,
    app_data: &AppData,
) -> Result<(), PublishError> {
    if let Some(temp) = settings.temp.as_ref() {
        std::fs::create_dir_all(temp)?;
    }

    app_data.apply_publish_settings_snapshot(settings.temp.as_deref(), &settings.ignore_globs);
    Ok(())
}

fn emit_publish_error(
    transaction: &Transaction,
    error: PublishError,
) -> Result<PublishSubmissionOutcome, PublishError> {
    transaction.error(&error);
    Err(error)
}

/// Fails the transaction with the preview path that could not be materialized
/// and yields the error the publish flow returns early with.
fn fail_publish_preview_path(transaction: &Transaction, path: PathBuf) -> PublishError {
    transaction.error(crate::transactions::TransactionError::detailed(
        crate::error_key::keys::PATH_IO_ERROR,
        crate::transactions::detail_from_serialize(path),
    ));
    PublishError::IOError(None)
}

/// A disconnected Steam callback channel means Steam died mid-publish; fail
/// the transaction and yield the error the publish flow returns early with.
fn fail_publish_callback_channel(transaction: &Transaction) -> PublishError {
    transaction.error(crate::transactions::TransactionError::detailed(
        crate::error_key::keys::STEAM_ERROR,
        Some("callback channel disconnected".to_owned()),
    ));
    PublishError::SteamError(SteamError::IOFailure)
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "the app-layer caller across the crate boundary already owns and moves this path in"
)]
pub fn record_published_local_path(
    app_data: &AppData,
    published_file_id: u64,
    content_path_src: PathBuf,
) {
    app_data.record_published_local_path(PublishedFileId(published_file_id), &content_path_src);
}

pub fn verify_whitelist(
    path: &Path,
    app_data: &AppData,
    whitelist: &AddonWhitelist,
) -> Result<(Vec<GMAEntry>, u64), PublishError> {
    if !path.is_dir() || !path.is_absolute() {
        return Err(PublishError::InvalidContentPath);
    }

    let content_root = path.to_path_buf();

    let ignore = app_data.publish_ignore_globs_snapshot();
    let whitelist_snapshot = whitelist.snapshot();

    let mut size = 0;
    let mut failed_extra = false;
    let mut failed = Vec::with_capacity(10);
    let mut files = Vec::new();

    #[cfg(not(target_os = "windows"))]
    let mut dedup: HashSet<String> = HashSet::new();

    for (path, relative_path) in WalkDir::new(path)
        .follow_links(false)
        .contents_first(true)
        .into_iter()
        .filter_map(|entry| {
            let entry = entry.ok()?;

            if entry.path_is_symlink() {
                return None;
            }

            let path = entry.into_path();

            if path.is_dir() {
                return None;
            }

            let relative_path = match path.strip_prefix(&content_root) {
                Ok(rel_path) => rel_path.to_slash_lossy().to_string(),
                Err(_) => return None,
            };

            Some((path, relative_path))
        })
        .filter(|(_, relative_path)| !crate::gma::whitelist::is_default_ignored(relative_path))
        .filter(|(_, relative_path)| !crate::gma::whitelist::is_ignored(relative_path, &ignore))
    {
        #[cfg(not(target_os = "windows"))]
        {
            if !dedup.insert(relative_path.clone()) {
                return Err(PublishError::DuplicateEntry(relative_path));
            }
        }

        if !crate::gma::whitelist::is_whitelisted_in(&whitelist_snapshot, &relative_path) {
            if failed.len() == 9 {
                failed_extra = true;
                break;
            }
            failed.push(relative_path);
        } else if failed.is_empty() {
            let entry_size = path.metadata().map_or(0, |metadata| metadata.len());
            size += entry_size;
            files.push(GMAEntry {
                path: relative_path,
                size: entry_size,
                crc: 0,
                index: 0,
            });
        }
    }

    if failed.is_empty() {
        if files.is_empty() {
            Err(PublishError::NoEntries)
        } else {
            Ok((files, size))
        }
    } else {
        failed.sort_unstable();

        if failed_extra {
            failed.push("...".to_string());
        }

        Err(PublishError::NotWhitelisted(failed))
    }
}

pub fn publish_icon(
    icon_path: PathBuf,
    upscale: bool,
    addon_id: PublishedFileId,
    steam: Arc<Steam>,
    app_data: Arc<AppData>,
    transactions: &Transactions,
) -> u32 {
    let transaction = transactions.begin();
    let id = transaction.id;

    rayon::spawn(move || {
        let preview = match WorkshopIcon::new(icon_path, upscale) {
            Ok(icon) => icon,
            Err(error) => {
                transaction.error(&error);
                return;
            }
        };

        let result = steam.update_icon(addon_id, preview, &transaction, &app_data);

        match result {
            Ok(legal_agreement) => {
                if legal_agreement {
                    let _ = crate::path::open(WORKSHOP_LEGAL_AGREEMENT_URL);
                }
                transaction.finished(crate::transactions::TransactionPayload::None);
            }
            Err(error) => {
                transaction.error(&error);
            }
        };
    });

    id
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appdata::AppDataPaths;
    use crate::events::BackendEventCollector;
    use image::GenericImageView;
    use std::fs;

    struct Fixture {
        app_data: AppData,
        steam: Steam,
        whitelist: AddonWhitelist,
        transactions: Transactions,
        collector: BackendEventCollector,
        _temp: tempfile::TempDir,
    }

    impl Fixture {
        fn new() -> Self {
            let temp = tempfile::tempdir().expect("tempdir");
            fs::create_dir_all(temp.path().join("default-temp")).expect("default temp dir");
            let collector = BackendEventCollector::default();
            let transactions = Transactions::new(Arc::new(collector.clone()), false);
            let app_data = AppData::load(
                AppDataPaths::for_test_root(temp.path()),
                transactions.clone(),
            );
            Self {
                app_data,
                steam: Steam::new(transactions.clone()),
                whitelist: AddonWhitelist::new(),
                transactions,
                collector,
                _temp: temp,
            }
        }

        fn with_publishing_settings(temp: &Path, ignore_globs: &[String]) -> Self {
            // SAFETY: nextest runs one test per process; no concurrent mutator.
            unsafe { std::env::set_var("ADDON_WHITELIST_OFFLINE", "1") };
            let fixture = Self::new();
            fixture.app_data.mutate_settings(|settings| {
                settings.temp = Some(temp.to_path_buf());
                settings.ignore_globs = ignore_globs.to_vec();
            });
            fixture
        }
    }

    fn write_png(path: &Path, width: u32, height: u32) {
        let image = DynamicImage::ImageRgba8(image::ImageBuffer::from_pixel(
            width,
            height,
            image::Rgba([32, 64, 96, 255]),
        ));
        image
            .save_with_format(path, ImageFormat::Png)
            .expect("write png");
    }

    fn assert_content_path_error(path: &Path, expected: &str) {
        match ContentPath::new(path) {
            Ok(_) => panic!("expected content path error"),
            Err(error) => assert_eq!(error.to_string(), expected),
        }
    }

    fn assert_verify_whitelist_error(fixture: &Fixture, path: &Path, expected: &str) {
        match verify_whitelist(path, &fixture.app_data, &fixture.whitelist) {
            Ok(_) => panic!("expected verify whitelist error"),
            Err(error) => assert_eq!(error.to_string(), expected),
        }
    }

    #[test]
    fn publishing_error_keys_match_upstream_strings() {
        assert_eq!(
            PublishError::NotWhitelisted(vec!["bad.exe".to_string(), "bad.dll".to_string()])
                .to_string(),
            "ERR_WHITELIST:bad.exe\nbad.dll"
        );
        assert_eq!(PublishError::NoEntries.to_string(), "ERR_NO_ENTRIES");
        assert_eq!(
            PublishError::DuplicateEntry("lua/init.lua".to_string()).to_string(),
            "ERR_DUPLICATE_ENTRIES:lua/init.lua"
        );
        assert_eq!(
            PublishError::InvalidContentPath.to_string(),
            "ERR_INVALID_CONTENT_PATH"
        );
        assert_eq!(PublishError::MultipleGMAs.to_string(), "ERR_MULTIPLE_GMAS");
        assert_eq!(PublishError::IconTooLarge.to_string(), "ERR_ICON_TOO_LARGE");
        assert_eq!(PublishError::IconTooSmall.to_string(), "ERR_ICON_TOO_SMALL");
        assert_eq!(
            PublishError::IconInvalidFormat.to_string(),
            "ERR_ICON_INVALID_FORMAT"
        );
        assert_eq!(PublishError::IOError(None).to_string(), "ERR_IO_ERROR");
    }

    #[test]
    fn publishing_content_path_accepts_single_gma_and_reports_upstream_errors() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_content_path_error(&dir.path().join("missing"), "ERR_INVALID_CONTENT_PATH");
        assert_content_path_error(dir.path(), "ERR_NO_ENTRIES");

        let single = dir.path().join("single");
        fs::create_dir(&single).expect("single dir");
        let gma = single.join("addon.gma");
        fs::write(&gma, "not parsed by ContentPath").expect("gma marker");
        let content_path = match ContentPath::new(&single) {
            Ok(content_path) => content_path,
            Err(error) => panic!("unexpected content path error: {error}"),
        };
        assert_eq!(PathBuf::from(content_path), gma);

        let multiple = dir.path().join("multiple");
        fs::create_dir(&multiple).expect("multiple dir");
        fs::write(multiple.join("one.gma"), "").expect("first gma marker");
        fs::write(multiple.join("two.gma"), "").expect("second gma marker");
        assert_content_path_error(&multiple, "ERR_MULTIPLE_GMAS");
    }

    #[test]
    fn publishing_whitelist_respects_default_and_custom_ignores() {
        let dir = tempfile::tempdir().expect("tempdir");
        let content = dir.path().join("content");
        fs::create_dir(&content).expect("content dir");
        fs::create_dir(content.join("lua")).expect("lua dir");
        fs::create_dir(content.join("materials")).expect("materials dir");
        fs::write(content.join("lua/phase6.lua"), "print('phase6')\n").expect("lua file");
        fs::write(
            content.join("materials/skipped.png"),
            "ignored by custom glob",
        )
        .expect("material file");
        fs::write(content.join("addon.json"), "ignored by default glob").expect("addon metadata");

        let fixture =
            Fixture::with_publishing_settings(dir.path(), &["materials/*.png".to_string()]);
        let (entries, size) =
            match verify_whitelist(&content, &fixture.app_data, &fixture.whitelist) {
                Ok(verified) => verified,
                Err(error) => panic!("unexpected whitelist error: {error}"),
            };

        assert_eq!(size, "print('phase6')\n".len() as u64);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, "lua/phase6.lua");
        assert_eq!(entries[0].size, "print('phase6')\n".len() as u64);
    }

    #[test]
    fn publishing_whitelist_reports_invalid_no_entry_and_not_whitelisted_errors() {
        let dir = tempfile::tempdir().expect("tempdir");
        let fixture = Fixture::with_publishing_settings(dir.path(), &[]);

        assert_verify_whitelist_error(
            &fixture,
            &PathBuf::from("relative-content"),
            "ERR_INVALID_CONTENT_PATH",
        );

        let ignored_only = dir.path().join("ignored");
        fs::create_dir(&ignored_only).expect("ignored dir");
        fs::write(ignored_only.join("addon.json"), "{}").expect("default ignored file");
        assert_verify_whitelist_error(&fixture, &ignored_only, "ERR_NO_ENTRIES");

        let invalid = dir.path().join("invalid");
        fs::create_dir(&invalid).expect("invalid dir");
        fs::write(invalid.join("bad.exe"), "bad").expect("invalid file");
        assert_verify_whitelist_error(&fixture, &invalid, "ERR_WHITELIST:bad.exe");
    }

    #[test]
    fn publishing_icon_validation_and_upscale_match_upstream_rules() {
        let dir = tempfile::tempdir().expect("tempdir");
        let icon_path = dir.path().join("icon.png");
        write_png(&icon_path, 128, 64);

        let icon = match WorkshopIcon::new(&icon_path, false) {
            Ok(icon) => icon,
            Err(error) => panic!("unexpected icon error: {error}"),
        };
        match icon {
            WorkshopIcon::Custom {
                format,
                width,
                height,
                upscale,
                ..
            } => {
                assert_eq!(format, ImageFormat::Png);
                assert_eq!((width, height), (128, 64));
                assert!(!upscale);
            }
            WorkshopIcon::Default => panic!("expected custom icon"),
        }

        assert!(!WorkshopIcon::can_upscale(512, 512, ImageFormat::Png));
        assert!(WorkshopIcon::can_upscale(512, 256, ImageFormat::Png));
        assert!(!WorkshopIcon::can_upscale(32, 32, ImageFormat::Gif));

        fs::write(dir.path().join("too-small.png"), [0_u8; 15]).expect("small icon");
        match WorkshopIcon::new(dir.path().join("too-small.png"), false) {
            Ok(_) => panic!("expected small icon error"),
            Err(error) => assert_eq!(error.to_string(), "ERR_ICON_TOO_SMALL"),
        }

        fs::write(
            dir.path().join("too-large.png"),
            vec![0_u8; WORKSHOP_ICON_MAX_SIZE as usize + 1],
        )
        .expect("large icon");
        match WorkshopIcon::new(dir.path().join("too-large.png"), false) {
            Ok(_) => panic!("expected large icon error"),
            Err(error) => assert_eq!(error.to_string(), "ERR_ICON_TOO_LARGE"),
        }

        fs::write(
            dir.path().join("icon.bmp"),
            vec![0_u8; WORKSHOP_ICON_MIN_SIZE as usize],
        )
        .expect("invalid format icon");
        match WorkshopIcon::new(dir.path().join("icon.bmp"), false) {
            Ok(_) => panic!("expected invalid format error"),
            Err(error) => assert_eq!(error.to_string(), "ERR_ICON_INVALID_FORMAT"),
        }
    }

    #[test]
    fn publishing_icon_temp_paths_are_unique_and_clean_up_owned_temps_on_drop() {
        let dir = tempfile::tempdir().expect("tempdir");
        let icon_path = dir.path().join("icon.png");
        write_png(&icon_path, 64, 32);
        let fixture = Fixture::with_publishing_settings(dir.path(), &[]);

        let upscaled_icon = match WorkshopIcon::new(&icon_path, true) {
            Ok(icon) => icon,
            Err(error) => panic!("unexpected icon error: {error}"),
        };
        let upscaled_preview = upscaled_icon
            .into_preview_path(&fixture.app_data)
            .expect("upscaled icon path");
        let upscaled_path = upscaled_preview.as_ref().to_path_buf();
        assert!(
            upscaled_path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .is_some_and(|stem| stem.starts_with("gmpublisher_upscaled_icon_"))
        );
        assert_eq!(
            upscaled_path.extension().and_then(|ext| ext.to_str()),
            Some("png")
        );
        let upscaled = image::open(&upscaled_path).expect("read upscaled icon");
        assert_eq!(upscaled.dimensions(), (512, 512));
        drop(upscaled_preview);
        assert!(
            !upscaled_path.exists(),
            "owned upscaled icon temp should be cleaned up on drop"
        );

        let default_preview = WorkshopIcon::Default
            .into_preview_path(&fixture.app_data)
            .expect("default icon path");
        let default_path = default_preview.as_ref().to_path_buf();
        assert!(
            default_path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .is_some_and(|stem| stem.starts_with("gmpublisher_default_icon_"))
        );
        assert_eq!(
            default_path.extension().and_then(|ext| ext.to_str()),
            Some("png")
        );
        assert_eq!(
            fs::read(&default_path).expect("default icon"),
            WORKSHOP_DEFAULT_ICON
        );
        drop(default_preview);
        assert!(
            !default_path.exists(),
            "owned default icon temp should be cleaned up on drop"
        );

        // Two resolutions never share a temp name, even for the same icon.
        let another_default = WorkshopIcon::Default
            .into_preview_path(&fixture.app_data)
            .expect("second default icon path");
        assert_ne!(default_path.as_path(), another_default.as_ref());

        // A non-upscaled custom icon isn't owned: the user's own file is
        // untouched by drop.
        let plain_icon = WorkshopIcon::new(&icon_path, false).expect("plain icon");
        let plain_preview = plain_icon
            .into_preview_path(&fixture.app_data)
            .expect("plain icon path");
        assert_eq!(plain_preview.as_ref(), icon_path.as_path());
        drop(plain_preview);
        assert!(
            icon_path.is_file(),
            "user-supplied icon must not be deleted"
        );
    }

    #[test]
    fn publishing_config_helpers_preserve_tags_status_keys_and_urls() {
        assert_eq!(
            publish_tags(
                vec!["fun".to_string(), "scenic".to_string()],
                "map".to_string()
            ),
            vec![
                "fun".to_string(),
                "scenic".to_string(),
                "Addon".to_string(),
                "map".to_string()
            ]
        );
        assert_eq!(
            WORKSHOP_DEFAULT_DESCRIPTION,
            "Uploaded with [url=https://github.com/charles-mills/gmpublished]gmpublished[/url]"
        );
        assert_eq!(
            WORKSHOP_LEGAL_AGREEMENT_URL,
            "https://steamcommunity.com/workshop/workshoplegalagreement"
        );
        assert_eq!(
            publish_update_status_key(steamworks::UpdateStatus::Invalid),
            None
        );
        assert_eq!(
            publish_update_status_key(steamworks::UpdateStatus::PreparingConfig),
            Some("PUBLISH_PREPARING_CONFIG")
        );
        assert_eq!(
            publish_update_status_key(steamworks::UpdateStatus::PreparingContent),
            Some("PUBLISH_PREPARING_CONTENT")
        );
        assert_eq!(
            publish_update_status_key(steamworks::UpdateStatus::UploadingContent),
            Some("PUBLISH_UPLOADING_CONTENT")
        );
        assert_eq!(
            publish_update_status_key(steamworks::UpdateStatus::UploadingPreviewFile),
            Some("PUBLISH_UPLOADING_PREVIEW_FILE")
        );
        assert_eq!(
            publish_update_status_key(steamworks::UpdateStatus::CommittingChanges),
            Some("PUBLISH_COMMITTING_CHANGES")
        );
    }

    #[test]
    fn publish_temp_dir_is_unique_per_call_and_confined_to_configured_temp_dir() {
        let dir = tempfile::tempdir().expect("tempdir");
        let configured_temp = dir.path().join("configured-temp");
        fs::create_dir_all(&configured_temp).expect("configured temp dir");
        let fixture = Fixture::with_publishing_settings(&configured_temp, &[]);

        let first = publish_temp_dir(&fixture.app_data);
        let second = publish_temp_dir(&fixture.app_data);

        assert_ne!(first, second, "each call must derive a fresh temp name");
        assert_eq!(
            first.parent(),
            Some(configured_temp.as_path()),
            "publish temps must live directly under the configured temp dir, never a parent of it"
        );
        assert_eq!(second.parent(), Some(configured_temp.as_path()));
    }

    #[test]
    fn publishing_submit_helper_emits_icon_error_before_live_steam() {
        let dir = tempfile::tempdir().expect("tempdir");
        let icon_path = dir.path().join("icon.bmp");
        fs::write(&icon_path, [0_u8; WORKSHOP_ICON_MIN_SIZE as usize]).expect("invalid icon");
        let publish_temp = dir.path().join("publish-temp");
        let ignore_globs = vec!["materials/private/*.png".to_string()];
        let fixture = Fixture::with_publishing_settings(dir.path(), &[]);

        let transaction = fixture.transactions.begin();
        let transaction_id = transaction.id;

        let result = submit_with_transaction(
            PublishSubmission {
                content_path_src: dir.path().join("content"),
                icon_path: Some(icon_path),
                title: "Boundary Proof".to_string(),
                tags: vec!["fun".to_string()],
                addon_type: "tool".to_string(),
                upscale: false,
                update_id: Some(123),
                changes: Some("icon check".to_string()),
                settings: Some(PublishSettingsSnapshot {
                    temp: Some(publish_temp.clone()),
                    ignore_globs: ignore_globs.clone(),
                }),
            },
            &transaction,
            &fixture.app_data,
            &fixture.steam,
            &fixture.whitelist,
        );

        assert!(matches!(result, Err(PublishError::IconInvalidFormat)));
        assert_eq!(fixture.app_data.settings.load().temp, Some(publish_temp));
        assert_eq!(fixture.app_data.settings.load().ignore_globs, ignore_globs);

        {
            let events = fixture.collector.drain();
            assert!(events.iter().any(|event| matches!(
                event,
                crate::events::BackendEvent::Transaction(
                    crate::events::TransactionEvent::Status { id, status }
                ) if *id == transaction_id && status == "PUBLISH_PROCESSING_ICON"
            )));
            assert!(events.iter().any(|event| matches!(
                event,
                crate::events::BackendEvent::Transaction(
                    crate::events::TransactionEvent::Error { id, error }
                ) if *id == transaction_id
                    && error.key.as_str() == "ERR_ICON_INVALID_FORMAT"
                    && error.detail.is_none()
            )));
        }
    }
}
