use std::fmt;

#[derive(Clone)]
pub struct RgbaImage {
    img: Vec<u8>,
    width: u32,
    height: u32,
}

impl RgbaImage {
    const BYTES_PER_PIXEL: usize = 4;

    #[must_use]
    pub fn try_new(img: Vec<u8>, width: u32, height: u32) -> Option<Self> {
        let expected = (width as usize)
            .checked_mul(height as usize)?
            .checked_mul(Self::BYTES_PER_PIXEL)?;

        (img.len() == expected).then_some(Self { img, width, height })
    }

    pub fn into_rgba_parts(self) -> (Vec<u8>, u32, u32) {
        (self.img, self.width, self.height)
    }
}

impl fmt::Debug for RgbaImage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RgbaImage")
            .field("bytes", &self.img.len())
            .field("width", &self.width)
            .field("height", &self.height)
            .field(
                "resolution",
                &format!("{}px", u64::from(self.width) * u64::from(self.height)),
            )
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::RgbaImage;

    #[test]
    fn an_exactly_sized_buffer_is_accepted_and_round_trips() {
        let image =
            RgbaImage::try_new(vec![0u8; 2 * 3 * 4], 2, 3).expect("2x3 RGBA is well formed");

        assert_eq!(image.into_rgba_parts(), (vec![0u8; 24], 2, 3));
    }

    #[test]
    fn a_buffer_that_disagrees_with_its_dimensions_is_rejected() {
        assert!(RgbaImage::try_new(vec![0u8; 23], 2, 3).is_none(), "short");
        assert!(RgbaImage::try_new(vec![0u8; 25], 2, 3).is_none(), "long");
        assert!(
            RgbaImage::try_new(Vec::new(), 64, 64).is_none(),
            "an empty buffer is not a 64x64 image"
        );
    }

    #[test]
    fn degenerate_dimensions_do_not_wrap_into_a_passing_size() {
        assert!(RgbaImage::try_new(Vec::new(), 0, 0).is_some());
        assert!(RgbaImage::try_new(vec![0u8; 4], 0, 1).is_none());
        assert!(RgbaImage::try_new(vec![0u8; 4], u32::MAX, u32::MAX).is_none());
    }
}
