use crate::generation::Generation;
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Effect {
    IdentityFetchRequested(Generation),
}
