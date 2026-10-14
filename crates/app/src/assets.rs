use std::sync::OnceLock;

use iced::{
    Font,
    widget::{image, svg},
};

use crate::media::thumbnail_worker::ThumbnailDecoder;

pub mod fonts {
    use std::{io::Read, sync::OnceLock};

    use super::Font;

    // Static Inter weights let fontdb select Regular, Semibold, and Bold.
    // Explicit italic faces are required because cosmic-text does not synthesize slant.
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
        let FontSegment { start, len } = FONT_SEGMENTS[index];
        &unpacked_fonts()[start..start + len]
    }

    pub fn inter_regular_bytes() -> &'static [u8] {
        font_bytes(INTER_REGULAR)
    }

    pub const fn default_font() -> Font {
        Font::with_name("Inter")
    }

    pub fn bundled_fonts() -> [&'static [u8]; FONT_COUNT] {
        [
            font_bytes(INTER_REGULAR),
            font_bytes(INTER_SEMI_BOLD),
            font_bytes(INTER_BOLD),
            font_bytes(INTER_ITALIC),
            font_bytes(INTER_BOLD_ITALIC),
            font_bytes(CJK_SC_REGULAR),
            font_bytes(CJK_KR_REGULAR),
        ]
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn the_generated_indices_name_distinct_faces() {
            let indices = [
                INTER_REGULAR,
                INTER_SEMI_BOLD,
                INTER_BOLD,
                INTER_ITALIC,
                INTER_BOLD_ITALIC,
                CJK_SC_REGULAR,
                CJK_KR_REGULAR,
            ];

            assert_eq!(indices.len(), FONT_COUNT);
            for (position, index) in indices.into_iter().enumerate() {
                assert_eq!(index, position, "the constants are out of order");
            }

            for (left, right) in (0..FONT_COUNT)
                .flat_map(|left| ((left + 1)..FONT_COUNT).map(move |right| (left, right)))
            {
                assert_ne!(
                    font_bytes(left),
                    font_bytes(right),
                    "faces {left} and {right} are the same file"
                );
            }
        }
    }
}

pub mod icons {
    use super::svg;

    macro_rules! svg_icon {
        ($name:ident, $path:literal) => {
            pub fn $name() -> svg::Handle {
                svg::Handle::from_memory(include_bytes!($path))
            }
        };
    }

    svg_icon!(globe, "../ui/icons/globe.svg");
    svg_icon!(akar_arrow_left, "../ui/icons/akar-arrow-left.svg");
    svg_icon!(akar_download, "../ui/icons/akar-download.svg");
    svg_icon!(akar_enlarge, "../ui/icons/akar-enlarge.svg");
    svg_icon!(akar_folder, "../ui/icons/akar-folder.svg");
    svg_icon!(akar_folder_add, "../ui/icons/akar-folder-add.svg");
    svg_icon!(akar_reduce, "../ui/icons/akar-reduce.svg");
    svg_icon!(check, "../ui/icons/check.svg");
    svg_icon!(chevron_up, "../ui/icons/chevron-up.svg");
    svg_icon!(chevron_down, "../ui/icons/chevron-down.svg");
    svg_icon!(circle_alert, "../ui/icons/circle-alert.svg");
    svg_icon!(circle_plus, "../ui/icons/circle-plus.svg");
    svg_icon!(cloud_download, "../ui/icons/cloud-download.svg");
    // Lucide (ISC), restroked to 1.5 to sit with the rest of the set.
    svg_icon!(cloud_off, "../ui/icons/cloud-off.svg");
    svg_icon!(cloud_upload, "../ui/icons/cloud-upload.svg");
    svg_icon!(
        context_cloud_download,
        "../ui/icons/context-menu/cloud-download.svg"
    );
    svg_icon!(context_copy, "../ui/icons/context-menu/copy.svg");
    svg_icon!(context_edit, "../ui/icons/context-menu/edit.svg");
    svg_icon!(context_folder, "../ui/icons/context-menu/folder.svg");
    svg_icon!(
        context_folder_add,
        "../ui/icons/context-menu/folder-add.svg"
    );
    svg_icon!(context_image, "../ui/icons/context-menu/image.svg");
    svg_icon!(
        context_link_chain,
        "../ui/icons/context-menu/link-chain.svg"
    );
    svg_icon!(context_link_out, "../ui/icons/context-menu/link-out.svg");
    svg_icon!(cross, "../ui/icons/cross.svg");
    svg_icon!(dead, "../ui/icons/dead.svg");
    svg_icon!(download_count, "../ui/icons/download-count.svg");
    // Lucide (ISC), restroked to 1.5 to sit with the rest of the set.
    svg_icon!(eye_off, "../ui/icons/eye-off.svg");
    svg_icon!(folder, "../ui/icons/folder.svg");
    svg_icon!(play, "../ui/icons/play.svg");
    svg_icon!(folder_add, "../ui/icons/folder-add.svg");
    // Lucide (ISC), restroked to 1.5 to sit with the rest of the set.
    svg_icon!(folder_x, "../ui/icons/folder-x.svg");
    svg_icon!(gear, "../ui/icons/gear.svg");

    /// Multicolor artwork: render WITHOUT a tint override so its natural
    /// blue/white fills show.
    pub fn gmod_logo() -> svg::Handle {
        svg::Handle::from_memory(include_bytes!("../ui/icons/gmod-logo.svg"))
    }

    // Lucide (ISC), restroked to 1.5 to sit with the rest of the set.
    svg_icon!(image, "../ui/icons/image.svg");
    svg_icon!(link_chain, "../ui/icons/link-chain.svg");
    // Lucide (ISC), restroked to 1.5 to sit with the rest of the set.
    svg_icon!(package_open, "../ui/icons/package-open.svg");

    svg_icon!(mode_fly, "../ui/icons/mode-fly.svg");
    svg_icon!(mode_walk, "../ui/icons/mode-walk.svg");

    svg_icon!(route_downloader, "../ui/icons/route-downloader.svg");
    svg_icon!(
        route_installed_addons,
        "../ui/icons/route-installed-addons.svg"
    );
    svg_icon!(route_models, "../ui/icons/route-models.svg");
    svg_icon!(route_my_workshop, "../ui/icons/route-my-workshop.svg");
    svg_icon!(route_size_analyzer, "../ui/icons/route-size-analyzer.svg");
    svg_icon!(search, "../ui/icons/search.svg");
    svg_icon!(star_filled, "../ui/icons/star-filled.svg");
    // Lucide (ISC), restroked to 1.5 to sit with the rest of the set.
    svg_icon!(table, "../ui/icons/table.svg");
    svg_icon!(tag_point, "../ui/icons/tag-point.svg");

    pub fn tooltip_arrow() -> svg::Handle {
        svg::Handle::from_memory(include_bytes!("../ui/icons/tooltip-arrow.svg"))
    }
}

pub mod silkicons {
    use super::{OnceLock, ThumbnailDecoder, image};
    use crate::widgets::file_types::SilkIcon;

    const SILKICON_MAX_EDGE: u32 = 16;
    const SILKICON_COUNT: usize = SilkIcon::ALL.len();

    static HANDLES: OnceLock<[image::Handle; SILKICON_COUNT]> = OnceLock::new();

    pub fn silkicon(icon: SilkIcon) -> image::Handle {
        HANDLES.get_or_init(decode_all)[icon.index()].clone()
    }

    fn decode_all() -> [image::Handle; SILKICON_COUNT] {
        std::array::from_fn(|index| {
            let (name, bytes) = SilkIcon::ALL[index].bytes();
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

    /// Cache by icon-well color so a theme change flattens transparency onto the new well.
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

    use super::{fonts, images};

    #[test]
    fn bundled_font_registry_contains_primary_and_cjk_fonts() {
        let fonts = fonts::bundled_fonts();

        assert_eq!(fonts.len(), 7);
        assert!(fonts.iter().all(|bytes| bytes.len() > 1_024));
        assert_eq!(fonts[0], include_bytes!("../ui/fonts/Inter-Regular.ttf"));
        assert_eq!(fonts[1], include_bytes!("../ui/fonts/Inter-SemiBold.ttf"));
        assert_eq!(fonts[2], include_bytes!("../ui/fonts/Inter-Bold.ttf"));
        assert_eq!(fonts[3], include_bytes!("../ui/fonts/Inter-Italic.ttf"));
        assert_eq!(fonts[4], include_bytes!("../ui/fonts/Inter-BoldItalic.ttf"));
        assert_eq!(
            fonts[5],
            include_bytes!("../ui/fonts/GMPCJKSCUI-Regular.otf")
        );
        assert_eq!(
            fonts[6],
            include_bytes!("../ui/fonts/GMPCJKKRUI-Regular.otf")
        );
        assert_eq!(fonts::default_font(), iced::Font::with_name("Inter"));
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
