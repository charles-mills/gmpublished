use std::time::{Duration, Instant};

use gmpublished_backend::error_key::keys;

use crate::bridge::tasks::{StatusKey, TaskEvent, TaskId, TaskKind, TaskUpdate};
use crate::bridge::ui_error::UiError;
use crate::theme::{Tokens, motion};

/// How long a settled toast lingers before its exit animation starts.
const EXPIRE_HOLD: Duration = Duration::from_millis(2500);

pub const TOAST_HEIGHT: f32 = 49.0;
pub const TOAST_GAP: f32 = TOAST_HEIGHT / 2.0;

/// Visible-stack cap: four toasts at a 1080p work area, scaling linearly
/// with viewport height, never fewer than two.
const TOASTS_AT_REFERENCE: f32 = 4.0;
const REFERENCE_VIEWPORT_HEIGHT: f32 = 1027.0;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Outcome {
    Pending,
    Finished { at: Instant },
    Error { error: UiError, at: Instant },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Toast {
    task_id: TaskId,
    kind: TaskKind,
    status: StatusKey,
    progress: f64,
    total_bytes: u64,
    started_at: Instant,
    outcome: Outcome,
    presence: motion::Presence<bool>,
    expiring: bool,
}

impl Toast {
    fn new(task_id: TaskId, kind: TaskKind, status: StatusKey, now: Instant) -> Self {
        let mut presence = motion::boolean(
            false,
            Tokens::dark().motion.overlay_toast_duration(),
            motion::expo_ease(),
        );
        presence.go(true, now);
        Self {
            task_id,
            kind,
            status,
            progress: 0.0,
            total_bytes: 0,
            started_at: now,
            outcome: Outcome::Pending,
            presence,
            expiring: false,
        }
    }

    pub(crate) const fn task_id(&self) -> TaskId {
        self.task_id
    }

    pub(crate) const fn kind(&self) -> &TaskKind {
        &self.kind
    }

    pub(crate) const fn status(&self) -> &StatusKey {
        &self.status
    }

    pub(crate) const fn progress(&self) -> f64 {
        self.progress
    }

    pub(crate) const fn total_bytes(&self) -> u64 {
        self.total_bytes
    }

    pub(crate) const fn outcome(&self) -> &Outcome {
        &self.outcome
    }

    pub(crate) const fn pending(&self) -> bool {
        matches!(self.outcome, Outcome::Pending)
    }

    /// A pending toast shows the cancel button; notices finish at birth and
    /// never had anything to cancel.
    pub(crate) const fn cancellable(&self) -> bool {
        self.pending() && !matches!(self.kind, TaskKind::Notice)
    }

    pub(crate) fn spinner_elapsed(&self, now: Instant) -> f32 {
        now.saturating_duration_since(self.started_at).as_secs_f32()
    }

    /// Enter/exit progress in `0..=1`; drives opacity and the clip-grow
    /// height so the stack slides as toasts appear and collapse.
    pub(crate) fn presence(&self, now: Instant) -> f32 {
        self.presence.interpolate(0.0, 1.0, now)
    }

    fn apply_update(&mut self, update: TaskUpdate, now: Instant) {
        if self.expiring {
            return;
        }
        match update {
            // A second Started for a live id resets the toast in place.
            TaskUpdate::Started { kind, status } => {
                self.kind = kind;
                self.status = status;
                self.progress = 0.0;
                self.total_bytes = 0;
                self.outcome = Outcome::Pending;
            }
            TaskUpdate::Status(status) => self.status = status,
            TaskUpdate::Progress(progress) => self.progress = progress.clamp(0.0, 1.0),
            TaskUpdate::ProgressIncr(delta) => {
                self.progress = (self.progress + delta).clamp(0.0, 1.0);
            }
            TaskUpdate::ProgressReset => self.progress = 0.0,
            TaskUpdate::Total(total_bytes) => self.total_bytes = total_bytes,
            TaskUpdate::Finished => {
                self.progress = 1.0;
                self.outcome = Outcome::Finished { at: now };
            }
            TaskUpdate::Error(error) => {
                self.outcome = Outcome::Error { error, at: now };
            }
            TaskUpdate::Abandoned => {
                self.outcome = Outcome::Error {
                    error: UiError::new(keys::UNKNOWN),
                    at: now,
                };
            }
        }
    }
}

/// Bottom-center toast stack fed by task events the Downloader page does not
/// own: publish submissions, quick extractions, and one-shot notices.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct State {
    toasts: Vec<Toast>,
}

impl State {
    pub(crate) fn toasts(&self) -> &[Toast] {
        &self.toasts
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.toasts.is_empty()
    }

    /// The clock stays live while any toast exists: pending toasts animate
    /// their spinner and settled toasts are waiting out the expiry hold, so
    /// there is no idle resting state until the stack empties.
    pub(crate) fn needs_ticks(&self) -> bool {
        !self.toasts.is_empty()
    }

    pub(crate) fn max_visible(viewport_height: f32) -> usize {
        if viewport_height <= 0.0 {
            return TOASTS_AT_REFERENCE as usize;
        }
        let scaled = (viewport_height * TOASTS_AT_REFERENCE / REFERENCE_VIEWPORT_HEIGHT).round();
        (scaled as usize).max(2)
    }

    pub(super) fn apply_task_events(&mut self, events: Vec<TaskEvent>, now: Instant) -> bool {
        let mut changed = false;
        for (task_id, update) in events {
            changed |= self.apply_task_update(task_id, update.into_update(), now);
        }
        changed
    }

    fn apply_task_update(&mut self, task_id: TaskId, update: TaskUpdate, now: Instant) -> bool {
        if let Some(toast) = self
            .toasts
            .iter_mut()
            .find(|toast| toast.task_id == task_id)
        {
            toast.apply_update(update, now);
            return true;
        }

        // Only overlay-owned kinds spawn toasts; Download/Extract/Search
        // updates belong to their own surfaces and fall through here.
        if let TaskUpdate::Started {
            kind: kind @ (TaskKind::Publish | TaskKind::OverlayExtract | TaskKind::Notice),
            status,
        } = update
        {
            self.toasts.push(Toast::new(task_id, kind, status, now));
            return true;
        }

        false
    }

    /// Starts exit animations for toasts whose hold elapsed and drops the
    /// ones whose exit settled, returning whether anything changed.
    pub(crate) fn tick(&mut self, now: Instant) -> bool {
        let mut changed = false;
        for toast in &mut self.toasts {
            let expire_at = match toast.outcome {
                Outcome::Pending => None,
                Outcome::Finished { at } | Outcome::Error { at, .. } => Some(at + EXPIRE_HOLD),
            };
            if !toast.expiring && expire_at.is_some_and(|at| now >= at) {
                toast.expiring = true;
                toast.presence.go(false, now);
                changed = true;
            }
            changed |= toast.presence.tick(now);
        }

        let before = self.toasts.len();
        self.toasts
            .retain(|toast| !toast.expiring || toast.presence.needs_ticks());
        changed || self.toasts.len() != before
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use crate::bridge::tasks::SharedTaskUpdate;

    use super::*;

    fn started(kind: TaskKind, status: &str) -> TaskUpdate {
        TaskUpdate::Started {
            kind,
            status: StatusKey::new(status),
        }
    }

    fn event(id: u64, update: TaskUpdate) -> TaskEvent {
        (TaskId::from_raw(id), SharedTaskUpdate::new(update))
    }

    #[test]
    fn only_overlay_kinds_spawn_toasts() {
        let mut state = State::default();
        let now = Instant::now();

        state.apply_task_events(
            vec![
                event(1, started(TaskKind::Publish, "PUBLISH_PACKING")),
                event(2, started(TaskKind::Download, "downloading")),
                event(3, started(TaskKind::Extract, "extracting_progress")),
                event(4, started(TaskKind::Search, "search")),
                event(5, started(TaskKind::OverlayExtract, "extracting_progress")),
                event(6, started(TaskKind::Notice, "debug-simulated-notice")),
            ],
            now,
        );

        let kinds: Vec<_> = state.toasts().iter().map(Toast::kind).collect();
        assert_eq!(
            kinds,
            [
                &TaskKind::Publish,
                &TaskKind::OverlayExtract,
                &TaskKind::Notice
            ]
        );
    }

    #[test]
    fn updates_for_unknown_tasks_are_ignored() {
        let mut state = State::default();
        let changed =
            state.apply_task_events(vec![event(9, TaskUpdate::Progress(0.5))], Instant::now());

        assert!(!changed);
        assert!(state.is_empty());
    }

    #[test]
    fn progress_and_status_flow_into_the_toast() {
        let mut state = State::default();
        let now = Instant::now();
        state.apply_task_events(
            vec![
                event(1, started(TaskKind::Publish, "PUBLISH_PACKING")),
                event(1, TaskUpdate::Total(1000)),
                event(1, TaskUpdate::Progress(0.4)),
                event(
                    1,
                    TaskUpdate::Status(StatusKey::new("PUBLISH_UPLOADING_CONTENT")),
                ),
            ],
            now,
        );

        let toast = &state.toasts()[0];
        assert_eq!(toast.total_bytes(), 1000);
        assert!((toast.progress() - 0.4).abs() < f64::EPSILON);
        assert_eq!(toast.status().key, "PUBLISH_UPLOADING_CONTENT");
        assert!(toast.cancellable());
    }

    #[test]
    fn finished_toast_holds_then_expires() {
        let mut state = State::default();
        let now = Instant::now();
        state.apply_task_events(
            vec![
                event(1, started(TaskKind::Publish, "PUBLISH_PACKING")),
                event(1, TaskUpdate::Finished),
            ],
            now,
        );
        assert!(matches!(
            state.toasts()[0].outcome(),
            Outcome::Finished { .. }
        ));
        assert!(!state.toasts()[0].cancellable());

        // Still held just before the expiry hold elapses.
        state.tick(now + Duration::from_millis(2400));
        assert_eq!(state.toasts().len(), 1);

        // Hold elapsed: exit starts, and the toast survives until the exit
        // animation settles.
        state.tick(now + Duration::from_millis(2600));
        assert_eq!(state.toasts().len(), 1);
        assert!(state.needs_ticks());

        state.tick(now + Duration::from_secs(4));
        assert!(state.is_empty());
        assert!(!state.needs_ticks());
    }

    #[test]
    fn error_toast_expires_like_a_finished_one() {
        let mut state = State::default();
        let now = Instant::now();
        state.apply_task_events(
            vec![
                event(1, started(TaskKind::OverlayExtract, "extracting_progress")),
                event(1, TaskUpdate::Error(UiError::new(keys::IO_ERROR))),
            ],
            now,
        );
        assert!(matches!(state.toasts()[0].outcome(), Outcome::Error { .. }));

        state.tick(now + Duration::from_secs(3));
        state.tick(now + Duration::from_secs(4));
        assert!(state.is_empty());
    }

    #[test]
    fn abandoned_maps_to_an_unknown_error() {
        let mut state = State::default();
        let now = Instant::now();
        state.apply_task_events(
            vec![
                event(1, started(TaskKind::Publish, "PUBLISH_PACKING")),
                event(1, TaskUpdate::Abandoned),
            ],
            now,
        );

        let Outcome::Error { error, .. } = state.toasts()[0].outcome() else {
            panic!("expected an error outcome");
        };
        assert_eq!(error.key, keys::UNKNOWN);
    }

    #[test]
    fn max_visible_scales_with_viewport_height() {
        assert_eq!(State::max_visible(1027.0), 4);
        assert_eq!(State::max_visible(2160.0), 8);
        assert_eq!(State::max_visible(300.0), 2);
        assert_eq!(State::max_visible(0.0), 4);
    }
}
