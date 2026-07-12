//! Raw RGBA image buffer for Steam avatars.
//!
//! Holds raw RGBA pixel bytes directly, with no built-in encoding; see
//! `steam::runtime::SteamAvatarRgba` for how they're consumed.

use std::fmt;

#[derive(Clone)]
pub struct RgbaImage {
    img: Vec<u8>,
    width: u32,
    height: u32,
}

impl RgbaImage {
    pub fn new(img: Vec<u8>, width: u32, height: u32) -> Self {
        Self { img, width, height }
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
            .field("resolution", &format!("{}px", self.width * self.height))
            .finish()
    }
}
