//! Addons the `debug` menu has hidden from the library.
//!
//! A ZST off `debug`, with every method an identity, so call sites stay
//! unconditional and a shipping build carries no filtering at all.

#[cfg(feature = "debug")]
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

#[cfg(feature = "debug")]
use crate::bridge::domain::PublishedFileId;
use crate::bridge::library::LibrarySnapshot;

#[derive(Debug, Default)]
pub(super) struct HiddenAddons {
    #[cfg(feature = "debug")]
    workshop_ids: HashSet<PublishedFileId>,
    #[cfg(feature = "debug")]
    paths: HashSet<PathBuf>,
}

impl HiddenAddons {
    /// Hides an addon by whichever identities it has: a Workshop item has an
    /// id, a local one a path, and an installed Workshop addon has both.
    #[cfg(feature = "debug")]
    pub(super) fn hide(&mut self, workshop_id: Option<PublishedFileId>, path: Option<&Path>) {
        if let Some(workshop_id) = workshop_id {
            self.workshop_ids.insert(workshop_id);
        }
        if let Some(path) = path {
            self.paths.insert(path.to_owned());
        }
    }

    #[cfg(feature = "debug")]
    pub(super) fn contains_workshop_id(&self, workshop_id: PublishedFileId) -> bool {
        self.workshop_ids.contains(&workshop_id)
    }

    #[cfg(feature = "debug")]
    pub(super) const fn workshop_ids(&self) -> &HashSet<PublishedFileId> {
        &self.workshop_ids
    }

    /// The snapshot with hidden addons removed.
    #[cfg(feature = "debug")]
    pub(super) fn visible(&self, snapshot: &LibrarySnapshot) -> LibrarySnapshot {
        LibrarySnapshot {
            addons: snapshot
                .addons
                .iter()
                .filter(|addon| {
                    !self.paths.contains(&addon.path)
                        && !addon
                            .workshop_id
                            .is_some_and(|workshop_id| self.workshop_ids.contains(&workshop_id))
                })
                .cloned()
                .collect::<Vec<_>>()
                .into(),
            epoch: snapshot.epoch,
        }
    }

    #[cfg(not(feature = "debug"))]
    #[expect(
        clippy::unused_self,
        reason = "matches the debug impl, which filters against this set"
    )]
    pub(super) fn visible(&self, snapshot: &LibrarySnapshot) -> LibrarySnapshot {
        snapshot.clone()
    }
}
