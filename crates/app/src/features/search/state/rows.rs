//! Search result projection, selection actions, and thumbnail row state.

use super::*;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Selection {
    pub(crate) title: String,
    pub(crate) action: SelectionAction,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SelectionAction {
    InstalledAddon {
        path: PathBuf,
        workshop_id: Option<PublishedFileId>,
        preview_url: Option<String>,
    },
    MyWorkshop {
        workshop_id: PublishedFileId,
        title: String,
        tags: Vec<String>,
        preview_url: Option<String>,
    },
    SteamWorkshop {
        workshop_id: PublishedFileId,
    },
    InstalledAddonFile {
        addon_path: PathBuf,
        addon_title: String,
        workshop_id: Option<PublishedFileId>,
        entry_path: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Row {
    pub(super) id: usize,
    pub(super) title: String,
    pub(super) source: RowSource,
    pub(super) association: String,
    pub(super) workshop_id: Option<PublishedFileId>,
    pub(super) thumbnail_url: Option<String>,
    pub(super) thumbnail: RowThumbnail,
    pub(super) action: SelectionAction,
}

impl Row {
    pub(crate) const fn id(&self) -> usize {
        self.id
    }

    pub(crate) fn title(&self) -> &str {
        &self.title
    }

    pub(crate) fn association(&self) -> &str {
        &self.association
    }

    pub(crate) const fn thumbnail(&self) -> &RowThumbnail {
        &self.thumbnail
    }

    pub(crate) fn source_label_key(&self) -> &'static str {
        self.source.label_key()
    }

    pub(super) fn thumbnail_demand(
        &self,
        priority: thumbnail_demand::Priority,
    ) -> Option<thumbnail_demand::Demand> {
        if !matches!(self.thumbnail, RowThumbnail::Loading) {
            return None;
        }
        let preview_url = self.thumbnail_url.as_deref()?.trim();
        if preview_url.is_empty() {
            return None;
        }

        Some(thumbnail_demand::Demand {
            id: thumbnail_demand::DemandId::search_row(self.id),
            input: ThumbnailInput::from_url(preview_url),
            logical_max_edge: SEARCH_THUMBNAIL_MAX_EDGE,
            priority,
            capabilities: thumbnail_demand::DemandCapabilities::SURFACE,
        })
    }

    pub(super) fn apply_thumbnail_delivery(
        &mut self,
        delivery: &thumbnail_demand::Delivery,
    ) -> bool {
        if delivery.id.search_row_index() != Some(self.id) {
            return false;
        }

        self.thumbnail = match &delivery.result {
            thumbnail_demand::DeliveryResult::Ready(ready) => {
                RowThumbnail::Ready(ready.handle().clone())
            }
            // Search rows keep their spinner rather than a blurred placeholder.
            thumbnail_demand::DeliveryResult::Placeholder(_) => return false,
            thumbnail_demand::DeliveryResult::Failed { .. } => RowThumbnail::Dead,
        };
        true
    }

    pub(super) fn apply_metadata_patch(&mut self, patch: &MetadataPatch) -> bool {
        if self.workshop_id != Some(patch.workshop_id) {
            return false;
        }

        if self.thumbnail_url == patch.preview_url {
            if patch.preview_url.is_none() && matches!(self.thumbnail, RowThumbnail::Loading) {
                self.thumbnail = RowThumbnail::Dead;
                return true;
            }
            return false;
        }

        self.thumbnail_url.clone_from(&patch.preview_url);
        self.thumbnail = if self.thumbnail_url.is_some() {
            RowThumbnail::Loading
        } else {
            RowThumbnail::Dead
        };
        true
    }

    pub(super) fn settle_without_metadata(&mut self) -> bool {
        if self.thumbnail_url.is_some() || !matches!(self.thumbnail, RowThumbnail::Loading) {
            return false;
        }

        self.thumbnail = RowThumbnail::Dead;
        true
    }

    pub(super) fn invalidate_ready_thumbnail(&mut self) -> bool {
        if !matches!(self.thumbnail, RowThumbnail::Ready(_)) {
            return false;
        }

        self.thumbnail = if self
            .thumbnail_url
            .as_deref()
            .is_some_and(|url| !url.trim().is_empty())
        {
            RowThumbnail::Loading
        } else {
            RowThumbnail::Dead
        };
        true
    }
}

pub(super) fn rows_from_hits(hits: Vec<SearchHit>) -> Vec<Row> {
    hits.into_iter()
        .enumerate()
        .map(|(index, hit)| row_from_search_item(index, &hit.item))
        .collect()
}

pub(super) fn rows_from_full_hits(start: usize, hits: &SearchFullHits) -> Vec<Row> {
    let mut index = start;
    hits.map_rows(|_score, item| {
        let row = row_from_search_item(index, item);
        index += 1;
        row
    })
}

pub(super) fn row_from_search_item(index: usize, item: &SearchItem) -> Row {
    let title = item.label.clone();
    match &item.source {
        SearchItemSource::InstalledAddons(path, workshop_id) => Row {
            id: index,
            title,
            source: RowSource::InstalledAddons,
            association: path.to_string_lossy().into_owned(),
            workshop_id: *workshop_id,
            thumbnail_url: None,
            thumbnail: thumbnail_for_workshop_id(*workshop_id),
            action: SelectionAction::InstalledAddon {
                path: path.clone(),
                workshop_id: *workshop_id,
                preview_url: None,
            },
        },
        SearchItemSource::InstalledAddonFile {
            addon_path,
            addon_title,
            workshop_id,
            entry_path,
            ..
        } => Row {
            id: index,
            title,
            source: RowSource::InstalledAddonFile,
            association: format!("{entry_path} - {addon_title}"),
            workshop_id: *workshop_id,
            thumbnail_url: None,
            thumbnail: thumbnail_for_workshop_id(*workshop_id),
            action: SelectionAction::InstalledAddonFile {
                addon_path: addon_path.clone(),
                addon_title: addon_title.clone(),
                workshop_id: *workshop_id,
                entry_path: entry_path.clone(),
            },
        },
        SearchItemSource::MyWorkshop(id) => Row {
            id: index,
            title: title.clone(),
            source: RowSource::MyWorkshop,
            association: workshop_item_url(*id),
            workshop_id: Some(*id),
            thumbnail_url: None,
            thumbnail: RowThumbnail::Loading,
            action: SelectionAction::MyWorkshop {
                workshop_id: *id,
                title,
                tags: my_workshop_tags_from_terms(&item.terms, *id),
                preview_url: None,
            },
        },
        SearchItemSource::WorkshopItem(id) => Row {
            id: index,
            title,
            source: RowSource::SteamWorkshop,
            association: workshop_item_url(*id),
            workshop_id: Some(*id),
            thumbnail_url: None,
            thumbnail: RowThumbnail::Loading,
            action: SelectionAction::SteamWorkshop { workshop_id: *id },
        },
    }
}

fn thumbnail_for_workshop_id(workshop_id: Option<PublishedFileId>) -> RowThumbnail {
    if workshop_id.is_some() {
        RowThumbnail::Loading
    } else {
        RowThumbnail::Dead
    }
}

fn my_workshop_tags_from_terms(terms: &[impl AsRef<str>], id: PublishedFileId) -> Vec<String> {
    let id_term = id.to_string();
    terms
        .iter()
        .map(AsRef::as_ref)
        .filter(|term| *term != id_term.as_str())
        .map(str::to_owned)
        .collect()
}
