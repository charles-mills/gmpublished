use crate::bridge::archive::PreviewArchiveSource;
use crate::features::file_preview::{PreviewRequest, RelatedPreviewKind, RelatedPreviewTarget};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum EntryClass {
    Code { syntax: CodeSyntax },
    Image(ImageClass),
    Font,
    Audio,
    Model,
    ModelCompanion,
    Map,
    Particle,
    Info,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CodeSyntax {
    Plain,
    Glua,
    Json,
    Vmt,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ImageClass {
    Encoded,
    Vtf,
}

pub(super) fn classify_entry_path(path: &str) -> EntryClass {
    if model_companion_parent_path(path).is_some() {
        return EntryClass::ModelCompanion;
    }

    match lower_extension(path).as_deref() {
        Some("lua") => EntryClass::Code {
            syntax: CodeSyntax::Glua,
        },
        Some("json") => EntryClass::Code {
            syntax: CodeSyntax::Json,
        },
        Some("vmt") => EntryClass::Code {
            syntax: CodeSyntax::Vmt,
        },
        Some("txt" | "cfg" | "vdf" | "res" | "ini" | "properties") => EntryClass::Code {
            syntax: CodeSyntax::Plain,
        },
        Some("png" | "jpg" | "jpeg") => EntryClass::Image(ImageClass::Encoded),
        Some("vtf") => EntryClass::Image(ImageClass::Vtf),
        Some("ttf") => EntryClass::Font,
        Some("wav" | "mp3" | "ogg") => EntryClass::Audio,
        Some("mdl") => EntryClass::Model,
        Some("bsp") => EntryClass::Map,
        Some("pcf") => EntryClass::Particle,
        Some(_) | None => EntryClass::Info,
    }
}

pub(super) fn model_companion_preview_request(request: &PreviewRequest) -> Option<PreviewRequest> {
    let parent_path = model_companion_parent_path(&request.entry_path)?;
    let parent = archive_entry_for_path(&request.archive, &parent_path)?;
    let mut request = request.clone();
    request.entry_path.clone_from(&parent.path);
    request.display_name = parent.path.rsplit_once('/').map_or_else(
        || parent.path.clone(),
        |(_, file_name)| file_name.to_owned(),
    );
    request.size_bytes = parent.size;
    request.crc32 = parent.crc32;
    Some(request)
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct RedirectedArchiveEntry {
    path: String,
    size: u64,
    crc32: u32,
}

fn archive_entry_for_path(
    archive: &PreviewArchiveSource,
    path: &str,
) -> Option<RedirectedArchiveEntry> {
    archive
        .entry(path)
        .ok()
        .map(|entry| RedirectedArchiveEntry {
            path: entry.path.to_owned(),
            size: entry.size,
            crc32: entry.crc32,
        })
        .or_else(|| {
            archive
                .entry_ignore_ascii_case(path)
                .ok()
                .map(|entry| RedirectedArchiveEntry {
                    path: entry.path.to_owned(),
                    size: entry.size,
                    crc32: entry.crc32,
                })
        })
}

pub(super) fn related_preview_target(
    request: &PreviewRequest,
    bytes: &[u8],
) -> Option<RelatedPreviewTarget> {
    match lower_extension(&request.entry_path).as_deref() {
        Some("vtf") => same_stem_material_target(request),
        Some("vmt") => base_texture_target(request, bytes),
        _ => None,
    }
}

fn same_stem_material_target(request: &PreviewRequest) -> Option<RelatedPreviewTarget> {
    let (stem, _) = request.entry_path.rsplit_once('.')?;
    let material_path = format!("{stem}.vmt");
    let entry = archive_entry_for_path(&request.archive, &material_path)?;
    Some(RelatedPreviewTarget {
        entry_path: entry.path,
        kind: RelatedPreviewKind::Material,
    })
}

fn base_texture_target(request: &PreviewRequest, bytes: &[u8]) -> Option<RelatedPreviewTarget> {
    let vmt_text = String::from_utf8_lossy(bytes);
    let texture_name = vformats::vmt::basetexture(&vmt_text, &vformats::Limits::default())?;
    let texture_path = material_texture_entry_path(&texture_name);
    let entry = archive_entry_for_path(&request.archive, &texture_path)?;
    Some(RelatedPreviewTarget {
        entry_path: entry.path,
        kind: RelatedPreviewKind::Texture,
    })
}

fn material_texture_entry_path(texture_name: &str) -> String {
    if texture_name
        .get(.."materials/".len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("materials/"))
    {
        format!("{texture_name}.vtf")
    } else {
        format!("materials/{texture_name}.vtf")
    }
}

/// The `.vtx` flavours Source ships, in the order a loader should try them.
///
/// Order is load-bearing twice over. [`model_companion_parent_path`] matches by
/// suffix, so the bare `.vtx` must come last or `foo.dx90.vtx` would yield
/// `foo.dx90.mdl`; and `load_model_companions` walks the same list as its
/// fallback chain, so the most specific (and most commonly shipped) flavour
/// must come first.
///
/// Both sides read this one list: classifying a flavour as a companion without
/// the loader also trying it means the file redirects to its parent `.mdl` and
/// then fails to load.
pub(super) const VTX_SUFFIXES: &[&str] = &[
    ".dx90.vtx",
    ".dx80.vtx",
    ".sw.vtx",
    ".360.vtx",
    ".xbox.vtx",
    ".vtx",
];

/// Non-`.vtx` companions that redirect to the parent `.mdl`. Unlike
/// [`VTX_SUFFIXES`] these are not alternatives to one another — `.vvd` is
/// loaded unconditionally and `.phy`/`.ani` are not loaded at all.
const OTHER_COMPANION_SUFFIXES: &[&str] = &[".vvd", ".phy", ".ani"];

pub(super) fn model_companion_parent_path(path: &str) -> Option<String> {
    let lower = path.to_ascii_lowercase();
    for suffix in VTX_SUFFIXES.iter().chain(OTHER_COMPANION_SUFFIXES) {
        if lower.ends_with(suffix) {
            let stem_len = path.len().saturating_sub(suffix.len());
            return Some(format!("{}.mdl", &path[..stem_len]));
        }
    }
    None
}

fn lower_extension(path: &str) -> Option<String> {
    path.rsplit_once('.')
        .map(|(_, extension)| extension.to_ascii_lowercase())
        .filter(|extension| !extension.is_empty())
}
