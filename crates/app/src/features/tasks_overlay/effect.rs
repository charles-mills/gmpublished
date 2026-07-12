use crate::backend::tasks::TaskId;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Effect {
    CancelRequested(TaskId),
}
