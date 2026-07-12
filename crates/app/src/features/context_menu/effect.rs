use super::state::ContextMenuAction;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Effect {
    ActionSelected(ContextMenuAction),
    Dismissed,
}
