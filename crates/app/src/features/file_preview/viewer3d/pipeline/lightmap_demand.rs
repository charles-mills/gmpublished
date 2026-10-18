use super::super::draw_plan::{DrawItem, DrawPlan, OverlayDrawItem};
use super::geometry::ViewerScene;
use super::reflections::SceneView;
use super::residency_demand::ViewKey;
use super::transparency::{Primitive, WorldDrawGroups};
use super::visibility::UploadedVisibility;
use gmpublished_domain::scene::map::LightmapPageId;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Weak};

type Pages = BTreeSet<LightmapPageId>;

#[derive(Debug)]
struct PlanPages {
    plan: Weak<DrawPlan>,
    revision: u64,
    groups: WorldDrawGroups,
    pages: Arc<Pages>,
}

#[derive(Debug, Default)]
pub(super) struct Cache {
    plans: BTreeMap<(ViewKey, bool), PlanPages>,
}

fn add_ranges(pages: &mut Pages, ranges: &[super::lightmap_ranges::PageRange]) {
    pages.extend(ranges.iter().filter_map(|range| range.page));
}

fn mesh(
    pages: &mut Pages,
    upload: &ViewerScene,
    visibility: &UploadedVisibility,
    item: DrawItem,
    range: Option<std::ops::Range<u32>>,
) {
    let source = &upload.meshes[item.mesh_index];
    if let Some(range) = range {
        pages.extend(
            source
                .lightmaps
                .intersect(range)
                .filter_map(|range| range.page),
        );
    } else if let Some(visible) = visibility.mesh_indices.get(&item.mesh_index) {
        add_ranges(pages, &visible.lightmaps);
    } else {
        add_ranges(pages, &source.lightmaps.indexed);
    }
}

fn mark(pages: &mut Pages, upload: &ViewerScene, item: OverlayDrawItem) {
    add_ranges(pages, &upload.overlays[item.overlay_index].lightmaps);
}

fn opaque(
    upload: &ViewerScene,
    visibility: &UploadedVisibility,
    plan: &DrawPlan,
    groups: WorldDrawGroups,
) -> Pages {
    let mut pages = Pages::new();
    for &item in &plan.opaque {
        if let Some(ranges) = plan.opaque_ranges.get(&item.mesh_index) {
            for range in &ranges.indices {
                mesh(&mut pages, upload, visibility, item, Some(range.clone()));
            }
        } else {
            mesh(&mut pages, upload, visibility, item, None);
        }
    }
    for &item in &plan.surface_marks {
        mark(&mut pages, upload, item);
    }
    for batch in &plan.world_opaque.batches {
        if groups.allows(batch.material_group) {
            for item in &batch.surfaces.meshes {
                mesh(
                    &mut pages,
                    upload,
                    visibility,
                    item.item,
                    Some(item.indices.clone()),
                );
            }
            for &item in &batch.surfaces.surface_marks {
                mark(&mut pages, upload, item);
            }
        }
    }
    for batch in &plan.brushes {
        for item in &batch.opaque.meshes {
            mesh(
                &mut pages,
                upload,
                visibility,
                item.item,
                Some(item.indices.clone()),
            );
        }
        for &item in &batch.opaque.surface_marks {
            mark(&mut pages, upload, item);
        }
    }
    pages
}

impl Cache {
    fn view(
        &mut self,
        key: ViewKey,
        view: &SceneView,
        upload: &ViewerScene,
        include_skybox: bool,
        output: &mut Pages,
    ) {
        let Some(plans) = &view.plans else { return };
        for (skybox, plan, visibility, groups, queue) in std::iter::once((
            false,
            &plans.world,
            &view.visibility,
            view.world_groups,
            &view.transparency,
        ))
        .chain(
            plans
                .map_skybox
                .as_ref()
                .filter(|_| include_skybox)
                .map(|plan| {
                    (
                        true,
                        plan,
                        &view.skybox_visibility,
                        WorldDrawGroups::ALL,
                        &view.skybox_transparency,
                    )
                }),
        ) {
            let entry_key = (key, skybox);
            let cached = self.plans.get(&entry_key).filter(|entry| {
                entry.revision == visibility.revision
                    && entry.groups == groups
                    && entry
                        .plan
                        .upgrade()
                        .is_some_and(|previous| Arc::ptr_eq(&previous, plan))
            });
            let pages = if let Some(entry) = cached {
                Arc::clone(&entry.pages)
            } else {
                let pages = Arc::new(opaque(upload, visibility, plan, groups));
                self.plans.insert(
                    entry_key,
                    PlanPages {
                        plan: Arc::downgrade(plan),
                        revision: visibility.revision,
                        groups,
                        pages: Arc::clone(&pages),
                    },
                );
                pages
            };
            output.extend(pages.iter().copied());
            for &item in &plan.planar {
                mesh(output, upload, visibility, item, None);
            }
            for draw in queue {
                match &draw.primitive {
                    Primitive::Mesh(item, range) => {
                        mesh(output, upload, visibility, *item, range.clone());
                    }
                    Primitive::Overlay(item) => mark(output, upload, *item),
                    Primitive::Sprite(_) => {}
                }
            }
        }
    }

    pub(super) fn collect(&mut self, frame: &super::PreparedFrame) -> Pages {
        let mut pages = Pages::new();
        let mut active = BTreeSet::new();
        let upload = &frame.scene;
        active.insert(ViewKey::Main);
        self.view(ViewKey::Main, &frame.main, upload, true, &mut pages);
        for optical in &frame.planar_views {
            if let Some(view) = &optical.reflection {
                let key = ViewKey::Reflection(optical.surface_index);
                active.insert(key);
                self.view(key, view, upload, true, &mut pages);
            }
            if let Some(view) = &optical.refraction {
                let key = ViewKey::Refraction(optical.surface_index);
                active.insert(key);
                self.view(key, view, upload, true, &mut pages);
            }
            add_ranges(&mut pages, &optical.surface_buffer.lightmaps);
        }
        if let Some(intersection) = frame
            .water_intersection
            .as_ref()
            .filter(|view| view.selection.is_some())
        {
            active.insert(ViewKey::Intersection);
            self.view(
                ViewKey::Intersection,
                &intersection.view,
                upload,
                false,
                &mut pages,
            );
        }
        self.plans.retain(|(view, _), _| active.contains(view));
        pages
    }
}
