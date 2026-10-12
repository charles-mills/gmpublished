use std::fmt;

/// `INITIAL` is never issued for a request, so pre-bump replies cannot match.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Generation(u64);

impl Generation {
    pub const INITIAL: Self = Self(0);

    /// Skip INITIAL on wraparound. Saturating would make all later replies share a generation.
    #[must_use]
    pub const fn next(self) -> Self {
        Self(match self.0.wrapping_add(1) {
            0 => 1,
            next => next,
        })
    }

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
