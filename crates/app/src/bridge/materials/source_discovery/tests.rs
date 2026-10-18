use super::super::*;
use crate::test_support::{GmaFixtureBuilder, write_gma_fixture};
use gmpublished_backend::source_discovery::{
    DiscoveryFailureKind, DiscoveryResource, SourceCategory,
};

fn resolver(
    game: &Path,
    limits: SourceDiscoveryLimits,
    addon: Arc<PreviewArchiveSource>,
) -> MaterialResolver {
    MaterialResolver::new_with_source_policy_cancellable(
        addon,
        Some(game.to_owned()),
        vformats::ArchiveIndexLimits::default(),
        Arc::new(TextureWorkingBudget::new(8 * 1024 * 1024)),
        limits,
        crate::preparation::PreparationCancellation::default(),
    )
    .unwrap()
}
fn empty() -> Arc<PreviewArchiveSource> {
    PreviewArchiveSource::from_folder([])
}

#[test]
fn unavailable_loose_category_stops_material_and_texture_lookup_but_preserves_addon() {
    let root = tempfile::tempdir().unwrap();
    let game = root.path().join("game");
    fs::create_dir_all(game.join("garrysmod")).unwrap();
    fs::write(game.join("garrysmod/addons"), b"not a directory").unwrap();
    let source_file = root.path().join("early.vmt");
    fs::write(&source_file, b"UnlitGeneric {}").unwrap();
    let addon =
        PreviewArchiveSource::from_folder([("materials/early.vmt".to_owned(), 16, source_file)]);
    let resolver = resolver(&game, Default::default(), addon);
    assert!(matches!(
        resolver.resolve_with_base2(&[], "early"),
        MaterialResolution::Loaded { .. }
    ));
    assert!(matches!(
        resolver.resolve_primary(&[], "later"),
        MaterialResolution::SourceUnavailable {
            error: SourceDiscoveryFailure {
                category: SourceCategory::Loose,
                kind: DiscoveryFailureKind::Io { .. },
                ..
            }
        }
    ));
    assert!(matches!(
        resolver.selected_texture_asset(&ContentPath::new("materials/later.vtf").unwrap()),
        Err(TextureSourceError::SourceUnavailable(_))
    ));
    assert!(resolver.shared.game_vpks.get_ref().is_none());
    assert!(!resolver.source_discovery_stats().failures.is_empty());
}

#[test]
fn bounded_category_has_no_silent_archive_prefix_and_a_new_wide_profile_can_retry() {
    let root = tempfile::tempdir().unwrap();
    let game = root.path().join("game");
    for name in ["z", "a", "m"] {
        write_gma_fixture(
            game.join(format!("garrysmod/addons/{name}.gma")),
            &GmaFixtureBuilder::new(name)
                .entry(format!("materials/{name}.vmt"), b"UnlitGeneric {}".to_vec())
                .build(),
        );
    }
    let strict = resolver(
        &game,
        SourceDiscoveryLimits {
            sources: 2,
            ..Default::default()
        },
        empty(),
    );
    assert!(strict.shared.sibling_gma_paths.is_empty());
    assert!(matches!(
        strict.resolve_primary(&[], "a"),
        MaterialResolution::SourceUnavailable {
            error: SourceDiscoveryFailure {
                kind: DiscoveryFailureKind::Limit {
                    resource: DiscoveryResource::Sources,
                    ..
                },
                ..
            }
        }
    ));
    assert!(strict.shared.sibling_gmas.get_ref().is_none());
    let wide = resolver(&game, Default::default(), empty());
    for name in ["a", "m", "z"] {
        assert!(matches!(
            wide.resolve_with_base2(&[], name),
            MaterialResolution::Loaded { .. }
        ));
    }
    assert!(wide.source_discovery_stats().failures.is_empty());
}

#[test]
fn configuration_policy_barrier_keeps_unknown_mounts_out_of_lower_tiers() {
    let root = tempfile::tempdir().unwrap();
    let game = root.path().join("game");
    fs::create_dir_all(game.join("garrysmod/cfg")).unwrap();
    fs::write(
        game.join("garrysmod/cfg/mount.cfg"),
        b"mountcfg { \"other\" \"/unknown\" }",
    )
    .unwrap();
    let strict = resolver(
        &game,
        SourceDiscoveryLimits {
            configuration_bytes: 1,
            ..Default::default()
        },
        empty(),
    );
    assert!(matches!(
        strict.resolve_primary(&[], "target"),
        MaterialResolution::SourceUnavailable {
            error: SourceDiscoveryFailure {
                kind: DiscoveryFailureKind::Limit {
                    resource: DiscoveryResource::ConfigurationBytes,
                    ..
                },
                ..
            }
        }
    ));
    assert!(strict.shared.game_vpk_paths.is_empty());
}

#[test]
fn empty_archives_consume_adapter_records_once_per_shared_universe() {
    let root = tempfile::tempdir().unwrap();
    let game = root.path().join("game");
    for index in 0..3 {
        write_gma_fixture(
            game.join(format!("garrysmod/addons/{index}.gma")),
            &GmaFixtureBuilder::new("empty").build(),
        );
    }
    let resolver = resolver(&game, Default::default(), empty());
    assert!(resolver.entry_bytes("materials/absent.vmt").is_none());
    let stats = resolver.archive_index_stats();
    assert_eq!(stats.archives, 3);
    assert_eq!(stats.entries, 0);
    assert!(stats.metadata_bytes >= 3 * size_of::<sibling_gma::SiblingGmaArchive>() as u64);
    assert!(
        resolver
            .with_decoded_texture_max_dimension(1)
            .entry_bytes("materials/absent.vmt")
            .is_none()
    );
    assert_eq!(resolver.archive_index_stats(), stats);
    assert_eq!(
        resolver.source_discovery_stats(),
        resolver
            .with_decoded_texture_max_dimension(1)
            .source_discovery_stats()
    );
}

#[test]
fn discovery_releases_scratch_before_lazy_indexes_and_cancelled_waiters_leave_no_charge() {
    use std::{
        thread,
        time::{Duration, Instant},
    };
    let root = tempfile::tempdir().unwrap();
    let game = root.path().join("game");
    fs::create_dir_all(&game).unwrap();
    let working = Arc::new(TextureWorkingBudget::new(1024 * 1024));
    let held = working.request(1024 * 1024).unwrap().acquire();
    let token = crate::preparation::PreparationCancellation::default();
    let other = token.clone();
    let gate = Arc::clone(&working);
    let worker = thread::spawn(move || {
        MaterialResolver::new_with_source_policy_cancellable(
            empty(),
            Some(game),
            Default::default(),
            gate,
            Default::default(),
            other,
        )
    });
    let deadline = Instant::now() + Duration::from_secs(2);
    while working.queued_requests() == 0 {
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }
    token.cancel();
    assert!(worker.join().unwrap().is_err());
    assert_eq!(working.queued_requests(), 0);
    drop(held);
    assert_eq!(working.current_bytes(), 0);
    let resolver = MaterialResolver::new_with_source_policy_cancellable(
        empty(),
        None,
        Default::default(),
        Arc::clone(&working),
        Default::default(),
        Default::default(),
    )
    .unwrap();
    assert_eq!(working.current_bytes(), 0);
    assert!(resolver.shared.sibling_gmas.get_ref().is_none());
}

#[test]
fn archive_discovery_and_indexing_do_not_discard_a_tail_above_the_old_cap() {
    let root = tempfile::tempdir().unwrap();
    let game = root.path().join("game");
    for index in 0..2050 {
        let builder = GmaFixtureBuilder::new("empty");
        let bytes = if index == 2049 {
            builder
                .entry("materials/tail.vmt", b"UnlitGeneric {}".to_vec())
                .build()
        } else {
            builder.build()
        };
        write_gma_fixture(
            game.join(format!("garrysmod/addons/{index:04}.gma")),
            &bytes,
        );
    }
    let resolver = resolver(&game, Default::default(), empty());
    assert_eq!(resolver.shared.sibling_gma_paths.len(), 2050);
    assert!(matches!(
        resolver.resolve_with_base2(&[], "tail"),
        MaterialResolution::Loaded { .. }
    ));
    assert_eq!(resolver.archive_index_stats().archives, 2050);
}

#[test]
fn empty_archive_adapter_rejection_is_a_barrier_and_does_not_hide_in_zero_entry_cost() {
    let root = tempfile::tempdir().unwrap();
    let game = root.path().join("game");
    let path = game.join("garrysmod/addons/empty.gma");
    write_gma_fixture(&path, &GmaFixtureBuilder::new("empty").build());
    let raw = gmpublished_backend::GmaView::open(&path)
        .unwrap()
        .index_cost_with_limits(Default::default())
        .unwrap();
    let limit =
        raw.retained_bytes().unwrap() + size_of::<sibling_gma::SiblingGmaArchive>() as u64 - 1;
    let resolver = MaterialResolver::new_with_source_policy_cancellable(
        empty(),
        Some(game),
        vformats::ArchiveIndexLimits {
            retained_bytes: limit,
            ..Default::default()
        },
        Arc::new(TextureWorkingBudget::new(8 * 1024 * 1024)),
        Default::default(),
        Default::default(),
    )
    .unwrap();
    assert!(matches!(
        resolver.resolve_primary(&[], "absent"),
        MaterialResolution::IndexUnavailable { .. }
    ));
    assert_eq!(resolver.archive_index_stats().archives, 0);
    assert_eq!(resolver.archive_index_stats().rejected_archives, 1);
}

#[test]
fn unavailable_named_cube_keeps_authored_identity_and_never_tries_default() {
    let root = tempfile::tempdir().unwrap();
    let game = root.path().join("game");
    fs::create_dir_all(game.join("garrysmod")).unwrap();
    fs::write(game.join("garrysmod/addons"), []).unwrap();
    let resolver = resolver(&game, Default::default(), empty());
    let cube = resolver.resolve_cubemap("authored/cube", 0).unwrap();
    assert_eq!(cube.load_outcome(), TextureLoadOutcome::SourceUnavailable);
    assert_eq!(cube.effective_path.as_deref(), Some("authored/cube"));
    assert_eq!(cube.resolution, CubemapResolution::Authored);
    assert!(matches!(
        cube.failure,
        Some(CubemapFailure::SourceUnavailable(_))
    ));
    assert_eq!(
        resolver.selected_texture_paths(),
        ["materials/authored/cube.vtf"]
    );
}

#[test]
fn native_error_images_remain_sampleable_while_unavailable_preparations_do_not() {
    for outcome in [
        TextureLoadOutcome::Loaded,
        TextureLoadOutcome::Generated,
        TextureLoadOutcome::MissingSource,
        TextureLoadOutcome::InvalidSource,
        TextureLoadOutcome::InvalidPayload,
    ] {
        assert!(!outcome.is_unavailable());
    }
    for outcome in [
        TextureLoadOutcome::QualityRejected,
        TextureLoadOutcome::SourceBudgetRejected,
        TextureLoadOutcome::SourceUnavailable,
        TextureLoadOutcome::Unsupported,
        TextureLoadOutcome::Deferred,
        TextureLoadOutcome::Unplanned,
    ] {
        assert!(outcome.is_unavailable());
    }
}

#[test]
fn unavailable_source_without_metadata_does_not_invent_default_mapping_dimensions() {
    let root = tempfile::tempdir().unwrap();
    let game = root.path().join("game");
    fs::create_dir_all(game.join("garrysmod")).unwrap();
    fs::write(game.join("garrysmod/addons"), []).unwrap();
    let material_path = root.path().join("authored.vmt");
    let material = br#"LightmappedGeneric { "$basetexture" "unavailable/image" }"#;
    fs::write(&material_path, material).unwrap();
    let addon = PreviewArchiveSource::from_folder([(
        "materials/authored.vmt".to_owned(),
        material.len() as u64,
        material_path,
    )]);
    let resolver = resolver(&game, Default::default(), addon);
    let resolved = resolver
        .resolve_primary(&[], "authored")
        .into_material()
        .unwrap();
    assert_eq!(
        resolved.texture.as_ref().unwrap().load_outcome(),
        TextureLoadOutcome::SourceUnavailable
    );
    assert_eq!(
        resolved
            .shader
            .mapping_dimensions(resolved.texture.as_deref()),
        None
    );
}
