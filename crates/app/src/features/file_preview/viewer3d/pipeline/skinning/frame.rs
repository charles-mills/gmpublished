use super::super::{UploadedModel, geometry::ViewerMesh, wgpu};
use super::gpu::{Dispatch, Placement, Resources};
use crate::media::preview_model::ModelAnimationPlaybackError;
use gmpublished_domain::math::Vec3;
use std::collections::{BTreeMap, BTreeSet};
#[cfg(feature = "map-capture")]
use std::sync::atomic::AtomicU64;
use std::sync::atomic::{AtomicBool, Ordering};
use vformats::mdl::{ModelAnimationError, ModelPose};

#[derive(Clone, Copy, Debug)]
pub(in crate::features::file_preview::viewer3d) struct Phase {
    pub sequence: Option<usize>,
    pub cycle: f32,
}
pub(in crate::features::file_preview::viewer3d) type PhaseOverrides = BTreeMap<usize, Phase>;

pub(in super::super) struct Request<'a> {
    pub demanded: &'a BTreeSet<usize>,
    pub time: f32,
    pub overrides: Option<&'a PhaseOverrides>,
}

#[derive(Debug, thiserror::Error)]
pub(in super::super) enum Error {
    #[error("animation phase override has no instance {0}")]
    InvalidOverride(usize),
    #[error(
        "animation instance {instance}, mesh {mesh}, effective material {material}: animated planar surfaces are unsupported"
    )]
    UnsupportedPlanarMaterial {
        instance: usize,
        mesh: usize,
        material: usize,
    },
    #[error("animation instance {instance}: {error}")]
    Playback {
        instance: usize,
        #[source]
        error: ModelAnimationPlaybackError,
    },
    #[error("animation instance {instance}: {error}")]
    Pose {
        instance: usize,
        #[source]
        error: ModelAnimationError,
    },
}

#[derive(Debug)]
pub(in super::super) struct Mesh {
    pub buffer: wgpu::Buffer,
    initialize: AtomicBool,
    pub centroid: Vec3,
    pub transparent_parts: Vec<super::super::transparency::Part>,
}

impl Mesh {
    fn new(device: &wgpu::Device, source: &super::super::UploadedMesh) -> Self {
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("file_preview.animation.vertices"),
            size: source.vertices().size(),
            usage: wgpu::BufferUsages::VERTEX
                | wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            buffer,
            initialize: AtomicBool::new(true),
            centroid: source.centroid(),
            transparent_parts: source.transparent_parts.clone(),
        }
    }
}

#[derive(Debug, Default)]
struct Instance {
    pose: ModelPose,
    phase: Option<(usize, u32)>,
    palette: Option<wgpu::Buffer>,
    dispatches: Vec<Dispatch>,
    dirty: AtomicBool,
}

#[derive(Debug, Default)]
pub(in super::super) struct Frame {
    instances: Vec<Instance>,
    pub lighting_origin_updates: BTreeMap<usize, Vec3>,
    revision: u64,
    active: Vec<usize>,
    #[cfg(feature = "map-capture")]
    evaluations: u64,
    #[cfg(feature = "map-capture")]
    palette_upload_bytes: u64,
    #[cfg(feature = "map-capture")]
    encoded_dispatches: AtomicU64,
    #[cfg(feature = "map-capture")]
    encoded_vertices: AtomicU64,
    #[cfg(feature = "map-capture")]
    output_initializations: AtomicU64,
}

impl Frame {
    pub fn new(count: usize) -> Self {
        Self {
            instances: (0..count).map(|_| Instance::default()).collect(),
            ..Self::default()
        }
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        resources: &Resources,
        assets: &UploadedModel,
        meshes: &mut [ViewerMesh],
        request: &Request<'_>,
    ) -> Result<(), Error> {
        let Request {
            demanded,
            time,
            overrides,
        } = *request;
        let sources = assets.source.model_animations();
        self.lighting_origin_updates.clear();
        if let Some(overrides) = overrides {
            for (&index, phase) in overrides {
                let source = sources.get(index).ok_or(Error::InvalidOverride(index))?;
                if !phase.cycle.is_finite() || !(0.0..=1.0).contains(&phase.cycle) {
                    return Err(Error::Pose {
                        instance: index,
                        error: ModelAnimationError::InvalidCycle,
                    });
                }
                if let Some(sequence) = phase.sequence {
                    source
                        .animation
                        .duration(sequence)
                        .map_err(|error| Error::Pose {
                            instance: index,
                            error,
                        })?;
                }
            }
        }
        self.active.clear();
        self.active.extend(demanded);
        for &index in demanded {
            let instance = &sources[index];
            let state = &mut self.instances[index];
            let phase = overrides.and_then(|overrides| overrides.get(&index));
            let sequence = phase
                .and_then(|phase| phase.sequence)
                .unwrap_or(instance.sequence);
            let cycle = phase.map_or_else(
                || {
                    instance
                        .playback
                        .cycle(&instance.animation, sequence, time)
                        .map_err(|error| Error::Playback {
                            instance: index,
                            error,
                        })
                },
                |phase| Ok(phase.cycle),
            )?;
            if state.phase == Some((sequence, cycle.to_bits())) {
                continue;
            }
            instance
                .animation
                .evaluate(sequence, cycle, &mut state.pose)
                .map_err(|error| Error::Pose {
                    instance: index,
                    error,
                })?;
            let palette = state.palette.get_or_insert_with(|| {
                device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("file_preview.animation.palette"),
                    size: state.pose.skin_matrices.len() as u64 * 64,
                    usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                })
            });
            queue.write_buffer(palette, 0, bytemuck::cast_slice(&state.pose.skin_matrices));
            #[cfg(feature = "map-capture")]
            {
                self.evaluations += 1;
                self.palette_upload_bytes += state.pose.skin_matrices.len() as u64 * 64;
            }

            let placement = Placement {
                origin: instance.origin,
                angles: instance.angles,
                scale: instance.scale,
            };
            if state.dispatches.is_empty() {
                for binding in &assets.animations.instances[index].bindings {
                    let mesh = &mut meshes[binding.mesh];
                    if mesh.animation.is_none() {
                        mesh.animation = Some(Mesh::new(device, mesh));
                    }
                    let output = mesh.animation.as_ref().expect("initialized animated mesh");
                    state.dispatches.extend(resources.dispatches(
                        device,
                        &binding.source,
                        &output.buffer,
                        palette,
                        placement,
                        binding.vertex_start,
                    ));
                }
            }
            for group in &assets.animations.instances[index].meshes {
                let mesh_index = group.mesh;
                let mesh = &mut meshes[mesh_index];
                let source_mesh = source_mesh(assets, mesh);
                let output = mesh.animation.as_mut().expect("prepared animated mesh");
                let mut sum = group.static_sum;
                let mut count = group.static_count;
                for &selected in &group.bindings {
                    let binding = &assets.animations.instances[index].bindings[selected];
                    let vertices = binding.source.centroid.count();
                    sum += binding
                        .source
                        .centroid
                        .sum(&state.pose.skin_matrices)
                        .rotate_source(instance.angles)
                        * instance.scale
                        + instance.origin * vertices as f32;
                    count += vertices;
                }
                output.centroid = sum / count as f32;
                for part in &mut output.transparent_parts {
                    let mut center = Vec3::ZERO;
                    for &vertex in
                        &source_mesh.indices[part.indices.start as usize..part.indices.end as usize]
                    {
                        let binding =
                            assets.animations.instances[index]
                                .bindings
                                .iter()
                                .find(|binding| {
                                    binding.mesh == mesh_index
                                        && vertex >= binding.vertex_start
                                        && (vertex - binding.vertex_start)
                                            < binding.source.vertices.len() as u32
                                });
                        center += binding.map_or_else(
                            || source_mesh.vertices[vertex as usize].position,
                            |binding| {
                                super::pose::position(
                                    &binding.source.vertices
                                        [(vertex - binding.vertex_start) as usize],
                                    &state.pose.skin_matrices,
                                )
                                .rotate_source(instance.angles)
                                    * instance.scale
                                    + instance.origin
                            },
                        );
                    }
                    part.centre = center / (part.indices.end - part.indices.start) as f32;
                }
            }
            if let Some(context) = instance.lighting_context {
                let valid_origin = assets
                    .map()
                    .and_then(|map| map.model_lighting.get(context.index()))
                    .is_some_and(|context| context.sample_point.is_ok());
                if valid_origin {
                    let position = instance.lighting_origin_override.unwrap_or_else(|| {
                        let local = instance
                            .illumination_attachment_index
                            .and_then(|attachment| state.pose.attachments.get(attachment))
                            .map_or(instance.illumination_position, |attachment| {
                                Vec3::from(attachment.model.map(|row| row[3]))
                            });
                        local.rotate_source(instance.angles) + instance.origin
                    });
                    self.lighting_origin_updates
                        .insert(context.index(), position);
                }
            }
            state.phase = Some((sequence, cycle.to_bits()));
            state.dirty.store(true, Ordering::Relaxed);
            self.revision = self.revision.wrapping_add(1);
        }
        Ok(())
    }

    #[cfg(feature = "map-capture")]
    pub fn diagnostics(&self, assets: &UploadedModel, meshes: &[ViewerMesh]) -> serde_json::Value {
        let sources = assets.source.model_animations();
        let active = self.active.iter().map(|&index| {
            let state = &self.instances[index];
            let source = &sources[index];
            let (sequence, cycle) = state.phase.expect("prepared active animation phase");
            serde_json::json!({
                "instance": index,
                "model": source.model_path,
                "entity_index": source.entity_index,
                "hammer_id": source.hammer_id,
                "origin": source.origin.to_array(),
                "sequence_index": sequence,
                "sequence": source.animation.sequences()[sequence].label,
                "cycle": f32::from_bits(cycle),
                "model_bounds": {"min":state.pose.bounds_min,"max":state.pose.bounds_max},
                "bone_to_model":state.pose.bone_to_model,
                "attachments":state.pose.attachments.iter().map(|attachment|attachment.model).collect::<Vec<_>>(),
            })
        }).collect::<Vec<_>>();
        let outputs = assets
            .animations
            .mesh_instances
            .keys()
            .filter_map(|&index| meshes[index].animation.as_ref())
            .collect::<Vec<_>>();
        let shared_sources = assets
            .animations
            .instances
            .iter()
            .flat_map(|instance| &instance.bindings)
            .map(|binding| std::sync::Arc::as_ptr(&binding.source) as usize)
            .collect::<BTreeSet<_>>();
        let palette_bytes = self
            .instances
            .iter()
            .filter_map(|instance| instance.palette.as_ref())
            .map(wgpu::Buffer::size)
            .sum::<u64>();
        serde_json::json!({
            "active_instances":active,
            "instance_count":sources.len(),
            "shared_bind_sources":shared_sources.len(),
            "output_meshes":outputs.len(),
            "output_bytes":outputs.iter().map(|output|output.buffer.size()).sum::<u64>(),
            "palette_bytes":palette_bytes,
            "evaluations":self.evaluations,
            "palette_upload_bytes":self.palette_upload_bytes,
            "encoded_dispatches":self.encoded_dispatches.load(Ordering::Relaxed),
            "encoded_vertices":self.encoded_vertices.load(Ordering::Relaxed),
            "output_initializations":self.output_initializations.load(Ordering::Relaxed),
            "pose_revision":self.revision,
        })
    }

    pub fn encode(
        &self,
        resources: &Resources,
        assets: &UploadedModel,
        meshes: &[ViewerMesh],
        encoder: &mut wgpu::CommandEncoder,
    ) {
        for &index in &self.active {
            let state = &self.instances[index];
            if !state.dirty.swap(false, Ordering::Relaxed) {
                continue;
            }
            for binding in &assets.animations.instances[index].bindings {
                let mesh = &meshes[binding.mesh];
                let output = mesh.animation.as_ref().expect("prepared animated mesh");
                if output.initialize.swap(false, Ordering::Relaxed) {
                    #[cfg(feature = "map-capture")]
                    self.output_initializations.fetch_add(1, Ordering::Relaxed);
                    encoder.copy_buffer_to_buffer(
                        assets.meshes[binding.mesh].vertices(),
                        0,
                        &output.buffer,
                        0,
                        output.buffer.size(),
                    );
                }
            }
            resources.encode(encoder, &state.dispatches);
            #[cfg(feature = "map-capture")]
            {
                self.encoded_dispatches
                    .fetch_add(state.dispatches.len() as u64, Ordering::Relaxed);
                self.encoded_vertices.fetch_add(
                    assets.animations.instances[index]
                        .bindings
                        .iter()
                        .map(|binding| binding.source.vertices.len() as u64)
                        .sum::<u64>(),
                    Ordering::Relaxed,
                );
            }
        }
    }
}

fn source_mesh<'a>(
    assets: &'a UploadedModel,
    mesh: &ViewerMesh,
) -> &'a crate::media::preview_model::MeshData {
    if mesh.map_skybox {
        &assets
            .map()
            .expect("skybox mesh belongs to a map")
            .map_skybox_meshes[mesh.scene_mesh_index]
    } else {
        &assets.source.render_scene().meshes[mesh.scene_mesh_index]
    }
}
