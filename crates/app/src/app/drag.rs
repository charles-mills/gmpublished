use super::{
    ADDON_DRAG_THRESHOLD, App, Event, MAX_DROPPED_TEXT_BYTES, Path, Point, PublishedFileId,
    RootMessage, Task, WORKSHOP_DRAG_PREFIX, addon_grid, downloader, event, fs, installed_addons,
    my_workshop, shell, size_analyzer, window, workshop_url,
};
use iced::widget::image;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AddonDragSource {
    MyWorkshop,
    InstalledAddons,
    SizeAnalyzer,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct AddonDragState {
    phase: AddonDragPhase,
}

impl AddonDragState {
    pub(super) fn press(
        &mut self,
        source: AddonDragSource,
        card_id: String,
        workshop_id: Option<PublishedFileId>,
        thumbnail: Option<image::Handle>,
    ) {
        self.phase = AddonDragPhase::Pending {
            source,
            card_id,
            workshop_id,
            thumbnail,
            origin: None,
        };
    }

    pub(super) fn cursor_moved(&mut self, position: Point) {
        let phase = std::mem::take(&mut self.phase);
        self.phase = match phase {
            AddonDragPhase::Pending {
                source,
                card_id,
                workshop_id,
                thumbnail,
                origin: None,
            } => AddonDragPhase::Pending {
                source,
                card_id,
                workshop_id,
                thumbnail,
                origin: Some(position),
            },
            AddonDragPhase::Pending {
                source,
                card_id,
                workshop_id: Some(workshop_id),
                thumbnail,
                origin: Some(origin),
            } if moved_beyond_drag_threshold(origin, position) => AddonDragPhase::Dragging {
                source,
                card_id,
                workshop_id,
                thumbnail,
                origin,
                cursor: position,
            },
            AddonDragPhase::Pending {
                source,
                card_id,
                workshop_id,
                thumbnail,
                origin,
            } => AddonDragPhase::Pending {
                source,
                card_id,
                workshop_id,
                thumbnail,
                origin,
            },
            AddonDragPhase::Dragging {
                source,
                card_id,
                workshop_id,
                thumbnail,
                origin,
                ..
            } => AddonDragPhase::Dragging {
                source,
                card_id,
                workshop_id,
                thumbnail,
                origin,
                cursor: position,
            },
            AddonDragPhase::Idle => AddonDragPhase::Idle,
        };
    }

    pub(super) fn release(&mut self, over_downloader: bool) -> Option<AddonDragOutcome> {
        match std::mem::take(&mut self.phase) {
            AddonDragPhase::Idle => None,
            AddonDragPhase::Pending {
                source, card_id, ..
            } => Some(AddonDragOutcome::Click { source, card_id }),
            AddonDragPhase::Dragging { workshop_id, .. } if over_downloader => {
                Some(AddonDragOutcome::Drop { workshop_id })
            }
            AddonDragPhase::Dragging { .. } => Some(AddonDragOutcome::Cancelled),
        }
    }

    fn cancel(&mut self) -> Option<AddonDragOutcome> {
        if self.is_active() {
            self.phase = AddonDragPhase::Idle;
            Some(AddonDragOutcome::Cancelled)
        } else {
            None
        }
    }

    pub(super) const fn is_active(&self) -> bool {
        !matches!(self.phase, AddonDragPhase::Idle)
    }

    pub(super) const fn is_dragging(&self) -> bool {
        matches!(self.phase, AddonDragPhase::Dragging { .. })
    }

    pub(super) const fn cursor(&self) -> Option<Point> {
        match self.phase {
            AddonDragPhase::Dragging { cursor, .. } => Some(cursor),
            AddonDragPhase::Idle | AddonDragPhase::Pending { .. } => None,
        }
    }

    pub(super) fn thumbnail(&self) -> Option<&image::Handle> {
        match &self.phase {
            AddonDragPhase::Dragging { thumbnail, .. } => thumbnail.as_ref(),
            AddonDragPhase::Idle | AddonDragPhase::Pending { .. } => None,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(super) enum AddonDragPhase {
    #[default]
    Idle,
    Pending {
        source: AddonDragSource,
        card_id: String,
        workshop_id: Option<PublishedFileId>,
        thumbnail: Option<image::Handle>,
        origin: Option<Point>,
    },
    Dragging {
        source: AddonDragSource,
        card_id: String,
        workshop_id: PublishedFileId,
        thumbnail: Option<image::Handle>,
        origin: Point,
        cursor: Point,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum AddonDragOutcome {
    Click {
        source: AddonDragSource,
        card_id: String,
    },
    Drop {
        workshop_id: PublishedFileId,
    },
    Cancelled,
}

#[derive(Clone, Debug)]
pub enum AddonDragMessage {
    CursorMoved(Point),
    Released,
    Cancelled,
}

pub(super) fn moved_beyond_drag_threshold(origin: Point, cursor: Point) -> bool {
    let dx = cursor.x - origin.x;
    let dy = cursor.y - origin.y;
    (dx * dx + dy * dy) >= ADDON_DRAG_THRESHOLD * ADDON_DRAG_THRESHOLD
}

impl App {
    pub(super) fn addon_drag_event_task(
        &mut self,
        message: &AddonDragMessage,
    ) -> Task<RootMessage> {
        match message {
            AddonDragMessage::CursorMoved(position) => {
                self.state.addon_drag.cursor_moved(*position);
                Task::none()
            }
            AddonDragMessage::Released => self.finish_addon_drag_task(),
            AddonDragMessage::Cancelled => {
                let outcome = self.state.addon_drag.cancel();
                self.addon_drag_outcome_task(outcome)
            }
        }
    }

    pub(super) fn finish_addon_drag_task(&mut self) -> Task<RootMessage> {
        let over_downloader = self.state.shell.downloader_drop_target_hovered();
        let outcome = self.state.addon_drag.release(over_downloader);
        self.addon_drag_outcome_task(outcome)
    }

    pub(super) fn addon_drag_outcome_task(
        &mut self,
        outcome: Option<AddonDragOutcome>,
    ) -> Task<RootMessage> {
        let clear_hover = self.apply_shell_message(shell::Message::DownloaderDropTargetExited);

        let Some(outcome) = outcome else {
            return clear_hover;
        };

        let action = match outcome {
            AddonDragOutcome::Click { source, card_id } => {
                self.addon_drag_click_task(source, card_id)
            }
            AddonDragOutcome::Drop { workshop_id } => Task::done(RootMessage::Downloader(
                downloader::Message::WorkshopIdsSubmitted(vec![workshop_id]),
            )),
            AddonDragOutcome::Cancelled => Task::none(),
        };

        Task::batch([clear_hover, action])
    }

    pub(super) fn addon_drag_click_task(
        &self,
        source: AddonDragSource,
        card_id: String,
    ) -> Task<RootMessage> {
        match source {
            AddonDragSource::MyWorkshop => Task::done(RootMessage::MyWorkshop(
                my_workshop::Message::Grid(addon_grid::Message::CardClicked(card_id)),
            )),
            AddonDragSource::InstalledAddons => Task::done(RootMessage::InstalledAddons(
                installed_addons::Message::Grid(addon_grid::Message::CardClicked(card_id)),
            )),
            AddonDragSource::SizeAnalyzer => Task::done(RootMessage::SizeAnalyzer(
                size_analyzer::Message::TreemapClicked,
            )),
        }
    }
}

pub(super) fn file_drop_event(
    event: Event,
    _status: event::Status,
    _window: window::Id,
) -> Option<RootMessage> {
    match event {
        Event::Window(window::Event::FileDropped(path)) => Some(RootMessage::FileDropped(path)),
        _ => None,
    }
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "signature fixed by iced's event::listen_with fn-pointer type"
)]
pub(super) fn addon_drag_event(
    event: Event,
    _status: event::Status,
    _window: window::Id,
) -> Option<RootMessage> {
    match event {
        Event::Mouse(iced::mouse::Event::CursorMoved { position }) => Some(RootMessage::AddonDrag(
            AddonDragMessage::CursorMoved(position),
        )),
        Event::Mouse(iced::mouse::Event::ButtonReleased(iced::mouse::Button::Left)) => {
            Some(RootMessage::AddonDrag(AddonDragMessage::Released))
        }
        Event::Mouse(iced::mouse::Event::CursorLeft) => {
            Some(RootMessage::AddonDrag(AddonDragMessage::Cancelled))
        }
        _ => None,
    }
}

pub(super) fn parse_dropped_workshop_ids(path: &Path) -> Vec<PublishedFileId> {
    let mut ids = Vec::new();
    if let Some(path_text) = path.to_str() {
        ids.extend(parse_workshop_ids_from_text(path_text));
    }
    if let Some(name) = path.file_name().and_then(|name| name.to_str()) {
        ids.extend(parse_workshop_ids_from_text(name));
    }

    if ids.is_empty()
        && path.is_file()
        && path
            .metadata()
            .is_ok_and(|metadata| metadata.len() <= MAX_DROPPED_TEXT_BYTES)
        && let Ok(text) = fs::read_to_string(path)
    {
        ids.extend(parse_embedded_workshop_ids(&text));
    }

    dedupe_workshop_ids(ids)
}

pub(super) fn parse_workshop_ids_from_text(text: &str) -> Vec<PublishedFileId> {
    workshop_url::parse_workshop_ids(text).unwrap_or_default()
}

pub(super) fn parse_embedded_workshop_ids(text: &str) -> Vec<PublishedFileId> {
    text.split(token_delimiter)
        .flat_map(|token| {
            token
                .trim()
                .strip_prefix(WORKSHOP_DRAG_PREFIX)
                .and_then(workshop_url::parse_workshop_id)
                .into_iter()
                .chain(parse_workshop_ids_from_text(token))
        })
        .collect()
}

pub(super) fn token_delimiter(character: char) -> bool {
    character.is_ascii_whitespace()
        || matches!(
            character,
            ',' | ';' | '"' | '\'' | '<' | '>' | '[' | ']' | '(' | ')'
        )
}

pub(super) fn dedupe_workshop_ids(ids: Vec<PublishedFileId>) -> Vec<PublishedFileId> {
    let mut seen = std::collections::HashSet::new();
    ids.into_iter()
        .filter(|id| id.get() != 0 && seen.insert(*id))
        .collect()
}
