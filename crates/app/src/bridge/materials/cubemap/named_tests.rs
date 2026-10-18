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
        resolver.selected_reloadable_paths(),
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
        resolver.selected_reloadable_paths(),
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
        let environment = strict.resolve_environment_map(&material).unwrap();
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

        assert_eq!(strict.selected_reloadable_paths(), vec![path]);
        assert_eq!(strict.archive_index_stats().archives, 0);
        let admitted = &strict;
        let cube = admitted.resolve_named_cubemap("test/cube", 0).unwrap();
        assert_eq!(
            cube.load_outcome(),
            TextureLoadOutcome::SourceBudgetRejected
        );
        assert_eq!(cube.resolution, CubemapResolution::Authored);
        assert_eq!(admitted.selected_reloadable_paths(), vec![path]);

        let wide = MaterialResolver::with_pakfile_source(addon, None, pak);
        let cube = wide.resolve_named_cubemap("test/cube", 0).unwrap();
        assert_eq!(cube.load_outcome(), TextureLoadOutcome::Loaded);
        assert_eq!(cube.resolution, CubemapResolution::Authored);
        assert_eq!(cube.source_metadata().unwrap().version_minor, Some(5));
    }
}

#[test]
fn invalid_authored_cube_reuses_default_source_and_keeps_metadata() {
    let resolver = resolver(Some(b"FAIL".to_vec()), Some(modern_cube()));
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
    assert_eq!(cube.load_outcome(), TextureLoadOutcome::Loaded);
    assert!(cube.resident().is_none());
    let again = resolver.resolve_named_cubemap("test/cube", 0).unwrap();
    let (source, request) = cube.on_demand().unwrap();
    assert!(Arc::ptr_eq(source, again.on_demand().unwrap().0));
    assert_eq!(source.page_cost(request).unwrap().resident_bytes, 504);
    assert_eq!(request.faces, 0..6);
    assert_eq!(request.mip_count, 3);
    assert_eq!(cube.read_for_test().byte_len(), 504);
}

#[test]
fn per_page_rejection_never_substitutes_an_authored_cube_and_also_applies_to_default() {
    for authored in [modern_cube(), b"FAIL".to_vec()] {
        let resolver = resolver(Some(authored.clone()), Some(modern_cube()));
        let strict = resolver.with_texture_resource_limit(503);
        let cube = strict.resolve_named_cubemap("test/cube", 0).unwrap();
        assert_eq!(cube.load_outcome(), TextureLoadOutcome::QualityRejected);
        assert!(matches!(
            cube.failure,
            Some(CubemapFailure::Texture(TextureAssetError::Vtf(
                VtfError::DecodedTooLarge {
                    size: 504,
                    max: 503
                }
            )))
        ));
        assert_eq!(
            matches!(cube.resolution, CubemapResolution::Authored),
            authored != b"FAIL"
        );
        if authored != b"FAIL" {
            assert!(
                !strict
                    .selected_reloadable_paths()
                    .iter()
                    .any(|path| path.contains("defaultcubemap"))
            );
        }
        let recovered = resolver
            .with_texture_resource_limit(504)
            .resolve_named_cubemap("test/cube", 0)
            .unwrap();
        assert_eq!(recovered.load_outcome(), TextureLoadOutcome::Loaded);
    }
}

#[test]
fn hdr_and_invalid_frame_failures_never_select_default() {
    let resolver = resolver(Some(modern_cube()), Some(modern_cube()));
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
        resolver.selected_reloadable_paths(),
        vec!["materials/test/cube.vtf"]
    );
}
