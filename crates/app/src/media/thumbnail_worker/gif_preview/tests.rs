use super::*;
use gif::{Encoder, Frame, Repeat};

const TRANSPARENT: [u8; 4] = [0, 0, 0, 0];
const RED: [u8; 4] = [255, 0, 0, 255];
const GREEN: [u8; 4] = [0, 255, 0, 255];
const BLUE: [u8; 4] = [0, 0, 255, 255];
const YELLOW: [u8; 4] = [255, 255, 0, 255];

#[test]
fn decodes_three_generated_frames_with_dimensions_and_delays()
-> Result<(), Box<dyn std::error::Error>> {
    let bytes = gif_bytes(&[(RED, 30), (GREEN, 120), (BLUE, 250)])?;

    let frames = decode_gif_preview_frames(&bytes)?;

    assert_eq!(frames.len(), 3);
    assert_eq!(frames[0].width(), 6);
    assert_eq!(frames[0].height(), 4);
    assert_eq!(frames[1].width(), 6);
    assert_eq!(frames[1].height(), 4);
    assert_eq!(frames[2].width(), 6);
    assert_eq!(frames[2].height(), 4);
    assert_eq!(frames[0].delay(), Duration::from_millis(30));
    assert_eq!(frames[1].delay(), Duration::from_millis(120));
    assert_eq!(frames[2].delay(), Duration::from_millis(250));
    assert_eq!(decoded_byte_len(&frames), 6 * 4 * 4 * 3);
    Ok(())
}

#[test]
fn lazy_preview_holds_only_first_decoded_frame_initially() -> Result<(), Box<dyn std::error::Error>>
{
    let bytes = gif_bytes(&[(RED, 30), (GREEN, 120), (BLUE, 250)])?;

    let preview = decode_lazy_gif_preview(bytes, GIF_PREVIEW_MAX_EDGE)?;

    assert_eq!(preview.frame_count(), 3);
    assert_eq!(preview.initial_decoded_byte_len(), 6 * 4 * 4);
    assert_eq!(preview.initial_peak_decoded_byte_len(), 6 * 4 * 4);
    assert_eq!(
        preview.delays.as_ref(),
        &[
            Duration::from_millis(30),
            Duration::from_millis(120),
            Duration::from_millis(250),
        ]
    );
    let playback = LazyGifPlayback::new(preview);
    let second = playback.frame(1)?;
    assert_eq!(playback.frame_count(), 3);
    assert_eq!(second.width(), 6);
    assert_eq!(second.height(), 4);
    assert_eq!(second.delay(), Duration::from_millis(120));
    Ok(())
}

#[test]
fn lazy_playback_matches_eager_decode_across_loop_wrap() -> Result<(), Box<dyn std::error::Error>> {
    let bytes = gif_bytes(&[(RED, 30), (GREEN, 120), (BLUE, 250)])?;
    let eager = decode_gif_preview_frames(&bytes)?;
    let preview = decode_lazy_gif_preview(bytes, GIF_PREVIEW_MAX_EDGE)?;
    let playback = LazyGifPlayback::new(preview);

    for requested in [0_usize, 1, 2, 0, 1] {
        let lazy = playback.frame(requested)?;
        let eager = &eager[requested];
        assert_eq!(lazy.width(), eager.width());
        assert_eq!(lazy.height(), eager.height());
        assert_eq!(lazy.delay(), eager.delay());
        assert_eq!(lazy.rgba_bytes(), eager.rgba_bytes());
    }

    Ok(())
}

#[test]
fn bakes_display_sprite_atlas_with_clips_and_timing() -> Result<(), Box<dyn std::error::Error>> {
    let bytes = gif_bytes(&[(RED, 30), (GREEN, 120), (BLUE, 250)])?;

    let baked = bake_gif_animation(&bytes, GIF_PREVIEW_MAX_EDGE)?;

    assert_eq!(baked.frame_count(), 3);
    assert_eq!(baked.tile_width(), 6);
    assert_eq!(baked.tile_height(), 4);
    assert_eq!(baked.columns(), 2);
    assert_eq!(baked.rows(), 2);
    assert_eq!(baked.atlas_width(), 12);
    assert_eq!(baked.atlas_height(), 8);
    assert_eq!(baked.atlas_byte_len(), 12 * 8 * 4);
    assert_eq!(
        baked.frame_clip(0),
        Some(BakedAnimationFrameClip {
            x: 0,
            y: 0,
            width: 6,
            height: 4,
        })
    );
    assert_eq!(
        baked.frame_clip(2),
        Some(BakedAnimationFrameClip {
            x: 0,
            y: 4,
            width: 6,
            height: 4,
        })
    );
    assert_eq!(baked.total_duration(), Duration::from_millis(400));
    assert_eq!(baked.frame_index_at(Duration::from_millis(0)), 0);
    assert_eq!(baked.frame_index_at(Duration::from_millis(30)), 1);
    assert_eq!(baked.frame_index_at(Duration::from_millis(149)), 1);
    assert_eq!(baked.frame_index_at(Duration::from_millis(150)), 2);
    assert_eq!(baked.frame_index_at(Duration::from_millis(400)), 0);

    assert_eq!(atlas_pixel(&baked, 0, 0), RED);
    assert_eq!(atlas_pixel(&baked, 6, 0), GREEN);
    assert_eq!(atlas_pixel(&baked, 0, 4), BLUE);
    Ok(())
}

#[test]
fn bakes_steady_state_loop_fills_partial_first_frame() -> Result<(), Box<dyn std::error::Error>> {
    let f0 = indexed_frame(
        (0, 0),
        (3, 4),
        &[RED],
        &[0; 12],
        None,
        30,
        DisposalMethod::Keep,
    );
    let f1 = indexed_frame(
        (0, 0),
        (6, 4),
        &[GREEN],
        &[0; 24],
        None,
        30,
        DisposalMethod::Keep,
    );
    let bytes = indexed_gif_bytes(6, 4, &[f0, f1])?;

    let baked = bake_gif_animation(&bytes, GIF_PREVIEW_MAX_EDGE)?;

    assert_eq!(atlas_pixel(&baked, 0, 0), RED);
    assert_eq!(atlas_pixel(&baked, 4, 0), GREEN);
    Ok(())
}

#[test]
fn long_gif_bake_decimates_frames_and_merges_delays() -> Result<(), Box<dyn std::error::Error>> {
    let frames = (0..70)
        .map(|index| {
            (
                [index as u8, 0, 255_u8.saturating_sub(index as u8), 255],
                10,
            )
        })
        .collect::<Vec<_>>();
    let bytes = gif_bytes(&frames)?;

    let baked = bake_gif_animation(&bytes, GIF_PREVIEW_MAX_EDGE)?;

    assert_eq!(baked.frame_count(), BAKED_ANIMATION_MAX_FRAMES);
    assert_eq!(baked.total_duration(), Duration::from_millis(700));
    assert!(
        baked
            .cumulative_frame_times()
            .windows(2)
            .all(|times| times[0] < times[1])
    );
    Ok(())
}

#[test]
fn atlas_budget_edge_cap_budgets_the_whole_packed_grid() -> Result<(), Box<dyn std::error::Error>> {
    // 26 frames pack into a 6x5 grid (30 tiles), so the cap must divide the
    // budget by the grid, not the frame count: the per-frame answer would
    // pack a ~19 MB atlas and overshoot the 16 MiB budget.
    let cap = atlas_budget_edge_cap(26, BAKED_ANIMATION_MAX_ATLAS_BYTES)?;
    assert_eq!(cap, 373);
    let grid_bytes = |edge: usize| 30 * edge * edge * 4;
    assert!(grid_bytes(cap as usize) <= BAKED_ANIMATION_MAX_ATLAS_BYTES);
    assert!(grid_bytes(cap as usize + 1) > BAKED_ANIMATION_MAX_ATLAS_BYTES);
    let per_frame_cap = (BAKED_ANIMATION_MAX_ATLAS_BYTES as u64 / (4 * 26)).isqrt() as usize;
    assert!(grid_bytes(per_frame_cap) > BAKED_ANIMATION_MAX_ATLAS_BYTES);

    // At the frame ceiling the grid is exactly 8x8, giving a 256 px floor.
    assert_eq!(
        atlas_budget_edge_cap(BAKED_ANIMATION_MAX_FRAMES, BAKED_ANIMATION_MAX_ATLAS_BYTES)?,
        256
    );
    Ok(())
}

#[test]
fn budgeted_tile_edge_keeps_requested_edge_below_the_cap() -> Result<(), Box<dyn std::error::Error>>
{
    assert_eq!(
        budgeted_tile_edge(64, 26, BAKED_ANIMATION_MAX_ATLAS_BYTES)?,
        64
    );
    assert_eq!(
        budgeted_tile_edge(512, 26, BAKED_ANIMATION_MAX_ATLAS_BYTES)?,
        373
    );
    Ok(())
}

#[test]
fn atlas_budget_edge_cap_never_exceeds_budget_after_rounding()
-> Result<(), Box<dyn std::error::Error>> {
    for frame_count in 1..=BAKED_ANIMATION_MAX_FRAMES {
        for budget in [4096, 65536, 1 << 20, BAKED_ANIMATION_MAX_ATLAS_BYTES] {
            let cap = atlas_budget_edge_cap(frame_count, budget)? as usize;
            let columns = atlas_columns(frame_count)? as usize;
            let rows = atlas_rows(frame_count, atlas_columns(frame_count)?)? as usize;
            assert!(
                columns * rows * cap * cap * 4 <= budget,
                "cap {cap} exceeds budget {budget} for {frame_count} frames"
            );
        }
    }
    Ok(())
}

#[test]
fn over_budget_gif_bake_downscales_frames_to_fit_atlas_budget()
-> Result<(), Box<dyn std::error::Error>> {
    // 26 frames of 700x500 at a 1024 px request would pack a ~59 MB atlas.
    let frames = (0..26_u8)
        .map(|index| ([index, 128, 64, 255], 20))
        .collect::<Vec<_>>();
    let bytes = gif_bytes_with_size(700, 500, &frames)?;

    let baked = bake_gif_animation(&bytes, 1024)?;

    assert_eq!(baked.frame_count(), 26);
    assert!(baked.atlas_byte_len() <= BAKED_ANIMATION_MAX_ATLAS_BYTES);
    assert_eq!(baked.tile_width(), 373);
    assert_eq!(baked.tile_height(), 266);
    Ok(())
}

#[test]
fn large_gif_downscales_to_preview_edge_and_preserves_delays()
-> Result<(), Box<dyn std::error::Error>> {
    let bytes = gif_bytes_with_size(512, 384, &[(RED, 40), (BLUE, 120)])?;

    let frames = decode_gif_preview_frames(&bytes)?;

    assert_eq!(frames.len(), 2);
    assert_eq!(frames[0].width(), GIF_PREVIEW_MAX_EDGE);
    assert_eq!(frames[0].height(), 192);
    assert_eq!(frames[1].width(), GIF_PREVIEW_MAX_EDGE);
    assert_eq!(frames[1].height(), 192);
    assert_eq!(frames[0].delay(), Duration::from_millis(40));
    assert_eq!(frames[1].delay(), Duration::from_millis(120));
    assert_eq!(
        decoded_byte_len(&frames),
        GIF_PREVIEW_MAX_EDGE as usize * 192 * 4 * 2
    );
    Ok(())
}

#[test]
fn small_gif_is_not_upscaled() -> Result<(), Box<dyn std::error::Error>> {
    let bytes = gif_bytes(&[([16, 32, 48, 255], 30)])?;

    let frames = decode_gif_preview_frames(&bytes)?;

    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].width(), 6);
    assert_eq!(frames[0].height(), 4);
    assert_eq!(decoded_byte_len(&frames), 6 * 4 * 4);
    Ok(())
}

#[test]
fn transparent_resize_uses_alpha_aware_resampling() -> Result<(), Box<dyn std::error::Error>> {
    let source_width = GIF_PREVIEW_MAX_EDGE * 2 + 1;
    let source_height = 16;
    let midpoint = source_width / 2;
    let mut rgba = Vec::with_capacity(
        crate::media::pixel::checked_rgba_len(source_width, source_height)
            .expect("test dimensions should fit in memory"),
    );
    for _y in 0..source_height {
        for x in 0..source_width {
            if x < midpoint {
                rgba.extend_from_slice(&[0, 255, 0, 0]);
            } else {
                rgba.extend_from_slice(&[240, 16, 8, 255]);
            }
        }
    }

    let mut decoder = GifFrameDecoder::new(GIF_PREVIEW_MAX_EDGE);
    let frame = decoder.frame_from_rgba(
        0,
        source_width,
        source_height,
        rgba,
        Duration::from_millis(30),
    )?;

    assert_eq!(frame.width(), GIF_PREVIEW_MAX_EDGE);
    let semitransparent_pixels = frame
        .rgba_bytes()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[3] < 255)
        .collect::<Vec<_>>();
    assert!(
        !semitransparent_pixels.is_empty(),
        "resize should create at least one antialiased edge pixel"
    );
    assert!(
        semitransparent_pixels
            .iter()
            .all(|pixel| pixel[0] >= 220 && pixel[1] <= 24 && pixel[2] <= 16),
        "transparent hidden green should not bleed into resized edge pixels"
    );
    Ok(())
}

#[test]
fn any_and_keep_disposal_preserve_composited_pixels() -> Result<(), Box<dyn std::error::Error>> {
    let bytes = indexed_gif_bytes(
        3,
        1,
        &[
            indexed_frame((0, 0), (1, 1), &[RED], &[0], None, 10, DisposalMethod::Any),
            indexed_frame(
                (0, 0),
                (3, 1),
                &[TRANSPARENT, GREEN],
                &[0, 1, 0],
                Some(0),
                20,
                DisposalMethod::Keep,
            ),
            indexed_frame(
                (2, 0),
                (1, 1),
                &[BLUE],
                &[0],
                None,
                30,
                DisposalMethod::Keep,
            ),
        ],
    )?;

    let frames = decode_gif_preview_frames(&bytes)?;

    assert_eq!(rgba_pixels(&frames[0]), vec![RED, TRANSPARENT, TRANSPARENT]);
    assert_eq!(rgba_pixels(&frames[1]), vec![RED, GREEN, TRANSPARENT]);
    assert_eq!(rgba_pixels(&frames[2]), vec![RED, GREEN, BLUE]);
    Ok(())
}

#[test]
fn background_disposal_clears_frame_rect_after_presentation()
-> Result<(), Box<dyn std::error::Error>> {
    let bytes = indexed_gif_bytes(
        3,
        1,
        &[
            indexed_frame(
                (0, 0),
                (3, 1),
                &[RED],
                &[0, 0, 0],
                None,
                10,
                DisposalMethod::Keep,
            ),
            indexed_frame(
                (1, 0),
                (1, 1),
                &[GREEN],
                &[0],
                None,
                20,
                DisposalMethod::Background,
            ),
            indexed_frame(
                (2, 0),
                (1, 1),
                &[BLUE],
                &[0],
                None,
                30,
                DisposalMethod::Keep,
            ),
        ],
    )?;

    let frames = decode_gif_preview_frames(&bytes)?;

    assert_eq!(rgba_pixels(&frames[0]), vec![RED, RED, RED]);
    assert_eq!(rgba_pixels(&frames[1]), vec![RED, GREEN, RED]);
    assert_eq!(rgba_pixels(&frames[2]), vec![RED, TRANSPARENT, BLUE]);
    Ok(())
}

#[test]
fn previous_disposal_restores_canvas_before_next_frame() -> Result<(), Box<dyn std::error::Error>> {
    let bytes = indexed_gif_bytes(
        3,
        1,
        &[
            indexed_frame(
                (0, 0),
                (3, 1),
                &[RED],
                &[0, 0, 0],
                None,
                10,
                DisposalMethod::Keep,
            ),
            indexed_frame(
                (1, 0),
                (1, 1),
                &[GREEN],
                &[0],
                None,
                20,
                DisposalMethod::Previous,
            ),
            indexed_frame(
                (2, 0),
                (1, 1),
                &[BLUE],
                &[0],
                None,
                30,
                DisposalMethod::Keep,
            ),
        ],
    )?;

    let frames = decode_gif_preview_frames(&bytes)?;

    assert_eq!(rgba_pixels(&frames[0]), vec![RED, RED, RED]);
    assert_eq!(rgba_pixels(&frames[1]), vec![RED, GREEN, RED]);
    assert_eq!(rgba_pixels(&frames[2]), vec![RED, RED, BLUE]);
    Ok(())
}

#[test]
fn lazy_playback_matches_eager_decode_with_disposal_methods()
-> Result<(), Box<dyn std::error::Error>> {
    let bytes = indexed_gif_bytes(
        4,
        1,
        &[
            indexed_frame(
                (0, 0),
                (4, 1),
                &[RED],
                &[0, 0, 0, 0],
                None,
                10,
                DisposalMethod::Any,
            ),
            indexed_frame(
                (1, 0),
                (1, 1),
                &[GREEN],
                &[0],
                None,
                20,
                DisposalMethod::Background,
            ),
            indexed_frame(
                (2, 0),
                (1, 1),
                &[BLUE],
                &[0],
                None,
                30,
                DisposalMethod::Previous,
            ),
            indexed_frame(
                (3, 0),
                (1, 1),
                &[YELLOW],
                &[0],
                None,
                40,
                DisposalMethod::Keep,
            ),
        ],
    )?;
    let eager = decode_gif_preview_frames(&bytes)?;
    let preview = decode_lazy_gif_preview(bytes, GIF_PREVIEW_MAX_EDGE)?;
    let playback = LazyGifPlayback::new(preview);

    for requested in [0_usize, 1, 2, 3, 0, 3] {
        let lazy = playback.frame(requested)?;
        let eager = &eager[requested];
        assert_eq!(lazy.delay(), eager.delay());
        assert_eq!(lazy.rgba_bytes(), eager.rgba_bytes());
    }

    Ok(())
}

#[test]
fn invalid_bytes_return_typed_decode_error() {
    let error = decode_gif_preview_frames(b"not a gif")
        .expect_err("invalid bytes should produce a decode error");

    assert!(matches!(error, GifPreviewError::Decode(_)));
}

#[test]
fn oversized_logical_screen_is_rejected_by_decoder_limits() {
    let width = u16::try_from(GIF_DECODER_MAX_IMAGE_EDGE + 1)
        .expect("test limit should fit in GIF logical screen width");
    let bytes = gif_with_logical_screen_and_tiny_frame_bytes(width, 1);

    let error = decode_gif_preview_frames(&bytes)
        .expect_err("oversized logical screen should be rejected by limits");

    assert!(matches!(
        error,
        GifPreviewError::LogicalScreenTooLarge { .. }
    ));
}

#[test]
fn zero_delay_frame_clamps_to_minimum_delay() -> Result<(), Box<dyn std::error::Error>> {
    let bytes = gif_bytes(&[([16, 32, 48, 255], 0)])?;
    let frames = decode_gif_preview_frames(&bytes)?;

    assert_eq!(frames[0].delay(), MIN_FRAME_DELAY);
    Ok(())
}

#[test]
fn public_frame_constructor_clamps_zero_delay() -> Result<(), Box<dyn std::error::Error>> {
    let frame = GifPreviewFrame::new(2, 2, vec![0; 2 * 2 * 4], Duration::ZERO)?;

    assert_eq!(frame.delay(), MIN_FRAME_DELAY);
    Ok(())
}

#[test]
fn huge_nanosecond_value_saturates() {
    assert_eq!(duration_from_nanos_saturating(u128::MAX), Duration::MAX);
}

#[test]
fn zero_denominator_delay_clamps_without_overflow() {
    assert_eq!(duration_from_ms_ratio(0, 0), MIN_FRAME_DELAY);
    assert_eq!(duration_from_ms_ratio(1, 0), MIN_FRAME_DELAY);
}

#[test]
fn tiny_delay_clamps_to_minimum_delay() {
    assert_eq!(duration_from_ms_ratio(1, 1), MIN_FRAME_DELAY);
    assert_eq!(duration_from_ms_ratio(1, 1_000), MIN_FRAME_DELAY);
}

fn gif_bytes(frames: &[([u8; 4], u32)]) -> GifPreviewResult<Vec<u8>> {
    gif_bytes_with_size(6, 4, frames)
}

fn atlas_pixel(baked: &BakedAnimation, x: u32, y: u32) -> [u8; 4] {
    let index = rgba_index(baked.atlas_width(), x, y).expect("test pixel should fit");
    baked.atlas_rgba_bytes()[index..index + 4]
        .try_into()
        .expect("test pixel should contain four bytes")
}

fn gif_with_logical_screen_and_tiny_frame_bytes(width: u16, height: u16) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(35);
    bytes.extend_from_slice(b"GIF89a");
    bytes.extend_from_slice(&width.to_le_bytes());
    bytes.extend_from_slice(&height.to_le_bytes());
    bytes.extend_from_slice(&[0x80, 0x00, 0x00]);
    bytes.extend_from_slice(&[0x00, 0x00, 0xff, 0xff, 0xff, 0xff]);
    bytes.extend_from_slice(&[
        0x2c, // Image descriptor.
        0x00, 0x00, // Left.
        0x00, 0x00, // Top.
        0x01, 0x00, // Width.
        0x01, 0x00, // Height.
        0x00, // No local color table.
        0x02, // LZW minimum code size.
        0x02, // Data block size.
        0x44, 0x01, // One-pixel image data.
        0x00, // Image data terminator.
    ]);
    bytes.push(0x3b);
    bytes
}

fn gif_bytes_with_size(
    width: u32,
    height: u32,
    frames: &[([u8; 4], u32)],
) -> GifPreviewResult<Vec<u8>> {
    let specs = frames
        .iter()
        .map(|(color, delay_ms)| {
            let width = u16::try_from(width).expect("test GIF width should fit u16");
            let height = u16::try_from(height).expect("test GIF height should fit u16");
            indexed_frame(
                (0, 0),
                (width, height),
                &[*color],
                &vec![0; usize::from(width) * usize::from(height)],
                None,
                *delay_ms,
                DisposalMethod::Keep,
            )
        })
        .collect::<Vec<_>>();
    let width = u16::try_from(width).expect("test GIF width should fit u16");
    let height = u16::try_from(height).expect("test GIF height should fit u16");
    indexed_gif_bytes(width, height, &specs)
}

struct IndexedGifFrame {
    left: u16,
    top: u16,
    width: u16,
    height: u16,
    palette: Vec<u8>,
    pixels: Vec<u8>,
    transparent: Option<u8>,
    delay_ms: u32,
    dispose: DisposalMethod,
}

fn indexed_frame(
    origin: (u16, u16),
    size: (u16, u16),
    colors: &[[u8; 4]],
    pixels: &[u8],
    transparent: Option<u8>,
    delay_ms: u32,
    dispose: DisposalMethod,
) -> IndexedGifFrame {
    let palette = colors
        .iter()
        .flat_map(|color| [color[0], color[1], color[2]])
        .collect::<Vec<_>>();
    IndexedGifFrame {
        left: origin.0,
        top: origin.1,
        width: size.0,
        height: size.1,
        palette,
        pixels: pixels.to_vec(),
        transparent,
        delay_ms,
        dispose,
    }
}

fn indexed_gif_bytes(
    width: u16,
    height: u16,
    frames: &[IndexedGifFrame],
) -> GifPreviewResult<Vec<u8>> {
    let mut bytes = Vec::new();
    {
        let mut encoder =
            Encoder::new(&mut bytes, width, height, &[]).map_err(GifPreviewError::Encode)?;
        encoder
            .set_repeat(Repeat::Infinite)
            .map_err(GifPreviewError::Encode)?;

        for spec in frames {
            let mut frame = Frame::from_palette_pixels(
                spec.width,
                spec.height,
                spec.pixels.clone(),
                spec.palette.clone(),
                spec.transparent,
            );
            frame.left = spec.left;
            frame.top = spec.top;
            frame.delay = gif_delay_cs(spec.delay_ms);
            frame.dispose = spec.dispose;
            encoder
                .write_frame(&frame)
                .map_err(GifPreviewError::Encode)?;
        }
    }
    Ok(bytes)
}

fn gif_delay_cs(delay_ms: u32) -> u16 {
    u16::try_from(delay_ms / 10).expect("test GIF delay should fit u16")
}

fn rgba_pixels(frame: &GifPreviewFrame) -> Vec<[u8; 4]> {
    frame
        .rgba_bytes()
        .chunks_exact(4)
        .map(|pixel| [pixel[0], pixel[1], pixel[2], pixel[3]])
        .collect()
}
