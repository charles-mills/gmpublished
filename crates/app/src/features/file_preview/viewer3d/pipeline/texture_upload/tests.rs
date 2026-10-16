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
                batch_bytes: 8192
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
                batch_bytes: 8192
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
    gpu_bc_roundtrips(&device, &queue, true);
}

fn gpu_bc_roundtrips(device: &wgpu::Device, queue: &wgpu::Queue, native: bool) {
    for format in [BcFormat::Bc1, BcFormat::Bc2, BcFormat::Bc3] {
        let uploader = TextureUploader::shared(
            device,
            queue,
            UploadPolicy {
                aggregate_staging_bytes: 512,
                batch_bytes: 256,
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
            .allocate(&wgpu::TextureDescriptor {
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
        assert!(stats.peak_staging_bytes <= 512);
        assert_eq!(stats.staging_bytes, 0);
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
        drop(texture);
        assert_eq!(uploader.lock().unwrap().stats().owned_image_allocations, 0);
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
    let uploader = TextureUploader::shared(
        &device,
        &queue,
        UploadPolicy {
            aggregate_staging_bytes: 512,
            batch_bytes: 256,
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
    assert_eq!(ledger.owned_images.load(Ordering::Acquire), 0);
    assert_eq!(ledger.staging.load(Ordering::Acquire), 0);
    assert_eq!(ledger.submissions.load(Ordering::Acquire), 0);
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
fn capture_policy_preserves_defaults_and_declared_pressure_overrides() {
    let defaults = capture_policy(None, None).unwrap();
    assert_eq!(defaults.aggregate_staging_bytes, 64 * 1024 * 1024);
    assert_eq!(defaults.batch_bytes, 8 * 1024 * 1024);
    let override_policy = capture_policy(Some("513"), Some("1024")).unwrap();
    assert_eq!(override_policy.aggregate_staging_bytes, 513);
    assert_eq!(override_policy.batch_bytes, 1024);
    assert_eq!(batch_capacity(override_policy, u64::MAX).unwrap(), 512);
    let batch_only = capture_policy(None, Some("256")).unwrap();
    assert_eq!(
        batch_only.aggregate_staging_bytes,
        defaults.aggregate_staging_bytes
    );
    assert_eq!(batch_only.batch_bytes, 256);
}

#[test]
fn capture_policy_rejects_unusable_or_malformed_budgets() {
    for value in [
        "",
        "0",
        "255",
        "-1",
        "1.5",
        "NaN",
        " 256",
        "18446744073709551616",
    ] {
        assert!(
            capture_policy(Some(value), None).is_err(),
            "staging={value}"
        );
        assert!(capture_policy(None, Some(value)).is_err(), "batch={value}");
    }
    let policy = capture_policy(Some("256"), Some("256")).unwrap();
    assert_eq!(batch_capacity(policy, 256).unwrap(), 256);
}
