use super::BackendServices;
use crate::bridge::library::{LibraryRefresh, LibraryRefreshReason, LibrarySnapshot};

impl BackendServices {
    pub(crate) fn library_snapshot(&self) -> Option<LibrarySnapshot> {
        self.library.snapshot()
    }
    pub(crate) fn begin_library_refresh(&self, reason: LibraryRefreshReason) -> bool {
        self.library.begin_refresh(reason)
    }
    pub(crate) fn refresh_library(&self, reason: LibraryRefreshReason) -> LibraryRefresh {
        self.library.refresh_blocking(&self.paths(), reason)
    }
    pub(crate) fn abort_library_refresh(&self) -> Option<LibraryRefreshReason> {
        self.library.abort_refresh()
    }
}
