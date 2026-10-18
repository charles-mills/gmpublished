use crate::preparation::PreparationCancellation;
pub use gmpublished_backend::source_discovery::{
    DiscoveryFailureKind, SourceCategory, SourceDiscoveryFailure, SourceDiscoveryLimits,
    SourceDiscoveryStats,
};
use gmpublished_backend::source_discovery::{
    SourceChild, SourceDiscoveryBudget, SourceDiscoveryError,
};
use std::path::Path;

pub(super) struct Discovery<'a> {
    pub budget: &'a mut SourceDiscoveryBudget,
    pub cancellation: &'a PreparationCancellation,
    pub category: SourceCategory,
}
impl Discovery<'_> {
    pub fn check(&self) -> Result<(), SourceDiscoveryError> {
        SourceDiscoveryBudget::check(&|| self.cancellation.is_cancelled())
    }
    pub fn children(&mut self, path: &Path) -> Result<Vec<SourceChild>, SourceDiscoveryError> {
        self.budget
            .sorted_children(self.category, path, &|| self.cancellation.is_cancelled())
    }
    pub fn is_dir(&self, path: &Path) -> Result<bool, SourceDiscoveryError> {
        self.check()?;
        self.budget
            .metadata(self.category, path)
            .map(|metadata| metadata.is_some_and(|metadata| metadata.is_dir()))
    }
    pub fn child_dir(&self, child: &SourceChild) -> Result<bool, SourceDiscoveryError> {
        self.check()?;
        self.budget.child_is_dir(self.category, child)
    }
    pub fn child_file(&self, child: &SourceChild) -> Result<bool, SourceDiscoveryError> {
        self.check()?;
        self.budget.child_is_file(self.category, child)
    }
    pub fn reserve<T>(
        &mut self,
        path: &Path,
        sources: &mut Vec<T>,
    ) -> Result<(), SourceDiscoveryError> {
        self.check()?;
        self.budget.reserve_source(self.category, path, sources)
    }
}

pub(super) fn category_result<T: Default>(
    result: Result<T, SourceDiscoveryError>,
    budget: &mut SourceDiscoveryBudget,
    failures: &mut std::collections::BTreeMap<super::ContentSourceTier, SourceDiscoveryFailure>,
    tiers: &[super::ContentSourceTier],
) -> Result<T, crate::preparation::Cancelled> {
    match result {
        Ok(value) => Ok(value),
        Err(SourceDiscoveryError::Cancelled) => Err(crate::preparation::Cancelled),
        Err(SourceDiscoveryError::Unavailable(error)) => {
            for tier in tiers {
                failures.insert(*tier, error.clone());
            }
            budget.fail(error);
            Ok(T::default())
        }
    }
}

#[cfg(test)]
mod tests;
