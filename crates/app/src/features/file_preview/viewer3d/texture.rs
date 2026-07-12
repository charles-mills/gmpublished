use super::{BcFormat, ResolvedBcMip, wgpu};

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

pub(super) fn write_texture_level(
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    mip_level: u32,
    level: TextureUploadLevel<'_>,
) {
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        level.rgba,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(level.width.max(1) * 4),
            rows_per_image: Some(level.height.max(1)),
        },
        wgpu::Extent3d {
            width: level.width.max(1),
            height: level.height.max(1),
            depth_or_array_layers: 1,
        },
    );
}

pub(super) fn bc_texture_format(format: BcFormat) -> wgpu::TextureFormat {
    match format {
        BcFormat::Bc1 => wgpu::TextureFormat::Bc1RgbaUnormSrgb,
        BcFormat::Bc2 => wgpu::TextureFormat::Bc2RgbaUnormSrgb,
        BcFormat::Bc3 => wgpu::TextureFormat::Bc3RgbaUnormSrgb,
    }
}

pub(super) fn bc_mip_is_valid(format: BcFormat, mip: &ResolvedBcMip) -> bool {
    bc_mip_byte_len(format, mip.width, mip.height).is_some_and(|len| len == mip.data.len())
}

pub(super) fn write_bc_texture_level(
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    mip_level: u32,
    format: BcFormat,
    mip: &ResolvedBcMip,
) {
    let width = mip.width.max(1);
    let height = mip.height.max(1);
    let block_bytes = format.block_bytes();
    let blocks_wide = width.div_ceil(4);
    let blocks_high = height.div_ceil(4);
    let row_bytes = blocks_wide * block_bytes;
    let padded_row_bytes = align_to(row_bytes, wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
    let upload;
    let data = if padded_row_bytes == row_bytes {
        mip.data.as_slice()
    } else {
        let Some(padded_len) = usize::try_from(padded_row_bytes)
            .ok()
            .and_then(|row| row.checked_mul(usize::try_from(blocks_high).ok()?))
        else {
            return;
        };
        let Some(row_bytes_usize) = usize::try_from(row_bytes).ok() else {
            return;
        };
        let Some(padded_row_bytes_usize) = usize::try_from(padded_row_bytes).ok() else {
            return;
        };
        upload = padded_bc_rows(
            &mip.data,
            row_bytes_usize,
            padded_row_bytes_usize,
            usize::try_from(blocks_high).unwrap_or(0),
            padded_len,
        );
        upload.as_slice()
    };
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        data,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(padded_row_bytes),
            rows_per_image: Some(blocks_high),
        },
        wgpu::Extent3d {
            width: blocks_wide * 4,
            height: blocks_high * 4,
            depth_or_array_layers: 1,
        },
    );
}

pub(super) fn padded_bc_rows(
    data: &[u8],
    row_bytes: usize,
    padded_row_bytes: usize,
    rows: usize,
    padded_len: usize,
) -> Vec<u8> {
    let mut padded = vec![0_u8; padded_len];
    for row in 0..rows {
        let source_start = row.saturating_mul(row_bytes);
        let source_end = source_start.saturating_add(row_bytes);
        let target_start = row.saturating_mul(padded_row_bytes);
        let target_end = target_start.saturating_add(row_bytes);
        if let (Some(source), Some(target)) = (
            data.get(source_start..source_end),
            padded.get_mut(target_start..target_end),
        ) {
            target.copy_from_slice(source);
        }
    }
    padded
}

pub(super) fn align_to(value: u32, alignment: u32) -> u32 {
    if alignment == 0 {
        value
    } else {
        value.div_ceil(alignment) * alignment
    }
}

pub(super) fn bc_mip_byte_len(format: BcFormat, width: u32, height: u32) -> Option<usize> {
    let blocks = width
        .max(1)
        .div_ceil(4)
        .checked_mul(height.max(1).div_ceil(4))?;
    let bytes = blocks.checked_mul(format.block_bytes())?;
    usize::try_from(bytes).ok()
}

pub(super) fn decode_bc_texture(
    format: BcFormat,
    width: u32,
    height: u32,
    data: &[u8],
) -> Option<Vec<u8>> {
    let expected_len = bc_mip_byte_len(format, width, height)?;
    if data.len() != expected_len {
        return None;
    }
    let width = width.max(1);
    let height = height.max(1);
    let mut rgba = vec![0_u8; texture_rgba_len(width, height)?];
    let blocks_wide = width.div_ceil(4);
    let blocks_high = height.div_ceil(4);
    let block_bytes = usize::try_from(format.block_bytes()).ok()?;
    for block_y in 0..blocks_high {
        for block_x in 0..blocks_wide {
            let block_index =
                usize::try_from(block_y.checked_mul(blocks_wide)?.checked_add(block_x)?).ok()?;
            let block_start = block_index.checked_mul(block_bytes)?;
            let block = data.get(block_start..block_start + block_bytes)?;
            match format {
                BcFormat::Bc1 => {
                    decode_bc1_block(block, block_x, block_y, width, height, &mut rgba)?;
                }
                BcFormat::Bc2 => {
                    decode_bc2_block(block, block_x, block_y, width, height, &mut rgba)?;
                }
                BcFormat::Bc3 => {
                    decode_bc3_block(block, block_x, block_y, width, height, &mut rgba)?;
                }
            }
        }
    }
    Some(rgba)
}

pub(super) fn decode_bc1_block(
    block: &[u8],
    block_x: u32,
    block_y: u32,
    width: u32,
    height: u32,
    rgba: &mut [u8],
) -> Option<()> {
    decode_bc_color_block(
        block,
        true,
        &mut BcDecodeBlockTarget {
            block_x,
            block_y,
            width,
            height,
            rgba,
        },
        None,
    )
}

pub(super) fn decode_bc2_block(
    block: &[u8],
    block_x: u32,
    block_y: u32,
    width: u32,
    height: u32,
    rgba: &mut [u8],
) -> Option<()> {
    let mut alpha = [255_u8; 16];
    let encoded = u64::from_le_bytes(block.get(0..8)?.try_into().ok()?);
    for (index, value) in alpha.iter_mut().enumerate() {
        let nibble = ((encoded >> (index * 4)) & 0x0f) as u8;
        *value = nibble * 17;
    }
    decode_bc_color_block(
        block.get(8..16)?,
        false,
        &mut BcDecodeBlockTarget {
            block_x,
            block_y,
            width,
            height,
            rgba,
        },
        Some(&alpha),
    )
}

pub(super) fn decode_bc3_block(
    block: &[u8],
    block_x: u32,
    block_y: u32,
    width: u32,
    height: u32,
    rgba: &mut [u8],
) -> Option<()> {
    let alpha = decode_bc3_alpha(block.get(0..8)?)?;
    decode_bc_color_block(
        block.get(8..16)?,
        false,
        &mut BcDecodeBlockTarget {
            block_x,
            block_y,
            width,
            height,
            rgba,
        },
        Some(&alpha),
    )
}

pub(super) fn decode_bc3_alpha(block: &[u8]) -> Option<[u8; 16]> {
    let alpha0 = *block.first()?;
    let alpha1 = *block.get(1)?;
    let mut palette = [0_u8; 8];
    palette[0] = alpha0;
    palette[1] = alpha1;
    if alpha0 > alpha1 {
        for i in 1..=6 {
            palette[i + 1] =
                (((7 - i) as u16 * u16::from(alpha0) + i as u16 * u16::from(alpha1) + 3) / 7) as u8;
        }
    } else {
        for i in 1..=4 {
            palette[i + 1] =
                (((5 - i) as u16 * u16::from(alpha0) + i as u16 * u16::from(alpha1) + 2) / 5) as u8;
        }
        palette[6] = 0;
        palette[7] = 255;
    }
    let mut indices = 0_u64;
    for (shift, byte) in block.get(2..8)?.iter().enumerate() {
        indices |= u64::from(*byte) << (shift * 8);
    }
    let mut alpha = [255_u8; 16];
    for (index, value) in alpha.iter_mut().enumerate() {
        let palette_index = ((indices >> (index * 3)) & 0x07) as usize;
        *value = palette[palette_index];
    }
    Some(alpha)
}

pub(super) struct BcDecodeBlockTarget<'a> {
    pub(super) block_x: u32,
    pub(super) block_y: u32,
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) rgba: &'a mut [u8],
}

pub(super) fn decode_bc_color_block(
    block: &[u8],
    punchthrough_alpha: bool,
    target: &mut BcDecodeBlockTarget<'_>,
    alpha_override: Option<&[u8; 16]>,
) -> Option<()> {
    let color0 = u16::from_le_bytes(block.get(0..2)?.try_into().ok()?);
    let color1 = u16::from_le_bytes(block.get(2..4)?.try_into().ok()?);
    let indices = u32::from_le_bytes(block.get(4..8)?.try_into().ok()?);
    let mut palette = [[0_u8; 4]; 4];
    palette[0] = rgb565_to_rgba(color0, 255);
    palette[1] = rgb565_to_rgba(color1, 255);
    if color0 > color1 || !punchthrough_alpha {
        palette[2] = interpolate_rgba(palette[0], palette[1], 2, 1, 3);
        palette[3] = interpolate_rgba(palette[0], palette[1], 1, 2, 3);
    } else {
        palette[2] = interpolate_rgba(palette[0], palette[1], 1, 1, 2);
        palette[3] = [0, 0, 0, 0];
    }

    for local_y in 0..4 {
        for local_x in 0..4 {
            let x = target.block_x * 4 + local_x;
            let y = target.block_y * 4 + local_y;
            if x >= target.width || y >= target.height {
                continue;
            }
            let pixel_index = usize::try_from(local_y * 4 + local_x).ok()?;
            let palette_index = ((indices >> (pixel_index * 2)) & 0x03) as usize;
            let mut pixel = palette[palette_index];
            if let Some(alpha) = alpha_override {
                pixel[3] = alpha[pixel_index];
            }
            let offset = texture_rgba_offset(x, y, target.width)?;
            target
                .rgba
                .get_mut(offset..offset + 4)?
                .copy_from_slice(&pixel);
        }
    }
    Some(())
}

pub(super) fn rgb565_to_rgba(value: u16, alpha: u8) -> [u8; 4] {
    let r = ((value >> 11) & 0x1f) as u8;
    let g = ((value >> 5) & 0x3f) as u8;
    let b = (value & 0x1f) as u8;
    [
        (r << 3) | (r >> 2),
        (g << 2) | (g >> 4),
        (b << 3) | (b >> 2),
        alpha,
    ]
}

pub(super) fn interpolate_rgba(
    left: [u8; 4],
    right: [u8; 4],
    left_weight: u16,
    right_weight: u16,
    total: u16,
) -> [u8; 4] {
    [
        ((u16::from(left[0]) * left_weight + u16::from(right[0]) * right_weight) / total) as u8,
        ((u16::from(left[1]) * left_weight + u16::from(right[1]) * right_weight) / total) as u8,
        ((u16::from(left[2]) * left_weight + u16::from(right[2]) * right_weight) / total) as u8,
        ((u16::from(left[3]) * left_weight + u16::from(right[3]) * right_weight) / total) as u8,
    ]
}

pub(super) fn texture_rgba_len(width: u32, height: u32) -> Option<usize> {
    let pixels = u64::from(width).checked_mul(u64::from(height))?;
    let bytes = pixels.checked_mul(4)?;
    usize::try_from(bytes).ok()
}

pub(super) fn texture_rgba_offset(x: u32, y: u32, width: u32) -> Option<usize> {
    let pixel = y.checked_mul(width)?.checked_add(x)?;
    usize::try_from(pixel.checked_mul(4)?).ok()
}
