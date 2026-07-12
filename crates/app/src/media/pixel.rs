/// Byte length of a `width * height` RGBA8 buffer, `None` on overflow.
pub fn checked_rgba_len(width: u32, height: u32) -> Option<usize> {
    u64::from(width)
        .checked_mul(u64::from(height))?
        .checked_mul(4)?
        .try_into()
        .ok()
}
