use super::*;
use crate::workshop_id::workshop_id;
use std::fs;

use crate::events::{BackendEvent, TransactionEvent, TransactionPayload};

fn test_cpu() -> crate::execution::CpuExecutor {
    crate::execution::CpuExecutor::build(2).expect("test CPU executor")
}

fn gma_for_search(
    path: PathBuf,
    title: &str,
    id: Option<WorkshopId>,
    modified: Option<u64>,
) -> GmaFile {
    let mut gma = GmaFile::for_creation(
        path,
        crate::gma::GmaMetadata::Standard {
            title: title.to_owned(),
            addon_type: "servercontent".to_owned(),
            tags: vec!["build".to_owned(), "fun".to_owned()],
            ignore: Vec::new(),
        },
    );
    if let Some(id) = id {
        gma.set_workshop_id(id);
    }
    gma.set_modified_for_test(modified);
    gma
}

#[test]
fn search_item_new_sorts_terms_and_tracks_max_length() {
    let item = SearchItem::new(
        SearchItemSource::MyWorkshop {
            id: workshop_id(99),
            tags: Vec::new(),
        },
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
        Some(workshop_id(123)),
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
            assert_eq!(id, Some(workshop_id(123)));
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
        Some(workshop_id(77)),
        Some(1),
    );
    let second = gma_for_search(
        dir.path().join("second.gma"),
        "Second",
        Some(workshop_id(77)),
        Some(2),
    );
    let local_only = gma_for_search(dir.path().join("local.gma"), "Local", None, Some(3));
    let search = Search::new(test_cpu());

    search.add_bulk(&[first, second, local_only]);

    assert_eq!(search.index.read().items.len(), 3);
    assert_eq!(search.index.read().installed_addons.len(), 1);
    assert!(
        search
            .index
            .read()
            .installed_addons
            .contains_key(&workshop_id(77))
    );
}

#[test]
fn sync_installed_addons_replaces_installed_subset_only() {
    let search = Search::new(test_cpu());
    search.add(&search_fixture(11, "Workshop Survivor", ["survivor"], 1));
    search.sync_installed_addons(vec![SearchItem::new_installed_addon(
        PathBuf::from("/tmp/old.gma"),
        Some(22),
        "Old Addon".to_owned(),
        vec!["oldterm".to_owned()],
        2_u64,
    )]);

    search.sync_installed_addons(vec![SearchItem::new_installed_addon(
        PathBuf::from("/tmp/new.gma"),
        Some(33),
        "New Addon".to_owned(),
        vec!["newterm".to_owned()],
        3_u64,
    )]);

    let labels = search
        .index
        .read()
        .items
        .iter()
        .map(|item| item.label().to_owned())
        .collect::<Vec<_>>();
    assert!(labels.contains(&"Workshop Survivor".to_owned()));
    assert!(labels.contains(&"New Addon".to_owned()));
    assert!(!labels.contains(&"Old Addon".to_owned()));
}

#[test]
fn sync_installed_addons_added_addon_is_findable_immediately() {
    let search = Search::new(test_cpu());

    search.sync_installed_addons(vec![SearchItem::new_installed_addon(
        PathBuf::from("/tmp/live.gma"),
        Some(44),
        "Live Refresh Addon".to_owned(),
        vec!["needle-live".to_owned()],
        4_u64,
    )]);
    let result = search.quick_search_with_scope("needle-live", SearchScope::Addons);

    assert_eq!(result.hits.len(), 1);
    assert_eq!(result.hits[0].item.label(), "Live Refresh Addon");
}

#[test]
fn sync_installed_addons_removed_addon_stops_matching() {
    let search = Search::new(test_cpu());
    search.sync_installed_addons(vec![SearchItem::new_installed_addon(
        PathBuf::from("/tmp/removed.gma"),
        Some(55),
        "Removed Addon".to_owned(),
        vec!["gone-needle".to_owned()],
        5_u64,
    )]);
    assert_eq!(
        search
            .quick_search_with_scope("gone-needle", SearchScope::Addons)
            .hits
            .len(),
        1
    );

    search.sync_installed_addons(Vec::new());
    let result = search.quick_search_with_scope("gone-needle", SearchScope::Addons);

    assert!(result.hits.is_empty());
}

#[test]
fn sync_installed_addon_files_searches_file_scope_only() {
    let search = Search::new(test_cpu());
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
        Some(workshop_id(66)),
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
            .quick_search_with_scope("rp_riverden", SearchScope::Addons)
            .hits
            .is_empty()
    );
    let result = search.quick_search_with_scope("rp_riverden", SearchScope::Files);

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
            .quick_search_with_scope("bsp", SearchScope::Files)
            .hits
            .len(),
        2
    );
}

#[test]
fn singular_adds_replace_by_identity() {
    let search = Search::new(test_cpu());
    search.add_bulk(&[
        search_fixture(1, "Alpha", ["tool"], 100),
        search_fixture(2, "Beta", ["tool"], 200),
    ]);

    search.add(&search_fixture(1, "Alpha Updated", ["tool"], 300));
    let index = search.index.read();
    let items = &index.items;
    assert_eq!(items.len(), 2);
    assert!(items.iter().any(|item| item.label() == "Alpha Updated"));
    assert!(items.iter().all(|item| item.label() != "Alpha"));
    drop(index);
}

#[derive(Clone)]
struct SearchFixture {
    id: WorkshopId,
    label: String,
    terms: Vec<String>,
    timestamp: u64,
}

impl Searchable for SearchFixture {
    fn search_item(&self) -> Option<SearchItem> {
        Some(SearchItem::new(
            SearchItemSource::MyWorkshop {
                id: self.id,
                tags: Vec::new(),
            },
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
        id: workshop_id(id),
        label: label.into(),
        terms: terms.into_iter().map(Into::into).collect(),
        timestamp,
    }
}

#[test]
fn search_quick_matches_terms_when_label_does_not_match() {
    let search = Search::new(test_cpu());
    search.add_bulk(&[search_fixture(501, "Completely Different", ["needle"], 1)]);

    let result = search.quick_scored("needle", SearchScope::Addons);

    assert_eq!(result.hits.len(), 1);
    assert_eq!(result.hits[0].item.label(), "Completely Different");
    assert!(!result.has_more);
}

#[test]
fn search_quick_multi_word_query_requires_all_atoms() {
    let search = Search::new(test_cpu());
    search.add_bulk(&[
        search_fixture(510, "Wire Model Pack", Vec::<String>::new(), 1),
        search_fixture(511, "Wire Extras", Vec::<String>::new(), 2),
        search_fixture(512, "Model Dump", Vec::<String>::new(), 3),
    ]);

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
    let search = Search::new(test_cpu());
    search.add_bulk(&fixtures);

    let pattern = Pattern::parse("needle", CaseMatching::Ignore, Normalization::Smart);
    let mut expected = search
        .index
        .read()
        .items
        .iter()
        .filter_map(|item| {
            best_search_item_score(&pattern, item).map(|score| QuickSearchHit {
                score,
                item: item.clone(),
            })
        })
        .collect::<Vec<_>>();
    expected.sort_by(quick_hit_order);
    expected.truncate(MAX_QUICK_RESULTS);

    let result = search.quick_scored("needle", SearchScope::Addons);

    assert_eq!(result.hits, expected);
    assert!(result.has_more);

    // Scheduling order must not affect results.
    for _ in 0..20 {
        assert_eq!(
            search.quick_scored("needle", SearchScope::Addons).hits,
            expected
        );
    }
}

#[test]
fn full_search_emits_progress_data_and_finished_transaction_events() {
    let collector = crate::events::BackendEventCollector::default();
    let transactions = crate::transactions::Transactions::new(Arc::new(collector.clone()));
    let search = Search::new(test_cpu());
    search.add_bulk(&[
        search_fixture(701, "Needle One", ["servercontent"], 1),
        search_fixture(702, "Other", ["needle"], 2),
        search_fixture(703, "Unrelated", ["sandbox"], 3),
    ]);
    let transaction = transactions.begin();
    let transaction_id = transaction.id();

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
        SearchItemSource::MyWorkshop {
            id: workshop_id(1),
            tags: Vec::new(),
        },
        SearchItemSource::MyWorkshop {
            id: workshop_id(2),
            tags: Vec::new(),
        },
        SearchItemSource::WorkshopItem(workshop_id(1)),
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
    let search = Search::new(test_cpu());
    search.add(&gma_for_search(
        PathBuf::from("/tmp/refresh.gma"),
        "Old Title",
        Some(workshop_id(77)),
        Some(1),
    ));

    let mut fresh = WorkshopItem::from(workshop_id(77));
    fresh.title = "New Title".to_owned();
    search.refresh_installed_addon_labels(&[fresh]);

    let installed = search
        .index
        .read()
        .installed_addons
        .get(&workshop_id(77))
        .cloned()
        .expect("installed addon entry");
    assert_eq!(installed.label(), "New Title");
    assert!(installed.terms().contains(&"Old Title".to_owned()));

    let items_labels: Vec<String> = search
        .index
        .read()
        .items
        .iter()
        .map(|item| item.label().to_owned())
        .collect();
    assert_eq!(items_labels, vec!["New Title".to_owned()]);

    assert_eq!(
        search
            .quick_search_with_scope("New Title", SearchScope::Addons)
            .hits
            .len(),
        1
    );
    assert_eq!(
        search
            .quick_search_with_scope("Old Title", SearchScope::Addons)
            .hits
            .len(),
        1
    );
}

#[test]
fn language_invalidation_removes_workshop_labels_and_retains_installed_search() {
    let search = Search::new(test_cpu());
    search.add(&search_fixture(11, "Old language", ["old"], 1));
    search.sync_installed_addons(vec![SearchItem::new_installed_addon(
        PathBuf::from("/tmp/local.gma"),
        Some(22),
        "Local addon".into(),
        vec![],
        2_u64,
    )]);
    search.invalidate_workshop_titles();
    let index = search.index.read();
    let items = &index.items;
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].label(), "Local addon");
    drop(index);
    assert!(
        search
            .index
            .read()
            .installed_addons
            .contains_key(&workshop_id(22))
    );
}

#[test]
fn query_operators_do_not_exclude_short_matching_candidates() {
    for (query, label) in [("^cat$", "cat"), ("!dog", "cat"), ("a b", "ab")] {
        let search = Search::new(test_cpu());
        search.add(&search_fixture(1, label, Vec::<String>::new(), 1));
        assert_eq!(
            search
                .quick_search_with_scope(query, SearchScope::Addons)
                .hits
                .len(),
            1,
            "{query:?}"
        );
        let collector = crate::events::BackendEventCollector::default();
        let transactions = crate::transactions::Transactions::new(Arc::new(collector.clone()));
        search.full_scored(query, SearchScope::Addons, &transactions.begin());
        assert!(collector.drain().iter().any(|event| matches!(event,
            BackendEvent::Transaction(TransactionEvent::Data { payload: TransactionPayload::SearchHits(hits), .. }) if hits.len() == 1
        )), "full search must accept {query:?}");
    }
}

#[test]
fn replacements_are_unique_even_before_the_index_is_sorted() {
    let search = Search::new(test_cpu());
    search.add_bulk(&[search_fixture(1, "Old", ["old"], 1)]);
    search.add(&search_fixture(1, "Updated", ["new"], 2));
    search.add_bulk(&[
        search_fixture(2, "Another old", ["old"], 3),
        search_fixture(2, "Another updated", ["new"], 4),
    ]);
    let hits = search
        .quick_search_with_scope("new", SearchScope::Addons)
        .hits;
    assert_eq!(hits.len(), 2);
    assert!(
        search
            .quick_search_with_scope("old", SearchScope::Addons)
            .hits
            .is_empty()
    );
    assert_eq!(search.index.read().items.len(), 2);
}

#[test]
fn concurrent_search_and_replacement_keep_one_entry_per_identity() {
    let search = Search::new(test_cpu());
    std::thread::scope(|scope| {
        for worker in 0..4 {
            let search = &search;
            scope.spawn(move || {
                for revision in 0..50 {
                    search.add_bulk(&[search_fixture(
                        1,
                        format!("version {worker} {revision}"),
                        ["match"],
                        revision,
                    )]);
                    let hits = search
                        .quick_search_with_scope("match", SearchScope::Addons)
                        .hits;
                    assert_eq!(hits.len(), 1);
                }
            });
        }
    });
    assert_eq!(search.index.read().items.len(), 1);
}

#[test]
fn full_search_retains_best_results_and_uses_quick_search_tie_order() {
    let search = Search::new(test_cpu());
    let count = FULL_SEARCH_MAX_HITS as u64 + 50;
    let fixtures = (1..=count)
        .map(|id| search_fixture(id, "Needle", Vec::<String>::new(), id))
        .collect::<Vec<_>>();
    search.add_bulk(&fixtures);
    let quick = search.quick_search_with_scope("needle", SearchScope::Addons);
    let collector = crate::events::BackendEventCollector::default();
    let transactions = crate::transactions::Transactions::new(Arc::new(collector.clone()));
    search.full_scored("needle", SearchScope::Addons, &transactions.begin());
    let events = collector.drain();
    let hits = events
        .iter()
        .find_map(|event| match event {
            BackendEvent::Transaction(TransactionEvent::Data {
                payload: TransactionPayload::SearchHits(hits),
                ..
            }) => Some(hits),
            _ => None,
        })
        .expect("search results");
    assert_eq!(hits.len(), FULL_SEARCH_MAX_HITS);
    assert_eq!(&hits[..MAX_QUICK_RESULTS], quick.hits.as_slice());
    assert_eq!(hits.first().unwrap().item.timestamp, count);
    assert_eq!(hits.last().unwrap().item.timestamp, 51);
}

#[test]
fn full_search_accumulator_stays_bounded_while_better_hits_arrive() {
    let mut hits = BestHits::new(3);
    for score in 0..100 {
        hits.push(QuickSearchHit {
            score,
            item: Arc::new(
                search_fixture(1, "Needle", Vec::<String>::new(), 1)
                    .search_item()
                    .unwrap(),
            ),
        });
        assert!(hits.hits.len() <= 3);
    }
    assert_eq!(
        hits.into_sorted()
            .iter()
            .map(|hit| hit.score)
            .collect::<Vec<_>>(),
        [99, 98, 97]
    );
}

#[test]
fn workshop_search_keeps_tags_separate_from_owner_search_terms() {
    let mut item = WorkshopItem::from(workshop_id(42));
    item.tags = vec!["model".to_owned(), "fun".to_owned()];
    item.steamid = Some(crate::SteamId::from_raw(76561198000000001));
    let indexed = item.search_item().unwrap();
    assert!(indexed.terms().contains(&"76561198000000001".to_owned()));
    assert!(
        matches!(&indexed.source, SearchItemSource::MyWorkshop { id, tags } if *id == workshop_id(42) && tags == &item.tags)
    );
}
