use super::*;
use std::fs;

use crate::events::{BackendEvent, TransactionEvent, TransactionPayload};

fn gma_for_search(
    path: PathBuf,
    title: &str,
    id: Option<PublishedFileId>,
    modified: Option<u64>,
) -> GMAFile {
    GMAFile {
        path,
        size: 0,
        id,
        metadata: crate::gma::GMAMetadata::Standard {
            title: title.to_owned(),
            addon_type: "servercontent".to_owned(),
            tags: vec!["build".to_owned(), "fun".to_owned()],
            ignore: Vec::new(),
        },
        version: 3,
        extracted_name: String::new(),
        modified,
    }
}

#[test]
fn search_item_new_sorts_terms_and_tracks_max_length() {
    let item = SearchItem::new(
        SearchItemSource::MyWorkshop(PublishedFileId(99)),
        "Medium".to_string(),
        vec![
            "longest-term".to_string(),
            "x".to_string(),
            "mid".to_string(),
        ],
        7_u64,
    );

    assert_eq!(
        item.terms(),
        &[
            "x".to_string(),
            "mid".to_string(),
            "longest-term".to_string()
        ]
    );
    assert_eq!(item.len, "longest-term".len());
    assert_eq!(item.timestamp, 7);
}

#[test]
fn gma_search_item_uses_metadata_terms_id_and_canonical_path() {
    let dir = tempfile::tempdir().expect("tempdir");
    let gma_path = dir.path().join("fixture.gma");
    fs::write(&gma_path, "path marker").expect("gma marker");
    let gma = gma_for_search(
        gma_path.clone(),
        "Search Fixture",
        Some(PublishedFileId(123)),
        Some(55),
    );

    let item = gma.search_item().expect("search item");

    assert_eq!(item.label(), "Search Fixture");
    assert_eq!(item.timestamp, 55);
    assert!(item.terms().contains(&"build".to_string()));
    assert!(item.terms().contains(&"fun".to_string()));
    assert!(item.terms().contains(&"servercontent".to_string()));
    assert!(item.terms().contains(&"123".to_string()));
    match item.source {
        SearchItemSource::InstalledAddons(path, id) => {
            assert_eq!(path, dunce::canonicalize(gma_path).expect("canonical path"));
            assert_eq!(id, Some(PublishedFileId(123)));
        }
        _ => panic!("expected installed addon source"),
    }
}

#[test]
fn search_add_bulk_indexes_installed_addons_and_deduplicates_workshop_ids() {
    let dir = tempfile::tempdir().expect("tempdir");
    let first = gma_for_search(
        dir.path().join("first.gma"),
        "First",
        Some(PublishedFileId(77)),
        Some(1),
    );
    let second = gma_for_search(
        dir.path().join("second.gma"),
        "Second",
        Some(PublishedFileId(77)),
        Some(2),
    );
    let local_only = gma_for_search(dir.path().join("local.gma"), "Local", None, Some(3));
    let search = Search::new();

    search.add_bulk(&[first, second, local_only]);
    search.dirty();

    assert_eq!(search.items.read().len(), 3);
    assert_eq!(search.installed_addons.read().len(), 1);
    assert!(
        search
            .installed_addons
            .read()
            .contains_key(&PublishedFileId(77))
    );
}

#[test]
fn sync_installed_addons_replaces_installed_subset_only() {
    let search = Search::new();
    search.add(&search_fixture(11, "Workshop Survivor", ["survivor"], 1));
    search.sync_installed_addons(vec![SearchItem::new_installed_addon(
        PathBuf::from("/tmp/old.gma"),
        Some(22),
        "Old Addon".to_owned(),
        vec!["oldterm".to_owned()],
        2_u64,
    )]);
    search.dirty();

    search.sync_installed_addons(vec![SearchItem::new_installed_addon(
        PathBuf::from("/tmp/new.gma"),
        Some(33),
        "New Addon".to_owned(),
        vec!["newterm".to_owned()],
        3_u64,
    )]);
    search.dirty();

    let labels = search
        .items
        .read()
        .iter()
        .map(|item| item.label().to_owned())
        .collect::<Vec<_>>();
    assert!(labels.contains(&"Workshop Survivor".to_owned()));
    assert!(labels.contains(&"New Addon".to_owned()));
    assert!(!labels.contains(&"Old Addon".to_owned()));
}

#[test]
fn sync_installed_addons_added_addon_is_findable_immediately() {
    let search = Search::new();

    search.sync_installed_addons(vec![SearchItem::new_installed_addon(
        PathBuf::from("/tmp/live.gma"),
        Some(44),
        "Live Refresh Addon".to_owned(),
        vec!["needle-live".to_owned()],
        4_u64,
    )]);
    let result = search.quick_search("needle-live".to_owned());

    assert_eq!(result.hits.len(), 1);
    assert_eq!(result.hits[0].item.label(), "Live Refresh Addon");
}

#[test]
fn sync_installed_addons_removed_addon_stops_matching() {
    let search = Search::new();
    search.sync_installed_addons(vec![SearchItem::new_installed_addon(
        PathBuf::from("/tmp/removed.gma"),
        Some(55),
        "Removed Addon".to_owned(),
        vec!["gone-needle".to_owned()],
        5_u64,
    )]);
    assert_eq!(search.quick_search("gone-needle".to_owned()).hits.len(), 1);

    search.sync_installed_addons(Vec::new());
    let result = search.quick_search("gone-needle".to_owned());

    assert!(result.hits.is_empty());
}

#[test]
fn sync_installed_addon_files_searches_file_scope_only() {
    let search = Search::new();
    search.sync_installed_addons(vec![SearchItem::new_installed_addon(
        PathBuf::from("/tmp/riverden.gma"),
        Some(66),
        "Riverden Addon".to_owned(),
        vec!["roleplay".to_owned()],
        6_u64,
    )]);
    let riverden = FileSearchAddon::new(
        PathBuf::from("/tmp/riverden.gma"),
        "Riverden Addon".to_owned(),
        Some(66),
    );
    search.sync_installed_addon_files(vec![
        SearchItem::new_installed_addon_file(
            riverden.clone(),
            "maps/rp_riverden_v1a.bsp".to_owned(),
            123,
            456,
            6_u64,
        ),
        SearchItem::new_installed_addon_file(
            riverden,
            "maps/gm_flatgrass.bsp".to_owned(),
            123,
            789,
            6_u64,
        ),
    ]);

    assert!(
        search
            .quick_search("rp_riverden".to_owned())
            .hits
            .is_empty()
    );
    let result = search.quick_search_with_scope("rp_riverden".to_owned(), SearchScope::Files);

    assert_eq!(result.hits.len(), 1);
    match &result.hits[0].item.source {
        SearchItemSource::InstalledAddonFile {
            entry_path, addon, ..
        } => {
            assert_eq!(entry_path, "maps/rp_riverden_v1a.bsp");
            assert_eq!(addon.title, "Riverden Addon");
        }
        source => panic!("expected file source, got {source:?}"),
    }
    assert_eq!(
        search
            .quick_search_with_scope("bsp".to_owned(), SearchScope::Files)
            .hits
            .len(),
        2
    );
}

#[test]
fn dirty_sorts_once_then_singular_adds_replace_by_identity() {
    let search = Search::new();
    search.add_bulk(&[
        search_fixture(1, "Alpha", ["tool"], 100),
        search_fixture(2, "Beta", ["tool"], 200),
    ]);

    // The first query sorts; the flag must clear so later queries skip it.
    search.dirty();
    assert!(!search.dirty.load(std::sync::atomic::Ordering::Acquire));

    // Same source, new timestamp: the old entry is replaced, not
    // duplicated (Ord-positional replacement would miss it because the
    // timestamp moved).
    search.add(&search_fixture(1, "Alpha Updated", ["tool"], 300));
    let items = search.items.read();
    assert_eq!(items.len(), 2);
    assert!(items.iter().any(|item| item.label() == "Alpha Updated"));
    assert!(items.iter().all(|item| item.label() != "Alpha"));
    // Newest-first index order held through the sorted insert.
    assert!(items.windows(2).all(|pair| pair[0] <= pair[1]));
    drop(items);
}

#[derive(Clone)]
struct SearchFixture {
    id: PublishedFileId,
    label: String,
    terms: Vec<String>,
    timestamp: u64,
}

impl Searchable for SearchFixture {
    fn search_item(&self) -> Option<SearchItem> {
        Some(SearchItem::new(
            SearchItemSource::MyWorkshop(self.id),
            self.label.clone(),
            self.terms.clone(),
            self.timestamp,
        ))
    }
}

fn search_fixture(
    id: u64,
    label: impl Into<String>,
    terms: impl IntoIterator<Item = impl Into<String>>,
    timestamp: u64,
) -> SearchFixture {
    SearchFixture {
        id: PublishedFileId(id),
        label: label.into(),
        terms: terms.into_iter().map(Into::into).collect(),
        timestamp,
    }
}

#[test]
fn search_quick_matches_terms_when_label_does_not_match() {
    let search = Search::new();
    search.add_bulk(&[search_fixture(501, "Completely Different", ["needle"], 1)]);
    search.dirty();

    let result = search.quick_scored("needle", SearchScope::Addons);

    assert_eq!(result.hits.len(), 1);
    assert_eq!(result.hits[0].item.label(), "Completely Different");
    assert!(!result.has_more);
}

#[test]
fn search_quick_multi_word_query_requires_all_atoms() {
    let search = Search::new();
    search.add_bulk(&[
        search_fixture(510, "Wire Model Pack", Vec::<String>::new(), 1),
        search_fixture(511, "Wire Extras", Vec::<String>::new(), 2),
        search_fixture(512, "Model Dump", Vec::<String>::new(), 3),
    ]);
    search.dirty();

    let result = search.quick_scored("wire model", SearchScope::Addons);

    assert_eq!(result.hits.len(), 1);
    assert_eq!(result.hits[0].item.label(), "Wire Model Pack");
}

#[test]
fn search_quick_caps_results_reports_has_more_and_orders_by_score() {
    let fixtures = (0..12)
        .map(|index| {
            search_fixture(
                600 + index,
                format!("Needle Result {index:02}"),
                Vec::<String>::new(),
                index,
            )
        })
        .collect::<Vec<_>>();
    let search = Search::new();
    search.add_bulk(&fixtures);
    search.dirty();

    let result = search.quick_scored("needle", SearchScope::Addons);

    assert_eq!(result.hits.len(), usize::from(MAX_QUICK_RESULTS));
    assert!(result.has_more);
    assert!(
        result
            .hits
            .windows(2)
            .all(|window| window[0].score >= window[1].score)
    );
}

#[test]
fn full_search_emits_progress_data_and_finished_transaction_events() {
    let collector = crate::events::BackendEventCollector::default();
    let transactions = crate::transactions::Transactions::new(Arc::new(collector.clone()), false);
    let search = Search::new();
    search.add_bulk(&[
        search_fixture(701, "Needle One", ["servercontent"], 1),
        search_fixture(702, "Other", ["needle"], 2),
        search_fixture(703, "Unrelated", ["sandbox"], 3),
    ]);
    search.dirty();
    let transaction = transactions.begin();
    let transaction_id = transaction.id;

    search.full_scored("needle", SearchScope::Addons, &transaction);

    let events = collector.drain();
    assert!(events.iter().any(|event| matches!(
        event,
        BackendEvent::Transaction(TransactionEvent::Progress { id, .. }) if *id == transaction_id
    )));
    let data_events = events
        .iter()
        .filter_map(|event| match event {
            BackendEvent::Transaction(TransactionEvent::Data { id, payload })
                if *id == transaction_id =>
            {
                Some(payload)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(data_events.len(), 1);
    assert!(matches!(
        data_events[0],
        TransactionPayload::SearchHits(hits) if hits.len() == 2
    ));
    assert!(matches!(
        events.last(),
        Some(BackendEvent::Transaction(TransactionEvent::Finished { id, payload }))
            if *id == transaction_id && payload == &TransactionPayload::None
    ));
}

#[test]
fn eq_matches_ord_across_a_matrix_of_key_components() {
    let sources = [
        SearchItemSource::MyWorkshop(PublishedFileId(1)),
        SearchItemSource::MyWorkshop(PublishedFileId(2)),
        SearchItemSource::WorkshopItem(PublishedFileId(1)),
    ];
    let timestamps = [10_u64, 20_u64];
    let labels = ["abc", "abcde"];

    let mut items = Vec::new();
    for source in &sources {
        for timestamp in timestamps {
            for label in labels {
                items.push(SearchItem::new(
                    source.clone(),
                    (*label).to_owned(),
                    Vec::new(),
                    timestamp,
                ));
            }
        }
    }

    for a in &items {
        for b in &items {
            assert_eq!(
                a == b,
                a.cmp(b) == std::cmp::Ordering::Equal,
                "eq/cmp disagreement: {a:?} vs {b:?}"
            );
        }
    }
}

#[test]
fn refresh_installed_addon_labels_replaces_stale_label_everywhere() {
    let search = Search::new();
    search.add(&gma_for_search(
        PathBuf::from("/tmp/refresh.gma"),
        "Old Title",
        Some(PublishedFileId(77)),
        Some(1),
    ));

    let mut fresh = WorkshopItem::from(PublishedFileId(77));
    fresh.title = "New Title".to_owned();
    search.refresh_installed_addon_labels(&[fresh]);

    let installed = search
        .installed_addons
        .read()
        .get(&PublishedFileId(77))
        .cloned()
        .expect("installed addon entry");
    assert_eq!(installed.label(), "New Title");
    assert!(installed.terms().contains(&"Old Title".to_owned()));

    // The replacement lands in `items` too, not just `installed_addons`.
    let items_labels: Vec<String> = search
        .items
        .read()
        .iter()
        .map(|item| item.label().to_owned())
        .collect();
    assert_eq!(items_labels, vec!["New Title".to_owned()]);

    // The stale label stays searchable as a term; the fresh label is
    // searchable as the new label itself.
    assert_eq!(search.quick_search("New Title".to_owned()).hits.len(), 1);
    assert_eq!(search.quick_search("Old Title".to_owned()).hits.len(), 1);
}
