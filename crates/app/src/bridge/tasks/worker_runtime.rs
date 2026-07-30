use gmpublished_backend::{ExecutionResources, ExecutionScheduleError};
use std::sync::Arc;
use thiserror::Error;

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum RunBlockingError {
    #[error(transparent)]
    Schedule(#[from] ScheduleError),
    #[error("worker dropped before returning a result")]
    WorkerDropped,
}

impl gmpublished_backend::HasErrorKey for RunBlockingError {
    fn error_key(&self) -> gmpublished_backend::ErrorKey {
        match self {
            Self::Schedule(error) => gmpublished_backend::HasErrorKey::error_key(error),
            Self::WorkerDropped => gmpublished_backend::error_keys::WORKER_DROPPED,
        }
    }

    fn error_detail(&self) -> Option<String> {
        Some(self.to_string())
    }
}

pub(super) fn show_native_open_error_dialog(description: String) {
    let _ = futures::executor::block_on(
        rfd::AsyncMessageDialog::new()
            .set_level(rfd::MessageLevel::Error)
            .set_title("gmpublished")
            .set_description(description)
            .show(),
    );
}

pub(super) type RuntimeJob = Box<dyn FnOnce() + Send + 'static>;
pub(super) type WorkerPoolSpawner =
    fn(&AppWorkerRuntime, Arc<str>, RuntimeJob) -> Result<(), ScheduleError>;

/// App-facing scheduler over the process-owned backend execution resources.
///
/// There is deliberately no second set of app pools here: filesystem/Steam
/// work shares the bounded blocking executor, media/CDN work shares the
/// bounded network executor, and CPU-parallel work enters through
/// `BackendServices::cpu_executor`.
#[derive(Debug)]
pub(super) struct AppWorkerRuntime {
    execution: ExecutionResources,
}

impl AppWorkerRuntime {
    pub(super) fn new(execution: ExecutionResources) -> Self {
        Self { execution }
    }

    pub(super) fn spawn_blocking(
        &self,
        name: impl Into<Arc<str>>,
        job: impl FnOnce() + Send + 'static,
    ) -> Result<(), ScheduleError> {
        self.execution.spawn_blocking(name, job).map_err(Into::into)
    }

    pub(super) fn spawn_blocking_job(
        &self,
        name: Arc<str>,
        job: RuntimeJob,
    ) -> Result<(), ScheduleError> {
        self.execution.spawn_blocking(name, job).map_err(Into::into)
    }

    pub(super) fn spawn_media_job(
        &self,
        name: Arc<str>,
        job: RuntimeJob,
    ) -> Result<(), ScheduleError> {
        self.execution.spawn_network(name, job).map_err(Into::into)
    }
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum ScheduleError {
    #[error("{pool} worker queue is full while scheduling `{job}`")]
    QueueFull { pool: &'static str, job: Arc<str> },
    #[error("{pool} worker queue is stopped while scheduling `{job}`")]
    PoolStopped { pool: &'static str, job: Arc<str> },
}

impl From<ExecutionScheduleError> for ScheduleError {
    fn from(error: ExecutionScheduleError) -> Self {
        match error {
            ExecutionScheduleError::Full { executor, job } => Self::QueueFull {
                pool: executor,
                job,
            },
            ExecutionScheduleError::Stopped { executor, job } => Self::PoolStopped {
                pool: executor,
                job,
            },
        }
    }
}

impl gmpublished_backend::HasErrorKey for ScheduleError {
    fn error_key(&self) -> gmpublished_backend::ErrorKey {
        match self {
            Self::QueueFull { .. } => gmpublished_backend::error_keys::WORKER_QUEUE_FULL,
            Self::PoolStopped { .. } => gmpublished_backend::error_keys::WORKER_POOL_STOPPED,
        }
    }

    fn error_detail(&self) -> Option<String> {
        Some(self.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::ui_error::UiError;
    use gmpublished_backend::HasErrorKey;

    #[test]
    fn scheduler_failures_keep_distinct_wire_meaning() {
        let full = ScheduleError::QueueFull {
            pool: "blocking",
            job: Arc::from("refresh"),
        };
        let stopped = ScheduleError::PoolStopped {
            pool: "network",
            job: Arc::from("thumbnail"),
        };

        assert_eq!(
            full.error_key(),
            gmpublished_backend::error_keys::WORKER_QUEUE_FULL
        );
        assert_eq!(
            stopped.error_key(),
            gmpublished_backend::error_keys::WORKER_POOL_STOPPED
        );
        assert_eq!(
            RunBlockingError::WorkerDropped.error_key(),
            gmpublished_backend::error_keys::WORKER_DROPPED
        );

        let ui = UiError::from(&full);
        assert_eq!(ui.key, gmpublished_backend::error_keys::WORKER_QUEUE_FULL);
        assert_eq!(
            ui.detail.as_deref(),
            Some("blocking worker queue is full while scheduling `refresh`")
        );
    }
}
