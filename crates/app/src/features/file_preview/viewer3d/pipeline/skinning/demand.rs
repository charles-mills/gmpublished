use super::super::super::draw_plan::DrawPlan;
use super::super::PreparedFrame;
use super::super::reflections::SceneView;
use super::super::residency_demand::ViewKey;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Weak};

#[derive(Debug)]
struct Entry {
    plan: Weak<DrawPlan>,
    instances: Arc<BTreeSet<usize>>,
}

#[derive(Debug, Default)]
pub(in super::super) struct Cache {
    plans: BTreeMap<(ViewKey, bool), Entry>,
}

impl Cache {
    fn view(
        &mut self,
        frame: &PreparedFrame,
        view: &SceneView,
        key: ViewKey,
        active: &mut BTreeSet<(ViewKey, bool)>,
        demanded: &mut BTreeSet<usize>,
    ) -> Result<(), super::frame::Error> {
        let Some(plans) = &view.plans else {
            return Ok(());
        };
        for (skybox, plan) in [
            (false, Some(&plans.world)),
            (true, plans.map_skybox.as_ref()),
        ] {
            let Some(plan) = plan else {
                continue;
            };
            for item in &plan.planar {
                if let Some(&instance) = frame
                    .scene
                    .assets
                    .animations
                    .mesh_instances
                    .get(&item.mesh_index)
                    .and_then(|owners| owners.first())
                {
                    return Err(super::frame::Error::UnsupportedPlanarMaterial {
                        instance,
                        mesh: item.mesh_index,
                        material: item.material_slot,
                    });
                }
            }
            let key = (key, skybox);
            active.insert(key);
            let entry = self.plans.entry(key).or_insert_with(|| Entry {
                plan: Weak::new(),
                instances: Arc::default(),
            });
            if !entry.plan.ptr_eq(&Arc::downgrade(plan)) {
                let mut instances = BTreeSet::new();
                for item in plan
                    .opaque
                    .iter()
                    .chain(&plan.translucent)
                    .chain(&plan.additive)
                    .chain(&plan.planar)
                {
                    if let Some(owners) = frame
                        .scene
                        .assets
                        .animations
                        .mesh_instances
                        .get(&item.mesh_index)
                    {
                        instances.extend(owners);
                    }
                }
                entry.plan = Arc::downgrade(plan);
                entry.instances = Arc::new(instances);
            }
            demanded.extend(entry.instances.iter().copied());
        }
        Ok(())
    }

    pub fn collect(
        &mut self,
        frame: &PreparedFrame,
    ) -> Result<BTreeSet<usize>, super::frame::Error> {
        let mut active = BTreeSet::new();
        let mut demanded = BTreeSet::new();
        if !frame.scene.assets.animations.instances.is_empty() {
            self.view(
                frame,
                &frame.main,
                ViewKey::Main,
                &mut active,
                &mut demanded,
            )?;
            for (index, planar) in frame.planar_views.iter().enumerate() {
                if let Some(view) = &planar.reflection {
                    self.view(
                        frame,
                        view,
                        ViewKey::Reflection(index),
                        &mut active,
                        &mut demanded,
                    )?;
                }
                if let Some(view) = &planar.refraction {
                    self.view(
                        frame,
                        view,
                        ViewKey::Refraction(index),
                        &mut active,
                        &mut demanded,
                    )?;
                }
            }
            if let Some(intersection) = frame
                .water_intersection
                .as_ref()
                .filter(|view| view.selection.is_some())
            {
                self.view(
                    frame,
                    &intersection.view,
                    ViewKey::Intersection,
                    &mut active,
                    &mut demanded,
                )?;
            }
        }
        self.plans.retain(|key, _| active.contains(key));
        Ok(demanded)
    }
}
