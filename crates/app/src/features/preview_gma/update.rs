pub fn nav_path_scrollable_id() -> iced::widget::Id {
    iced::widget::Id::new("preview-gma-nav-path")
}

use super::{Effect, Message, State};

pub fn update(state: &mut State, message: Message) -> Vec<Effect> {
    match message {
        Message::OpenRequested(target) => {
            let request = state.begin_open(target);
            let mut effects = vec![
                Effect::ModalOpenRequested,
                Effect::ArchiveOpenRequested(request),
            ];
            if let Some(request) = state.take_workshop_metadata_request() {
                effects.push(Effect::WorkshopMetadataRequested(request));
            }
            effects.push(Effect::ThumbnailDemandsChanged);
            effects
        }
        Message::ArchiveOpened(request_id, result) => {
            if !state.apply_archive_opened(request_id, result) {
                return Vec::new();
            }
            let effects = vec![Effect::BrowserPathChanged, Effect::ThumbnailDemandsChanged];
            #[cfg(feature = "asset-studio")]
            let mut effects = effects;
            #[cfg(feature = "asset-studio")]
            if let Some(request) = state.take_initial_entry_preview_request() {
                effects.insert(0, Effect::EntryPreviewRequested(request));
            }
            effects
        }
        Message::WorkshopMetadataCompleted(request_id, workshop_id, result) => {
            if !state.apply_workshop_metadata(request_id, workshop_id, result) {
                return Vec::new();
            }
            let mut effects = Vec::new();
            if let Some(request) = state.take_author_request() {
                effects.push(Effect::AuthorFetchRequested(request));
            }
            effects.push(Effect::ThumbnailDemandsChanged);
            effects
        }
        Message::AuthorFetchCompleted(request_id, steamid64, result) => {
            let _changed = state.apply_author_result(request_id, steamid64, result);
            Vec::new()
        }
        Message::AuthorLinkRequested => state
            .author_profile_url()
            .map_or_else(Vec::new, |url| vec![Effect::OpenUrlRequested(url)]),
        Message::DirectoryOpened(path) => {
            if state.open_directory(&path) {
                vec![Effect::BrowserPathChanged]
            } else {
                Vec::new()
            }
        }
        Message::ExtractArchiveRequested => {
            if state.request_archive_extraction() {
                vec![Effect::DestinationSelectRequested]
            } else {
                Vec::new()
            }
        }
        #[cfg(not(feature = "asset-studio"))]
        Message::ExtractEntryRequested(path) => state
            .entry_extraction_request(&path)
            .map_or_else(Vec::new, |request| {
                vec![Effect::EntryExtractionRequested(request)]
            }),
        #[cfg(feature = "asset-studio")]
        Message::PreviewEntryRequested(path) => state
            .entry_preview_request(&path)
            .map_or_else(Vec::new, |request| {
                vec![Effect::EntryPreviewRequested(request)]
            }),
        #[cfg(feature = "asset-studio")]
        Message::FilePreview(_) => Vec::new(),
        Message::WorkshopLinkRequested => state
            .workshop_link_url()
            .map_or_else(Vec::new, |url| vec![Effect::OpenUrlRequested(url)]),
        Message::DescriptionLinkRequested(url) => normalize_description_url(&url)
            .map_or_else(Vec::new, |url| vec![Effect::OpenUrlRequested(url)]),
        Message::DescriptionSpoilerToggled(id) => {
            state.toggle_description_spoiler(id);
            Vec::new()
        }
        Message::PanesResized { split, ratio } => {
            state.resize_panes(split, ratio);
            Vec::new()
        }
        Message::PanesLayoutChanged(width) => {
            state.set_pane_ratio(super::view::effective_sidebar_ratio(
                state.sidebar_ratio(),
                width,
            ));
            Vec::new()
        }
        Message::PanesReset(width) => {
            state.reset_panes();
            state.set_pane_ratio(super::view::effective_sidebar_ratio(
                state.sidebar_ratio(),
                width,
            ));
            Vec::new()
        }
        Message::CopyCurrentPathRequested => state
            .copy_current_path_text()
            .map_or_else(Vec::new, |text| vec![Effect::CopyTextRequested(text)]),
        Message::OpenLocationRequested => state
            .reveal_target()
            .map_or_else(Vec::new, |path| vec![Effect::RevealPathRequested(path)]),
        Message::AnimationTick(now) => {
            let _changed = state.tick_animation(now);
            Vec::new()
        }
        Message::UpRequested => {
            if state.go_up() {
                vec![Effect::BrowserPathChanged]
            } else {
                Vec::new()
            }
        }
        Message::CloseFinished => {
            state.close();
            vec![Effect::ThumbnailDemandsChanged]
        }
    }
}

fn normalize_description_url(url: &str) -> Option<String> {
    let url = url.trim();
    if url.is_empty() || url.chars().any(char::is_whitespace) {
        return None;
    }
    let lower = url.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        let (_, remainder) = url.split_once("://")?;
        return (!remainder.is_empty()).then(|| url.to_owned());
    }
    if url.contains("://") {
        return None;
    }
    Some(format!("https://{url}"))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::backend::domain::PublishedFileId;
    use crate::backend::gma::PreviewArchive;
    use crate::features::preview_gma::{LoadedArchive, MetadataRequest, OpenRequest, OpenTarget};
    use crate::test_support::GmaFixtureBuilder;

    fn loaded_archive() -> LoadedArchive {
        let archive = PreviewArchive::from_gma(
            GmaFixtureBuilder::new("Fixture")
                .entry("lua/autorun/init.lua", b"print('ok')\n".to_vec())
                .build(),
        )
        .expect("fixture archive should load");
        LoadedArchive::from_archive(archive)
    }

    #[test]
    fn open_requested_updates_state() {
        let mut state = State::default();

        let effects = update(
            &mut state,
            Message::OpenRequested(OpenTarget::new(
                PathBuf::from("/tmp/addon.gma"),
                "Addon",
                Some(PublishedFileId::new(123).expect("test fixture ids are always nonzero")),
            )),
        );

        assert!(state.is_open());
        assert!(state.loading());
        assert_eq!(
            state.workshop_id(),
            Some(PublishedFileId::new(123).expect("test fixture ids are always nonzero"))
        );
        assert_eq!(
            effects,
            vec![
                Effect::ModalOpenRequested,
                Effect::ArchiveOpenRequested(OpenRequest {
                    request_id: 1,
                    path: PathBuf::from("/tmp/addon.gma"),
                    workshop_id: Some(
                        PublishedFileId::new(123).expect("test fixture ids are always nonzero")
                    ),
                }),
                Effect::WorkshopMetadataRequested(MetadataRequest {
                    request_id: 1,
                    workshop_id: PublishedFileId::new(123)
                        .expect("test fixture ids are always nonzero"),
                }),
                Effect::ThumbnailDemandsChanged,
            ]
        );
    }

    #[test]
    fn close_finished_clears_modal_state() {
        let mut state = State::default();
        let _task = update(
            &mut state,
            Message::OpenRequested(OpenTarget::new(
                PathBuf::from("/tmp/addon.gma"),
                "Addon",
                None,
            )),
        );

        let effects = update(&mut state, Message::CloseFinished);

        assert_eq!(effects, vec![Effect::ThumbnailDemandsChanged]);
        assert!(!state.is_open());
        assert!(!state.loading());
        assert!(state.archive_path().is_none());
    }

    #[test]
    fn loaded_archive_emits_nav_and_thumbnail_effects_after_parallel_metadata_start() {
        let mut state = State::default();
        let _effects = update(
            &mut state,
            Message::OpenRequested(OpenTarget::new(
                PathBuf::from("/tmp/addon.gma"),
                "Addon",
                Some(PublishedFileId::new(123).expect("test fixture ids are always nonzero")),
            )),
        );

        let effects = update(&mut state, Message::ArchiveOpened(1, Ok(loaded_archive())));

        assert!(matches!(
            effects.as_slice(),
            [Effect::BrowserPathChanged, Effect::ThumbnailDemandsChanged,]
        ));
    }

    #[cfg(feature = "asset-studio")]
    #[test]
    fn loaded_archive_with_initial_entry_emits_entry_preview_effect() {
        let mut state = State::default();
        let _effects = update(
            &mut state,
            Message::OpenRequested(
                OpenTarget::new(
                    PathBuf::from("/tmp/addon.gma"),
                    "Addon",
                    Some(PublishedFileId::new(123).expect("test fixture ids are always nonzero")),
                )
                .with_initial_entry_preview("lua/autorun/init.lua"),
            ),
        );

        let effects = update(&mut state, Message::ArchiveOpened(1, Ok(loaded_archive())));

        assert!(effects.iter().any(|effect| {
            matches!(
                effect,
                Effect::EntryPreviewRequested(request)
                    if request.entry_path == "lua/autorun/init.lua"
                        && request.display_name == "init.lua"
            )
        }));
    }

    #[test]
    fn stale_archive_open_result_emits_no_effects() {
        let mut state = State::default();
        let _effects = update(
            &mut state,
            Message::OpenRequested(OpenTarget::new(
                PathBuf::from("/tmp/first.gma"),
                "First",
                Some(PublishedFileId::new(123).expect("test fixture ids are always nonzero")),
            )),
        );
        let _effects = update(
            &mut state,
            Message::OpenRequested(OpenTarget::new(
                PathBuf::from("/tmp/second.gma"),
                "Second",
                Some(PublishedFileId::new(456).expect("test fixture ids are always nonzero")),
            )),
        );

        let effects = update(&mut state, Message::ArchiveOpened(1, Ok(loaded_archive())));

        assert!(effects.is_empty());
    }

    #[test]
    fn unavailable_native_actions_emit_no_effects() {
        let mut state = State::default();

        assert!(update(&mut state, Message::WorkshopLinkRequested).is_empty());
        assert!(update(&mut state, Message::CopyCurrentPathRequested).is_empty());
        assert!(update(&mut state, Message::OpenLocationRequested).is_empty());
        assert!(update(&mut state, Message::UpRequested).is_empty());
    }

    #[test]
    fn description_links_only_emit_safe_web_urls() {
        let mut state = State::default();
        assert_eq!(
            update(
                &mut state,
                Message::DescriptionLinkRequested("example.com/path".to_owned())
            ),
            vec![Effect::OpenUrlRequested(
                "https://example.com/path".to_owned()
            )]
        );
        assert_eq!(
            update(
                &mut state,
                Message::DescriptionLinkRequested("https://example.com".to_owned())
            ),
            vec![Effect::OpenUrlRequested("https://example.com".to_owned())]
        );
        assert!(
            update(
                &mut state,
                Message::DescriptionLinkRequested("file:///tmp/addon".to_owned())
            )
            .is_empty()
        );
        assert!(
            update(
                &mut state,
                Message::DescriptionLinkRequested("https://example.com/has space".to_owned())
            )
            .is_empty()
        );
    }

    #[test]
    fn description_spoilers_toggle_without_side_effects() {
        use gmpublished_backend::bbcode::{Document, ElementKind, Node};

        let document = Document::parse("[spoiler]secret[/spoiler]");
        let Node::Element(element) = &document.nodes()[0] else {
            panic!("expected spoiler element");
        };
        let ElementKind::Spoiler(id) = element.kind() else {
            panic!("expected spoiler id");
        };
        let mut state = State::default();

        assert!(update(&mut state, Message::DescriptionSpoilerToggled(*id)).is_empty());
        assert!(state.revealed_description_spoilers().contains(id));
        assert!(update(&mut state, Message::DescriptionSpoilerToggled(*id)).is_empty());
        assert!(!state.revealed_description_spoilers().contains(id));
    }

    #[test]
    fn pane_ratio_clamps_to_layout_and_survives_modal_close() {
        let mut state = State::default();
        state.set_pane_ratio(0.8);

        assert!(update(&mut state, Message::PanesLayoutChanged(1000.0)).is_empty());
        assert_eq!(state.sidebar_ratio(), 0.45);

        assert_eq!(
            update(&mut state, Message::CloseFinished),
            vec![Effect::ThumbnailDemandsChanged]
        );
        assert_eq!(state.sidebar_ratio(), 0.45);
    }
}
