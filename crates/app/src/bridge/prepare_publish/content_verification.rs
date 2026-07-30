//! Content-tree collection and publish/snapshot verification.

use super::{
    Arc, ArchiveEntryPath, ArchiveService, ConfigService, ContentPathVerificationRequest,
    GmaMetaEntry, HashSet, MAX_WHITELIST_FAILURES, Path, PathBuf, PathOperation,
    PreparePublishPathError, UiError, VerifiedContentPath, WorkshopSnapshotInventory, file_browser,
    keys, whitelist,
};

pub fn verify_content_path(
    config: ConfigService<'_>,
    archive: ArchiveService<'_>,
    request: ContentPathVerificationRequest,
) -> Result<Arc<VerifiedContentPath>, UiError> {
    let settings = config.settings_snapshot();
    let whitelist = archive.whitelist_snapshot();
    collect_content_tree(
        request.display_path,
        request.path,
        ContentCollectionPolicy::Publish {
            ignore_globs: &settings.backend.ignore_globs,
            whitelist: &whitelist,
        },
    )
    .map(VerifiedContentPath::from)
    .map(Arc::new)
}

pub fn inspect_workshop_snapshot(
    request: ContentPathVerificationRequest,
) -> Result<Arc<WorkshopSnapshotInventory>, UiError> {
    collect_content_tree(
        request.display_path,
        request.path,
        ContentCollectionPolicy::Snapshot,
    )
    .map(WorkshopSnapshotInventory::from)
    .map(Arc::new)
}

#[derive(Clone, Copy)]
enum ContentCollectionPolicy<'a> {
    Publish {
        ignore_globs: &'a [String],
        whitelist: &'a [String],
    },
    Snapshot,
}

fn collect_content_tree(
    display_path: String,
    path: PathBuf,
    policy: ContentCollectionPolicy<'_>,
) -> Result<CollectedContentTree, UiError> {
    if !path.is_absolute() {
        return Err(UiError::new(keys::INVALID_CONTENT_PATH));
    }
    let root_metadata = path.metadata().map_err(|source| {
        PreparePublishPathError::io(PathOperation::InspectMetadata, &path, source).into_ui()
    })?;
    if !root_metadata.is_dir() {
        return Err(UiError::new(keys::INVALID_CONTENT_PATH));
    }

    let mut state = ContentCollectionState::default();
    collect_content_entries(&path, &path, policy, &mut state)?;
    let ContentCollectionState {
        entries,
        total_size,
        mut failed,
        duplicate,
        ..
    } = state;

    if let Some(duplicate) = duplicate {
        return Err(UiError::detailed(keys::DUPLICATE_ENTRIES, Some(duplicate)));
    }
    if !failed.is_empty() {
        failed.sort_unstable();
        if failed.len() > MAX_WHITELIST_FAILURES {
            failed.truncate(MAX_WHITELIST_FAILURES);
            failed.push("...".to_owned());
        }
        return Err(UiError::detailed(keys::WHITELIST, Some(failed.join("\n"))));
    }
    if entries.is_empty() {
        return Err(UiError::new(keys::NO_ENTRIES));
    }

    let browser_entries = entries
        .iter()
        .map(|(entry, _)| file_browser_entry(entry))
        .collect::<Result<Vec<_>, _>>()?;

    let preview_source = crate::bridge::archive::PreviewArchiveSource::from_folder(
        entries
            .into_iter()
            .map(|(entry, disk_path)| (entry.path, entry.size, disk_path)),
    );

    Ok(CollectedContentTree {
        display_path,
        path,
        total_size,
        entries: browser_entries,
        preview_source,
    })
}

struct CollectedContentTree {
    display_path: String,
    path: PathBuf,
    total_size: u64,
    entries: Vec<file_browser::Entry>,
    preview_source: Arc<crate::bridge::archive::PreviewArchiveSource>,
}

impl From<CollectedContentTree> for VerifiedContentPath {
    fn from(tree: CollectedContentTree) -> Self {
        Self {
            display_path: tree.display_path,
            path: tree.path,
            total_size: tree.total_size,
            entries: tree.entries,
            preview_source: tree.preview_source,
        }
    }
}

impl From<CollectedContentTree> for WorkshopSnapshotInventory {
    fn from(tree: CollectedContentTree) -> Self {
        Self {
            entries: tree.entries,
            preview_source: tree.preview_source,
        }
    }
}

/// Accumulator state threaded through the recursive directory walk in
/// [`collect_content_entries`]. Bundled into one struct because these fields
/// are mutated together at every recursion depth, and each is written from
/// multiple call sites rather than just one — passing them individually
/// would mean a long, unwieldy parameter list threaded through every
/// recursive call for no benefit.
#[derive(Default)]
struct ContentCollectionState {
    entries: Vec<(GmaMetaEntry, PathBuf)>,
    total_size: u64,
    failed: Vec<String>,
    duplicate: Option<String>,
    seen: HashSet<String>,
}

fn collect_content_entries(
    root: &Path,
    dir: &Path,
    policy: ContentCollectionPolicy<'_>,
    state: &mut ContentCollectionState,
) -> Result<(), UiError> {
    let read_dir = dir.read_dir().map_err(|source| {
        PreparePublishPathError::io(PathOperation::ReadDirectory, dir, source).into_ui()
    })?;
    for entry in read_dir {
        let entry = entry.map_err(|source| {
            PreparePublishPathError::io(PathOperation::ReadDirectoryEntry, dir, source).into_ui()
        })?;
        let path = entry.path();
        let file_type = entry.file_type().map_err(|source| {
            PreparePublishPathError::io(PathOperation::InspectMetadata, &path, source).into_ui()
        })?;
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            collect_content_entries(root, &path, policy, state)?;
            continue;
        }
        if !file_type.is_file() {
            continue;
        }

        let relative_path = relative_slash_path(root, &path)?;
        if let ContentCollectionPolicy::Publish { ignore_globs, .. } = policy
            && (whitelist::is_default_ignored(&relative_path)
                || whitelist::is_ignored(&relative_path, ignore_globs))
        {
            continue;
        }
        if !state.seen.insert(relative_path.clone()) {
            state.duplicate = Some(relative_path);
            continue;
        }
        if let ContentCollectionPolicy::Publish { whitelist, .. } = policy
            && !whitelist::is_whitelisted_in(whitelist, &relative_path)
        {
            state.failed.push(relative_path);
            continue;
        }
        if state.failed.is_empty() {
            let size = path
                .metadata()
                .map(|metadata| metadata.len())
                .map_err(|source| {
                    PreparePublishPathError::io(PathOperation::InspectMetadata, &path, source)
                        .into_ui()
                })?;
            state.total_size = state.total_size.saturating_add(size);
            state.entries.push((
                GmaMetaEntry {
                    path: relative_path,
                    size,
                    crc32: 0,
                },
                path,
            ));
        }
    }
    Ok(())
}

/// A path under `root` in the archive's canonical form.
///
/// ASCII lowercasing specifically, matching
/// [`crate::bridge::content_path::normalize_archive_path`]: a Unicode
/// lowercase would fold characters it does not, so `CAFÉ.vmt` would index one
/// way here and be looked up another, and nothing would ever find it.
pub(super) fn relative_slash_path(root: &Path, path: &Path) -> Result<String, UiError> {
    let relative = path.strip_prefix(root).map_err(|_| {
        PreparePublishPathError::OutsideContentRoot {
            root: root.to_path_buf(),
            path: path.to_path_buf(),
        }
        .into_ui()
    })?;
    let mut output = String::new();
    for component in relative.components() {
        let std::path::Component::Normal(component) = component else {
            continue;
        };
        let component = component.to_string_lossy().to_ascii_lowercase();
        if !component.is_empty() {
            if !output.is_empty() {
                output.push('/');
            }
            output.push_str(&component);
        }
    }
    Ok(output)
}

fn file_browser_entry(entry: &GmaMetaEntry) -> Result<file_browser::Entry, UiError> {
    let Some(path) = ArchiveEntryPath::from_validated(entry.path.clone()) else {
        log::warn!("Prepare Publish verifier returned an invalid archive path");
        return Err(UiError::new(keys::UNKNOWN));
    };
    Ok(file_browser::Entry::from_archive_path(path, entry.size))
}
