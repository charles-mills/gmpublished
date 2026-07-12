use std::sync::OnceLock;

use iced::{
    Font,
    widget::{image, svg},
};

use crate::media::thumbnail_worker::ThumbnailDecoder;

pub mod fonts {
    use std::{io::Read, sync::OnceLock};

    use super::Font;

    // Primary UI family, bundled as STATIC weight instances (fontTools
    // varLib.instancer over the source variable font, slnt pinned at 0).
    // fontdb registers a variable font as a single weight-400 face, so
    // `Font::with_name("Inter")` with Semibold/Bold silently rendered
    // Regular; static 400/600/700 faces make those weights real.
    include!(concat!(env!("OUT_DIR"), "/font_segments.rs"));

    const COMPRESSED_FONTS: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/bundled_fonts.lzma"));

    fn unpacked_fonts() -> &'static [u8] {
        static FONTS: OnceLock<Box<[u8]>> = OnceLock::new();
        FONTS.get_or_init(|| {
            let mut decompressed = Vec::with_capacity(FONTS_UNCOMPRESSED_LEN);
            lzma_rust2::LzmaReader::new_mem_limit(COMPRESSED_FONTS, u32::MAX, None)
                .and_then(|mut reader| reader.read_to_end(&mut decompressed))
                .expect("bundled fonts must decompress");
            assert_eq!(
                decompressed.len(),
                FONTS_UNCOMPRESSED_LEN,
                "bundled font length must match its build-time segment table"
            );
            decompressed.into_boxed_slice()
        })
    }

    fn font_bytes(index: usize) -> &'static [u8] {
        let (start, len) = FONT_SEGMENTS[index];
        &unpacked_fonts()[start..start + len]
    }

    pub fn inter_regular_bytes() -> &'static [u8] {
        font_bytes(0)
    }

    pub const fn default_font() -> Font {
        Font::with_name("Inter")
    }

    /// All bundled font byte slices in builder registration order.
    pub fn bundled_fonts() -> [&'static [u8]; 5] {
        [
            font_bytes(0),
            font_bytes(1),
            font_bytes(2),
            font_bytes(3),
            font_bytes(4),
        ]
    }
}

pub mod icons {
    use super::svg;

    /// Declares a `pub(crate) fn $name() -> svg::Handle` that wraps the SVG
    /// bytes at `$path` (relative to this file), for the many icons that are
    /// otherwise a plain "load these bytes" one-liner.
    macro_rules! svg_icon {
        ($name:ident, $path:literal) => {
            pub fn $name() -> svg::Handle {
                svg::Handle::from_memory(include_bytes!($path))
            }
        };
    }

    svg_icon!(akar_arrow_left, "../ui/images/akar-arrow-left.svg");
    svg_icon!(akar_download, "../ui/images/akar-download.svg");
    svg_icon!(akar_enlarge, "../ui/images/akar-enlarge.svg");
    svg_icon!(akar_folder, "../ui/images/akar-folder.svg");
    svg_icon!(akar_folder_add, "../ui/images/akar-folder-add.svg");
    svg_icon!(akar_reduce, "../ui/images/akar-reduce.svg");
    svg_icon!(check, "../ui/images/check.svg");
    svg_icon!(chevron_up, "../ui/images/chevron-up.svg");
    svg_icon!(chevron_down, "../ui/images/chevron-down.svg");
    svg_icon!(circle_alert, "../ui/images/circle-alert.svg");
    svg_icon!(circle_plus, "../ui/images/circle-plus.svg");
    svg_icon!(cloud_download, "../ui/images/cloud-download.svg");
    svg_icon!(cloud_upload, "../ui/images/cloud-upload.svg");
    svg_icon!(
        context_cloud_download,
        "../ui/images/context-menu/cloud-download.svg"
    );
    svg_icon!(context_copy, "../ui/images/context-menu/copy.svg");
    svg_icon!(context_folder, "../ui/images/context-menu/folder.svg");
    svg_icon!(
        context_folder_add,
        "../ui/images/context-menu/folder-add.svg"
    );
    svg_icon!(context_image, "../ui/images/context-menu/image.svg");
    svg_icon!(
        context_link_chain,
        "../ui/images/context-menu/link-chain.svg"
    );
    svg_icon!(context_link_out, "../ui/images/context-menu/link-out.svg");
    svg_icon!(cross, "../ui/images/cross.svg");
    svg_icon!(dead, "../ui/icons/dead.svg");
    svg_icon!(download_count, "../ui/images/download-count.svg");
    svg_icon!(folder, "../ui/images/folder.svg");
    svg_icon!(folder_add, "../ui/images/folder-add.svg");
    svg_icon!(gear, "../ui/images/gear.svg");

    /// Multicolor artwork: render WITHOUT a tint override so its natural
    /// blue/white fills show.
    pub fn gmod_logo() -> svg::Handle {
        svg::Handle::from_memory(include_bytes!("../ui/images/gmod-logo.svg"))
    }

    svg_icon!(link_chain, "../ui/images/link-chain.svg");

    #[cfg(feature = "asset-studio")]
    svg_icon!(mode_fly, "../ui/images/mode-fly.svg");
    #[cfg(feature = "asset-studio")]
    svg_icon!(mode_walk, "../ui/images/mode-walk.svg");

    svg_icon!(route_downloader, "../ui/images/route-downloader.svg");
    svg_icon!(
        route_installed_addons,
        "../ui/images/route-installed-addons.svg"
    );
    svg_icon!(route_my_workshop, "../ui/images/route-my-workshop.svg");
    svg_icon!(route_size_analyzer, "../ui/images/route-size-analyzer.svg");
    svg_icon!(search, "../ui/images/search.svg");
    svg_icon!(star_filled, "../ui/images/star-filled.svg");
    svg_icon!(tag_point, "../ui/images/tag-point.svg");

    /// Solid downward triangle (16x8) tinted to the tooltip background for
    /// the size-analyzer anchored tooltip arrow. Flip vertically for the
    /// upward variant when the tooltip is placed below its square.
    pub fn tooltip_arrow() -> svg::Handle {
        svg::Handle::from_memory(include_bytes!("../ui/images/tooltip-arrow.svg"))
    }
}

/// Bundled 16px silkicon rasters for file-browser rows.
pub mod silkicons {
    use super::{OnceLock, ThumbnailDecoder, image};
    use crate::widgets::file_types::SilkIcon;

    const SILKICON_MAX_EDGE: u32 = 16;
    const SILKICON_COUNT: usize = 13;

    const BYTES: [(&str, &[u8]); SILKICON_COUNT] = [
        (
            "bricks",
            include_bytes!("../ui/images/silkicons/bricks.png"),
        ),
        (
            "comments",
            include_bytes!("../ui/images/silkicons/comments.png"),
        ),
        (
            "folder",
            include_bytes!("../ui/images/silkicons/folder.png"),
        ),
        ("font", include_bytes!("../ui/images/silkicons/font.png")),
        ("map", include_bytes!("../ui/images/silkicons/map.png")),
        (
            "page_white",
            include_bytes!("../ui/images/silkicons/page_white.png"),
        ),
        (
            "page_white_text",
            include_bytes!("../ui/images/silkicons/page_white_text.png"),
        ),
        (
            "page_white_wrench",
            include_bytes!("../ui/images/silkicons/page_white_wrench.png"),
        ),
        ("photo", include_bytes!("../ui/images/silkicons/photo.png")),
        (
            "picture_link",
            include_bytes!("../ui/images/silkicons/picture_link.png"),
        ),
        (
            "script_code",
            include_bytes!("../ui/images/silkicons/script_code.png"),
        ),
        ("sound", include_bytes!("../ui/images/silkicons/sound.png")),
        ("wand", include_bytes!("../ui/images/silkicons/wand.png")),
    ];

    static HANDLES: OnceLock<[image::Handle; SILKICON_COUNT]> = OnceLock::new();

    pub fn silkicon(icon: SilkIcon) -> image::Handle {
        let index = match icon {
            SilkIcon::Bricks => 0,
            SilkIcon::Comments => 1,
            SilkIcon::Folder => 2,
            SilkIcon::Font => 3,
            SilkIcon::Map => 4,
            SilkIcon::PageWhite => 5,
            SilkIcon::PageWhiteText => 6,
            SilkIcon::PageWhiteWrench => 7,
            SilkIcon::Photo => 8,
            SilkIcon::PictureLink => 9,
            SilkIcon::ScriptCode => 10,
            SilkIcon::Sound => 11,
            SilkIcon::Wand => 12,
        };
        HANDLES.get_or_init(decode_all)[index].clone()
    }

    fn decode_all() -> [image::Handle; SILKICON_COUNT] {
        std::array::from_fn(|index| {
            let (name, bytes) = BYTES[index];
            let mut decoder = ThumbnailDecoder::new();
            let thumbnail = decoder
                .decode_and_resize_bytes(bytes, SILKICON_MAX_EDGE)
                .unwrap_or_else(|error| panic!("bundled silkicon `{name}` must decode: {error}"));
            let metadata = thumbnail.metadata();
            image::Handle::from_rgba(
                metadata.width,
                metadata.height,
                thumbnail.rgba_bytes().to_vec(),
            )
        })
    }
}

/// Static raster assets decoded once into codec-free Iced RGBA handles.
pub mod images {
    use super::{OnceLock, ThumbnailDecoder, image};

    const DEFAULT_ICON_BYTES: &[u8] = include_bytes!("../ui/images/gmpublisher_default_icon.png");
    const STEAM_ANONYMOUS_BYTES: &[u8] = include_bytes!("../ui/images/steam_anonymous.jpg");
    const MAX_STATIC_EDGE: u32 = 512;

    static DEFAULT_ICON: OnceLock<image::Handle> = OnceLock::new();
    static DEFAULT_ICON_BACKDROP: parking_lot::Mutex<Option<([u8; 3], image::Handle)>> =
        parking_lot::Mutex::new(None);
    static STEAM_ANONYMOUS: OnceLock<image::Handle> = OnceLock::new();

    pub fn default_icon() -> image::Handle {
        DEFAULT_ICON
            .get_or_init(|| decode_raster("default workshop icon", DEFAULT_ICON_BYTES))
            .clone()
    }

    /// Cached per icon-well color so a theme switch re-flattens transparency
    /// onto the new well; in practice this bakes once per session.
    pub fn default_icon_backdrop(well_rgb: [u8; 3]) -> image::Handle {
        let mut cached = DEFAULT_ICON_BACKDROP.lock();
        if let Some((well, handle)) = cached.as_ref()
            && *well == well_rgb
        {
            return handle.clone();
        }

        let still = default_icon();
        let handle = if let image::Handle::Rgba {
            width,
            height,
            ref pixels,
            ..
        } = still
        {
            crate::media::backdrop::bake_blurred_backdrop(width, height, pixels, well_rgb)
                .unwrap_or_else(|| still.clone())
        } else {
            still
        };
        *cached = Some((well_rgb, handle.clone()));
        handle
    }

    pub fn steam_anonymous() -> image::Handle {
        STEAM_ANONYMOUS
            .get_or_init(|| decode_raster("steam anonymous avatar", STEAM_ANONYMOUS_BYTES))
            .clone()
    }

    fn decode_raster(name: &str, bytes: &[u8]) -> image::Handle {
        let mut decoder = ThumbnailDecoder::new();
        let thumbnail = decoder
            .decode_and_resize_bytes(bytes, MAX_STATIC_EDGE)
            .unwrap_or_else(|error| panic!("bundled raster asset `{name}` must decode: {error}"));
        let metadata = thumbnail.metadata();
        image::Handle::from_rgba(
            metadata.width,
            metadata.height,
            thumbnail.rgba_bytes().to_vec(),
        )
    }
}

#[cfg(test)]
mod tests {
    use iced::widget::image;

    use super::{fonts, icons, images};

    #[test]
    fn bundled_font_registry_contains_primary_and_cjk_fonts() {
        let fonts = fonts::bundled_fonts();

        assert_eq!(fonts.len(), 5);
        assert!(fonts.iter().all(|bytes| bytes.len() > 1_024));
        assert_eq!(fonts[0], include_bytes!("../ui/fonts/Inter-Regular.ttf"));
        assert_eq!(fonts[1], include_bytes!("../ui/fonts/Inter-SemiBold.ttf"));
        assert_eq!(fonts[2], include_bytes!("../ui/fonts/Inter-Bold.ttf"));
        assert_eq!(
            fonts[3],
            include_bytes!("../ui/fonts/GMPCJKSCUI-Regular.otf")
        );
        assert_eq!(
            fonts[4],
            include_bytes!("../ui/fonts/GMPCJKKRUI-Regular.otf")
        );
        assert_eq!(fonts::default_font(), iced::Font::with_name("Inter"));
    }

    #[test]
    fn svg_registry_uses_bundled_memory_handles() {
        for (first, second) in [
            (icons::akar_download(), icons::akar_download()),
            (icons::akar_folder(), icons::akar_folder()),
            (icons::akar_folder_add(), icons::akar_folder_add()),
            (icons::circle_plus(), icons::circle_plus()),
            (icons::cloud_download(), icons::cloud_download()),
            (icons::cloud_upload(), icons::cloud_upload()),
            (
                icons::context_cloud_download(),
                icons::context_cloud_download(),
            ),
            (icons::context_copy(), icons::context_copy()),
            (icons::context_folder(), icons::context_folder()),
            (icons::context_folder_add(), icons::context_folder_add()),
            (icons::context_image(), icons::context_image()),
            (icons::context_link_chain(), icons::context_link_chain()),
            (icons::context_link_out(), icons::context_link_out()),
            (icons::cross(), icons::cross()),
            (icons::dead(), icons::dead()),
            (icons::download_count(), icons::download_count()),
            (icons::folder(), icons::folder()),
            (icons::folder_add(), icons::folder_add()),
            (icons::gear(), icons::gear()),
            (icons::gmod_logo(), icons::gmod_logo()),
            (icons::link_chain(), icons::link_chain()),
            #[cfg(feature = "asset-studio")]
            (icons::mode_fly(), icons::mode_fly()),
            #[cfg(feature = "asset-studio")]
            (icons::mode_walk(), icons::mode_walk()),
            (icons::route_downloader(), icons::route_downloader()),
            (
                icons::route_installed_addons(),
                icons::route_installed_addons(),
            ),
            (icons::route_my_workshop(), icons::route_my_workshop()),
            (icons::route_size_analyzer(), icons::route_size_analyzer()),
            (icons::search(), icons::search()),
            (icons::star_filled(), icons::star_filled()),
        ] {
            assert_eq!(first.id(), second.id());
        }
    }

    #[test]
    fn silkicon_registry_decodes_all_bundled_icons() {
        use crate::widgets::file_types::SilkIcon;
        for icon in [
            SilkIcon::Bricks,
            SilkIcon::Comments,
            SilkIcon::Folder,
            SilkIcon::Font,
            SilkIcon::Map,
            SilkIcon::PageWhite,
            SilkIcon::PageWhiteText,
            SilkIcon::PageWhiteWrench,
            SilkIcon::Photo,
            SilkIcon::PictureLink,
            SilkIcon::ScriptCode,
            SilkIcon::Sound,
            SilkIcon::Wand,
        ] {
            assert_rgba_handle(super::silkicons::silkicon(icon), 16, 16);
        }
    }

    #[test]
    fn raster_registry_decodes_to_rgba_handles() {
        assert_rgba_handle(images::default_icon(), 512, 512);
        assert_rgba_handle(images::default_icon_backdrop([0x10, 0x10, 0x10]), 64, 64);
        assert_rgba_handle(images::steam_anonymous(), 184, 184);
    }

    fn assert_rgba_handle(handle: image::Handle, expected_width: u32, expected_height: u32) {
        let image::Handle::Rgba {
            width,
            height,
            pixels,
            ..
        } = handle
        else {
            panic!("static raster assets must use decoded RGBA handles");
        };

        assert_eq!(width, expected_width);
        assert_eq!(height, expected_height);
        assert_eq!(pixels.len(), (width * height * 4) as usize);
    }
}
