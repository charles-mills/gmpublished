//! ThumbHash perceptual placeholders.
//!
//! [`encode`] turns a decoded thumbnail into a ~25-byte hash; [`decode`] turns
//! that hash back into a tiny RGBA image the GPU upscales into a blurred
//! placeholder. The hash codec is the `thumbhash` crate (algorithm:
//! <https://evanw.github.io/thumbhash/>); this module only box-downsamples the
//! encode input to the ≤100px the codec asserts on.

// ThumbHash captures only a ~7x7 luminance basis, so a small box-averaged
// input is indistinguishable from a full-resolution one while staying under
// the codec's 100px limit.
const ENCODE_MAX_EDGE: u32 = 64;

/// Encodes straight-RGBA pixels into a ThumbHash. ThumbHash is presentation
/// garnish, so a malformed buffer yields `None` (no placeholder) rather than an
/// error.
pub fn encode(width: u32, height: u32, rgba: &[u8]) -> Option<Box<[u8]>> {
    let (w, h) = (width as usize, height as usize);
    let expected = w.checked_mul(h)?.checked_mul(4)?;
    if expected == 0 || rgba.len() != expected {
        return None;
    }

    let (small_w, small_h, small) = box_downsample(width, height, rgba, ENCODE_MAX_EDGE);
    Some(thumbhash::rgba_to_thumb_hash(small_w, small_h, &small).into_boxed_slice())
}

/// Decodes a ThumbHash into a tiny `(width, height, straight-RGBA)` image, or
/// `None` if the hash is malformed or its dimensions overflow `u32`.
pub fn decode(hash: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let (w, h, rgba) = thumbhash::thumb_hash_to_rgba(hash).ok()?;
    Some((u32::try_from(w).ok()?, u32::try_from(h).ok()?, rgba))
}

/// Box-averages an image to fit within `max_edge`, preserving aspect ratio.
/// Images already within the bound are copied unchanged.
fn box_downsample(width: u32, height: u32, rgba: &[u8], max_edge: u32) -> (usize, usize, Vec<u8>) {
    let source_max = width.max(height);
    if source_max <= max_edge {
        return (width as usize, height as usize, rgba.to_vec());
    }

    let dst_w = (u64::from(width) * u64::from(max_edge) / u64::from(source_max)).max(1) as usize;
    let dst_h = (u64::from(height) * u64::from(max_edge) / u64::from(source_max)).max(1) as usize;
    let (src_w, src_h) = (width as usize, height as usize);
    let mut out = vec![0_u8; dst_w * dst_h * 4];

    for dy in 0..dst_h {
        let y0 = dy * src_h / dst_h;
        let y1 = ((dy + 1) * src_h / dst_h).max(y0 + 1);
        for dx in 0..dst_w {
            let x0 = dx * src_w / dst_w;
            let x1 = ((dx + 1) * src_w / dst_w).max(x0 + 1);
            let (mut r, mut g, mut b, mut a, mut count) = (0_u32, 0_u32, 0_u32, 0_u32, 0_u32);
            for y in y0..y1 {
                let row = (y * src_w) * 4;
                for x in x0..x1 {
                    let px = row + x * 4;
                    r += u32::from(rgba[px]);
                    g += u32::from(rgba[px + 1]);
                    b += u32::from(rgba[px + 2]);
                    a += u32::from(rgba[px + 3]);
                    count += 1;
                }
            }
            let dst = (dy * dst_w + dx) * 4;
            out[dst] = (r / count) as u8;
            out[dst + 1] = (g / count) as u8;
            out[dst + 2] = (b / count) as u8;
            out[dst + 3] = (a / count) as u8;
        }
    }

    (dst_w, dst_h, out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gradient(width: u32, height: u32) -> Vec<u8> {
        let mut rgba = vec![0_u8; (width * height * 4) as usize];
        for y in 0..height {
            for x in 0..width {
                let px = ((y * width + x) * 4) as usize;
                rgba[px] = (x * 255 / width.max(1)) as u8;
                rgba[px + 1] = (y * 255 / height.max(1)) as u8;
                rgba[px + 2] = 128;
                rgba[px + 3] = 255;
            }
        }
        rgba
    }

    #[test]
    fn encode_decode_round_trips_to_a_non_empty_tiny_image() {
        let hash = encode(200, 120, &gradient(200, 120)).expect("gradient encodes");
        assert!(hash.len() >= 5 && hash.len() <= 30);

        let (w, h, rgba) = decode(&hash).expect("hash decodes");
        assert!(w > 0 && h > 0);
        assert!(w <= 40 && h <= 40, "placeholder stays tiny: {w}x{h}");
        assert_eq!(rgba.len(), (w * h * 4) as usize);
        assert!(w > h, "landscape aspect is preserved");
    }

    #[test]
    fn encode_rejects_malformed_buffers() {
        assert!(encode(0, 0, &[]).is_none());
        assert!(encode(2, 2, &[0; 3]).is_none());
    }

    #[test]
    fn decode_rejects_truncated_hashes() {
        assert!(decode(&[]).is_none());
        assert!(decode(&[0, 0, 0]).is_none());
    }

    #[test]
    fn small_inputs_skip_downsampling() {
        let (w, h, out) = box_downsample(8, 4, &gradient(8, 4), ENCODE_MAX_EDGE);
        assert_eq!((w, h), (8, 4));
        assert_eq!(out, gradient(8, 4));
    }
}
