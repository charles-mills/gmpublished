use super::BcFormat;

#[derive(Clone, Copy, Debug)]
pub(super) struct TextureUploadLevel<'a> {
    pub(super) rgba: &'a [u8],
    pub(super) width: u32,
    pub(super) height: u32,
}

impl TextureUploadLevel<'_> {
    pub(super) fn is_valid(self) -> bool {
        texture_rgba_len(self.width, self.height).is_some_and(|len| len == self.rgba.len())
    }
}

pub(super) fn visit_bc_rgba(
    format: BcFormat,
    width: u32,
    height: u32,
    data: &[u8],
    mut visit: impl FnMut(&[u8]),
) -> Option<usize> {
    if width == 0 || height == 0 {
        return None;
    }
    let count = usize::try_from(u64::from(width).checked_mul(u64::from(height))?).ok()?;
    let mut scratch = [0_u8; 256];
    for y in 0..height {
        let mut x = 0;
        while x < width {
            let pixels = (width - x).min(64);
            let row_bytes = pixels as usize * 4;
            vformats::vtf::decode_bc_rgba_region_into(
                format,
                [width, height],
                [x, y, pixels, 1],
                data,
                &mut scratch[..row_bytes],
                row_bytes,
            )
            .ok()?;
            visit(&scratch[..row_bytes]);
            x += pixels;
        }
    }
    Some(count)
}

pub(super) fn texture_rgba_len(width: u32, height: u32) -> Option<usize> {
    let pixels = u64::from(width).checked_mul(u64::from(height))?;
    let bytes = pixels.checked_mul(4)?;
    usize::try_from(bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_scratch_visits_bc_pixels_in_the_original_row_order() {
        for format in [BcFormat::Bc1, BcFormat::Bc2, BcFormat::Bc3] {
            let dimensions = [130_u32, 9];
            let bytes =
                (0..dimensions[0].div_ceil(4) * dimensions[1].div_ceil(4) * format.block_bytes())
                    .map(|value| value as u8)
                    .collect::<Vec<_>>();
            let full = vformats::vtf::decode_bc_rgba(
                format,
                dimensions[0],
                dimensions[1],
                &bytes,
                &vformats::Limits::default(),
            )
            .unwrap();
            let mut visited = Vec::new();
            assert_eq!(
                visit_bc_rgba(format, dimensions[0], dimensions[1], &bytes, |row| {
                    assert!(row.len() <= 256);
                    visited.extend_from_slice(row);
                }),
                Some((dimensions[0] * dimensions[1]) as usize)
            );
            assert_eq!(visited, full.rgba);
        }
        let mut visits = 0;
        assert_eq!(
            visit_bc_rgba(BcFormat::Bc1, 1, 1, &[0; 7], |_| visits += 1),
            None
        );
        assert_eq!(visits, 0);
    }
}
