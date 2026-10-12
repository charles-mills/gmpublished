use crate::transactions::TransactionStatus;
use crate::{
    STEAM_GMOD_APP_ID, Transaction,
    appdata::AppData,
    gma::{GmaFile, GmaMetadata, whitelist::AddonWhitelist},
};
use image::{DynamicImage, ImageError, ImageFormat};
use std::{
    fs::File,
    io::BufReader,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    time::{Duration, Instant, SystemTime},
};
use steamworks::SteamError;

use crate::WorkshopId;

/// Item updates can legitimately spend a long time preparing or committing
/// without changing the byte counters exposed by Steam. Keep an absolute
/// safety bound for a callback Steam may abandon, but do not treat ordinary
/// progress inactivity as proof that the upload stopped.
const PUBLISH_RESULT_TIMEOUT: Duration = Duration::from_secs(6 * 60 * 60);

#[must_use]
pub const fn workshop_icon_can_upscale(width: u32, height: u32, animated: bool) -> bool {
    !animated && (width < 512 || height < 512 || width != height)
}

#[derive(Debug, thiserror::Error)]
pub enum PublishError {
    #[error("the addon folder contains no uploadable files")]
    NoEntries,
    #[error("the content path is not an absolute directory")]
    InvalidContentPath,
    #[error("the addon folder contains more than one GMA")]
    MultipleGMAs,
    #[error("the icon exceeds the Workshop size limit")]
    IconTooLarge,
    #[error("the icon is below the Workshop size limit")]
    IconTooSmall,
    #[error("the icon is not a format the Workshop accepts")]
    IconInvalidFormat,
    #[error("the title must contain 1 to 128 UTF-8 bytes and no NUL characters")]
    InvalidTitle,
    #[error("the description exceeds the Workshop length limit")]
    DescriptionTooLong,
    #[error("publish I/O failed")]
    IoError(#[source] crate::IoFailure),
    #[error("publishing failed on {}", path.display())]
    PathIo { path: PathBuf },
    #[error("Steam returned an error: {0}")]
    SteamError(SteamError),
    #[error("Steam reported success without a Workshop item ID")]
    MissingPublishedFileId,
    #[error("Steam abandoned the publishing callback")]
    CallbackAbandoned,
    #[error("image processing failed: {0}")]
    ImageError(#[source] ImageError),
    #[error("cancelled")]
    Cancelled,
    #[error(transparent)]
    Gma(#[from] crate::gma::GmaError),
    #[error(transparent)]
    Runtime(#[from] crate::steam::runtime::SteamRuntimeError),
}
impl crate::error_key::HasErrorKey for PublishError {
    fn error_key(&self) -> crate::error_key::ErrorKey {
        use crate::error_key::keys;
        match self {
            Self::NoEntries => keys::NO_ENTRIES,
            Self::InvalidContentPath => keys::INVALID_CONTENT_PATH,
            Self::MultipleGMAs => keys::MULTIPLE_GMAS,
            Self::IconTooLarge => keys::ICON_TOO_LARGE,
            Self::IconTooSmall => keys::ICON_TOO_SMALL,
            Self::IconInvalidFormat => keys::ICON_INVALID_FORMAT,
            Self::InvalidTitle => keys::STEAM_ERROR,
            Self::DescriptionTooLong => keys::DESCRIPTION_TOO_LONG,
            Self::IoError(_) => keys::IO_ERROR,
            Self::PathIo { .. } => keys::PATH_IO_ERROR,
            Self::SteamError(_) => keys::STEAM_ERROR,
            Self::MissingPublishedFileId | Self::CallbackAbandoned => keys::STEAM_ERROR,
            Self::ImageError(_) => keys::IMAGE_ERROR,
            Self::Cancelled => keys::CANCELLED,
            Self::Gma(error) => error.error_key(),
            Self::Runtime(error) => error.error_key(),
        }
    }

    fn error_detail(&self) -> Option<String> {
        match self {
            Self::IoError(source) => Some(source.to_string()),
            Self::PathIo { path } => crate::transactions::detail_from_serialize(path),
            Self::SteamError(error) => Some(error.to_string()),
            Self::InvalidTitle | Self::MissingPublishedFileId | Self::CallbackAbandoned => {
                Some(self.to_string())
            }
            Self::ImageError(error) => Some(error.to_string()),
            Self::Gma(error) => error.error_detail(),
            Self::Runtime(error) => error.error_detail(),
            _ => None,
        }
    }
}

impl PublishError {
    /// Steam offers no cancellation API once an item update is submitted.
    /// When either error comes from the post-submit progress pump, it means
    /// only that this process stopped waiting; the client may still be reading
    /// the upload inputs and committing the revision.
    fn submitted_upload_may_be_active(&self) -> bool {
        matches!(self, Self::CallbackAbandoned | Self::Cancelled)
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
        Self::IoError(error.into())
    }
}

use super::{ConnectedSteam, Steam};
struct ContentPath(PathBuf);
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
    fn new(path: &Path) -> Result<Self, PublishError> {
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
pub const WORKSHOP_DEFAULT_DESCRIPTION: &str = "No description provided";

pub const WORKSHOP_LEGACY_DEFAULT_DESCRIPTION: &str =
    "Uploaded with [url=https://github.com/charles-mills/gmpublished]gmpublished[/url]";

/// Steam refuses `SetItemDescription` input past this byte length
/// (`k_cchPublishedDocumentDescriptionMax`).
pub const WORKSHOP_DESCRIPTION_MAX_BYTES: usize = 8000;

fn validate_title(title: &str) -> Result<(), PublishError> {
    if title.trim().is_empty() || title.len() > 128 || title.contains('\0') {
        Err(PublishError::InvalidTitle)
    } else {
        Ok(())
    }
}

fn creation_description(custom: Option<&str>) -> Result<String, PublishError> {
    let custom = custom.map(str::trim).filter(|text| !text.is_empty());
    Ok(sanitize_description(custom.unwrap_or(WORKSHOP_DEFAULT_DESCRIPTION))?.into_owned())
}

/// Drop NUL bytes before Steam builds a CString. Reject oversize descriptions without truncating.
pub fn sanitize_description(description: &str) -> Result<std::borrow::Cow<'_, str>, PublishError> {
    let description = if description.contains('\0') {
        std::borrow::Cow::Owned(description.replace('\0', ""))
    } else {
        std::borrow::Cow::Borrowed(description)
    };
    if description.len() > WORKSHOP_DESCRIPTION_MAX_BYTES {
        return Err(PublishError::DescriptionTooLong);
    }
    Ok(description)
}

fn unique_temp_suffix() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{nanos}_{counter}")
}

fn publish_temp_dir(app_data: &AppData, settings: Option<&PublishSettingsSnapshot>) -> PathBuf {
    let mut dir = settings
        .and_then(|settings| settings.temp.clone())
        .filter(|temp| temp.is_dir())
        .unwrap_or_else(|| app_data.temp_dir());
    dir.push(format!("gmpublisher_publishing_{}", unique_temp_suffix()));
    dir
}

fn ensure_temp_dir(app_data: &AppData) -> PathBuf {
    let dir = app_data.temp_dir();
    if let Err(error) = std::fs::create_dir_all(&dir) {
        log::debug!(
            "Failed to create temporary directory {}: {error}",
            dir.display()
        );
    }
    dir
}

/// Deletes the whole per-publish temp directory (the packed GMA, and any
/// debris from an interrupted pack) once the flow reaches a known terminal
/// outcome. Indeterminate submitted uploads explicitly disarm it.
struct PublishDirGuard(Option<PathBuf>);
impl PublishDirGuard {
    fn new(path: PathBuf) -> Self {
        Self(Some(path))
    }

    /// Leaves the directory for Steam when an already-submitted update has an
    /// indeterminate outcome. The app's ordinary temp cleanup can reclaim it
    /// on a later run, after Steam can no longer be using it.
    fn preserve(mut self) {
        self.0.take();
    }
}
impl Drop for PublishDirGuard {
    fn drop(&mut self) {
        let Some(path) = self.0.take() else {
            return;
        };
        if let Err(error) = std::fs::remove_dir_all(&path) {
            log::debug!(
                "Failed to remove temporary publish directory {}: {error}",
                path.display()
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

fn publish_update_status(processed: steamworks::UpdateStatus) -> Option<TransactionStatus> {
    Some(match processed {
        steamworks::UpdateStatus::Invalid => return None,
        steamworks::UpdateStatus::PreparingConfig => TransactionStatus::PublishPreparingConfig,
        steamworks::UpdateStatus::PreparingContent => TransactionStatus::PublishPreparingContent,
        steamworks::UpdateStatus::UploadingContent => TransactionStatus::PublishUploadingContent,
        steamworks::UpdateStatus::UploadingPreviewFile => {
            TransactionStatus::PublishUploadingPreviewFile
        }
        steamworks::UpdateStatus::CommittingChanges => TransactionStatus::PublishCommittingChanges,
    })
}

/// Polls submit completion and reports per-phase progress, resetting it when
/// the phase changes or its total is unknown.
/// Cancellation stops waiting; Steam owns the submitted upload and cannot recall it.
fn pump_publish_progress(
    update_handle: &steamworks::UpdateWatchHandle,
    transaction: &Transaction,
    result_rx: &mpsc::Receiver<Result<(steamworks::PublishedFileId, bool), SteamError>>,
) -> Result<Result<(steamworks::PublishedFileId, bool), SteamError>, PublishError> {
    let mut last_processed = None;
    let callback_deadline = Instant::now() + PUBLISH_RESULT_TIMEOUT;
    loop {
        if transaction.aborted() {
            return Err(PublishError::Cancelled);
        }
        let (processed, progress, total) = update_handle.progress();
        if Instant::now() >= callback_deadline {
            return Err(PublishError::CallbackAbandoned);
        }
        if let Some(status) = publish_update_status(processed) {
            transaction.status(status);
        }
        if total == 0 || last_processed != Some(processed) {
            transaction.progress_reset();
        } else {
            transaction.data(crate::transactions::TransactionPayload::TotalBytes(total));
            transaction.progress(progress as f64 / total as f64);
        }
        last_processed = Some(processed);

        match result_rx.recv_timeout(
            callback_deadline
                .saturating_duration_since(Instant::now())
                .min(Duration::from_millis(50)),
        ) {
            Ok(result) => return Ok(result),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(PublishError::CallbackAbandoned);
            }
        }
    }
}

pub struct WorkshopIcon {
    source: WorkshopIconSource,
}

enum WorkshopIconSource {
    Custom {
        path: PathBuf,
        upscale: Option<WorkshopIconUpscale>,
    },
    Default,
}

struct WorkshopIconUpscale {
    image: DynamicImage,
    format: UpscalableImageFormat,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum UpscalableImageFormat {
    Png,
    Jpeg,
}

impl UpscalableImageFormat {
    const fn image_format(self) -> ImageFormat {
        match self {
            Self::Png => ImageFormat::Png,
            Self::Jpeg => ImageFormat::Jpeg,
        }
    }

    const fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpg",
        }
    }
}

impl Default for WorkshopIcon {
    fn default() -> Self {
        Self {
            source: WorkshopIconSource::Default,
        }
    }
}
#[derive(Debug)]
struct PreviewIconWriteFailed(PathBuf);

/// The preview file path handed to Steam for an upload. A temp file this
/// resolved (an upscaled copy or the bundled default icon) is deleted once
/// Steam is known to be done reading it, on drop; an indeterminate upload can
/// disarm cleanup, and a user-supplied icon path is always left untouched.
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

    fn preserve(mut self) {
        self.owned_temp.take();
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
    fn into_preview_path(self, app_data: &AppData) -> Result<PreviewPath, PreviewIconWriteFailed> {
        match self.source {
            WorkshopIconSource::Custom { path, upscale } => {
                if let Some(WorkshopIconUpscale { image, format }) = upscale {
                    let mut temp_img = ensure_temp_dir(app_data);
                    temp_img.push(format!(
                        "gmpublisher_upscaled_icon_{}.{}",
                        unique_temp_suffix(),
                        format.extension()
                    ));

                    let image =
                        image.resize_exact(512, 512, image::imageops::FilterType::CatmullRom);
                    match image.save_with_format(&temp_img, format.image_format()) {
                        Ok(_) => Ok(PreviewPath::owned(temp_img)),
                        Err(_) => Ok(PreviewPath::borrowed(path)),
                    }
                } else {
                    Ok(PreviewPath::borrowed(path))
                }
            }
            WorkshopIconSource::Default => {
                let mut path = ensure_temp_dir(app_data);
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

    fn resolve_preview_path(self, app_data: &AppData) -> Result<PreviewPath, PublishError> {
        self.into_preview_path(app_data)
            .map_err(|PreviewIconWriteFailed(path)| PublishError::PathIo { path })
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
        let (image_format, upscale_format) = match file_extension.as_str() {
            "png" => (ImageFormat::Png, Some(UpscalableImageFormat::Png)),
            "gif" => (ImageFormat::Gif, None),
            "jpeg" | "jpg" => (ImageFormat::Jpeg, Some(UpscalableImageFormat::Jpeg)),
            _ => return Err(PublishError::IconInvalidFormat),
        };

        let image = image::load(BufReader::new(File::open(path)?), image_format)?;
        let can_upscale =
            workshop_icon_can_upscale(image.width(), image.height(), upscale_format.is_none());
        let upscale = match (upscale && can_upscale, upscale_format) {
            (true, Some(format)) => Some(WorkshopIconUpscale { image, format }),
            _ => None,
        };

        Ok(Self {
            source: WorkshopIconSource::Custom {
                path: path.to_path_buf(),
                upscale,
            },
        })
    }
}

struct WorkshopCreation {
    language: crate::WorkshopLanguage,
    title: String,
    path: ContentPath,
    tags: Vec<String>,
    addon_type: String,
    preview: WorkshopIcon,
    /// Already sanitized; see [`sanitize_description`].
    description: String,
}

struct WorkshopUpdate {
    path: ContentPath,
    tags: Vec<String>,
    addon_type: String,
    preview: Option<WorkshopIcon>,
    changes: Option<String>,
}

enum WorkshopRevision {
    Creation(WorkshopCreation),
    Update(WorkshopUpdate),
}

#[derive(Clone, Debug)]
pub struct PublishSettingsSnapshot {
    pub temp: Option<PathBuf>,
    pub ignore_globs: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct PublishSubmission {
    pub language: crate::WorkshopLanguage,
    pub content_path_src: PathBuf,
    pub icon_path: Option<PathBuf>,
    /// Embedded in the packed GMA's metadata for both modes; the Workshop item
    /// title is only set when creating.
    pub title: String,
    pub tags: Vec<String>,
    pub addon_type: String,
    pub upscale: bool,
    /// Creation only. None uses the default placeholder; updates keep the existing description.
    pub description: Option<String>,
    pub mode: PublishSubmissionMode,
    pub settings: Option<PublishSettingsSnapshot>,
}

#[derive(Clone, Debug)]
pub enum PublishSubmissionMode {
    Create,
    Update {
        id: WorkshopId,
        changes: Option<String>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PublishSubmissionOutcome {
    pub published_file_id: WorkshopId,
    pub legal_agreement_required: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PublishOutcome {
    id: WorkshopId,
    legal_agreement_required: bool,
}

impl ConnectedSteam<'_> {
    fn submit_revision(
        &self,
        id: WorkshopId,
        details: WorkshopRevision,
        transaction: &Transaction,
        app_data: &AppData,
    ) -> Result<bool, PublishError> {
        let (result_tx, result_rx) = mpsc::channel();
        // Keep the preview file alive until Steam finishes reading it, not just until
        // the builder returns.
        let (update_handle, preview) = match details {
            WorkshopRevision::Creation(WorkshopCreation {
                language,
                title,
                path,
                tags,
                addon_type,
                preview,
                description,
            }) => {
                let tags = publish_tags(tags, addon_type);

                let preview_path = preview.resolve_preview_path(app_data)?;

                let handle = self
                    .interface
                    .client()
                    .ugc()
                    .start_item_update(STEAM_GMOD_APP_ID, id.into())
                    .content_path(path.as_ref())
                    .language(language.api_code())
                    .title(&title)
                    .preview_path(preview_path.as_ref())
                    .tags(tags, false)
                    .description(&description)
                    .submit(None, move |result| {
                        let _ = result_tx.send(result);
                    });
                (handle, Some(preview_path))
            }

            WorkshopRevision::Update(WorkshopUpdate {
                path,
                tags,
                addon_type,
                preview,
                changes,
            }) => {
                let tags = publish_tags(tags, addon_type);

                let preview_path = preview
                    .map(|icon| icon.resolve_preview_path(app_data))
                    .transpose()?;

                let update = self
                    .interface
                    .client()
                    .ugc()
                    .start_item_update(STEAM_GMOD_APP_ID, id.into());
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

        let result = match pump_publish_progress(&update_handle, transaction, &result_rx) {
            Ok(result) => result,
            Err(error) => {
                if error.submitted_upload_may_be_active()
                    && let Some(preview) = preview
                {
                    preview.preserve();
                }
                return Err(error);
            }
        };

        match result {
            Ok((_, legal_agreement)) => {
                transaction.progress(1.);
                Ok(legal_agreement)
            }
            Err(error) => Err(PublishError::SteamError(error)),
        }
    }

    fn update(
        &self,
        id: WorkshopId,
        details: WorkshopUpdate,
        transaction: &Transaction,
        app_data: &AppData,
    ) -> Result<bool, PublishError> {
        self.submit_revision(id, WorkshopRevision::Update(details), transaction, app_data)
    }

    /// Creates a new Workshop item and uploads `details` as its first
    /// revision. A definitively failed revision deletes the empty item; when
    /// completion is unknown, the item and its inputs remain intact because
    /// Steam may still be committing them.
    fn publish(
        &self,
        details: WorkshopCreation,
        transaction: &Transaction,
        app_data: &AppData,
    ) -> Result<PublishOutcome, PublishError> {
        let (published_tx, published_rx) = mpsc::channel();
        self.interface.client().ugc().create_item(
            STEAM_GMOD_APP_ID,
            steamworks::FileType::Community,
            move |result| {
                let _ = published_tx.send(result);
            },
        );

        let id = match published_rx.recv_timeout(super::CALLBACK_RESULT_TIMEOUT) {
            Ok(Ok((id, _))) => {
                WorkshopId::try_from(id).map_err(|_| PublishError::MissingPublishedFileId)?
            }
            Ok(Err(error)) => return Err(PublishError::SteamError(error)),
            Err(_) => return Err(PublishError::CallbackAbandoned),
        };

        match self.submit_revision(
            id,
            WorkshopRevision::Creation(details),
            transaction,
            app_data,
        ) {
            Ok(legal_agreement_required) => Ok(PublishOutcome {
                id,
                legal_agreement_required,
            }),
            Err(error) => {
                if !error.submitted_upload_may_be_active() {
                    self.interface.client().ugc().delete_item(id.into(), |_| {});
                }
                Err(error)
            }
        }
    }

    pub(crate) fn update_metadata(
        &self,
        addon_id: WorkshopId,
        language: crate::WorkshopLanguage,
        title: Option<&str>,
        description: Option<&str>,
        transaction: &Transaction,
    ) -> Result<bool, PublishError> {
        if let Some(title) = title {
            validate_title(title)?;
        }
        let description = description.map(sanitize_description).transpose()?;
        let (result_tx, result_rx) = mpsc::channel();
        let mut update = self
            .interface
            .client()
            .ugc()
            .start_item_update(STEAM_GMOD_APP_ID, addon_id.into())
            .language(language.api_code());
        if let Some(title) = title {
            update = update.title(title);
        }
        if let Some(description) = description.as_deref() {
            update = update.description(description);
        }
        let update_handle = update.submit(None, move |result| {
            let _sent = result_tx.send(result);
        });
        match pump_publish_progress(&update_handle, transaction, &result_rx)? {
            Ok((_, legal_agreement)) => {
                transaction.progress(1.);
                Ok(legal_agreement)
            }
            Err(error) => Err(PublishError::SteamError(error)),
        }
    }

    pub(crate) fn update_icon(
        &self,
        addon_id: WorkshopId,
        icon: WorkshopIcon,
        transaction: &Transaction,
        app_data: &AppData,
    ) -> Result<bool, PublishError> {
        let preview_path = icon.resolve_preview_path(app_data)?;

        let (result_tx, result_rx) = mpsc::channel();
        let update_handle = self
            .interface
            .client()
            .ugc()
            .start_item_update(STEAM_GMOD_APP_ID, addon_id.into())
            .preview_path(preview_path.as_ref())
            .submit(None, move |result| {
                let _ = result_tx.send(result);
            });

        let result = match pump_publish_progress(&update_handle, transaction, &result_rx) {
            Ok(result) => result,
            Err(error) => {
                if error.submitted_upload_may_be_active() {
                    preview_path.preserve();
                }
                return Err(error);
            }
        };

        match result {
            Ok((_, legal_agreement)) => {
                transaction.progress(1.);
                Ok(legal_agreement)
            }
            Err(error) => Err(PublishError::SteamError(error)),
        }
    }
}

pub(crate) fn submit_with_transaction(
    submission: PublishSubmission,
    transaction: &Transaction,
    app_data: &AppData,
    steam: &Steam,
    whitelist: &AddonWhitelist,
    cpu: &crate::execution::CpuExecutor,
) -> Result<PublishSubmissionOutcome, PublishError> {
    // Keep deterministic temp cleanup outside `complete` so its potentially
    // slow directory walk cannot delay the terminal event.
    let mut publish_dir_guard = None;
    let result = transaction.complete(
        || {
            let PublishSubmission {
                language,
                content_path_src,
                icon_path,
                title,
                tags,
                addon_type,
                upscale,
                description,
                mode,
                settings,
            } = submission;

            let description = creation_description(description.as_deref())?;
            if matches!(mode, PublishSubmissionMode::Create) {
                validate_title(&title)?;
            }

            let ignore_globs = settings.as_ref().map_or_else(
                || app_data.publish_ignore_globs_snapshot(),
                |settings| settings.ignore_globs.clone(),
            );

            if let Some(settings) = settings.as_ref() {
                prepare_publish_temp_dir(settings)?;
            }

            // `Some` only for a custom icon; `None` means "keep the existing preview"
            // when updating, or "use the default icon" when creating (resolved at the
            // mode-specific request construction below, where each branch knows which).
            let custom_icon = match icon_path {
                Some(icon_path) => {
                    transaction
                        .status(crate::transactions::TransactionStatus::PublishProcessingIcon);

                    Some(WorkshopIcon::new(icon_path, upscale)?)
                }
                None => None,
            };

            transaction.status(crate::transactions::TransactionStatus::PublishPacking);

            let publish_dir = publish_temp_dir(app_data, settings.as_ref());
            std::fs::create_dir_all(&publish_dir)?;
            publish_dir_guard = Some(PublishDirGuard::new(publish_dir.clone()));

            {
                let gma = GmaFile::for_creation(
                    publish_dir.join("gmpublisher.gma"),
                    GmaMetadata::Standard {
                        title: title.clone(),
                        addon_type: addon_type.clone(),
                        tags: tags.clone(),
                        ignore: ignore_globs,
                    },
                );

                if let Err(error) = gma.create(&content_path_src, transaction, whitelist, cpu) {
                    // Use the packer's cancellation error. Checking
                    // transaction state again could mislabel a concurrent
                    // I/O failure.
                    if matches!(error, crate::GmaError::Cancelled) {
                        return Err(PublishError::Cancelled);
                    }
                    return Err(error.into());
                }
            }

            let content_path = ContentPath::new(&publish_dir)?;

            transaction.status(crate::transactions::TransactionStatus::PublishStarting);

            // Everything above runs without Steam; the client is required only from
            // here, so validation failures stay reachable offline.
            let steam = steam.require_client().map_err(PublishError::from)?;

            let outcome = if let PublishSubmissionMode::Update { id, changes } = mode {
                steam
                    .update(
                        id,
                        WorkshopUpdate {
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
                    WorkshopCreation {
                        language,
                        title,
                        path: content_path,
                        tags,
                        addon_type,
                        preview: custom_icon.unwrap_or_default(),
                        description,
                    },
                    transaction,
                    app_data,
                )
            };

            match outcome {
                Ok(PublishOutcome {
                    id,
                    legal_agreement_required,
                }) => Ok(PublishSubmissionOutcome {
                    published_file_id: id,
                    legal_agreement_required,
                }),
                Err(error) => {
                    if error.submitted_upload_may_be_active() {
                        publish_dir_guard
                            .take()
                            .expect("the publish directory guard is installed before submission")
                            .preserve();
                    }
                    Err(error)
                }
            }
        },
        |_| crate::transactions::TransactionPayload::None,
    );
    drop(publish_dir_guard);
    result
}

fn prepare_publish_temp_dir(settings: &PublishSettingsSnapshot) -> Result<(), PublishError> {
    if let Some(temp) = settings.temp.as_ref() {
        std::fs::create_dir_all(temp)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::workshop_id::workshop_id;

    use super::*;
    use crate::appdata::AppDataPaths;
    use crate::events::BackendEventCollector;
    use crate::transactions::Transactions;
    use image::GenericImageView;
    use std::fs;
    use std::sync::Arc;

    #[test]
    fn runtime_failures_keep_their_own_detail_through_publish_error() {
        use crate::error_key::HasErrorKey as _;
        use crate::steam::runtime::SteamRuntimeError;

        let unavailable = PublishError::from(SteamRuntimeError::Unavailable);
        let disconnected = PublishError::from(SteamRuntimeError::NotConnected);

        assert_eq!(unavailable.error_key(), crate::error_key::keys::STEAM_ERROR);
        assert_eq!(
            disconnected.error_key(),
            crate::error_key::keys::STEAM_ERROR
        );
        assert_ne!(unavailable.error_detail(), disconnected.error_detail());
        assert_eq!(
            unavailable.error_detail(),
            SteamRuntimeError::Unavailable.error_detail()
        );
    }

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
            let transactions = Transactions::new(Arc::new(collector.clone()));
            let app_data = AppData::load(
                AppDataPaths::for_test_root(temp.path()),
                transactions.clone(),
            );
            Self {
                app_data,
                steam: Steam::new(transactions.clone()),
                whitelist: AddonWhitelist::builtin_only(),
                transactions,
                collector,
                _temp: temp,
            }
        }

        fn with_publishing_settings(temp: &Path, ignore_globs: &[String]) -> Self {
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

    fn write_gif(path: &Path, width: u32, height: u32) {
        let image = DynamicImage::ImageRgba8(image::ImageBuffer::from_pixel(
            width,
            height,
            image::Rgba([32, 64, 96, 255]),
        ));
        image
            .save_with_format(path, ImageFormat::Gif)
            .expect("write gif");
    }

    fn assert_content_path_error(path: &Path, expected_key: &str) {
        use crate::error_key::HasErrorKey;
        match ContentPath::new(path) {
            Ok(_) => panic!("expected content path error"),
            Err(error) => assert_eq!(error.error_key().as_str(), expected_key, "{error}"),
        }
    }

    #[test]
    fn publishing_error_keys_match_upstream_strings() {
        use crate::error_key::HasErrorKey;

        let assert_wire = |error: PublishError, key: &str, detail: Option<&str>| {
            assert_eq!(error.error_key().as_str(), key, "{error}");
            assert_eq!(error.error_detail().as_deref(), detail, "{error}");
        };

        assert_wire(
            PublishError::Gma(crate::GmaError::DuplicateEntry("lua/init.lua".to_string())),
            "ERR_DUPLICATE_ENTRIES",
            Some("lua/init.lua"),
        );
        assert_wire(
            PublishError::InvalidContentPath,
            "ERR_INVALID_CONTENT_PATH",
            None,
        );
        assert_wire(PublishError::MultipleGMAs, "ERR_MULTIPLE_GMAS", None);
        assert_wire(PublishError::IconTooLarge, "ERR_ICON_TOO_LARGE", None);
        assert_wire(PublishError::IconTooSmall, "ERR_ICON_TOO_SMALL", None);
        assert_wire(
            PublishError::IconInvalidFormat,
            "ERR_ICON_INVALID_FORMAT",
            None,
        );
        assert_wire(
            PublishError::PathIo {
                path: PathBuf::from("/addons/icon.png"),
            },
            "ERR_PATH_IO_ERROR",
            Some("/addons/icon.png"),
        );
        assert_wire(
            PublishError::MissingPublishedFileId,
            "ERR_STEAM_ERROR",
            Some("Steam reported success without a Workshop item ID"),
        );
        assert_wire(
            PublishError::CallbackAbandoned,
            "ERR_STEAM_ERROR",
            Some("Steam abandoned the publishing callback"),
        );
    }

    #[test]
    fn indeterminate_uploads_preserve_inputs_while_terminal_paths_clean_up() {
        assert!(PublishError::CallbackAbandoned.submitted_upload_may_be_active());
        assert!(PublishError::Cancelled.submitted_upload_may_be_active());
        assert!(!PublishError::NoEntries.submitted_upload_may_be_active());

        let root = tempfile::tempdir().expect("tempdir");
        let preserved_dir = root.path().join("preserved-publish");
        fs::create_dir(&preserved_dir).expect("preserved publish dir");
        PublishDirGuard::new(preserved_dir.clone()).preserve();
        assert!(preserved_dir.is_dir());

        let cleaned_dir = root.path().join("cleaned-publish");
        fs::create_dir(&cleaned_dir).expect("cleaned publish dir");
        drop(PublishDirGuard::new(cleaned_dir.clone()));
        assert!(!cleaned_dir.exists());

        let preserved_preview = root.path().join("preserved-preview.png");
        fs::write(&preserved_preview, b"preview").expect("preserved preview");
        PreviewPath::owned(preserved_preview.clone()).preserve();
        assert!(preserved_preview.is_file());

        let cleaned_preview = root.path().join("cleaned-preview.png");
        fs::write(&cleaned_preview, b"preview").expect("cleaned preview");
        drop(PreviewPath::owned(cleaned_preview.clone()));
        assert!(!cleaned_preview.exists());
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
    fn publishing_icon_validation_and_upscale_match_upstream_rules() {
        let dir = tempfile::tempdir().expect("tempdir");
        let icon_path = dir.path().join("icon.png");
        write_png(&icon_path, 128, 64);

        let icon = match WorkshopIcon::new(&icon_path, false) {
            Ok(icon) => icon,
            Err(error) => panic!("unexpected icon error: {error}"),
        };
        match icon.source {
            WorkshopIconSource::Custom { path, upscale } => {
                assert_eq!(path, icon_path);
                assert!(upscale.is_none());
            }
            WorkshopIconSource::Default => panic!("expected custom icon"),
        }

        let square_icon = dir.path().join("square.png");
        write_png(&square_icon, 512, 512);
        let square_icon = WorkshopIcon::new(&square_icon, true).expect("square icon");
        assert!(matches!(
            square_icon.source,
            WorkshopIconSource::Custom { upscale: None, .. }
        ));

        fs::write(dir.path().join("too-small.png"), [0_u8; 15]).expect("small icon");
        match WorkshopIcon::new(dir.path().join("too-small.png"), false) {
            Ok(_) => panic!("expected small icon error"),
            Err(error) => {
                use crate::error_key::HasErrorKey;
                assert_eq!(error.error_key().as_str(), "ERR_ICON_TOO_SMALL", "{error}");
            }
        }

        fs::write(
            dir.path().join("too-large.png"),
            vec![0_u8; WORKSHOP_ICON_MAX_SIZE as usize + 1],
        )
        .expect("large icon");
        match WorkshopIcon::new(dir.path().join("too-large.png"), false) {
            Ok(_) => panic!("expected large icon error"),
            Err(error) => {
                use crate::error_key::HasErrorKey;
                assert_eq!(error.error_key().as_str(), "ERR_ICON_TOO_LARGE", "{error}");
            }
        }

        fs::write(
            dir.path().join("icon.bmp"),
            vec![0_u8; WORKSHOP_ICON_MIN_SIZE as usize],
        )
        .expect("invalid format icon");
        match WorkshopIcon::new(dir.path().join("icon.bmp"), false) {
            Ok(_) => panic!("expected invalid format error"),
            Err(error) => {
                use crate::error_key::HasErrorKey;
                assert_eq!(
                    error.error_key().as_str(),
                    "ERR_ICON_INVALID_FORMAT",
                    "{error}"
                );
            }
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

        let default_preview = WorkshopIcon::default()
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

        let another_default = WorkshopIcon::default()
            .into_preview_path(&fixture.app_data)
            .expect("second default icon path");
        assert_ne!(default_path.as_path(), another_default.as_ref());

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

        // GIF is accepted by Steam but cannot be safely rewritten by this
        // path. Requesting upscale must therefore be represented as a plain
        // borrowed icon, never as an upscale operation with a format that the
        // resolver has to reject or mark unreachable.
        let gif_path = dir.path().join("animated.gif");
        write_gif(&gif_path, 32, 16);
        let gif_icon = WorkshopIcon::new(&gif_path, true).expect("valid gif icon");
        assert!(matches!(
            &gif_icon.source,
            WorkshopIconSource::Custom { upscale: None, .. }
        ));
        let gif_preview = gif_icon
            .into_preview_path(&fixture.app_data)
            .expect("gif preview path");
        assert_eq!(gif_preview.as_ref(), gif_path.as_path());
    }

    #[test]
    fn sanitize_description_strips_nuls_and_refuses_oversize_input() {
        assert_eq!(
            sanitize_description("safe [b]markup[/b]").expect("within limits"),
            "safe [b]markup[/b]"
        );
        assert_eq!(
            sanitize_description("interior\0nul").expect("nuls are dropped"),
            "interiornul"
        );
        let oversize = "a".repeat(WORKSHOP_DESCRIPTION_MAX_BYTES + 1);
        assert!(matches!(
            sanitize_description(&oversize),
            Err(PublishError::DescriptionTooLong)
        ));
        let exactly_at_limit = "a".repeat(WORKSHOP_DESCRIPTION_MAX_BYTES);
        assert!(sanitize_description(&exactly_at_limit).is_ok());
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
        assert_eq!(WORKSHOP_DEFAULT_DESCRIPTION, "No description provided");
        assert_eq!(
            publish_update_status(steamworks::UpdateStatus::Invalid),
            None
        );
        assert_eq!(
            publish_update_status(steamworks::UpdateStatus::PreparingConfig),
            Some(TransactionStatus::PublishPreparingConfig)
        );
        assert_eq!(
            publish_update_status(steamworks::UpdateStatus::PreparingContent),
            Some(TransactionStatus::PublishPreparingContent)
        );
        assert_eq!(
            publish_update_status(steamworks::UpdateStatus::UploadingContent),
            Some(TransactionStatus::PublishUploadingContent)
        );
        assert_eq!(
            publish_update_status(steamworks::UpdateStatus::UploadingPreviewFile),
            Some(TransactionStatus::PublishUploadingPreviewFile)
        );
        assert_eq!(
            publish_update_status(steamworks::UpdateStatus::CommittingChanges),
            Some(TransactionStatus::PublishCommittingChanges)
        );
    }

    #[test]
    fn publish_temp_dir_is_unique_per_call_and_confined_to_configured_temp_dir() {
        let dir = tempfile::tempdir().expect("tempdir");
        let configured_temp = dir.path().join("configured-temp");
        fs::create_dir_all(&configured_temp).expect("configured temp dir");
        let fixture = Fixture::with_publishing_settings(&configured_temp, &[]);

        let first = publish_temp_dir(&fixture.app_data, None);
        let second = publish_temp_dir(&fixture.app_data, None);

        assert_ne!(first, second, "each call must derive a fresh temp name");
        assert_eq!(
            first.parent(),
            Some(configured_temp.as_path()),
            "publish temps must live directly under the configured temp dir, never a parent of it"
        );
        assert_eq!(second.parent(), Some(configured_temp.as_path()));
    }

    #[test]
    fn a_submissions_own_temp_dir_wins_over_the_stored_setting() {
        let dir = tempfile::tempdir().expect("tempdir");
        let stored = dir.path().join("stored-temp");
        let submitted = dir.path().join("submitted-temp");
        for path in [&stored, &submitted] {
            fs::create_dir_all(path).expect("temp dir");
        }
        let fixture = Fixture::with_publishing_settings(&stored, &[]);
        let snapshot = PublishSettingsSnapshot {
            temp: Some(submitted.clone()),
            ignore_globs: Vec::new(),
        };

        assert_eq!(
            publish_temp_dir(&fixture.app_data, Some(&snapshot)).parent(),
            Some(submitted.as_path())
        );
        assert_eq!(
            publish_temp_dir(&fixture.app_data, None).parent(),
            Some(stored.as_path()),
            "a submission without a snapshot still follows the stored setting"
        );
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
        let transaction_id = transaction.id();

        let result = submit_with_transaction(
            PublishSubmission {
                language: crate::WorkshopLanguage::English,
                content_path_src: dir.path().join("content"),
                icon_path: Some(icon_path),
                title: "Boundary Proof".to_string(),
                tags: vec!["fun".to_string()],
                addon_type: "tool".to_string(),
                upscale: false,
                description: None,
                mode: PublishSubmissionMode::Update {
                    id: workshop_id(123),
                    changes: Some("icon check".to_string()),
                },
                settings: Some(PublishSettingsSnapshot {
                    temp: Some(publish_temp.clone()),
                    ignore_globs,
                }),
            },
            &transaction,
            &fixture.app_data,
            &fixture.steam,
            &fixture.whitelist,
            &crate::execution::CpuExecutor::build(2).expect("test CPU executor"),
        );

        assert!(matches!(result, Err(PublishError::IconInvalidFormat)));
        assert_eq!(
            fixture.app_data.settings().temp,
            Some(dir.path().to_owned())
        );
        assert!(fixture.app_data.settings().ignore_globs.is_empty());
        assert!(
            publish_temp.is_dir(),
            "the submission's temp directory is still created"
        );

        {
            let events = fixture.collector.drain();
            assert!(events.iter().any(|event| matches!(
                event,
                crate::events::BackendEvent::Transaction(
                    crate::events::TransactionEvent::Status { id, status }
                ) if *id == transaction_id
                    && *status == TransactionStatus::PublishProcessingIcon
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
