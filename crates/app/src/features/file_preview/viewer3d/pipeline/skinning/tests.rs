use super::super::wgpu;
use super::gpu::{Placement, Resources, SourceCache};
use crate::media::preview_model::ModelVertex;
use gmpublished_domain::math::Vec3;
use std::sync::Arc;

#[test]
fn skin_storage_layout_matches_the_render_vertex_contract() {
    use iced::wgpu::naga::{self, TypeInner, valid};
    let module = naga::front::wgsl::parse_str(include_str!("skin.wgsl")).unwrap();
    valid::Validator::new(valid::ValidationFlags::all(), valid::Capabilities::all())
        .validate(&module)
        .unwrap();
    let ty = module
        .types
        .iter()
        .find(|(_, ty)| ty.name.as_deref() == Some("RenderVertex"))
        .unwrap()
        .1;
    let TypeInner::Struct { members, span } = &ty.inner else {
        panic!("render vertex storage structure")
    };
    assert_eq!(*span as usize, std::mem::size_of::<ModelVertex>());
    let offsets = [
        ("position", std::mem::offset_of!(ModelVertex, position)),
        ("normal", std::mem::offset_of!(ModelVertex, normal)),
        ("uv", std::mem::offset_of!(ModelVertex, uv)),
        (
            "lightmap_uv",
            std::mem::offset_of!(ModelVertex, lightmap_uv),
        ),
        (
            "lightmap_bump_step",
            std::mem::offset_of!(ModelVertex, lightmap_bump_step),
        ),
        ("color", std::mem::offset_of!(ModelVertex, color)),
        (
            "blend_alpha",
            std::mem::offset_of!(ModelVertex, blend_alpha),
        ),
        ("opacity", std::mem::offset_of!(ModelVertex, opacity)),
        (
            "illumination",
            std::mem::offset_of!(ModelVertex, illumination),
        ),
        (
            "lighting_mode",
            std::mem::offset_of!(ModelVertex, lighting_mode),
        ),
        ("tangent_s", std::mem::offset_of!(ModelVertex, tangent_s)),
        ("tangent_t", std::mem::offset_of!(ModelVertex, tangent_t)),
        (
            "tangent_sign",
            std::mem::offset_of!(ModelVertex, tangent_sign),
        ),
        (
            "lighting_context",
            std::mem::offset_of!(ModelVertex, lighting_context),
        ),
        (
            "surface_mark",
            std::mem::offset_of!(ModelVertex, surface_mark),
        ),
        (
            "lightmap_page",
            std::mem::offset_of!(ModelVertex, lightmap_page),
        ),
    ];
    assert_eq!(members.len(), offsets.len());
    for (member, (name, offset)) in members.iter().zip(offsets) {
        assert_eq!(member.name.as_deref(), Some(name));
        assert_eq!(member.offset as usize, offset, "{name}");
    }
}

#[test]
#[ignore = "requires exclusive Vulkan GPU"]
fn gpu_skinning_preserves_weights_vectors_placement_and_untouched_vertex_state() {
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
        backends: wgpu::Backends::VULKAN,
        ..Default::default()
    });
    let adapter = iced::futures::executor::block_on(
        instance.request_adapter(&wgpu::RequestAdapterOptions::default()),
    )
    .unwrap();
    let (device, queue) =
        iced::futures::executor::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            required_limits: wgpu::Limits {
                max_storage_buffer_binding_size: 4096,
                ..Default::default()
            },
            ..Default::default()
        }))
        .unwrap();
    let source_vertex = vformats::mdl::ModelVertex {
        position: [0.25, -0.5, 0.75],
        normal: [0.25, 0.5, 0.75],
        tangent_s: [0.75, -0.5, 0.25],
        tangent_t: [0.0; 3],
        tangent_sign: -1.0,
        uv: [0.125, 0.875],
    };
    let sources: Arc<[_]> = (0..129)
        .map(|index| [(0.25, 0.75, 2), (0.25, 0.5, 2), (0.0, 0.0, 0)][index % 3])
        .map(|(a, b, count)| vformats::mdl::ModelSkinVertex {
            vertex: source_vertex,
            bones: [0, 1, 0],
            weights: [a, b, 0.0],
            bone_count: count,
        })
        .collect::<Vec<_>>()
        .into();
    let mut initial = vec![ModelVertex::from(&source_vertex); sources.len() + 2];
    for (index, vertex) in initial.iter_mut().enumerate() {
        vertex.color = Vec3::new(0.1, 0.2, index as f32);
        vertex.lightmap_page = 7;
        vertex.lighting_context = 3;
        vertex.lighting_mode = 4096;
        vertex.surface_mark = 2;
        vertex.opacity = 0.75;
        vertex.illumination = Vec3::new(0.33, 0.44, 0.55);
    }
    let output = initialized_buffer(
        &device,
        "skin output",
        bytemuck::cast_slice(&initial),
        wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_SRC,
    );
    let palette = [
        [
            [1., 0., 0., 0.],
            [0., 1., 0., 0.],
            [0., 0., 1., 0.],
            [2., 0., 0., 1.],
        ],
        [
            [0., 1., 0., 0.],
            [-1., 0., 0., 0.],
            [0., 0., 1., 0.],
            [0., 3., 0., 1.],
        ],
    ];
    let palette_buffer = initialized_buffer(
        &device,
        "skin palette",
        bytemuck::cast_slice(&palette),
        wgpu::BufferUsages::STORAGE,
    );
    let placement = Placement {
        origin: Vec3::new(4., 5., 6.),
        angles: Vec3::new(0., 90., 0.),
        scale: 2.,
    };
    let mut cache = SourceCache::default();
    let shared = cache.upload(&device, &queue, &sources);
    assert!(Arc::ptr_eq(
        &shared,
        &cache.upload(&device, &queue, &sources)
    ));
    let resources = Resources::new(&device);
    let dispatches = resources.dispatches(&device, &shared, &output, &palette_buffer, placement, 1);
    assert!(
        dispatches.len() > 1,
        "constrained storage limit must exercise chunked windows"
    );
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("skin readback"),
        size: output.size(),
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    resources.encode(&mut encoder, &dispatches);
    encoder.copy_buffer_to_buffer(&output, 0, &readback, 0, output.size());
    queue.submit([encoder.finish()]);
    readback
        .slice(..)
        .map_async(wgpu::MapMode::Read, |result| result.unwrap());
    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    let bytes = readback.slice(..).get_mapped_range();
    let actual: &[ModelVertex] = bytemuck::cast_slice(&bytes);
    assert_eq!(actual[0], initial[0]);
    assert_eq!(actual.last(), initial.last());
    for (index, source) in sources.iter().enumerate() {
        let skin = |value: [f32; 3], point: bool| {
            if source.bone_count == 0 {
                return Vec3::from(value);
            }
            let mut result = Vec3::ZERO;
            for slot in 0..source.bone_count as usize {
                let matrix = palette[source.bones[slot] as usize];
                result += Vec3::from(std::array::from_fn(|row| {
                    matrix[0][row] * value[0]
                        + matrix[1][row] * value[1]
                        + matrix[2][row] * value[2]
                        + if point { matrix[3][row] } else { 0.0 }
                })) * source.weights[slot];
            }
            result
        };
        let mut expected = initial[index + 1];
        expected.position = skin(source.vertex.position, true).rotate_source(placement.angles)
            * placement.scale
            + placement.origin;
        expected.normal = skin(source.vertex.normal, false).rotate_source(placement.angles);
        expected.tangent_s = skin(source.vertex.tangent_s, false).rotate_source(placement.angles);
        expected.tangent_t = (skin(source.vertex.normal, false)
            .cross(skin(source.vertex.tangent_s, false))
            * source.vertex.tangent_sign)
            .rotate_source(placement.angles);
        expected.tangent_sign = source.vertex.tangent_sign;
        for (label, a, b) in [
            ("position", actual[index + 1].position, expected.position),
            ("normal", actual[index + 1].normal, expected.normal),
            ("tangent_s", actual[index + 1].tangent_s, expected.tangent_s),
            ("tangent_t", actual[index + 1].tangent_t, expected.tangent_t),
        ] {
            assert!(
                (0..3).all(|axis| (a[axis] - b[axis]).abs() < 0.00001),
                "{label}: {a:?} versus {b:?}"
            );
        }
        expected.position = actual[index + 1].position;
        expected.normal = actual[index + 1].normal;
        expected.tangent_s = actual[index + 1].tangent_s;
        expected.tangent_t = actual[index + 1].tangent_t;
        assert_eq!(
            actual[index + 1],
            expected,
            "skinning altered non-geometric vertex state"
        );
    }
}

fn initialized_buffer(
    device: &wgpu::Device,
    label: &str,
    bytes: &[u8],
    usage: wgpu::BufferUsages,
) -> wgpu::Buffer {
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: bytes.len() as u64,
        usage,
        mapped_at_creation: true,
    });
    buffer
        .slice(..)
        .get_mapped_range_mut()
        .copy_from_slice(bytes);
    buffer.unmap();
    buffer
}

#[test]
fn animated_centroid_reduction_preserves_original_weight_sums_and_stored_fallback() {
    let vertex = vformats::mdl::ModelVertex {
        position: [2., 4., 6.],
        normal: [0., 0., 1.],
        tangent_s: [1., 0., 0.],
        tangent_t: [0., 1., 0.],
        tangent_sign: 1.,
        uv: [0.; 2],
    };
    let vertices = [
        vformats::mdl::ModelSkinVertex {
            vertex,
            bones: [0, 1, 0],
            weights: [0.25, 0.5, 0.],
            bone_count: 2,
        },
        vformats::mdl::ModelSkinVertex {
            vertex,
            bones: [0, 0, 0],
            weights: [0.; 3],
            bone_count: 0,
        },
    ];
    let palette = [
        [
            [1., 0., 0., 0.],
            [0., 1., 0., 0.],
            [0., 0., 1., 0.],
            [8., 0., 0., 1.],
        ],
        [
            [0., 1., 0., 0.],
            [-1., 0., 0., 0.],
            [0., 0., 1., 0.],
            [0., 6., 0., 1.],
        ],
    ];
    let reduced = super::pose::Centroid::new(&vertices);
    assert_eq!(reduced.count(), 2);
    assert_eq!(reduced.sum(&palette), Vec3::new(2.5, 9., 10.5));
    assert_eq!(
        super::pose::position(&vertices[0], &palette),
        Vec3::new(0.5, 5., 4.5)
    );
    assert_eq!(
        super::pose::position(&vertices[1], &palette),
        Vec3::new(2., 4., 6.)
    );
}

#[test]
fn skinning_admission_requires_a_window_for_every_unaligned_vertex_start() {
    let mut limits = wgpu::Limits {
        min_storage_buffer_offset_alignment: 256,
        max_storage_buffer_binding_size: 4096,
        ..Default::default()
    };
    assert!(super::gpu::supports_storage_windows(&limits));
    limits.max_storage_buffer_binding_size = 1024;
    assert!(!super::gpu::supports_storage_windows(&limits));
    limits.max_storage_buffer_binding_size = 4096;
    limits.max_compute_workgroups_per_dimension = 0;
    assert!(!super::gpu::supports_storage_windows(&limits));
}
