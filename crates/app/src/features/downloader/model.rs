use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    time::Instant,
};

use crate::backend::domain::{
    PublishedFileId, WorkshopDownloadResult, WorkshopDownloadSuccess, WorkshopMetadata,
};
use crate::backend::tasks::{
    CoalescedTaskTerminal, CoalescedTaskUpdate, DOWNLOAD_STATUS_DOWNLOADING,
    DOWNLOAD_STATUS_LOCATING, TaskEvent, TaskId, TaskKind, TaskUpdate, WorkshopDownloadTaskKind,
};
use crate::backend::ui_error::UiError;
use crate::theme::motion;
use gmpublished_backend::error_key::keys;
use iced::animation::Easing;

const SYNTHETIC_ROW_START: i32 = -1;
const PROGRESS_SMOOTH_DURATION: std::time::Duration = std::time::Duration::from_millis(250);

fn smoothed_ratio(initial: f32) -> motion::Presence<f32> {
    motion::Presence::new(initial, PROGRESS_SMOOTH_DURATION, Easing::EaseOut)
}
const MAX_PENDING_TASK_UPDATES: usize = 128;
pub const EXTRACT_STATUS: &str = "extracting_progress";

/// A downloader row's identity: positive for rows keyed off a real
/// [`TaskId`], negative for synthetic rows (already-finished or errored
/// entries with no running task) minted from a private countdown.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct RowId(i32);

impl RowId {
    #[cfg(test)]
    pub(crate) const fn new(value: i32) -> Self {
        Self(value)
    }
}

/// A finished download the archive previewer can open in-app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DownloadPreviewTarget {
    pub(crate) path: PathBuf,
    pub(crate) title: String,
    pub(crate) workshop_id: Option<PublishedFileId>,
}

fn is_gma_file(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("gma"))
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Section {
    Downloading,
    Extracting,
}

impl Section {
    pub(crate) const fn label_key(self) -> &'static str {
        match self {
            Self::Downloading => "downloader-downloading",
            Self::Extracting => "downloader-extracting",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum DownloaderEvent {
    TaskStarted {
        kind: WorkshopDownloadTaskKind,
        item_id: PublishedFileId,
        task_id: TaskId,
    },
    SubmissionFailed {
        item_ids: Vec<PublishedFileId>,
        error_key: UiError,
    },
    LocalExtractionStarted {
        path: PathBuf,
        task_id: TaskId,
        total_bytes: u64,
    },
    LocalExtractionFinished {
        task_id: TaskId,
        outcome: LocalExtractionOutcome,
    },
    WorkshopMetadataResolved {
        requested_item_ids: Vec<PublishedFileId>,
        items: Vec<WorkshopMetadata>,
    },
    WorkshopDownloadFinished(WorkshopDownloadResult),
}

/// Terminal result for local `.gma` extraction rows.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LocalExtractionOutcome {
    Success(PathBuf),
    Error(UiError),
}

#[derive(Clone, Debug, PartialEq)]
pub struct DownloaderJob {
    id: RowId,
    task_id: Option<TaskId>,
    workshop_id: Option<PublishedFileId>,
    title: String,
    total_bytes: u64,
    progress: JobProgress,
    started_at: Option<Instant>,
    open_path: Option<PathBuf>,
    /// The source `.gma` the archive previewer can open, when it survives
    /// extraction (installed workshop content or a local file; temp
    /// download payloads are deleted and never previewable).
    preview_path: Option<PathBuf>,
    workshop_title_resolved: bool,
    /// Eased display value chasing `progress`, so the bar sweeps between
    /// chunky updates instead of jumping.
    smoothed: motion::Presence<f32>,
}

impl DownloaderJob {
    fn running(
        id: RowId,
        task_id: TaskId,
        workshop_id: PublishedFileId,
        status_key: impl Into<String>,
        started_at: Instant,
    ) -> Self {
        Self {
            id,
            task_id: Some(task_id),
            workshop_id: Some(workshop_id),
            title: workshop_id.to_string(),
            total_bytes: 0,
            progress: JobProgress::Running {
                ratio: 0.0,
                status_key: status_key.into(),
            },
            started_at: Some(started_at),
            open_path: None,
            preview_path: None,
            workshop_title_resolved: false,
            smoothed: smoothed_ratio(0.0),
        }
    }

    pub(crate) fn finished_extract(
        id: RowId,
        workshop_id: Option<PublishedFileId>,
        title: impl Into<String>,
        open_path: impl Into<PathBuf>,
        preview_path: Option<PathBuf>,
    ) -> Self {
        Self {
            id,
            task_id: None,
            workshop_id,
            title: title.into(),
            total_bytes: 0,
            progress: JobProgress::Finished,
            started_at: None,
            open_path: Some(open_path.into()),
            preview_path,
            workshop_title_resolved: true,
            smoothed: smoothed_ratio(0.0),
        }
    }

    fn local_extract(
        id: RowId,
        task_id: TaskId,
        source_path: &Path,
        total_bytes: u64,
        started_at: Instant,
    ) -> Self {
        Self {
            id,
            task_id: Some(task_id),
            workshop_id: None,
            title: title_for_path(source_path),
            total_bytes,
            progress: JobProgress::Running {
                ratio: 0.0,
                status_key: EXTRACT_STATUS.to_owned(),
            },
            started_at: Some(started_at),
            open_path: None,
            preview_path: Some(source_path.to_owned()).filter(|path| is_gma_file(path)),
            workshop_title_resolved: true,
            smoothed: smoothed_ratio(0.0),
        }
    }

    pub(crate) fn errored(
        id: RowId,
        workshop_id: Option<PublishedFileId>,
        title: impl Into<String>,
        error_key: impl Into<UiError>,
    ) -> Self {
        Self {
            id,
            task_id: None,
            workshop_id,
            title: title.into(),
            total_bytes: 0,
            progress: JobProgress::Error(error_key.into()),
            started_at: None,
            open_path: None,
            preview_path: None,
            workshop_title_resolved: workshop_id.is_none(),
            smoothed: smoothed_ratio(0.0),
        }
    }

    pub(crate) const fn id(&self) -> RowId {
        self.id
    }

    pub(crate) const fn workshop_id(&self) -> Option<PublishedFileId> {
        self.workshop_id
    }

    pub(crate) fn title(&self) -> &str {
        &self.title
    }

    pub(crate) const fn total_bytes(&self) -> u64 {
        self.total_bytes
    }

    pub(crate) const fn progress(&self) -> &JobProgress {
        &self.progress
    }

    /// The eased bar position chasing the raw ratio.
    pub(crate) fn smoothed_ratio(&self, now: Instant) -> f32 {
        self.smoothed.current(now).clamp(0.0, 1.0)
    }

    fn needs_progress_ticks(&self) -> bool {
        self.is_running() && self.smoothed.needs_ticks()
    }

    fn tick_progress(&mut self, now: Instant) {
        self.smoothed.tick(now);
    }

    pub(crate) const fn started_at(&self) -> Option<Instant> {
        self.started_at
    }

    pub(crate) fn open_path(&self) -> Option<&Path> {
        self.open_path.as_deref()
    }

    /// Whether the finished row still has a `.gma` source the archive
    /// previewer can open.
    pub(crate) fn previewable(&self) -> bool {
        matches!(self.progress, JobProgress::Finished) && self.preview_path.is_some()
    }

    pub(crate) fn is_running(&self) -> bool {
        matches!(self.progress, JobProgress::Running { .. })
    }

    fn mark_finished(&mut self, extracted_path: PathBuf) -> RowId {
        self.title = title_for_path_or(&extracted_path, &self.title);
        self.progress = JobProgress::Finished;
        self.started_at = None;
        self.open_path = Some(extracted_path);
        self.task_id = None;
        self.id
    }

    fn mark_errored(&mut self, error_key: UiError) -> RowId {
        self.progress = JobProgress::Error(error_key);
        self.started_at = None;
        self.open_path = None;
        self.preview_path = None;
        self.id
    }

    fn workshop_title_query_id(&self) -> Option<PublishedFileId> {
        if self.workshop_title_resolved {
            None
        } else {
            self.workshop_id
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum JobProgress {
    Running { ratio: f64, status_key: String },
    Finished,
    Error(UiError),
}

#[derive(Clone, Debug, PartialEq)]
pub struct DownloaderUiState {
    downloading: Vec<DownloaderJob>,
    extracting: Vec<DownloaderJob>,
    active_items: HashSet<PublishedFileId>,
    hidden_active_items: HashSet<PublishedFileId>,
    task_rows: HashMap<RowId, TaskId>,
    task_row_index: HashMap<TaskId, RowLocation>,
    workshop_row_index: HashMap<PublishedFileId, Vec<RowLocation>>,
    pending_task_updates: HashMap<TaskId, CoalescedTaskUpdate>,
    workshop_title_requests: HashSet<PublishedFileId>,
    /// Tasks that started for already-cancelled (hidden) items; drained by
    /// the update layer into a cancellation effect so they abort at birth.
    pending_cancellations: Vec<TaskId>,
    next_synthetic_row_id: i32,
}

impl Default for DownloaderUiState {
    fn default() -> Self {
        Self {
            downloading: Vec::new(),
            extracting: Vec::new(),
            active_items: HashSet::new(),
            hidden_active_items: HashSet::new(),
            task_rows: HashMap::new(),
            task_row_index: HashMap::new(),
            workshop_row_index: HashMap::new(),
            pending_task_updates: HashMap::new(),
            workshop_title_requests: HashSet::new(),
            pending_cancellations: Vec::new(),
            next_synthetic_row_id: SYNTHETIC_ROW_START,
        }
    }
}

impl DownloaderUiState {
    pub(crate) fn downloading(&self) -> &[DownloaderJob] {
        &self.downloading
    }

    pub(crate) fn extracting(&self) -> &[DownloaderJob] {
        &self.extracting
    }

    pub(crate) fn active_job_count(&self) -> usize {
        self.downloading
            .iter()
            .chain(&self.extracting)
            .filter(|job| job.is_running())
            .count()
    }

    pub(crate) fn needs_progress_ticks(&self) -> bool {
        self.downloading
            .iter()
            .chain(&self.extracting)
            .any(DownloaderJob::needs_progress_ticks)
    }

    pub(crate) fn tick_progress(&mut self, now: Instant) {
        for job in self.downloading.iter_mut().chain(&mut self.extracting) {
            job.tick_progress(now);
        }
    }

    pub(crate) fn accept_submission(
        &mut self,
        item_ids: Vec<PublishedFileId>,
    ) -> Vec<PublishedFileId> {
        let mut accepted = Vec::new();

        for item_id in item_ids {
            if !self.active_items.insert(item_id) {
                continue;
            }

            self.remove_terminal_rows_for_item(item_id);
            self.hidden_active_items.remove(&item_id);
            accepted.push(item_id);
        }

        accepted
    }

    pub(crate) fn apply_event(&mut self, event: DownloaderEvent, now: Instant) -> bool {
        match event {
            DownloaderEvent::TaskStarted {
                kind,
                item_id,
                task_id,
            } => {
                self.apply_task_started(kind, item_id, task_id, now);
                true
            }
            DownloaderEvent::SubmissionFailed {
                item_ids,
                error_key,
            } => {
                for item_id in item_ids {
                    self.active_items.remove(&item_id);
                    self.apply_error(item_id, error_key.clone());
                }
                true
            }
            DownloaderEvent::LocalExtractionStarted {
                path,
                task_id,
                total_bytes,
            } => {
                self.apply_local_extraction_started(&path, task_id, total_bytes, now);
                true
            }
            DownloaderEvent::LocalExtractionFinished { task_id, outcome } => {
                self.apply_local_extraction_finished(task_id, outcome)
            }
            DownloaderEvent::WorkshopMetadataResolved {
                requested_item_ids,
                items,
            } => self.apply_workshop_metadata(requested_item_ids, items),
            DownloaderEvent::WorkshopDownloadFinished(result) => {
                self.apply_result(result);
                true
            }
        }
    }

    pub(crate) fn apply_task_events(&mut self, events: Vec<TaskEvent>, now: Instant) -> bool {
        let mut changed = false;
        for (task_id, update) in events {
            changed |= self.apply_task_update(task_id, update.as_update().clone(), now);
        }
        changed
    }

    fn apply_task_update(&mut self, task_id: TaskId, update: TaskUpdate, now: Instant) -> bool {
        let Some((section, index)) = self.running_row_index_for_task(task_id) else {
            self.remember_pending_task_update(task_id, update);
            return false;
        };

        self.apply_task_update_at(section, index, update, now)
    }

    fn apply_task_update_at(
        &mut self,
        section: Section,
        index: usize,
        update: TaskUpdate,
        now: Instant,
    ) -> bool {
        match update {
            TaskUpdate::Started { status, .. } => {
                let Some(job) = self.job_mut(section, index) else {
                    return false;
                };
                job.total_bytes = 0;
                job.progress = JobProgress::Running {
                    ratio: 0.0,
                    status_key: status.key,
                };
                if job.started_at.is_none() {
                    job.started_at = Some(now);
                }
                true
            }
            TaskUpdate::Status(status) => {
                let Some(job) = self.job_mut(section, index) else {
                    return false;
                };
                let JobProgress::Running { status_key, .. } = &mut job.progress else {
                    return false;
                };
                *status_key = status.key;
                true
            }
            TaskUpdate::Progress(progress) => {
                let Some(job) = self.job_mut(section, index) else {
                    return false;
                };
                let JobProgress::Running { ratio, .. } = &mut job.progress else {
                    return false;
                };
                *ratio = progress.clamp(0.0, 1.0);
                let target = *ratio as f32;
                job.smoothed.go(target, Instant::now());
                true
            }
            TaskUpdate::ProgressIncr(delta) => {
                let Some(job) = self.job_mut(section, index) else {
                    return false;
                };
                let JobProgress::Running { ratio, .. } = &mut job.progress else {
                    return false;
                };
                *ratio = (*ratio + delta).clamp(0.0, 1.0);
                let target = *ratio as f32;
                job.smoothed.go(target, Instant::now());
                true
            }
            TaskUpdate::ProgressReset => {
                let Some(job) = self.job_mut(section, index) else {
                    return false;
                };
                let JobProgress::Running { ratio, .. } = &mut job.progress else {
                    return false;
                };
                *ratio = 0.0;
                // A reset refills from zero; sweeping backwards would read as
                // the download losing work.
                job.smoothed.snap(0.0);
                true
            }
            TaskUpdate::Total(total_bytes) => {
                let Some(job) = self.job_mut(section, index) else {
                    return false;
                };
                job.total_bytes = total_bytes;
                true
            }
            TaskUpdate::Finished => {
                let Some(job) = self.job_mut(section, index) else {
                    return false;
                };
                let JobProgress::Running { ratio, .. } = &mut job.progress else {
                    return false;
                };
                *ratio = 1.0;
                true
            }
            TaskUpdate::Error(error_key) => {
                self.mark_error(section, index, error_key);
                true
            }
            TaskUpdate::Abandoned => {
                self.mark_error(section, index, UiError::new(keys::UNKNOWN));
                true
            }
        }
    }

    pub(crate) fn apply_task_started(
        &mut self,
        kind: WorkshopDownloadTaskKind,
        item_id: PublishedFileId,
        task_id: TaskId,
        now: Instant,
    ) {
        if self.hidden_active_items.contains(&item_id) {
            // The item was cancelled before this task materialized (e.g. it
            // was still resolving, or extraction started after its download
            // row was removed): cancel the task at birth instead of showing
            // a row for it.
            self.pending_task_updates.remove(&task_id);
            self.pending_cancellations.push(task_id);
            return;
        }
        self.active_items.insert(item_id);

        let row_id = self.row_id_for_task(task_id);
        match kind {
            WorkshopDownloadTaskKind::Download => {
                self.remove_terminal_rows_for_item(item_id);
                self.upsert_workshop_running_row(
                    Section::Downloading,
                    row_id,
                    task_id,
                    item_id,
                    DOWNLOAD_STATUS_DOWNLOADING,
                    now,
                );
            }
            WorkshopDownloadTaskKind::Extract => {
                self.remove_download_rows_for_item(item_id);
                self.remove_terminal_rows_for_item(item_id);
                self.upsert_workshop_running_row(
                    Section::Extracting,
                    row_id,
                    task_id,
                    item_id,
                    DOWNLOAD_STATUS_LOCATING,
                    now,
                );
            }
        }
    }

    fn upsert_workshop_running_row(
        &mut self,
        section: Section,
        row_id: RowId,
        task_id: TaskId,
        item_id: PublishedFileId,
        status_key: &'static str,
        now: Instant,
    ) {
        let existing = self.row_index_for_workshop(section, item_id);

        if let Some(index) = existing {
            let old_row_id = {
                let job = &mut self.section_jobs_mut(section)[index];
                let old_row_id = job.id;
                job.id = row_id;
                job.task_id = Some(task_id);
                job.progress = JobProgress::Running {
                    ratio: 0.0,
                    status_key: status_key.to_owned(),
                };
                job.started_at = Some(now);
                job.open_path = None;
                job.preview_path = None;
                job.workshop_title_resolved = false;
                old_row_id
            };
            self.unregister_task_row(old_row_id);
        } else {
            let job = DownloaderJob::running(row_id, task_id, item_id, status_key, now);
            self.section_jobs_mut(section).push(job);
        }

        self.register_task_row(row_id, task_id);
        self.rebuild_row_indexes();
        self.apply_pending_task_update(task_id, now);
    }

    pub(crate) fn apply_result(&mut self, result: WorkshopDownloadResult) {
        let item_id = result.item_id;
        self.active_items.remove(&item_id);
        if self.hidden_active_items.remove(&item_id) {
            return;
        }

        match result.outcome {
            Ok(success) => self.apply_success(success),
            Err(error) => self.apply_error(item_id, error),
        }
    }

    fn apply_success(&mut self, success: WorkshopDownloadSuccess) {
        let item_id = success.item_id;
        self.remove_download_rows_for_item(item_id);
        let preview_path = success.installed_path.filter(|path| is_gma_file(path));

        if let Some(index) = self.row_index_for_workshop(Section::Extracting, item_id) {
            let row_id = {
                let job = &mut self.extracting[index];
                let row_id = job.mark_finished(success.extracted_path);
                job.preview_path = preview_path;
                job.workshop_title_resolved = true;
                row_id
            };
            self.unregister_task_row(row_id);
            return;
        }

        let row_id = RowId(self.next_synthetic_row_id());
        let title = title_for_path_or(&success.extracted_path, &item_id.to_string());
        self.extracting.push(DownloaderJob::finished_extract(
            row_id,
            Some(item_id),
            title,
            success.extracted_path,
            preview_path,
        ));
    }

    pub(crate) fn take_pending_cancellations(&mut self) -> Vec<TaskId> {
        std::mem::take(&mut self.pending_cancellations)
    }

    /// Returns Workshop IDs whose row titles still need metadata lookups.
    pub(crate) fn take_workshop_title_query_ids(&mut self) -> Vec<PublishedFileId> {
        let row_item_ids = self
            .downloading
            .iter()
            .chain(&self.extracting)
            .filter_map(DownloaderJob::workshop_title_query_id)
            .collect::<Vec<_>>();
        let mut item_ids = Vec::new();

        for item_id in row_item_ids {
            if self.workshop_title_requests.insert(item_id) {
                item_ids.push(item_id);
            }
        }

        item_ids
    }

    pub(crate) fn apply_workshop_metadata(
        &mut self,
        requested_item_ids: Vec<PublishedFileId>,
        items: Vec<WorkshopMetadata>,
    ) -> bool {
        let mut changed = false;

        for item_id in &requested_item_ids {
            self.workshop_title_requests.remove(item_id);
        }

        let requested_item_ids = requested_item_ids.into_iter().collect::<HashSet<_>>();

        for item in items {
            let item_id = item.id;
            if !requested_item_ids.contains(&item_id) {
                continue;
            }
            let title = item.title.trim();
            if title.is_empty() {
                continue;
            }
            changed |= self.apply_workshop_title(item_id, title);
        }

        changed
    }

    fn apply_workshop_title(&mut self, item_id: PublishedFileId, title: &str) -> bool {
        let mut changed = false;
        let Some(locations) = self.workshop_row_index.get(&item_id).cloned() else {
            return false;
        };

        for location in locations {
            let Some(job) = self.job_mut(location.section, location.index) else {
                continue;
            };
            if job.workshop_id == Some(item_id) {
                if job.title != title {
                    title.clone_into(&mut job.title);
                    changed = true;
                }
                job.workshop_title_resolved = true;
            }
        }

        changed
    }

    pub(crate) fn apply_local_extraction_started(
        &mut self,
        path: &Path,
        task_id: TaskId,
        total_bytes: u64,
        now: Instant,
    ) {
        let row_id = self.row_id_for_task(task_id);
        self.extracting.push(DownloaderJob::local_extract(
            row_id,
            task_id,
            path,
            total_bytes,
            now,
        ));
        self.register_task_row(row_id, task_id);
        self.rebuild_row_indexes();
        self.apply_pending_task_update(task_id, now);
    }

    fn apply_local_extraction_finished(
        &mut self,
        task_id: TaskId,
        outcome: LocalExtractionOutcome,
    ) -> bool {
        let Some(index) = self
            .extracting
            .iter()
            .position(|job| job.task_id == Some(task_id))
        else {
            return false;
        };

        match outcome {
            LocalExtractionOutcome::Success(path) => {
                let row_id = self.extracting[index].mark_finished(path);
                self.unregister_task_row(row_id);
            }
            LocalExtractionOutcome::Error(error_key) => {
                self.mark_error(Section::Extracting, index, error_key);
            }
        }

        true
    }

    fn apply_error(&mut self, item_id: PublishedFileId, error_key: UiError) {
        if let Some(index) = self.row_index_for_workshop(Section::Extracting, item_id) {
            self.remove_download_rows_for_item(item_id);
            self.mark_error(Section::Extracting, index, error_key);
            return;
        }

        if let Some(index) = self.row_index_for_workshop(Section::Downloading, item_id) {
            self.mark_error(Section::Downloading, index, error_key);
            return;
        }

        let row_id = RowId(self.next_synthetic_row_id());
        self.downloading.push(DownloaderJob::errored(
            row_id,
            Some(item_id),
            item_id.to_string(),
            error_key,
        ));
        self.rebuild_row_indexes();
    }

    /// Removes the row the user X-ed out and returns its task to cancel if
    /// it was still running. Mirrors [`Self::remove_all`]'s per-row
    /// semantics: the row leaves the list immediately whatever its state
    /// (a terminal row is simply dismissed), and a running item is hidden
    /// so a follow-up task (a download finishing into extraction) is
    /// cancelled at birth instead of resurrecting it.
    pub(crate) fn cancel_row(&mut self, section: Section, row_id: RowId) -> Option<TaskId> {
        let Some(index) = self.row_index(section, row_id) else {
            self.unregister_task_row(row_id);
            return None;
        };

        let task_id = self.task_rows.get(&row_id).copied();
        let (running, workshop_id) = {
            let job = &self.section_jobs(section)[index];
            (job.is_running(), job.workshop_id)
        };
        if running && let Some(item_id) = workshop_id {
            self.hidden_active_items.insert(item_id);
            self.active_items.remove(&item_id);
        }

        match section {
            Section::Downloading => {
                retain_jobs(&mut self.downloading, &mut self.task_rows, |job| {
                    job.id != row_id
                });
            }
            Section::Extracting => {
                retain_jobs(&mut self.extracting, &mut self.task_rows, |job| {
                    job.id != row_id
                });
            }
        }
        self.rebuild_row_indexes();

        if running { task_id } else { None }
    }

    pub(crate) fn remove_all(&mut self, section: Section) -> Vec<TaskId> {
        let mut remove_rows = HashSet::new();
        let mut cancel_tasks = Vec::new();
        let rows = self
            .section_jobs(section)
            .iter()
            .map(|job| {
                (
                    job.id,
                    job.is_running(),
                    job.workshop_id,
                    self.task_rows.get(&job.id).copied(),
                )
            })
            .collect::<Vec<_>>();

        for (row_id, running, workshop_id, task_id) in rows {
            if !running {
                remove_rows.insert(row_id);
                continue;
            }

            if let Some(task_id) = task_id {
                if let Some(item_id) = workshop_id {
                    self.hidden_active_items.insert(item_id);
                    self.active_items.remove(&item_id);
                }
                remove_rows.insert(row_id);
                cancel_tasks.push(task_id);
            }
        }

        // Items submitted but still resolving (no row in either section yet)
        // have no task to cancel; hide them so their tasks are cancelled at
        // birth when they eventually start.
        if section == Section::Downloading {
            let rowless = self
                .active_items
                .iter()
                .filter(|item_id| !self.workshop_row_index.contains_key(item_id))
                .copied()
                .collect::<Vec<_>>();
            for item_id in rowless {
                self.active_items.remove(&item_id);
                self.hidden_active_items.insert(item_id);
            }
        }

        if remove_rows.is_empty() {
            return Vec::new();
        }

        match section {
            Section::Extracting => {
                retain_jobs(&mut self.extracting, &mut self.task_rows, |job| {
                    !remove_rows.contains(&job.id)
                });
            }
            Section::Downloading => {
                retain_jobs(&mut self.downloading, &mut self.task_rows, |job| {
                    !remove_rows.contains(&job.id)
                });
            }
        }

        self.rebuild_row_indexes();
        cancel_tasks
    }

    pub(crate) fn open_path(&self, section: Section, row_id: RowId) -> Option<PathBuf> {
        self.section_jobs(section)
            .iter()
            .find(|job| job.id == row_id && matches!(job.progress, JobProgress::Finished))
            .and_then(|job| job.open_path.clone())
    }

    pub(crate) fn preview_target(
        &self,
        section: Section,
        row_id: RowId,
    ) -> Option<DownloadPreviewTarget> {
        self.section_jobs(section)
            .iter()
            .find(|job| job.id == row_id && matches!(job.progress, JobProgress::Finished))
            .and_then(|job| {
                Some(DownloadPreviewTarget {
                    path: job.preview_path.clone()?,
                    title: job.title.clone(),
                    workshop_id: job.workshop_id,
                })
            })
    }

    pub(crate) fn finished_extract_paths(&self) -> Vec<PathBuf> {
        self.extracting
            .iter()
            .filter(|job| matches!(job.progress, JobProgress::Finished))
            .filter_map(|job| job.open_path.clone())
            .collect()
    }

    fn row_index(&self, section: Section, row_id: RowId) -> Option<usize> {
        self.section_jobs(section)
            .iter()
            .position(|job| job.id == row_id)
    }

    fn running_row_index_for_task(&self, task_id: TaskId) -> Option<(Section, usize)> {
        let location = self.task_row_index.get(&task_id).copied()?;
        self.section_jobs(location.section)
            .get(location.index)
            .is_some_and(|job| job.task_id == Some(task_id) && job.is_running())
            .then_some((location.section, location.index))
    }

    fn remember_pending_task_update(&mut self, task_id: TaskId, update: TaskUpdate) {
        if self.running_row_index_for_task(task_id).is_some() {
            return;
        }

        let should_track = self.pending_task_updates.contains_key(&task_id)
            || matches!(
                &update,
                TaskUpdate::Started {
                    kind: TaskKind::Download | TaskKind::Extract,
                    ..
                }
            );
        if !should_track {
            return;
        }

        if !self.pending_task_updates.contains_key(&task_id)
            && self.pending_task_updates.len() >= MAX_PENDING_TASK_UPDATES
            && let Some(stale_id) = self.pending_task_updates.keys().next().copied()
        {
            self.pending_task_updates.remove(&stale_id);
        }

        self.pending_task_updates
            .entry(task_id)
            .or_default()
            .observe(update, 0.0);
    }

    fn apply_pending_task_update(&mut self, task_id: TaskId, now: Instant) -> bool {
        let Some(update) = self.pending_task_updates.remove(&task_id) else {
            return false;
        };
        let Some((section, index)) = self.running_row_index_for_task(task_id) else {
            return false;
        };

        self.apply_pending_task_update_at(section, index, update, now)
    }

    fn apply_pending_task_update_at(
        &mut self,
        section: Section,
        index: usize,
        update: CoalescedTaskUpdate,
        now: Instant,
    ) -> bool {
        let unregister_task = {
            let Some(job) = self.job_mut(section, index) else {
                return false;
            };

            if let Some(started) = update.started {
                job.total_bytes = 0;
                job.progress = JobProgress::Running {
                    ratio: 0.0,
                    status_key: started.status.key,
                };
                if job.started_at.is_none() {
                    job.started_at = Some(now);
                }
            }

            if let Some(status) = update.status
                && let JobProgress::Running { status_key, .. } = &mut job.progress
            {
                *status_key = status.key;
            }

            if let Some(total_bytes) = update.total_bytes {
                job.total_bytes = total_bytes;
            }

            if let Some(progress) = update.progress
                && let JobProgress::Running { ratio, .. } = &mut job.progress
            {
                *ratio = progress;
            }

            match update.terminal {
                Some(CoalescedTaskTerminal::Finished) => {
                    if let JobProgress::Running { ratio, .. } = &mut job.progress {
                        *ratio = 1.0;
                    }
                    None
                }
                Some(CoalescedTaskTerminal::Error(error_key)) => {
                    Some((job.mark_errored(error_key), job.workshop_id))
                }
                Some(CoalescedTaskTerminal::Abandoned) => Some((
                    job.mark_errored(UiError::new(keys::UNKNOWN)),
                    job.workshop_id,
                )),
                None => None,
            }
        };

        if let Some((row_id, workshop_id)) = unregister_task {
            if let Some(item_id) = workshop_id {
                self.active_items.remove(&item_id);
            }
            self.unregister_task_row(row_id);
        }

        true
    }

    fn job_mut(&mut self, section: Section, index: usize) -> Option<&mut DownloaderJob> {
        self.section_jobs_mut(section).get_mut(index)
    }

    fn register_task_row(&mut self, row_id: RowId, task_id: TaskId) {
        self.task_rows.insert(row_id, task_id);
    }

    fn unregister_task_row(&mut self, row_id: RowId) {
        if let Some(task_id) = self.task_rows.remove(&row_id) {
            self.task_row_index.remove(&task_id);
        }
    }

    /// Marks a row errored and releases its item from `active_items`: an
    /// errored (or cancelled) pipeline is terminal, so the item must become
    /// resubmittable again.
    fn mark_error(&mut self, section: Section, index: usize, error_key: UiError) {
        let Some((row_id, workshop_id)) = self.job_mut(section, index).map(|job| {
            let row_id = job.mark_errored(error_key);
            job.task_id = None;
            (row_id, job.workshop_id)
        }) else {
            return;
        };
        if let Some(item_id) = workshop_id {
            self.active_items.remove(&item_id);
        }
        self.unregister_task_row(row_id);
    }

    fn remove_download_rows_for_item(&mut self, item_id: PublishedFileId) {
        retain_jobs(&mut self.downloading, &mut self.task_rows, |job| {
            job.workshop_id != Some(item_id)
        });
        self.rebuild_row_indexes();
    }

    fn remove_terminal_rows_for_item(&mut self, item_id: PublishedFileId) {
        retain_jobs(&mut self.downloading, &mut self.task_rows, |job| {
            job.workshop_id != Some(item_id) || job.is_running()
        });
        retain_jobs(&mut self.extracting, &mut self.task_rows, |job| {
            job.workshop_id != Some(item_id) || job.is_running()
        });
        self.rebuild_row_indexes();
    }

    fn row_id_for_task(&mut self, task_id: TaskId) -> RowId {
        RowId(i32::try_from(task_id.get()).unwrap_or_else(|_| self.next_synthetic_row_id()))
    }

    fn next_synthetic_row_id(&mut self) -> i32 {
        let id = self.next_synthetic_row_id;
        self.next_synthetic_row_id = self.next_synthetic_row_id.saturating_sub(1);
        id
    }

    fn section_jobs(&self, section: Section) -> &[DownloaderJob] {
        match section {
            Section::Downloading => &self.downloading,
            Section::Extracting => &self.extracting,
        }
    }

    fn section_jobs_mut(&mut self, section: Section) -> &mut Vec<DownloaderJob> {
        match section {
            Section::Downloading => &mut self.downloading,
            Section::Extracting => &mut self.extracting,
        }
    }

    fn row_index_for_workshop(&self, section: Section, item_id: PublishedFileId) -> Option<usize> {
        self.workshop_row_index
            .get(&item_id)?
            .iter()
            .find(|location| location.section == section)
            .and_then(|location| {
                self.section_jobs(section)
                    .get(location.index)
                    .is_some_and(|job| job.workshop_id == Some(item_id))
                    .then_some(location.index)
            })
    }

    fn rebuild_row_indexes(&mut self) {
        let mut task_row_index = HashMap::new();
        let mut workshop_row_index = HashMap::<PublishedFileId, Vec<RowLocation>>::new();
        for section in [Section::Downloading, Section::Extracting] {
            for (index, job) in self.section_jobs(section).iter().enumerate() {
                let location = RowLocation { section, index };
                if let Some(task_id) = job.task_id
                    && job.is_running()
                {
                    task_row_index.insert(task_id, location);
                }
                if let Some(item_id) = job.workshop_id {
                    workshop_row_index
                        .entry(item_id)
                        .or_default()
                        .push(location);
                }
            }
        }
        self.task_row_index = task_row_index;
        self.workshop_row_index = workshop_row_index;
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RowLocation {
    section: Section,
    index: usize,
}

fn retain_jobs(
    jobs: &mut Vec<DownloaderJob>,
    task_rows: &mut HashMap<RowId, TaskId>,
    mut keep: impl FnMut(&DownloaderJob) -> bool,
) -> bool {
    let mut removed = Vec::new();
    jobs.retain(|job| {
        let should_keep = keep(job);
        if !should_keep {
            removed.push(job.id);
        }
        should_keep
    });
    let removed_any = !removed.is_empty();
    for row_id in removed {
        task_rows.remove(&row_id);
    }
    removed_any
}

fn title_for_path(path: &Path) -> String {
    title_from_file_name(path).unwrap_or_else(|| path.display().to_string())
}

fn title_for_path_or(path: &Path, fallback: &str) -> String {
    title_from_file_name(path).unwrap_or_else(|| fallback.to_owned())
}

fn title_from_file_name(path: &Path) -> Option<String> {
    path.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .map(ToOwned::to_owned)
}

#[cfg(test)]
pub fn workshop_result_success(item_id: u64, extracted_path: PathBuf) -> WorkshopDownloadResult {
    workshop_result_success_with_gma(item_id, extracted_path, None)
}

#[cfg(test)]
pub fn workshop_result_success_with_gma(
    item_id: u64,
    extracted_path: PathBuf,
    installed_path: Option<PathBuf>,
) -> WorkshopDownloadResult {
    WorkshopDownloadResult {
        item_id: crate::backend::domain::PublishedFileId::new(item_id)
            .expect("test fixture ids are always nonzero"),
        outcome: Ok(WorkshopDownloadSuccess {
            item_id: crate::backend::domain::PublishedFileId::new(item_id)
                .expect("test fixture ids are always nonzero"),
            installed_path,
            extracted_path,
        }),
    }
}
