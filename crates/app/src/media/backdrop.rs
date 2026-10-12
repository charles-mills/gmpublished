use iced::widget::image;

const BACKDROP_MAX_EDGE: u32 = 64;
const BACKDROP_SIGMA: f32 = 2.0;

/// Flatten transparency onto the icon-well color before blurring, so the
/// edges fade into the well. The GPU scales up the small result.
pub fn bake_blurred_backdrop(
    width: u32,
    height: u32,
    rgba: &[u8],
    well_rgb: [u8; 3],
) -> Option<image::Handle> {
    let flattened = flatten_onto(width, height, rgba, well_rgb)?;
    let (small_width, small_height) = scaled_edges(width, height)?;
    let small = ::image::imageops::thumbnail(&flattened, small_width, small_height);
    let blurred = ::image::imageops::blur(&small, BACKDROP_SIGMA);
    Some(image::Handle::from_rgba(
        blurred.width(),
        blurred.height(),
        blurred.into_raw(),
    ))
}

fn flatten_onto(
    width: u32,
    height: u32,
    rgba: &[u8],
    well_rgb: [u8; 3],
) -> Option<::image::RgbaImage> {
    let mut source = ::image::RgbaImage::from_raw(width, height, rgba.to_vec())?;
    for pixel in source.pixels_mut() {
        let alpha = u16::from(pixel[3]);
        if alpha == 255 {
            continue;
        }
        let inverse = 255 - alpha;
        for channel in 0..3 {
            let blended =
                (u16::from(pixel[channel]) * alpha + u16::from(well_rgb[channel]) * inverse) / 255;
            pixel[channel] = blended as u8;
        }
        pixel[3] = 255;
    }
    Some(source)
}

fn scaled_edges(width: u32, height: u32) -> Option<(u32, u32)> {
    if width == 0 || height == 0 {
        return None;
    }
    let longest = width.max(height);
    if longest <= BACKDROP_MAX_EDGE {
        return Some((width, height));
    }
    let scale = f64::from(BACKDROP_MAX_EDGE) / f64::from(longest);
    let scaled = |edge: u32| ((f64::from(edge) * scale).round() as u32).max(1);
    Some((scaled(width), scaled(height)))
}

#[cfg(test)]
mod tests {
    use super::bake_blurred_backdrop;

    const WELL: [u8; 3] = [0x10, 0x10, 0x10];

    #[test]
    fn bakes_a_downscaled_rgba_handle() {
        let rgba = vec![128_u8; 256 * 128 * 4];

        let handle = bake_blurred_backdrop(256, 128, &rgba, WELL).expect("backdrop should bake");

        let iced::widget::image::Handle::Rgba { width, height, .. } = handle else {
            panic!("backdrop must be a decoded RGBA handle");
        };
        assert_eq!((width, height), (64, 32));
    }

    #[test]
    fn transparent_pixels_flatten_to_the_well_color() {
        let rgba = [255, 255, 255, 0].repeat(16 * 16);

        let handle = bake_blurred_backdrop(16, 16, &rgba, WELL).expect("backdrop should bake");

        let iced::widget::image::Handle::Rgba { pixels, .. } = handle else {
            panic!("backdrop must be a decoded RGBA handle");
        };
        assert_eq!(&pixels[..4], &[0x10, 0x10, 0x10, 255]);
    }

    #[test]
    fn rejects_empty_buffers() {
        assert!(bake_blurred_backdrop(0, 0, &[], WELL).is_none());
    }
}
