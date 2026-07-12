use crate::backend::tasks::{TaskEvent, TaskId};

#[derive(Clone, Debug)]
pub enum Message {
    TaskEventsReceived(Vec<TaskEvent>),
    CancelPressed(TaskId),
}
