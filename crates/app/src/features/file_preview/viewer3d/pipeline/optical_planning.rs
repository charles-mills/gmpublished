use super::super::draw_plan::{DrawPlanRequest, DrawPlanSelection};
use super::draw::ViewKind;
use super::reflections::{PlanarView, Plane, SceneView};
use super::refraction::OpticalInputs;
use super::targets::{WaterBindingInputs, WaterBindings};
use super::transparency::WaterSide;
use super::view_preparation::Entities;
use super::{ModelPrimitive, RenderResources, Uniforms, ViewerScene, wgpu};
use gmpublished_backend::CpuExecutor;

const MAX_JOBS: usize = 4;

/// Prepared worker inputs; camera history remains on the render thread.
#[derive(Clone, Copy)]
pub(super) struct Input {
    pub world: Uniforms,
    pub skybox: Option<Uniforms>,
    pub skybox_visible: bool,
    pub entities: Entities,
    pub water_side: Option<WaterSide>,
    pub include_water: bool,
    pub kind: ViewKind,
    pub excluded_plane: Option<Plane>,
    pub size: (u32, u32),
}

impl Input {
    fn request<'a>(
        &'a self,
        visibility: &'a super::visibility::UploadedVisibility,
        common: Common<'a>,
    ) -> DrawPlanRequest<'a> {
        DrawPlanRequest {
            world_view: Some(&self.world),
            skybox_view: self.skybox.as_ref(),
            content_id: common.content_id,
            selection: common.selection,
            camera_position: self.world.camera_position,
            map_skybox_visible: self.skybox_visible,
            map_skybox_content_present: common.skybox_present,
            map_skybox_camera_position: self.skybox.map(|uniforms| uniforms.camera_position),
            visibility_plan: visibility.plan.as_ref(),
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct Common<'a> {
    content_id: u64,
    selection: DrawPlanSelection<'a>,
    skybox_present: bool,
}

impl<'a> Common<'a> {
    pub fn new(primitive: &'a ModelPrimitive, upload: &ViewerScene) -> Self {
        Self {
            content_id: primitive.content_id,
            selection: DrawPlanSelection {
                skin_remap: &primitive.skin_remap,
                bodygroup_choices: &primitive.bodygroup_choices,
            },
            skybox_present: upload.assets.has_map_skybox_content(),
        }
    }
}

pub(super) fn prepare_view(
    view: &mut SceneView,
    input: &Input,
    upload: &ViewerScene,
    common: Common<'_>,
    parallel: bool,
) {
    let _ = parallel;
    view.entities = input.entities;
    let plans = view.plan_cache.prepare(
        upload,
        input.request(&view.visibility, common),
        view.visibility.revision,
        input.entities,
    );
    view.plans = Some(plans);
    super::transparency::prepare(
        view,
        upload,
        &input.world,
        input.skybox.as_ref(),
        input.water_side,
        input.include_water,
    );
}

struct Inputs {
    reflection: Option<Input>,
    refraction: Option<Input>,
}

struct Job<'a> {
    view: &'a mut SceneView,
    input: &'a Input,
}

pub(super) fn prepare(
    surfaces: &mut [PlanarView],
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    resources: &RenderResources,
    primitive: &ModelPrimitive,
    upload: &ViewerScene,
    inputs: OpticalInputs<'_>,
) {
    let task_count: usize = surfaces
        .iter()
        .map(|surface| {
            let source = &upload.assets.reflectors[surface.surface_index];
            let shader = &primitive.preview.render_scene().materials[source.material_slot].shader;
            let (reflect, refract) = super::reflections::optical_passes(shader);
            usize::from(reflect) + usize::from(refract)
        })
        .sum();
    let executor = primitive
        .cpu_executor
        .as_ref()
        .filter(|executor| executor.thread_count() > 1 && task_count > 1);
    let Some(executor) = executor else {
        for surface in surfaces {
            surface.prepare_optics(device, queue, resources, primitive, upload, inputs);
            prepare_bindings(surface, device, resources);
        }
        return;
    };
    let planned: Vec<_> = surfaces
        .iter_mut()
        .map(|surface| {
            let source = &upload.assets.reflectors[surface.surface_index];
            let shader = &primitive.preview.render_scene().materials[source.material_slot].shader;
            let (reflect, refract) = super::reflections::optical_passes(shader);
            let reflection = if reflect {
                surface.reflection.get_or_insert_with(|| {
                    SceneView::new(device, resources, inputs.lighting, inputs.surface_marks)
                });
                surface.prepare_reflection_input(device, queue, primitive, upload, inputs.size)
            } else {
                surface.reflection = None;
                None
            };
            let refraction =
                if refract {
                    Some(surface.prepare_refraction_input(
                        device, queue, resources, primitive, upload, inputs,
                    ))
                } else {
                    surface.refraction = None;
                    None
                };
            Inputs {
                reflection,
                refraction,
            }
        })
        .collect();
    let mut jobs = Vec::with_capacity(task_count);
    for (surface, input) in surfaces.iter_mut().zip(&planned) {
        if let Some((view, input)) = surface.reflection.as_mut().zip(input.reflection.as_ref()) {
            jobs.push(Job { view, input });
        }
        if let Some((view, input)) = surface.refraction.as_mut().zip(input.refraction.as_ref()) {
            jobs.push(Job { view, input });
        }
    }
    let common = Common::new(primitive, upload);
    // Reuse PlanCache's predicate so cached views stay on the render thread.
    jobs.retain_mut(|job| {
        let cached = job.view.plan_cache.is_cached(
            upload,
            job.input.request(&job.view.visibility, common),
            job.view.visibility.revision,
            job.input.entities,
        );
        if cached {
            prepare_view(job.view, job.input, upload, common, false);
        }
        !cached
    });
    // Workers prepare each view's world and sky plans after visibility, before animation advances.
    if jobs.len() <= 1 {
        for job in &mut jobs {
            prepare_view(job.view, job.input, upload, common, false);
        }
    } else {
        execute(&mut jobs, executor, |job, parallel| {
            prepare_view(job.view, job.input, upload, common, parallel);
        });
    }

    drop(jobs);
    for (surface, input) in surfaces.iter_mut().zip(&planned) {
        if let Some(input) = &input.reflection {
            surface.finish_reflection(device, queue, resources, primitive, upload, input);
        }
        if let Some(input) = &input.refraction {
            surface.finish_refraction(device, queue, resources, primitive, upload, input);
        }
        prepare_bindings(surface, device, resources);
    }
}

fn execute<T: Send>(jobs: &mut [T], executor: &CpuExecutor, work: impl Fn(&mut T, bool) + Sync) {
    let maximum_jobs = executor.thread_count().min(MAX_JOBS);
    for wave in jobs.chunks_mut(maximum_jobs) {
        if wave.len() == 1 {
            work(&mut wave[0], false);
        } else {
            executor.rayon_pool().scope(|scope| {
                for job in wave {
                    let work = &work;
                    scope.spawn(move |_| work(job, true));
                }
            });
        }
    }
}

fn prepare_bindings(surface: &mut PlanarView, device: &wgpu::Device, resources: &RenderResources) {
    let refracted = surface
        .refraction
        .as_ref()
        .or(surface.reflection.as_ref())
        .and_then(|view| view.targets.as_ref())
        .expect("active planar input");
    let reflected = surface
        .reflection
        .as_ref()
        .and_then(|view| view.targets.as_ref())
        .unwrap_or(refracted);
    WaterBindings::ensure(
        &mut surface.water_bind_group,
        device,
        resources,
        WaterBindingInputs {
            uniforms: &surface.surface_uniforms.buffer,
            refraction: &refracted.resolve_view,
            depth: &refracted.depth,
            reflection: &reflected.resolve_view,
        },
    );
}

#[cfg(test)]
mod tests;
