use super::*;
use std::time::Duration;
use wgpu::util::DeviceExt;

#[test]
#[ignore = "requires an exclusively reserved Vulkan GPU"]
fn distinct_world_sky_and_two_optical_slots_survive_one_submission() {
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
        backends: wgpu::Backends::VULKAN,
        flags: wgpu::InstanceFlags::VALIDATION,
        ..Default::default()
    });
    let adapter = iced::futures::executor::block_on(
        instance.request_adapter(&wgpu::RequestAdapterOptions::default()),
    )
    .expect("transparent MDI slot contract untested: no Vulkan adapter");
    let native = wgpu::Features::MULTI_DRAW_INDIRECT_COUNT;
    // A distinct no-COUNT device must exercise the original direct fallback,
    // even when the same physical adapter supports native MDI.
    let (direct_device, direct_queue) = iced::futures::executor::block_on(
        adapter.request_device(&wgpu::DeviceDescriptor::default()),
    )
    .expect("direct device");
    assert!(!direct_device.features().contains(native));
    let direct = render_slots(&direct_device, &direct_queue, false);
    let expected = expected_pixels();
    assert_eq!(direct.len(), expected.len());
    assert!(
        direct
            .iter()
            .zip(&expected)
            .all(|(a, b)| a.abs_diff(*b) <= 1)
    );
    if adapter.features().contains(native) {
        let (device, queue) =
            iced::futures::executor::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                required_features: native,
                ..Default::default()
            }))
            .expect("native MDI device");
        assert!(device.features().contains(native));
        let independent = render_slots(&device, &queue, false);
        assert_eq!(independent, direct);
        let shared_region = render_slots(&device, &queue, true);
        assert_ne!(shared_region, independent);
        println!(
            "native transparent MDI slot coverage passed on {:?}",
            adapter.get_info()
        );
    } else {
        println!(
            "native MDI unavailable on {:?}; no-COUNT fallback passed",
            adapter.get_info()
        );
    }
}

fn expected_pixels() -> Vec<u8> {
    let colors = [
        [64, 128, 0, 192],
        [0, 64, 128, 192],
        [128, 0, 64, 192],
        [192, 64, 64, 192],
    ];
    (0..16)
        .flat_map(|y| {
            (0..16).flat_map(move |x| {
                let quadrant = usize::from(x >= 8) + if y < 8 { 2 } else { 0 };
                colors[quadrant]
            })
        })
        .collect()
}

fn render_slots(device: &wgpu::Device, queue: &wgpu::Queue, alias_last_slot: bool) -> Vec<u8> {
    device.push_error_scope(wgpu::ErrorFilter::Validation);
    let native = device
        .features()
        .contains(wgpu::Features::MULTI_DRAW_INDIRECT_COUNT);
    assert!(!alias_last_slot || native);
    let mut main = Slots::default();
    let mut reflection = Slots::default();
    let mut refraction = Slots::default();
    let parts: Vec<_> = (0..4_u32)
        .map(|slot| {
            [0, 6]
                .into_iter()
                .map(|offset| {
                    (
                        DrawItem {
                            mesh_index: 0,
                            material_slot: 0,
                        },
                        PageRange {
                            indices: slot * 12 + offset..slot * 12 + offset + 6,
                            page: None,
                        },
                    )
                })
                .collect::<Vec<_>>()
        })
        .collect();
    for (slot, parts) in [
        &mut main.world,
        &mut main.skybox,
        &mut reflection.world,
        &mut refraction.world,
    ]
    .into_iter()
    .zip(&parts)
    {
        if native {
            let operations: Vec<_> = parts
                .iter()
                .map(|(item, part)| Draw {
                    primitive: Primitive::Mesh(*item, Some(part.indices.clone())),
                    blend: RenderMode::Translucent,
                    depth: 0.0,
                })
                .collect();
            slot.build(&operations, |_, range| {
                Some((
                    false,
                    PageRange {
                        indices: range.unwrap().clone(),
                        page: None,
                    },
                ))
            });
            slot.upload_arguments(device, queue);
            assert!(slot.program().is_some());
        } else {
            assert!(slot.program().is_none());
            assert!(slot.buffer.is_none());
            assert!(slot.arguments.is_empty());
        }
    }
    let indices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("transparent MDI slot indices"),
        contents: bytemuck::cast_slice(&(0..48_u32).collect::<Vec<_>>()),
        usage: wgpu::BufferUsages::INDEX,
    });
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("transparent MDI slot shader"),
        source: wgpu::ShaderSource::Wgsl(
            r"
struct Vertex { @builtin(position) position: vec4<f32>, @location(0) color: vec4<f32> }
@vertex fn vs(@builtin(vertex_index) index: u32) -> Vertex {
    let corners = array(vec2(-1.0,-1.0),vec2(0.0,-1.0),vec2(-1.0,0.0),
                        vec2(-1.0,0.0),vec2(0.0,-1.0),vec2(0.0,0.0));
    let colors = array(vec4(1.0,0.0,0.0,0.5),vec4(0.0,1.0,0.0,0.5),
                       vec4(0.0,1.0,0.0,0.5),vec4(0.0,0.0,1.0,0.5),
                       vec4(0.0,0.0,1.0,0.5),vec4(1.0,0.0,0.0,0.5),
                       vec4(1.0,1.0,1.0,0.5),vec4(1.0,0.0,0.0,0.5));
    let quadrant = index / 12u;
    let xy = corners[index % 6u] + vec2(f32(quadrant % 2u),f32(quadrant / 2u));
    return Vertex(vec4(xy,0.0,1.0),colors[index / 6u]);
}
@fragment fn fs(input: Vertex) -> @location(0) vec4<f32> { return input.color; }
"
            .into(),
        ),
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("transparent MDI slot pipeline"),
        layout: None,
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: wgpu::TextureFormat::Rgba8Unorm,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: Default::default(),
        depth_stencil: None,
        multisample: Default::default(),
        multiview: None,
        cache: None,
    });
    let extent = wgpu::Extent3d {
        width: 16,
        height: 16,
        depth_or_array_layers: 1,
    };
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("transparent MDI slot target"),
        size: extent,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let target_view = target.create_view(&Default::default());
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("transparent MDI slot readback"),
        size: 256 * 16,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("four transparent MDI slot programs"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &target_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        pass.set_pipeline(&pipeline);
        pass.set_index_buffer(indices.slice(..), wgpu::IndexFormat::Uint32);
        for (slot, parts) in [
            &main.world,
            &main.skybox,
            &reflection.world,
            &refraction.world,
        ]
        .into_iter()
        .zip(&parts)
        {
            if let Some(program) = slot.program() {
                let buffer = if alias_last_slot {
                    refraction.world.buffer.as_ref().unwrap()
                } else {
                    program.buffer
                };
                assert_eq!(program.runs.len(), 1);
                let run = &program.runs[0];
                pass.multi_draw_indexed_indirect(buffer, run.offset, run.count());
            } else {
                for (_, part) in parts {
                    pass.draw_indexed(part.indices.clone(), 0, 0..1);
                }
            }
        }
    }
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &target,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(256),
                rows_per_image: Some(16),
            },
        },
        extent,
    );
    let submission = queue.submit([encoder.finish()]);
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    readback
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| {
            sender.send(result).unwrap();
        });
    device
        .poll(wgpu::PollType::Wait {
            submission_index: Some(submission),
            timeout: Some(Duration::from_secs(30)),
        })
        .unwrap();
    receiver
        .recv_timeout(Duration::from_secs(1))
        .unwrap()
        .unwrap();
    let mapped = readback.slice(..).get_mapped_range();
    let pixels = mapped
        .chunks_exact(256)
        .flat_map(|row| row[..64].iter().copied())
        .collect();
    drop(mapped);
    readback.unmap();
    assert!(iced::futures::executor::block_on(device.pop_error_scope()).is_none());
    pixels
}
