use std::borrow::Cow;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EntryClass {
    /// A `.mdl` sidecar, which has no preview of its own: the caller either
    /// redirects to the parent model or reports the parent missing.
    ModelCompanion,
    Previewable(PreviewClass),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PreviewClass {
    Code { syntax: CodeSyntax },
    Image(ImageClass),
    Font,
    Audio,
    Model,
    Map,
    Particle,
    Info,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CodeSyntax {
    Plain,
    Glua,
    Json,
    Vmt,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ImageClass {
    Encoded,
    Vtf,
}

pub fn classify_entry_path(path: &str) -> EntryClass {
    if model_companion_stem(path).is_some() {
        return EntryClass::ModelCompanion;
    }

    EntryClass::Previewable(match lower_extension(path).as_ref() {
        "lua" => PreviewClass::Code {
            syntax: CodeSyntax::Glua,
        },
        "json" => PreviewClass::Code {
            syntax: CodeSyntax::Json,
        },
        "vmt" => PreviewClass::Code {
            syntax: CodeSyntax::Vmt,
        },
        "txt" | "cfg" | "vdf" | "res" | "ini" | "properties" => PreviewClass::Code {
            syntax: CodeSyntax::Plain,
        },
        "png" | "jpg" | "jpeg" => PreviewClass::Image(ImageClass::Encoded),
        "vtf" => PreviewClass::Image(ImageClass::Vtf),
        "ttf" => PreviewClass::Font,
        "wav" | "mp3" | "ogg" => PreviewClass::Audio,
        "mdl" => PreviewClass::Model,
        "bsp" => PreviewClass::Map,
        "pcf" => PreviewClass::Particle,
        _ => PreviewClass::Info,
    })
}

/// Try plain `.vtx` last, or `foo.dx90.vtx` would resolve to `foo.dx90.mdl`.
pub const VTX_SUFFIXES: &[&str] = &[
    ".dx90.vtx",
    ".dx80.vtx",
    ".sw.vtx",
    ".360.vtx",
    ".xbox.vtx",
    ".vtx",
];

const OTHER_COMPANION_SUFFIXES: &[&str] = &[".vvd", ".phy", ".ani"];

pub fn model_companion_parent_path(path: &str) -> Option<String> {
    model_companion_stem(path).map(|stem| format!("{stem}.mdl"))
}

fn model_companion_stem(path: &str) -> Option<&str> {
    VTX_SUFFIXES
        .iter()
        .chain(OTHER_COMPANION_SUFFIXES)
        .find_map(|suffix| {
            let stem_len = path.len().checked_sub(suffix.len())?;
            path.get(stem_len..)?
                .eq_ignore_ascii_case(suffix)
                .then(|| &path[..stem_len])
        })
}

pub fn lower_extension(path: &str) -> Cow<'_, str> {
    let extension = path.rsplit_once('.').map_or("", |(_, extension)| extension);
    if extension.bytes().any(|byte| byte.is_ascii_uppercase()) {
        Cow::Owned(extension.to_ascii_lowercase())
    } else {
        Cow::Borrowed(extension)
    }
}
