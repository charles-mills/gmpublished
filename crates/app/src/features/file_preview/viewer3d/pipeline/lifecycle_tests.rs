use super::*;
use crate::{
    bridge::{archive::PreviewArchiveSource, materials::MaterialResolver},
    media::preview_model::{MaterialSlot, ModelPreview, ModelStats, RenderScene},
    test_support::{TestDir, fixture_vtf_bytes},
};
use iced::advanced::renderer::Headless as _;
use iced::{Color, Length, Size, mouse};
use iced_runtime::{UserInterface, user_interface};
use std::time::{Duration, Instant};

struct Fixture {
    model: Arc<ModelPreview>,
    _files: TestDir,
}

fn model(rgb: [u8; 3]) -> Fixture {
    model_frames(&[rgb], 1)
}

fn model_frames(colors: &[[u8; 3]], mip_count: u8) -> Fixture {
    let files = TestDir::new("gmpublished-lifecycle13a");
    let proxy = r#"Proxies { AnimatedTexture { animatedTextureVar "$basetexture" animatedTextureFrameNumVar "$frame" animatedTextureFrameRate 30 } }"#;
    let vmt = format!("UnlitGeneric {{ $basetexture lifecycle $nocull 1 $nofog 1 {proxy} }}");
    let mut vtf = fixture_vtf_bytes(
        4,
        4,
        vformats::vtf::VtfFormat::Rgba8888,
        &vec![&[][..]; usize::from(mip_count)],
    );
    vtf.resize(80, 0);
    vtf[8..12].copy_from_slice(&5_u32.to_le_bytes());
    vtf[12..16].copy_from_slice(&80_u32.to_le_bytes());
    vtf[24..26].copy_from_slice(&u16::try_from(colors.len()).unwrap().to_le_bytes());
    vtf[63..65].copy_from_slice(&1_u16.to_le_bytes());
    for mip in (0..mip_count).rev() {
        let side = 4_usize >> mip;
        for color in colors {
            vtf.extend([color[0], color[1], color[2], 255].repeat(side * side));
        }
    }
    let vmt_path = files.file("materials/lifecycle.vmt", &vmt);
    let vtf_path = files.file("materials/lifecycle.vtf", &vtf);
    let archive = PreviewArchiveSource::from_folder([
        (
            "materials/lifecycle.vmt".to_owned(),
            vmt.len() as u64,
            vmt_path,
        ),
        (
            "materials/lifecycle.vtf".to_owned(),
            vtf.len() as u64,
            vtf_path,
        ),
    ]);
    let resolver = MaterialResolver::new(archive, None).with_on_demand_textures();
    let resolution = resolver.resolve_with_base2(&[], "lifecycle");
    let material = resolution.material().unwrap();
    let texture = material.texture.as_ref().unwrap();
    assert!(
        texture.resident().is_none(),
        "fixture must exercise demand paging"
    );
    let (_, recipe) = texture.on_demand().unwrap();
    assert_eq!(recipe.first_source_mip, 0);
    assert_eq!(recipe.mip_count, mip_count);
    assert_eq!(recipe.faces, 0..1);
    assert_eq!(
        texture.source_metadata().unwrap().frame_count,
        colors.len() as u32
    );
    assert_eq!(texture.source_metadata().unwrap().mip_count, mip_count);
    let vertices = [(-1., -1.), (1., -1.), (1., 1.), (-1., 1.)].map(|(x, y)| ModelVertex {
        position: Vec3::new(x, y, 0.),
        normal: Vec3::new(0., 0., 1.),
        uv: [x, y],
        lightmap_uv: [0.; 2],
        lightmap_bump_step: 0.,
        lightmap_page: 0,
        color: Vec3::splat(1.0),
        blend_alpha: 0.,
        opacity: 1.,
        illumination: Vec3::splat(1.0),
        lighting_mode: crate::media::preview_model::vertex_lighting::MODEL_DRAW,
        tangent_s: Vec3::new(1., 0., 0.),
        tangent_t: Vec3::new(0., 1., 0.),
        tangent_sign: 1.,
        lighting_context: 0,
        surface_mark: 0,
    });
    let model = Arc::new(ModelPreview {
        scene: Arc::new(RenderScene {
            meshes: vec![MeshData {
                geometry: crate::media::preview_model::MeshGeometry::Model,
                vertices: vertices.to_vec(),
                indices: vec![0, 1, 2, 0, 2, 3],
                material_index: 0,
                bodygroup: 0,
                bodygroup_choice: 0,
            }],
            materials: vec![MaterialSlot {
                resolution: resolution.outcome(),
                canonical_name: resolution.canonical_name().map(str::to_owned),
                name: "lifecycle".to_owned(),
                texture: material.texture.clone(),
                texture2: material.texture2.clone(),
                shader: material.shader.clone(),
                render_mode: material.render_mode,
                alpha_policy: material.render_mode.texture_alpha_policy(),
            }],
            phy_debug_meshes: Vec::new(),
            stats: ModelStats {
                bone_count: 0,
                sequence_count: 0,
                vertex_count: 4,
                triangle_count: 2,
                mesh_count: 1,
                material_count: 1,
                resolved_material_count: 1,
            },
            bounds_min: Vec3::new(-1., -1., 0.),
            bounds_max: Vec3::new(1., 1., 0.),
        }),
        skin_tables: Vec::new(),
        bodygroups: Vec::new(),
    });
    Fixture {
        model,
        _files: files,
    }
}

type View = (
    Arc<ModelPreview>,
    Arc<super::super::feedback::RenderFeedback>,
);

#[derive(Clone, Copy)]
enum ClockPolicy {
    Exact,
    Live,
}

struct ControlledClockView {
    observer: super::super::capture::CaptureDiagnostics,
    viewer: super::super::camera::Viewer3d,
    time: f32,
    policy: ClockPolicy,
    map: Option<Arc<super::super::MapPreview>>,
}
impl iced::widget::shader::Program<crate::features::file_preview::Message> for ControlledClockView {
    type State = super::super::camera::Camera;
    type Primitive = ModelPrimitive;
    fn draw(
        &self,
        state: &Self::State,
        cursor: mouse::Cursor,
        bounds: Rectangle,
    ) -> ModelPrimitive {
        let mut primitive = self.viewer.draw(state, cursor, bounds);
        primitive.animation_clock = crate::bridge::materials::AnimationClock {
            time_seconds: self.time,
            frame_seconds: 1.0 / 30.0,
        };
        primitive.model_lighting_time = f64::from(self.time);
        if let Some(map) = &self.map {
            primitive.preview = PreviewScene::Map(Arc::clone(map));
            primitive.uniforms.water_time_sky_tint[0] = self.time;
        }
        primitive.exact_texture_frame = matches!(self.policy, ClockPolicy::Exact);
        primitive.capture_diagnostics = Some(self.observer.clone());
        primitive
    }
}

#[derive(Default)]
struct Harness {
    cache: user_interface::Cache,
    observers: Vec<super::super::capture::CaptureDiagnostics>,
    bodygroups: Vec<Vec<usize>>,
    skin_remaps: Vec<Vec<u16>>,
    maps: Vec<Option<Arc<super::super::MapPreview>>>,
}
impl Harness {
    fn draw(&mut self, renderer: &mut iced::Renderer, views: &[View], times: &[f32]) -> Vec<u8> {
        self.draw_with_policy(renderer, views, times, ClockPolicy::Exact, true)
    }
    fn draw_live(
        &mut self,
        renderer: &mut iced::Renderer,
        views: &[View],
        times: &[f32],
    ) -> Vec<u8> {
        self.draw_with_policy(renderer, views, times, ClockPolicy::Live, false)
    }
    fn draw_with_policy(
        &mut self,
        renderer: &mut iced::Renderer,
        views: &[View],
        times: &[f32],
        policy: ClockPolicy,
        settle: bool,
    ) -> Vec<u8> {
        assert_eq!(views.len(), times.len());
        self.observers.resize_with(views.len(), Default::default);
        let widgets = views
            .iter()
            .zip(times)
            .zip(&self.observers)
            .enumerate()
            .map(|(index, (((model, feedback), &time), observer))| {
                iced::widget::shader(ControlledClockView {
                    observer: observer.clone(),
                    time,
                    policy,
                    map: self.maps.get(index).cloned().flatten(),
                    viewer: super::super::camera::Viewer3d {
                        model: Arc::clone(model),
                        render_feedback: Arc::clone(feedback),
                        content_id: 1,
                        skin_remap: self.skin_remaps.get(index).cloned().unwrap_or_default(),
                        bodygroup_choices: self.bodygroups.get(index).cloned().unwrap_or_default(),
                        phy_debug_visible: false,
                        pose: None,
                    },
                })
                .width(Length::Fill)
                .height(Length::Fill)
                .into()
            });
        let element: iced::Element<'_, crate::features::file_preview::Message> =
            iced::widget::Row::with_children(widgets)
                .width(Length::Fill)
                .height(Length::Fill)
                .into();
        let mut ui = UserInterface::build(
            element,
            Size::new(320., 180.),
            std::mem::take(&mut self.cache),
            renderer,
        );
        ui.draw(
            renderer,
            &iced::Theme::Dark,
            &iced::advanced::renderer::Style {
                text_color: Color::WHITE,
            },
            mouse::Cursor::Unavailable,
        );
        self.cache = ui.into_cache();
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let pixels = renderer.screenshot(Size::new(320, 180), 1., Color::BLACK);
            if !settle
                || matches!(policy, ClockPolicy::Live)
                || views.iter().all(|(_, feedback)| {
                    !matches!(
                        feedback.snapshot(),
                        super::super::feedback::RenderStatus::Pending
                            | super::super::feedback::RenderStatus::Retiring { .. }
                    )
                })
            {
                return pixels;
            }
            assert!(
                Instant::now() < deadline,
                "texture paging did not settle: {:?}",
                views
                    .iter()
                    .map(|(_, feedback)| feedback.snapshot())
                    .collect::<Vec<_>>()
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    fn image_residency(&self, index: usize) -> serde_json::Value {
        self.observers[index].lock().unwrap()["image_resources"].clone()
    }
}

fn renderer() -> iced::Renderer {
    iced::futures::executor::block_on(iced::Renderer::new(
        iced::Font::default(),
        iced::Pixels(16.),
        Some("wgpu"),
    ))
    .expect("native GPU renderer")
}

fn center(pixels: &[u8], x: usize) -> [u8; 4] {
    pixels[(90 * 320 + x) * 4..(90 * 320 + x) * 4 + 4]
        .try_into()
        .unwrap()
}
fn ready(feedback: &super::super::feedback::RenderFeedback, unavailable_images: usize) {
    assert_eq!(
        feedback.snapshot(),
        super::super::feedback::RenderStatus::Ready { unavailable_images }
    );
    assert!(!feedback.wants_redraw(std::time::Instant::now()));
}

#[test]
#[ignore = "requires exclusive GPU and GMPUBLISHED_MAP_CAPTURE_GPU_IMAGE_BYTES=64 / ALLOCATIONS=1"]
fn iced_scene_replacement_retries_only_after_retirement_and_preserves_other_views() {
    let policy = super::texture_upload::configured_policy();
    assert_eq!(policy.sampled_image_bytes, 64);
    assert_eq!(policy.sampled_image_allocations, 1);
    let mut renderer = renderer();
    let mut harness = Harness::default();
    let a_fixture = model([224, 32, 16]);
    let b_fixture = model([16, 224, 32]);
    let a = (Arc::clone(&a_fixture.model), Arc::default());
    let b = (Arc::clone(&b_fixture.model), Arc::default());
    let red = center(
        &harness.draw(&mut renderer, std::slice::from_ref(&a), &[0.0]),
        160,
    );
    assert!(red[0] > red[1] + 100, "visible red control: {red:?}");
    ready(&a.1, 0);
    let green = center(
        &harness.draw(&mut renderer, std::slice::from_ref(&b), &[0.0]),
        160,
    );
    assert!(
        green[1] > green[0] + 100,
        "same-scene B must recover: {green:?}"
    );
    ready(&b.1, 0);
    let simultaneous = harness.draw(&mut renderer, &[b.clone(), a.clone()], &[0.0, 0.0]);
    ready(&b.1, 0);
    match a.1.snapshot() {
        super::super::feedback::RenderStatus::Rejected { reason } => {
            assert!(
                reason.contains("visible texture working set requires"),
                "{reason}"
            );
            assert!(reason.contains("128 bytes in 2 allocations"), "{reason}");
            assert!(reason.contains("64 bytes in 1 allocations"), "{reason}");
        }
        other => {
            panic!("visible union must report insufficient capacity, not a checker: {other:?}")
        }
    }
    assert_eq!(
        center(&simultaneous, 80),
        green,
        "blocked A cannot evict active B"
    );
    let recovered = center(
        &harness.draw(&mut renderer, std::slice::from_ref(&a), &[0.0]),
        160,
    );
    assert_eq!(
        recovered, red,
        "closing B must recover the existing A scene"
    );
    ready(&a.1, 0);
    let scene_key = PreviewScene::Model(Arc::clone(&a.0)).key();
    let rejected = harness.draw(&mut renderer, std::slice::from_ref(&a), &[f32::NAN]);
    assert_eq!(PreviewScene::Model(Arc::clone(&a.0)).key(), scene_key);
    match a.1.snapshot() {
        super::super::feedback::RenderStatus::Rejected { reason } => {
            assert!(reason.contains("material animation"), "{reason}");
            assert!(reason.contains("not finite"), "{reason}");
        }
        other => panic!("nonfinite clock must reject the same scene: {other:?}"),
    }
    assert_eq!(
        center(&rejected, 160),
        [0, 0, 0, 255],
        "rejected scene must not draw its old binding"
    );
    let sibling = harness.draw(&mut renderer, std::slice::from_ref(&b), &[0.0]);
    ready(&b.1, 0);
    assert_eq!(
        center(&sibling, 160),
        green,
        "rejected A must release its only allocation for sibling B"
    );
    let recovered = harness.draw(&mut renderer, std::slice::from_ref(&a), &[0.0]);
    ready(&a.1, 0);
    assert_eq!(
        center(&recovered, 160),
        red,
        "same scene must recover at a finite clock"
    );
    let equivalent_fixture = model([224, 32, 16]);
    let equivalent = (Arc::clone(&equivalent_fixture.model), Arc::default());
    let shared = harness.draw(&mut renderer, &[a.clone(), equivalent.clone()], &[0.0, 0.0]);
    ready(&a.1, 0);
    ready(&equivalent.1, 0);
    assert_eq!(center(&shared, 80), red);
    assert_eq!(center(&shared, 240), red);
}

fn palette(frame: usize) -> [u8; 3] {
    [
        (frame % 256) as u8,
        if frame < 256 { 32 } else { 224 },
        (32 + frame * 73 % 192) as u8,
    ]
}
fn time(frame: usize) -> f32 {
    (frame as f32 + 0.125) / 30.0
}
fn color(actual: [u8; 4], expected: [u8; 3], context: &str) {
    assert!(
        (0..3).all(|channel| actual[channel].abs_diff(expected[channel]) <= 2),
        "{context}: {actual:?} versus {expected:?}"
    );
    assert_eq!(actual[3], 255);
}

#[test]
#[ignore = "requires exclusive GPU and normal image capacity"]
fn iced_demand_pages_preserve_363_frames_wrap_and_simultaneous_clocks() {
    let policy = super::texture_upload::configured_policy();
    assert!(policy.sampled_image_bytes >= 1024 * 1024);
    assert!(policy.sampled_image_allocations >= 4);
    let fixture = model_frames(&(0..363).map(palette).collect::<Vec<_>>(), 3);
    let view = (Arc::clone(&fixture.model), Arc::default());
    let independent = (Arc::clone(&fixture.model), Arc::default());
    let mut renderer = renderer();
    let mut harness = Harness::default();
    let mut reference = Vec::new();
    for frame in 0..363 {
        let pixels = harness.draw(&mut renderer, std::slice::from_ref(&view), &[time(frame)]);
        ready(&view.1, 0);
        let pixel = center(&pixels, 160);
        color(pixel, palette(frame), &format!("cold frame {frame}"));
        reference.push(pixel);
        assert_eq!(harness.image_residency(0)["allocations"], 1);
        assert_eq!(
            harness.image_residency(0)["estimated_bytes"],
            84,
            "all authored4x4/2x2/1x1 mips stay resident"
        );
    }
    assert_ne!(reference[255], reference[256]);
    assert_ne!(reference[362], reference[0]);
    for frame in [255, 256, 362, 0, 363, 726] {
        for _ in 0..3 {
            let pixels = harness.draw(&mut renderer, std::slice::from_ref(&view), &[time(frame)]);
            ready(&view.1, 0);
            assert_eq!(
                center(&pixels, 160),
                reference[frame % 363],
                "stationary/wrapped frame {frame}"
            );
        }
    }
    for (left, right) in [(0, 256), (362, 255), (256, 0), (255, 362)] {
        let pixels = harness.draw(
            &mut renderer,
            &[view.clone(), independent.clone()],
            &[time(left), time(right)],
        );
        ready(&view.1, 0);
        ready(&independent.1, 0);
        assert_eq!(center(&pixels, 80), reference[left], "left clock {left}");
        assert_eq!(
            center(&pixels, 240),
            reference[right],
            "right clock {right}"
        );
    }
    let reversed = harness.draw(&mut renderer, &[independent, view], &[time(256), time(0)]);
    assert_eq!(center(&reversed, 80), reference[256]);
    assert_eq!(center(&reversed, 240), reference[0]);
}

fn solid_center(pixels: &[u8], x: usize) -> [u8; 4] {
    let expected = center(pixels, x);
    for y in 88..=92 {
        for column in x - 2..=x + 2 {
            assert_eq!(
                &pixels[(y * 320 + column) * 4..(y * 320 + column) * 4 + 4],
                expected.as_slice(),
                "incomplete uniform receiver at {column},{y}"
            );
        }
    }
    expected
}

#[test]
#[ignore = "requires exclusive GPU and normal image capacity"]
fn iced_live_animation_keeps_complete_pixels_while_cold_pages_load() {
    let policy = super::texture_upload::configured_policy();
    assert!(policy.sampled_image_bytes >= 1024 * 1024);
    assert!(policy.sampled_image_allocations >= 4);
    let fixture = model_frames(&(0..363).map(palette).collect::<Vec<_>>(), 3);
    let sibling_fixture = model([17, 231, 93]);
    let view = (Arc::clone(&fixture.model), Arc::default());
    let sibling = (Arc::clone(&sibling_fixture.model), Arc::default());
    let views = [view, sibling];
    let (source, _) = fixture.model.scene.materials[0]
        .texture
        .as_ref()
        .unwrap()
        .on_demand()
        .unwrap();
    let mut renderer = renderer();
    let mut harness = Harness::default();
    let initial = harness.draw(&mut renderer, &views, &[time(1), 0.0]);
    let mut previous = solid_center(&initial, 80);
    color(previous, palette(1), "initial live frame");
    let sibling_pixel = solid_center(&initial, 240);
    color(sibling_pixel, [17, 231, 93], "independent preview");
    for frame in [255, 256, 362, 363] {
        let pause = source.pause_page_reads_for_test();
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut clock = time(frame);
        loop {
            let pixels = harness.draw_live(&mut renderer, &views, &[clock, 0.0]);
            clock += 1.0 / 120.0;
            assert_eq!(solid_center(&pixels, 80), previous, "pending frame {frame}");
            assert_eq!(solid_center(&pixels, 240), sibling_pixel);
            assert!(matches!(
                views[0].1.snapshot(),
                super::super::feedback::RenderStatus::Ready {
                    unavailable_images: 0
                }
            ));
            if pause.has_waiter() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "cold frame {frame} never requested"
            );
        }
        for _ in 0..3 {
            let pixels = harness.draw_live(&mut renderer, &views, &[clock, 0.0]);
            clock += 1.0 / 120.0;
            assert_eq!(
                solid_center(&pixels, 80),
                previous,
                "paused live frame {frame}"
            );
            assert_eq!(solid_center(&pixels, 240), sibling_pixel);
        }
        drop(pause);
        loop {
            let pixels = harness.draw_live(&mut renderer, &views, &[clock, 0.0]);
            clock += 1.0 / 120.0;
            let actual = solid_center(&pixels, 80);
            assert_eq!(solid_center(&pixels, 240), sibling_pixel);
            assert!(matches!(
                views[0].1.snapshot(),
                super::super::feedback::RenderStatus::Ready {
                    unavailable_images: 0
                }
            ));
            if actual != previous {
                color(
                    actual,
                    palette(frame % 363),
                    &format!("live progression {frame}"),
                );
                previous = actual;
                break;
            }
            assert!(
                Instant::now() < deadline,
                "live frame {frame} never became visible"
            );
        }
        assert_eq!(harness.image_residency(0)["estimated_bytes"], 84);
    }
}

#[test]
fn lifecycle_fixture_uses_file_backed_single_frame_full_mip_requests() {
    let fixture = model_frames(&(0..363).map(palette).collect::<Vec<_>>(), 3);
    assert!(fixture._files.join("materials/lifecycle.vtf").is_file());
    let texture = fixture.model.scene.materials[0].texture.as_ref().unwrap();
    let (source, recipe) = texture.on_demand().unwrap();
    assert_eq!(source.metadata().frame_count, 363);
    assert_eq!(recipe.mip_count, 3);
    let mut last = recipe.clone();
    last.frame = 362;
    assert_eq!(source.page_cost(&last).unwrap().resident_bytes, 84);
    assert!(texture.resident().is_none());
    assert_eq!(
        fixture.model.scene.materials[0]
            .shader
            .surface()
            .unwrap()
            .animated_textures
            .len(),
        1
    );
}

fn assert_pane_equal(actual: &[u8], expected: &[u8], pane: usize) {
    for row in 0..180 {
        let start = (row * 320 + pane * 160) * 4;
        assert_eq!(
            &actual[start..start + 160 * 4],
            &expected[start..start + 160 * 4],
            "pane {pane}, row {row}"
        );
    }
}

fn find_solid_patch(pixels: &[u8], pane: usize, rgb: [u8; 3]) -> (usize, usize, [u8; 4]) {
    for y in 2..178 {
        for x in pane * 160 + 2..(pane + 1) * 160 - 2 {
            let pixel: [u8; 4] = pixels[(y * 320 + x) * 4..(y * 320 + x) * 4 + 4]
                .try_into()
                .unwrap();
            if pixel[3] == 255
                && (0..3).all(|channel| pixel[channel].abs_diff(rgb[channel]) <= 2)
                && (y - 2..=y + 2).all(|row| {
                    (x - 2..=x + 2).all(|column| {
                        pixels[(row * 320 + column) * 4..(row * 320 + column) * 4 + 4] == pixel
                    })
                })
            {
                return (x, y, pixel);
            }
        }
    }
    panic!("no solid {rgb:?} receiver patch in pane {pane}");
}

fn paired_model(a_fixture: &Fixture, b_fixture: &Fixture) -> Arc<ModelPreview> {
    let mut combined = a_fixture.model.as_ref().clone();
    let scene = Arc::make_mut(&mut combined.scene);
    for vertex in &mut scene.meshes[0].vertices {
        vertex.position = Vec3::new(
            (vertex.position.x() - 1.05) * 0.45,
            vertex.position.y(),
            vertex.position.z(),
        );
    }
    let mut b_mesh = b_fixture.model.scene.meshes[0].clone();
    for vertex in &mut b_mesh.vertices {
        vertex.position = Vec3::new(
            (vertex.position.x() + 1.05) * 0.45,
            vertex.position.y(),
            vertex.position.z(),
        );
    }
    b_mesh.material_index = 1;
    b_mesh.bodygroup = 1;
    b_mesh.bodygroup_choice = 1;
    scene.meshes.push(b_mesh);
    scene
        .materials
        .push(b_fixture.model.scene.materials[0].clone());
    scene.stats.vertex_count = 8;
    scene.stats.triangle_count = 4;
    scene.stats.mesh_count = 2;
    scene.stats.material_count = 2;
    scene.stats.resolved_material_count = 2;
    combined.bodygroups = vec![1, 2];
    Arc::new(combined)
}

fn lightmapped_preview(model: &Arc<ModelPreview>) -> Arc<super::super::MapPreview> {
    use gmpublished_domain::scene::map::{LightmapSet, LightmapSource};
    let resolver = MaterialResolver::prop_lighting_fixture(
        vec![(
            "materials/lit.vmt",
            br#"LightmappedGeneric { "$basetexture" "white" "$nocull" "1" "$nofog" "1" }"#.to_vec(),
        )],
        Vec::new(),
    );
    let shader = resolver
        .resolve_with_base2(&[], "lit")
        .into_material()
        .unwrap()
        .shader;
    let mut map =
        super::super::test_support::empty_preview(model.scene.bounds_min, model.scene.bounds_max);
    map.scene = Arc::clone(&model.scene);
    let scene = Arc::make_mut(&mut map.scene);
    for material in &mut scene.materials {
        material.shader = shader.clone();
    }
    for (index, mesh) in scene.meshes.iter_mut().enumerate() {
        mesh.geometry = crate::media::preview_model::MeshGeometry::Brush;
        for triangle in mesh.indices.chunks_exact_mut(3) {
            triangle.swap(1, 2);
        }
        for vertex in &mut mesh.vertices {
            vertex.lightmap_page = u32::try_from(index + 1).unwrap();
            vertex.lightmap_uv = [0.375; 2];
            vertex.lighting_mode = crate::media::preview_model::vertex_lighting::LIGHTMAP;
        }
    }
    map.lightmap = Some(LightmapSet::style_pages_for_tests(
        LightmapSource::Ldr,
        &[vec![(0, [255, 0, 0, 0])], vec![(32, [0, 255, 0, 0])]],
        &[(32, "am")],
    ));
    Arc::new(map)
}

#[test]
#[ignore = "requires exclusive GPU, 4x MSAA and normal image capacity"]
fn iced_subpixel_brush_faces_sample_only_their_covered_lightmap_chart() {
    assert_eq!(super::super::scene_sample_count(), 4);
    let policy = super::texture_upload::configured_policy();
    assert!(policy.sampled_image_bytes >= 1024 * 1024);
    let fixture = model([255, 255, 255]);
    let mut map = lightmapped_preview(&fixture.model);
    let scene = Arc::make_mut(&mut Arc::make_mut(&mut map).scene);
    let prototype = scene.meshes[0].clone();
    scene.meshes = (0..96)
        .map(|index| {
            let width = [0.001, 0.002, 0.004][index % 3];
            let center = -0.8 + index as f32 * 1.6 / 95.0;
            let mut mesh = prototype.clone();
            for vertex in &mut mesh.vertices {
                let right = vertex.position.x() > 0.0;
                vertex.position = Vec3::new(
                    center + if right { width * 0.5 } else { -width * 0.5 },
                    vertex.position.y() * 0.9,
                    vertex.position.z(),
                );
                vertex.lightmap_uv = [if right { 0.4375 } else { 0.3125 }, 0.375];
            }
            mesh
        })
        .collect();
    scene.stats.mesh_count = u32::try_from(scene.meshes.len()).unwrap();
    scene.stats.vertex_count = scene.stats.mesh_count * 4;
    scene.stats.triangle_count = scene.stats.mesh_count * 2;
    let view = (Arc::clone(&fixture.model), Arc::default());
    let mut harness = Harness {
        maps: vec![Some(map)],
        ..Harness::default()
    };
    let mut renderer = renderer();
    let pixels = harness.draw(&mut renderer, std::slice::from_ref(&view), &[0.0]);
    ready(&view.1, 0);
    let mut covered = 0;
    for (index, pixel) in pixels.chunks_exact(4).enumerate() {
        assert_eq!(
            &pixel[1..3],
            &[0, 0],
            "neighboring lightmap texels leaked into subpixel brush coverage at {},{}: {pixel:?}",
            index % 320,
            index / 320,
        );
        covered += usize::from(pixel[0] > 0);
    }
    assert!(
        covered >= 32,
        "thin brush faces had insufficient coverage: {covered}"
    );
}

#[test]
#[ignore = "requires exclusive GPU and normal image capacity"]
fn iced_lightmap_pages_share_static_storage_and_isolate_style_clocks_with_warm_revisits() {
    let a_fixture = model([255, 255, 255]);
    let b_fixture = model([255, 255, 255]);
    let combined = paired_model(&a_fixture, &b_fixture);
    let map = lightmapped_preview(&combined);
    let views = [
        (Arc::clone(&combined), Arc::default()),
        (combined, Arc::default()),
    ];
    let mut harness = Harness {
        maps: vec![Some(Arc::clone(&map)), Some(map)],
        bodygroups: vec![vec![0, 1]; 2],
        ..Harness::default()
    };
    let mut renderer = renderer();
    let original = harness.draw(&mut renderer, &views, &[0.0, 0.1]);
    ready(&views[0].1, 0);
    ready(&views[1].1, 0);
    find_solid_patch(&original, 0, [255, 0, 0]);
    let (green_x, green_y, green_pixel) = find_solid_patch(&original, 1, [0, 255, 0]);
    let dark_offset = (green_y * 320 + green_x - 160) * 4;
    assert_eq!(
        &original[dark_offset..dark_offset + 4],
        &[0, 0, 0, 255],
        "style0 viewer borrowed its sibling's lit page"
    );
    let cache = harness.observers[0].lock().unwrap()["lightmap_cache"].clone();
    assert_eq!(cache["resident_pages"], 3, "{cache}");
    assert_eq!(cache["uploads"], 3, "{cache}");
    let both_lit = harness.draw(&mut renderer, &views, &[0.1, 0.1]);
    find_solid_patch(&both_lit, 0, [0, 255, 0]);
    assert_pane_equal(&both_lit, &original, 1);
    let restored = harness.draw(&mut renderer, &views, &[0.0, 0.1]);
    assert_eq!(restored, original, "styled page leaked between viewers");
    let cache = harness.observers[0].lock().unwrap()["lightmap_cache"].clone();
    assert_eq!(
        cache["uploads"], 3,
        "warm revisit uploaded another page: {cache}"
    );
    assert!(cache["warm_hits"].as_u64().unwrap() > 0, "{cache}");
    for index in 0..8 {
        let times = if index % 2 == 0 {
            [0.1, 0.0]
        } else {
            [0.0, 0.1]
        };
        let pixels = harness.draw(&mut renderer, &views, &times);
        if index % 2 == 1 {
            assert_eq!(pixels, original);
        }
        ready(&views[0].1, 0);
        ready(&views[1].1, 0);
    }
    harness.maps.remove(0);
    harness.bodygroups.remove(0);
    let closed = harness.draw(&mut renderer, std::slice::from_ref(&views[1]), &[0.1]);
    assert!(closed.chunks_exact(4).any(|pixel| pixel == green_pixel));
    ready(&views[1].1, 0);
    let final_cache = harness.observers[0].lock().unwrap()["lightmap_cache"].clone();
    assert_eq!(final_cache["uploads"], 3, "{final_cache}");
}

#[test]
#[ignore = "requires exclusive GPU and GMPUBLISHED_MAP_CAPTURE_GPU_IMAGE_BYTES=128 / ALLOCATIONS=2"]
fn iced_lightmap_and_material_pressure_preserves_resident_sibling_and_recovers_after_close() {
    use gmpublished_domain::scene::map::{LightmapSet, LightmapSource};
    let policy = super::texture_upload::configured_policy();
    assert_eq!(policy.sampled_image_bytes, 128);
    assert_eq!(policy.sampled_image_allocations, 2);
    let fixture = model([255, 255, 255]);
    let red_map = lightmapped_preview(&fixture.model);
    let mut green_map = red_map.as_ref().clone();
    green_map.lightmap = Some(LightmapSet::style_pages_for_tests(
        LightmapSource::Ldr,
        &[vec![(0, [0, 255, 0, 0])]],
        &[],
    ));
    let green_map = Arc::new(green_map);
    let a = (Arc::clone(&fixture.model), Arc::default());
    let b = (Arc::clone(&fixture.model), Arc::default());
    let mut renderer = renderer();
    let mut harness = Harness {
        maps: vec![Some(Arc::clone(&green_map))],
        ..Harness::default()
    };
    let green = center(
        &harness.draw(&mut renderer, std::slice::from_ref(&b), &[0.0]),
        160,
    );
    assert_eq!(green, [0, 254, 0, 255]);
    ready(&b.1, 0);
    assert_eq!(
        harness.observers[0].lock().unwrap()["lightmap_resources"]["pages"],
        1
    );
    harness.maps = vec![Some(green_map), Some(Arc::clone(&red_map))];
    let simultaneous = harness.draw(&mut renderer, &[b.clone(), a.clone()], &[0.0, 0.0]);
    ready(&b.1, 0);
    assert_eq!(center(&simultaneous, 80), green);
    match a.1.snapshot() {
        super::super::feedback::RenderStatus::Rejected { reason } => {
            assert!(reason.contains("working set"), "{reason}");
        }
        other => panic!(
            "combined material/lightmap capacity must reject the unsatisfied view: {other:?}"
        ),
    }
    harness.maps = vec![Some(red_map)];
    let recovered = harness.draw(&mut renderer, std::slice::from_ref(&a), &[0.0]);
    assert_eq!(center(&recovered, 160), [254, 0, 0, 255]);
    ready(&a.1, 0);
}

#[test]
#[ignore = "requires exclusive GPU and GMPUBLISHED_MAP_CAPTURE_GPU_IMAGE_BYTES=128 / ALLOCATIONS=2"]
fn iced_rejected_lightmap_demand_does_not_block_sibling_animation() {
    use gmpublished_domain::scene::map::{LightmapSet, LightmapSource};
    let policy = super::texture_upload::configured_policy();
    assert_eq!(policy.sampled_image_bytes, 128);
    assert_eq!(policy.sampled_image_allocations, 2);
    for page_count in [1, 3] {
        let lit = model([255, 255, 255]);
        let animated = model_frames(&[[255, 0, 0], [0, 255, 0], [0, 0, 255]], 1);
        let mut map = lightmapped_preview(&lit.model);
        let scene = Arc::make_mut(&mut Arc::make_mut(&mut map).scene);
        let mesh = scene.meshes[0].clone();
        scene.meshes = (0..page_count)
            .map(|index| {
                let mut mesh = mesh.clone();
                for vertex in &mut mesh.vertices {
                    vertex.lightmap_page = index + 1;
                }
                mesh
            })
            .collect();
        Arc::make_mut(&mut map).lightmap = Some(LightmapSet::style_pages_for_tests(
            LightmapSource::Ldr,
            &vec![vec![(0, [255, 255, 255, 0])]; page_count as usize],
            &[],
        ));
        let a = (Arc::clone(&lit.model), Arc::default());
        let b = (Arc::clone(&animated.model), Arc::default());
        let mut renderer = renderer();
        let mut harness = Harness::default();
        assert_eq!(
            center(
                &harness.draw(&mut renderer, std::slice::from_ref(&b), &[0.0]),
                160
            ),
            [255, 0, 0, 255]
        );
        ready(&b.1, 0);
        harness.maps = vec![None, Some(map)];
        for (time, expected) in [(0.0, [255, 0, 0]), (0.04, [0, 255, 0]), (0.07, [0, 0, 255])] {
            let pixels = harness.draw(&mut renderer, &[b.clone(), a.clone()], &[time, 0.0]);
            ready(&b.1, 0);
            assert_eq!(
                center(&pixels, 80),
                [expected[0], expected[1], expected[2], 255]
            );
            match a.1.snapshot() {
                super::super::feedback::RenderStatus::Rejected { reason } => {
                    assert!(reason.contains("working set"), "{reason}");
                }
                other => panic!("inadmissible lightmap preview must be rejected: {other:?}"),
            }
        }
    }
}

#[test]
#[ignore = "requires exclusive GPU and GMPUBLISHED_MAP_CAPTURE_GPU_IMAGE_BYTES=128 / ALLOCATIONS=2"]
fn iced_lightmap_subset_releases_stale_pages_before_sibling_admission() {
    use gmpublished_domain::scene::map::{LightmapSet, LightmapSource};
    let policy = super::texture_upload::configured_policy();
    assert_eq!(policy.sampled_image_bytes, 128);
    assert_eq!(policy.sampled_image_allocations, 2);
    let fixture = model([255, 255, 255]);
    let mut red_map = lightmapped_preview(&fixture.model);
    let scene = Arc::make_mut(&mut Arc::make_mut(&mut red_map).scene);
    let mut unlit = scene.meshes[0].clone();
    unlit.bodygroup_choice = 1;
    for vertex in &mut unlit.vertices {
        vertex.lightmap_page = 0;
        vertex.lighting_mode = crate::media::preview_model::vertex_lighting::WORLD_CONSTANT;
    }
    scene.meshes.push(unlit);
    let mut green_map = red_map.as_ref().clone();
    green_map.lightmap = Some(LightmapSet::style_pages_for_tests(
        LightmapSource::Ldr,
        &[vec![(0, [0, 255, 0, 0])]],
        &[],
    ));
    let a = (Arc::clone(&fixture.model), Arc::default());
    let b = (Arc::clone(&fixture.model), Arc::default());
    let mut renderer = renderer();
    let mut harness = Harness {
        maps: vec![Some(Arc::clone(&red_map))],
        bodygroups: vec![vec![0]],
        ..Harness::default()
    };
    assert_eq!(
        center(
            &harness.draw(&mut renderer, std::slice::from_ref(&a), &[0.0]),
            160
        ),
        [254, 0, 0, 255]
    );
    ready(&a.1, 0);
    harness.maps = vec![Some(red_map), Some(Arc::new(green_map))];
    harness.bodygroups = vec![vec![1], vec![0]];
    let first = harness.draw_with_policy(
        &mut renderer,
        &[a.clone(), b.clone()],
        &[0.0, 0.0],
        ClockPolicy::Exact,
        false,
    );
    ready(&a.1, 0);
    ready(&b.1, 0);
    assert_eq!(center(&first, 80), [255, 255, 255, 255]);
    assert_eq!(center(&first, 240), [0, 254, 0, 255]);
    let diagnostic = harness.observers[0].lock().unwrap();
    assert_eq!(diagnostic["lightmap_resources"]["pages"], 0);
    assert_eq!(diagnostic["lightmap_cache"]["resident_pages"], 1);
    drop(diagnostic);
}

#[test]
#[ignore = "requires exclusive GPU and normal image capacity"]
fn iced_no_draw_skin_omits_draw_and_missing_image_demand_without_affecting_siblings() {
    let a_fixture = model([224, 32, 16]);
    let b_fixture = model([16, 32, 224]);
    let sibling_fixture = model([17, 231, 93]);
    let files = TestDir::new("gmpublished-hidden-skin");
    let vmt = r#"UnlitGeneric { "$no_Draw" "1" "$basetexture" "absent" "$translucent" "1" }"#;
    let path = files.file("materials/hidden.vmt", vmt);
    let resolver = MaterialResolver::new(
        PreviewArchiveSource::from_folder([(
            "materials/hidden.vmt".into(),
            vmt.len() as u64,
            path,
        )]),
        None,
    )
    .with_on_demand_textures();
    let resolution = resolver.resolve_with_base2(&[], "hidden");
    let material = resolution.material().unwrap();
    assert_eq!(
        material.render_mode,
        crate::bridge::materials::RenderMode::NoDraw
    );
    assert!(material.texture.is_none());
    let mut combined = paired_model(&a_fixture, &b_fixture);
    let scene = Arc::make_mut(&mut Arc::make_mut(&mut combined).scene);
    scene.materials.push(MaterialSlot {
        resolution: resolution.outcome(),
        canonical_name: resolution.canonical_name().map(str::to_owned),
        name: "hidden".into(),
        texture: material.texture.clone(),
        texture2: material.texture2.clone(),
        shader: material.shader.clone(),
        render_mode: material.render_mode,
        alpha_policy: material.render_mode.texture_alpha_policy(),
    });
    scene.stats.material_count = 3;
    scene.stats.resolved_material_count = 3;
    let views = [
        (combined, Arc::default()),
        (Arc::clone(&sibling_fixture.model), Arc::default()),
    ];
    let mut renderer = renderer();
    let mut harness = Harness::default();
    let baseline = harness.draw(&mut renderer, &views, &[0.0, 0.0]);
    find_solid_patch(&baseline, 0, [224, 32, 16]);
    find_solid_patch(&baseline, 1, [17, 231, 93]);
    harness.bodygroups = vec![vec![0, 1], Vec::new()];
    harness.skin_remaps = vec![vec![0, 2], Vec::new()];
    let hidden = harness.draw(&mut renderer, &views, &[0.0, 0.0]);
    ready(&views[0].1, 0);
    ready(&views[1].1, 0);
    assert_eq!(hidden, baseline, "hidden skin changed rendered output");
    assert_eq!(harness.image_residency(0)["allocations"], 1);
    harness.skin_remaps.clear();
    let visible = harness.draw(&mut renderer, &views, &[0.0, 0.0]);
    ready(&views[0].1, 0);
    find_solid_patch(&visible, 0, [16, 32, 224]);
    assert_eq!(harness.image_residency(0)["allocations"], 2);
    assert_pane_equal(&visible, &baseline, 1);
    harness.skin_remaps = vec![vec![0, 2], Vec::new()];
    let hidden_again = harness.draw(&mut renderer, &views, &[0.0, 0.0]);
    assert_eq!(hidden_again, baseline);
    ready(&views[0].1, 0);
}

#[test]
#[ignore = "requires exclusive GPU and normal image capacity"]
fn iced_new_visible_slot_retains_only_submitted_complete_rasters_and_revisits_warm_images() {
    let a_fixture = model([224, 32, 16]);
    let b_fixture = model_frames(&(0..363).map(palette).collect::<Vec<_>>(), 3);
    let sibling_fixture = model([17, 231, 93]);
    let combined = paired_model(&a_fixture, &b_fixture);
    let view = (combined, Arc::default());
    let sibling = (Arc::clone(&sibling_fixture.model), Arc::default());
    let views = [view, sibling];
    let (b_source, _) = b_fixture.model.scene.materials[0]
        .texture
        .as_ref()
        .unwrap()
        .on_demand()
        .unwrap();
    let mut renderer = renderer();
    let mut harness = Harness::default();
    let exact_initial = harness.draw(&mut renderer, &views, &[time(255), 0.0]);
    ready(&views[0].1, 0);
    let initial = harness.draw_live(&mut renderer, &views, &[time(255), 0.0]);
    assert_eq!(initial, exact_initial);
    let a_patch = find_solid_patch(&initial, 0, [224, 32, 16]);
    find_solid_patch(&initial, 1, [17, 231, 93]);
    let pause = b_source.pause_page_reads_for_test();
    let profile = harness.observers[0].profile(1024);
    harness.bodygroups = vec![vec![0, 1], Vec::new()];
    let mut clock = time(255);
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut attempts = 0;
    loop {
        let pixels = harness.draw_live(&mut renderer, &views, &[clock, 0.0]);
        clock += 0.0001;
        attempts += 1;
        assert_pane_equal(&pixels, &initial, 0);
        assert_pane_equal(&pixels, &initial, 1);
        if pause.has_waiter() && attempts >= 4 {
            break;
        }
        assert!(
            attempts < 128 && Instant::now() < deadline,
            "new visible slot never reached its read gate"
        );
    }
    let counts = harness.observers[0].observed_presentation();
    assert!(counts["retained"].as_u64().unwrap() >= 4, "{counts}");
    assert_eq!(counts["current"], 0);
    assert_eq!(counts["previous"], 0);
    assert_eq!(counts["pending"], 0);
    profile.finish();
    let exact_pending = harness.draw_with_policy(
        &mut renderer,
        &views,
        &[clock, 0.0],
        ClockPolicy::Exact,
        false,
    );
    assert!(
        (0..180).all(|y| (0..160)
            .all(|x| exact_pending[(y * 320 + x) * 4..(y * 320 + x) * 4 + 4] == [0, 0, 0, 255])),
        "exact capture reused a retained raster"
    );
    assert_pane_equal(&exact_pending, &initial, 1);
    drop(pause);
    let completion_profile = harness.observers[0].profile(1024);
    let completed = loop {
        let pixels = harness.draw_live(&mut renderer, &views, &[clock, 0.0]);
        clock += 0.0001;
        attempts += 1;
        assert_pane_equal(&pixels, &initial, 1);
        if matches!(
            views[0].1.snapshot(),
            super::super::feedback::RenderStatus::Ready {
                unavailable_images: 0
            }
        ) {
            break pixels;
        }
        assert!(
            attempts < 128 && Instant::now() < deadline,
            "new visible slot never completed"
        );
    };
    let counts = harness.observers[0].observed_presentation();
    assert!(counts["current"].as_u64().unwrap() > 0, "{counts}");
    assert_eq!(counts["retained"], 0);
    completion_profile.finish();
    assert!(clock > time(255));
    assert_eq!((clock * 30.0).floor() as usize, 255);
    find_solid_patch(&completed, 0, palette(255));
    assert_eq!(
        &completed[(a_patch.1 * 320 + a_patch.0) * 4..(a_patch.1 * 320 + a_patch.0) * 4 + 4],
        a_patch.2
    );
    assert_eq!(harness.image_residency(0)["estimated_bytes"], 148);
    let uploads = harness.observers[0].lock().unwrap()["image_cache"]["uploads"]
        .as_u64()
        .unwrap();
    harness.bodygroups[0] = vec![0, 0];
    harness.draw(&mut renderer, &views, &[time(255), 0.0]);
    harness.bodygroups[0] = vec![0, 1];
    let revisited = harness.draw(&mut renderer, &views, &[time(255), 0.0]);
    assert_pane_equal(&revisited, &completed, 0);
    assert_eq!(
        harness.observers[0].lock().unwrap()["image_cache"]["uploads"],
        uploads
    );
    let live_revisited = harness.draw_live(&mut renderer, &views, &[time(255), 0.0]);
    assert_pane_equal(&live_revisited, &revisited, 0);
    let rejected = harness.draw_live(&mut renderer, &views, &[f32::NAN, 0.0]);
    assert_eq!(
        &rejected[(a_patch.1 * 320 + a_patch.0) * 4..(a_patch.1 * 320 + a_patch.0) * 4 + 4],
        [0, 0, 0, 255],
        "rejected live view retained a previously colored receiver",
    );
    assert!(matches!(
        views[0].1.snapshot(),
        super::super::feedback::RenderStatus::Rejected { .. }
    ));
    assert_pane_equal(&rejected, &initial, 1);
    harness.bodygroups.clear();
    let closed = harness.draw(&mut renderer, std::slice::from_ref(&views[1]), &[0.0]);
    color(
        solid_center(&closed, 160),
        [17, 231, 93],
        "sibling after main close",
    );
}

#[test]
#[ignore = "requires exclusive GPU and normal image capacity"]
fn iced_underwater_pending_slots_preserve_submitted_overlay_and_catch_up_clock() {
    let a_fixture = model([224, 32, 16]);
    let b_fixture = model([35, 55, 240]);
    let sibling_fixture = model([17, 231, 93]);
    let combined = paired_model(&a_fixture, &b_fixture);
    let normal: Vec<u8> = (0..16)
        .flat_map(|pixel| {
            if (pixel / 4 + pixel % 4) % 2 == 0 {
                [128, 128, 255, 64]
            } else {
                [255, 0, 255, 255]
            }
        })
        .collect();
    let resolver = MaterialResolver::prop_lighting_fixture(vec![
        ("materials/normal.vtf", fixture_vtf_bytes(4, 4, vformats::vtf::VtfFormat::Rgba8888, &[&normal])),
        ("materials/immersion.vmt", br#"Water { "$normalmap" "normal" "$underwateroverlay" "immersion_effect" }"#.to_vec()),
        ("materials/immersion_effect.vmt", br#"Refract { "$normalmap" "normal" "$refractamount" "0.03" "$refracttint" "[0.2 0.7 0.3]" Proxies { TextureScroll { "textureScrollVar" "$bumptransform" "textureScrollRate" "0.37" "textureScrollAngle" "0" } } }"#.to_vec()),
    ], Vec::new()).with_on_demand_textures();
    let resolution = resolver.resolve_with_base2(&[], "immersion");
    let material = resolution.material().unwrap();
    assert!(
        material
            .shader
            .water()
            .unwrap()
            .underwater_overlay
            .is_some()
    );
    let mut map = super::super::test_support::empty_preview(
        combined.scene.bounds_min,
        combined.scene.bounds_max,
    );
    map.scene = Arc::clone(&combined.scene);
    let map_scene = Arc::make_mut(&mut map.scene);
    map_scene.materials.push(MaterialSlot {
        resolution: resolution.outcome(),
        canonical_name: resolution.canonical_name().map(str::to_owned),
        name: "immersion".to_owned(),
        texture: material.texture.clone(),
        texture2: material.texture2.clone(),
        shader: material.shader.clone(),
        render_mode: material.render_mode,
        alpha_policy: material.render_mode.texture_alpha_policy(),
    });
    map_scene.stats.material_count += 1;
    map_scene.stats.resolved_material_count += 1;
    map.water_volumes = Some(
        gmpublished_domain::scene::map::MapWaterVolumes::box_for_tests(
            Vec3::splat(-100.0),
            Vec3::splat(100.0),
            "immersion".to_owned(),
        ),
    );
    let mut without_overlay = map.clone();
    let crate::bridge::materials::MaterialShader::Water(water) =
        &mut Arc::make_mut(&mut without_overlay.scene).materials[2].shader
    else {
        unreachable!()
    };
    Arc::make_mut(water).underwater_overlay = None;
    let views = [
        (combined, Arc::default()),
        (Arc::clone(&sibling_fixture.model), Arc::default()),
    ];
    let (b_source, _) = b_fixture.model.scene.materials[0]
        .texture
        .as_ref()
        .unwrap()
        .on_demand()
        .unwrap();
    let mut renderer = renderer();
    let mut harness = Harness {
        maps: vec![Some(Arc::new(without_overlay)), None],
        ..Harness::default()
    };
    let plain = harness.draw(&mut renderer, &views, &[0.0, 0.0]);
    harness.maps[0] = Some(Arc::new(map));
    let exact = harness.draw(&mut renderer, &views, &[0.0, 0.0]);
    assert_ne!(
        exact, plain,
        "underwater effect must actually alter rendered pixels"
    );
    assert_pane_equal(&exact, &plain, 1);
    let initial = harness.draw_live(&mut renderer, &views, &[0.0, 0.0]);
    assert_eq!(initial, exact);
    let pause = b_source.pause_page_reads_for_test();
    harness.bodygroups = vec![vec![0, 1], Vec::new()];
    let profile = harness.observers[0].profile(1024);
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut clock = 0.25;
    let mut attempts = 0;
    loop {
        let pixels = harness.draw_live(&mut renderer, &views, &[clock, 0.0]);
        clock += 0.25;
        attempts += 1;
        assert_pane_equal(&pixels, &initial, 0);
        assert_pane_equal(&pixels, &initial, 1);
        if pause.has_waiter() && attempts >= 4 {
            break;
        }
        assert!(
            attempts < 128 && Instant::now() < deadline,
            "underwater cold slot never reached its read gate"
        );
    }
    let counts = harness.observers[0].observed_presentation();
    assert!(counts["retained"].as_u64().unwrap() >= 4, "{counts}");
    assert_eq!(counts["current"], 0);
    assert_eq!(counts["previous"], 0);
    assert_eq!(counts["pending"], 0);
    profile.finish();
    let pending_exact = harness.draw_with_policy(
        &mut renderer,
        &views,
        &[clock, 0.0],
        ClockPolicy::Exact,
        false,
    );
    assert!((0..180).all(|y| {
        (0..160).all(|x| pending_exact[(y * 320 + x) * 4..(y * 320 + x) * 4 + 4] == [0, 0, 0, 255])
    }));
    assert_pane_equal(&pending_exact, &initial, 1);
    drop(pause);
    let profile = harness.observers[0].profile(1024);
    let (completed, completed_clock) = loop {
        let pixels = harness.draw_live(&mut renderer, &views, &[clock, 0.0]);
        attempts += 1;
        assert_pane_equal(&pixels, &initial, 1);
        if matches!(
            views[0].1.snapshot(),
            super::super::feedback::RenderStatus::Ready {
                unavailable_images: 0
            }
        ) {
            break (pixels, clock);
        }
        clock += 0.25;
        assert!(
            attempts < 128 && Instant::now() < deadline,
            "underwater cold slot never completed"
        );
    };
    let counts = harness.observers[0].observed_presentation();
    assert!(counts["current"].as_u64().unwrap() > 0, "{counts}");
    profile.finish();
    assert!(completed_clock >= 1.25);
    assert_ne!(completed, initial);
    let reference = harness.draw(&mut renderer, &views, &[completed_clock, 0.0]);
    assert_pane_equal(&completed, &reference, 0);
    assert_pane_equal(&reference, &initial, 1);
}
