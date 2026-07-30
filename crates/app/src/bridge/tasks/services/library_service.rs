use super::BackendServices;
use crate::bridge::library::{LibraryRefresh, LibraryRefreshReason, LibrarySnapshot};

/// Installed-addon library capability exposed to app features.
#[derive(Clone, Copy)]
pub struct LibraryService<'a> {
    inner: &'a BackendServices,
}

impl<'a> LibraryService<'a> {
    pub(super) const fn new(inner: &'a BackendServices) -> Self {
        Self { inner }
    }
    pub(crate) fn snapshot(self) -> Option<LibrarySnapshot> {
        self.inner.library.snapshot()
    }
    pub(crate) fn begin_refresh(self, reason: LibraryRefreshReason) -> bool {
        self.inner.library.begin_refresh(reason)
    }
    pub(crate) fn refresh(self, reason: LibraryRefreshReason) -> LibraryRefresh {
        self.inner
            .library
            .refresh_blocking(&self.inner.config().paths(), reason)
    }
    pub(crate) fn abort_refresh(self) -> Option<LibraryRefreshReason> {
        self.inner.library.abort_refresh()
    }
}
