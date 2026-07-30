use gmpublished_backend::error_keys as keys;

use crate::bridge::domain::SteamId;

use super::{
    PublishSelectedPreview, PublishSubmitMode, PublishSubmitPreview, PublishSubmitRequest,
    PublishedFileId, SearchFullBatch, SearchFullRequest, SearchHit, SearchItem, SearchItemSource,
    SearchQuickBatch, SearchQuickRequest, SteamUser, UiError, WorkshopItem,
};
use gmpublished_backend::SteamAvatarRgba;
use gmpublished_backend::SteamRuntimeUser;
use gmpublished_backend::TransactionPayload;
use gmpublished_backend::publishing as steam_publishing;
use gmpublished_backend::steam_users;
use std::collections::HashMap;
use std::path::PathBuf;

pub(super) fn subscription_counts_from_items(
    items: &[WorkshopItem],
) -> HashMap<PublishedFileId, u64> {
    items
        .iter()
        .filter(|item| !item.dead)
        .map(|item| (item.id, item.subscriptions))
        .collect()
}

pub(super) fn publish_submission_from_app_request(
    request: PublishSubmitRequest,
) -> steam_publishing::PublishSubmission {
    let (icon_path, upscale) = publish_preview_from_app_request(request.preview);
    // The changelog belongs to the update case, so it travels inside it.
    let mode = match request.mode {
        PublishSubmitMode::New => steam_publishing::PublishSubmissionMode::Create,
        PublishSubmitMode::Update { workshop_id } => {
            steam_publishing::PublishSubmissionMode::Update {
                id: workshop_id.into(),
                changes: request.changelog,
            }
        }
    };

    steam_publishing::PublishSubmission {
        content_path_src: request.content_source_path,
        icon_path,
        title: request.title,
        tags: request.tags,
        addon_type: request.addon_type,
        upscale,
        mode,
        settings: Some(steam_publishing::PublishSettingsSnapshot {
            temp: Some(request.temp_dir),
            ignore_globs: request.ignore_globs,
        }),
    }
}

pub(super) fn publish_preview_from_app_request(
    preview: Option<PublishSubmitPreview>,
) -> (Option<PathBuf>, bool) {
    match preview {
        Some(PublishSubmitPreview::Selected(PublishSelectedPreview { path, upscale })) => {
            (Some(path), upscale)
        }
        Some(PublishSubmitPreview::Default(_)) | None => (None, false),
    }
}

pub(super) fn search_quick_batch_from_backend(
    request: &SearchQuickRequest,
    result: &gmpublished_backend::QuickSearchResult,
) -> SearchQuickBatch {
    let hits = result.hits.iter().map(search_hit_from_backend).collect();
    let key = request.key().clone();
    SearchQuickBatch::new(key, hits, result.has_more)
}

pub(super) fn search_full_batch_from_transaction_payload(
    request: &SearchFullRequest,
    sequence: u64,
    payload: &TransactionPayload,
) -> Result<SearchFullBatch, UiError> {
    let TransactionPayload::SearchHits(hits) = payload else {
        return Err(UiError::new(keys::SEARCH_DATA_SHAPE));
    };
    Ok(SearchFullBatch::new(
        request.key().clone(),
        request.task_id(),
        sequence,
        hits.iter().map(search_hit_from_backend).collect(),
    ))
}

pub(super) fn search_hit_from_backend(hit: &gmpublished_backend::QuickSearchHit) -> SearchHit {
    SearchHit {
        score: hit.score,
        item: search_item_from_backend(&hit.item),
    }
}

pub(super) fn search_item_from_backend(item: &gmpublished_backend::SearchItem) -> SearchItem {
    SearchItem {
        label: item.label().to_owned(),
        terms: item.terms().to_vec(),
        timestamp: item.timestamp,
        len: item.len,
        source: search_item_source_from_backend(&item.source),
    }
}

pub(super) fn search_item_source_from_backend(
    source: &gmpublished_backend::SearchItemSource,
) -> SearchItemSource {
    match source {
        // The backend's own `PublishedFileId` already excludes zero (see
        // its `nonzero_workshop_id` helper), so every id it hands us
        // converts cleanly.
        gmpublished_backend::SearchItemSource::InstalledAddons(path, id) => {
            SearchItemSource::InstalledAddons(path.clone(), id.map(PublishedFileId::from))
        }
        gmpublished_backend::SearchItemSource::InstalledAddonFile {
            addon,
            entry_path,
            size_bytes,
            crc32,
        } => SearchItemSource::InstalledAddonFile {
            addon_path: addon.path.clone(),
            addon_title: addon.title.clone(),
            workshop_id: addon.workshop_id.map(PublishedFileId::from),
            entry_path: entry_path.clone(),
            size_bytes: *size_bytes,
            crc32: *crc32,
        },
        gmpublished_backend::SearchItemSource::MyWorkshop(id) => {
            SearchItemSource::MyWorkshop(PublishedFileId::from(*id))
        }
        gmpublished_backend::SearchItemSource::WorkshopItem(id) => {
            SearchItemSource::WorkshopItem(PublishedFileId::from(*id))
        }
    }
}

pub(super) fn steam_user_from_backend(user: SteamRuntimeUser) -> SteamUser {
    SteamUser {
        steamid: SteamId::new(user.steamid.raw()),
        name: user.name,
        avatar: user.avatar.and_then(avatar_from_backend),
        dead: user.dead,
    }
}

pub(super) fn steam_user_from_workshop_backend(user: steam_users::SteamUser) -> SteamUser {
    SteamUser {
        steamid: SteamId::new(user.steamid.raw()),
        name: user.name,
        avatar: user
            .avatar
            .map(SteamAvatarRgba::from)
            .and_then(avatar_from_backend),
        dead: user.dead,
    }
}

pub(super) fn workshop_item_from_backend(item: gmpublished_backend::WorkshopItem) -> WorkshopItem {
    WorkshopItem {
        id: PublishedFileId::from(item.id),
        title: item.title,
        owner: item.owner.map(steam_user_from_workshop_backend),
        steamid: item.steamid.map(|steamid| SteamId::new(steamid.raw())),
        time_created: item.time_created,
        time_updated: item.time_updated,
        description: item.description,
        score: item.score,
        tags: item.tags,
        preview_url: item.preview_url,
        subscriptions: item.subscriptions,
        local_file: item.local_file,
        dead: item.dead,
    }
}

pub(super) fn avatar_from_backend(
    avatar: SteamAvatarRgba,
) -> Option<crate::bridge::domain::AvatarRgba> {
    crate::bridge::domain::AvatarRgba::new(avatar.width, avatar.height, avatar.rgba)
}

#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub(super) struct ClearDirectoryError(#[from] std::io::Error);

impl gmpublished_backend::HasErrorKey for ClearDirectoryError {
    fn error_key(&self) -> gmpublished_backend::ErrorKey {
        gmpublished_backend::error_keys::IO_ERROR
    }

    fn error_detail(&self) -> Option<String> {
        Some(self.to_string())
    }
}

pub(super) fn clear_directory_contents(path: &std::path::Path) -> Result<(), ClearDirectoryError> {
    if !path.exists() {
        return Ok(());
    }
    let entries = path.read_dir()?;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            std::fs::remove_dir_all(&path)?;
        } else {
            std::fs::remove_file(&path)?;
        }
    }
    Ok(())
}
