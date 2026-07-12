//! Keyless Steam Web API preflight for Workshop downloads.
//!
//! Resolves collections and fetches per-item download facts in a few
//! batched HTTP calls. Garry's Mod items last updated before the ~2020
//! SteamPipe migration keep a public CDN `file_url` serving the legacy
//! LZMA payload; those download over plain HTTPS in parallel instead of
//! the Steam client's serial download queue.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use steamworks::PublishedFileId;

const API_BASE: &str = "https://api.steampowered.com/ISteamRemoteStorage";
const BATCH_SIZE: usize = 100;

/// Collections nested deeper than this stop being expanded; leftovers are
/// passed to the details call as plain items.
const MAX_COLLECTION_DEPTH: usize = 8;

/// `EWorkshopFileType` value marking a child row as a nested collection.
const FILETYPE_COLLECTION: u64 = 2;

/// `EResult` success.
const RESULT_OK: u64 = 1;

#[derive(Debug, thiserror::Error)]
pub(super) enum WebApiError {
    #[error("request failed: {0}")]
    Http(Box<ureq::Error>),
    #[error("malformed response: {0}")]
    Parse(&'static str),
}

impl From<ureq::Error> for WebApiError {
    fn from(error: ureq::Error) -> Self {
        Self::Http(Box::new(error))
    }
}

/// Per-item facts from `GetPublishedFileDetails`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct PublishedFileDetail {
    pub id: PublishedFileId,
    /// Steam reported the item exists (`result == 1`).
    pub found: bool,
    /// Public CDN URL of the legacy LZMA payload; `None` for SteamPipe
    /// items, which only the Steam client can download.
    pub file_url: Option<String>,
    pub file_size: u64,
}

impl PublishedFileDetail {
    fn missing(id: PublishedFileId) -> Self {
        Self {
            id,
            found: false,
            file_url: None,
            file_size: 0,
        }
    }
}

fn api_agent() -> ureq::Agent {
    // The workspace ureq build carries no bundled webpki roots; certificate
    // verification must go through the OS trust store (PlatformVerifier).
    ureq::Agent::config_builder()
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                .build(),
        )
        .timeout_global(Some(Duration::from_secs(30)))
        .build()
        .into()
}

/// Agent for CDN payload downloads: no global deadline (large addons on
/// slow lines take minutes), but bounded connect/response latency.
pub(super) fn download_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                .build(),
        )
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_recv_response(Some(Duration::from_secs(30)))
        .build()
        .into()
}

/// Expands collections (recursively, deduplicated) and returns details for
/// every reachable item, in stable order: `known_items` first, then
/// discovered items. Performs all network calls upfront so a failure has
/// no side effects and callers can fall back to Steamworks queries.
pub(super) fn resolve_downloads(
    known_items: Vec<PublishedFileId>,
    possible_collections: Vec<PublishedFileId>,
) -> Result<Vec<PublishedFileDetail>, WebApiError> {
    let agent = api_agent();

    let mut resolution = CollectionResolution::new(known_items, possible_collections);
    for _ in 0..MAX_COLLECTION_DEPTH {
        let frontier = std::mem::take(&mut resolution.frontier);
        if frontier.is_empty() {
            break;
        }
        for batch in frontier.chunks(BATCH_SIZE) {
            let json = post_ids(&agent, "GetCollectionDetails", "collectioncount", batch)?;
            resolution.apply_rows(batch, &parse_collection_details(&json)?);
        }
    }
    let mut item_ids = resolution.items;
    item_ids.append(&mut resolution.frontier);

    let mut details = Vec::with_capacity(item_ids.len());
    for batch in item_ids.chunks(BATCH_SIZE) {
        let json = post_ids(&agent, "GetPublishedFileDetails", "itemcount", batch)?;
        details.extend(align_details(batch, parse_published_file_details(&json)?));
    }
    Ok(details)
}

fn post_ids(
    agent: &ureq::Agent,
    method: &str,
    count_key: &str,
    ids: &[PublishedFileId],
) -> Result<String, WebApiError> {
    let mut form = Vec::with_capacity(ids.len() + 1);
    form.push((count_key.to_owned(), ids.len().to_string()));
    for (i, id) in ids.iter().enumerate() {
        form.push((format!("publishedfileids[{i}]"), id.0.to_string()));
    }

    agent
        .post(format!("{API_BASE}/{method}/v1/"))
        .send_form(form)?
        .body_mut()
        .with_config()
        .limit(64 * 1024 * 1024)
        .read_to_string()
        .map_err(|_| WebApiError::Parse("unreadable response body"))
}

struct CollectionRow {
    id: PublishedFileId,
    /// `None` for plain items; children as `(id, is_collection)` otherwise.
    children: Option<Vec<(PublishedFileId, bool)>>,
}

/// Frontier-at-a-time collection expansion with global deduplication.
struct CollectionResolution {
    seen: HashSet<PublishedFileId>,
    items: Vec<PublishedFileId>,
    frontier: Vec<PublishedFileId>,
}

impl CollectionResolution {
    fn new(known_items: Vec<PublishedFileId>, possible_collections: Vec<PublishedFileId>) -> Self {
        let mut seen: HashSet<PublishedFileId> = known_items.iter().copied().collect();
        seen.extend(possible_collections.iter().copied());
        Self {
            seen,
            items: known_items,
            frontier: possible_collections,
        }
    }

    fn apply_rows(&mut self, requested: &[PublishedFileId], rows: &[CollectionRow]) {
        let children_by_id: HashMap<PublishedFileId, &Option<Vec<(PublishedFileId, bool)>>> =
            rows.iter().map(|row| (row.id, &row.children)).collect();

        for id in requested {
            // Absent rows and non-collection rows both mean "treat as item".
            let Some(Some(children)) = children_by_id.get(id) else {
                self.items.push(*id);
                continue;
            };
            for (child, is_collection) in children {
                if !self.seen.insert(*child) {
                    continue;
                }
                if *is_collection {
                    self.frontier.push(*child);
                } else {
                    self.items.push(*child);
                }
            }
        }
    }
}

fn value_as_u64(value: &serde_json::Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|s| s.parse().ok()))
}

fn response_rows<'a>(
    root: &'a serde_json::Value,
    key: &str,
) -> Result<&'a Vec<serde_json::Value>, WebApiError> {
    root.get("response")
        .and_then(|response| response.get(key))
        .and_then(serde_json::Value::as_array)
        .ok_or(WebApiError::Parse("missing details array"))
}

fn row_id(row: &serde_json::Value) -> Result<PublishedFileId, WebApiError> {
    row.get("publishedfileid")
        .and_then(value_as_u64)
        .map(PublishedFileId)
        .ok_or(WebApiError::Parse("missing publishedfileid"))
}

fn parse_collection_details(json: &str) -> Result<Vec<CollectionRow>, WebApiError> {
    let root: serde_json::Value =
        serde_json::from_str(json).map_err(|_| WebApiError::Parse("invalid JSON"))?;

    response_rows(&root, "collectiondetails")?
        .iter()
        .map(|row| {
            let id = row_id(row)?;
            // Plain items report result 9 (file not found) here; only real
            // collections return success.
            let is_collection = row.get("result").and_then(value_as_u64) == Some(RESULT_OK);
            let children = is_collection.then(|| {
                row.get("children")
                    .and_then(serde_json::Value::as_array)
                    .map(|children| {
                        children
                            .iter()
                            .filter_map(|child| {
                                let id = child.get("publishedfileid").and_then(value_as_u64)?;
                                let is_collection = child.get("filetype").and_then(value_as_u64)
                                    == Some(FILETYPE_COLLECTION);
                                Some((PublishedFileId(id), is_collection))
                            })
                            .collect()
                    })
                    .unwrap_or_default()
            });
            Ok(CollectionRow { id, children })
        })
        .collect()
}

fn parse_published_file_details(json: &str) -> Result<Vec<PublishedFileDetail>, WebApiError> {
    let root: serde_json::Value =
        serde_json::from_str(json).map_err(|_| WebApiError::Parse("invalid JSON"))?;

    response_rows(&root, "publishedfiledetails")?
        .iter()
        .map(|row| {
            let id = row_id(row)?;
            let found = row.get("result").and_then(value_as_u64) == Some(RESULT_OK);
            let file_url = row
                .get("file_url")
                .and_then(serde_json::Value::as_str)
                .filter(|url| !url.is_empty())
                .map(str::to_owned);
            let file_size = row.get("file_size").and_then(value_as_u64).unwrap_or(0);
            Ok(PublishedFileDetail {
                id,
                found,
                file_url,
                file_size,
            })
        })
        .collect()
}

/// Re-keys parsed rows onto the requested id order; ids Steam omitted from
/// the response come back as not-found.
fn align_details(
    requested: &[PublishedFileId],
    rows: Vec<PublishedFileDetail>,
) -> Vec<PublishedFileDetail> {
    let mut by_id: HashMap<PublishedFileId, PublishedFileDetail> =
        rows.into_iter().map(|detail| (detail.id, detail)).collect();
    requested
        .iter()
        .map(|id| {
            by_id
                .remove(id)
                .unwrap_or_else(|| PublishedFileDetail::missing(*id))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(id: u64) -> PublishedFileId {
        PublishedFileId(id)
    }

    #[test]
    fn collection_details_distinguishes_items_collections_and_nesting() {
        let rows = parse_collection_details(
            r#"{"response":{"result":1,"resultcount":3,"collectiondetails":[
                {"publishedfileid":"10","result":9},
                {"publishedfileid":"20","result":1,"children":[
                    {"publishedfileid":"21","sortorder":0,"filetype":0},
                    {"publishedfileid":"22","sortorder":1,"filetype":2}
                ]},
                {"publishedfileid":"30","result":1}
            ]}}"#,
        )
        .expect("parse");

        assert_eq!(rows[0].id, id(10));
        assert!(rows[0].children.is_none());
        assert_eq!(
            rows[1].children,
            Some(vec![(id(21), false), (id(22), true)])
        );
        assert_eq!(rows[2].children, Some(Vec::new()));
    }

    #[test]
    fn published_file_details_parses_string_sizes_and_empty_urls() {
        let details = parse_published_file_details(
            r#"{"response":{"result":1,"resultcount":3,"publishedfiledetails":[
                {"publishedfileid":"1","result":1,"file_size":"20670035","file_url":""},
                {"publishedfileid":"2","result":1,"file_size":123,"file_url":"https://cdn.example/ugc/x/"},
                {"publishedfileid":"3","result":9}
            ]}}"#,
        )
        .expect("parse");

        assert_eq!(
            details,
            vec![
                PublishedFileDetail {
                    id: id(1),
                    found: true,
                    file_url: None,
                    file_size: 20_670_035,
                },
                PublishedFileDetail {
                    id: id(2),
                    found: true,
                    file_url: Some("https://cdn.example/ugc/x/".to_owned()),
                    file_size: 123,
                },
                PublishedFileDetail {
                    id: id(3),
                    found: false,
                    file_url: None,
                    file_size: 0,
                },
            ]
        );
    }

    #[test]
    fn malformed_payloads_are_parse_errors() {
        assert!(matches!(
            parse_published_file_details("not json"),
            Err(WebApiError::Parse(_))
        ));
        assert!(matches!(
            parse_collection_details(r#"{"response":{}}"#),
            Err(WebApiError::Parse(_))
        ));
    }

    #[test]
    fn collection_resolution_expands_nested_collections_once() {
        let mut resolution = CollectionResolution::new(vec![id(1)], vec![id(20), id(40)]);

        // Level 0: id 20 is a collection containing an item, a nested
        // collection, and a duplicate of the already-known item 1; id 40
        // has no row at all (treated as an item).
        let frontier = std::mem::take(&mut resolution.frontier);
        assert_eq!(frontier, vec![id(20), id(40)]);
        resolution.apply_rows(
            &frontier,
            &[CollectionRow {
                id: id(20),
                children: Some(vec![(id(21), false), (id(22), true), (id(1), false)]),
            }],
        );
        assert_eq!(resolution.frontier, vec![id(22)]);
        assert_eq!(resolution.items, vec![id(1), id(21), id(40)]);

        // Level 1: nested collection resolves to one new item plus a
        // duplicate of 21, which must not repeat.
        let frontier = std::mem::take(&mut resolution.frontier);
        resolution.apply_rows(
            &frontier,
            &[CollectionRow {
                id: id(22),
                children: Some(vec![(id(23), false), (id(21), false)]),
            }],
        );
        assert!(resolution.frontier.is_empty());
        assert_eq!(resolution.items, vec![id(1), id(21), id(40), id(23)]);
    }

    #[test]
    fn details_align_to_requested_order_with_missing_rows_marked_not_found() {
        let aligned = align_details(
            &[id(1), id(2)],
            vec![PublishedFileDetail {
                id: id(2),
                found: true,
                file_url: None,
                file_size: 7,
            }],
        );

        assert_eq!(aligned[0], PublishedFileDetail::missing(id(1)));
        assert_eq!(aligned[1].id, id(2));
        assert!(aligned[1].found);
    }
}
