/// Outward consequences of a Steam session state transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Effect {
    /// The current-user worker should be started for this generation.
    IdentityFetchRequested(u64),
}
