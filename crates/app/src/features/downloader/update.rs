use std::time::Instant;

use crate::backend::domain::PublishedFileId;

use super::model::Section;
use super::{Effect, Message, State};

pub fn update(state: &mut State, message: Message) -> Vec<Effect> {
    let active_jobs_before = state.active_job_count();
    let selected_count_before = state.section_count(state.compact_section());
    let total_count_before = state.downloading().len() + state.extracting().len();
    let jobs_may_change = matches!(
        &message,
        Message::EventReceived(_)
            | Message::TaskEventsReceived(_)
            | Message::CancelRequested { .. }
            | Message::RemoveAllRequested(_)
    );
    let mut effects = match message {
        Message::RouteEntered => {
            state.enter_route();
            Vec::new()
        }
        Message::RouteExited => {
            state.exit_route();
            Vec::new()
        }
        Message::CompactSectionSelected(section) => {
            state.select_compact_section(section);
            Vec::new()
        }
        Message::InputEdited(value) => {
            state.edit_input(value);
            Vec::new()
        }
        Message::InputSubmitted => accepted_submission_effects(state.submit_input()),
        Message::WorkshopIdsSubmitted(item_ids) => {
            accepted_submission_effects(state.submit_workshop_ids(item_ids))
        }
        Message::EventReceived(event) => {
            let _changed = state.apply_event(event, Instant::now());
            Vec::new()
        }
        Message::TaskEventsReceived(events) => {
            let _changed = state.apply_task_events(events, Instant::now());
            Vec::new()
        }
        Message::CancelRequested { section, row_id } => state
            .cancel_row(section, row_id)
            .map_or_else(Vec::new, |task_id| {
                vec![Effect::TaskCancellationRequested(vec![task_id])]
            }),
        Message::RemoveAllRequested(section) => {
            let task_ids = state.remove_all(section);
            let mut effects = Vec::new();
            if section == Section::Downloading {
                effects.push(Effect::DownloadQueueCancellationRequested);
            }
            if !task_ids.is_empty() {
                effects.push(Effect::TaskCancellationRequested(task_ids));
            }
            effects
        }
        Message::OpenRequested { section, row_id } => state
            .open_path(section, row_id)
            .map_or_else(Vec::new, |path| {
                vec![Effect::PathsOpenRequested(vec![path])]
            }),
        Message::PreviewRequested { section, row_id } => state
            .preview_target(section, row_id)
            .map_or_else(Vec::new, |target| vec![Effect::PreviewRequested(target)]),
        Message::OpenAllRequested => {
            let paths = state.finished_extract_paths();
            if paths.is_empty() {
                Vec::new()
            } else {
                vec![Effect::PathsOpenRequested(paths)]
            }
        }
        Message::OpenWorkshopRequested(workshop_id) => {
            vec![Effect::WorkshopPageOpenRequested(workshop_id)]
        }
        Message::BulkExtractRequested => vec![Effect::BulkExtractPickerRequested],
        Message::BulkExtractPathsSelected(paths) => local_extraction_effects(paths),
        Message::DestinationRequested => vec![Effect::DestinationSelectionRequested],
        Message::DestinationLabelChanged(label) => {
            state.set_destination_label(label);
            Vec::new()
        }
    };

    if jobs_may_change {
        state.follow_job_transition(selected_count_before, total_count_before);
    }

    append_active_job_count_effect(&mut effects, active_jobs_before, state.active_job_count());
    append_workshop_title_query_effect(&mut effects, state.take_workshop_title_query_ids());
    append_pending_cancellation_effect(&mut effects, state.take_pending_cancellations());
    effects
}

fn accepted_submission_effects(item_ids: Vec<PublishedFileId>) -> Vec<Effect> {
    if item_ids.is_empty() {
        Vec::new()
    } else {
        vec![Effect::WorkshopSubmissionAccepted(item_ids)]
    }
}

fn local_extraction_effects(paths: Vec<std::path::PathBuf>) -> Vec<Effect> {
    if paths.is_empty() {
        Vec::new()
    } else {
        vec![Effect::LocalExtractionRequested(paths)]
    }
}

fn append_active_job_count_effect(
    effects: &mut Vec<Effect>,
    active_jobs_before: usize,
    active_jobs_after: usize,
) {
    if active_jobs_before == active_jobs_after {
        return;
    }

    let count = u32::try_from(active_jobs_after).unwrap_or(u32::MAX);
    effects.push(Effect::ActiveJobCountChanged(count));
}

fn append_workshop_title_query_effect(effects: &mut Vec<Effect>, item_ids: Vec<PublishedFileId>) {
    if !item_ids.is_empty() {
        effects.push(Effect::WorkshopTitleQueryRequested(item_ids));
    }
}

fn append_pending_cancellation_effect(
    effects: &mut Vec<Effect>,
    task_ids: Vec<crate::backend::tasks::TaskId>,
) {
    if !task_ids.is_empty() {
        effects.push(Effect::TaskCancellationRequested(task_ids));
    }
}

#[cfg(test)]
mod tests;
