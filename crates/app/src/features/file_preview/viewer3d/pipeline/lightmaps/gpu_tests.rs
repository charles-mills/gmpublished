use super::super::texture_upload::{TextureUploader, UploadError, UploadPolicy};
use super::*;
use gmpublished_backend::CpuExecutor;
use gmpublished_domain::{math::Vec3, scene::map::LightmapSource};
use std::time::{Duration, Instant};

#[test]
#[ignore = "requires exclusive Vulkan GPU; GMPUBLISHED_PARALLEL_LIGHTMAP_COOKING must be unset"]
fn production_pool_parallel_pages_match_serial_reuse_and_error_retirement() {
    assert!(cooking::enabled(), "run the real parallel path");
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
        backends: wgpu::Backends::VULKAN,
        flags: wgpu::InstanceFlags::VALIDATION,
        ..Default::default()
    });
    let adapter = iced::futures::executor::block_on(
        instance.request_adapter(&wgpu::RequestAdapterOptions::default()),
    )
    .expect("parallel lightmap pool contract requires Vulkan");
    let (device, queue) = iced::futures::executor::block_on(
        adapter.request_device(&wgpu::DeviceDescriptor::default()),
    )
    .unwrap();
    let _completion_scope = super::super::completion::TestDeviceScope::new(&device);
    let executor = CpuExecutor::build(4).unwrap();
    device.push_error_scope(wgpu::ErrorFilter::Validation);
    let layout = page_layout(&device);
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor::default());
    let mut map = super::super::super::test_support::empty_preview(Vec3::ZERO, Vec3::splat(1.0));
    map.lightmap = Some(LightmapSet::style_pages_for_tests(
        LightmapSource::Ldr,
        &(0..5)
            .map(|index| {
                vec![
                    (0, [32 + index * 16, 64, 96, 0]),
                    (32, [16, 8 + index, 4, 0]),
                ]
            })
            .collect::<Vec<_>>(),
        &[(32, "amz")],
    ));
    let source = PreviewScene::Map(Arc::new(map));
    let parallel_request = Request::new(source.clone(), (0..5).map(LightmapPageId), 0.1)
        .unwrap()
        .with_cpu_executor(Some(executor.clone()));
    let serial_request = Request::new(source.clone(), (0..5).map(LightmapPageId), 0.1).unwrap();
    let other_view = Request::new(
        source.clone(),
        [LightmapPageId(4), LightmapPageId(1), LightmapPageId(1)],
        0.1,
    )
    .unwrap()
    .with_cpu_executor(Some(executor.clone()));
    assert_eq!(
        other_view.keys.len(),
        2,
        "request constructor deduplicates views' page IDs"
    );
    // One 1024-byte staging slot holds exactly one 4x4 padded page.
    let policy = UploadPolicy {
        aggregate_staging_bytes: 1024,
        batch_bytes: 1024,
        ..Default::default()
    };
    let (mut parallel_pool, parallel_uploader) = pool(&device, &queue, &layout, &sampler, policy);
    let (mut serial_pool, serial_uploader) = pool(&device, &queue, &layout, &sampler, policy);
    for (pool, request) in [
        (&mut parallel_pool, &parallel_request),
        (&mut serial_pool, &serial_request),
    ] {
        let missing = pool.demand(&[request, &other_view]).unwrap();
        assert_eq!((missing.bytes, missing.allocations), (5 * 64, 5));
    }
    let parallel_before = parallel_uploader.lock().unwrap().stats().submissions;
    let serial_before = serial_uploader.lock().unwrap().stats().submissions;
    let parallel = parallel_pool
        .prepare(&device, &layout, &parallel_request)
        .unwrap();
    let serial = serial_pool
        .prepare(&device, &layout, &serial_request)
        .unwrap();
    let diagnostics = parallel_pool.stats();
    assert_eq!(diagnostics["parallel_cooking"]["executor_workers"], 4);
    assert_eq!(diagnostics["parallel_cooking"]["peak_wave_jobs"], 4);
    assert_eq!(diagnostics["parallel_cooking"]["attempted_pages"], 5);
    assert_eq!(diagnostics["parallel_cooking"]["parallel_enabled"], true);
    assert_eq!(
        serial_pool.stats()["parallel_cooking"]["parallel_enabled"],
        false
    );
    for (uploader, before) in [
        (&parallel_uploader, parallel_before),
        (&serial_uploader, serial_before),
    ] {
        let stats = uploader.lock().unwrap().stats();
        assert_eq!(stats.submissions - before, 5);
        assert!(stats.pressure_flushes >= 4);
        assert!(stats.peak_staging_bytes <= 1024);
    }
    for id in (0..5).map(LightmapPageId) {
        let expected = parallel_request
            .lightmaps()
            .unwrap()
            .render_page(id, &parallel_request.weights)
            .unwrap();
        let parallel_pixels = read_page(&device, &queue, &parallel.pages[&id].1);
        let serial_pixels = read_page(&device, &queue, &serial.pages[&id].1);
        assert_eq!(parallel_pixels, serial_pixels);
        assert_eq!(parallel_pixels, expected);
    }
    let uploads = parallel_pool.uploads;
    let sibling = parallel_pool
        .prepare(&device, &layout, &other_view)
        .unwrap();
    assert_eq!(parallel_pool.uploads, uploads);
    for id in [LightmapPageId(1), LightmapPageId(4)] {
        assert!(Arc::ptr_eq(&parallel.pages[&id].1, &sibling.pages[&id].1));
    }
    // Cancel the first view while a sibling still owns its same current pages.
    drop(parallel);
    assert_eq!(
        parallel_pool.demand(&[&other_view]).unwrap(),
        MissingCost::default()
    );
    assert_eq!(parallel_pool.active_cost().allocations, 2);
    assert_eq!(
        read_page(&device, &queue, &sibling.pages[&LightmapPageId(1)].1),
        parallel_request
            .lightmaps()
            .unwrap()
            .render_page(LightmapPageId(1), &parallel_request.weights)
            .unwrap()
    );
    drop(sibling);
    drop(serial);
    parallel_pool.demand(&[]).unwrap();
    serial_pool.demand(&[]).unwrap();
    drop(parallel_pool);
    drop(serial_pool);
    await_retired(&parallel_uploader);
    await_retired(&serial_uploader);

    // Fault injection bypasses Request::new's eager missing-page rejection only
    // to exercise Pool's real ordinal failure path. Keys remain unique.
    for missing_position in [3, 1] {
        for cpu in [None, Some(executor.clone())] {
            let limited = UploadPolicy {
                sampled_image_bytes: 128,
                sampled_image_allocations: 2,
                aggregate_staging_bytes: 2048,
                batch_bytes: 2048,
            };
            let (mut pool, uploader) = pool(&device, &queue, &layout, &sampler, limited);
            let mut request = Request::new(source.clone(), (0..3).map(LightmapPageId), 0.1)
                .unwrap()
                .with_cpu_executor(cpu);
            request.keys.insert(
                missing_position,
                Key {
                    scene: source.key(),
                    page: LightmapPageId(u32::MAX),
                    styles: Vec::new(),
                },
            );
            let result = pool.prepare(&device, &layout, &request);
            let expected_prefix = if missing_position == 3 {
                assert!(matches!(
                    result,
                    Err(Error::Upload(UploadError::ImageBudget { .. }))
                ));
                2
            } else {
                assert!(matches!(
                    result,
                    Err(Error::MissingPage(LightmapPageId(u32::MAX)))
                ));
                1
            };
            assert_eq!(pool.pages.len(), expected_prefix);
            let keys: BTreeSet<_> = pool.pages.keys().map(|key| key.page.0).collect();
            assert_eq!(keys, (0..expected_prefix as u32).collect());
            if request.cpu_executor.is_some() {
                assert_eq!(pool.stats()["parallel_cooking"]["peak_wave_jobs"], 4);
            }
            // prepare's existing error path does not promise a final flush.
            // Submit its actual committed prefix explicitly for readback/cleanup.
            uploader.lock().unwrap().flush();
            for (key, entry) in &pool.pages {
                let id = key.page;
                assert_eq!(
                    read_page(&device, &queue, &entry.page),
                    request
                        .lightmaps()
                        .unwrap()
                        .render_page(id, &request.weights)
                        .unwrap()
                );
            }
            pool.demand(&[]).unwrap();
            drop(pool);
            await_retired(&uploader);
        }
    }
    assert!(iced::futures::executor::block_on(device.pop_error_scope()).is_none());
}

fn pool(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    layout: &wgpu::BindGroupLayout,
    sampler: &wgpu::Sampler,
    policy: UploadPolicy,
) -> (Pool, SharedUploader) {
    let uploader = TextureUploader::shared(device, queue, policy).unwrap();
    let white = Page::white(device, layout, &uploader, sampler).unwrap();
    uploader.lock().unwrap().flush();
    (Pool::new(&uploader, white), uploader)
}

fn await_retired(uploader: &SharedUploader) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let stats = uploader.lock().unwrap().stats();
        if stats.sampled_image_allocations == 0
            && stats.owned_image_allocations == 0
            && serde_json::to_value(stats.completion).unwrap()["pending"] == 0
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "closed lightmap pool did not retire: {stats:?}"
        );
        // No owner-thread device.poll: the actual completion worker must drain.
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn page_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    let texture = |binding| wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    };
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: None,
        entries: &[
            texture(0),
            texture(1),
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    })
}

fn read_page(device: &wgpu::Device, queue: &wgpu::Queue, page: &Page) -> Vec<u8> {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: None,
        source: wgpu::ShaderSource::Wgsl(
            "@group(0) @binding(0) var image: texture_2d<f32>;
            @group(0) @binding(1) var<storage, read_write> output: array<u32>;
            @compute @workgroup_size(1) fn main(@builtin(global_invocation_id) id: vec3<u32>) {
                let width = textureDimensions(image, 0).x;
                let pixel = vec4<u32>(round(textureLoad(image, vec2<i32>(id.xy), 0) * 255.0));
                let start = (id.y * width + id.x) * 4u;
                output[start] = pixel.x; output[start+1u] = pixel.y;
                output[start+2u] = pixel.z; output[start+3u] = pixel.w;
            }"
            .into(),
        ),
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: None,
        layout: None,
        module: &shader,
        entry_point: Some("main"),
        compilation_options: Default::default(),
        cache: None,
    });
    // All fixture pages are 4x4 RGBA; storage readback expands bytes to u32.
    assert_eq!(page.bytes, 64);
    let output = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: 256,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: 256,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &pipeline.get_bind_group_layout(0),
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&page._raw),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: output.as_entire_binding(),
            },
        ],
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_compute_pass(&Default::default());
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &group, &[]);
        pass.dispatch_workgroups(4, 4, 1);
    }
    encoder.copy_buffer_to_buffer(&output, 0, &readback, 0, 256);
    queue.submit([encoder.finish()]);
    let (mapped, ready) = std::sync::mpsc::channel();
    readback
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| {
            let _ = mapped.send(result);
        });
    device
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: Some(Duration::from_secs(5)),
        })
        .unwrap();
    ready.recv_timeout(Duration::from_secs(5)).unwrap().unwrap();
    let read = readback.slice(..).get_mapped_range();
    let result = read
        .chunks_exact(4)
        .map(|bytes| u8::try_from(u32::from_ne_bytes(bytes.try_into().unwrap())).unwrap())
        .collect();
    drop(read);
    readback.unmap();
    result
}
