use super::*;
use crate::{
    bridge::{gma::PreviewArchive, materials::MaterialResolver},
    media::preview_model::{MaterialSlot, ModelPreview, ModelStats, RenderScene},
    test_support::{GmaFixtureBuilder, fixture_vtf_bytes},
};
use iced::advanced::renderer::Headless as _;
use iced::{Color, Length, Size, mouse};
use iced_runtime::UserInterface;

fn model(rgb: [u8; 3]) -> Arc<ModelPreview> {
    let pixels = [rgb[0], rgb[1], rgb[2], 255].repeat(16);
    let archive = PreviewArchive::from_gma(
        GmaFixtureBuilder::new("lifecycle")
            .entry(
                "materials/lifecycle.vmt",
                b"UnlitGeneric { $basetexture lifecycle $nocull 1 $nofog 1 }".to_vec(),
            )
            .entry(
                "materials/lifecycle.vtf",
                fixture_vtf_bytes(4, 4, vformats::vtf::VtfFormat::Rgba8888, &[&pixels]),
            )
            .build(),
    )
    .unwrap();
    let resolver = MaterialResolver::new(Arc::new(archive), None);
    let resolution = resolver.resolve_with_base2(&[], "lifecycle");
    let material = resolution.material().unwrap();
    let vertices = [(-1., -1.), (1., -1.), (1., 1.), (-1., 1.)].map(|(x, y)| ModelVertex {
        position: Vec3::new(x, y, 0.),
        normal: Vec3::new(0., 0., 1.),
        uv: [x, y],
        lightmap_uv: [0.; 2],
        lightmap_bump_step: 0.,
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
    Arc::new(ModelPreview {
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
    })
}

type View = (
    Arc<ModelPreview>,
    Arc<super::super::feedback::RenderFeedback>,
);

fn draw(renderer: &mut iced::Renderer, views: &[View]) -> Vec<u8> {
    let widgets = views.iter().map(|(model, feedback)| {
        iced::widget::shader(super::super::camera::Viewer3d {
            model: Arc::clone(model),
            render_feedback: Arc::clone(feedback),
            content_id: 1,
            skin_remap: Vec::new(),
            bodygroup_choices: Vec::new(),
            phy_debug_visible: false,
            pose: None,
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
    let size = Size::new(320., 180.);
    let mut ui = UserInterface::build(element, size, Default::default(), renderer);
    ui.draw(
        renderer,
        &iced::Theme::Dark,
        &iced::advanced::renderer::Style {
            text_color: Color::WHITE,
        },
        mouse::Cursor::Unavailable,
    );
    let mut pixels = Vec::new();
    for _ in 0..4 {
        pixels = renderer.screenshot(Size::new(320, 180), 1., Color::BLACK);
    }
    pixels
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
    assert_eq!(policy.material_image_bytes, 64);
    assert_eq!(policy.material_image_allocations, 1);
    let mut renderer = iced::futures::executor::block_on(iced::Renderer::new(
        iced::Font::default(),
        iced::Pixels(16.),
        Some("wgpu"),
    ))
    .expect("native GPU renderer");
    let a = (model([224, 32, 16]), Arc::default());
    let b = (model([16, 224, 32]), Arc::default());
    let red = center(&draw(&mut renderer, std::slice::from_ref(&a)), 160);
    assert!(red[0] > red[1] + 100, "visible red control: {red:?}");
    ready(&a.1, 0);
    let green = center(&draw(&mut renderer, std::slice::from_ref(&b)), 160);
    assert!(
        green[1] > green[0] + 100,
        "same-scene B must recover: {green:?}"
    );
    ready(&b.1, 0);
    let simultaneous = draw(&mut renderer, &[b.clone(), a.clone()]);
    ready(&b.1, 0);
    ready(&a.1, 1);
    assert_eq!(
        center(&simultaneous, 80),
        green,
        "blocked A cannot evict active B"
    );
    let recovered = center(&draw(&mut renderer, std::slice::from_ref(&a)), 160);
    assert_eq!(
        recovered, red,
        "closing B must recover the existing A scene"
    );
    ready(&a.1, 0);
    let equivalent = (model([224, 32, 16]), Arc::default());
    let shared = draw(&mut renderer, &[a.clone(), equivalent.clone()]);
    ready(&a.1, 0);
    ready(&equivalent.1, 0);
    assert_eq!(center(&shared, 80), red);
    assert_eq!(center(&shared, 240), red);
}
