use std::path::{Path, PathBuf};

use crate::bridge::ui_error::UiError;
use crate::bridge::{AppPaths, ExtractDestination, Settings};
use crate::util::paths::path_to_display;

const HISTORY_LIMIT: usize = 20;

#[derive(Clone, Debug, PartialEq)]
pub struct SettingsSnapshot {
    pub(super) settings: Settings,
    pub(super) paths: AppPaths,
}

impl SettingsSnapshot {
    pub(crate) fn new(settings: Settings, paths: AppPaths) -> Self {
        Self { settings, paths }
    }
}

/// Declaration order is the tile rendering order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DestinationKind {
    Browse,
    Temp,
    Addons,
    Downloads,
}

impl DestinationKind {
    pub(crate) const ALL: [Self; 4] = [Self::Browse, Self::Temp, Self::Addons, Self::Downloads];

    pub(crate) const fn label_key(self) -> &'static str {
        match self {
            Self::Browse => "destination-browse",
            Self::Temp => "destination-open",
            Self::Addons => "destination-addons",
            Self::Downloads => "destination-downloads",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DestinationPersistRequest {
    pub(crate) destination: ExtractDestination,
    pub(crate) create_folder: bool,
    pub(crate) history_path: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DestinationError {
    InvalidPath,
    SaveFailed(UiError),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum DestinationSelection {
    None,
    Root(DestinationRoot),
    Custom(PathBuf),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum DestinationRoot {
    Temp,
    Addons,
    Downloads,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ResolvedDestinations {
    pub(super) temp: Option<PathBuf>,
    pub(super) addons: Option<PathBuf>,
    pub(super) downloads: Option<PathBuf>,
}

impl ResolvedDestinations {
    pub(super) fn from_paths(paths: &AppPaths) -> Self {
        Self {
            temp: Some(paths.temp_dir.clone()),
            addons: paths.gmod_dir.as_ref().and_then(|gmod| {
                let addons = gmod.join("GarrysMod").join("addons");
                addons.is_dir().then_some(addons)
            }),
            downloads: paths.downloads_dir.clone(),
        }
    }

    pub(super) fn path_for_root(&self, root: DestinationRoot) -> Option<&Path> {
        match root {
            DestinationRoot::Temp => self.temp.as_deref(),
            DestinationRoot::Addons => self.addons.as_deref(),
            DestinationRoot::Downloads => self.downloads.as_deref(),
        }
    }
}

pub fn apply_persist_request(settings: &mut Settings, request: DestinationPersistRequest) {
    settings.create_folder_on_extract = request.create_folder;
    settings.extract_destination = request.destination;

    if let Some(path) = request.history_path {
        push_history_destination(&mut settings.destinations, path);
    }
}

/// Formats the current extract destination for display (e.g. the downloader's
/// "set destination" row). Reads `settings.extract_destination` through the
/// same sanitization `sanitize` applies, without cloning the whole
/// `Settings` (its history list and per-workshop overrides are irrelevant
/// here).
pub fn destination_label(settings: &Settings, paths: &AppPaths) -> String {
    let destination = settings.sanitized_extract_destination(paths);
    let roots = ResolvedDestinations::from_paths(paths);
    match selection_from_extract_destination(&destination, &roots) {
        DestinationSelection::Root(DestinationRoot::Temp) => "Temporary".to_owned(),
        DestinationSelection::Root(DestinationRoot::Downloads) => "Downloads".to_owned(),
        DestinationSelection::Root(DestinationRoot::Addons) => "Garry's Mod addons".to_owned(),
        DestinationSelection::Custom(path) => path_to_display(path),
        DestinationSelection::None => roots
            .temp
            .as_ref()
            .map_or_else(|| "Temporary".to_owned(), path_to_display),
    }
}

fn push_history_destination(destinations: &mut Vec<PathBuf>, path: PathBuf) {
    destinations.retain(|existing| existing != &path);
    destinations.insert(0, path);
    destinations.truncate(HISTORY_LIMIT);
}

fn selection_from_extract_destination(
    destination: &ExtractDestination,
    roots: &ResolvedDestinations,
) -> DestinationSelection {
    match destination {
        ExtractDestination::Temp => DestinationSelection::Root(DestinationRoot::Temp),
        ExtractDestination::Downloads if roots.downloads.is_some() => {
            DestinationSelection::Root(DestinationRoot::Downloads)
        }
        ExtractDestination::Addons if roots.addons.is_some() => {
            DestinationSelection::Root(DestinationRoot::Addons)
        }
        ExtractDestination::Directory(path) | ExtractDestination::NamedDirectory(path)
            if valid_custom_path(path) =>
        {
            DestinationSelection::Custom(path.clone())
        }
        ExtractDestination::Downloads
        | ExtractDestination::Addons
        | ExtractDestination::Directory(_)
        | ExtractDestination::NamedDirectory(_) => DestinationSelection::None,
    }
}

pub(super) fn selection_to_extract_destination(
    selection: &DestinationSelection,
    create_folder: bool,
) -> Option<ExtractDestination> {
    match selection {
        DestinationSelection::None => None,
        DestinationSelection::Root(DestinationRoot::Temp) => Some(ExtractDestination::Temp),
        DestinationSelection::Root(DestinationRoot::Addons) => Some(ExtractDestination::Addons),
        DestinationSelection::Root(DestinationRoot::Downloads) => {
            Some(ExtractDestination::Downloads)
        }
        DestinationSelection::Custom(path) if create_folder => {
            Some(ExtractDestination::NamedDirectory(path.clone()))
        }
        DestinationSelection::Custom(path) => Some(ExtractDestination::Directory(path.clone())),
    }
}

pub(super) fn selected_base_path<'a>(
    selection: &'a DestinationSelection,
    roots: &'a ResolvedDestinations,
) -> Option<&'a Path> {
    match selection {
        DestinationSelection::None => None,
        DestinationSelection::Root(root) => roots.path_for_root(*root),
        DestinationSelection::Custom(path) => Some(path.as_path()),
    }
}

pub(super) fn valid_custom_path(path: &Path) -> bool {
    path.is_absolute() && path.is_dir()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::ExtractDestination;

    #[test]
    fn history_is_capped_and_deduped() {
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let existing = temp.path().join("existing");
        let mut destinations = vec![existing.clone()];
        for index in 0..HISTORY_LIMIT {
            destinations.push(temp.path().join(format!("d{index}")));
        }

        push_history_destination(&mut destinations, existing.clone());

        assert_eq!(destinations.len(), HISTORY_LIMIT);
        assert_eq!(destinations[0], existing);
    }

    #[test]
    fn apply_persist_request_updates_settings_history_and_create_folder() {
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let first = temp.path().join("first");
        let second = temp.path().join("second");
        let mut settings = Settings {
            destinations: vec![first.clone()],
            create_folder_on_extract: false,
            ..Settings::default()
        };

        apply_persist_request(
            &mut settings,
            DestinationPersistRequest {
                destination: ExtractDestination::NamedDirectory(second.clone()),
                create_folder: true,
                history_path: Some(second.clone()),
            },
        );

        assert!(settings.create_folder_on_extract);
        assert_eq!(
            settings.extract_destination,
            ExtractDestination::NamedDirectory(second.clone())
        );
        assert_eq!(settings.destinations, vec![second, first]);
    }
}
