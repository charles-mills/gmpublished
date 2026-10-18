use super::*;
use crate::bridge::materials::{
    MaterialShader, ResolvedLightmapLayoutMaterial, ResolverConfig, SurfaceFamily, material_paths,
};
use gmpublished_domain::scene::map::SourceLightmapRequirement;

fn shadowed(input: &[u8]) -> MaterialResolver {
    MaterialResolver::prop_lighting_fixture(
        vec![("materials/test/input.vmt", input.to_vec())],
        vec![(
            "materials/test/input.vmt",
            br#"LightmappedGeneric { "$nodraw" "0" }"#.to_vec(),
        )],
    )
}

#[test]
fn unsupported_selected_material_shadows_lower_supported_copy_for_every_consumer() {
    let resolver = shadowed(br#"UnsupportedShader { "$nodraw" "1" }"#);
    assert_eq!(
        resolver.resolve_lightmap_layout_material("test/input"),
        ResolvedLightmapLayoutMaterial {
            canonical_name: "test/input".to_owned(),
            requirement: SourceLightmapRequirement::None,
        }
    );
    assert!(
        resolver
            .resolve_with_base2(&[], "test/input")
            .into_material()
            .is_none()
    );
    assert!(
        resolver
            .resolve_primary(&[], "test/input")
            .into_material()
            .is_none()
    );
    assert!(
        resolver
            .resolve_surface_lighting("test/input")
            .unwrap()
            .flags
            .no_draw
    );
    let sky = resolver
        .resolve_sky_material_at_path("materials/test/input.vmt", None)
        .unwrap();
    assert_eq!(sky.shader, "UnsupportedShader");
}

#[test]
fn missing_selected_image_does_not_change_the_material_source_or_shader() {
    let resolver = shadowed(br#"UnlitGeneric { "$basetexture" "test/absent" "$translucent" "1" }"#);
    let resolved = resolver
        .resolve_with_base2(&[], "test/input")
        .into_material()
        .unwrap();
    let texture = resolved.texture.as_ref().unwrap();
    assert_eq!(
        texture.load_outcome(),
        crate::bridge::materials::TextureLoadOutcome::MissingSource
    );
    assert!(texture.has_effective_base_texture());
    assert!(
        matches!(resolved.shader, MaterialShader::Surface(ref surface) if surface.family == SurfaceFamily::Unlit)
    );
    assert_eq!(
        resolver
            .resolve_lightmap_layout_material("test/input")
            .requirement,
        SourceLightmapRequirement::None
    );
    assert_eq!(
        resolver
            .resolve_surface_lighting("test/input")
            .unwrap()
            .reflectivity,
        crate::bridge::materials::Vec3::splat(0.5)
    );
}

#[test]
fn malformed_existing_material_shadows_a_lower_readable_definition() {
    let resolver = shadowed(b"");
    assert!(
        resolver
            .resolve_with_base2(&[], "test/input")
            .into_material()
            .is_none()
    );
    assert!(resolver.resolve_surface_lighting("test/input").is_none());
    assert_eq!(
        resolver
            .resolve_lightmap_layout_material("test/input")
            .canonical_name,
        "___error"
    );
}

#[test]
fn source_tier_precedence_still_precedes_candidate_directory_order() {
    let resolver = MaterialResolver::prop_lighting_fixture(
        vec![("materials/b/input.vmt", b"UnlitGeneric {}".to_vec())],
        vec![("materials/a/input.vmt", b"LightmappedGeneric {}".to_vec())],
    );
    let paths = material_paths(&["a".to_owned(), "b".to_owned()], "input");
    let selected = resolver.material_definition(&paths).unwrap();
    assert_eq!(selected.path.as_str(), "materials/b/input.vmt");
    assert_eq!(selected.material.shader, "UnlitGeneric");
}

#[test]
fn different_search_queries_share_the_same_selected_definition() {
    let resolver = MaterialResolver::prop_lighting_fixture(
        vec![("materials/b/input.vmt", b"UnlitGeneric {}".to_vec())],
        Vec::new(),
    );
    let searched = resolver
        .material_definition(&material_paths(&["a".to_owned(), "b".to_owned()], "input"))
        .unwrap();
    let direct = resolver
        .material_definition(&material_paths(&[], "b/input"))
        .unwrap();
    assert!(Arc::ptr_eq(&searched, &direct));
    assert_eq!(resolver.shared.material_definitions.queries.lock().len(), 2);
    assert_eq!(
        resolver.shared.material_definitions.documents.lock().len(),
        1
    );
    assert_eq!(
        resolver
            .shared
            .material_definitions
            .definitions
            .lock()
            .len(),
        1
    );
}

#[test]
fn profile_variants_share_selected_document_but_keep_effective_parameters_separate() {
    let resolver = MaterialResolver::prop_lighting_fixture(
        vec![(
            "materials/test/input.vmt",
            br#"LightmappedGeneric { "hdr" { "$bumpmap" "normal" } }"#.to_vec(),
        )],
        Vec::new(),
    );
    let hdr = resolver.variant(ResolverConfig {
        hardware_profile: profile::HardwareProfile::HDR,
        ..resolver.config.clone()
    });
    let bounded = resolver.with_decoded_texture_max_dimension(32);
    let paths = material_paths(&[], "test/input");
    let ldr_definition = resolver.material_definition(&paths).unwrap();
    assert!(Arc::ptr_eq(
        &ldr_definition,
        &bounded.material_definition(&paths).unwrap()
    ));
    assert!(!Arc::ptr_eq(
        &ldr_definition,
        &hdr.material_definition(&paths).unwrap()
    ));
    assert_eq!(
        resolver
            .resolve_lightmap_layout_material("test/input")
            .requirement,
        SourceLightmapRequirement::Lightmapped
    );
    assert_eq!(
        hdr.resolve_lightmap_layout_material("test/input")
            .requirement,
        SourceLightmapRequirement::Bumped
    );
    assert_eq!(
        resolver.shared.material_definitions.documents.lock().len(),
        1
    );
    assert_eq!(
        resolver
            .shared
            .material_definitions
            .definitions
            .lock()
            .len(),
        2
    );
}

#[test]
fn concurrent_cross_patch_cycles_finish_without_waiting_on_effective_definition_cells() {
    use std::sync::{Barrier, mpsc};
    use std::time::Duration;

    let resolver = Arc::new(MaterialResolver::prop_lighting_fixture(
        vec![
            (
                "materials/test/a.vmt",
                br#"Patch { "include" "materials/test/b.vmt" }"#.to_vec(),
            ),
            (
                "materials/test/b.vmt",
                br#"Patch { "include" "materials/test/a.vmt" }"#.to_vec(),
            ),
        ],
        Vec::new(),
    ));
    let barrier = Arc::new(Barrier::new(2));
    let (tx, rx) = mpsc::channel();
    let workers = ["test/a", "test/b"].map(|name| {
        let resolver = Arc::clone(&resolver);
        let barrier = Arc::clone(&barrier);
        let tx = tx.clone();
        std::thread::spawn(move || {
            barrier.wait();
            let value = resolver.resolve_lightmap_layout_material(name);
            tx.send(value).unwrap();
        })
    });
    for _ in 0..2 {
        let value = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("cyclic includes finish");
        assert_eq!(value.canonical_name, "___error");
    }
    for worker in workers {
        worker.join().unwrap();
    }
}
