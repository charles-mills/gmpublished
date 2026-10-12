use crate::bridge::archive::{ArchivePreviewEntry, PreviewArchiveSource};
use crate::media::preview_model::{PreviewRequest, RelatedPreviewKind, RelatedPreviewTarget};

pub(super) use crate::media::file_kind::{
    CodeSyntax, EntryClass, ImageClass, PreviewClass, VTX_SUFFIXES, classify_entry_path,
    model_companion_parent_path,
};

pub(super) fn model_companion_preview_request(request: &PreviewRequest) -> Option<PreviewRequest> {
    let parent_path = model_companion_parent_path(&request.entry_path)?;
    let parent = archive_entry_for_path(&request.archive, &parent_path)?;
    let mut request = request.clone();
    parent.path.clone_into(&mut request.entry_path);
    request.display_name = parent.path.rsplit_once('/').map_or_else(
        || parent.path.to_owned(),
        |(_, file_name)| file_name.to_owned(),
    );
    request.size_bytes = parent.size;
    request.crc32 = parent.crc32;
    Some(request)
}

fn archive_entry_for_path<'a>(
    archive: &'a PreviewArchiveSource,
    path: &str,
) -> Option<ArchivePreviewEntry<'a>> {
    archive
        .entry(path)
        .or_else(|_| archive.entry_ignore_ascii_case(path))
        .ok()
}

pub(super) fn related_preview_target(
    request: &PreviewRequest,
    bytes: &[u8],
) -> Option<RelatedPreviewTarget> {
    match classify_entry_path(&request.entry_path) {
        EntryClass::Previewable(PreviewClass::Image(ImageClass::Vtf)) => {
            same_stem_material_target(request)
        }
        EntryClass::Previewable(PreviewClass::Code {
            syntax: CodeSyntax::Vmt,
        }) => base_texture_target(request, bytes),
        _ => None,
    }
}

fn same_stem_material_target(request: &PreviewRequest) -> Option<RelatedPreviewTarget> {
    let (stem, _) = request.entry_path.rsplit_once('.')?;
    let material_path = format!("{stem}.vmt");
    let entry = archive_entry_for_path(&request.archive, &material_path)?;
    Some(RelatedPreviewTarget {
        entry_path: entry.path.to_owned(),
        kind: RelatedPreviewKind::Material,
    })
}

fn base_texture_target(request: &PreviewRequest, bytes: &[u8]) -> Option<RelatedPreviewTarget> {
    let vmt_text = String::from_utf8_lossy(bytes);
    let texture_name = vformats::vmt::basetexture(&vmt_text, &vformats::Limits::default())?;
    let texture_path = material_texture_entry_path(&texture_name);
    let entry = archive_entry_for_path(&request.archive, &texture_path)?;
    Some(RelatedPreviewTarget {
        entry_path: entry.path.to_owned(),
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
