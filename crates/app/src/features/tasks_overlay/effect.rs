use crate::bridge::tasks::TaskId;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Effect {
    CancelRequested(TaskId),
}
