use super::super::{PreviewScene, UploadedMesh, wgpu};
use super::gpu::{Source, SourceCache};
use gmpublished_domain::scene::map::MapReceiverMesh;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub(in super::super) struct PreparationIssue {
    pub instance: usize,
    pub binding: Option<usize>,
    pub reason: String,
}

pub(in super::super) fn validate(
    source: &PreviewScene,
    limits: &wgpu::Limits,
) -> Vec<PreparationIssue> {
    let mut issues = Vec::new();
    let mut occupied = HashMap::<MapReceiverMesh, Vec<std::ops::Range<u64>>>::new();
    let mut owners = HashMap::new();
    for (index, instance) in source.model_animations().iter().enumerate() {
        let mut note = |binding, reason: String| {
            issues.push(PreparationIssue {
                instance: index,
                binding,
                reason,
            });
        };
        if !instance.origin.is_finite()
            || !instance.angles.is_finite()
            || !instance.scale.is_finite()
            || instance.scale <= 0.0
        {
            note(
                None,
                "animation placement must have finite transform and positive uniform scale"
                    .to_owned(),
            );
        }
        let bones = instance.animation.bone_count();
        let palette_bytes = (bones as u64).checked_mul(64);
        if bones == 0
            || palette_bytes.is_none_or(|bytes| {
                bytes > u64::from(limits.max_storage_buffer_binding_size)
                    || bytes > limits.max_buffer_size
            })
        {
            note(
                None,
                format!("animation palette has {bones} bones and exceeds storage binding limits"),
            );
        }
        for (binding_index, binding) in instance.bindings.iter().enumerate() {
            let mesh = match binding.receiver_mesh {
                MapReceiverMesh::World(index) => source.render_scene().meshes.get(index),
                MapReceiverMesh::Skybox(index) => source
                    .map()
                    .and_then(|map| map.map_skybox_meshes.get(index)),
                MapReceiverMesh::Door { .. } => None,
            };
            let start = u64::from(binding.vertex_start);
            let end = start.checked_add(binding.vertices.len() as u64);
            if binding.vertices.is_empty()
                || mesh.is_none_or(|mesh| mesh.indices.is_empty())
                || end.is_none_or(|end| end > mesh.map_or(0, |mesh| mesh.vertices.len() as u64))
            {
                note(
                    Some(binding_index),
                    "animation vertex range is empty or outside its scene mesh".to_owned(),
                );
                continue;
            }
            let end = end.expect("checked animation range");
            if owners
                .insert(binding.receiver_mesh, index)
                .is_some_and(|owner| owner != index)
            {
                note(
                    Some(binding_index),
                    "animated scene mesh is shared by multiple placements".to_owned(),
                );
            }
            let ranges = occupied.entry(binding.receiver_mesh).or_default();
            if ranges
                .iter()
                .any(|range| range.start < end && start < range.end)
            {
                note(
                    Some(binding_index),
                    "animation vertex ranges overlap".to_owned(),
                );
            }
            ranges.push(start..end);
            if (binding.vertices.len() as u64)
                .checked_mul(super::gpu::SOURCE_VERTEX_BYTES)
                .is_none_or(|bytes| bytes > limits.max_buffer_size)
            {
                note(
                    Some(binding_index),
                    "animation source exceeds GPU buffer size".to_owned(),
                );
            }
            if !super::gpu::supports_storage_windows(limits) {
                note(
                    Some(binding_index),
                    "device cannot bind an animation storage window".to_owned(),
                );
            }
            if let Some(vertex) = binding.vertices.iter().position(|vertex| {
                vertex.bone_count > 3
                    || !vertex
                        .vertex
                        .position
                        .iter()
                        .chain(&vertex.vertex.normal)
                        .chain(&vertex.vertex.tangent_s)
                        .all(|value| value.is_finite())
                    || !vertex.vertex.tangent_sign.is_finite()
                    || (vertex.bone_count != 0
                        && (vertex.weights[..vertex.bone_count.min(3) as usize]
                            .iter()
                            .any(|weight| !weight.is_finite() || *weight < 0.0)
                            || vertex.weights[..vertex.bone_count.min(3) as usize]
                                .iter()
                                .sum::<f32>()
                                <= f32::EPSILON
                            || vertex.bones[..vertex.bone_count.min(3) as usize]
                                .iter()
                                .any(|&bone| bone as usize >= bones)))
            }) {
                note(
                    Some(binding_index),
                    format!("animation vertex {vertex} has invalid bind vectors or influences"),
                );
            }
        }
    }
    issues
}

#[derive(Debug)]
pub(in super::super) struct Binding {
    pub mesh: usize,
    pub vertex_start: u32,
    pub source: Arc<Source>,
}

#[derive(Debug)]
pub(in super::super) struct Instance {
    pub bindings: Vec<Binding>,
    pub meshes: Vec<MeshBinding>,
}

#[derive(Debug)]
pub(in super::super) struct MeshBinding {
    pub mesh: usize,
    pub bindings: Vec<usize>,
    pub static_sum: gmpublished_domain::math::Vec3,
    pub static_count: usize,
}

#[derive(Debug, Default)]
pub(in super::super) struct Assets {
    pub instances: Vec<Instance>,
    pub mesh_instances: HashMap<usize, Vec<usize>>,
}

impl Assets {
    pub fn upload(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        source: &PreviewScene,
        meshes: &[UploadedMesh],
        cache: &mut SourceCache,
    ) -> Self {
        let receivers: HashMap<_, _> = meshes
            .iter()
            .enumerate()
            .map(|(index, mesh)| (mesh.receiver_source(), index))
            .collect();
        let mut mesh_instances = HashMap::<_, Vec<_>>::new();
        let instances = source
            .model_animations()
            .iter()
            .enumerate()
            .map(|(index, instance)| {
                let bindings: Vec<_> = instance
                    .bindings
                    .iter()
                    .map(|binding| {
                        let mesh = receivers[&binding.receiver_mesh];
                        let instances = mesh_instances.entry(mesh).or_default();
                        if !instances.contains(&index) {
                            instances.push(index);
                        }
                        Binding {
                            mesh,
                            vertex_start: binding.vertex_start,
                            source: cache.upload(device, queue, &binding.vertices),
                        }
                    })
                    .collect();
                let mut grouped = std::collections::BTreeMap::<usize, Vec<usize>>::new();
                for (index, binding) in bindings.iter().enumerate() {
                    grouped.entry(binding.mesh).or_default().push(index);
                }
                let meshes = grouped
                    .into_iter()
                    .map(|(mesh, selected)| {
                        let uploaded = &meshes[mesh];
                        let cpu = if uploaded.map_skybox {
                            &source
                                .map()
                                .expect("skybox animation belongs to a map")
                                .map_skybox_meshes[uploaded.scene_mesh_index]
                        } else {
                            &source.render_scene().meshes[uploaded.scene_mesh_index]
                        };
                        let mut static_sum = gmpublished_domain::math::Vec3::ZERO;
                        let mut static_count = 0;
                        for (index, vertex) in cpu.vertices.iter().enumerate() {
                            if !selected.iter().any(|&selected| {
                                let binding = &bindings[selected];
                                index >= binding.vertex_start as usize
                                    && index - (binding.vertex_start as usize)
                                        < binding.source.vertices.len()
                            }) {
                                static_sum += vertex.position;
                                static_count += 1;
                            }
                        }
                        MeshBinding {
                            mesh,
                            bindings: selected,
                            static_sum,
                            static_count,
                        }
                    })
                    .collect();
                Instance { bindings, meshes }
            })
            .collect();
        Self {
            instances,
            mesh_instances,
        }
    }
}
