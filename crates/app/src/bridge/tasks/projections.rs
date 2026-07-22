use gmpublished_backend::error_key::keys;

use super::{
    HashMap, PathBuf, PublishSelectedPreview, PublishSubmitMode, PublishSubmitPreview,
    PublishSubmitRequest, PublishedFileId, SearchFullBatch, SearchFullRequest, SearchHit,
    SearchItem, SearchItemSource, SearchQuickBatch, SearchQuickRequest, SteamAvatarRgba,
    SteamRuntimeUser, SteamUser, TransactionPayload, UiError, WorkshopItem, steam_publishing,
    steam_users,
};

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
    let update_id = match request.mode {
        PublishSubmitMode::New => None,
        PublishSubmitMode::Update { workshop_id } => Some(workshop_id.get()),
    };

    steam_publishing::PublishSubmission {
        content_path_src: request.content_source_path,
        icon_path,
        title: request.title,
        tags: request.tags,
        addon_type: request.addon_type,
        upscale,
        update_id,
        changes: request.changelog,
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
        Some(PublishSubmitPreview::Selected(PublishSelectedPreview::Source { path, upscale })) => {
            (Some(path), upscale)
        }
        Some(PublishSubmitPreview::Default(_)) | None => (None, false),
    }
}

pub(super) fn search_quick_batch_from_backend(
    request: &SearchQuickRequest,
    result: &gmpublished_backend::search::QuickSearchResult,
) -> SearchQuickBatch {
    let hits = result.hits.iter().map(search_hit_from_backend).collect();
    let key = request.key().clone();
    let carry = request.carry().clone();
    SearchQuickBatch::new(key, hits, result.has_more, carry)
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

pub(super) fn search_hit_from_backend(
    hit: &gmpublished_backend::search::QuickSearchHit,
) -> SearchHit {
    SearchHit {
        score: hit.score,
        item: search_item_from_backend(&hit.item),
    }
}

pub(super) fn search_item_from_backend(
    item: &gmpublished_backend::search::SearchItem,
) -> SearchItem {
    SearchItem {
        label: item.label().to_owned(),
        terms: item.terms().to_vec(),
        timestamp: item.timestamp,
        len: item.len,
        source: search_item_source_from_backend(&item.source),
    }
}

pub(super) fn search_item_source_from_backend(
    source: &gmpublished_backend::search::SearchItemSource,
) -> SearchItemSource {
    match source {
        // The backend's own `PublishedFileId` already excludes zero (see
        // its `nonzero_workshop_id` helper), so every id it hands us
        // converts cleanly.
        gmpublished_backend::search::SearchItemSource::InstalledAddons(path, id) => {
            SearchItemSource::InstalledAddons(
                path.clone(),
                id.map(|id| {
                    PublishedFileId::new(id.0).expect("backend never stores a zero workshop id")
                }),
            )
        }
        gmpublished_backend::search::SearchItemSource::InstalledAddonFile {
            addon,
            entry_path,
            size_bytes,
            crc32,
        } => SearchItemSource::InstalledAddonFile {
            addon_path: addon.path.clone(),
            addon_title: addon.title.clone(),
            workshop_id: addon.workshop_id.map(|id| {
                PublishedFileId::new(id.0).expect("backend never stores a zero workshop id")
            }),
            entry_path: entry_path.clone(),
            size_bytes: *size_bytes,
            crc32: *crc32,
        },
        gmpublished_backend::search::SearchItemSource::MyWorkshop(id) => {
            SearchItemSource::MyWorkshop(
                PublishedFileId::new(id.0).expect("backend never stores a zero workshop id"),
            )
        }
        gmpublished_backend::search::SearchItemSource::WorkshopItem(id) => {
            SearchItemSource::WorkshopItem(
                PublishedFileId::new(id.0).expect("backend never stores a zero workshop id"),
            )
        }
    }
}

pub(super) fn steam_user_from_backend(user: SteamRuntimeUser) -> SteamUser {
    SteamUser {
        steamid: user.steamid.raw(),
        name: user.name,
        avatar: user.avatar.and_then(avatar_from_backend),
        dead: user.dead,
    }
}

pub(super) fn steam_user_from_workshop_backend(user: steam_users::SteamUser) -> SteamUser {
    SteamUser {
        steamid: user.steamid.raw(),
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
        id: PublishedFileId::new(item.id.0).expect("Steam never issues a zero published file id"),
        title: item.title,
        owner: item.owner.map(steam_user_from_workshop_backend),
        steamid: item.steamid.map(|steamid| steamid.raw()),
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

impl gmpublished_backend::error_key::HasErrorKey for ClearDirectoryError {
    fn error_key(&self) -> gmpublished_backend::error_key::ErrorKey {
        gmpublished_backend::error_key::keys::IO_ERROR
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
