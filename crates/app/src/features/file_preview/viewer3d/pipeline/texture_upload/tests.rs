use super::*;
use vformats::vtf::BcFormat;

#[test]
fn declared_batch_is_lowered_only_by_aggregate_device_and_host_limits() {
    let policy = UploadPolicy::default();
    assert_eq!(batch_capacity(policy, u64::MAX).unwrap(), 8 * 1024 * 1024);
    assert_eq!(batch_capacity(policy, 1023).unwrap(), 768);
    assert_eq!(
        batch_capacity(
            UploadPolicy {
                aggregate_staging_bytes: 513,
                batch_bytes: 8192,
                ..Default::default()
            },
            4096
        )
        .unwrap(),
        512
    );
    assert!(batch_capacity(policy, 255).is_err());
    assert!(
        batch_capacity(
            UploadPolicy {
                aggregate_staging_bytes: 0,
                batch_bytes: 8192,
                ..Default::default()
            },
            4096
        )
        .is_err()
    );
}

#[test]
fn copy_tiles_cover_odd_and_narrow_images_once_with_bounded_padded_rows() {
    for dimensions in [[37, 19], [1, 512], [512, 1], [2, 2]] {
        let bytes = vec![11; byte_len(TextureStorage::Rgba8, dimensions).unwrap() as usize];
        let plan = CopyPlan::new(
            Input {
                storage: TextureStorage::Rgba8,
                dimensions,
                bytes: &bytes,
            },
            TextureStorage::Rgba8,
            512,
        )
        .unwrap();
        let mut hits = vec![0_u8; (dimensions[0] * dimensions[1]) as usize];
        for tile in plan.tiles() {
            assert!(tile.bytes <= 512);
            assert_eq!(tile.row_pitch % 256, 0);
            assert_eq!(
                tile.bytes,
                u64::from(tile.row_pitch) * u64::from(tile.block_count[1])
            );
            for y in tile.region[1]..tile.region[1] + tile.region[3] {
                for x in tile.region[0]..tile.region[0] + tile.region[2] {
                    hits[(y * dimensions[0] + x) as usize] += 1;
                }
            }
        }
        assert!(hits.iter().all(|&count| count == 1));
    }
}

#[test]
fn native_bc_copies_preserve_physical_blocks_and_fallback_bands_preserve_pixels() {
    for format in [BcFormat::Bc1, BcFormat::Bc2, BcFormat::Bc3] {
        let dimensions = [7, 9];
        let storage = TextureStorage::Bc(format);
        let bytes = (0..byte_len(storage, dimensions).unwrap())
            .map(|value| value as u8)
            .collect::<Vec<_>>();
        let input = Input {
            storage,
            dimensions,
            bytes: &bytes,
        };
        let native = CopyPlan::new(input, storage, 256).unwrap();
        let tiles = native.tiles().collect::<Vec<_>>();
        assert!(
            tiles
                .iter()
                .all(|tile| tile.region.iter().all(|value| value % 4 == 0))
        );
        assert_eq!(
            tiles
                .iter()
                .map(|tile| tile.block_count[0] * tile.block_count[1])
                .sum::<u32>(),
            6
        );
        let fallback = CopyPlan::new(input, TextureStorage::Rgba8, 256).unwrap();
        let full = vformats::vtf::decode_bc_rgba(
            format,
            dimensions[0],
            dimensions[1],
            &bytes,
            &vformats::Limits::default(),
        )
        .unwrap();
        let mut assembled = vec![0; full.rgba.len()];
        for tile in fallback.tiles() {
            let mut padded = vec![211; tile.bytes as usize];
            vformats::vtf::decode_bc_rgba_region_into(
                format,
                dimensions,
                tile.region,
                &bytes,
                &mut padded,
                tile.row_pitch as usize,
            )
            .unwrap();
            for row in 0..tile.region[3] as usize {
                let dst = ((tile.region[1] as usize + row) * dimensions[0] as usize
                    + tile.region[0] as usize)
                    * 4;
                let count = tile.region[2] as usize * 4;
                assembled[dst..dst + count].copy_from_slice(
                    &padded[row * tile.row_pitch as usize..row * tile.row_pitch as usize + count],
                );
            }
        }
        assert_eq!(assembled, full.rgba);
    }
}

#[test]
fn malformed_sources_and_invalid_policies_fail_before_any_copy_plan() {
    let input = Input {
        storage: TextureStorage::Rgba8,
        dimensions: [4, 4],
        bytes: &[0; 63],
    };
    assert!(matches!(
        CopyPlan::new(input, TextureStorage::Rgba8, 256),
        Err(UploadError::InvalidLayout)
    ));
    let input = Input {
        storage: TextureStorage::Rgba8,
        dimensions: [1, 1],
        bytes: &[0; 4],
    };
    assert!(matches!(
        CopyPlan::new(input, TextureStorage::Rgba8, 255),
        Err(UploadError::InvalidPolicy)
    ));
    assert!(matches!(
        CopyPlan::new(input, TextureStorage::Bc(BcFormat::Bc1), 256),
        Err(UploadError::InvalidLayout)
    ));
    assert_eq!(byte_len(TextureStorage::Rgba8, [u32::MAX, u32::MAX]), None);
    assert!(matches!(
        CopyPlan::new(
            Input {
                storage: TextureStorage::Rgba8,
                dimensions: [u32::MAX, u32::MAX],
                bytes: &[]
            },
            TextureStorage::Rgba8,
            256
        ),
        Err(UploadError::Overflow)
    ));
}

#[test]
fn completion_owned_permits_outlive_submission_and_image_owners() {
    let ledger = Arc::new(Ledger::default());
    let permit = StagingPermit::new(&ledger, 256);
    let completion = move || drop(permit);
    assert_eq!(ledger.staging.load(Ordering::Acquire), 256);
    assert_eq!(ledger.peak_staging.load(Ordering::Acquire), 256);
    completion();
    assert_eq!(ledger.staging.load(Ordering::Acquire), 0);
    let pending = StagingPermit::new(&ledger, 512);
    assert_eq!(ledger.peak_staging.load(Ordering::Acquire), 512);
    drop(pending);
    assert_eq!(ledger.staging.load(Ordering::Acquire), 0);
}

#[test]
fn cached_staging_remains_charged_and_excess_or_closed_returns_release_ownership() {
    let ledger = Arc::new(Ledger::default());
    let failed = AtomicBool::new(false);
    let (sender, receiver) = mpsc::sync_channel(2);
    let mut submitted: Vec<_> = (0..3)
        .map(|id| Some((id, StagingPermit::new(&ledger, 256))))
        .collect();
    for index in [2, 0, 1] {
        return_staging(submitted[index].take().unwrap(), &Ok(()), &sender, &failed);
    }
    assert_eq!(ledger.peak_staging.load(Ordering::Acquire), 768);
    assert_eq!(ledger.staging.load(Ordering::Acquire), 512);
    assert!(!failed.load(Ordering::Acquire));
    let reused = receiver.try_recv().unwrap();
    assert_eq!(reused.0, 2);
    assert_eq!(ledger.staging.load(Ordering::Acquire), 512);
    drop(reused);
    assert_eq!(ledger.staging.load(Ordering::Acquire), 256);
    drop(receiver);
    assert_eq!(ledger.staging.load(Ordering::Acquire), 0);
    return_staging(
        (3, StagingPermit::new(&ledger, 256)),
        &Ok(()),
        &sender,
        &failed,
    );
    assert_eq!(ledger.staging.load(Ordering::Acquire), 0);
    assert!(!failed.load(Ordering::Acquire));
}

#[test]
fn failed_remapping_is_reported_and_does_not_publish_or_replace_ready_storage() {
    let ledger = Arc::new(Ledger::default());
    let failed = AtomicBool::new(false);
    let (sender, receiver) = mpsc::sync_channel(1);
    return_staging(StagingPermit::new(&ledger, 256), &Ok(()), &sender, &failed);
    return_staging(
        StagingPermit::new(&ledger, 512),
        &Err(wgpu::BufferAsyncError),
        &sender,
        &failed,
    );
    assert!(failed.load(Ordering::Acquire));
    assert_eq!(ledger.staging.load(Ordering::Acquire), 256);
    assert_eq!(receiver.try_recv().unwrap().bytes, 256);
    assert_eq!(ledger.staging.load(Ordering::Acquire), 0);
    assert!(receiver.try_recv().is_err());
}

#[test]
fn staging_idle_retention_is_limited_by_slots_bytes_and_the_original_aggregate_budget() {
    let mib = 1024 * 1024;
    assert_eq!(staging_idle_limit(8 * mib, 64 * mib), 2);
    assert_eq!(staging_idle_limit(8 * mib, 8 * mib), 1);
    assert_eq!(staging_idle_limit(16 * mib, 64 * mib), 1);
    assert_eq!(staging_idle_limit(32 * mib, 64 * mib), 0);
    assert_eq!(staging_idle_limit(256, 512), 2);
    assert_eq!(staging_idle_limit(256, 256), 1);
}

#[test]
#[ignore = "requires an exclusively reserved Vulkan GPU"]
fn gpu_bc_fallback_all_mips_frames_cube_views_and_pressure_are_bounded() {
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
        backends: wgpu::Backends::VULKAN,
        ..Default::default()
    });
    let adapter = iced::futures::executor::block_on(
        instance.request_adapter(&wgpu::RequestAdapterOptions::default()),
    )
    .unwrap();
    let (device, queue) = iced::futures::executor::block_on(
        adapter.request_device(&wgpu::DeviceDescriptor::default()),
    )
    .unwrap();
    let _completion_scope =
        crate::features::file_preview::viewer3d::pipeline::completion::TestDeviceScope::new(
            &device,
        );
    assert!(
        !device
            .features()
            .contains(wgpu::Features::TEXTURE_COMPRESSION_BC)
    );
    gpu_bc_roundtrips(&device, &queue, false);
}

#[test]
#[ignore = "requires an exclusively reserved BC-capable Vulkan GPU"]
fn gpu_native_bc_all_mips_frames_and_views_preserve_blocks() {
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
        backends: wgpu::Backends::VULKAN,
        ..Default::default()
    });
    let adapter = iced::futures::executor::block_on(
        instance.request_adapter(&wgpu::RequestAdapterOptions::default()),
    )
    .unwrap();
    assert!(
        adapter
            .features()
            .contains(wgpu::Features::TEXTURE_COMPRESSION_BC)
    );
    let (device, queue) =
        iced::futures::executor::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            required_features: wgpu::Features::TEXTURE_COMPRESSION_BC,
            ..Default::default()
        }))
        .unwrap();
    let _completion_scope =
        crate::features::file_preview::viewer3d::pipeline::completion::TestDeviceScope::new(
            &device,
        );
    gpu_bc_roundtrips(&device, &queue, true);
}

fn gpu_bc_roundtrips(device: &wgpu::Device, queue: &wgpu::Queue, native: bool) {
    for aggregate_staging_bytes in [256, 512] {
        gpu_bc_roundtrips_with_budget(device, queue, native, aggregate_staging_bytes);
    }
}

fn gpu_bc_roundtrips_with_budget(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    native: bool,
    aggregate_staging_bytes: u64,
) {
    for format in [BcFormat::Bc1, BcFormat::Bc2, BcFormat::Bc3] {
        let uploader = TextureUploader::shared(
            device,
            queue,
            UploadPolicy {
                aggregate_staging_bytes,
                batch_bytes: 256,
                ..Default::default()
            },
        )
        .unwrap();
        let output_storage = if native {
            TextureStorage::Bc(format)
        } else {
            TextureStorage::Rgba8
        };
        let texture_format = if native {
            match format {
                BcFormat::Bc1 => wgpu::TextureFormat::Bc1RgbaUnorm,
                BcFormat::Bc2 => wgpu::TextureFormat::Bc2RgbaUnorm,
                BcFormat::Bc3 => wgpu::TextureFormat::Bc3RgbaUnorm,
            }
        } else {
            wgpu::TextureFormat::Rgba8Unorm
        };
        let texture = uploader
            .lock()
            .unwrap()
            .allocate_sampled(&wgpu::TextureDescriptor {
                label: Some("bounded fallback fixture"),
                size: wgpu::Extent3d {
                    width: 8,
                    height: 8,
                    depth_or_array_layers: 12,
                },
                mip_level_count: 4,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: texture_format,
                usage: wgpu::TextureUsages::COPY_DST
                    | wgpu::TextureUsages::COPY_SRC
                    | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            })
            .unwrap();
        let mut expected = Vec::new();
        for layer in 0..12 {
            for mip in 0..4 {
                let side = (8_u32 >> mip).max(1);
                let bytes = (0..byte_len(TextureStorage::Bc(format), [side, side]).unwrap())
                    .map(|index| (index as u8).wrapping_add((layer * 13 + mip * 17) as u8))
                    .collect::<Vec<_>>();
                expected.push(if native {
                    bytes.clone()
                } else {
                    vformats::vtf::decode_bc_rgba(
                        format,
                        side,
                        side,
                        &bytes,
                        &vformats::Limits::default(),
                    )
                    .unwrap()
                    .rgba
                });
                uploader
                    .lock()
                    .unwrap()
                    .write(
                        &texture,
                        mip,
                        wgpu::Origin3d {
                            x: 0,
                            y: 0,
                            z: layer,
                        },
                        Input {
                            storage: TextureStorage::Bc(format),
                            dimensions: [side, side],
                            bytes: &bytes,
                        },
                    )
                    .unwrap();
            }
        }
        if !native {
            let patch = [33, 66, 99, 123].repeat(9);
            let before_invalid = uploader.lock().unwrap().stats();
            assert!(matches!(
                uploader.lock().unwrap().write(
                    &texture,
                    0,
                    wgpu::Origin3d { x: 7, y: 7, z: 5 },
                    Input {
                        storage: TextureStorage::Rgba8,
                        dimensions: [3, 3],
                        bytes: &patch
                    }
                ),
                Err(UploadError::InvalidLayout)
            ));
            assert_eq!(
                uploader.lock().unwrap().stats().copied_bytes,
                before_invalid.copied_bytes
            );
            uploader
                .lock()
                .unwrap()
                .write(
                    &texture,
                    0,
                    wgpu::Origin3d { x: 2, y: 2, z: 5 },
                    Input {
                        storage: TextureStorage::Rgba8,
                        dimensions: [3, 3],
                        bytes: &patch,
                    },
                )
                .unwrap();
            for y in 2..5 {
                for x in 2..5 {
                    expected[5 * 4][(y * 8 + x) * 4..(y * 8 + x + 1) * 4]
                        .copy_from_slice(&[33, 66, 99, 123]);
                }
            }
        }
        uploader.lock().unwrap().flush();
        for first in [0, 6] {
            let _view = texture.create_view(&wgpu::TextureViewDescriptor {
                dimension: Some(wgpu::TextureViewDimension::Cube),
                base_array_layer: first,
                array_layer_count: Some(6),
                ..Default::default()
            });
        }
        let _array = texture.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        let mut outputs = Vec::new();
        for layer in 0..12 {
            for mip in 0..4 {
                let side = (8_u32 >> mip).max(1);
                let (block, block_bytes) = block(output_storage);
                let rows = side.div_ceil(block);
                let row_bytes = side.div_ceil(block) * block_bytes;
                let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("fallback readback"),
                    size: u64::from(256 * rows),
                    usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                encoder.copy_texture_to_buffer(
                    wgpu::TexelCopyTextureInfo {
                        texture: &texture,
                        mip_level: mip,
                        origin: wgpu::Origin3d {
                            x: 0,
                            y: 0,
                            z: layer,
                        },
                        aspect: wgpu::TextureAspect::All,
                    },
                    wgpu::TexelCopyBufferInfo {
                        buffer: &buffer,
                        layout: wgpu::TexelCopyBufferLayout {
                            offset: 0,
                            bytes_per_row: Some(256),
                            rows_per_image: Some(rows),
                        },
                    },
                    wgpu::Extent3d {
                        width: side.div_ceil(block) * block,
                        height: rows * block,
                        depth_or_array_layers: 1,
                    },
                );
                outputs.push((buffer, row_bytes));
            }
        }
        queue.submit([encoder.finish()]);
        for (buffer, _) in &outputs {
            buffer
                .slice(..)
                .map_async(wgpu::MapMode::Read, |result| result.unwrap());
        }
        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        for ((buffer, row_bytes), expected) in outputs.iter().zip(expected) {
            let read = buffer.slice(..).get_mapped_range();
            let actual = read
                .chunks_exact(256)
                .flat_map(|row| row[..(*row_bytes as usize)].iter().copied())
                .collect::<Vec<_>>();
            assert_eq!(actual, expected);
            drop(read);
            buffer.unmap();
        }
        let stats = uploader.lock().unwrap().stats();
        println!(
            "BC {format:?} native={native}: {}",
            serde_json::to_string(&stats).unwrap()
        );
        assert!(stats.submissions > 1);
        assert!(stats.peak_staging_bytes <= aggregate_staging_bytes);
        assert!(stats.staging_reuses > 0);
        assert!(stats.staging_allocations <= aggregate_staging_bytes / 256);
        assert!(stats.staging_bytes > 0);
        assert!(stats.staging_bytes <= aggregate_staging_bytes);
        assert_eq!(stats.owned_image_allocations, 1);
        assert_eq!(
            stats.owned_image_texel_bytes,
            (0..4)
                .map(
                    |mip| byte_len(output_storage, [(8 >> mip).max(1), (8 >> mip).max(1)]).unwrap()
                )
                .sum::<u64>()
                * 12
        );
        assert_eq!(stats.sampled_image_bytes, stats.owned_image_texel_bytes);
        drop(texture);
        assert_eq!(uploader.lock().unwrap().stats().owned_image_allocations, 1);
        drop(_array);
        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        device.poll(wgpu::PollType::Poll).unwrap();
        let retired = uploader.lock().unwrap().stats();
        assert_eq!(retired.owned_image_allocations, 0);
        assert_eq!(retired.sampled_image_bytes, 0);
        let ledger = Arc::clone(&uploader.lock().unwrap().ledger);
        drop(uploader);
        assert_eq!(ledger.staging.load(Ordering::Acquire), 0);
    }
}

#[test]
#[ignore = "requires an exclusively reserved Vulkan GPU"]
fn gpu_cancel_unsubmitted_batch_releases_staging_and_texture_ownership() {
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
        backends: wgpu::Backends::VULKAN,
        ..Default::default()
    });
    let adapter = iced::futures::executor::block_on(
        instance.request_adapter(&wgpu::RequestAdapterOptions::default()),
    )
    .unwrap();
    let (device, queue) = iced::futures::executor::block_on(
        adapter.request_device(&wgpu::DeviceDescriptor::default()),
    )
    .unwrap();
    let _completion_scope =
        crate::features::file_preview::viewer3d::pipeline::completion::TestDeviceScope::new(
            &device,
        );
    let uploader = TextureUploader::shared(
        &device,
        &queue,
        UploadPolicy {
            aggregate_staging_bytes: 512,
            batch_bytes: 256,
            ..Default::default()
        },
    )
    .unwrap();
    let texture = uploader
        .lock()
        .unwrap()
        .allocate(&wgpu::TextureDescriptor {
            label: Some("cancel fixture"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        })
        .unwrap();
    uploader
        .lock()
        .unwrap()
        .write(
            &texture,
            0,
            wgpu::Origin3d::ZERO,
            Input {
                storage: TextureStorage::Rgba8,
                dimensions: [1, 1],
                bytes: &[1, 2, 3, 4],
            },
        )
        .unwrap();
    let ledger = Arc::clone(&uploader.lock().unwrap().ledger);
    drop(texture);
    assert_eq!(ledger.owned_images.load(Ordering::Acquire), 1);
    assert_eq!(ledger.staging.load(Ordering::Acquire), 256);
    drop(uploader);
    // A concurrent completion worker may already own the callback when a main
    // thread poll returns. Observe real retirement without another render/poll.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while ledger.owned_images.load(Ordering::Acquire) != 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "image retirement stalled"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(ledger.owned_images.load(Ordering::Acquire), 0);
    assert_eq!(ledger.staging.load(Ordering::Acquire), 0);
    assert_eq!(ledger.submissions.load(Ordering::Acquire), 0);
}

#[test]
#[ignore = "requires an exclusively reserved Vulkan GPU"]
fn gpu_scoped_image_lease_batches_preserve_uploads_pressure_and_cancellation() {
    use super::super::images::{ImageCache, ImageView};
    use crate::bridge::materials::{DecodedTextureFrames, ResolvedTextureMip, TextureReadTransfer};

    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
        backends: wgpu::Backends::VULKAN,
        ..Default::default()
    });
    let adapter = iced::futures::executor::block_on(
        instance.request_adapter(&wgpu::RequestAdapterOptions::default()),
    )
    .unwrap();
    let (device, queue) = iced::futures::executor::block_on(
        adapter.request_device(&wgpu::DeviceDescriptor::default()),
    )
    .unwrap();
    let _completion_scope = super::super::completion::TestDeviceScope::new(&device);
    let frames = |pixel: [u8; 4]| {
        DecodedTextureFrames::from_rgba(
            0,
            vec![vec![ResolvedTextureMip {
                rgba: pixel.to_vec(),
                width: 1,
                height: 1,
            }]],
        )
        .unwrap()
    };
    let uploader = |bytes| {
        TextureUploader::shared(
            &device,
            &queue,
            UploadPolicy {
                aggregate_staging_bytes: bytes,
                batch_bytes: bytes,
                ..Default::default()
            },
        )
        .unwrap()
    };

    let pressure = uploader(512);
    let mut cache = ImageCache::default();
    let mut views = Vec::new();
    {
        let batch = UploadLeaseBatch::new(&pressure);
        for value in 1..=5 {
            let source = frames([value, 32, 96, 255]);
            let mut lease = cache.lease_batched(&batch);
            views.push(
                lease
                    .view(
                        &device,
                        &queue,
                        source.resident().unwrap(),
                        TextureReadTransfer::Unmodified,
                        ImageView::D2(0),
                    )
                    .unwrap(),
            );
            assert!(lease.finish().1.is_empty());
        }
        assert_eq!(pressure.lock().unwrap().stats().submissions, 2);
    }
    let stats = pressure.lock().unwrap().stats();
    assert_eq!(stats.submissions, 3);
    assert!(stats.peak_staging_bytes <= 512);
    for (value, view) in (1..=5).zip(&views) {
        assert_eq!(
            read_image_pixel(&device, &queue, view),
            [value, 32, 96, 255]
        );
    }

    let standalone = uploader(1024);
    let mut cache = ImageCache::default();
    let first = frames([17, 31, 47, 255]);
    let second = frames([59, 73, 89, 255]);
    let batch = UploadLeaseBatch::new(&standalone);
    let mut grouped = cache.lease_batched(&batch);
    let first_view = grouped
        .view(
            &device,
            &queue,
            first.resident().unwrap(),
            TextureReadTransfer::Unmodified,
            ImageView::D2(0),
        )
        .unwrap();
    grouped.finish();
    assert_eq!(standalone.lock().unwrap().stats().submissions, 0);
    let mut separate = cache.lease(&standalone);
    let second_view = separate
        .view(
            &device,
            &queue,
            second.resident().unwrap(),
            TextureReadTransfer::Unmodified,
            ImageView::D2(0),
        )
        .unwrap();
    separate.finish();
    assert_eq!(standalone.lock().unwrap().stats().submissions, 1);
    drop(batch);
    assert_eq!(standalone.lock().unwrap().stats().submissions, 1);
    assert_eq!(
        read_image_pixel(&device, &queue, &first_view),
        [17, 31, 47, 255]
    );
    assert_eq!(
        read_image_pixel(&device, &queue, &second_view),
        [59, 73, 89, 255]
    );

    // Unfinished leases retain pending-copy permits until GPU completion.
    let canceled = uploader(1024);
    let mut cache = ImageCache::default();
    let mut retained = None;
    let mut fail_after_uploads = || -> Result<(), ()> {
        let batch = UploadLeaseBatch::new(&canceled);
        let source = frames([101, 113, 127, 255]);
        let mut lease = cache.lease_batched(&batch);
        retained = Some(
            lease
                .view(
                    &device,
                    &queue,
                    source.resident().unwrap(),
                    TextureReadTransfer::Unmodified,
                    ImageView::D2(0),
                )
                .unwrap(),
        );
        lease.finish();
        let source = frames([139, 151, 163, 255]);
        let mut lease = cache.lease_batched(&batch);
        drop(
            lease
                .view(
                    &device,
                    &queue,
                    source.resident().unwrap(),
                    TextureReadTransfer::Unmodified,
                    ImageView::D2(0),
                )
                .unwrap(),
        );
        drop(lease);
        let stats = canceled.lock().unwrap().stats();
        assert_eq!(stats.submissions, 0);
        assert_eq!(stats.owned_image_allocations, 2);
        Err(())
    };
    let result = fail_after_uploads();
    assert!(result.is_err());
    assert_eq!(canceled.lock().unwrap().stats().submissions, 1);
    assert_eq!(
        read_image_pixel(&device, &queue, retained.as_ref().unwrap()),
        [101, 113, 127, 255]
    );
    drop(retained);
    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while canceled.lock().unwrap().stats().owned_image_allocations != 0 {
        assert!(
            Instant::now() < deadline,
            "batched image retirement stalled"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

// Read the sampled image through a separately submitted compute pipeline. This
// exercises visibility to real GPU work without adding COPY_SRC to image uploads.
fn read_image_pixel(device: &wgpu::Device, queue: &wgpu::Queue, view: &UploadedView) -> [u32; 4] {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("scoped image lease readback"),
        source: wgpu::ShaderSource::Wgsl(std::borrow::Cow::Borrowed(
            "@group(0) @binding(0) var image: texture_2d<f32>;
             @group(0) @binding(1) var<storage, read_write> output: array<u32>;
             @compute @workgroup_size(1) fn main() {
                 let pixel = vec4<u32>(round(textureLoad(image, vec2<i32>(0), 0) * 255.0));
                 output[0] = pixel.x; output[1] = pixel.y;
                 output[2] = pixel.z; output[3] = pixel.w;
             }",
        )),
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: None,
        layout: None,
        module: &shader,
        entry_point: Some("main"),
        compilation_options: Default::default(),
        cache: None,
    });
    let output = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: 16,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: 16,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &pipeline.get_bind_group_layout(0),
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(view),
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
        pass.dispatch_workgroups(1, 1, 1);
    }
    encoder.copy_buffer_to_buffer(&output, 0, &readback, 0, 16);
    queue.submit([encoder.finish()]);
    readback
        .slice(..)
        .map_async(wgpu::MapMode::Read, |result| result.unwrap());
    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    let read = readback.slice(..).get_mapped_range();
    let pixel = std::array::from_fn(|index| {
        u32::from_ne_bytes(read[index * 4..index * 4 + 4].try_into().unwrap())
    });
    drop(read);
    readback.unmap();
    pixel
}

#[test]
fn completion_timeout_is_bounded_and_distinguished_from_invalid_submission() {
    assert_eq!(PRESSURE_WAIT_TIMEOUT, Duration::from_secs(30));
    assert!(matches!(
        UploadError::from(wgpu::PollError::Timeout),
        UploadError::CompletionTimeout(duration) if duration == PRESSURE_WAIT_TIMEOUT
    ));
    assert!(matches!(
        UploadError::from(wgpu::PollError::WrongSubmissionIndex(2, 1)),
        UploadError::Poll(wgpu::PollError::WrongSubmissionIndex(2, 1))
    ));
}

#[test]
fn image_cost_counts_actual_storage_all_mips_and_layers() {
    assert_eq!(
        image_texel_bytes(wgpu::TextureFormat::Rgba8Unorm, [8, 8], 4, 12).unwrap(),
        4080
    );
    assert_eq!(
        image_texel_bytes(wgpu::TextureFormat::Rgba8UnormSrgb, [8, 8], 4, 12).unwrap(),
        4080
    );
    assert_eq!(
        image_texel_bytes(wgpu::TextureFormat::Bc1RgbaUnorm, [8, 8], 4, 12).unwrap(),
        672
    );
    assert_eq!(
        image_texel_bytes(wgpu::TextureFormat::Bc3RgbaUnormSrgb, [8, 8], 4, 12).unwrap(),
        1344
    );
    assert!(matches!(
        image_texel_bytes(wgpu::TextureFormat::Rgba8Unorm, [u32::MAX; 2], 1, u32::MAX),
        Err(UploadError::Overflow)
    ));
    assert!(matches!(
        image_texel_bytes(wgpu::TextureFormat::Rgba8Unorm, [1, 1], 2, 1),
        Err(UploadError::InvalidLayout)
    ));
}

#[test]
fn material_admission_is_transactional_and_releases_only_owned_allocations() {
    let policy = UploadPolicy {
        sampled_image_bytes: 100,
        sampled_image_allocations: 2,
        ..Default::default()
    };
    let mut admission = ImageAdmission::default();
    admission.reserve(policy, 60).unwrap();
    assert!(matches!(
        admission.reserve(policy, 41),
        Err(UploadError::ImageBudget { .. })
    ));
    assert_eq!((admission.bytes, admission.allocations), (60, 1));
    admission.reserve(policy, 40).unwrap();
    assert!(matches!(
        admission.reserve(policy, 0),
        Err(UploadError::ImageBudget { .. })
    ));
    assert_eq!(
        (admission.bytes, admission.allocations, admission.rejections),
        (100, 2, 2)
    );
    admission.release(60);
    admission.reserve(policy, 60).unwrap();
    admission.release(40);
    admission.release(60);
    assert_eq!((admission.bytes, admission.allocations), (0, 0));
    assert_eq!((admission.peak_bytes, admission.peak_allocations), (100, 2));
}

#[test]
#[ignore = "requires an exclusively reserved Vulkan GPU"]
fn gpu_final_close_drains_submission_staging_and_image_retirement_without_another_render() {
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
        backends: wgpu::Backends::VULKAN,
        ..Default::default()
    });
    let adapter = iced::futures::executor::block_on(
        instance.request_adapter(&wgpu::RequestAdapterOptions::default()),
    )
    .unwrap();
    let (device, queue) = iced::futures::executor::block_on(
        adapter.request_device(&wgpu::DeviceDescriptor::default()),
    )
    .unwrap();
    let _completion_scope =
        crate::features::file_preview::viewer3d::pipeline::completion::TestDeviceScope::new(
            &device,
        );
    let uploader = TextureUploader::shared(
        &device,
        &queue,
        UploadPolicy {
            aggregate_staging_bytes: 512,
            batch_bytes: 256,
            sampled_image_bytes: 64,
            sampled_image_allocations: 1,
        },
    )
    .unwrap();
    let completion = uploader.lock().unwrap().completion();
    let shared = super::super::completion::CompletionDriver::shared(&device).unwrap();
    assert!(
        Arc::ptr_eq(&completion, &shared),
        "one completion driver per actual device"
    );
    drop(shared);
    let driver_weak = Arc::downgrade(&completion);
    let texture = uploader
        .lock()
        .unwrap()
        .allocate_sampled(&wgpu::TextureDescriptor {
            label: Some("idle retirement fixture"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        })
        .unwrap();
    let ledger = Arc::clone(&uploader.lock().unwrap().ledger);
    uploader
        .lock()
        .unwrap()
        .write(
            &texture,
            0,
            wgpu::Origin3d::ZERO,
            Input {
                storage: TextureStorage::Rgba8,
                dimensions: [1, 1],
                bytes: &[1, 2, 3, 255],
            },
        )
        .unwrap();
    uploader.lock().unwrap().flush();
    queue.submit([]);
    super::super::residency_demand::SubmissionPins::new(vec![Arc::new(texture)])
        .submitted(&queue, &completion);
    drop(uploader);
    drop(completion);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let usage = ledger.sampled_images.lock().unwrap();
        let released = usage.bytes == 0 && usage.allocations == 0;
        drop(usage);
        if released
            && ledger.owned_images.load(Ordering::Acquire) == 0
            && ledger.staging.load(Ordering::Acquire) == 0
            && ledger.retirements.state().pending == 0
            && driver_weak.strong_count() == 0
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "idle completion driver did not release final-close resources"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(ledger.retirements.state().failure, None);
}
