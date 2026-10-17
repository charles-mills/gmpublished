use super::*;
use crate::scene::map::{load_map, load_map_with_pool};

fn fixture() -> Vec<u8> {
    crate::scene::map::tests::door_bmodel_bsp_fixture("")
}

#[test]
fn decoded_asset_exposes_texdata_order_and_missing_identities_before_preparation() {
    let mut asset = decode_map(&fixture()).unwrap();
    let expected = asset.material_names().collect::<Vec<_>>();
    assert_eq!(expected.len(), asset.bsp.texdatas.len());
    assert!(!expected.is_empty());
    assert!(expected.iter().all(Option::is_some));
    asset.bsp.texdatas[0].name_index = -1;
    assert_eq!(asset.material_names().next(), Some(None));
}

#[test]
fn preparation_rejects_missing_or_extra_effective_material_requirements() {
    for extra in [false, true] {
        let asset = decode_map(&fixture()).unwrap();
        let expected = asset.material_names().len();
        let actual = if extra { expected + 1 } else { expected - 1 };
        let error = asset
            .prepare(&vec![SourceLightmapRequirement::Lightmapped; actual])
            .unwrap_err();
        assert!(matches!(
            error,
            BspError::MaterialRequirementCount { expected: e, actual: a }
                if e == expected && a == actual
        ));
    }
}

#[test]
fn decoded_asset_owns_inputs_and_retains_lighting_source_through_preparation() {
    let asset = decode_map(&fixture()).unwrap();
    let source = asset.lighting_source();
    let requirements = vec![SourceLightmapRequirement::Lightmapped; asset.material_names().len()];
    let prepared = asset.prepare(&requirements).unwrap();
    assert_eq!(prepared.lighting_source, source);
    assert!(!prepared.meshes.is_empty());
    assert_eq!(prepared, load_map(&fixture()).unwrap());
}

#[test]
fn staged_preparation_uses_the_supplied_pool_without_redecoding() {
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(2)
        .build()
        .unwrap();
    let asset = decode_map_with_pool(&fixture(), &pool).unwrap();
    let requirements = vec![SourceLightmapRequirement::Lightmapped; asset.material_names().len()];
    assert_eq!(
        asset.prepare_with_pool(&requirements, &pool).unwrap(),
        load_map_with_pool(&fixture(), &pool).unwrap()
    );
}

#[test]
fn asset_lighting_channel_and_oversized_payload_survive_page_preparation() {
    let bytes =
        crate::scene::map::tests::lightmap_stats_fixture(0, [4094, 0], [0, 255, 255, 255], 4095);
    let asset = decode_map(&bytes).unwrap();
    assert_eq!(asset.lighting_source(), Some(LightmapSource::Ldr));
    let requirements = vec![SourceLightmapRequirement::Lightmapped; asset.material_names().len()];
    let prepared = asset.prepare(&requirements).unwrap();
    assert!(prepared.lightmap.is_some());
    assert_eq!(prepared.stats.lightmap.atlas_dropped_blocks, 0);
    assert_eq!(prepared.lighting_source, Some(LightmapSource::Ldr));
}

#[test]
fn unlightmapped_material_omits_atlas_without_changing_raw_radiosity_stride() {
    let bytes = crate::scene::map::tests::lightmap_stats_fixture(0, [0, 0], [0, 32, 255, 255], 8);
    let mut asset = decode_map(&bytes).unwrap();
    asset.bsp.texinfos[0].flags |= 0x0800;
    asset.bsp.lighting[4] = super::super::ColorRgbExp {
        r: 32,
        g: 64,
        b: 96,
        exponent: 0,
    };
    let requirements = vec![SourceLightmapRequirement::None; asset.material_names().len()];
    let prepared = asset.prepare(&requirements).unwrap();
    assert!(prepared.lightmap.is_none());
    assert_eq!(
        prepared.stats.lightmap,
        super::super::MapLightmapStats {
            unlightmapped_surfaces: 2,
            ..Default::default()
        }
    );
    let radiosity = prepared.radiosity_surfaces.unwrap();
    assert_eq!(radiosity.texinfos[0].flags & 0x0800, 0x0800);
    let mut values = [0.0; 128];
    values[32] = 1.0;
    let actual = radiosity
        .sample_luxel(0, [0, 0], &values, super::super::MapRadiosityStyles::All)
        .unwrap();
    let expected = crate::math::Vec3::new(32.0, 64.0, 96.0) / 255.0;
    assert!((actual - expected).length_squared() < 1e-12);
}

#[test]
fn effective_material_changes_gpu_basis_without_rewriting_compiled_sample_flags() {
    for (raw_bumped, output, samples, directional_output) in [
        (false, SourceLightmapRequirement::Bumped, 2, true),
        (true, SourceLightmapRequirement::Lightmapped, 8, false),
    ] {
        let bytes =
            crate::scene::map::tests::lightmap_stats_fixture(0, [0, 0], [0, 32, 255, 255], samples);
        let mut asset = decode_map(&bytes).unwrap();
        if raw_bumped {
            asset.bsp.texinfos[0].flags |= 0x0800;
        }
        let requirements = vec![output; asset.material_names().len()];
        let prepared = asset.prepare(&requirements).unwrap();
        assert_eq!(prepared.stats.lightmap.valid_payloads, 1);
        assert_eq!(prepared.stats.lightmap.invalid_payloads, 0);
        let vertices = prepared
            .meshes
            .iter()
            .flat_map(|mesh| &mesh.vertices)
            .filter(|vertex| vertex.lightmap_page.is_some())
            .collect::<Vec<_>>();
        assert!(!vertices.is_empty());
        assert!(
            vertices
                .iter()
                .all(|vertex| { (vertex.lightmap_bump_step > 0.0) == directional_output })
        );
        let radiosity = prepared.radiosity_surfaces.unwrap();
        assert_eq!(radiosity.texinfos[0].flags & 0x0800 != 0, raw_bumped);
    }
}
