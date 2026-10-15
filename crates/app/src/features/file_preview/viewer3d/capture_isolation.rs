//! Opt-in GPU acceptance gate for two simultaneous production shader widgets.
use super::{CaptureCamera, FixedView, Message};
use iced::advanced::renderer::Headless as _;
use iced::{Color, Length, Size, mouse};
use iced_runtime::{UserInterface, user_interface};
use serde_json::{Value, json};

fn pixels<P: iced::widget::shader::Program<Message>>(
    renderer: &mut iced::Renderer,
    views: Vec<(P, u32)>,
    height: u32,
    settle: bool,
) -> (Vec<u8>, u32) {
    let width =
        views.iter().map(|(_, width)| *width).sum::<u32>() + 16 * (views.len() as u32 - 1) + 24;
    let widgets = views.into_iter().map(|(view, width)| {
        iced::widget::shader(view)
            .width(Length::Fixed(width as f32))
            .height(Length::Fixed(height as f32))
            .into()
    });
    let element: iced::Element<'_, Message> =
        iced::widget::container(iced::widget::Row::with_children(widgets).spacing(16))
            .padding(12)
            .into();
    let mut ui = UserInterface::build(
        element,
        Size::new(width as f32, (height + 24) as f32),
        user_interface::Cache::default(),
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
    let size = Size::new(width, height + 24);
    if settle {
        let _ = renderer.screenshot(size, 1.0, Color::BLACK);
    }
    (renderer.screenshot(size, 1.0, Color::BLACK), width)
}

fn crop(bytes: &[u8], width: u32, x: u32, y: u32, w: u32, h: u32) -> Vec<u8> {
    (y..y + h)
        .flat_map(|row| {
            let start = ((row * width + x) * 4) as usize;
            bytes[start..start + (w * 4) as usize].iter().copied()
        })
        .collect()
}

pub(super) fn verify(
    renderer: &mut iced::Renderer,
    make: impl Fn(&CaptureCamera) -> FixedView,
    cameras: &[CaptureCamera],
    directory: &std::path::Path,
) -> Value {
    let cameras = cameras
        .iter()
        .filter(|shot| !shot.reflection_target && !shot.refraction_target && !shot.live_animation)
        .take(2)
        .collect::<Vec<_>>();
    assert_eq!(cameras.len(), 2, "isolation gate needs two fixed cameras");
    let mut results = Vec::new();
    for (iteration, (widths, height, reverse)) in [
        ([320, 480], 360, false),
        ([512, 256], 288, true),
        ([320, 480], 360, false),
    ]
    .into_iter()
    .enumerate()
    {
        let order = if reverse {
            [cameras[1], cameras[0]]
        } else {
            [cameras[0], cameras[1]]
        };
        let (pair, pair_width) = pixels(
            renderer,
            vec![(make(order[0]), widths[0]), (make(order[1]), widths[1])],
            height,
            true,
        );
        let file = format!("view-isolation-{iteration}.png");
        image::save_buffer(
            directory.join(&file),
            &pair,
            pair_width,
            height + 24,
            image::ColorType::Rgba8,
        )
        .expect("save view isolation image");
        for side in 0..2 {
            let (solo, solo_width) = pixels(
                renderer,
                vec![(make(order[side]), widths[side])],
                height,
                true,
            );
            let actual = crop(
                &pair,
                pair_width,
                if side == 0 { 12 } else { 28 + widths[0] },
                12,
                widths[side],
                height,
            );
            let expected = crop(&solo, solo_width, 12, 12, widths[side], height);
            assert_eq!(
                actual, expected,
                "camera/visibility/targets must be independent for {} in composite {iteration}",
                order[side].name
            );
            results.push(json!({"iteration":iteration,"camera":order[side].name,"dimensions":[widths[side],height],"pair_matches_solo_pixels":true,"pixel_crc32":format!("{:08x}",crc32fast::hash(&actual)),"file":file}));
        }
    }
    json!({"status":"passed","checks":results,"covers":["independent cameras and PVS","independent reflection targets","nonzero widget origins","resize","widget order/count changes"]})
}

struct BorrowedView<'a>(&'a FixedView, Option<super::super::camera::ViewId>);

impl iced::widget::shader::Program<Message> for BorrowedView<'_> {
    type State = ();
    type Primitive = super::ModelPrimitive;

    fn draw(&self, _: &(), cursor: mouse::Cursor, bounds: iced::Rectangle) -> Self::Primitive {
        let mut primitive = self.0.draw(&self.0.camera, cursor, bounds);
        if let Some(identity) = self.1 {
            primitive.view_id = identity;
        }
        primitive
    }
}

pub(super) fn verify_temporal(
    renderer: &mut iced::Renderer,
    make: impl Fn(&CaptureCamera) -> FixedView,
    cameras: &[CaptureCamera],
    directory: &std::path::Path,
) -> Value {
    let cameras = cameras
        .iter()
        .filter(|shot| !shot.reflection_target && !shot.refraction_target && !shot.live_animation)
        .take(2)
        .collect::<Vec<_>>();
    assert_eq!(
        cameras.len(),
        2,
        "temporal gate needs clear and dark-occluded Sun cameras"
    );
    let mut views = [
        make(cameras[0]),
        make(cameras[1]),
        make(cameras[0]),
        make(cameras[1]),
    ];
    let clear = (
        views[0].camera.position,
        views[0].camera.yaw,
        views[0].camera.pitch,
    );
    let blocked = (
        views[1].camera.position,
        views[1].camera.yaw,
        views[1].camera.pitch,
    );
    let mut checks = Vec::new();
    let mut clear_pixels = Vec::new();
    for step in 0..12 {
        let width = if step < 8 { 320 } else { 256 };
        let height = if step < 8 { 240 } else { 192 };
        let pose = if (3..6).contains(&step) {
            blocked
        } else {
            clear
        };
        for index in [0, 2] {
            let camera = &mut views[index].camera;
            camera.position = pose.0;
            camera.yaw = pose.1;
            camera.pitch = pose.2;
        }
        for view in &mut views {
            view.camera.water_time = step as f32 / 60.0;
            view.camera.model_lighting_time = f64::from(view.camera.water_time);
        }
        let order = if step % 2 == 0 {
            [0, 1, 2, 3]
        } else {
            [1, 0, 2, 3]
        };
        let (rgba, full_width) = pixels(
            renderer,
            order
                .iter()
                .map(|&index| (BorrowedView(&views[index], None), width))
                .collect(),
            height,
            false,
        );
        for actual_index in [0, 1] {
            let actual_slot = order
                .iter()
                .position(|&index| index == actual_index)
                .unwrap();
            let reference_slot = actual_index + 2;
            let actual = crop(
                &rgba,
                full_width,
                12 + actual_slot as u32 * (width + 16),
                12,
                width,
                height,
            );
            let reference = crop(
                &rgba,
                full_width,
                12 + reference_slot as u32 * (width + 16),
                12,
                width,
                height,
            );
            assert_eq!(
                actual, reference,
                "temporal view state leaked after reorder at step {step}, view {actual_index}"
            );
            if actual_index == 0 && step < 8 {
                clear_pixels.push(crc32fast::hash(&actual));
            }
            checks.push(json!({"step":step,"time":step as f32/60.0,"view":actual_index,"dimensions":[width,height],"matches_reference":true,"pixel_crc32":format!("{:08x}",crc32fast::hash(&actual))}));
        }
        if [0, 1, 3, 4, 6, 7, 8, 11].contains(&step) {
            image::save_buffer(
                directory.join(format!("temporal-isolation-{step:02}.png")),
                &rgba,
                full_width,
                height + 24,
                image::ColorType::Rgba8,
            )
            .expect("save temporal isolation image");
        }
    }
    assert_eq!(
        clear_pixels[0], clear_pixels[1],
        "unmoved clear view changed after reorder"
    );
    assert_eq!(
        clear_pixels[1], clear_pixels[2],
        "unmoved clear view changed after reorder"
    );
    assert_ne!(
        clear_pixels[3], clear_pixels[4],
        "blocked Sun must fade across advancing frames"
    );
    assert_ne!(
        clear_pixels[6], clear_pixels[7],
        "revealed Sun must fade across advancing frames"
    );
    for index in [0, 2] {
        views[index].camera.position = clear.0;
        views[index].camera.yaw = clear.1;
        views[index].camera.pitch = clear.2;
    }
    // Draw-order identities must break this gate; otherwise frozen captures
    // could conceal a transfer of temporal GPU history between widgets.
    let legacy_ids = views.each_ref().map(|view| view.camera.view_id);
    for view in &mut views {
        view.camera.water_time = 0.0;
        view.camera.model_lighting_time = 0.0;
    }
    let _ = pixels(
        renderer,
        (0..4)
            .map(|index| (BorrowedView(&views[index], Some(legacy_ids[index])), 320))
            .collect(),
        240,
        false,
    );
    for view in &mut views {
        view.camera.water_time = 1.0 / 60.0;
        view.camera.model_lighting_time = 1.0 / 60.0;
    }
    let order = [1, 0, 2, 3];
    let (negative, negative_width) = pixels(
        renderer,
        order
            .iter()
            .enumerate()
            .map(|(slot, &index)| (BorrowedView(&views[index], Some(legacy_ids[slot])), 320))
            .collect(),
        240,
        false,
    );
    let mut mismatched_channels = 0;
    for actual_index in [0, 1] {
        let slot = order
            .iter()
            .position(|&index| index == actual_index)
            .unwrap();
        let actual = crop(
            &negative,
            negative_width,
            12 + slot as u32 * 336,
            12,
            320,
            240,
        );
        let reference = crop(
            &negative,
            negative_width,
            12 + (actual_index + 2) as u32 * 336,
            12,
            320,
            240,
        );
        mismatched_channels += actual
            .iter()
            .zip(reference)
            .filter(|(a, b)| **a != *b)
            .count();
    }
    assert!(
        mismatched_channels > 0,
        "draw-order identity negative control must expose leaked Sun history"
    );
    image::save_buffer(
        directory.join("temporal-isolation-negative-control.png"),
        &negative,
        negative_width,
        264,
        image::ColorType::Rgba8,
    )
    .expect("save temporal negative control");
    json!({"status":"passed","checks":checks,"single_gpu_render_per_time":true,"draw_order_negative_control_mismatched_channels":mismatched_channels,"covers":["stable widget identity","advancing visibility history","reorder","camera changes","resize","independent reference views"]})
}
