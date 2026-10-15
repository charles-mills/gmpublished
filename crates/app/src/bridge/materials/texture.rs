use std::collections::BTreeSet;
#[cfg(not(test))]
use std::panic;
#[cfg(not(test))]
use std::sync::OnceLock;

#[cfg(not(test))]
use iced::wgpu;
use vformats::Limits;

use super::{
    ResolvedBcMip, ResolvedTexture, ResolvedTextureMip, ResolvedTexturePayload,
    TextureReadTransfer, linear_to_srgb_byte, normalize_archive_path, srgb_byte_to_linear,
};

pub fn decode_vtf_rgba(bytes: &[u8]) -> Result<vformats::vtf::RgbaImage, vformats::vtf::VtfError> {
    vformats::vtf::parse(bytes, &Limits::default())?.decode_rgba(0, 0, 0, &Limits::default())
}

pub(super) struct DecodedVtfImage {
    pub image: vformats::vtf::RgbaImage,
    pub original_dimensions: [u32; 2],
    pub flags: u32,
    pub reflectivity_bits: [u32; 3],
}

/// Decodes the stored mip nearest at or below `max_dimension`, falling back
/// to mip 0 on failure. Returns original mip-0 dimensions as well: BSP texel
/// UVs normalize against them, not the decoded mip size.
pub(super) fn decode_vtf_rgba_max(
    bytes: &[u8],
    max_dimension: Option<u32>,
) -> Result<DecodedVtfImage, vformats::vtf::VtfError> {
    let limits = Limits::default();
    let vtf = vformats::vtf::parse(bytes, &limits)?;
    let (source_width, source_height) = (vtf.width(), vtf.height());
    let side = source_width.max(source_height);
    let mip = match max_dimension {
        Some(max) if max > 0 && side > max => {
            let want = (f64::from(side) / f64::from(max)).log2().ceil() as u8;
            want.min(vtf.mip_count().saturating_sub(1))
        }
        _ => 0,
    };
    let image = if mip == 0 {
        vtf.decode_rgba(0, 0, 0, &limits)?
    } else {
        vtf.decode_rgba(0, 0, mip, &limits)
            .or_else(|_| vtf.decode_rgba(0, 0, 0, &limits))?
    };
    Ok(DecodedVtfImage {
        image,
        original_dimensions: [source_width, source_height],
        flags: vtf.flags(),
        reflectivity_bits: vtf.reflectivity().map(f32::to_bits),
    })
}

pub(super) fn push_matching_path(
    path: &str,
    matches: &impl Fn(&str) -> bool,
    seen: &mut BTreeSet<String>,
    out: &mut Vec<String>,
) {
    let Some(path) = normalize_archive_path(path) else {
        return;
    };
    if matches(&path) && seen.insert(path.clone()) {
        out.push(path);
    }
}

pub(super) fn downscale_resolved_texture(
    texture: ResolvedTexture,
    max_dimension: Option<u32>,
) -> ResolvedTexture {
    let Some(max_dimension) = max_dimension else {
        return texture;
    };
    let source_width = texture.width;
    let source_height = texture.height;
    let Some((width, height)) =
        downscaled_texture_dimensions(source_width, source_height, max_dimension)
    else {
        return texture;
    };
    if width == source_width && height == source_height {
        return texture;
    }
    let Some(expected_len) = crate::media::pixel::checked_rgba_len(source_width, source_height)
    else {
        return texture;
    };
    let ResolvedTexture {
        payload,
        original_width,
        original_height,
        water_fallback,
        flags,
        reflectivity_bits,
        ..
    } = texture;
    let ResolvedTexturePayload::Rgba { rgba, .. } = payload else {
        return ResolvedTexture {
            payload,
            width: source_width,
            height: source_height,
            original_width,
            original_height,
            water_fallback,
            flags,
            reflectivity_bits,
        };
    };
    if rgba.len() != expected_len {
        return ResolvedTexture {
            payload: ResolvedTexturePayload::Rgba {
                rgba,
                mip_chain: Vec::new(),
            },
            width: source_width,
            height: source_height,
            original_width,
            original_height,
            water_fallback,
            flags,
            reflectivity_bits,
        };
    }
    let image = ::image::RgbaImage::from_raw(source_width, source_height, rgba)
        .expect("RGBA length was checked before resize");
    let resized = ::image::imageops::resize(
        &image,
        width,
        height,
        ::image::imageops::FilterType::Triangle,
    );
    let mut result = ResolvedTexture::rgba(
        resized.into_raw(),
        width,
        height,
        original_width,
        original_height,
        water_fallback,
    );
    result.flags = flags;
    result.reflectivity_bits = reflectivity_bits;
    result
}

pub(super) fn with_generated_mip_chain(
    mut tex: ResolvedTexture,
    transfer: TextureReadTransfer,
) -> ResolvedTexture {
    if tex.water_fallback {
        return tex;
    }
    if let ResolvedTexturePayload::Rgba { rgba, mip_chain } = &mut tex.payload {
        *mip_chain = generate_mip_chain(rgba, tex.width, tex.height, transfer).unwrap_or_default();
    }
    tex
}

pub(super) fn resolved_bc_texture(
    bytes: &[u8],
    max_dimension: Option<u32>,
) -> Option<ResolvedTexture> {
    let vtf = vformats::vtf::parse(bytes, &Limits::default()).ok()?;
    let raw = vtf.raw_bc(0, 0)?;
    let mips = raw
        .mips
        .iter()
        .rev()
        .map(|mip| ResolvedBcMip {
            data: mip.data.to_vec(),
            width: mip.width.max(1),
            height: mip.height.max(1),
        })
        .collect::<Vec<_>>();
    let mips = drop_bc_mips_to_max_dimension(mips, max_dimension);
    let mut texture = ResolvedTexture::bc(raw.format, mips, raw.width.max(1), raw.height.max(1))?;
    texture.flags = Some(vtf.flags());
    texture.reflectivity_bits = Some(vtf.reflectivity().map(f32::to_bits));
    Some(texture)
}

pub(super) fn drop_bc_mips_to_max_dimension(
    mut mips: Vec<ResolvedBcMip>,
    max_dimension: Option<u32>,
) -> Vec<ResolvedBcMip> {
    let Some(max_dimension) = max_dimension.filter(|dimension| *dimension > 0) else {
        return mips;
    };
    while mips.len() > 1
        && mips
            .first()
            .is_some_and(|mip| mip.width.max(mip.height) > max_dimension)
    {
        mips.remove(0);
    }
    mips
}

#[cfg(test)]
pub fn generate_srgb_mip_chain(
    base_rgba: &[u8],
    width: u32,
    height: u32,
) -> Option<Vec<ResolvedTextureMip>> {
    generate_mip_chain(base_rgba, width, height, TextureReadTransfer::SrgbToLinear)
}

fn generate_mip_chain(
    base_rgba: &[u8],
    width: u32,
    height: u32,
    transfer: TextureReadTransfer,
) -> Option<Vec<ResolvedTextureMip>> {
    if width == 0
        || height == 0
        || base_rgba.len() != crate::media::pixel::checked_rgba_len(width, height)?
    {
        return None;
    }

    let mut levels = Vec::new();
    let mut previous_rgba = base_rgba.to_vec();
    let mut previous_width = width;
    let mut previous_height = height;

    while previous_width > 1 || previous_height > 1 {
        let next_width = previous_width.div_ceil(2);
        let next_height = previous_height.div_ceil(2);
        let next_rgba =
            downsample_mip_level(&previous_rgba, previous_width, previous_height, transfer)?;

        levels.push(ResolvedTextureMip {
            rgba: next_rgba.clone(),
            width: next_width,
            height: next_height,
        });
        previous_rgba = next_rgba;
        previous_width = next_width;
        previous_height = next_height;
    }

    Some(levels)
}

fn downsample_mip_level(
    rgba: &[u8],
    width: u32,
    height: u32,
    transfer: TextureReadTransfer,
) -> Option<Vec<u8>> {
    if width == 0
        || height == 0
        || rgba.len() != crate::media::pixel::checked_rgba_len(width, height)?
    {
        return None;
    }
    let next_width = width.div_ceil(2);
    let next_height = height.div_ceil(2);
    let mut next = Vec::with_capacity(crate::media::pixel::checked_rgba_len(
        next_width,
        next_height,
    )?);

    for y in 0..next_height {
        for x in 0..next_width {
            let mut rgb_linear = [0.0_f32; 3];
            let mut alpha = 0.0_f32;
            let mut count = 0.0_f32;
            for source_y in (y * 2)..((y * 2 + 2).min(height)) {
                for source_x in (x * 2)..((x * 2 + 2).min(width)) {
                    let offset = rgba_offset(source_x, source_y, width)?;
                    for channel in 0..3 {
                        rgb_linear[channel] += match transfer {
                            TextureReadTransfer::SrgbToLinear => {
                                srgb_byte_to_linear(rgba[offset + channel])
                            }
                            TextureReadTransfer::Unmodified => {
                                f32::from(rgba[offset + channel]) / 255.0
                            }
                        };
                    }
                    alpha += f32::from(rgba[offset + 3]);
                    count += 1.0;
                }
            }
            for value in rgb_linear {
                next.push(match transfer {
                    TextureReadTransfer::SrgbToLinear => linear_to_srgb_byte(value / count),
                    TextureReadTransfer::Unmodified => {
                        (value / count * 255.0).round().clamp(0.0, 255.0) as u8
                    }
                });
            }
            next.push((alpha / count).round().clamp(0.0, 255.0) as u8);
        }
    }

    Some(next)
}

pub(super) fn rgba_offset(x: u32, y: u32, width: u32) -> Option<usize> {
    let pixel = u64::from(y)
        .checked_mul(u64::from(width))?
        .checked_add(u64::from(x))?;
    usize::try_from(pixel.checked_mul(4)?).ok()
}

pub(super) fn downscaled_texture_dimensions(
    width: u32,
    height: u32,
    max_dimension: u32,
) -> Option<(u32, u32)> {
    if width == 0 || height == 0 || max_dimension == 0 {
        return None;
    }
    if width <= max_dimension && height <= max_dimension {
        return Some((width, height));
    }
    let scale = f64::from(max_dimension) / f64::from(width.max(height));
    let scaled_width = (f64::from(width) * scale)
        .round()
        .clamp(1.0, f64::from(max_dimension)) as u32;
    let scaled_height = (f64::from(height) * scale)
        .round()
        .clamp(1.0, f64::from(max_dimension)) as u32;
    Some((scaled_width, scaled_height))
}

pub(super) fn force_opaque_alpha(rgba: &mut [u8]) {
    for alpha in rgba.iter_mut().skip(3).step_by(4) {
        *alpha = 255;
    }
}

#[cfg(not(test))]
pub(super) fn bc_supported() -> bool {
    static SUPPORTED: OnceLock<bool> = OnceLock::new();
    *SUPPORTED.get_or_init(|| {
        panic::catch_unwind(|| {
            let instance = wgpu::Instance::default();
            futures::executor::block_on(
                instance.request_adapter(&wgpu::RequestAdapterOptions::default()),
            )
            .is_ok_and(|adapter| {
                adapter
                    .features()
                    .contains(wgpu::Features::TEXTURE_COMPRESSION_BC)
            })
        })
        .unwrap_or(false)
    })
}

#[cfg(test)]
pub(super) fn bc_supported() -> bool {
    // Tests opt into BC payloads explicitly; adapter discovery needs a real GPU.
    false
}
