use super::super::draw_plan::{DrawPlan, DrawPlans};
use super::reflections::SceneView;
use super::transparency::{Primitive, WorldDrawGroups};
use super::{ModelPrimitive, PreparedFrame};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Weak};

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub(super) enum DemandError {
    #[error("material animation: {0}")]
    Animation(String),
    #[error("demanded material {slot} exceeds material count {count}")]
    InvalidMaterial { slot: usize, count: usize },
    #[error("prepared detail draw has no {skybox:?} detail material")]
    MissingDetails { skybox: bool },
}

#[derive(Debug)]
struct PlanEntry {
    plan: Weak<DrawPlan>,
    groups: WorldDrawGroups,
    materials: Arc<[usize]>,
}

#[derive(Debug, Default)]
pub(super) struct ViewDemandCache {
    world: Option<PlanEntry>,
    skybox: Option<PlanEntry>,
}

fn plan_materials(plan: &DrawPlan, groups: WorldDrawGroups) -> Arc<[usize]> {
    let mut slots = BTreeSet::new();
    for item in &plan.opaque {
        if plan
            .opaque_ranges
            .get(&item.mesh_index)
            .is_none_or(|ranges| ranges.indices.iter().any(|range| !range.is_empty()))
        {
            slots.insert(item.material_slot);
        }
    }
    slots.extend(plan.planar.iter().map(|item| item.material_slot));
    slots.extend(plan.surface_marks.iter().map(|item| item.material_slot));
    for batch in &plan.world_opaque.batches {
        if groups.allows(batch.material_group) {
            slots.extend(
                batch
                    .surfaces
                    .meshes
                    .iter()
                    .filter(|mesh| !mesh.indices.is_empty())
                    .map(|mesh| mesh.item.material_slot),
            );
            slots.extend(
                batch
                    .surfaces
                    .surface_marks
                    .iter()
                    .map(|mark| mark.material_slot),
            );
        }
    }
    for batch in &plan.brushes {
        slots.extend(
            batch
                .opaque
                .meshes
                .iter()
                .filter(|mesh| !mesh.indices.is_empty())
                .map(|mesh| mesh.item.material_slot),
        );
        slots.extend(
            batch
                .opaque
                .surface_marks
                .iter()
                .map(|mark| mark.material_slot),
        );
    }
    slots.into_iter().collect()
}

fn cached_plan(
    entry: &mut Option<PlanEntry>,
    plan: &Arc<DrawPlan>,
    groups: WorldDrawGroups,
) -> Arc<[usize]> {
    if let Some(entry) = entry.as_ref()
        && entry.groups == groups
        && entry
            .plan
            .upgrade()
            .is_some_and(|cached| Arc::ptr_eq(&cached, plan))
    {
        return Arc::clone(&entry.materials);
    }
    let materials = plan_materials(plan, groups);
    *entry = Some(PlanEntry {
        plan: Arc::downgrade(plan),
        groups,
        materials: Arc::clone(&materials),
    });
    materials
}

impl ViewDemandCache {
    pub(super) fn collect(
        &mut self,
        view: &SceneView,
        details: [Option<usize>; 2],
        material_count: usize,
        include_skybox: bool,
    ) -> Result<Arc<[usize]>, DemandError> {
        let Some(plans) = &view.plans else {
            *self = Self::default();
            return Ok(Arc::from([]));
        };
        self.collect_parts(
            plans,
            view.world_groups,
            [&view.transparency, &view.skybox_transparency],
            details,
            material_count,
            include_skybox,
        )
    }

    fn collect_parts(
        &mut self,
        plans: &DrawPlans,
        groups: WorldDrawGroups,
        queues: [&[super::transparency::Draw]; 2],
        details: [Option<usize>; 2],
        material_count: usize,
        include_skybox: bool,
    ) -> Result<Arc<[usize]>, DemandError> {
        let mut slots: BTreeSet<_> = cached_plan(&mut self.world, &plans.world, groups)
            .iter()
            .copied()
            .collect();
        if include_skybox && let Some(plan) = &plans.map_skybox {
            slots.extend(
                cached_plan(&mut self.skybox, plan, WorldDrawGroups::ALL)
                    .iter()
                    .copied(),
            );
        } else {
            self.skybox = None;
        }
        for (skybox, queue) in queues.into_iter().enumerate() {
            if skybox != 0 && (!include_skybox || plans.map_skybox.is_none()) {
                continue;
            }
            for draw in queue {
                let slot = match &draw.primitive {
                    Primitive::Mesh(item, range) => {
                        if range.as_ref().is_some_and(std::ops::Range::is_empty) {
                            continue;
                        }
                        item.material_slot
                    }
                    Primitive::Overlay(item) => item.material_slot,
                    Primitive::Sprite(range) => {
                        if range.is_empty() {
                            continue;
                        }
                        details[skybox].ok_or(DemandError::MissingDetails {
                            skybox: skybox != 0,
                        })?
                    }
                };
                slots.insert(slot);
            }
        }
        if let Some(&slot) = slots.iter().find(|&&slot| slot >= material_count) {
            return Err(DemandError::InvalidMaterial {
                slot,
                count: material_count,
            });
        }
        Ok(slots.into_iter().collect())
    }
}

fn sorted_material_slots(
    slots: Arc<[usize]>,
    additional: impl IntoIterator<Item = usize>,
) -> Arc<[usize]> {
    let mut additions: Vec<_> = additional
        .into_iter()
        .filter(|slot| slots.binary_search(slot).is_err())
        .collect();
    if additions.is_empty() {
        return slots;
    }
    additions.sort_unstable();
    additions.dedup();
    let mut merged = Vec::with_capacity(slots.len() + additions.len());
    let mut original = slots.iter().copied().peekable();
    for slot in additions {
        while original.peek().is_some_and(|&existing| existing < slot) {
            if let Some(existing) = original.next() {
                merged.push(existing);
            }
        }
        merged.push(slot);
    }
    merged.extend(original);
    merged.into()
}

fn complete_material_slots(
    slots: Arc<[usize]>,
    additional: impl IntoIterator<Item = usize>,
    material_count: usize,
) -> Result<Arc<[usize]>, DemandError> {
    #[cfg(feature = "map-capture")]
    let slots = {
        static SORTED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if *SORTED.get_or_init(|| {
            std::env::var_os("GMPUBLISHED_SORTED_MATERIAL_DEMAND").as_deref()
                != Some(std::ffi::OsStr::new("0"))
        }) {
            sorted_material_slots(slots, additional)
        } else {
            let mut original: BTreeSet<_> = slots.iter().copied().collect();
            original.extend(additional);
            original.into_iter().collect()
        }
    };
    #[cfg(not(feature = "map-capture"))]
    let slots = sorted_material_slots(slots, additional);
    if let Some(&slot) = slots.iter().find(|&&slot| slot >= material_count) {
        return Err(DemandError::InvalidMaterial {
            slot,
            count: material_count,
        });
    }
    Ok(slots)
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) enum ViewKey {
    Main,
    Reflection(usize),
    Refraction(usize),
    Intersection,
}

#[derive(Debug)]
pub(super) struct ViewMaterialDemand {
    pub materials: Arc<[usize]>,
    pub sky_background: bool,
}

#[derive(Debug, Default)]
pub(super) struct FrameMaterialDemand {
    pub views: Vec<ViewMaterialDemand>,
    pub immersion_effect: Option<String>,
}

#[derive(Debug, Default)]
pub(super) struct FrameDemandCache {
    views: BTreeMap<ViewKey, ViewDemandCache>,
}

impl FrameDemandCache {
    pub(super) fn collect(
        &mut self,
        frame: &PreparedFrame,
        primitive: &ModelPrimitive,
    ) -> Result<FrameMaterialDemand, DemandError> {
        let upload = &frame.scene.assets;
        let details = [
            upload
                .detail_sprites
                .as_ref()
                .map(|sprites| sprites.material_index),
            upload
                .map_skybox_detail_sprites
                .as_ref()
                .map(|sprites| sprites.material_index),
        ];
        let material_count = primitive.preview.render_scene().materials.len();
        let mut active = BTreeSet::from([ViewKey::Main]);
        for optical in &frame.planar_views {
            if optical.reflection.is_some() {
                active.insert(ViewKey::Reflection(optical.surface_index));
            }
            if optical.refraction.is_some() {
                active.insert(ViewKey::Refraction(optical.surface_index));
            }
        }
        if frame
            .water_intersection
            .as_ref()
            .is_some_and(|view| view.selection.is_some())
        {
            active.insert(ViewKey::Intersection);
        }
        self.views.retain(|key, _| active.contains(key));
        let mut output = FrameMaterialDemand::default();
        let mut add =
            |key, view: &SceneView, sky_background: bool, main: bool| -> Result<(), DemandError> {
                let slots = self.views.entry(key).or_default().collect(
                    view,
                    details,
                    material_count,
                    key != ViewKey::Intersection,
                )?;
                let debug_meshes = (main && primitive.phy_debug_visible)
                    .then(|| upload.phy_debug_meshes.get())
                    .flatten();
                let additional = sky_background
                    .then_some(&view.sun)
                    .into_iter()
                    .flat_map(super::sun::gpu::Frame::material_slots)
                    .chain(
                        debug_meshes
                            .into_iter()
                            .flatten()
                            .map(|mesh| mesh.material_index),
                    );
                output.views.push(ViewMaterialDemand {
                    materials: complete_material_slots(slots, additional, material_count)?,
                    sky_background,
                });
                Ok(())
            };
        add(ViewKey::Main, &frame.main, !primitive.submerged, true)?;
        for optical in &frame.planar_views {
            if let Some(view) = &optical.reflection {
                add(
                    ViewKey::Reflection(optical.surface_index),
                    view,
                    true,
                    false,
                )?;
            }
            if let Some(view) = &optical.refraction {
                let material = upload.reflectors[optical.surface_index].material_slot;
                let sky = primitive.preview.render_scene().materials[material]
                    .shader
                    .water()
                    .is_none_or(|water| !water.above_water);
                add(ViewKey::Refraction(optical.surface_index), view, sky, false)?;
            }
        }
        if let Some(intersection) = frame
            .water_intersection
            .as_ref()
            .filter(|view| view.selection.is_some())
        {
            add(ViewKey::Intersection, &intersection.view, false, false)?;
        }
        let eye = super::Vec3::from([
            primitive.uniforms.camera_position[0],
            primitive.uniforms.camera_position[1],
            primitive.uniforms.camera_position[2],
        ]);
        output.immersion_effect = primitive
            .preview
            .map()
            .and_then(|map| map.immersion_material(eye))
            .and_then(|water| water.underwater_overlay.as_deref())
            .map(|effect| effect.name.clone());
        Ok(output)
    }
}

// Retain this guard from finalization through encoding; arm it only after queue.submit.
#[must_use = "retain pins through encoder submission or explicitly cancel the unsubmitted frame"]
#[derive(Debug)]
pub(super) struct SubmissionPins<P> {
    pins: Vec<Arc<P>>,
}
impl<P: Send + Sync + 'static> SubmissionPins<P> {
    pub(super) fn new(pins: Vec<Arc<P>>) -> Self {
        Self { pins }
    }
    pub(super) fn submitted(
        self,
        queue: &super::wgpu::Queue,
        completion: &Arc<super::completion::CompletionDriver>,
    ) {
        if !self.pins.is_empty() {
            self.retire_with(|completed| completion.on_done(queue, completed));
        }
    }
    fn retire_with(self, register: impl FnOnce(Box<dyn FnOnce() + Send>)) {
        register(Box::new(move || drop(self.pins)));
    }
}

#[cfg(test)]
mod tests;
