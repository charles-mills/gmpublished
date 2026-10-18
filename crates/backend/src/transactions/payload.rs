use std::path::PathBuf;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TransactionPayload {
    None,
    SourcePath(PathBuf),
    TotalBytes(u64),
    ByteSize { source: Option<String>, bytes: u64 },
    ExtractedPath(PathBuf),
    WhitelistViolation { path: String },
    SearchHits(Vec<crate::search::QuickSearchHit>),
}
