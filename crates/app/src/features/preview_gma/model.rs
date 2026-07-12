use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use crate::backend::domain::{
    AvatarRgba, PublishedFileId, SteamUser, WorkshopItem, workshop_url::workshop_item_url,
};

use crate::backend::gma::PreviewArchive;
use crate::backend::tasks::BackendServices;
use crate::backend::ui_error::UiError;
use crate::features::steam_session;
use crate::widgets::file_browser::{Entry as FileBrowserEntry, State as FileBrowserState};
use gmpublished_backend::error_key::keys;

/// Data the click source already rendered, seeded for the first frame.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct OpenSeed {
    pub(crate) preview_url: Option<String>,
    pub(crate) subscription_count: Option<u64>,
    pub(crate) score_bucket: Option<i32>,
    pub(crate) score_label: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OpenTarget {
    pub(crate) path: PathBuf,
    pub(crate) title: String,
    pub(crate) workshop_id: Option<PublishedFileId>,
    pub(crate) seed: OpenSeed,
    pub(crate) initial_entry_preview: Option<String>,
}

impl OpenTarget {
    pub(crate) fn new(
        path: PathBuf,
        title: impl Into<String>,
        workshop_id: Option<PublishedFileId>,
    ) -> Self {
        Self {
            path,
            title: title.into(),
            workshop_id,
            seed: OpenSeed::default(),
            initial_entry_preview: None,
        }
    }

    pub(crate) fn with_seed(mut self, seed: OpenSeed) -> Self {
        self.seed = seed;
        self
    }

    pub(crate) fn with_initial_entry_preview(mut self, entry_path: impl Into<String>) -> Self {
        self.initial_entry_preview = Some(entry_path.into());
        self
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OpenRequest {
    pub(crate) request_id: u64,
    pub(crate) path: PathBuf,
    /// Resolved workshop id (click source or path inference); stamps the
    /// archive so its `extracted_name` carries the `<title>_<id>` suffix.
    pub(crate) workshop_id: Option<PublishedFileId>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MetadataRequest {
    pub(crate) request_id: u64,
    pub(crate) workshop_id: PublishedFileId,
}

/// Author lookup request emitted when metadata carries only a steamid.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthorRequest {
    pub(crate) request_id: u64,
    pub(crate) steamid64: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthorInfo {
    pub(crate) name: String,
    pub(crate) avatar: Option<AvatarRgba>,
}

/// Renders a steamid64 in the classic Steam2 form, shown as a placeholder
/// while the author lookup is in flight.
pub fn steam2_rendered_id(steamid64: u64) -> String {
    const ACCOUNT_OFFSET: u64 = 76_561_197_960_265_728;
    let account = steamid64.saturating_sub(ACCOUNT_OFFSET);
    format!("STEAM_1:{}:{}", account & 1, account >> 1)
}

pub fn query_steam_user_streaming(
    ctx: &BackendServices,
    steamid64: u64,
    mut on_author: impl FnMut(Result<AuthorInfo, UiError>),
) -> Result<(), UiError> {
    ctx.steam_user_details_streaming(steamid64, |user| {
        on_author(author_info_from_user(user));
    })
}

fn author_info_from_user(user: SteamUser) -> Result<AuthorInfo, UiError> {
    if user.dead {
        return Err(UiError::new(keys::STEAM_ERROR));
    }
    let name = user.name.trim().to_owned();
    if name.is_empty() {
        return Err(UiError::new(keys::STEAM_ERROR));
    }
    Ok(AuthorInfo {
        name,
        avatar: user.avatar,
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExtractionIntent {
    Archive { total_bytes: u64 },
    Entry { path: String, size_bytes: u64 },
}

impl ExtractionIntent {
    pub(crate) const fn total_bytes(&self) -> u64 {
        match self {
            Self::Archive { total_bytes } => *total_bytes,
            Self::Entry { size_bytes, .. } => *size_bytes,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExtractionRequest {
    pub(crate) request_id: u64,
    pub(crate) archive: Arc<PreviewArchive>,
    pub(crate) intent: ExtractionIntent,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkshopMetadata {
    pub(crate) id: PublishedFileId,
    pub(crate) title: String,
    pub(crate) author: Option<String>,
    pub(crate) steamid64: Option<u64>,
    pub(crate) avatar: Option<AvatarRgba>,
    pub(crate) time_created: u32,
    pub(crate) time_updated: u32,
    pub(crate) description: String,
    pub(crate) tags: Vec<String>,
    pub(crate) preview_url: Option<String>,
    pub(crate) subscriptions: u64,
    pub(crate) score_bucket: i32,
    pub(crate) score_label: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LoadedArchive {
    pub(crate) archive: Arc<PreviewArchive>,
    pub(crate) browser: FileBrowserState,
}

impl LoadedArchive {
    pub(crate) fn open_path(
        path: &Path,
        workshop_id: Option<PublishedFileId>,
    ) -> Result<Self, UiError> {
        PreviewArchive::open_with_workshop_id(path, workshop_id.map(PublishedFileId::get))
            .map(Self::from_archive)
            .map_err(|error| UiError::from(&error))
    }

    pub(crate) fn from_archive(archive: PreviewArchive) -> Self {
        let entries = archive
            .entries_owned()
            .into_iter()
            .map(|entry| FileBrowserEntry::from_archive_path(entry.path, entry.size));
        Self {
            archive: Arc::new(archive),
            browser: FileBrowserState::from_entries(entries),
        }
    }
}

pub fn query_workshop_metadata(
    ctx: &BackendServices,
    workshop_id: PublishedFileId,
) -> Result<Option<WorkshopMetadata>, UiError> {
    let attempt = steam_session::connect_context_for_operation(ctx);
    if !attempt.connected() {
        return Err(attempt
            .error()
            .cloned()
            .unwrap_or_else(|| UiError::new(keys::STEAM_ERROR)));
    }

    let item = ctx.workshop_item_details(workshop_id)?;
    // Author resolution stays asynchronous: when the item lacks a live owner
    // the modal shows the Steam2 placeholder and fetches the profile
    // separately.
    let owner = item.owner.as_ref().filter(|owner| !owner.dead).cloned();

    Ok(workshop_metadata_from_item(item, owner))
}

pub fn cached_workshop_metadata(
    ctx: &BackendServices,
    workshop_id: PublishedFileId,
) -> Option<WorkshopMetadata> {
    let metadata = ctx.cached_workshop_item_details(workshop_id)?;
    Some(WorkshopMetadata {
        id: metadata.id,
        title: metadata.title.trim().to_owned(),
        author: None,
        steamid64: metadata.owner_steamid,
        avatar: None,
        time_created: metadata.time_created,
        time_updated: metadata.time_updated,
        description: metadata.full_description.unwrap_or_default(),
        tags: metadata.tags,
        preview_url: metadata.preview_url,
        subscriptions: metadata.subscriptions,
        score_bucket: score_bucket(metadata.score),
        score_label: score_label(metadata.score),
    })
}

pub fn workshop_url(workshop_id: PublishedFileId) -> String {
    workshop_item_url(workshop_id)
}

fn workshop_metadata_from_item(
    item: WorkshopItem,
    owner: Option<SteamUser>,
) -> Option<WorkshopMetadata> {
    if item.dead {
        return None;
    }

    let description = item
        .description
        .as_deref()
        .map(str::trim)
        .filter(|description| !description.is_empty())
        .unwrap_or_default()
        .to_owned();
    let preview_url = item
        .preview_url
        .as_deref()
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .map(str::to_owned);
    let tags = item
        .tags
        .into_iter()
        .map(|tag| tag.trim().to_owned())
        .filter(|tag| !tag.is_empty())
        .collect();
    let author = owner.as_ref().and_then(live_user_name);
    let avatar = owner.and_then(|owner| owner.avatar);
    let steamid64 = item.steamid;

    Some(WorkshopMetadata {
        id: item.id,
        steamid64,
        title: item.title.trim().to_owned(),
        author,
        avatar,
        time_created: item.time_created,
        time_updated: item.time_updated,
        description,
        tags,
        preview_url,
        subscriptions: item.subscriptions,
        score_bucket: score_bucket(item.score),
        score_label: score_label(item.score),
    })
}

fn live_user_name(user: &SteamUser) -> Option<String> {
    if user.dead {
        return None;
    }

    let name = user.name.trim();
    (!name.is_empty()).then(|| name.to_owned())
}

fn score_bucket(score: f32) -> i32 {
    (score.clamp(0.0, 1.0) * 5.0).round() as i32
}

fn score_label(score: f32) -> String {
    format!("{:.2}%", score.clamp(0.0, 1.0) * 100.0)
}
