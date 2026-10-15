use super::{SunView, sun_quads};
use crate::features::file_preview::viewer3d::pipeline::targets::RenderTargets;
use crate::features::file_preview::viewer3d::pipeline::{
    MapPreview, RenderMode, Uniforms, ViewerScene, wgpu,
};
use gmpublished_domain::math::Vec3;
use std::sync::Arc;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    matrix: [[f32; 4]; 4],
    positions: [[f32; 4]; 8],
    colors: [[f32; 4]; 2],
    world_rect: [f32; 4],
    sky_rect: [f32; 4],
    query: [f32; 4],
    flags: [u32; 4],
}

#[derive(Clone, Debug)]
pub struct Resources {
    query_layout: wgpu::BindGroupLayout,
    draw: Arc<DrawResources>,
    sample_count: u32,
    target_format: wgpu::TextureFormat,
    query_world: wgpu::ComputePipeline,
    query_sky: wgpu::ComputePipeline,
    opaque: wgpu::RenderPipeline,
    transparent: wgpu::RenderPipeline,
    additive: wgpu::RenderPipeline,
}

impl Resources {
    pub fn new(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        material: &wgpu::BindGroupLayout,
        sample_count: u32,
    ) -> Self {
        let draw = Arc::new(DrawResources::new(device, material));
        Self::new_for_samples(device, format, sample_count, draw)
    }

    pub fn with_sample_count(&self, device: &wgpu::Device, sample_count: u32) -> Self {
        if self.sample_count == sample_count {
            return self.clone();
        }
        Self::new_for_samples(
            device,
            self.target_format,
            sample_count,
            Arc::clone(&self.draw),
        )
    }

    fn new_for_samples(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        sample_count: u32,
        draw: Arc<DrawResources>,
    ) -> Self {
        let query_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("sun.depth_query.layout"),
            entries: &[
                uniform_layout_entry(wgpu::ShaderStages::COMPUTE),
                state_layout_entry(wgpu::ShaderStages::COMPUTE, false),
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Depth,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: sample_count > 1,
                    },
                    count: None,
                },
            ],
        });
        let query_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("sun.depth_query.shader"),
            source: wgpu::ShaderSource::Wgsl(super::super::sampling::visibility_shader(
                include_str!("visibility.wgsl"),
                sample_count,
            )),
        });
        let query_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("sun.depth_query.pipeline_layout"),
                bind_group_layouts: &[&query_layout],
                push_constant_ranges: &[],
            });
        let compute = |entry| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(entry),
                layout: Some(&query_pipeline_layout),
                module: &query_shader,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        let query_world = compute("query_world");
        let query_sky = compute("query_sky");
        let render = |blend| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("sun.draw.pipeline"),
                layout: Some(&draw.pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &draw.shader,
                    entry_point: Some("vs_main"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &draw.shader,
                    entry_point: Some("fs_main"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState {
                    cull_mode: None,
                    ..Default::default()
                },
                depth_stencil: None,
                multisample: wgpu::MultisampleState {
                    count: sample_count,
                    ..Default::default()
                },
                multiview: None,
                cache: None,
            })
        };
        let opaque = render(None);
        let transparent = render(Some(wgpu::BlendState::ALPHA_BLENDING));
        let additive = render(Some(wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::SrcAlpha,
                dst_factor: wgpu::BlendFactor::One,
                operation: wgpu::BlendOperation::Add,
            },
            alpha: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::Zero,
                dst_factor: wgpu::BlendFactor::One,
                operation: wgpu::BlendOperation::Add,
            },
        }));
        Self {
            query_layout,
            draw,
            sample_count,
            target_format: format,
            query_world,
            query_sky,
            opaque,
            transparent,
            additive,
        }
    }
}

#[derive(Debug)]
struct DrawResources {
    layout: wgpu::BindGroupLayout,
    shader: wgpu::ShaderModule,
    pipeline_layout: wgpu::PipelineLayout,
}

impl DrawResources {
    fn new(device: &wgpu::Device, material: &wgpu::BindGroupLayout) -> Self {
        let draw_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("sun.draw.layout"),
            entries: &[
                uniform_layout_entry(wgpu::ShaderStages::VERTEX),
                state_layout_entry(wgpu::ShaderStages::VERTEX, true),
            ],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("sun.draw.shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("draw.wgsl").into()),
        });
        let draw_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("sun.draw.pipeline_layout"),
            bind_group_layouts: &[&draw_layout, material],
            push_constant_ranges: &[],
        });
        Self {
            layout: draw_layout,
            shader,
            pipeline_layout: draw_pipeline_layout,
        }
    }
}

fn uniform_layout_entry(visibility: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding: 0,
        visibility,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: wgpu::BufferSize::new(std::mem::size_of::<Params>() as u64),
        },
        count: None,
    }
}

fn state_layout_entry(
    visibility: wgpu::ShaderStages,
    read_only: bool,
) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding: 1,
        visibility,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only },
            has_dynamic_offset: false,
            min_binding_size: wgpu::BufferSize::new(16),
        },
        count: None,
    }
}

#[derive(Debug)]
struct DepthQuery {
    depth: wgpu::TextureView,
    layout: wgpu::BindGroupLayout,
    bind_group: wgpu::BindGroup,
}

#[derive(Debug)]
struct Entry {
    uniforms: wgpu::Buffer,
    state: wgpu::Buffer,
    query: Option<DepthQuery>,
    draw_group: wgpu::BindGroup,
    materials: [usize; 2],
    modes: [RenderMode; 2],
    layers: [bool; 2],
}

#[derive(Debug, Default)]
pub struct Frame {
    entries: Vec<Entry>,
    last_time: Option<f32>,
}

#[derive(Clone, Copy)]
pub struct Inputs<'a> {
    pub map: Option<&'a MapPreview>,
    pub world: Uniforms,
    pub skybox: Option<Uniforms>,
    pub targets: &'a RenderTargets,
}

impl Frame {
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        resources: &Resources,
        inputs: &Inputs<'_>,
    ) {
        let Inputs {
            map,
            world,
            skybox,
            targets,
        } = *inputs;
        let Some(map) = map else {
            self.entries.clear();
            return;
        };
        let time = world.water_time_sky_tint[0];
        let reset = self.last_time.is_none_or(|last| time <= last);
        let rate = self
            .last_time
            .map_or(1.0, |last| (time - last).clamp(0.0, 0.25) / 0.0625);
        self.last_time = Some(time);
        while self.entries.len() < map.suns.len() {
            let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("sun.view.uniforms"),
                size: std::mem::size_of::<Params>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let state = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("sun.view.visibility"),
                size: 16,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let draw_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("sun.draw.inputs"),
                layout: &resources.draw.layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: uniforms.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: state.as_entire_binding(),
                    },
                ],
            });
            queue.write_buffer(&state, 0, bytemuck::cast_slice(&[1.0_f32; 4]));
            self.entries.push(Entry {
                uniforms,
                state,
                draw_group,
                query: None,
                materials: [0; 2],
                modes: [RenderMode::Opaque; 2],
                layers: [false; 2],
            });
        }
        self.entries.truncate(map.suns.len());
        let eye = Vec3::from([
            world.camera_position[0],
            world.camera_position[1],
            world.camera_position[2],
        ]);
        let matrix = world.view_proj;
        let forward = Vec3::new(matrix[0][3], matrix[1][3], matrix[2][3]).normalize_or_zero();
        for ((sun, slots), entry) in map
            .suns
            .iter()
            .zip(&map.sun_material_slots)
            .zip(&mut self.entries)
        {
            let quads = sun_quads(sun, SunView { eye, forward }, 1.0);
            let maximum = sun.color_srgb.into_iter().max().unwrap_or(0);
            let core = if maximum == 0 {
                Vec3::splat(1.0)
            } else {
                Vec3::from(sun.color_srgb.map(|x| f32::from(x) / f32::from(maximum)))
            };
            let glow = if sun.overlay_color_srgb == [0; 3] {
                core
            } else {
                Vec3::from(sun.overlay_color_srgb.map(|x| f32::from(x) / 255.0))
            };
            let opacity = ((sun.direction_to_sun.normalize_or_zero().dot(forward) - 0.9) * 7.5)
                .clamp(0.0, 0.75);
            let mut positions = [[0.0; 4]; 8];
            for (layer, quad) in quads.iter().enumerate() {
                if let Some(quad) = quad {
                    for (index, vertex) in quad.vertices.iter().enumerate() {
                        positions[layer * 4 + index] = [
                            vertex.position[0],
                            vertex.position[1],
                            vertex.position[2],
                            1.0,
                        ];
                    }
                }
            }
            let (world_rect, world_depth) = proxy(world, sun.direction_to_sun, targets.size);
            let (sky_rect, sky_depth) = skybox.map_or(([0.0; 4], 0.0), |uniforms| {
                proxy(uniforms, sun.direction_to_sun, targets.size)
            });
            let color = |v: Vec3| [v[0], v[1], v[2], 1.0];
            let params = Params {
                matrix,
                positions,
                colors: [color(core), color(glow * opacity)],
                world_rect,
                sky_rect,
                query: [world_depth, sky_depth, rate, f32::from(reset)],
                flags: [0, 0, u32::from(skybox.is_some()), 0],
            };
            queue.write_buffer(&entry.uniforms, 0, bytemuck::bytes_of(&params));
            if entry.query.as_ref().is_none_or(|query| {
                query.depth != targets.depth || query.layout != resources.query_layout
            }) {
                let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("sun.depth_query.inputs"),
                    layout: &resources.query_layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: entry.uniforms.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: entry.state.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: wgpu::BindingResource::TextureView(&targets.depth),
                        },
                    ],
                });
                entry.query = Some(DepthQuery {
                    depth: targets.depth.clone(),
                    layout: resources.query_layout.clone(),
                    bind_group,
                });
            }
            entry.materials = slots.map(|slot| slot.unwrap_or(0));
            entry.modes = slots.map(|slot| {
                slot.map_or(RenderMode::Opaque, |slot| {
                    map.scene.materials[slot].render_mode
                })
            });
            entry.layers =
                std::array::from_fn(|index| slots[index].is_some() && quads[index].is_some());
        }
    }

    pub fn query(&self, encoder: &mut wgpu::CommandEncoder, resources: &Resources, skybox: bool) {
        if self.entries.is_empty() {
            return;
        }
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("sun.depth_visibility"),
            timestamp_writes: None,
        });
        pass.set_pipeline(if skybox {
            &resources.query_sky
        } else {
            &resources.query_world
        });
        for entry in &self.entries {
            if entry.layers.iter().any(|layer| *layer) {
                pass.set_bind_group(
                    0,
                    &entry
                        .query
                        .as_ref()
                        .expect("prepared Sun depth query")
                        .bind_group,
                    &[],
                );
                pass.dispatch_workgroups(1, 1, 1);
            }
        }
    }

    pub fn draw(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        resources: &Resources,
        upload: &ViewerScene,
        targets: &RenderTargets,
    ) {
        if self.entries.is_empty() {
            return;
        }
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("sun.overlay"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &targets.color,
                resolve_target: (targets.sample_count > 1).then_some(&targets.resolve_view),
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        for entry in &self.entries {
            for layer in 0..2usize {
                if !entry.layers[layer] {
                    continue;
                }
                pass.set_pipeline(match entry.modes[layer] {
                    RenderMode::Opaque | RenderMode::Cutout => &resources.opaque,
                    RenderMode::Translucent => &resources.transparent,
                    RenderMode::Additive => &resources.additive,
                    RenderMode::Modulate => continue,
                });
                pass.set_bind_group(0, &entry.draw_group, &[]);
                pass.set_bind_group(
                    1,
                    &upload.assets.material_bind_groups[entry.materials[layer]],
                    &[],
                );
                pass.draw(0..6, layer as u32..layer as u32 + 1);
            }
        }
    }
}

fn proxy(uniforms: Uniforms, direction: Vec3, size: (u32, u32)) -> ([f32; 4], f32) {
    let matrix = uniforms.view_proj;
    let row = |i| Vec3::new(matrix[0][i], matrix[1][i], matrix[2][i]);
    let forward = row(3).normalize_or_zero();
    let right = (row(0) - forward * row(0).dot(forward)).normalize_or_zero();
    let up = (row(1) - forward * row(1).dot(forward)).normalize_or_zero();
    let eye = Vec3::from([
        uniforms.camera_position[0],
        uniforms.camera_position[1],
        uniforms.camera_position[2],
    ]);
    let distance = uniforms.water_depth_params[1] * 0.999 - 0.05;
    let direction = direction.normalize_or_zero();
    let center = eye + direction * distance;
    let axial = direction.dot(forward) * distance;
    if axial <= 0.0 {
        return ([0.0; 4], 0.0);
    }
    let half_size = axial * 0.025 * std::f32::consts::FRAC_1_SQRT_2;
    let project = |point: Vec3| {
        let p = [point[0], point[1], point[2], 1.0];
        let clip =
            std::array::from_fn::<_, 4, _>(|r| (0..4).map(|c| matrix[c][r] * p[c]).sum::<f32>());
        [
            (clip[0] / clip[3] * 0.5 + 0.5) * size.0 as f32,
            (0.5 - clip[1] / clip[3] * 0.5) * size.1 as f32,
            clip[2] / clip[3],
        ]
    };
    let top_left = project(center - right * half_size + up * half_size);
    let bottom_right = project(center + right * half_size - up * half_size);
    let center = project(center);
    (
        [top_left[0], top_left[1], bottom_right[0], bottom_right[1]],
        center[2],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::wgpu::naga;
    use std::mem::{offset_of, size_of};

    #[test]
    fn sun_draw_and_visibility_shaders_share_the_cpu_uniform_layout() {
        for source in [include_str!("draw.wgsl"), include_str!("visibility.wgsl")] {
            let module = naga::front::wgsl::parse_str(source).unwrap();
            naga::valid::Validator::new(
                naga::valid::ValidationFlags::all(),
                naga::valid::Capabilities::all(),
            )
            .validate(&module)
            .unwrap();
            let layout = module
                .types
                .iter()
                .find_map(|(_, ty)| (ty.name.as_deref() == Some("Params")).then_some(&ty.inner))
                .unwrap();
            let naga::TypeInner::Struct { members, span } = layout else {
                panic!("sun uniform layout");
            };
            assert_eq!(*span as usize, size_of::<Params>());
            let fields = [
                ("matrix", offset_of!(Params, matrix)),
                ("positions", offset_of!(Params, positions)),
                ("colors", offset_of!(Params, colors)),
                ("world_rect", offset_of!(Params, world_rect)),
                ("sky_rect", offset_of!(Params, sky_rect)),
                ("query", offset_of!(Params, query)),
                ("flags", offset_of!(Params, flags)),
            ];
            assert_eq!(members.len(), fields.len());
            for (member, (name, offset)) in members.iter().zip(fields) {
                assert_eq!(member.name.as_deref(), Some(name));
                assert_eq!(member.offset as usize, offset);
            }
        }
    }
}
