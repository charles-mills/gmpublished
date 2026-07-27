//! File-type mapping: extension → silkicon + type label.

use std::borrow::Cow;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SilkIcon {
    Bricks,
    Comments,
    Folder,
    Font,
    Map,
    PageWhite,
    PageWhiteText,
    PageWhiteWrench,
    Photo,
    PictureLink,
    ScriptCode,
    Sound,
    Wand,
}

/// Matches steam.js `getFileTypeInfo`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileTypeInfo<'a> {
    pub(crate) icon: SilkIcon,
    pub(crate) translation_key: &'static str,
    pub(crate) extension: Cow<'a, str>,
}

pub fn file_type_info(name: &str) -> FileTypeInfo<'_> {
    let extension = name.rsplit_once('.').map_or("", |(_, extension)| extension);
    let extension = if extension.bytes().any(|byte| byte.is_ascii_uppercase()) {
        Cow::Owned(extension.to_ascii_lowercase())
    } else {
        Cow::Borrowed(extension)
    };
    FileTypeInfo {
        icon: file_icon(&extension),
        translation_key: file_type_key(&extension),
        extension,
    }
}

fn file_icon(extension: &str) -> SilkIcon {
    match extension {
        "lua" => SilkIcon::ScriptCode,
        "mp3" | "ogg" | "wav" => SilkIcon::Sound,
        "png" | "jpg" | "jpeg" => SilkIcon::Photo,
        "bsp" | "nav" | "ain" | "fgd" => SilkIcon::Map,
        "pcf" => SilkIcon::Wand,
        "vcd" => SilkIcon::Comments,
        "ttf" => SilkIcon::Font,
        "txt" => SilkIcon::PageWhiteText,
        "properties" => SilkIcon::PageWhiteWrench,
        "vmt" | "vtf" => SilkIcon::PictureLink,
        "mdl" | "vtx" | "phy" | "ani" | "vvd" => SilkIcon::Bricks,
        _ => SilkIcon::PageWhite,
    }
}

fn file_type_key(extension: &str) -> &'static str {
    match extension {
        "mp3" | "ogg" | "wav" => "file-type-audio",
        "png" | "jpg" | "jpeg" => "file-type-image",
        "bsp" | "map" => "file-type-map",
        "vtf" => "file-type-vtf",
        "vmt" => "file-type-vmt",
        "ain" => "file-type-ain",
        "nav" => "file-type-nav",
        "ttf" => "file-type-ttf",
        "vcd" => "file-type-vcd",
        "fgd" => "file-type-fgd",
        "pcf" => "file-type-pcf",
        "lua" => "file-type-lua",
        "mdl" => "file-type-mdl",
        "vtx" => "file-type-vtx",
        "phy" => "file-type-phy",
        "ani" => "file-type-ani",
        "vvd" => "file-type-vvd",
        "txt" => "file-type-txt",
        "properties" => "file-type-properties",
        _ => "file-type-unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::{SilkIcon, file_type_info};

    #[test]
    fn extensions_map_to_upstream_icons_and_types() {
        let lua = file_type_info("lua/autorun/init.lua");
        assert_eq!(lua.icon, SilkIcon::ScriptCode);
        assert_eq!(lua.translation_key, "file-type-lua");
        assert_eq!(lua.extension, "lua");

        let audio = file_type_info("sound/music.OGG");
        assert_eq!(audio.icon, SilkIcon::Sound);
        assert_eq!(audio.translation_key, "file-type-audio");
        assert_eq!(audio.extension, "ogg");

        let map = file_type_info("maps/gm_flatgrass.bsp");
        assert_eq!(map.icon, SilkIcon::Map);
        assert_eq!(map.translation_key, "file-type-map");

        let material = file_type_info("materials/icon.vmt");
        assert_eq!(material.icon, SilkIcon::PictureLink);
        assert_eq!(material.translation_key, "file-type-vmt");

        let unknown = file_type_info("data/blob.dat");
        assert_eq!(unknown.icon, SilkIcon::PageWhite);
        assert_eq!(unknown.translation_key, "file-type-unknown");
        assert_eq!(unknown.extension, "dat");

        let bare = file_type_info("noextension");
        assert_eq!(bare.icon, SilkIcon::PageWhite);
        assert_eq!(bare.translation_key, "file-type-unknown");
        assert_eq!(bare.extension, "");
    }
}
