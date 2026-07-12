use super::{ContextMenuAction, OpenRequest};

#[derive(Clone, Debug, PartialEq)]
pub enum Message {
    OpenRequested(OpenRequest),
    ActionSelected(ContextMenuAction),
    DismissRequested,
    EscapePressed,
}
