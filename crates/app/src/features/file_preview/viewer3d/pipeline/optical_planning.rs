//! Only per-view CPU plan/queue work can run on the explicitly owned CPU pool.
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

/// Values computed by the serial GPU/visibility prefix; never camera history.
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

#[cfg(feature = "map-capture")]
#[derive(Clone, Copy, Debug, serde::Serialize)]
pub(super) struct Diagnostics {
    parallel: bool,
    // Worker duration is separate from the owner-thread join wall scope.
    cpu_job_ms: f64,
    // Fixed-size evidence survives later owner-thread cache hits.
    parallel_preparations: u64,
}

pub(super) fn prepare_view(
    view: &mut SceneView,
    input: &Input,
    upload: &ViewerScene,
    common: Common<'_>,
    parallel: bool,
) {
    #[cfg(feature = "gui-benchmark")]
    let _cpu_plan_queue = iced_wgpu::profiling::begin_cpu_scope(match input.kind {
        ViewKind::Reflection => "map.optics.reflection.cpu_plan_queue",
        ViewKind::Refraction => "map.optics.refraction.cpu_plan_queue",
        ViewKind::Main => unreachable!("optical planning excludes the main view"),
    });
    #[cfg(feature = "map-capture")]
    let started = std::time::Instant::now();
    #[cfg(not(feature = "map-capture"))]
    let _ = parallel;
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
    #[cfg(feature = "map-capture")]
    {
        let parallel_preparations = view
            .optical_cpu_planning
            .map_or(0, |previous| previous.parallel_preparations)
            .saturating_add(u64::from(parallel));
        view.optical_cpu_planning = Some(Diagnostics {
            parallel,
            cpu_job_ms: started.elapsed().as_secs_f64() * 1000.0,
            parallel_preparations,
        });
    }
}

fn enabled() -> bool {
    #[cfg(any(feature = "map-capture", feature = "gui-benchmark"))]
    {
        static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *ENABLED.get_or_init(|| {
            std::env::var_os("GMPUBLISHED_PARALLEL_OPTICAL_PLANNING").as_deref()
                != Some(std::ffi::OsStr::new("0"))
        })
    }
    #[cfg(not(any(feature = "map-capture", feature = "gui-benchmark")))]
    true
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
        .filter(|executor| executor.thread_count() > 1 && task_count > 1 && enabled());
    let Some(executor) = executor else {
        // Preserve reflection->refraction->bindings for each original surface.
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
    #[cfg(feature = "gui-benchmark")]
    let wall = iced_wgpu::profiling::begin_cpu_scope("map.optics.cpu_plan_queue_wall");
    // Cached-plan views stay on the owner thread; their queues may still rebuild.
    // Both the probe and prepare use PlanCache's existing exact cache predicate.
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
    // Upload/visibility work is complete, and shared animation has not advanced.
    // Each job retains world->sky sequencing inside its original view cache.
    if jobs.len() <= 1 {
        for job in &mut jobs {
            prepare_view(job.view, job.input, upload, common, false);
        }
    } else {
        execute(&mut jobs, executor, |job, parallel| {
            prepare_view(job.view, job.input, upload, common, parallel);
        });
    }
    #[cfg(feature = "gui-benchmark")]
    drop(wall);
    drop(jobs);
    // Commit in the original surface, reflection, refraction order. No worker
    // owns resource admission, GPU calls, sun state, or a shared scene mutation.
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
            // ThreadPool::scope uses only this owned pool and joins before the
            // next bounded wave or any owner-thread GPU/animation work proceeds.
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
