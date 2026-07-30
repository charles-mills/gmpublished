//! In-flight Workshop snapshot ownership.

use super::{PathBuf, PublishedFileId};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct WorkshopContentLoad {
    pub(super) workshop_id: PublishedFileId,
    pub(super) destination: PathBuf,
}
