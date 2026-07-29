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

impl SilkIcon {
    /// Every icon, in declaration order.
    ///
    /// A new variant is caught by [`Self::bytes`]'s exhaustive match, not by
    /// this array — its length is a literal, so a missing entry here compiles.
    /// Getting the *order* wrong compiles too, which is what
    /// `all_matches_declaration_order` exists for.
    pub const ALL: [Self; 13] = [
        Self::Bricks,
        Self::Comments,
        Self::Folder,
        Self::Font,
        Self::Map,
        Self::PageWhite,
        Self::PageWhiteText,
        Self::PageWhiteWrench,
        Self::Photo,
        Self::PictureLink,
        Self::ScriptCode,
        Self::Sound,
        Self::Wand,
    ];

    /// The bundled PNG for this icon, and the file name to blame if it fails
    /// to decode.
    ///
    /// An exhaustive match rather than a position in a parallel array: the
    /// icon and its bytes are one fact, and a positional pairing is two
    /// orderings kept in step by hand.
    #[must_use]
    pub const fn bytes(self) -> (&'static str, &'static [u8]) {
        match self {
            Self::Bricks => (
                "bricks",
                include_bytes!("../../ui/images/silkicons/bricks.png"),
            ),
            Self::Comments => (
                "comments",
                include_bytes!("../../ui/images/silkicons/comments.png"),
            ),
            Self::Folder => (
                "folder",
                include_bytes!("../../ui/images/silkicons/folder.png"),
            ),
            Self::Font => ("font", include_bytes!("../../ui/images/silkicons/font.png")),
            Self::Map => ("map", include_bytes!("../../ui/images/silkicons/map.png")),
            Self::PageWhite => (
                "page_white",
                include_bytes!("../../ui/images/silkicons/page_white.png"),
            ),
            Self::PageWhiteText => (
                "page_white_text",
                include_bytes!("../../ui/images/silkicons/page_white_text.png"),
            ),
            Self::PageWhiteWrench => (
                "page_white_wrench",
                include_bytes!("../../ui/images/silkicons/page_white_wrench.png"),
            ),
            Self::Photo => (
                "photo",
                include_bytes!("../../ui/images/silkicons/photo.png"),
            ),
            Self::PictureLink => (
                "picture_link",
                include_bytes!("../../ui/images/silkicons/picture_link.png"),
            ),
            Self::ScriptCode => (
                "script_code",
                include_bytes!("../../ui/images/silkicons/script_code.png"),
            ),
            Self::Sound => (
                "sound",
                include_bytes!("../../ui/images/silkicons/sound.png"),
            ),
            Self::Wand => ("wand", include_bytes!("../../ui/images/silkicons/wand.png")),
        }
    }

    /// This icon's position in [`Self::ALL`], which is declaration order.
    #[must_use]
    pub const fn index(self) -> usize {
        self as usize
    }
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
    let (icon, translation_key) = icon_and_type_key(&extension);
    FileTypeInfo {
        icon,
        translation_key,
        extension,
    }
}

/// One table so an extension cannot gain a label without an icon, or the
/// reverse. Extensions sharing a label share this arm.
fn icon_and_type_key(extension: &str) -> (SilkIcon, &'static str) {
    match extension {
        "lua" => (SilkIcon::ScriptCode, "file-type-lua"),
        "mp3" | "ogg" | "wav" => (SilkIcon::Sound, "file-type-audio"),
        "png" | "jpg" | "jpeg" => (SilkIcon::Photo, "file-type-image"),
        "bsp" | "map" => (SilkIcon::Map, "file-type-map"),
        "nav" => (SilkIcon::Map, "file-type-nav"),
        "ain" => (SilkIcon::Map, "file-type-ain"),
        "fgd" => (SilkIcon::Map, "file-type-fgd"),
        "pcf" => (SilkIcon::Wand, "file-type-pcf"),
        "vcd" => (SilkIcon::Comments, "file-type-vcd"),
        "ttf" => (SilkIcon::Font, "file-type-ttf"),
        "txt" => (SilkIcon::PageWhiteText, "file-type-txt"),
        "properties" => (SilkIcon::PageWhiteWrench, "file-type-properties"),
        "vmt" => (SilkIcon::PictureLink, "file-type-vmt"),
        "vtf" => (SilkIcon::PictureLink, "file-type-vtf"),
        "mdl" => (SilkIcon::Bricks, "file-type-mdl"),
        "vtx" => (SilkIcon::Bricks, "file-type-vtx"),
        "phy" => (SilkIcon::Bricks, "file-type-phy"),
        "ani" => (SilkIcon::Bricks, "file-type-ani"),
        "vvd" => (SilkIcon::Bricks, "file-type-vvd"),
        _ => (SilkIcon::PageWhite, "file-type-unknown"),
    }
}

#[cfg(test)]
mod tests {
    use super::{SilkIcon, file_type_info};

    /// `.map` and `.bsp` share a label, so they must share an icon. Two
    /// separate tables let `.map` keep the label while falling through to the
    /// generic page icon.
    #[test]
    fn extensions_sharing_a_label_share_an_icon() {
        let bsp = file_type_info("de_dust2.bsp");
        let map = file_type_info("de_dust2.map");

        assert_eq!(bsp.translation_key, map.translation_key);
        assert_eq!(bsp.icon, map.icon);
        assert_eq!(map.icon, SilkIcon::Map);
    }

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

    /// `ALL` is indexed by `index()`, which is the discriminant. If the two
    /// disagree, every icon past the divergence renders as another icon —
    /// which nothing else in the app would notice.
    #[test]
    fn all_matches_declaration_order() {
        for (index, icon) in SilkIcon::ALL.into_iter().enumerate() {
            assert_eq!(icon.index(), index, "{icon:?} is out of order in ALL");
        }
    }

    /// Two icons sharing a PNG is always a copy-paste slip, and the slip
    /// pairs a *correct* name with the wrong bytes — so comparing names alone
    /// would miss it.
    #[test]
    fn every_icon_has_its_own_bundled_image() {
        for (position, icon) in SilkIcon::ALL.into_iter().enumerate() {
            for other in SilkIcon::ALL.into_iter().skip(position + 1) {
                assert_ne!(
                    icon.bytes().1,
                    other.bytes().1,
                    "{icon:?} and {other:?} include the same PNG"
                );
                assert_ne!(icon.bytes().0, other.bytes().0, "{icon:?} name reused");
            }
        }
    }
}
