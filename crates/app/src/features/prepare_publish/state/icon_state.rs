//! Icon thumbnail ownership and backdrop projection.

use super::thumbnail_demand;

pub(super) fn thumbnail_owner() -> thumbnail_demand::Owner {
    thumbnail_demand::Owner::PreparePublish
}

pub(super) fn seeded_backdrop(
    still: &iced::widget::image::Handle,
    well_rgb: [u8; 3],
) -> iced::widget::image::Handle {
    if let iced::widget::image::Handle::Rgba {
        width,
        height,
        ref pixels,
        ..
    } = *still
    {
        crate::media::backdrop::bake_blurred_backdrop(width, height, pixels, well_rgb)
            .unwrap_or_else(|| still.clone())
    } else {
        still.clone()
    }
}
