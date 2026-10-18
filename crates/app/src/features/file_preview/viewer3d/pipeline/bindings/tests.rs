use super::*;
use crate::bridge::{archive::PreviewArchiveSource, materials::MaterialResolver};
use crate::test_support::{TestDir, fixture_vtf_bytes};
use std::time::{Duration, Instant};

fn material_need_fixture() -> MaterialSlot {
    MaterialSlot {
        resolution: Default::default(),
        canonical_name: None,
        name: "need-order".into(),
        shader: crate::bridge::materials::MaterialShader::Surface(Arc::new(Default::default())),
        texture: None,
        texture2: None,
        render_mode: super::super::RenderMode::Opaque,
        alpha_policy: super::super::RenderMode::Opaque.texture_alpha_policy(),
    }
}

#[test]
fn material_need_visitor_preserves_absent_roles_and_stops_at_first_error() {
    use crate::bridge::materials::{MaterialShader, SurfaceFamily};
    use TextureReadTransfer::{SrgbToLinear, Unmodified};
    let mut slot = material_need_fixture();
    let MaterialShader::Surface(surface) = &mut slot.shader else {
        panic!("surface");
    };
    Arc::make_mut(surface).family = SurfaceFamily::DecalModulate;
    let frames = MaterialFrames {
        base: 11,
        base2: 12,
        normal: 13,
        normal2: 14,
        detail: 15,
        environment: 16,
        environment_mask: 17,
    };
    // Base, base2, normal, normal2, detail, blend modulation, emission mask.
    // Even absent roles are visited. DecalModulate's base and detail are raw.
    let expected = vec![
        (11, Unmodified),
        (12, Unmodified),
        (13, Unmodified),
        (14, Unmodified),
        (15, Unmodified),
        (0, Unmodified),
        (0, Unmodified),
    ];
    for visitor in [false, true] {
        let mut seen = Vec::new();
        infallible(visit_material_needs_with_mode(
            &slot,
            &frames,
            visitor,
            |need| {
                assert!(need.resident.is_none() && need.source.is_none());
                seen.push((need.frame, need.transfer));
                Ok(())
            },
        ));
        assert_eq!(seen, expected);
        for error_at in 0..expected.len() {
            let mut seen = Vec::new();
            let result = visit_material_needs_with_mode(&slot, &frames, visitor, |need| {
                seen.push((need.frame, need.transfer));
                if seen.len() == error_at + 1 {
                    Err("first invalid allocation")
                } else {
                    Ok(())
                }
            });
            assert_eq!(result, Err("first invalid allocation"));
            assert_eq!(seen, expected[..=error_at]);
        }
    }
    slot.shader = MaterialShader::Unknown;
    for visitor in [false, true] {
        let mut seen = Vec::new();
        infallible(visit_material_needs_with_mode(
            &slot,
            &frames,
            visitor,
            |need| {
                seen.push((need.frame, need.transfer));
                Ok(())
            },
        ));
        assert_eq!(
            seen,
            [
                (11, SrgbToLinear),
                (12, SrgbToLinear),
                (13, Unmodified),
                (14, Unmodified)
            ]
        );
    }
    slot.render_mode = super::super::RenderMode::NoDraw;
    infallible(visit_material_needs_with_mode(&slot, &frames, true, |_| {
        panic!("NoDraw must not emit texture roles");
    }));
}

#[test]
fn material_need_visitor_emits_exact_surface_sources_frames_and_transfers() {
    use crate::bridge::materials::{
        DecodedTextureFrames, EnvironmentMapMask, EnvironmentMapMaterial, MaterialShader,
        PhongMaterial, PhongTexture, ResolvedCubemap, ResolvedTextureMip, SurfaceFamily,
    };
    use TextureReadTransfer::{SrgbToLinear, Unmodified};
    let data: Vec<_> = (1..=9)
        .map(|value| {
            Arc::new(
                DecodedTextureFrames::from_rgba(
                    0,
                    vec![vec![ResolvedTextureMip {
                        rgba: vec![value, 0, 0, 255],
                        width: 1,
                        height: 1,
                    }]],
                )
                .unwrap(),
            )
        })
        .collect();
    let cube = Arc::new(
        ResolvedCubemap::from_rgba(
            0,
            0,
            std::array::from_fn(|_| {
                vec![ResolvedTextureMip {
                    rgba: vec![10, 0, 0, 255],
                    width: 1,
                    height: 1,
                }]
            }),
        )
        .unwrap(),
    );
    // Obtain private detail/emission field types through real material parsing.
    let resolver = MaterialResolver::prop_lighting_fixture(
        vec![
            (
                "materials/test/roles.vmt",
                br#"VertexLitGeneric {
                "$detail" "test/role" "$detailblendmode" "1"
                "$selfillum" "1" "$selfillummask" "test/role"
            }"#
                .to_vec(),
            ),
            (
                "materials/test/role.vtf",
                fixture_vtf_bytes(
                    1,
                    1,
                    vformats::vtf::VtfFormat::Rgba8888,
                    &[&[255, 255, 255, 255]],
                ),
            ),
        ],
        Vec::new(),
    );
    let parsed = resolver
        .resolve_with_base2(&[], "test/roles")
        .into_material()
        .unwrap();
    let mut slot = material_need_fixture();
    slot.shader = parsed.shader;
    let MaterialShader::Surface(surface) = &mut slot.shader else {
        panic!("surface");
    };
    let surface = Arc::make_mut(surface);
    surface.family = SurfaceFamily::VertexLit;
    surface.normal = Some(Arc::clone(&data[0]));
    surface.normal2 = Some(Arc::clone(&data[1]));
    surface.detail.as_mut().expect("parsed detail role").texture = Some(Arc::clone(&data[2]));
    surface.blend_modulation = Some(Arc::clone(&data[3]));
    surface
        .emission
        .as_mut()
        .expect("parsed emission role")
        .mask = Some(Arc::clone(&data[4]));
    surface.envmap = Some(Arc::new(EnvironmentMapMaterial {
        source: EnvironmentMapSource::Named(Arc::clone(&cube)),
        path: "cube".into(),
        frame: 0,
        tint: [1.0; 3],
        contrast: 0.0,
        saturation: 1.0,
        mask: EnvironmentMapMask::Texture,
        mask_texture: Some(Arc::clone(&data[5])),
        mask_frame: 0,
        mask_transform: [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0]],
        fresnel: 1.0,
    }));
    let phong_texture = |index: usize| {
        Some(PhongTexture {
            path: format!("phong-{index}"),
            texture: Some(Arc::clone(&data[index])),
        })
    };
    surface.phong = Some(Arc::new(PhongMaterial {
        exponent_texture: phong_texture(6),
        diffuse_warp: phong_texture(7),
        specular_warp: phong_texture(8),
        ..Default::default()
    }));
    let frames = MaterialFrames {
        base: 11,
        base2: 12,
        normal: 13,
        normal2: 14,
        detail: 15,
        environment: 16,
        environment_mask: 17,
    };
    let id = |index: usize| Some(data[index].resident().unwrap().id().clone());
    // Explicit role ordering, including the deliberately absent base slots.
    let expected = vec![
        (None, 11, SrgbToLinear),
        (None, 12, SrgbToLinear),
        (id(0), 13, Unmodified),
        (id(1), 14, Unmodified),
        (id(2), 15, SrgbToLinear),
        (id(3), 0, Unmodified),
        (id(4), 0, Unmodified),
        (id(5), 17, Unmodified),
        (id(6), 0, Unmodified),
        (id(7), 0, Unmodified),
        (id(8), 0, Unmodified),
        (
            Some(cube.resident().unwrap().id().clone()),
            16,
            SrgbToLinear,
        ),
    ];
    for visitor in [false, true] {
        let mut seen = Vec::new();
        infallible(visit_material_needs_with_mode(
            &slot,
            &frames,
            visitor,
            |need| {
                assert!(need.source.is_none());
                seen.push((
                    need.resident.map(|source| source.id().clone()),
                    need.frame,
                    need.transfer,
                ));
                Ok(())
            },
        ));
        assert_eq!(seen, expected);
    }
}

#[test]
fn append_material_pages_preserves_full_recipes_duplicates_and_existing_prefix() {
    use crate::bridge::materials::MaterialShader;
    use TextureReadTransfer::{SrgbToLinear, Unmodified};
    let material = br#"WorldVertexTransition {
        "$basetexture" "test/a" "$basetexture2" "test/b"
        "$bumpmap" "test/b" "$detail" "test/a" "$detailblendmode" "1"
    }"#;
    let mut a = fixture_vtf_bytes(
        1,
        1,
        vformats::vtf::VtfFormat::Rgba8888,
        &[&[1, 2, 3, 255].repeat(20)],
    );
    a[24..26].copy_from_slice(&20_u16.to_le_bytes());
    let mut b = a.clone();
    b[64] = 71;
    let resolver = MaterialResolver::prop_lighting_fixture(
        vec![
            ("materials/test/control.vmt", material.to_vec()),
            ("materials/test/a.vtf", a),
            ("materials/test/b.vtf", b),
        ],
        Vec::new(),
    )
    .with_on_demand_textures();
    let material = resolver
        .resolve_with_base2(&[], "test/control")
        .into_material()
        .unwrap();
    let mut slot = material_need_fixture();
    slot.shader = material.shader;
    slot.texture = material.texture;
    slot.texture2 = material.texture2;
    let frames = MaterialFrames {
        base: 11,
        base2: 12,
        normal: 13,
        detail: 15,
        ..Default::default()
    };
    let MaterialShader::Surface(surface) = &slot.shader else {
        panic!("surface");
    };
    let recipe = |(source, request): (&Arc<ReloadableTextureSource>, &TextureFrameRequest),
                  frame,
                  transfer| {
        let mut request = request.clone();
        request.frame = frame;
        (PageKey { request, transfer }, Arc::clone(source))
    };
    let expected = vec![
        recipe(
            slot.texture.as_ref().unwrap().on_demand().unwrap(),
            11,
            SrgbToLinear,
        ),
        recipe(
            slot.texture2.as_ref().unwrap().on_demand().unwrap(),
            12,
            SrgbToLinear,
        ),
        recipe(
            surface.normal.as_ref().unwrap().on_demand().unwrap(),
            13,
            Unmodified,
        ),
        recipe(
            surface
                .detail
                .as_ref()
                .unwrap()
                .texture
                .as_ref()
                .unwrap()
                .on_demand()
                .unwrap(),
            15,
            SrgbToLinear,
        ),
    ];
    for visitor in [false, true] {
        let mut seen = vec![expected[0].clone()];
        append_material_pages_with_mode(&mut seen, &slot, &frames, visitor);
        assert_eq!(seen.len(), expected.len() + 1);
        for (actual, expected) in seen
            .iter()
            .zip(std::iter::once(&expected[0]).chain(&expected))
        {
            assert_eq!(actual.0, expected.0);
            assert!(Arc::ptr_eq(&actual.1, &expected.1));
        }
    }
}

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
    let _completion_scope =
        crate::features::file_preview::viewer3d::pipeline::completion::TestDeviceScope::new(
            &device,
        );
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
    let _completion_scope =
        crate::features::file_preview::viewer3d::pipeline::completion::TestDeviceScope::new(
            &device,
        );
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
    cache.demand_pages(&device, &[a.clone(), alias.clone()]);
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
    cache.demand_pages(&device, std::slice::from_ref(&b));
    prepare_until_ready(
        &mut cache,
        &device,
        &queue,
        &uploader,
        std::slice::from_ref(&b),
    );
    cache.demand_pages(&device, &[]);
    assert_eq!(cache.warm.stats()["allocations"], 2);
    assert_eq!(cache.warm.stats()["logical_entries"], 3);
    cache.demand_pages(&device, std::slice::from_ref(&c));
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
    cache.demand_pages(&device, &[c.clone(), b.clone()]);
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
    cache.demand_pages(&device, &[]);
    assert_eq!(cache.warm.stats()["allocations"], 2);
    cache.demand(&device, &[]);
    assert_eq!(
        cache.warm.stats()["allocations"],
        0,
        "closed widgets retire images even while provider metadata is held"
    );
}

#[test]
fn cached_page_needs_remain_local_to_selected_visibility_and_animation_clock() {
    use crate::bridge::materials::AnimationClock;
    use crate::media::preview_model::{ModelPreview, ModelStats, RenderScene};

    let pixels = [255, 128, 64, 255].repeat(3);
    let mut animated = fixture_vtf_bytes(1, 1, vformats::vtf::VtfFormat::Rgba8888, &[&pixels]);
    animated[24..26].copy_from_slice(&3u16.to_le_bytes());
    let resolver = MaterialResolver::prop_lighting_fixture(
        vec![
            (
                "materials/test/animated.vmt",
                br#"UnlitGeneric {
                    "$basetexture" "test/animated"
                    Proxies { AnimatedTexture {
                        animatedtexturevar "$basetexture"
                        animatedtextureframenumvar "$frame"
                        animatedtextureframerate "1"
                    } }
                }"#
                .to_vec(),
            ),
            ("materials/test/animated.vtf", animated),
            (
                "materials/test/other.vmt",
                br#"UnlitGeneric { "$basetexture" "test/other" }"#.to_vec(),
            ),
            (
                "materials/test/other.vtf",
                fixture_vtf_bytes(1, 1, vformats::vtf::VtfFormat::Rgba8888, &[&[1, 2, 3, 255]]),
            ),
        ],
        Vec::new(),
    )
    .with_on_demand_textures();
    let materials = ["test/animated", "test/other"]
        .into_iter()
        .map(|name| {
            let material = resolver
                .resolve_with_base2(&[], name)
                .into_material()
                .unwrap();
            MaterialSlot {
                resolution: Default::default(),
                canonical_name: None,
                name: name.into(),
                shader: material.shader,
                texture: material.texture,
                texture2: material.texture2,
                render_mode: material.render_mode,
                alpha_policy: material.render_mode.texture_alpha_policy(),
            }
        })
        .collect();
    let source = PreviewScene::Model(Arc::new(ModelPreview {
        model_animations: Vec::new(),
        model_animation_issues: Vec::new(),
        skin_tables: Vec::new(),
        bodygroups: Vec::new(),
        scene: Arc::new(RenderScene {
            meshes: Vec::new(),
            materials,
            phy_debug_meshes: Vec::new(),
            stats: ModelStats {
                bone_count: 0,
                sequence_count: 0,
                vertex_count: 0,
                triangle_count: 0,
                mesh_count: 0,
                material_count: 2,
                resolved_material_count: 2,
            },
            bounds_min: super::super::Vec3::ZERO,
            bounds_max: super::super::Vec3::new(1.0, 1.0, 1.0),
        }),
    }));
    let request = |slots: &[usize], time_seconds| {
        let clock = AnimationClock {
            time_seconds,
            frame_seconds: 1.0 / 60.0,
        };
        FrameRequest {
            page_needs: OnceLock::new(),
            source: source.clone(),
            materials: slots
                .iter()
                .map(|&slot| MaterialKey {
                    scene: source.key(),
                    slot,
                    frames: MaterialFrames::select(
                        &source.render_scene().materials[slot],
                        clock,
                        0.0,
                    )
                    .unwrap(),
                })
                .collect(),
            sky: false,
            immersion: None,
            eye: super::super::Vec3::ZERO,
            water_time: 0.0,
            clock,
            frame_water_time: 0.0,
            #[cfg(feature = "map-capture")]
            exact_frame: false,
        }
    };
    let first = request(&[0], 0.25);
    assert!(first.page_needs.get().is_none());
    let uncached = first.page_needs_with_cache(false);
    assert!(matches!(&uncached, Cow::Owned(_)));
    assert!(first.page_needs.get().is_none());
    let cached = first.page_needs_with_cache(true);
    assert!(matches!(&cached, Cow::Borrowed(_)));
    assert_eq!(cached.len(), 1);
    for ((key, source), (expected_key, expected_source)) in cached.iter().zip(uncached.iter()) {
        assert_eq!(key, expected_key);
        assert!(Arc::ptr_eq(source, expected_source));
    }
    for _ in 0..4 {
        assert!(std::ptr::eq(
            first.page_needs_with_cache(true).as_ref(),
            cached.as_ref()
        ));
    }
    let fresh = first.page_needs_with_cache(false);
    assert!(matches!(&fresh, Cow::Owned(_)));
    assert_eq!(fresh.as_ref(), cached.as_ref());
    assert_eq!(cached[0].0.request.frame, 0);
    let later = request(&[0], 2.25);
    assert_eq!(later.page_needs_with_cache(true)[0].0.request.frame, 2);
    assert_eq!(first.page_needs_with_cache(true)[0].0.request.frame, 0);
    let second_view = request(&[1], 2.25);
    assert_ne!(
        second_view.page_needs_with_cache(true)[0]
            .0
            .request
            .source_id,
        cached[0].0.request.source_id
    );
    assert!(request(&[], 2.25).page_needs_with_cache(true).is_empty());
    let union = request(&[0, 1], 2.25);
    assert_eq!(union.page_needs_with_cache(true).len(), 2);
    assert_eq!(
        union.page_needs_with_cache(true)[0].0,
        later.page_needs_with_cache(true)[0].0
    );
    assert_eq!(
        union.page_needs_with_cache(true)[1].0,
        second_view.page_needs_with_cache(true)[0].0
    );
}

fn source_page_validation(need: &PageNeed) -> Result<ValidatedPage, String> {
    let cost = need
        .1
        .validate_page(&need.0.request)
        .map_err(|error| error.to_string())?;
    Ok(ValidatedPage {
        normalized: need.0.request.clone(),
        bytes: cost.resident_bytes as u64,
    })
}

#[test]
fn page_validation_reuse_is_invalidated_by_each_device_policy_dependency() {
    let (_files, need) = rgba_page([1, 2, 3]);
    let mut cache = PageValidation::default();
    let policy = PageDevicePolicy {
        supports_bc: true,
        max_dimension: 4096,
        max_array_layers: 256,
    };
    let initial = cache
        .get_or_init(policy, || source_page_validation(&need))
        .unwrap();
    assert_eq!(initial.normalized, need.0.request);
    assert_eq!(initial.bytes, 64);
    cache
        .get_or_init(policy, || panic!("unchanged policy must reuse validation"))
        .unwrap();
    let mut reevaluations = 0;
    for changed in [
        PageDevicePolicy {
            supports_bc: false,
            ..policy
        },
        PageDevicePolicy {
            max_dimension: 2048,
            ..policy
        },
        PageDevicePolicy {
            max_array_layers: 128,
            ..policy
        },
    ] {
        cache
            .get_or_init(policy, || source_page_validation(&need))
            .unwrap();
        cache
            .get_or_init(changed, || {
                reevaluations += 1;
                source_page_validation(&need)
            })
            .unwrap();
        cache
            .get_or_init(changed, || panic!("updated policy must reuse validation"))
            .unwrap();
    }
    assert_eq!(reevaluations, 3);
}

#[test]
fn page_validation_cache_keeps_exact_request_limits_before_compatible_image_reuse() {
    let (_files, permissive) = rgba_page([1, 2, 3]);
    let mut strict = permissive.clone();
    strict.0.request.limits.resident_bytes = 63;
    assert_eq!(permissive.0.request.identity(), strict.0.request.identity());
    let policy = PageDevicePolicy {
        supports_bc: true,
        max_dimension: 4096,
        max_array_layers: 256,
    };
    let mut validations: HashMap<PageKey, PageValidation> = HashMap::new();
    validations
        .entry(permissive.0.clone())
        .or_default()
        .get_or_init(policy, || source_page_validation(&permissive))
        .unwrap();
    let first_error = validations
        .entry(strict.0.clone())
        .or_default()
        .get_or_init(policy, || source_page_validation(&strict))
        .unwrap_err();
    let cached_error = validations
        .get_mut(&strict.0)
        .unwrap()
        .get_or_init(policy, || {
            panic!("unchanged strict request must retain its rejection")
        })
        .unwrap_err();
    assert_eq!(first_error, cached_error);
    assert_eq!(validations.len(), 2);
    validations
        .get_mut(&permissive.0)
        .unwrap()
        .get_or_init(policy, || {
            panic!("strict failure must not invalidate permissive request")
        })
        .unwrap();
}

#[test]
fn page_validation_counterfactual_revalidates_without_populating_or_replacing_cache() {
    let (_files, need) = rgba_page([1, 2, 3]);
    let mut cache = PageValidation::default();
    let policy = PageDevicePolicy {
        supports_bc: true,
        max_dimension: 4096,
        max_array_layers: 256,
    };
    let first = cache
        .get_with(policy, false, || source_page_validation(&need))
        .unwrap();
    assert!(matches!(first, Cow::Owned(_)));
    assert!(cache.cached.is_none());
    let cached = cache
        .get_with(policy, true, || source_page_validation(&need))
        .unwrap();
    assert!(matches!(cached, Cow::Borrowed(_)));
    let mut reevaluations = 0;
    for _ in 0..3 {
        cache
            .get_with(policy, false, || {
                reevaluations += 1;
                source_page_validation(&need)
            })
            .unwrap();
    }
    assert_eq!(reevaluations, 3);
    cache
        .get_with(policy, true, || {
            panic!("counterfactual must not replace cached validation")
        })
        .unwrap();
}

#[test]
fn page_validation_rejects_and_recovers_when_device_policy_changes() {
    let (_files, need) = rgba_page([1, 2, 3]);
    let capable = PageDevicePolicy {
        supports_bc: true,
        max_dimension: 4,
        max_array_layers: 256,
    };
    let constrained = PageDevicePolicy {
        max_dimension: 2,
        ..capable
    };
    let evaluate = |policy: PageDevicePolicy| {
        if need
            .1
            .metadata()
            .original_dimensions
            .iter()
            .any(|&side| side > policy.max_dimension)
        {
            Err(super::super::images::UploadError::DeviceLimit.to_string())
        } else {
            source_page_validation(&need)
        }
    };
    let mut cache = PageValidation::default();
    assert_eq!(
        cache
            .get_or_init(capable, || evaluate(capable))
            .unwrap()
            .bytes,
        64
    );
    let error = cache
        .get_or_init(constrained, || evaluate(constrained))
        .unwrap_err();
    assert_eq!(
        error,
        super::super::images::UploadError::DeviceLimit.to_string()
    );
    let recovered = cache.get_or_init(capable, || evaluate(capable)).unwrap();
    assert_eq!(recovered.normalized, need.0.request);
    assert_eq!(recovered.bytes, 64);
    assert_eq!(
        cache
            .get_or_init(constrained, || evaluate(constrained))
            .unwrap_err(),
        error
    );
}
