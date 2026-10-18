use super::*;

fn modern_cube() -> Vec<u8> {
    super::tests::fixture(6, 1, 3)
}

fn resolver(authored: Option<Vec<u8>>, default: Option<Vec<u8>>) -> MaterialResolver {
    let entries = authored
        .into_iter()
        .map(|bytes| ("materials/test/cube.vtf", bytes))
        .chain(
            default
                .into_iter()
                .map(|bytes| ("materials/engine/defaultcubemap.vtf", bytes)),
        )
        .collect();
    MaterialResolver::prop_lighting_fixture(entries, Vec::new())
}

#[test]
fn named_invalid_fallback_is_planned_once_and_keeps_effective_metadata() {
    let resolver = resolver(Some(b"FAIL".to_vec()), Some(modern_cube()));
    let (planned, plan) = resolver.plan_texture_requests(|resolver| {
        let first = resolver.resolve_named_cubemap("test/cube", 0).unwrap();
        let second = resolver.resolve_named_cubemap("test/cube", 0).unwrap();
        (first, second)
    });
    assert_eq!(plan.cost().resident_bytes, 504);
    assert_eq!(plan.cost().image_count, 18);
    assert_eq!(planned.0.load_outcome(), TextureLoadOutcome::Deferred);
    assert_eq!(planned.1.load_outcome(), TextureLoadOutcome::Deferred);
    let resolver = resolver.with_texture_admissions(resolver.admit_texture_requests(plan));
    let cube = resolver.resolve_named_cubemap("test/cube", 0).unwrap();
    assert!(matches!(
        cube.resolution,
        CubemapResolution::Default {
            authored_failure: CubemapFailure::InvalidVtf(VtfError::BadMagic)
        }
    ));
    assert_eq!(cube.effective_path.as_deref(), Some(DEFAULT_CUBEMAP));
    assert_eq!(cube.source_metadata().unwrap().version_minor, Some(5));
    assert_eq!(
        cube.source_metadata().unwrap().flags,
        Some(texture_flags::ENVMAP)
    );
    assert_eq!(cube.frame, 0);
    assert_eq!(cube.load_outcome(), TextureLoadOutcome::Loaded);
    assert!(Arc::ptr_eq(
        cube.resident().unwrap(),
        resolver
            .resolve_named_cubemap("test/cube", 0)
            .unwrap()
            .resident()
            .unwrap()
    ));
}

#[test]
fn missing_and_invalid_payload_fallback_is_bounded() {
    let cube = resolver(None, Some(modern_cube()))
        .resolve_named_cubemap("test/cube", 7)
        .unwrap();
    assert_eq!(
        cube.resolution,
        CubemapResolution::Default {
            authored_failure: CubemapFailure::Missing
        }
    );
    assert_eq!(cube.effective_path.as_deref(), Some(DEFAULT_CUBEMAP));
    assert_eq!(cube.frame, 0);
    let unavailable = resolver(None, None);
    assert!(matches!(unavailable.resolve_named_cubemap("test/cube", 0),
        Err(CubemapFailure::DefaultUnavailable { authored, fallback })
        if *authored == CubemapFailure::Missing && *fallback == CubemapFailure::Missing));
    let mut truncated = modern_cube();
    truncated.pop();
    let cube = resolver(Some(truncated), Some(modern_cube()))
        .resolve_named_cubemap("test/cube", 0)
        .unwrap();
    assert!(matches!(
        cube.resolution,
        CubemapResolution::Default {
            authored_failure: CubemapFailure::InvalidVtf(VtfError::Truncated { .. })
        }
    ));
    let failed = resolver(Some(b"FAIL".to_vec()), Some(b"FAIL".to_vec()));
    assert!(matches!(
        failed.resolve_named_cubemap("test/cube", 0),
        Err(CubemapFailure::DefaultUnavailable { .. })
    ));
    assert!(matches!(
        failed.resolve_named_cubemap(DEFAULT_CUBEMAP, 0),
        Err(CubemapFailure::InvalidVtf(_))
    ));
}

#[test]
fn policy_and_unplanned_failures_never_select_default() {
    let resolver = resolver(Some(modern_cube()), Some(modern_cube()))
        .with_decoded_texture_budget(Arc::new(super::super::DecodedTextureBudget::new(0)));
    let (_, plan) =
        resolver.plan_texture_requests(|resolver| resolver.resolve_named_cubemap("test/cube", 0));
    let resolver = resolver.with_texture_admissions(resolver.admit_texture_requests(plan));
    let cube = resolver.resolve_named_cubemap("test/cube", 0).unwrap();
    assert_eq!(cube.load_outcome(), TextureLoadOutcome::QualityRejected);
    assert!(matches!(
        cube.failure,
        Some(CubemapFailure::Texture(TextureAssetError::Budget))
    ));
    assert_eq!(cube.resolution, CubemapResolution::Authored);
    assert_eq!(
        resolver.selected_texture_paths(),
        vec!["materials/test/cube.vtf"]
    );

    let resolver = super::named_tests::resolver(Some(modern_cube()), Some(modern_cube()));
    let (_, empty) = resolver.plan_texture_requests(|_| ());
    let resolver = resolver.with_texture_admissions(resolver.admit_texture_requests(empty));
    let cube = resolver.resolve_named_cubemap("test/cube", 0).unwrap();
    assert_eq!(cube.load_outcome(), TextureLoadOutcome::Unplanned);
    assert_eq!(cube.resolution, CubemapResolution::Authored);
    assert!(
        !resolver
            .selected_texture_paths()
            .iter()
            .any(|path| path.contains("defaultcubemap"))
    );
}

#[test]
fn source_budget_hdr_and_frame_failures_never_select_default() {
    let resolver = resolver(Some(modern_cube()), Some(modern_cube()))
        .with_encoded_texture_budget(Arc::new(super::super::EncodedTextureBudget::new(0)));
    let cube = resolver.resolve_named_cubemap("test/cube", 0).unwrap();
    assert_eq!(
        cube.load_outcome(),
        TextureLoadOutcome::SourceBudgetRejected
    );
    assert_eq!(cube.failure, Some(CubemapFailure::SourceBudget));
    assert_eq!(
        resolver.selected_texture_paths(),
        vec!["materials/test/cube.vtf"]
    );
    let resolver = super::named_tests::resolver(Some(modern_cube()), Some(modern_cube()));
    assert!(matches!(
        resolver.resolve_named_cubemap("test/cube", 1),
        Err(CubemapFailure::Frame {
            requested: 1,
            count: 1
        })
    ));
    assert!(matches!(
        resolver.resolve_named_cubemap("test/cube.hdr", 0),
        Err(CubemapFailure::UnsupportedHdr)
    ));
    assert_eq!(
        resolver.selected_texture_paths(),
        vec!["materials/test/cube.vtf"]
    );
}

#[test]
fn environment_identity_stays_authored_when_resident_is_default() {
    let resolver = resolver(Some(b"FAIL".to_vec()), Some(modern_cube()));
    let document = vformats::vmt::parse(
        r#"LightmappedGeneric { "$envmap" "test/cube" }"#,
        &Limits::default(),
    )
    .unwrap();
    let environment = resolver
        .resolve_environment_map(&EffectiveMaterial::from_document(&document))
        .unwrap();
    assert_eq!(environment.path, "test/cube");
    let EnvironmentMapSource::Named(cube) = &environment.source else {
        panic!("expected named environment")
    };
    assert_eq!(cube.effective_path.as_deref(), Some(DEFAULT_CUBEMAP));
}

#[test]
fn default_substitution_obeys_frozen_resident_admission() {
    let resolver = resolver(Some(b"FAIL".to_vec()), Some(modern_cube()))
        .with_decoded_texture_budget(Arc::new(super::super::DecodedTextureBudget::new(0)));
    let (_, plan) = resolver
        .plan_texture_requests(|resolver| resolver.resolve_named_cubemap("test/cube", 0).unwrap());
    assert_eq!(plan.cost().resident_bytes, 504);
    let resolver = resolver.with_texture_admissions(resolver.admit_texture_requests(plan));
    let cube = resolver.resolve_named_cubemap("test/cube", 0).unwrap();
    assert!(matches!(cube.resolution, CubemapResolution::Default { .. }));
    assert_eq!(cube.load_outcome(), TextureLoadOutcome::QualityRejected);
    assert!(matches!(
        cube.failure,
        Some(CubemapFailure::Texture(TextureAssetError::Budget))
    ));
    assert_eq!(resolver.resident_texture_asset_count(), 0);
}

#[test]
fn working_and_shape_failures_do_not_substitute_default() {
    let resolver = resolver(Some(modern_cube()), Some(modern_cube()))
        .with_texture_source_working_budget(Arc::new(super::super::TextureWorkingBudget::new(0)));
    let cube = resolver.resolve_named_cubemap("test/cube", 0).unwrap();
    assert_eq!(
        cube.load_outcome(),
        TextureLoadOutcome::SourceBudgetRejected
    );
    assert!(matches!(
        cube.failure,
        Some(CubemapFailure::Texture(
            TextureAssetError::WorkingLimit { .. }
        ))
    ));
    assert_eq!(
        resolver.selected_texture_paths(),
        vec!["materials/test/cube.vtf"]
    );
    let mut bytes = modern_cube();
    bytes[20..24].copy_from_slice(&0_u32.to_le_bytes());
    let resolver = super::named_tests::resolver(Some(bytes), Some(modern_cube()));
    assert!(matches!(
        resolver.resolve_named_cubemap("test/cube", 0),
        Err(CubemapFailure::NotCubemap)
    ));
    assert_eq!(
        resolver.selected_texture_paths(),
        vec!["materials/test/cube.vtf"]
    );
}

#[test]
fn rejected_cold_and_warm_pak_indices_preserve_named_state_without_fallback() {
    use crate::bridge::gma::PreviewArchive;
    use crate::bridge::materials::tests::{zip_fixture, zip_stored_entry};
    use crate::test_support::GmaFixtureBuilder;
    use gmpublished_domain::scene::map::MapPakFile;

    let path = "materials/test/cube.vtf";
    for warm in [false, true] {
        let pak = MapPakFile::from_pak_bytes(zip_fixture([zip_stored_entry(path, modern_cube())]));
        if warm {
            assert_eq!(pak.indexed_entries().unwrap().len(), 1);
        }
        let addon = Arc::new(
            PreviewArchive::from_gma(
                GmaFixtureBuilder::new("Lower cube and native fallback")
                    .entry(path, modern_cube())
                    .entry("materials/engine/defaultcubemap.vtf", modern_cube())
                    .build(),
            )
            .unwrap(),
        );
        let strict = MaterialResolver::with_pakfile_source_and_index_limits(
            Arc::clone(&addon),
            None,
            pak.clone(),
            vformats::ArchiveIndexLimits {
                entries: 0,
                ..Default::default()
            },
            Arc::new(super::super::TextureWorkingBudget::new(1024 * 1024)),
        );
        let document = vformats::vmt::parse(
            r#"LightmappedGeneric { "$envmap" "test/cube" }"#,
            &Limits::default(),
        )
        .unwrap();
        let material = EffectiveMaterial::from_document(&document);
        let (_, plan) = strict.plan_texture_requests(|resolver| {
            let environment = resolver.resolve_environment_map(&material).unwrap();
            assert_eq!(environment.path, "test/cube");
            let EnvironmentMapSource::Named(cube) = &environment.source else {
                panic!("index rejection must preserve the authored named envmap");
            };
            assert_eq!(cube.resolution, CubemapResolution::Authored);
            assert_eq!(cube.effective_path.as_deref(), Some("test/cube"));
            assert_eq!(
                cube.load_outcome(),
                TextureLoadOutcome::SourceBudgetRejected
            );
            assert!(matches!(
                cube.failure,
                Some(CubemapFailure::Texture(TextureAssetError::ArchiveIndex(
                    vformats::ArchiveIndexLimit {
                        kind: vformats::ArchiveIndexLimitKind::Entries,
                        ..
                    }
                )))
            ));
            assert!(cube.source_metadata().is_none());
        });
        assert_eq!(plan.cost().resident_bytes, 0);
        assert_eq!(plan.cost().image_count, 0);
        assert_eq!(strict.resident_texture_asset_count(), 0);
        assert_eq!(strict.selected_texture_paths(), vec![path]);
        assert_eq!(strict.archive_index_stats().archives, 0);
        let admitted = strict.with_texture_admissions(strict.admit_texture_requests(plan));
        let cube = admitted.resolve_named_cubemap("test/cube", 0).unwrap();
        assert_eq!(
            cube.load_outcome(),
            TextureLoadOutcome::SourceBudgetRejected
        );
        assert_eq!(cube.resolution, CubemapResolution::Authored);
        assert_eq!(admitted.selected_texture_paths(), vec![path]);

        let wide = MaterialResolver::with_pakfile_source(addon, None, pak);
        let cube = wide.resolve_named_cubemap("test/cube", 0).unwrap();
        assert_eq!(cube.load_outcome(), TextureLoadOutcome::Loaded);
        assert_eq!(cube.resolution, CubemapResolution::Authored);
        assert_eq!(cube.source_metadata().unwrap().version_minor, Some(5));
    }
}
