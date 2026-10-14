//! Opt-in GPU acceptance gate for two simultaneous production shader widgets.
use super::{CaptureCamera, FixedView, Message};
use iced::advanced::renderer::Headless as _;
use iced::{Color, Length, Size, mouse};
use iced_runtime::{UserInterface, user_interface};
use serde_json::{Value, json};

fn pixels(
    renderer: &mut iced::Renderer,
    views: Vec<(FixedView, u32)>,
    height: u32,
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
    let _ = renderer.screenshot(size, 1.0, Color::BLACK);
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
        .filter(|shot| !shot.reflection_target && !shot.live_animation)
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
            let (solo, solo_width) =
                pixels(renderer, vec![(make(order[side]), widths[side])], height);
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
