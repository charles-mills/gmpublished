use gmpublished_backend::ExecutionScheduleError;
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
