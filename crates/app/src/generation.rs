//! Monotonic tokens used to discard results from superseded requests.

use std::fmt;

/// A request generation.
///
/// State that issues async work stamps each request with a generation and
/// discards replies that do not carry the current one. [`Self::INITIAL`] is
/// never re-issued, so a reply minted before the first bump can never match a
/// live request.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Generation(u64);

impl Generation {
    /// Before anything has been issued.
    pub const INITIAL: Self = Self(0);

    /// The next generation, skipping [`Self::INITIAL`] on wraparound.
    ///
    /// Wrapping rather than saturating: saturating would stall at `u64::MAX`
    /// and make every later reply look current.
    #[must_use]
    pub const fn next(self) -> Self {
        Self(match self.0.wrapping_add(1) {
            0 => 1,
            next => next,
        })
    }

    /// Steps this generation in place and returns the new value.
    pub const fn bump(&mut self) -> Self {
        *self = self.next();
        *self
    }

    #[cfg(test)]
    pub(crate) const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }
}

impl fmt::Display for Generation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[cfg(test)]
mod tests {
    use super::Generation;

    #[test]
    fn next_never_returns_initial() {
        assert_ne!(Generation::INITIAL.next(), Generation::INITIAL);
        assert_eq!(
            Generation::from_raw(u64::MAX).next(),
            Generation::from_raw(1)
        );
    }

    #[test]
    fn bump_advances_and_returns_the_new_value() {
        let mut generation = Generation::INITIAL;

        assert_eq!(generation.bump(), Generation::from_raw(1));
        assert_eq!(generation, Generation::from_raw(1));
        assert_eq!(generation.bump(), Generation::from_raw(2));
    }
}
