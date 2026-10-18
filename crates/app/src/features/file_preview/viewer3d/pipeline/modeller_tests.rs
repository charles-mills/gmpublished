use super::super::test_support::empty_model_preview;
use super::*;
use iced::Point;

fn prepared_scene<'a>(pipeline: &'a ModelPipeline, primitive: &ModelPrimitive) -> &'a ViewerScene {
    let frame = &pipeline.frames[primitive.prepared_frame.load(Ordering::Relaxed)];
    primitive.stage.as_ref().map_or(&frame.scene, |_| {
        &pipeline.frames[frame.stage_frame.expect("prepared stage")].scene
    })
}

fn prepare(
    primitive: &ModelPrimitive,
    pipeline: &mut ModelPipeline,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    bounds: &Rectangle,
    viewport: &Viewport,
) {
    shader::Pipeline::trim(pipeline);
    shader::Pipeline::begin_prepare(pipeline);
    shader::Primitive::prepare(primitive, pipeline, device, queue, bounds, viewport);
    shader::Pipeline::finish_prepare(pipeline, device, queue);
}

#[test]
#[ignore = "requires an exclusively reserved Vulkan GPU"]
fn staged_model_renders_and_reuses_gpu_geometry_after_transform() {
    render_uniform_quad(true, false, false);
}

#[test]
#[ignore = "requires an exclusively reserved Vulkan GPU"]
fn skeletal_animation_reuses_buffers_and_restores_bind_pose() {
    render_uniform_quad(false, true, false);
    render_uniform_quad(true, true, false);
}

#[test]
#[ignore = "requires an exclusively reserved Vulkan GPU"]
fn lod_switch_replaces_geometry_and_keeps_selected_animation_in_both_viewers() {
    render_uniform_quad(false, true, true);
    render_uniform_quad(true, true, true);
}

fn render_uniform_quad(staged: bool, animated: bool, lods: bool) {
    const WIDTH: u32 = 512;
    const HEIGHT: u32 = 384;

    // Naga's GLSL backend cannot translate depth textureLoad in the water shader.
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
        backends: wgpu::Backends::PRIMARY,
        ..wgpu::InstanceDescriptor::default()
    });
    let Ok(adapter) = futures::executor::block_on(
        instance.request_adapter(&wgpu::RequestAdapterOptions::default()),
    ) else {
        eprintln!("skipping: no wgpu adapter available");
        return;
    };
    let (device, queue) =
        futures::executor::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("test.uniform_shading"),
            // Match iced_wgpu's shared-device limits.
            required_limits: wgpu::Limits {
                max_bind_groups: 4,
                ..wgpu::Limits::default()
            },
            ..wgpu::DeviceDescriptor::default()
        }))
        .expect("device");

    let vertex = |x: f32, y: f32| ModelVertex {
        position: Vec3::new(x, y, 20.0),
        normal: Vec3::new(0.0, 0.0, 1.0),
        uv: [0.25, 0.25],
        lightmap_uv: [0.0; 2],
        lightmap_bump_step: 0.0,
        opacity: 1.0,
        illumination: Vec3::splat(1.0),
        lighting_mode: crate::media::preview_model::vertex_lighting::PREVIEW,
        tangent_s: Vec3::new(1.0, 0.0, 0.0),
        tangent_t: Vec3::new(0.0, 1.0, 0.0),
        tangent_sign: 1.0,
        lighting_context: 0,
        surface_mark: 0,
        lightmap_page: 0,
        color: Vec3::splat(1.0),
        blend_alpha: 0.0,
    };
    let mesh = MeshData {
        geometry: crate::media::preview_model::MeshGeometry::Model,
        vertices: vec![
            vertex(-48.0, -24.0),
            vertex(48.0, -24.0),
            vertex(48.0, 24.0),
            vertex(-48.0, 24.0),
        ],
        // Keep the fixture visible from either side of the default orbit camera.
        indices: vec![0, 1, 2, 0, 2, 3, 2, 1, 0, 3, 2, 0],
        material_index: 0,
        bodygroup: 0,
        bodygroup_choice: 0,
    };
    let mut preview =
        empty_model_preview(Vec3::new(-48.0, -24.0, 0.0), Vec3::new(48.0, 24.0, 38.0));
    let scene = Arc::make_mut(&mut preview.scene);
    scene.meshes = vec![mesh];
    scene.materials = vec![MaterialSlot {
        tint_base: None,
        name: "test".to_owned(),
        texture: None,
        texture2: None,
        resolution: crate::bridge::materials::MaterialResolutionOutcome::Loaded,
        canonical_name: None,
        shader: crate::bridge::materials::MaterialShader::Unknown,
        alpha_policy: RenderMode::Opaque.texture_alpha_policy(),
        render_mode: RenderMode::Opaque,
    }];
    let preview = if lods {
        crate::model_animation_test_support::lod_blended_model()
    } else if animated {
        crate::model_animation_test_support::blended_model()
    } else {
        Arc::new(preview)
    };
    let mut map =
        super::super::test_support::empty_preview(Vec3::splat(-256.0), Vec3::splat(256.0));
    let map_scene = Arc::make_mut(&mut map.scene);
    let mut floor_vertices = vec![
        vertex(-256.0, -256.0),
        vertex(256.0, -256.0),
        vertex(256.0, 256.0),
        vertex(-256.0, 256.0),
    ];
    for vertex in &mut floor_vertices {
        vertex.position[2] = -16.0;
    }
    map_scene.meshes.push(MeshData {
        geometry: crate::media::preview_model::MeshGeometry::Brush,
        vertices: floor_vertices,
        indices: vec![0, 1, 2, 0, 2, 3, 2, 1, 0, 3, 2, 0],
        material_index: 0,
        bodygroup: 0,
        bodygroup_choice: 0,
    });
    map_scene.materials = preview.scene.materials.clone();

    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let bounds = Rectangle::new(Point::ORIGIN, iced::Size::new(WIDTH as f32, HEIGHT as f32));
    let clip_bounds = Rectangle::<u32> {
        x: 0,
        y: 0,
        width: WIDTH,
        height: HEIGHT,
    };
    let viewport = Viewport::with_physical_size(iced::Size::new(WIDTH, HEIGHT), 1.0);
    let mut camera = Camera::default();
    camera.ensure_spawn(1, None);
    if animated {
        camera.orbit.set_distance(1.6);
    }
    let mut primitive = ModelPrimitive {
        cpu_executor: None,
        render_feedback: None,
        model_lighting_time: 0.0,
        animation_clock: Default::default(),
        view_id: ViewId::default(),
        clear_color: None,
        air_fog: None,
        prepared_frame: std::sync::atomic::AtomicUsize::new(usize::MAX),
        skybox_transform: None,
        view_camera: None,
        studio_grid: (!staged).then(|| {
            StudioGrid::new(
                Uniforms::for_model(&preview, &camera, bounds, None).view_proj,
                crate::media::preview_model::ModelBounds {
                    min: preview.scene.bounds_min,
                    max: preview.scene.bounds_max,
                },
            )
        }),
        animation_pose: None,
        stage: staged.then(|| super::StagePrimitive {
            local_bounds: preview.visible_bounds(None, &[]),
            animation_pose: None,
            model: Arc::clone(&preview),
            content_id: 2,
            transform: crate::features::file_preview::state::StageTransform::default(),
            live_transform: super::super::identity_matrix(),
            material_tints: vec![(0, [255, 64, 64])],
            skin_remap: vec![0],
            bodygroup_choices: Vec::new(),
            phy_debug_visible: false,
        }),
        skin_remap: vec![0],
        material_tints: if staged {
            Vec::new()
        } else {
            vec![(0, [255, 64, 64])]
        },
        bodygroup_choices: Vec::new(),
        map_skybox_visible: false,
        visibility_culling: false,
        phy_debug_visible: false,
        uniforms: Uniforms::for_model(&preview, &camera, bounds, None),
        submerged: false,
        map_skybox_uniforms: None,
        sky_uniforms: None,
        door_poses: Vec::new(),
        preview: if staged {
            PreviewScene::Map(Arc::new(map))
        } else {
            PreviewScene::Model(Arc::clone(&preview))
        },
        content_id: 1,
    };
    let mut pipeline_state = <ModelPipeline as shader::Pipeline>::new(&device, &queue, format);
    prepare(
        &primitive,
        &mut pipeline_state,
        &device,
        &queue,
        &bounds,
        &viewport,
    );

    if animated {
        let buffer = prepared_scene(&pipeline_state, &primitive).meshes[0]
            .vertices()
            .clone();
        let mut posed_buffer = None;
        let centroid = prepared_scene(&pipeline_state, &primitive).meshes[0].centroid();
        let mut animated_centroid: Option<Vec3> = None;
        for pose in [Some((1.0, 1.0)), Some((1.0, 0.0)), None, Some((0.5, 1.0))].map(|pose| {
            pose.map(|(cycle, parameter)| PreviewPose {
                sequence: 0,
                cycle,
                parameters: vec![parameter],
            })
        }) {
            if let Some(stage) = primitive.stage.as_mut() {
                stage.animation_pose = pose.clone();
            } else {
                primitive.animation_pose = pose.clone();
            }
            prepare(
                &primitive,
                &mut pipeline_state,
                &device,
                &queue,
                &bounds,
                &viewport,
            );
            let mesh = &prepared_scene(&pipeline_state, &primitive).meshes[0];
            if pose.is_some() {
                let old = posed_buffer.get_or_insert_with(|| mesh.vertices().clone());
                assert_eq!(mesh.vertices(), old);
            } else {
                assert_eq!(mesh.vertices(), &buffer);
            }
            if let Some(pose) = &pose {
                assert!((mesh.centroid() - centroid).length() > 1.0);
                if pose.parameters == [0.0] {
                    let first = animated_centroid.unwrap();
                    assert!((mesh.centroid() - first).length() > 1.0);
                    assert!(
                        (mesh.centroid() - centroid - (first - centroid) * 0.5).length() < 0.025
                    );
                } else {
                    animated_centroid = Some(mesh.centroid());
                }
            } else {
                assert!((mesh.centroid() - centroid).length() < 0.001);
            }
        }
    }

    if lods {
        let original_buffer = prepared_scene(&pipeline_state, &primitive).meshes[0]
            .vertices()
            .clone();
        let lower = preview.at_lod(1);
        assert_eq!(lower.scene.meshes[0].indices.len(), 3);
        if let Some(stage) = primitive.stage.as_mut() {
            stage.model = Arc::clone(&lower);
        } else {
            primitive.preview = PreviewScene::Model(Arc::clone(&lower));
        }
        prepare(
            &primitive,
            &mut pipeline_state,
            &device,
            &queue,
            &bounds,
            &viewport,
        );
        let mesh = &prepared_scene(&pipeline_state, &primitive).meshes[0];
        assert_ne!(*mesh.vertices(), original_buffer);
        assert_eq!(mesh.index_count, 3);
        let expected = lower
            .animation
            .as_ref()
            .unwrap()
            .as_ref()
            .unwrap()
            .evaluate_lod(0, 0.5, &[1.0], 1)
            .unwrap();
        let centroid = expected[0].iter().fold(Vec3::default(), |sum, vertex| {
            sum + Vec3::from(vertex.position)
        }) / expected[0].len() as f32;
        assert!((mesh.centroid() - centroid).length() < 0.001);
        let lower_buffer = mesh.vertices().clone();
        prepare(
            &primitive,
            &mut pipeline_state,
            &device,
            &queue,
            &bounds,
            &viewport,
        );
        assert_eq!(
            *prepared_scene(&pipeline_state, &primitive).meshes[0].vertices(),
            lower_buffer
        );
        if let Some(stage) = primitive.stage.as_mut() {
            stage.model = Arc::clone(&preview);
        } else {
            primitive.preview = PreviewScene::Model(Arc::clone(&preview));
        }
        prepare(
            &primitive,
            &mut pipeline_state,
            &device,
            &queue,
            &bounds,
            &viewport,
        );
    }

    if staged {
        let map_vertices = pipeline_state.frames[primitive.prepared_frame.load(Ordering::Relaxed)]
            .scene
            .meshes
            .len();
        let stage_vertices = prepared_scene(&pipeline_state, &primitive).meshes[0]
            .vertices()
            .clone();
        primitive.stage.as_mut().unwrap().transform.position = Vec3::new(4.0, 6.0, 2.0);
        primitive.stage.as_mut().unwrap().transform.angles = Vec3::new(0.0, 15.0, 0.0);
        prepare(
            &primitive,
            &mut pipeline_state,
            &device,
            &queue,
            &bounds,
            &viewport,
        );
        assert_eq!(
            pipeline_state.frames[primitive.prepared_frame.load(Ordering::Relaxed)]
                .scene
                .meshes
                .len(),
            map_vertices
        );
        assert_eq!(
            *prepared_scene(&pipeline_state, &primitive).meshes[0].vertices(),
            stage_vertices
        );
    }

    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("test.uniform_shading.target"),
        size: wgpu::Extent3d {
            width: WIDTH,
            height: HEIGHT,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let target_view = target.create_view(&wgpu::TextureViewDescriptor::default());
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    {
        let _clear = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("test.uniform_shading.clear"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &target_view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLUE),
                    store: wgpu::StoreOp::Store,
                },
                depth_slice: None,
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
    }
    shader::Primitive::render(
        &primitive,
        &pipeline_state,
        &mut encoder,
        &target_view,
        &clip_bounds,
    );
    let padded_row = (WIDTH * 4).div_ceil(256) * 256;
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("test.uniform_shading.readback"),
        size: u64::from(padded_row) * u64::from(HEIGHT),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
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
                bytes_per_row: Some(padded_row),
                rows_per_image: Some(HEIGHT),
            },
        },
        wgpu::Extent3d {
            width: WIDTH,
            height: HEIGHT,
            depth_or_array_layers: 1,
        },
    );
    queue.submit([encoder.finish()]);
    let (sender, receiver) = std::sync::mpsc::channel();
    readback
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| {
            let _ = sender.send(result);
        });
    let _ = device.poll(wgpu::PollType::Wait {
        submission_index: None,
        timeout: None,
    });
    receiver
        .recv()
        .expect("map_async callback")
        .expect("map readback");
    let mapped = readback.slice(..).get_mapped_range();
    let mut pixels = Vec::with_capacity((WIDTH * HEIGHT * 4) as usize);
    for y in 0..HEIGHT {
        let start = (y * padded_row) as usize;
        pixels.extend_from_slice(&mapped[start..start + (WIDTH * 4) as usize]);
    }
    drop(mapped);
    readback.unmap();

    // Exclude the blue background and its antialiased silhouette pixels.
    let is_quad = |x: u32, y: u32| {
        let offset = ((y * WIDTH + x) * 4) as usize;
        let pixel = &pixels[offset..offset + 4];
        pixel[0] > pixel[1].saturating_add(20) && pixel[0] > pixel[2].saturating_add(20)
    };
    let mut interior = 0_usize;
    for y in 1..HEIGHT - 1 {
        for x in 1..WIDTH - 1 {
            if !(is_quad(x, y)
                && is_quad(x - 1, y)
                && is_quad(x + 1, y)
                && is_quad(x, y - 1)
                && is_quad(x, y + 1))
            {
                continue;
            }
            interior += 1;
        }
    }
    assert!(
        interior > if animated { 100 } else { 1000 },
        "model geometry was not rendered: {interior} interior pixels"
    );
}
