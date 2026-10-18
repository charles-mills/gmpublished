use super::texture_asset::SelectedTextureAsset;
use super::{
    Arc, DecodedTextureBudget, MaterialResolver, Mutex, TextureAssetError, TextureResidentId,
};
use std::collections::{HashMap, HashSet};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TextureRequestCost {
    pub resident_bytes: usize,
    pub image_count: usize,
    pub layer_count: usize,
    pub resident_record_bytes: usize,
    pub source_encoded_bytes: usize,
    pub decode_peak_bytes: usize,
    pub payload_working_bytes: usize,
}

#[derive(Clone, Debug)]
struct TextureRequest {
    id: TextureResidentId,
    cost: TextureRequestCost,
    source: Arc<SelectedTextureAsset>,
    preflight_error: Option<TextureAssetError>,
}

#[derive(Debug, Default)]
pub struct TextureRequestPlan {
    requests: Vec<TextureRequest>,
    sources: HashSet<u64>,
    residents: HashSet<TextureResidentId>,
    backings: HashMap<usize, super::TextureBytes>,
    cost: TextureRequestCost,
}

impl Clone for TextureRequestPlan {
    fn clone(&self) -> Self {
        Self {
            requests: self.requests.clone(),
            sources: self.sources.clone(),
            residents: self.residents.clone(),
            backings: self
                .backings
                .iter()
                .map(|(id, bytes)| (*id, bytes.legacy_alias()))
                .collect(),
            cost: self.cost,
        }
    }
}

impl TextureRequestPlan {
    pub(crate) fn append(&mut self, other: Self) {
        self.sources.extend(other.sources);
        for bytes in other.backings.into_values() {
            self.source(bytes);
        }
        for request in other.requests {
            self.push(request);
        }
    }

    fn source(&mut self, bytes: super::TextureBytes) {
        let id = bytes.backing_id();
        if let std::collections::hash_map::Entry::Vacant(entry) = self.backings.entry(id) {
            self.cost.source_encoded_bytes += bytes.backing_len();
            entry.insert(bytes);
        }
    }

    fn push(&mut self, request: TextureRequest) {
        self.source(request.source.asset.source_bytes());
        if self.residents.insert(request.id.clone()) {
            self.cost.resident_bytes += request.cost.resident_bytes;
            self.cost.image_count += request.cost.image_count;
            self.cost.layer_count += request.cost.layer_count;
            self.cost.resident_record_bytes += request.cost.resident_record_bytes;
            self.cost.decode_peak_bytes = self
                .cost
                .decode_peak_bytes
                .max(request.cost.decode_peak_bytes);
            self.cost.payload_working_bytes = self
                .cost
                .payload_working_bytes
                .max(request.cost.payload_working_bytes);
            self.requests.push(request);
        }
    }

    pub(crate) fn cost(&self) -> TextureRequestCost {
        self.cost
    }

    pub(crate) fn admit(self, budget: Arc<DecodedTextureBudget>) -> Arc<TextureAdmissions> {
        let admitted = self
            .requests
            .into_iter()
            .map(|request| {
                let result = request.preflight_error.map_or_else(
                    || {
                        budget.try_admit(
                            &request.id,
                            request.cost.resident_bytes,
                            request.cost.image_count,
                        )
                    },
                    Err,
                );
                (request.id, result)
            })
            .collect();
        Arc::new(TextureAdmissions {
            admitted,
            sources: self.sources,
            budget,
            cost: self.cost,
        })
    }
}

#[derive(Debug)]
pub struct TextureAdmissions {
    sources: HashSet<u64>,
    admitted: HashMap<TextureResidentId, Result<(), TextureAssetError>>,
    budget: Arc<DecodedTextureBudget>,
    cost: TextureRequestCost,
}

impl TextureAdmissions {
    pub(crate) fn cost(&self) -> TextureRequestCost {
        self.cost
    }

    pub(super) fn check(&self, id: &TextureResidentId) -> Result<(), TextureAssetError> {
        self.admitted
            .get(id)
            .map_or(Err(TextureAssetError::UnplannedRequest), Clone::clone)
    }

    pub(super) fn release(&self, id: &TextureResidentId) {
        self.budget.release(id);
    }
}

#[derive(Clone, Debug, Default)]
pub(super) enum TextureAccess {
    #[default]
    Direct,
    Planning(Arc<Mutex<Option<TextureRequestPlan>>>),
    Admitted(Arc<TextureAdmissions>),
}

impl TextureAccess {
    pub(super) fn source_request(&self, token: u64) -> Result<(), TextureAssetError> {
        match self {
            Self::Direct => Ok(()),
            Self::Planning(recorder) => {
                recorder
                    .lock()
                    .as_mut()
                    .ok_or(TextureAssetError::ClosedPlan)?
                    .sources
                    .insert(token);
                Ok(())
            }
            Self::Admitted(admissions) if admissions.sources.contains(&token) => Ok(()),
            Self::Admitted(_) => Err(TextureAssetError::UnplannedRequest),
        }
    }

    pub(super) fn source(&self, bytes: super::TextureBytes) {
        if let Self::Planning(recorder) = self
            && let Some(plan) = recorder.lock().as_mut()
        {
            plan.source(bytes);
        }
    }
    pub(super) fn record(
        &self,
        id: &TextureResidentId,
        cost: TextureRequestCost,
        source: Arc<SelectedTextureAsset>,
        preflight_error: Option<TextureAssetError>,
    ) -> Result<bool, TextureAssetError> {
        if let Self::Planning(recorder) = self {
            recorder
                .lock()
                .as_mut()
                .ok_or(TextureAssetError::ClosedPlan)?
                .push(TextureRequest {
                    id: id.clone(),
                    cost,
                    source,
                    preflight_error,
                });
            Ok(true)
        } else {
            Ok(false)
        }
    }
}

impl MaterialResolver {
    pub(crate) fn admit_texture_requests(
        &self,
        plan: TextureRequestPlan,
    ) -> Arc<TextureAdmissions> {
        plan.admit(self.config.decoded_texture_budget.as_ref().map_or_else(
            || Arc::new(DecodedTextureBudget::new(usize::MAX)),
            Arc::clone,
        ))
    }
    pub(crate) fn plan_texture_requests<T>(
        &self,
        run: impl FnOnce(&Self) -> T,
    ) -> (T, TextureRequestPlan) {
        let recorder = Arc::new(Mutex::new(Some(TextureRequestPlan::default())));
        let resolver = self.variant(super::ResolverConfig {
            texture_access: TextureAccess::Planning(Arc::clone(&recorder)),
            ..self.config.clone()
        });
        let result = run(&resolver);
        let plan = recorder.lock().take().expect("texture plan closes once");
        (result, plan)
    }

    pub(crate) fn with_texture_admissions(
        &self,
        admissions: Arc<super::TextureAdmissions>,
    ) -> Self {
        self.variant(super::ResolverConfig {
            texture_access: TextureAccess::Admitted(admissions),
            ..self.config.clone()
        })
    }

    pub(super) fn with_single_texture_mip(&self) -> Self {
        self.variant(super::ResolverConfig {
            single_texture_mip: true,
            ..self.config.clone()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::materials::TextureLoadOutcome;
    use crate::test_support::fixture_vtf_bytes;
    use vformats::vtf::VtfFormat;

    fn resolver() -> MaterialResolver {
        MaterialResolver::prop_lighting_fixture(
            ["a", "b", "c"].into_iter().enumerate().flat_map(|(index, name)| {
                [
                    (format!("materials/test/{name}.vmt"), format!("LightmappedGeneric {{ \"$basetexture\" \"test/{name}\" \"$bumpmap\" \"test/{name}\" }}").into_bytes()),
                    (format!("materials/test/{name}.vtf"), fixture_vtf_bytes(2, 2, VtfFormat::Rgba8888, &[&[index as u8;4], &[index as u8;16]])),
                ]
            }).collect::<Vec<_>>().iter().map(|(path, bytes)| (path.as_str(), bytes.clone())).collect(),
            Vec::new(),
        )
    }

    fn material(resolver: &MaterialResolver, name: &str) -> super::super::ResolvedPrimaryMaterial {
        resolver.resolve_primary(&[], name).into_material().unwrap()
    }

    #[test]
    fn planning_preserves_capabilities_without_materializing_or_charging() {
        let budget = Arc::new(DecodedTextureBudget::new(20));
        let resolver = resolver().with_decoded_texture_budget(Arc::clone(&budget));
        let (planned, plan) =
            resolver.plan_texture_requests(|resolver| material(resolver, "test/a"));
        assert_eq!(
            planned.texture.as_ref().unwrap().load_outcome(),
            TextureLoadOutcome::Deferred
        );
        assert!(planned.texture.as_ref().unwrap().resident().is_none());
        assert!(
            planned
                .texture
                .as_ref()
                .unwrap()
                .has_effective_base_texture()
        );
        assert_eq!(
            planned.texture.as_ref().unwrap().mapping_dimensions(),
            Some((2, 2))
        );
        assert_eq!(
            planned
                .shader
                .surface()
                .unwrap()
                .normal
                .as_ref()
                .unwrap()
                .load_outcome(),
            TextureLoadOutcome::Deferred
        );
        assert_eq!(plan.requests.len(), 1);
        assert_eq!(plan.cost().resident_bytes, 20);
        assert_eq!(plan.cost().decode_peak_bytes, 40);
        assert!(plan.cost().source_encoded_bytes > 20);
        assert_eq!(budget.decoded_bytes(), 0);
        assert_eq!(resolver.shared.texture_assets.resident_count(), 0);
        let admitted = resolver.with_texture_admissions(resolver.admit_texture_requests(plan));
        assert_eq!(
            material(&admitted, "test/a")
                .texture
                .as_ref()
                .unwrap()
                .load_outcome(),
            TextureLoadOutcome::Loaded
        );
        assert_eq!(budget.decoded_bytes(), 20);
    }

    #[test]
    fn image_and_payload_admission_is_atomic_deduplicated_and_ordered() {
        for _ in 0..8 {
            let budget = Arc::new(DecodedTextureBudget::with_image_limit(100, 3));
            let resolver = resolver().with_decoded_texture_budget(Arc::clone(&budget));
            let mut plan = TextureRequestPlan::default();
            for name in ["test/a", "test/a", "test/b", "test/c"] {
                let (_, next) = resolver.plan_texture_requests(|resolver| material(resolver, name));
                plan.append(next);
            }
            assert_eq!(plan.cost().image_count, 6);
            assert_eq!(plan.cost().layer_count, 3);
            let admissions = resolver.admit_texture_requests(plan);
            assert_eq!(budget.decoded_bytes(), 20);
            assert_eq!(budget.admitted_images(), 2);
            let admitted = resolver.with_texture_admissions(admissions);
            std::thread::scope(|scope| {
                for (name, expected) in [
                    ("test/c", TextureLoadOutcome::QualityRejected),
                    ("test/a", TextureLoadOutcome::Loaded),
                    ("test/b", TextureLoadOutcome::QualityRejected),
                    ("test/a", TextureLoadOutcome::Loaded),
                ] {
                    let admitted = &admitted;
                    scope.spawn(move || {
                        assert_eq!(
                            material(admitted, name)
                                .texture
                                .as_ref()
                                .unwrap()
                                .load_outcome(),
                            expected
                        );
                    });
                }
            });
            assert_eq!(budget.decoded_bytes(), 20);
            assert_eq!(budget.admitted_images(), 2);
            assert_eq!(budget.rejected_textures(), 2);
        }
    }

    #[test]
    fn warm_residents_still_require_image_admission() {
        let resolver = resolver();
        assert_eq!(
            material(&resolver, "test/a")
                .texture
                .as_ref()
                .unwrap()
                .load_outcome(),
            TextureLoadOutcome::Loaded
        );
        let budget = Arc::new(DecodedTextureBudget::with_image_limit(100, 1));
        let resolver = resolver.with_decoded_texture_budget(Arc::clone(&budget));
        let (_, plan) = resolver.plan_texture_requests(|resolver| material(resolver, "test/a"));
        let admitted = resolver.with_texture_admissions(resolver.admit_texture_requests(plan));
        assert_eq!(
            material(&admitted, "test/a")
                .texture
                .as_ref()
                .unwrap()
                .load_outcome(),
            TextureLoadOutcome::QualityRejected
        );
        assert_eq!(budget.decoded_bytes(), 0);
        assert_eq!(budget.admitted_images(), 0);
    }

    #[test]
    fn tiny_animated_images_can_hit_count_limit_before_payload_limit() {
        let entries = [("a", 64_u16), ("b", 64), ("c", 1)]
            .into_iter()
            .flat_map(|(name, frames)| {
                let mut bytes = fixture_vtf_bytes(
                    1,
                    1,
                    VtfFormat::Rgba8888,
                    &[&vec![name.as_bytes()[0]; usize::from(frames) * 4]],
                );
                bytes[24..26].copy_from_slice(&frames.to_le_bytes());
                [
                    (format!("materials/test/{name}.vtf"), bytes),
                    (
                        format!("materials/test/{name}.vmt"),
                        format!("LightmappedGeneric {{ \"$basetexture\" \"test/{name}\" }}")
                            .into_bytes(),
                    ),
                ]
            })
            .collect::<Vec<_>>();
        let resolver = MaterialResolver::prop_lighting_fixture(
            entries
                .iter()
                .map(|(name, bytes)| (name.as_str(), bytes.clone()))
                .collect(),
            Vec::new(),
        );
        let budget = Arc::new(DecodedTextureBudget::with_image_limit(2048, 96));
        let resolver = resolver.with_decoded_texture_budget(Arc::clone(&budget));
        let (_, plan) = resolver.plan_texture_requests(|resolver| {
            ["test/a", "test/b", "test/c"].map(|name| material(resolver, name))
        });
        assert_eq!(plan.cost().resident_bytes, 516);
        assert_eq!(plan.cost().image_count, 129);
        assert!(plan.cost().resident_record_bytes > plan.cost().resident_bytes);
        let admitted = resolver.with_texture_admissions(resolver.admit_texture_requests(plan));
        assert_eq!(
            material(&admitted, "test/a")
                .texture
                .as_ref()
                .unwrap()
                .load_outcome(),
            TextureLoadOutcome::Loaded
        );
        assert_eq!(
            material(&admitted, "test/b")
                .texture
                .as_ref()
                .unwrap()
                .load_outcome(),
            TextureLoadOutcome::QualityRejected
        );
        assert_eq!(
            material(&admitted, "test/c")
                .texture
                .as_ref()
                .unwrap()
                .load_outcome(),
            TextureLoadOutcome::Loaded
        );
        assert_eq!(budget.decoded_bytes(), 260);
        assert_eq!(budget.admitted_images(), 65);
    }

    #[test]
    fn ordered_merge_decides_winners_independently_of_discovery_and_warm_cache() {
        for discovery in [
            ["test/c", "test/b", "test/a"],
            ["test/a", "test/b", "test/c"],
        ] {
            let resolver = resolver();
            let _warm = material(&resolver, "test/b");
            let mut plans = HashMap::new();
            for name in discovery {
                let (_, plan) = resolver.plan_texture_requests(|resolver| material(resolver, name));
                plans.insert(name, plan);
            }
            let mut plan = TextureRequestPlan::default();
            for name in ["test/a", "test/b", "test/c"] {
                plan.append(plans.remove(name).unwrap());
            }
            let budget = Arc::new(DecodedTextureBudget::new(20));
            let resolver = resolver.with_decoded_texture_budget(Arc::clone(&budget));
            let admitted = resolver.with_texture_admissions(resolver.admit_texture_requests(plan));
            assert_eq!(
                material(&admitted, "test/a")
                    .texture
                    .as_ref()
                    .unwrap()
                    .load_outcome(),
                TextureLoadOutcome::Loaded
            );
            for name in ["test/b", "test/c"] {
                assert_eq!(
                    material(&admitted, name)
                        .texture
                        .as_ref()
                        .unwrap()
                        .load_outcome(),
                    TextureLoadOutcome::QualityRejected
                );
            }
            assert_eq!(budget.decoded_bytes(), 20);
            assert_eq!(budget.rejected_textures(), 2);
        }
    }

    #[test]
    fn unplanned_and_closed_recorders_cannot_decode() {
        let resolver = resolver();
        let (escaped, plan) =
            resolver.plan_texture_requests(|resolver| resolver.variant(resolver.config.clone()));
        assert_eq!(
            material(&escaped, "test/a")
                .texture
                .as_ref()
                .unwrap()
                .load_outcome(),
            TextureLoadOutcome::Unplanned
        );
        let admitted = resolver.with_texture_admissions(resolver.admit_texture_requests(plan));
        assert_eq!(
            material(&admitted, "test/a")
                .texture
                .as_ref()
                .unwrap()
                .load_outcome(),
            TextureLoadOutcome::Unplanned
        );
        assert_eq!(resolver.shared.texture_assets.resident_count(), 0);
    }

    #[test]
    fn failed_decode_does_not_backfill_declined_requests_in_later_phases() {
        let mut unsupported = fixture_vtf_bytes(1, 1, VtfFormat::Rgba8888, &[&[42]]);
        unsupported[52..56].copy_from_slice(&7_i32.to_le_bytes());
        let resolver = MaterialResolver::prop_lighting_fixture(
            vec![
                (
                    "materials/test/a.vmt",
                    br#"LightmappedGeneric { "$basetexture" "test/a" }"#.to_vec(),
                ),
                ("materials/test/a.vtf", unsupported),
                (
                    "materials/test/b.vmt",
                    br#"LightmappedGeneric { "$basetexture" "test/b" }"#.to_vec(),
                ),
                (
                    "materials/test/b.vtf",
                    fixture_vtf_bytes(2, 2, VtfFormat::Rgba8888, &[&[0; 4], &[0; 16]]),
                ),
            ],
            Vec::new(),
        );
        let (_, mut plan) = resolver.plan_texture_requests(|resolver| material(resolver, "test/a"));
        let (_, next) = resolver.plan_texture_requests(|resolver| material(resolver, "test/b"));
        plan.append(next);
        let budget = Arc::new(DecodedTextureBudget::new(20));
        let resolver = resolver.with_decoded_texture_budget(Arc::clone(&budget));
        let admissions = resolver.admit_texture_requests(plan);
        let admitted = resolver.with_texture_admissions(admissions);
        assert_eq!(budget.decoded_bytes(), 4);
        assert_eq!(budget.admitted_images(), 1);
        for _ in 0..2 {
            assert_eq!(
                material(&admitted, "test/a")
                    .texture
                    .as_ref()
                    .unwrap()
                    .load_outcome(),
                TextureLoadOutcome::Unsupported
            );
            assert_eq!(budget.decoded_bytes(), 0);
            assert_eq!(budget.admitted_images(), 0);
        }
        let (_, later) = resolver.plan_texture_requests(|resolver| material(resolver, "test/b"));
        let later = resolver.with_texture_admissions(resolver.admit_texture_requests(later));
        assert_eq!(
            material(&later, "test/b")
                .texture
                .as_ref()
                .unwrap()
                .load_outcome(),
            TextureLoadOutcome::QualityRejected
        );
        assert_eq!(budget.decoded_bytes(), 0);
    }

    #[test]
    fn backing_cost_counts_physical_allocations_without_double_charging_aliases() {
        let bytes = super::super::TextureBytes::owned(vec![7; 100]);
        let duplicate = super::super::TextureBytes::owned(vec![7; 100]);
        let mut first = TextureRequestPlan::default();
        first.source(bytes.legacy_alias());
        let mut second = TextureRequestPlan::default();
        second.source(bytes);
        second.source(duplicate);
        first.append(second);
        assert_eq!(first.cost().source_encoded_bytes, 200);
        assert_eq!(first.cost().resident_bytes, 0);
    }

    #[test]
    fn sky_single_level_recipe_is_planned_before_budget_admission() {
        let resolver = resolver();
        let sky = resolver.with_single_texture_mip();
        let (_, plan) = sky.plan_texture_requests(|resolver| material(resolver, "test/a"));
        assert_eq!(plan.cost().resident_bytes, 16);
        assert_eq!(plan.requests[0].id.mip_count, 1);
        let budget = Arc::new(DecodedTextureBudget::new(16));
        let sky = sky.with_decoded_texture_budget(Arc::clone(&budget));
        let admitted = sky.with_texture_admissions(sky.admit_texture_requests(plan));
        let material = material(&admitted, "test/a");
        assert_eq!(
            material
                .texture
                .as_ref()
                .unwrap()
                .source_metadata()
                .unwrap()
                .mip_count,
            2
        );
        assert_eq!(
            material
                .texture
                .as_ref()
                .unwrap()
                .resident()
                .unwrap()
                .id()
                .mip_count,
            1
        );
        assert_eq!(budget.decoded_bytes(), 16);
    }
    #[test]
    fn invalid_source_backing_is_reported_without_creating_a_resident_request() {
        let resolver = MaterialResolver::prop_lighting_fixture(
            vec![
                (
                    "materials/test/invalid.vmt",
                    br#"LightmappedGeneric { "$basetexture" "test/invalid" }"#.to_vec(),
                ),
                (
                    "materials/test/invalid.vtf",
                    b"invalid-selected-vtf".to_vec(),
                ),
            ],
            Vec::new(),
        );
        let (resolved, mut plan) =
            resolver.plan_texture_requests(|resolver| material(resolver, "test/invalid"));
        assert_eq!(
            resolved.texture.as_ref().unwrap().load_outcome(),
            TextureLoadOutcome::InvalidSource
        );
        assert_eq!(
            plan.cost().source_encoded_bytes,
            b"invalid-selected-vtf".len()
        );
        assert_eq!(plan.cost().resident_bytes, 0);
        let (_, duplicate) =
            resolver.plan_texture_requests(|resolver| material(resolver, "test/invalid"));
        plan.append(duplicate);
        assert_eq!(
            plan.cost().source_encoded_bytes,
            b"invalid-selected-vtf".len()
        );
    }
}
