//! Workshop icon validation and display-preview preparation.

use super::{
    Arc, Duration, IconFormat, IconVerificationRequest, PathOperation, PreparePublishPathError,
    PreparedAnimation, UiError, VerifiedIcon, VerifiedIconPreview, WORKSHOP_ICON_MAX_SIZE,
    WORKSHOP_ICON_MIN_SIZE, WORKSHOP_ICON_PREVIEW_MAX_EDGE, fs, image, keys, thumbnail_animation,
};
use ::image::GenericImageView as _;

pub fn verify_icon_preview(
    request: IconVerificationRequest,
) -> Result<Arc<VerifiedIconPreview>, UiError> {
    verify_icon_preview_local(request).map(Arc::new)
}

fn verify_icon_preview_local(
    request: IconVerificationRequest,
) -> Result<VerifiedIconPreview, UiError> {
    let metadata = request.path.metadata().map_err(|source| {
        PreparePublishPathError::io(PathOperation::InspectMetadata, &request.path, source).into_ui()
    })?;
    if !metadata.is_file() {
        return Err(PreparePublishPathError::NotRegularFile { path: request.path }.into_ui());
    }
    if metadata.len() > WORKSHOP_ICON_MAX_SIZE {
        return Err(UiError::new(keys::ICON_TOO_LARGE));
    }
    if metadata.len() < WORKSHOP_ICON_MIN_SIZE {
        return Err(UiError::new(keys::ICON_TOO_SMALL));
    }

    let format = IconFormat::try_from(request.path.as_path())?;
    match format {
        IconFormat::Gif => verify_gif_icon(request, metadata.len()),
        IconFormat::Png | IconFormat::Jpeg => verify_still_icon(request, metadata.len(), format),
    }
}

fn verify_still_icon(
    request: IconVerificationRequest,
    byte_size: u64,
    format: IconFormat,
) -> Result<VerifiedIconPreview, UiError> {
    let image = ::image::open(&request.path).map_err(|error| {
        log::warn!("Prepare Publish icon decode failed: {error}");
        UiError::detailed(keys::IMAGE_ERROR, Some(error.to_string()))
    })?;
    let (source_width, source_height) = image.dimensions();
    let can_upscale = gmpublished_backend::publishing::workshop_icon_can_upscale(
        source_width,
        source_height,
        format == IconFormat::Gif,
    );

    let (rgba, width, height) = display_preview_rgba(&image);
    let icon = VerifiedIcon {
        display_path: request.display_path,
        source_path: request.path.clone(),
        path: request.path,
        format,
        width,
        height,
        byte_size,
        can_upscale,
    };

    let backdrop =
        crate::media::backdrop::bake_blurred_backdrop(width, height, &rgba, request.well_rgb);
    let still = image::Handle::from_rgba(width, height, rgba);
    Ok(VerifiedIconPreview {
        backdrop: backdrop.unwrap_or_else(|| still.clone()),
        still,
        icon,
        animation: None,
    })
}

fn verify_gif_icon(
    request: IconVerificationRequest,
    byte_size: u64,
) -> Result<VerifiedIconPreview, UiError> {
    let bytes = fs::read(&request.path).map_err(|source| {
        PreparePublishPathError::io(PathOperation::ReadFile, &request.path, source).into_ui()
    })?;
    let animation = PreparedAnimation::from_encoded_gif(&bytes, WORKSHOP_ICON_PREVIEW_MAX_EDGE)
        .map_err(|error| {
            log::warn!("Prepare Publish GIF icon preview could not be baked: {error}");
            UiError::detailed(keys::IMAGE_ERROR, Some(error.to_string()))
        })?;
    let backdrop = animation.frames().first().and_then(|frame| {
        crate::media::backdrop::bake_blurred_backdrop(
            frame.width(),
            frame.height(),
            frame.rgba_bytes(),
            request.well_rgb,
        )
    });
    let frames = animation
        .frames()
        .iter()
        .map(|frame| {
            (
                image::Handle::from_rgba(
                    frame.width(),
                    frame.height(),
                    frame.rgba_bytes().to_vec(),
                ),
                frame.delay(),
                frame.width(),
                frame.height(),
            )
        })
        .collect::<Vec<_>>();
    let Some((still, _delay, width, height)) = frames.first().cloned() else {
        return Err(UiError::new(keys::IMAGE_ERROR));
    };
    let animation = thumbnail_animation::Playback::from_frame_handles(
        frames
            .into_iter()
            .map(|(handle, delay, _, _)| (handle, nonzero_delay(delay))),
    )
    .ok_or_else(|| UiError::new(keys::IMAGE_ERROR))?;

    let icon = VerifiedIcon {
        display_path: request.display_path,
        source_path: request.path.clone(),
        path: request.path,
        format: IconFormat::Gif,
        width,
        height,
        byte_size,
        can_upscale: false,
    };

    Ok(VerifiedIconPreview {
        icon,
        backdrop: backdrop.unwrap_or_else(|| still.clone()),
        still,
        animation: Some(animation),
    })
}

/// Downscales oversized sources for the on-screen preview only; submit and
/// upload always read the original file. Matches the GIF path, which already
/// bakes at WORKSHOP_ICON_PREVIEW_MAX_EDGE.
fn display_preview_rgba(image: &::image::DynamicImage) -> (Vec<u8>, u32, u32) {
    let (width, height) = image.dimensions();
    if width <= WORKSHOP_ICON_PREVIEW_MAX_EDGE && height <= WORKSHOP_ICON_PREVIEW_MAX_EDGE {
        return (image.to_rgba8().into_raw(), width, height);
    }

    // Triangle is plenty for a display-only downscale and much cheaper than
    // CatmullRom at 4K-source sizes; the submitted file is never resampled here.
    let resized = image.resize(
        WORKSHOP_ICON_PREVIEW_MAX_EDGE,
        WORKSHOP_ICON_PREVIEW_MAX_EDGE,
        ::image::imageops::FilterType::Triangle,
    );
    let (width, height) = resized.dimensions();
    (resized.to_rgba8().into_raw(), width, height)
}

fn nonzero_delay(delay: Duration) -> Duration {
    if delay.is_zero() {
        Duration::from_millis(1)
    } else {
        delay
    }
}
