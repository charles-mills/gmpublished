use std::path::PathBuf;

use steamworks::PublishedFileId;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TransactionPayload {
    None,
    WorkshopItem(PublishedFileId),
    TotalBytes(u64),
    ByteSize { source: Option<String>, bytes: u64 },
    ExtractedPath(PathBuf),
    WhitelistViolation { path: String },
    SearchHits(Vec<crate::search::QuickSearchHit>),
}
