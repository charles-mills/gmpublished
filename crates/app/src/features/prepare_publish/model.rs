use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use crate::backend::ui_error::UiError;
use crate::backend::{
    Settings,
    domain::PublishedFileId,
    publish::{
        DEFAULT_WORKSHOP_ICON_FILE_NAME, IconFormat, PublishSelectedPreview, PublishSubmitOutcome,
        PublishSubmitRequest,
    },
    tasks::{
        BackendContext, BackendRuntimeEvent, BackendServices, TaskHandle, TransactionRuntimeEvent,
    },
};
use ::image::GenericImageView;
use gmpublished_backend::error_key::keys;
use iced::widget::image;

use crate::{
    backend::gma::{ArchiveEntryPath, GmaMetaEntry, whitelist},
    media::{thumbnail_animation, thumbnail_worker::PreparedAnimation},
    widgets::file_browser,
};

const WORKSHOP_ICON_MAX_SIZE: u64 = 1_048_576;
const WORKSHOP_ICON_MIN_SIZE: u64 = 16;
const WORKSHOP_ICON_PREVIEW_MAX_EDGE: u32 = 512;
const MAX_WHITELIST_FAILURES: usize = 9;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContentPathVerificationRequest {
    pub(crate) generation: u64,
    pub(crate) display_path: String,
    pub(crate) path: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedContentPath {
    pub(crate) display_path: String,
    pub(crate) path: PathBuf,
    pub(crate) total_size: u64,
    pub(crate) entries: Vec<file_browser::Entry>,
    #[cfg(feature = "asset-studio")]
    pub(crate) preview_source: Arc<crate::backend::archive::PreviewArchiveSource>,
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
    pub(crate) generation: u64,
    pub(crate) display_path: String,
    pub(crate) path: PathBuf,
    pub(crate) temp_dir: PathBuf,
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

pub use crate::widgets::select_option::SelectOption;

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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublishSubmitContext {
    pub(crate) ignore_globs: Vec<String>,
    pub(crate) temp_dir: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublishSubmitRequestEnvelope {
    pub(crate) generation: u64,
    pub(crate) request: PublishSubmitRequest,
}

impl PublishSubmitRequestEnvelope {
    pub(crate) const fn initial_status(&self) -> &'static str {
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
    pub(crate) generation: u64,
    pub(crate) icon_source_path: PathBuf,
    pub(crate) upscale: bool,
    pub(crate) workshop_id: PublishedFileId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PublishIconSubmitResult {
    pub(crate) legal_agreement_required: bool,
}

pub fn verify_content_path(
    ctx: &BackendServices,
    request: ContentPathVerificationRequest,
) -> Result<Arc<VerifiedContentPath>, UiError> {
    let settings = ctx.settings_snapshot();
    verify_content_tree(
        request.display_path,
        request.path,
        &settings.ignore_globs,
        &ctx.whitelist_snapshot(),
    )
    .map(Arc::new)
}

pub fn verify_icon_preview(
    request: IconVerificationRequest,
) -> Result<Arc<VerifiedIconPreview>, UiError> {
    verify_icon_preview_local(request).map(Arc::new)
}

pub fn apply_ignore_pattern_mutation(
    ctx: &BackendServices,
    mutation: IgnorePatternMutation,
) -> IgnorePatternMutationResult {
    let mut changed = false;
    let mut save_error = None;
    if let Err(error) = ctx.update_settings_snapshot(|settings| match mutation {
        IgnorePatternMutation::Add(pattern) => {
            let pattern = pattern.trim();
            if !pattern.is_empty() && !settings.ignore_globs.iter().any(|glob| glob == pattern) {
                settings.ignore_globs.push(pattern.to_owned());
                changed = true;
            }
        }
        IgnorePatternMutation::Remove(pattern) => {
            let before = settings.ignore_globs.len();
            settings.ignore_globs.retain(|glob| glob != &pattern);
            changed = settings.ignore_globs.len() != before;
        }
    }) {
        save_error = Some(error.to_string());
    }
    let settings = ctx.settings_snapshot();

    IgnorePatternMutationResult {
        changed,
        ignored_patterns: ignored_patterns_from_settings(&settings),
        save_error,
    }
}

pub fn run_publish_submit(
    backend_ctx: &BackendContext,
    services: &BackendServices,
    connect_steam: impl FnOnce(&BackendServices) -> Result<(), UiError>,
    task: TaskHandle,
    request: PublishSubmitRequest,
) -> Result<PublishSubmitResult, UiError> {
    task.status("PUBLISH_STARTING");
    if let Err(error) = connect_steam(services) {
        task.error(error.clone());
        return Err(error);
    }

    let transaction = services.begin_transaction();
    let transaction_id = transaction.id;
    backend_ctx.correlate_backend_transaction(transaction_id, task);

    match services.submit_publish_request(request, &transaction) {
        Ok(outcome) => {
            let _effects = backend_ctx.handle_backend_runtime_event(
                &BackendRuntimeEvent::Transaction(TransactionRuntimeEvent::Finished {
                    id: transaction_id,
                    payload: gmpublished_backend::events::TransactionPayload::None,
                }),
            );
            Ok(outcome.into())
        }
        Err(error) => {
            let _handled =
                backend_ctx.error_backend_transaction_task(transaction_id, error.clone());
            Err(error)
        }
    }
}

pub fn run_publish_icon_submit(
    backend_ctx: &BackendContext,
    services: &BackendServices,
    connect_steam: impl FnOnce(&BackendServices) -> Result<(), UiError>,
    task: TaskHandle,
    request: &PublishIconSubmitRequestEnvelope,
) -> Result<PublishIconSubmitResult, UiError> {
    task.status("PUBLISH_PROCESSING_ICON");
    if let Err(error) = connect_steam(services) {
        task.error(error.clone());
        return Err(error);
    }

    let transaction = services.begin_transaction();
    let transaction_id = transaction.id;
    backend_ctx.correlate_backend_transaction(transaction_id, task);

    match services.submit_publish_icon_request(
        &request.icon_source_path,
        request.upscale,
        request.workshop_id,
        &transaction,
    ) {
        Ok(legal_agreement_required) => {
            let _effects = backend_ctx.handle_backend_runtime_event(
                &BackendRuntimeEvent::Transaction(TransactionRuntimeEvent::Finished {
                    id: transaction_id,
                    payload: gmpublished_backend::events::TransactionPayload::None,
                }),
            );
            Ok(PublishIconSubmitResult {
                legal_agreement_required,
            })
        }
        Err(error) => {
            let _handled =
                backend_ctx.error_backend_transaction_task(transaction_id, error.clone());
            Err(error)
        }
    }
}

pub fn ignored_patterns_from_settings(settings: &Settings) -> Vec<IgnoredPattern> {
    let mut patterns = Vec::with_capacity(
        settings
            .ignore_globs
            .len()
            .saturating_add(whitelist::DEFAULT_IGNORE.len()),
    );
    patterns.extend(settings.ignore_globs.iter().map(|pattern| IgnoredPattern {
        pattern: pattern.clone(),
        default_pattern: false,
    }));
    let mut default_patterns = whitelist::DEFAULT_IGNORE.to_vec();
    default_patterns.sort_unstable();
    patterns.extend(default_patterns.into_iter().map(|pattern| IgnoredPattern {
        pattern: pattern.to_owned(),
        default_pattern: true,
    }));
    patterns
}

pub fn default_icon_path(temp_dir: &Path) -> PathBuf {
    temp_dir.join(DEFAULT_WORKSHOP_ICON_FILE_NAME)
}

pub fn publish_selected_preview(icon: &VerifiedIcon, upscale_icon: bool) -> PublishSelectedPreview {
    PublishSelectedPreview::Source {
        path: icon.path.clone(),
        upscale: upscale_icon && icon.can_upscale,
    }
}

fn verify_content_tree(
    display_path: String,
    path: PathBuf,
    ignore_globs: &[String],
    whitelist_snapshot: &[String],
) -> Result<VerifiedContentPath, UiError> {
    if !path.is_dir() || !path.is_absolute() {
        return Err(UiError::new(keys::INVALID_CONTENT_PATH));
    }

    let mut state = ContentCollectionState::default();
    collect_content_entries(&path, &path, ignore_globs, whitelist_snapshot, &mut state)?;
    let ContentCollectionState {
        entries,
        total_size,
        mut failed,
        duplicate,
        ..
    } = state;

    if let Some(duplicate) = duplicate {
        return Err(UiError::detailed(keys::DUPLICATE_ENTRIES, Some(duplicate)));
    }
    if !failed.is_empty() {
        failed.sort_unstable();
        if failed.len() > MAX_WHITELIST_FAILURES {
            failed.truncate(MAX_WHITELIST_FAILURES);
            failed.push("...".to_owned());
        }
        return Err(UiError::detailed(keys::WHITELIST, Some(failed.join("\n"))));
    }
    if entries.is_empty() {
        return Err(UiError::new(keys::NO_ENTRIES));
    }

    let browser_entries = entries
        .iter()
        .map(|(entry, _)| file_browser_entry(entry))
        .collect::<Result<Vec<_>, _>>()?;

    #[cfg(feature = "asset-studio")]
    let preview_source = crate::backend::archive::PreviewArchiveSource::from_folder(
        entries
            .into_iter()
            .map(|(entry, disk_path)| (entry.path, entry.size, disk_path)),
    );

    Ok(VerifiedContentPath {
        display_path,
        path,
        total_size,
        entries: browser_entries,
        #[cfg(feature = "asset-studio")]
        preview_source,
    })
}

/// Accumulator state threaded through the recursive directory walk in
/// [`collect_content_entries`]. Bundled into one struct because these fields
/// are mutated together at every recursion depth, and each is written from
/// multiple call sites rather than just one — passing them individually
/// would mean a long, unwieldy parameter list threaded through every
/// recursive call for no benefit.
#[derive(Default)]
struct ContentCollectionState {
    entries: Vec<(GmaMetaEntry, PathBuf)>,
    total_size: u64,
    failed: Vec<String>,
    duplicate: Option<String>,
    seen: HashSet<String>,
}

fn collect_content_entries(
    root: &Path,
    dir: &Path,
    ignore_globs: &[String],
    whitelist_snapshot: &[String],
    state: &mut ContentCollectionState,
) -> Result<(), UiError> {
    let read_dir = dir.read_dir().map_err(|_| UiError::new(keys::IO_ERROR))?;
    for entry in read_dir {
        let entry = entry.map_err(|_| UiError::new(keys::IO_ERROR))?;
        let file_type = entry
            .file_type()
            .map_err(|_| UiError::new(keys::IO_ERROR))?;
        let path = entry.path();
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            collect_content_entries(root, &path, ignore_globs, whitelist_snapshot, state)?;
            continue;
        }
        if !file_type.is_file() {
            continue;
        }

        let relative_path = relative_slash_path(root, &path)?;
        if whitelist::is_default_ignored(&relative_path)
            || whitelist::is_ignored(&relative_path, ignore_globs)
        {
            continue;
        }
        if !state.seen.insert(relative_path.clone()) {
            state.duplicate = Some(relative_path);
            continue;
        }
        if !whitelist::is_whitelisted_in(whitelist_snapshot, &relative_path) {
            state.failed.push(relative_path);
            continue;
        }
        if state.failed.is_empty() {
            let size = path
                .metadata()
                .map(|metadata| metadata.len())
                .unwrap_or_default();
            state.total_size = state.total_size.saturating_add(size);
            state.entries.push((
                GmaMetaEntry {
                    path: relative_path,
                    size,
                    crc32: 0,
                },
                path,
            ));
        }
    }
    Ok(())
}

fn relative_slash_path(root: &Path, path: &Path) -> Result<String, UiError> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| UiError::new(keys::IO_ERROR))?;
    let mut output = String::new();
    for component in relative.components() {
        let std::path::Component::Normal(component) = component else {
            continue;
        };
        let component = component.to_string_lossy().to_lowercase();
        if !component.is_empty() {
            if !output.is_empty() {
                output.push('/');
            }
            output.push_str(&component);
        }
    }
    Ok(output)
}

fn file_browser_entry(entry: &GmaMetaEntry) -> Result<file_browser::Entry, UiError> {
    let Some(path) = ArchiveEntryPath::from_validated(entry.path.clone()) else {
        log::warn!("Prepare Publish verifier returned an invalid archive path");
        return Err(UiError::new(keys::UNKNOWN));
    };
    Ok(file_browser::Entry::from_archive_path(path, entry.size))
}

fn verify_icon_preview_local(
    request: IconVerificationRequest,
) -> Result<VerifiedIconPreview, UiError> {
    let metadata = request
        .path
        .metadata()
        .map_err(|_| UiError::new(keys::IO_ERROR))?;
    if !metadata.is_file() {
        return Err(UiError::new(keys::IO_ERROR));
    }
    if metadata.len() > WORKSHOP_ICON_MAX_SIZE {
        return Err(UiError::new(keys::ICON_TOO_LARGE));
    }
    if metadata.len() < WORKSHOP_ICON_MIN_SIZE {
        return Err(UiError::new(keys::ICON_TOO_SMALL));
    }

    let format = IconFormat::try_from(request.path.as_path())?;
    match format {
        IconFormat::Gif => verify_gif_icon(request, metadata.len()),
        IconFormat::Png | IconFormat::Jpeg => verify_still_icon(request, metadata.len(), format),
    }
}

fn verify_still_icon(
    request: IconVerificationRequest,
    byte_size: u64,
    format: IconFormat,
) -> Result<VerifiedIconPreview, UiError> {
    let image = ::image::open(&request.path).map_err(|error| {
        log::warn!("Prepare Publish icon decode failed: {error}");
        UiError::detailed(keys::IMAGE_ERROR, Some(error.to_string()))
    })?;
    let (source_width, source_height) = image.dimensions();
    let can_upscale = icon_can_upscale(source_width, source_height, format);

    let (rgba, width, height) = display_preview_rgba(&image);
    let icon = VerifiedIcon {
        display_path: request.display_path,
        source_path: request.path.clone(),
        path: request.path,
        format,
        width,
        height,
        byte_size,
        can_upscale,
    };

    let backdrop =
        crate::media::backdrop::bake_blurred_backdrop(width, height, &rgba, request.well_rgb);
    let still = image::Handle::from_rgba(width, height, rgba);
    Ok(VerifiedIconPreview {
        backdrop: backdrop.unwrap_or_else(|| still.clone()),
        still,
        icon,
        animation: None,
    })
}

fn verify_gif_icon(
    request: IconVerificationRequest,
    byte_size: u64,
) -> Result<VerifiedIconPreview, UiError> {
    let bytes = fs::read(&request.path).map_err(|_| UiError::new(keys::IO_ERROR))?;
    let animation = PreparedAnimation::from_encoded_gif(&bytes, WORKSHOP_ICON_PREVIEW_MAX_EDGE)
        .map_err(|error| {
            log::warn!("Prepare Publish GIF icon preview could not be baked: {error}");
            UiError::detailed(keys::IMAGE_ERROR, Some(error.to_string()))
        })?;
    let backdrop = animation.frames().first().and_then(|frame| {
        crate::media::backdrop::bake_blurred_backdrop(
            frame.width(),
            frame.height(),
            frame.rgba_bytes(),
            request.well_rgb,
        )
    });
    let frames = animation
        .frames()
        .iter()
        .map(|frame| {
            (
                image::Handle::from_rgba(
                    frame.width(),
                    frame.height(),
                    frame.rgba_bytes().to_vec(),
                ),
                frame.delay(),
                frame.width(),
                frame.height(),
            )
        })
        .collect::<Vec<_>>();
    let Some((still, _delay, width, height)) = frames.first().cloned() else {
        return Err(UiError::new(keys::IMAGE_ERROR));
    };
    let animation = thumbnail_animation::Playback::from_frame_handles(
        frames
            .into_iter()
            .map(|(handle, delay, _, _)| (handle, nonzero_delay(delay))),
    )
    .ok_or_else(|| UiError::new(keys::IMAGE_ERROR))?;

    let icon = VerifiedIcon {
        display_path: request.display_path,
        source_path: request.path.clone(),
        path: request.path,
        format: IconFormat::Gif,
        width,
        height,
        byte_size,
        can_upscale: false,
    };

    Ok(VerifiedIconPreview {
        icon,
        backdrop: backdrop.unwrap_or_else(|| still.clone()),
        still,
        animation: Some(animation),
    })
}

/// Downscales oversized sources for the on-screen preview only; submit and
/// upload always read the original file. Matches the GIF path, which already
/// bakes at WORKSHOP_ICON_PREVIEW_MAX_EDGE.
fn display_preview_rgba(image: &::image::DynamicImage) -> (Vec<u8>, u32, u32) {
    let (width, height) = image.dimensions();
    if width <= WORKSHOP_ICON_PREVIEW_MAX_EDGE && height <= WORKSHOP_ICON_PREVIEW_MAX_EDGE {
        return (image.to_rgba8().into_raw(), width, height);
    }

    // Triangle is plenty for a display-only downscale and much cheaper than
    // CatmullRom at 4K-source sizes; the submitted file is never resampled here.
    let resized = image.resize(
        WORKSHOP_ICON_PREVIEW_MAX_EDGE,
        WORKSHOP_ICON_PREVIEW_MAX_EDGE,
        ::image::imageops::FilterType::Triangle,
    );
    let (width, height) = resized.dimensions();
    (resized.to_rgba8().into_raw(), width, height)
}

fn icon_can_upscale(width: u32, height: u32, format: IconFormat) -> bool {
    format != IconFormat::Gif && ((width < 512 || height < 512) || width != height)
}

impl From<PublishSubmitOutcome> for PublishSubmitResult {
    fn from(outcome: PublishSubmitOutcome) -> Self {
        Self {
            published_file_id: outcome.published_file_id,
            legal_agreement_required: outcome.legal_agreement_required,
        }
    }
}

fn nonzero_delay(delay: Duration) -> Duration {
    if delay.is_zero() {
        Duration::from_millis(1)
    } else {
        delay
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::backend::tasks::BackendServices;
    use crate::test_support::TestDir;

    #[test]
    fn ignored_patterns_keep_user_order_then_defaults_alphabetical() {
        let settings = Settings {
            ignore_globs: vec!["zzz/*".to_owned(), "aaa.txt".to_owned()],
            ..Settings::default()
        };

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

        let verified = verify_content_path(
            &BackendServices::for_test(),
            ContentPathVerificationRequest {
                generation: 1,
                display_path: root.path_text(),
                path: root.path().to_path_buf(),
            },
        )
        .expect("content path should verify");

        let browser = file_browser::State::from_entries(verified.entries.iter().cloned());
        assert_eq!(verified.total_size, 14);
        assert_eq!(browser.rows()[0].shortcut_prefix.as_deref(), Some("lua/"));
        assert_eq!(browser.rows()[0].display_name, "autorun");
    }

    #[test]
    fn verify_content_path_rejects_relative_paths() {
        let result = verify_content_path(
            &BackendServices::for_test(),
            ContentPathVerificationRequest {
                generation: 1,
                display_path: "relative".to_owned(),
                path: PathBuf::from("relative"),
            },
        );

        assert!(result.is_err());
    }

    #[test]
    fn verify_icon_preview_maps_png_to_still_handle() {
        let root = TestDir::new("prepare-publish-icon-png");
        let source = root.image("icon.png", ::image::ImageFormat::Png, 32, 48);

        let verified = verify_icon_preview(IconVerificationRequest {
            generation: 1,
            display_path: source.to_string_lossy().into_owned(),
            path: source.clone(),
            temp_dir: root.path().join("temp"),
            well_rgb: [0x10, 0x10, 0x10],
        })
        .expect("png icon should verify");

        assert_eq!((verified.icon.width, verified.icon.height), (32, 48));
        assert_eq!(verified.icon.source_path, source);
        assert!(verified.animation.is_none());
    }

    #[test]
    fn oversized_square_icon_previews_at_display_resolution() {
        let root = TestDir::new("prepare-publish-icon-big");
        let source = root.image("icon.png", ::image::ImageFormat::Png, 1024, 1024);

        let verified = verify_icon_preview(IconVerificationRequest {
            generation: 1,
            display_path: source.to_string_lossy().into_owned(),
            path: source.clone(),
            temp_dir: root.path().join("temp"),
            well_rgb: [0x10, 0x10, 0x10],
        })
        .expect("big square icon should verify");

        // Display preview is bounded; submit still reads the original file.
        assert_eq!((verified.icon.width, verified.icon.height), (512, 512));
        assert_eq!(verified.icon.path, source);
        assert_eq!(verified.icon.source_path, source);
    }
}
