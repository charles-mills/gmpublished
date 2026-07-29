use super::ActiveModal;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    /// A modal was replaced in place rather than animating closed, so the
    /// layer's `tick` will never report it as finished. Its feature still owns
    /// live state — staged temp files, thumbnail demands — that only the close
    /// teardown releases.
    Displaced(ActiveModal),
}
