use super::*;
use crate::bridge::{archive::PreviewArchiveSource, materials::MaterialResolver};
use crate::test_support::{TestDir, fixture_vtf_bytes};
use std::time::{Duration, Instant};

#[test]
fn hidden_material_pages_are_not_demanded_even_if_a_texture_descriptor_is_retained() {
    let resolver = MaterialResolver::prop_lighting_fixture(
        vec![
            (
                "materials/test/visible.vmt",
                br#"VertexLitGeneric { "$basetexture" "test/image" }"#.to_vec(),
            ),
            (
                "materials/test/image.vtf",
                fixture_vtf_bytes(1, 1, vformats::vtf::VtfFormat::Rgba8888, &[&[1, 2, 3, 255]]),
            ),
        ],
        Vec::new(),
    )
    .with_on_demand_textures();
    let material = resolver
        .resolve_with_base2(&[], "test/visible")
        .into_material()
        .unwrap();
    let mut slot = MaterialSlot {
        resolution: Default::default(),
        canonical_name: None,
        name: "test/visible".into(),
        shader: material.shader,
        texture: material.texture,
        texture2: None,
        render_mode: super::super::RenderMode::Opaque,
        alpha_policy: super::super::RenderMode::Opaque.texture_alpha_policy(),
    };
    let frames = MaterialFrames::default();
    assert_eq!(material_pages(&slot, &frames).len(), 1);
    slot.render_mode = super::super::RenderMode::NoDraw;
    assert!(material_pages(&slot, &frames).is_empty());
    assert!(slot.texture.is_some());
}

#[test]
#[ignore = "requires an exclusively reserved Vulkan GPU"]
fn normalized_page_policy_applies_before_shared_gpu_hits() {
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
        backends: wgpu::Backends::VULKAN,
        ..Default::default()
    });
    let adapter = iced::futures::executor::block_on(
        instance.request_adapter(&wgpu::RequestAdapterOptions::default()),
    )
    .expect("Vulkan adapter");
    let (device, queue) = iced::futures::executor::block_on(
        adapter.request_device(&wgpu::DeviceDescriptor::default()),
    )
    .expect("Vulkan device");
    let files = TestDir::new("gmpublished-page-policy");
    let bytes = fixture_vtf_bytes(2, 2, vformats::vtf::VtfFormat::Dxt1, &[&[0; 8]]);
    let path = files.file("materials/policy.vtf", &bytes);
    let vmt = "UnlitGeneric { $basetexture policy }";
    let vmt_path = files.file("materials/policy.vmt", vmt);
    let resolver = MaterialResolver::new(
        PreviewArchiveSource::from_folder([
            ("materials/policy.vtf".into(), bytes.len() as u64, path),
            ("materials/policy.vmt".into(), vmt.len() as u64, vmt_path),
        ]),
        None,
    )
    .with_on_demand_textures();
    let material = resolver
        .resolve_with_base2(&[], "policy")
        .into_material()
        .expect("material");
    let texture = material.texture.expect("texture");
    let (source, recipe) = texture.on_demand().expect("reloadable source");
    let mut recipe = recipe.clone();
    recipe.storage = crate::bridge::materials::TextureStorage::Bc(vformats::vtf::BcFormat::Bc1);
    recipe.limits.resident_bytes = 64;
    recipe.limits.decode_working_bytes = 64;
    let key = PageKey {
        request: recipe,
        transfer: TextureReadTransfer::SrgbToLinear,
    };
    let need = (key.clone(), Arc::clone(source));
    let resources =
        RenderResources::new(&device, &queue, wgpu::TextureFormat::Rgba8UnormSrgb, None);
    let mut cache = BindingCache::new(None);
    let deadline = Instant::now() + Duration::from_secs(10);
    while !cache
        .prepare_pages(
            &device,
            &queue,
            &resources.texture_uploader,
            std::slice::from_ref(&need),
        )
        .unwrap()
    {
        assert!(Instant::now() < deadline, "page did not become resident");
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(cache.pages[&key].image.as_ref().unwrap().byte_len(), 16);
    let mut strict = key.clone();
    strict.request.limits.resident_bytes = 8;
    assert_eq!(
        source
            .validate_page(&strict.request)
            .unwrap()
            .resident_bytes,
        8
    );
    assert!(
        cache
            .images
            .lookup_page(&strict.request, strict.transfer)
            .is_some()
    );
    let error = cache
        .prepare_pages(
            &device,
            &queue,
            &resources.texture_uploader,
            &[(strict, Arc::clone(source))],
        )
        .unwrap_err();
    assert!(error.contains("16"), "{error}");
    let mut strict = key;
    strict.request.limits.decode_working_bytes = 23;
    let error = cache
        .prepare_pages(
            &device,
            &queue,
            &resources.texture_uploader,
            &[(strict, Arc::clone(source))],
        )
        .unwrap_err();
    assert!(error.contains("24"), "{error}");
    assert!(
        cache
            .prepare_pages(&device, &queue, &resources.texture_uploader, &[need])
            .unwrap()
    );
    assert_eq!(cache.stats().allocations, 1);
}

fn rgba_page(rgb: [u8; 3]) -> (TestDir, PageNeed) {
    let files = TestDir::new("gmpublished-warm-page");
    let payload = [rgb[0], rgb[1], rgb[2], 255].repeat(16);
    let bytes = fixture_vtf_bytes(4, 4, vformats::vtf::VtfFormat::Rgba8888, &[&payload]);
    let path = files.file("materials/warm.vtf", &bytes);
    let vmt = "UnlitGeneric { $basetexture warm }";
    let vmt_path = files.file("materials/warm.vmt", vmt);
    let resolver = MaterialResolver::new(
        PreviewArchiveSource::from_folder([
            ("materials/warm.vtf".into(), bytes.len() as u64, path),
            ("materials/warm.vmt".into(), vmt.len() as u64, vmt_path),
        ]),
        None,
    )
    .with_on_demand_textures();
    let material = resolver
        .resolve_with_base2(&[], "warm")
        .into_material()
        .unwrap();
    let texture = material.texture.unwrap();
    let (source, recipe) = texture.on_demand().unwrap();
    (
        files,
        (
            PageKey {
                request: recipe.clone(),
                transfer: TextureReadTransfer::SrgbToLinear,
            },
            Arc::clone(source),
        ),
    )
}

fn prepare_until_ready(
    cache: &mut BindingCache,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    uploader: &super::super::texture_upload::SharedUploader,
    needs: &[PageNeed],
) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !cache.prepare_pages(device, queue, uploader, needs).unwrap() {
        assert!(Instant::now() < deadline, "warm page preparation stalled");
        device.poll(wgpu::PollType::Poll).unwrap();
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
#[ignore = "requires an exclusively reserved Vulkan GPU"]
fn warm_pages_evict_physical_aliases_once_and_wait_for_real_retirement() {
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
    let uploader = super::super::texture_upload::TextureUploader::shared(
        &device,
        &queue,
        super::super::texture_upload::UploadPolicy {
            sampled_image_bytes: 128,
            sampled_image_allocations: 2,
            ..Default::default()
        },
    )
    .unwrap();
    let (_a_files, a) = rgba_page([224, 32, 16]);
    let (_b_files, b) = rgba_page([16, 224, 32]);
    let (_c_files, c) = rgba_page([32, 16, 224]);
    let mut alias = a.clone();
    alias.0.request.limits.resident_bytes = 64;
    alias.0.request.limits.decode_working_bytes = 64;
    let mut cache = BindingCache::new(None);
    cache
        .open_sources
        .extend([a.1.source_id(), b.1.source_id(), c.1.source_id()]);
    cache.demand_pages(&device, vec![a.clone(), alias.clone()]);
    prepare_until_ready(
        &mut cache,
        &device,
        &queue,
        &uploader,
        &[a.clone(), alias.clone()],
    );
    let held = Arc::clone(cache.pages[&a.0].image.as_ref().unwrap());
    assert!(Arc::ptr_eq(
        &held,
        cache.pages[&alias.0].image.as_ref().unwrap()
    ));
    cache.demand_pages(&device, vec![b.clone()]);
    prepare_until_ready(
        &mut cache,
        &device,
        &queue,
        &uploader,
        std::slice::from_ref(&b),
    );
    cache.demand_pages(&device, Vec::new());
    assert_eq!(cache.warm.stats()["allocations"], 2);
    assert_eq!(cache.warm.stats()["logical_entries"], 3);
    cache.demand_pages(&device, vec![c.clone()]);
    let deadline = Instant::now() + Duration::from_secs(10);
    while !cache.capacity_blocked() {
        assert!(
            !cache
                .prepare_pages(&device, &queue, &uploader, std::slice::from_ref(&c))
                .unwrap()
        );
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(cache.warm.stats()["evictions"], 1);
    assert_eq!(cache.warm.stats()["allocations"], 1);
    assert!(
        cache
            .images
            .lookup_page(&a.0.request, a.0.transfer)
            .is_none()
    );
    assert!(
        cache
            .images
            .lookup_page(&alias.0.request, alias.0.transfer)
            .is_none()
    );
    assert_eq!(uploader.lock().unwrap().sampled_usage(), (128, 2));
    for _ in 0..3 {
        assert!(
            !cache
                .prepare_pages(&device, &queue, &uploader, std::slice::from_ref(&c))
                .unwrap()
        );
        assert_eq!(
            cache.warm.stats()["evictions"],
            1,
            "pending retirement must not drain other warm allocations"
        );
    }
    drop(held);
    let submission = queue.submit([]);
    device
        .poll(wgpu::PollType::Wait {
            submission_index: Some(submission),
            timeout: Some(Duration::from_secs(10)),
        })
        .unwrap();
    prepare_until_ready(
        &mut cache,
        &device,
        &queue,
        &uploader,
        std::slice::from_ref(&c),
    );
    assert_eq!(cache.stats().uploads, 3);
    assert_eq!(uploader.lock().unwrap().sampled_usage(), (128, 2));
    cache.demand_pages(&device, vec![c.clone(), b.clone()]);
    prepare_until_ready(
        &mut cache,
        &device,
        &queue,
        &uploader,
        &[c.clone(), b.clone()],
    );
    assert_eq!(
        cache.stats().uploads,
        3,
        "later-view warm demand is pinned before first-view admission"
    );
    assert_eq!(cache.warm.stats()["hits"], 1);
    assert_eq!(cache.warm.stats()["allocations"], 0);
    assert!(
        cache.pressure_for(&uploader).is_none(),
        "warm history is not part of demanded pressure"
    );
    cache.demand_pages(&device, Vec::new());
    assert_eq!(cache.warm.stats()["allocations"], 2);
    cache.demand(&device, &[]);
    assert_eq!(
        cache.warm.stats()["allocations"],
        0,
        "closed widgets retire images even while provider metadata is held"
    );
}
