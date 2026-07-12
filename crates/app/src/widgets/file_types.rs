//! File-type mapping: extension → silkicon + type label.

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
pub struct FileTypeInfo {
    pub(crate) icon: SilkIcon,
    /// Fluent key suffix under `file-type-*`.
    pub(crate) type_key: &'static str,
    pub(crate) extension: String,
}

pub fn file_type_info(name: &str) -> FileTypeInfo {
    let extension = name
        .rsplit_once('.')
        .map(|(_, extension)| extension.to_ascii_lowercase())
        .unwrap_or_default();
    FileTypeInfo {
        icon: file_icon(&extension),
        type_key: file_type_key(&extension),
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
        "mp3" | "ogg" | "wav" => "audio",
        "png" | "jpg" | "jpeg" => "image",
        "bsp" => "map",
        "vtf" => "vtf",
        "vmt" => "vmt",
        "map" => "map",
        "ain" => "ain",
        "nav" => "nav",
        "ttf" => "ttf",
        "vcd" => "vcd",
        "fgd" => "fgd",
        "pcf" => "pcf",
        "lua" => "lua",
        "mdl" => "mdl",
        "vtx" => "vtx",
        "phy" => "phy",
        "ani" => "ani",
        "vvd" => "vvd",
        "txt" => "txt",
        "properties" => "properties",
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::{SilkIcon, file_type_info};

    #[test]
    fn extensions_map_to_upstream_icons_and_types() {
        let lua = file_type_info("lua/autorun/init.lua");
        assert_eq!(lua.icon, SilkIcon::ScriptCode);
        assert_eq!(lua.type_key, "lua");
        assert_eq!(lua.extension, "lua");

        let audio = file_type_info("sound/music.OGG");
        assert_eq!(audio.icon, SilkIcon::Sound);
        assert_eq!(audio.type_key, "audio");
        assert_eq!(audio.extension, "ogg");

        let map = file_type_info("maps/gm_flatgrass.bsp");
        assert_eq!(map.icon, SilkIcon::Map);
        assert_eq!(map.type_key, "map");

        let material = file_type_info("materials/icon.vmt");
        assert_eq!(material.icon, SilkIcon::PictureLink);
        assert_eq!(material.type_key, "vmt");

        let unknown = file_type_info("data/blob.dat");
        assert_eq!(unknown.icon, SilkIcon::PageWhite);
        assert_eq!(unknown.type_key, "unknown");
        assert_eq!(unknown.extension, "dat");

        let bare = file_type_info("noextension");
        assert_eq!(bare.icon, SilkIcon::PageWhite);
        assert_eq!(bare.type_key, "unknown");
        assert_eq!(bare.extension, "");
    }
}
