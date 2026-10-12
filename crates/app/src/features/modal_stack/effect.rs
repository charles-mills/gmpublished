use super::ActiveModal;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Effect {
    /// A replaced modal never finishes a close animation. Run its teardown now
    /// to release staged files and thumbnail demands.
    Displaced(ActiveModal),
}
