use std::path::PathBuf;
use std::time::Instant;

use crate::bridge::domain::{PublishedFileId, WorkshopMetadata};
use crate::bridge::tasks::{
    SharedTaskUpdate, StatusKey, TaskId, TaskKind, TaskUpdate, WorkshopDownloadTaskKind,
};
use crate::bridge::ui_error::UiError;
use gmpublished_backend::error_key::keys;

use super::super::model::{
    DownloaderEvent, JobProgress, LocalExtractionOutcome, RowId, Section, workshop_result_success,
    workshop_result_success_with_gma,
};
use super::{Effect, Message, State, update};

#[test]
fn route_entry_marks_page_visible() {
    let mut state = State::default();

    assert!(update(&mut state, Message::RouteEntered).is_empty());

    assert!(state.is_route_visible());
}

#[test]
fn route_exit_hides_the_page() {
    let mut state = State::default();
    assert!(update(&mut state, Message::RouteEntered).is_empty());

    assert!(update(&mut state, Message::RouteExited).is_empty());

    assert!(!state.is_route_visible());
}

#[test]
fn compact_section_selection_is_explicit() {
    let mut state = State::default();

    assert_eq!(state.compact_section(), Section::Downloading);
    assert!(
        update(
            &mut state,
            Message::CompactSectionSelected(Section::Extracting)
        )
        .is_empty()
    );
    assert_eq!(state.compact_section(), Section::Extracting);
}

#[test]
fn input_submission_parses_ids_and_clears_valid_input() {
    let mut state = State::default();
    assert!(update(&mut state, Message::InputEdited("123 456 123".to_owned())).is_empty());

    let effects = update(&mut state, Message::InputSubmitted);

    assert_eq!(state.input_text(), "");
    assert!(!state.input_error());
    assert_eq!(state.active_job_count(), 0);
    assert_eq!(
        effects,
        vec![Effect::WorkshopSubmissionAccepted(vec![
            PublishedFileId::new(123).expect("test fixture ids are always nonzero"),
            PublishedFileId::new(456).expect("test fixture ids are always nonzero")
        ])]
    );
}

#[test]
fn invalid_input_sets_error_until_edited_empty() {
    let mut state = State::default();
    assert!(update(&mut state, Message::InputEdited("not an id".to_owned())).is_empty());
    assert!(update(&mut state, Message::InputSubmitted).is_empty());

    assert!(state.input_error());

    assert!(update(&mut state, Message::InputEdited(String::new())).is_empty());

    assert!(!state.input_error());
}

#[test]
fn task_started_creates_download_row_and_metadata_updates_title() {
    let mut state = State::default();
    let now = Instant::now();

    let effects = update(
        &mut state,
        Message::EventReceived(DownloaderEvent::TaskStarted {
            kind: WorkshopDownloadTaskKind::Download,
            item_id: PublishedFileId::new(123).expect("test fixture ids are always nonzero"),
            task_id: TaskId::from_raw(7),
        }),
    );

    assert_eq!(state.downloading().len(), 1);
    assert_eq!(state.downloading()[0].title(), "123");
    assert_eq!(
        effects,
        vec![
            Effect::ActiveJobCountChanged(1),
            Effect::WorkshopTitleQueryRequested(vec![
                PublishedFileId::new(123).expect("test fixture ids are always nonzero")
            ]),
        ]
    );

    let _changed = state.apply_event(
        DownloaderEvent::WorkshopMetadataResolved {
            requested_item_ids: vec![
                PublishedFileId::new(123).expect("test fixture ids are always nonzero"),
            ],
            items: vec![workshop_item(123, "Resolved Addon")],
        },
        now,
    );

    assert_eq!(state.downloading()[0].title(), "Resolved Addon");
}

#[test]
fn workshop_snapshot_tasks_never_create_downloader_rows() {
    let mut state = State::default();
    let task_id = TaskId::from_raw(70);

    for update_message in [
        TaskUpdate::Started {
            kind: TaskKind::WorkshopSnapshot,
            status: StatusKey::new("downloading"),
        },
        TaskUpdate::Progress(0.5),
        TaskUpdate::Finished,
    ] {
        let effects = update(
            &mut state,
            Message::TaskEventsReceived(vec![(task_id, update_message.into())]),
        );
        assert!(effects.is_empty());
    }

    assert!(state.downloading().is_empty());
    assert!(state.extracting().is_empty());
}

#[test]
fn extract_start_moves_item_from_downloading_to_extracting() {
    let mut state = State::default();
    let now = Instant::now();
    let _ = state.apply_event(
        DownloaderEvent::TaskStarted {
            kind: WorkshopDownloadTaskKind::Download,
            item_id: PublishedFileId::new(123).expect("test fixture ids are always nonzero"),
            task_id: TaskId::from_raw(1),
        },
        now,
    );
    let _ = state.apply_event(
        DownloaderEvent::TaskStarted {
            kind: WorkshopDownloadTaskKind::Extract,
            item_id: PublishedFileId::new(123).expect("test fixture ids are always nonzero"),
            task_id: TaskId::from_raw(2),
        },
        now,
    );

    assert!(state.downloading().is_empty());
    assert_eq!(state.extracting().len(), 1);
    assert_eq!(
        state.extracting()[0].workshop_id(),
        Some(PublishedFileId::new(123).expect("test fixture ids are always nonzero"))
    );
}

#[test]
fn compact_section_follows_the_only_job_across_download_and_extract() {
    let mut state = State::default();

    let _effects = update(
        &mut state,
        Message::EventReceived(DownloaderEvent::TaskStarted {
            kind: WorkshopDownloadTaskKind::Download,
            item_id: PublishedFileId::new(123).expect("test fixture ids are always nonzero"),
            task_id: TaskId::from_raw(1),
        }),
    );
    assert_eq!(state.compact_section(), Section::Downloading);

    let _effects = update(
        &mut state,
        Message::EventReceived(DownloaderEvent::TaskStarted {
            kind: WorkshopDownloadTaskKind::Extract,
            item_id: PublishedFileId::new(123).expect("test fixture ids are always nonzero"),
            task_id: TaskId::from_raw(2),
        }),
    );
    assert_eq!(state.compact_section(), Section::Extracting);
}

#[test]
fn compact_section_does_not_jump_while_both_sections_have_jobs() {
    let mut state = State::default();
    let _effects = update(
        &mut state,
        Message::EventReceived(DownloaderEvent::TaskStarted {
            kind: WorkshopDownloadTaskKind::Download,
            item_id: PublishedFileId::new(123).expect("test fixture ids are always nonzero"),
            task_id: TaskId::from_raw(1),
        }),
    );
    let _effects = update(
        &mut state,
        Message::EventReceived(DownloaderEvent::TaskStarted {
            kind: WorkshopDownloadTaskKind::Download,
            item_id: PublishedFileId::new(456).expect("test fixture ids are always nonzero"),
            task_id: TaskId::from_raw(2),
        }),
    );
    let _effects = update(
        &mut state,
        Message::EventReceived(DownloaderEvent::TaskStarted {
            kind: WorkshopDownloadTaskKind::Extract,
            item_id: PublishedFileId::new(123).expect("test fixture ids are always nonzero"),
            task_id: TaskId::from_raw(3),
        }),
    );

    assert_eq!(state.downloading().len(), 1);
    assert_eq!(state.extracting().len(), 1);
    assert_eq!(state.compact_section(), Section::Downloading);
}

#[test]
fn task_events_update_running_row_progress() {
    let mut state = State::default();
    let task_id = TaskId::from_raw(99);
    let now = Instant::now();
    let _ = state.apply_event(
        DownloaderEvent::LocalExtractionStarted {
            path: PathBuf::from("/tmp/local/addon.gma"),
            task_id,
            total_bytes: 1024,
        },
        now,
    );

    let effects = update(
        &mut state,
        Message::TaskEventsReceived(vec![
            (task_id, SharedTaskUpdate::new(TaskUpdate::Total(4096))),
            (task_id, SharedTaskUpdate::new(TaskUpdate::Progress(0.5))),
            (
                task_id,
                SharedTaskUpdate::new(TaskUpdate::Status(StatusKey::from("decompressing"))),
            ),
        ]),
    );

    assert!(effects.is_empty());

    let job = &state.extracting()[0];
    assert_eq!(job.total_bytes(), 4096);
    assert!(matches!(
        job.progress(),
        JobProgress::Running { ratio, status_key }
            if *ratio == 0.5 && status_key == "decompressing"
    ));
}

#[test]
fn task_error_event_emits_active_job_count_effect() {
    let mut state = State::default();
    let task_id = TaskId::from_raw(100);
    let _effects = update(
        &mut state,
        Message::EventReceived(DownloaderEvent::LocalExtractionStarted {
            path: PathBuf::from("/tmp/local/error.gma"),
            task_id,
            total_bytes: 1024,
        }),
    );

    let effects = update(
        &mut state,
        Message::TaskEventsReceived(vec![(
            task_id,
            SharedTaskUpdate::new(TaskUpdate::Error(UiError::new(keys::CANCELLED))),
        )]),
    );

    assert_eq!(effects, vec![Effect::ActiveJobCountChanged(0)]);
}

#[test]
fn workshop_ids_submission_emits_effect_only_for_new_ids() {
    let mut state = State::default();

    assert_eq!(
        update(
            &mut state,
            Message::WorkshopIdsSubmitted(vec![
                PublishedFileId::new(123).expect("test fixture ids are always nonzero"),
                PublishedFileId::new(456).expect("test fixture ids are always nonzero"),
                PublishedFileId::new(123).expect("test fixture ids are always nonzero")
            ])
        ),
        vec![Effect::WorkshopSubmissionAccepted(vec![
            PublishedFileId::new(123).expect("test fixture ids are always nonzero"),
            PublishedFileId::new(456).expect("test fixture ids are always nonzero")
        ])]
    );
    assert!(
        update(
            &mut state,
            Message::WorkshopIdsSubmitted(vec![
                PublishedFileId::new(123).expect("test fixture ids are always nonzero")
            ])
        )
        .is_empty()
    );
}

#[test]
fn empty_submission_emits_no_effect() {
    let mut state = State::default();

    assert!(update(&mut state, Message::InputSubmitted).is_empty());
    assert!(update(&mut state, Message::WorkshopIdsSubmitted(Vec::new())).is_empty());
}

#[test]
fn cancellation_removes_the_running_row_and_requests_task_cancel() {
    let mut state = State::default();
    let task_id = TaskId::from_raw(42);
    let _effects = update(
        &mut state,
        Message::EventReceived(DownloaderEvent::TaskStarted {
            kind: WorkshopDownloadTaskKind::Download,
            item_id: PublishedFileId::new(123).expect("test fixture ids are always nonzero"),
            task_id,
        }),
    );
    let row_id = state.downloading()[0].id();

    let effects = update(
        &mut state,
        Message::CancelRequested {
            section: Section::Downloading,
            row_id,
        },
    );

    // The X is a remove button: the row leaves the list immediately and
    // the backend task is asked to stop, exactly like Remove All scoped to
    // one row.
    assert_eq!(
        effects,
        vec![
            Effect::TaskCancellationRequested(vec![task_id]),
            Effect::ActiveJobCountChanged(0),
        ]
    );
    assert!(state.downloading().is_empty());

    // The backend's terminal confirmation for the removed row is a no-op.
    let effects = update(
        &mut state,
        Message::TaskEventsReceived(vec![(
            task_id,
            SharedTaskUpdate::new(TaskUpdate::Error(UiError::new(keys::CANCELLED))),
        )]),
    );

    assert!(effects.is_empty());
    assert!(state.downloading().is_empty());
}

#[test]
fn cancellation_dismisses_a_terminal_row_without_effects() {
    let mut state = State::default();
    let _ = state.apply_event(
        DownloaderEvent::TaskStarted {
            kind: WorkshopDownloadTaskKind::Extract,
            item_id: PublishedFileId::new(123).expect("test fixture ids are always nonzero"),
            task_id: TaskId::from_raw(77),
        },
        Instant::now(),
    );
    let _ = state.apply_event(
        DownloaderEvent::WorkshopDownloadFinished(workshop_result_success(
            123,
            PathBuf::from("/tmp/extracted/Done"),
        )),
        Instant::now(),
    );
    assert!(matches!(
        state.extracting()[0].progress(),
        JobProgress::Finished
    ));
    let row_id = state.extracting()[0].id();

    let effects = update(
        &mut state,
        Message::CancelRequested {
            section: Section::Extracting,
            row_id,
        },
    );

    assert!(effects.is_empty());
    assert!(state.extracting().is_empty());
}

#[test]
fn cancelled_item_extraction_follow_up_task_is_cancelled_at_birth() {
    let mut state = State::default();
    let item_id = PublishedFileId::new(123).expect("test fixture ids are always nonzero");
    let _effects = update(
        &mut state,
        Message::EventReceived(DownloaderEvent::TaskStarted {
            kind: WorkshopDownloadTaskKind::Download,
            item_id,
            task_id: TaskId::from_raw(1),
        }),
    );
    let row_id = state.downloading()[0].id();
    let _effects = update(
        &mut state,
        Message::CancelRequested {
            section: Section::Downloading,
            row_id,
        },
    );

    // The download completes anyway (the cancel lost the race) and its
    // extraction task starts: the hidden item must cancel it at birth
    // instead of resurrecting a row the user removed.
    let extract_task = TaskId::from_raw(2);
    let effects = update(
        &mut state,
        Message::EventReceived(DownloaderEvent::TaskStarted {
            kind: WorkshopDownloadTaskKind::Extract,
            item_id,
            task_id: extract_task,
        }),
    );

    assert_eq!(
        effects,
        vec![Effect::TaskCancellationRequested(vec![extract_task])]
    );
    assert!(state.extracting().is_empty());
}

#[test]
fn cancellation_for_unknown_row_emits_no_effect() {
    let mut state = State::default();

    assert!(
        update(
            &mut state,
            Message::CancelRequested {
                section: Section::Downloading,
                row_id: RowId::new(404),
            },
        )
        .is_empty()
    );
}

#[test]
fn remove_all_emits_cancellation_effect_for_running_rows() {
    let mut state = State::default();
    let _effects = update(
        &mut state,
        Message::EventReceived(DownloaderEvent::TaskStarted {
            kind: WorkshopDownloadTaskKind::Download,
            item_id: PublishedFileId::new(123).expect("test fixture ids are always nonzero"),
            task_id: TaskId::from_raw(1),
        }),
    );
    let _effects = update(
        &mut state,
        Message::EventReceived(DownloaderEvent::TaskStarted {
            kind: WorkshopDownloadTaskKind::Download,
            item_id: PublishedFileId::new(456).expect("test fixture ids are always nonzero"),
            task_id: TaskId::from_raw(2),
        }),
    );

    let effects = update(
        &mut state,
        Message::RemoveAllRequested(Section::Downloading),
    );

    assert_eq!(
        effects,
        vec![
            Effect::DownloadQueueCancellationRequested,
            Effect::TaskCancellationRequested(vec![TaskId::from_raw(1), TaskId::from_raw(2)]),
            Effect::ActiveJobCountChanged(0),
        ]
    );
    assert!(state.downloading().is_empty());
}

#[test]
fn remove_all_without_running_tasks_only_cancels_the_queue() {
    let mut state = State::default();

    assert_eq!(
        update(
            &mut state,
            Message::RemoveAllRequested(Section::Downloading)
        ),
        vec![Effect::DownloadQueueCancellationRequested]
    );
    assert!(update(&mut state, Message::RemoveAllRequested(Section::Extracting)).is_empty());
}

#[test]
fn remove_all_cancels_late_tasks_for_items_still_resolving() {
    let mut state = State::default();
    let item_id = PublishedFileId::new(123).expect("test fixture ids are always nonzero");
    let effects = update(&mut state, Message::WorkshopIdsSubmitted(vec![item_id]));
    assert_eq!(
        effects,
        vec![Effect::WorkshopSubmissionAccepted(vec![item_id])]
    );

    // No row has materialized yet when the user removes all.
    let _effects = update(
        &mut state,
        Message::RemoveAllRequested(Section::Downloading),
    );

    // The task that eventually starts is cancelled at birth, with no row.
    let task_id = TaskId::from_raw(7);
    let effects = update(
        &mut state,
        Message::EventReceived(DownloaderEvent::TaskStarted {
            kind: WorkshopDownloadTaskKind::Download,
            item_id,
            task_id,
        }),
    );
    assert_eq!(
        effects,
        vec![Effect::TaskCancellationRequested(vec![task_id])]
    );
    assert!(state.downloading().is_empty());
}

#[test]
fn cancelled_item_can_be_resubmitted() {
    let mut state = State::default();
    let item_id = PublishedFileId::new(123).expect("test fixture ids are always nonzero");
    let task_id = TaskId::from_raw(1);
    let _effects = update(&mut state, Message::WorkshopIdsSubmitted(vec![item_id]));
    let _effects = update(
        &mut state,
        Message::EventReceived(DownloaderEvent::TaskStarted {
            kind: WorkshopDownloadTaskKind::Download,
            item_id,
            task_id,
        }),
    );

    let row_id = state.downloading()[0].id();
    let _effects = update(
        &mut state,
        Message::CancelRequested {
            section: Section::Downloading,
            row_id,
        },
    );

    // Removing the row releases the item, so it can be resubmitted right
    // away — same contract as Remove All.
    let effects = update(&mut state, Message::WorkshopIdsSubmitted(vec![item_id]));
    assert_eq!(
        effects,
        vec![Effect::WorkshopSubmissionAccepted(vec![item_id])]
    );
}

#[test]
fn errored_item_can_be_resubmitted() {
    let mut state = State::default();
    let item_id = PublishedFileId::new(123).expect("test fixture ids are always nonzero");
    let task_id = TaskId::from_raw(1);
    let _effects = update(&mut state, Message::WorkshopIdsSubmitted(vec![item_id]));
    let _effects = update(
        &mut state,
        Message::EventReceived(DownloaderEvent::TaskStarted {
            kind: WorkshopDownloadTaskKind::Download,
            item_id,
            task_id,
        }),
    );
    let _effects = update(
        &mut state,
        Message::TaskEventsReceived(vec![(
            task_id,
            SharedTaskUpdate::new(TaskUpdate::Error(UiError::new(keys::UNKNOWN))),
        )]),
    );

    let effects = update(&mut state, Message::WorkshopIdsSubmitted(vec![item_id]));
    assert_eq!(
        effects,
        vec![Effect::WorkshopSubmissionAccepted(vec![item_id])]
    );
}

#[test]
fn open_requested_emits_path_effect_for_finished_row() {
    let mut state = State::default();
    let path = PathBuf::from("/tmp/extracted/Open Me");
    let _effects = update(
        &mut state,
        Message::EventReceived(DownloaderEvent::TaskStarted {
            kind: WorkshopDownloadTaskKind::Extract,
            item_id: PublishedFileId::new(123).expect("test fixture ids are always nonzero"),
            task_id: TaskId::from_raw(77),
        }),
    );
    let _effects = update(
        &mut state,
        Message::EventReceived(DownloaderEvent::WorkshopDownloadFinished(
            workshop_result_success(123, path.clone()),
        )),
    );
    let row_id = state.extracting()[0].id();

    let effects = update(
        &mut state,
        Message::OpenRequested {
            section: Section::Extracting,
            row_id,
        },
    );

    assert_eq!(effects, vec![Effect::PathsOpenRequested(vec![path])]);
}

#[test]
fn preview_requested_emits_target_only_when_the_source_gma_survives() {
    // Steamworks download: the installed .gma outlives extraction, so the
    // finished row targets it for the archive previewer.
    let mut state = State::default();
    let gma = PathBuf::from("/tmp/workshop/content/4000/123/addon_123.gma");
    let _effects = update(
        &mut state,
        Message::EventReceived(DownloaderEvent::TaskStarted {
            kind: WorkshopDownloadTaskKind::Extract,
            item_id: PublishedFileId::new(123).expect("test fixture ids are always nonzero"),
            task_id: TaskId::from_raw(78),
        }),
    );
    let _effects = update(
        &mut state,
        Message::EventReceived(DownloaderEvent::WorkshopDownloadFinished(
            workshop_result_success_with_gma(
                123,
                PathBuf::from("/tmp/extracted/Addon"),
                Some(gma.clone()),
            ),
        )),
    );
    assert!(state.extracting()[0].previewable());
    let row_id = state.extracting()[0].id();

    let effects = update(
        &mut state,
        Message::PreviewRequested {
            section: Section::Extracting,
            row_id,
        },
    );

    assert!(matches!(
        effects.as_slice(),
        [Effect::PreviewRequested(target)]
            if target.path == gma && target.workshop_id == Some(PublishedFileId::new(123).expect("test fixture ids are always nonzero"))
    ));

    // Legacy temp download: the payload is deleted after extraction, so the
    // row reports no source .gma and Preview stays unavailable.
    let mut ephemeral = State::default();
    let _effects = update(
        &mut ephemeral,
        Message::EventReceived(DownloaderEvent::TaskStarted {
            kind: WorkshopDownloadTaskKind::Extract,
            item_id: PublishedFileId::new(124).expect("test fixture ids are always nonzero"),
            task_id: TaskId::from_raw(79),
        }),
    );
    let _effects = update(
        &mut ephemeral,
        Message::EventReceived(DownloaderEvent::WorkshopDownloadFinished(
            workshop_result_success(124, PathBuf::from("/tmp/extracted/Open Me")),
        )),
    );
    assert!(!ephemeral.extracting()[0].previewable());
    let row_id = ephemeral.extracting()[0].id();

    assert!(
        update(
            &mut ephemeral,
            Message::PreviewRequested {
                section: Section::Extracting,
                row_id,
            },
        )
        .is_empty()
    );
}

#[test]
fn local_extraction_rows_preview_their_source_gma() {
    let mut state = State::default();
    let source = PathBuf::from("/home/user/addons/funny_barrel.gma");
    let task_id = TaskId::from_raw(80);
    let _effects = update(
        &mut state,
        Message::EventReceived(DownloaderEvent::LocalExtractionStarted {
            path: source.clone(),
            task_id,
            total_bytes: 42,
        }),
    );
    let _effects = update(
        &mut state,
        Message::EventReceived(DownloaderEvent::LocalExtractionFinished {
            task_id,
            outcome: LocalExtractionOutcome::Success(PathBuf::from("/tmp/extracted/funny_barrel")),
        }),
    );
    assert!(state.extracting()[0].previewable());
    let row_id = state.extracting()[0].id();

    let effects = update(
        &mut state,
        Message::PreviewRequested {
            section: Section::Extracting,
            row_id,
        },
    );

    assert!(matches!(
        effects.as_slice(),
        [Effect::PreviewRequested(target)]
            if target.path == source && target.workshop_id.is_none()
    ));
}

#[test]
fn open_requested_for_running_or_unknown_row_emits_no_effect() {
    let mut state = State::default();
    let _effects = update(
        &mut state,
        Message::EventReceived(DownloaderEvent::TaskStarted {
            kind: WorkshopDownloadTaskKind::Extract,
            item_id: PublishedFileId::new(123).expect("test fixture ids are always nonzero"),
            task_id: TaskId::from_raw(77),
        }),
    );
    let row_id = state.extracting()[0].id();

    assert!(
        update(
            &mut state,
            Message::OpenRequested {
                section: Section::Extracting,
                row_id,
            },
        )
        .is_empty()
    );
    assert!(
        update(
            &mut state,
            Message::OpenRequested {
                section: Section::Extracting,
                row_id: RowId::new(404),
            },
        )
        .is_empty()
    );
}

#[test]
fn open_all_emits_all_finished_extract_paths() {
    let mut state = State::default();
    let first = PathBuf::from("/tmp/extracted/First");
    let second = PathBuf::from("/tmp/extracted/Second");
    let _ = state.apply_event(
        DownloaderEvent::WorkshopDownloadFinished(workshop_result_success(123, first.clone())),
        Instant::now(),
    );
    let _ = state.apply_event(
        DownloaderEvent::WorkshopDownloadFinished(workshop_result_success(456, second.clone())),
        Instant::now(),
    );

    assert_eq!(
        update(&mut state, Message::OpenAllRequested),
        vec![Effect::PathsOpenRequested(vec![first, second])]
    );
}

#[test]
fn open_all_without_finished_paths_emits_no_effect() {
    let mut state = State::default();

    assert!(update(&mut state, Message::OpenAllRequested).is_empty());
}

#[test]
fn direct_outward_requests_emit_effects() {
    let mut state = State::default();
    let paths = vec![
        PathBuf::from("/tmp/local/a.gma"),
        PathBuf::from("/tmp/local/b.gma"),
    ];

    assert_eq!(
        update(
            &mut state,
            Message::OpenWorkshopRequested(Some(
                PublishedFileId::new(123).expect("test fixture ids are always nonzero")
            ))
        ),
        vec![Effect::WorkshopPageOpenRequested(Some(
            PublishedFileId::new(123).expect("test fixture ids are always nonzero")
        ))]
    );
    assert_eq!(
        update(&mut state, Message::OpenWorkshopRequested(None)),
        vec![Effect::WorkshopPageOpenRequested(None)]
    );
    assert_eq!(
        update(&mut state, Message::BulkExtractRequested),
        vec![Effect::BulkExtractPickerRequested]
    );
    assert_eq!(
        update(&mut state, Message::BulkExtractPathsSelected(paths.clone())),
        vec![Effect::LocalExtractionRequested(paths)]
    );
    assert!(update(&mut state, Message::BulkExtractPathsSelected(Vec::new())).is_empty());
    assert_eq!(
        update(&mut state, Message::DestinationRequested),
        vec![Effect::DestinationSelectionRequested]
    );
}

#[test]
fn destination_label_change_emits_no_effects() {
    let mut state = State::default();
    assert!(update(&mut state, Message::RouteEntered).is_empty());

    assert!(
        update(
            &mut state,
            Message::DestinationLabelChanged("Downloads".to_owned()),
        )
        .is_empty()
    );
}

#[test]
fn terminal_download_result_finishes_extract_row_with_open_path() {
    let mut state = State::default();
    let _ = state.apply_event(
        DownloaderEvent::TaskStarted {
            kind: WorkshopDownloadTaskKind::Extract,
            item_id: PublishedFileId::new(123).expect("test fixture ids are always nonzero"),
            task_id: TaskId::from_raw(77),
        },
        Instant::now(),
    );

    let _ = state.apply_event(
        DownloaderEvent::WorkshopDownloadFinished(workshop_result_success(
            123,
            PathBuf::from("/tmp/extracted/My Addon"),
        )),
        Instant::now(),
    );

    assert_eq!(state.extracting().len(), 1);
    assert!(matches!(
        state.extracting()[0].progress(),
        JobProgress::Finished
    ));
    let expected_path = PathBuf::from("/tmp/extracted/My Addon");
    assert_eq!(
        state.extracting()[0].open_path(),
        Some(expected_path.as_path())
    );
}

#[test]
fn local_extraction_result_finishes_local_row() {
    let mut state = State::default();
    let task_id = TaskId::from_raw(88);
    let _ = state.apply_event(
        DownloaderEvent::LocalExtractionStarted {
            path: PathBuf::from("/tmp/source/selected.gma"),
            task_id,
            total_bytes: 2048,
        },
        Instant::now(),
    );

    assert!(state.apply_event(
        DownloaderEvent::LocalExtractionFinished {
            task_id,
            outcome: LocalExtractionOutcome::Success(PathBuf::from("/tmp/extracted/Local Addon")),
        },
        Instant::now(),
    ));

    assert!(matches!(
        state.extracting()[0].progress(),
        JobProgress::Finished
    ));
    assert_eq!(state.extracting()[0].title(), "Local Addon");
}

fn workshop_item(id: u64, title: &str) -> WorkshopMetadata {
    WorkshopMetadata {
        id: PublishedFileId::new(id).expect("test fixture ids are always nonzero"),
        title: title.to_owned(),
        time_created: 0,
        time_updated: 0,
        score: 0.0,
        tags: Vec::new(),
        preview_url: None,
        subscriptions: 0,
        full_description: None,
        owner_steamid: None,
        thumbhash: None,
    }
}
