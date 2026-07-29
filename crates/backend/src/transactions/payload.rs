use std::path::PathBuf;

use crate::WorkshopId;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TransactionPayload {
    None,
    WorkshopItem(WorkshopId),
    TotalBytes(u64),
    ByteSize { source: Option<String>, bytes: u64 },
    ExtractedPath(PathBuf),
    WhitelistViolation { path: String },
    SearchHits(Vec<crate::search::QuickSearchHit>),
}
