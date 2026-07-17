use std::path::PathBuf;
use std::time::Instant;

use crate::bridge::domain::PublishedFileId;
use crate::bridge::domain::workshop_url::parse_workshop_ids;
use crate::bridge::tasks::{TaskEvent, TaskId};

use super::model::{DownloaderEvent, DownloaderJob, DownloaderUiState, RowId, Section};

#[derive(Clone, Debug, PartialEq)]
pub struct State {
    route_visible: bool,
    input_text: String,
    input_error: bool,
    destination_label: String,
    jobs: DownloaderUiState,
    compact_section: Section,
}

impl Default for State {
    fn default() -> Self {
        Self {
            route_visible: false,
            input_text: String::new(),
            input_error: false,
            destination_label: String::new(),
            jobs: DownloaderUiState::default(),
            compact_section: Section::Downloading,
        }
    }
}

impl State {
    #[cfg(test)]
    pub(crate) const fn is_route_visible(&self) -> bool {
        self.route_visible
    }

    pub(crate) fn input_text(&self) -> &str {
        &self.input_text
    }

    pub(crate) const fn input_error(&self) -> bool {
        self.input_error
    }

    pub(crate) fn downloading(&self) -> &[DownloaderJob] {
        self.jobs.downloading()
    }

    pub(crate) fn extracting(&self) -> &[DownloaderJob] {
        self.jobs.extracting()
    }

    pub(crate) const fn compact_section(&self) -> Section {
        self.compact_section
    }

    pub(crate) fn section_count(&self, section: Section) -> usize {
        match section {
            Section::Downloading => self.downloading().len(),
            Section::Extracting => self.extracting().len(),
        }
    }

    pub(crate) fn active_job_count(&self) -> usize {
        self.jobs.active_job_count()
    }

    /// Bars only render on the visible route, so the redraw clock stays off
    /// while downloads run in the background; entering the route settles any
    /// stale animations on the first tick.
    pub(crate) fn needs_progress_ticks(&self) -> bool {
        self.route_visible && self.jobs.needs_progress_ticks()
    }

    pub(crate) fn tick_progress(&mut self, now: Instant) {
        self.jobs.tick_progress(now);
    }

    pub(crate) fn set_destination_label(&mut self, label: String) {
        self.destination_label = label;
    }

    pub(crate) fn take_workshop_title_query_ids(&mut self) -> Vec<PublishedFileId> {
        self.jobs.take_workshop_title_query_ids()
    }

    pub(crate) fn take_pending_cancellations(&mut self) -> Vec<TaskId> {
        self.jobs.take_pending_cancellations()
    }

    pub(super) fn enter_route(&mut self) {
        self.route_visible = true;
    }

    pub(super) fn exit_route(&mut self) {
        self.route_visible = false;
    }

    pub(super) fn select_compact_section(&mut self, section: Section) {
        self.compact_section = section;
    }

    pub(super) fn follow_job_transition(
        &mut self,
        selected_count_before: usize,
        total_count_before: usize,
    ) {
        if self.section_count(self.compact_section) != 0 {
            return;
        }

        let other = match self.compact_section {
            Section::Downloading => Section::Extracting,
            Section::Extracting => Section::Downloading,
        };
        if self.section_count(other) != 0 && (selected_count_before != 0 || total_count_before == 0)
        {
            self.compact_section = other;
        }
    }

    pub(super) fn edit_input(&mut self, value: String) {
        self.input_error = self.input_error && !value.trim().is_empty();
        self.input_text = value;
    }

    pub(super) fn submit_input(&mut self) -> Vec<PublishedFileId> {
        match parse_workshop_ids(&self.input_text) {
            Ok(ids) if ids.is_empty() => {
                self.input_error = false;
                Vec::new()
            }
            Ok(ids) => {
                self.input_error = false;
                self.input_text.clear();
                self.jobs.accept_submission(ids)
            }
            Err(error) => {
                log::debug!("invalid Downloader input ignored: {error}");
                self.input_error = true;
                Vec::new()
            }
        }
    }

    pub(super) fn submit_workshop_ids(
        &mut self,
        item_ids: Vec<PublishedFileId>,
    ) -> Vec<PublishedFileId> {
        self.jobs.accept_submission(item_ids)
    }

    pub(super) fn apply_event(&mut self, event: DownloaderEvent, now: Instant) -> bool {
        self.jobs.apply_event(event, now)
    }

    pub(super) fn apply_task_events(&mut self, events: Vec<TaskEvent>, now: Instant) -> bool {
        self.jobs.apply_task_events(events, now)
    }

    pub(super) fn cancel_row(&mut self, section: Section, row_id: RowId) -> Option<TaskId> {
        self.jobs.cancel_row(section, row_id)
    }

    pub(super) fn remove_all(&mut self, section: Section) -> Vec<TaskId> {
        self.jobs.remove_all(section)
    }

    pub(super) fn open_path(&self, section: Section, row_id: RowId) -> Option<PathBuf> {
        self.jobs.open_path(section, row_id)
    }

    pub(super) fn preview_target(
        &self,
        section: Section,
        row_id: RowId,
    ) -> Option<super::model::DownloadPreviewTarget> {
        self.jobs.preview_target(section, row_id)
    }

    pub(super) fn finished_extract_paths(&self) -> Vec<PathBuf> {
        self.jobs.finished_extract_paths()
    }
}
