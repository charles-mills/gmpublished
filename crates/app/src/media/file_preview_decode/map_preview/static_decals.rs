use crate::bridge::materials::{SourceDecalFormat, SurfaceFamily, SurfaceMaterial};
use crate::media::preview_model::{StaticDecalPass, StaticDecalSort};
use gmpublished_domain::scene::map::{MapDecalPermanence, SourceLightmapPage};
use std::collections::HashMap;

#[derive(Default)]
pub(in super::super) struct StaticDecalRegistry {
    formats: HashMap<SourceDecalFormat, FormatBuckets>,
}

struct FormatBuckets {
    index: usize,
    passes: [HashMap<(usize, SourceLightmapPage), usize>; 3],
}

impl StaticDecalRegistry {
    pub(in super::super) fn register(
        &mut self,
        material_index: usize,
        material: &SurfaceMaterial,
        receiver_page: SourceLightmapPage,
        permanence: MapDecalPermanence,
    ) -> StaticDecalSort {
        let format = material.source_decal_format();
        let pass = match (material.family, permanence) {
            (
                SurfaceFamily::Lightmapped | SurfaceFamily::Transition,
                MapDecalPermanence::Permanent,
            ) => StaticDecalPass::PermanentLightmapped,
            (
                SurfaceFamily::Lightmapped | SurfaceFamily::Transition,
                MapDecalPermanence::NonPermanent,
            ) => StaticDecalPass::NonPermanentLightmapped,
            _ => StaticDecalPass::Unlit,
        };
        let page = if pass == StaticDecalPass::Unlit {
            SourceLightmapPage::White
        } else {
            receiver_page
        };
        let next_index = self.formats.len();
        let group = self.formats.entry(format).or_insert_with(|| FormatBuckets {
            index: next_index,
            passes: std::array::from_fn(|_| HashMap::new()),
        });
        let buckets = &mut group.passes[pass as usize];
        let next_bucket = buckets.len();
        let bucket = *buckets.entry((material_index, page)).or_insert(next_bucket);
        StaticDecalSort {
            pass,
            format_group: group.index,
            material_page_bucket: bucket,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::materials::MaterialResolver;

    fn material(vmt: &str) -> SurfaceMaterial {
        let resolver = MaterialResolver::prop_lighting_fixture(
            vec![("materials/test/decal.vmt", vmt.as_bytes().to_vec())],
            Vec::new(),
        );
        let resolved = resolver
            .resolve_with_base2(&[], "test/decal")
            .into_material()
            .unwrap();
        assert!(resolved.texture.is_none());
        resolved.shader.surface().unwrap().clone()
    }

    fn lightmapped() -> SurfaceMaterial {
        material("LightmappedGeneric {}")
    }

    #[test]
    fn formats_are_shared_across_all_decal_passes() {
        let lit = lightmapped();
        let unlit = material(r#"UnlitGeneric { "$separatedetailuvs" "1" }"#);
        assert_eq!(lit.source_decal_format(), unlit.source_decal_format());
        let mut registry = StaticDecalRegistry::default();
        for (material_index, surface, permanence, expected_pass) in [
            (
                8,
                &unlit,
                MapDecalPermanence::Permanent,
                StaticDecalPass::Unlit,
            ),
            (
                2,
                &lit,
                MapDecalPermanence::NonPermanent,
                StaticDecalPass::NonPermanentLightmapped,
            ),
            (
                2,
                &lit,
                MapDecalPermanence::Permanent,
                StaticDecalPass::PermanentLightmapped,
            ),
        ] {
            let sort = registry.register(
                material_index,
                surface,
                SourceLightmapPage::Physical(3),
                permanence,
            );
            assert_eq!(sort.pass, expected_pass);
            assert_eq!(sort.format_group, 0);
            assert_eq!(sort.material_page_bucket, 0);
        }
    }

    #[test]
    fn unviewed_registration_keeps_its_format_and_bucket_ranks() {
        let lit = lightmapped();
        let colored = material(r#"LightmappedGeneric { "$vertexcolor" "1" }"#);
        let mut registry = StaticDecalRegistry::default();
        let unviewed = registry.register(
            91,
            &colored,
            SourceLightmapPage::Physical(12),
            MapDecalPermanence::Permanent,
        );
        let visible_other_format = registry.register(
            2,
            &lit,
            SourceLightmapPage::Physical(0),
            MapDecalPermanence::Permanent,
        );
        let visible_same_format = registry.register(
            1,
            &colored,
            SourceLightmapPage::Physical(0),
            MapDecalPermanence::Permanent,
        );
        assert_eq!(unviewed.format_group, 0);
        assert_eq!(visible_other_format.format_group, 1);
        assert_eq!(visible_same_format.format_group, 0);
        assert_eq!(visible_same_format.material_page_bucket, 1);
        assert_eq!(
            registry.register(
                91,
                &colored,
                SourceLightmapPage::Physical(12),
                MapDecalPermanence::Permanent,
            ),
            unviewed,
        );
    }

    #[test]
    fn material_page_bucket_registration_is_independent_per_pass() {
        let lit = lightmapped();
        let mut registry = StaticDecalRegistry::default();
        for (material_index, page, permanence, expected_bucket) in [
            (9, 7, MapDecalPermanence::Permanent, 0),
            (4, 2, MapDecalPermanence::Permanent, 1),
            (4, 2, MapDecalPermanence::NonPermanent, 0),
            (9, 7, MapDecalPermanence::NonPermanent, 1),
            (9, 7, MapDecalPermanence::Permanent, 0),
            (9, 2, MapDecalPermanence::NonPermanent, 2),
        ] {
            let sort = registry.register(
                material_index,
                &lit,
                SourceLightmapPage::Physical(page),
                permanence,
            );
            assert_eq!(sort.format_group, 0);
            assert_eq!(sort.material_page_bucket, expected_bucket);
        }
    }

    #[test]
    fn nonlightmapped_marks_share_one_bucket_across_receiver_pages_and_permanence() {
        for surface in [material("UnlitGeneric {}"), material("DecalModulate {}")] {
            let mut registry = StaticDecalRegistry::default();
            for page in [
                SourceLightmapPage::Physical(0),
                SourceLightmapPage::WhiteBump,
                SourceLightmapPage::Physical(17),
                SourceLightmapPage::White,
            ] {
                for permanence in [
                    MapDecalPermanence::Permanent,
                    MapDecalPermanence::NonPermanent,
                ] {
                    let sort = registry.register(6, &surface, page, permanence);
                    assert_eq!(sort.pass, StaticDecalPass::Unlit);
                    assert_eq!(sort.material_page_bucket, 0);
                }
            }
            assert_eq!(
                registry
                    .register(
                        7,
                        &surface,
                        SourceLightmapPage::White,
                        MapDecalPermanence::Permanent,
                    )
                    .material_page_bucket,
                1,
            );
        }
    }

    #[test]
    fn lightmapped_marks_keep_physical_white_and_white_bump_buckets_distinct() {
        let lit = lightmapped();
        let mut registry = StaticDecalRegistry::default();
        for (page, expected_bucket) in [
            (SourceLightmapPage::WhiteBump, 0),
            (SourceLightmapPage::Physical(0), 1),
            (SourceLightmapPage::White, 2),
            (SourceLightmapPage::Physical(1), 3),
            (SourceLightmapPage::WhiteBump, 0),
        ] {
            let sort = registry.register(3, &lit, page, MapDecalPermanence::Permanent);
            assert_eq!(sort.material_page_bucket, expected_bucket);
        }
    }
}
