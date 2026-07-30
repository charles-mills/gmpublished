//! Search thumbnail state and demand ownership.

use super::*;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RowThumbnail {
    Loading,
    Dead,
    Ready(image::Handle),
}

pub(super) fn thumbnail_owner() -> thumbnail_demand::Owner {
    thumbnail_demand::Owner::Search
}
