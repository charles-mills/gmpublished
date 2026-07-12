use std::path::{Path, PathBuf};

use crate::backend::ui_error::UiError;
use crate::backend::{AppPaths, Settings};
use crate::util::paths::{fallback_current_dir, fallback_paths, path_to_display};

use super::model::{
    self, DestinationError, DestinationKind, DestinationPersistRequest, DestinationRoot,
    DestinationSelection, ResolvedDestinations, SettingsSnapshot,
};

/// Placeholder shown when the caller supplied no extracted name.
const FALLBACK_EXTRACTED_NAME: &str = "addon_name";

/// Caller-specific facts threaded through the open message: the confirm
/// button label, the name appended to previewed paths, and whether the
/// create-folder behavior is forced on (downloader set-destination).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OpenContext {
    pub(crate) confirm_label_key: &'static str,
    pub(crate) extracted_name: Option<String>,
    pub(crate) force_create_folder: bool,
}

impl Default for OpenContext {
    fn default() -> Self {
        Self {
            confirm_label_key: "destination-extract",
            extracted_name: None,
            force_create_folder: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct State {
    settings: Settings,
    paths: AppPaths,
    roots: ResolvedDestinations,
    selection: DestinationSelection,
    path_input: String,
    saving: bool,
    error: Option<DestinationError>,
    context: OpenContext,
}

impl Default for State {
    fn default() -> Self {
        let mut settings = Settings::default();
        let paths = fallback_paths(&settings);
        settings.sanitize(&paths);
        let roots = ResolvedDestinations::from_paths(&paths);
        Self {
            settings,
            paths,
            roots,
            selection: DestinationSelection::None,
            path_input: String::new(),
            saving: false,
            error: None,
            context: OpenContext::default(),
        }
    }
}

impl State {
    /// Resets from fresh settings; the chooser always opens UNSELECTED.
    pub(crate) fn reset_from_snapshot(&mut self, snapshot: SettingsSnapshot) {
        self.absorb_snapshot(snapshot);
        self.selection = DestinationSelection::None;
        self.path_input.clear();
        self.saving = false;
        self.error = None;
    }

    pub(crate) fn open(&mut self, snapshot: SettingsSnapshot, context: OpenContext) {
        self.reset_from_snapshot(snapshot);
        self.context = context;
    }

    /// Refreshes settings/paths without touching the live selection; used
    /// when the create-folder checkbox persists mid-session.
    pub(crate) fn absorb_snapshot(&mut self, snapshot: SettingsSnapshot) {
        let SettingsSnapshot {
            mut settings,
            paths,
        } = snapshot;
        settings.sanitize(&paths);
        self.roots = ResolvedDestinations::from_paths(&paths);
        self.settings = settings;
        self.paths = paths;
    }

    pub(crate) fn error(&self) -> Option<&DestinationError> {
        self.error.as_ref()
    }

    pub(crate) fn path_input(&self) -> &str {
        &self.path_input
    }

    pub(crate) const fn settings(&self) -> &Settings {
        &self.settings
    }

    pub(crate) const fn paths(&self) -> &AppPaths {
        &self.paths
    }

    pub(crate) const fn create_folder(&self) -> bool {
        self.settings.create_folder_on_extract
    }

    pub(crate) fn confirm_label_key(&self) -> &'static str {
        self.context.confirm_label_key
    }

    /// The create-folder checkbox renders only for a live Browse/custom
    /// selection and never when the caller forces folder creation.
    pub(crate) const fn shows_create_folder(&self) -> bool {
        matches!(self.selection, DestinationSelection::Custom(_))
            && !self.context.force_create_folder
    }

    pub(crate) fn history(&self) -> &[PathBuf] {
        &self.settings.destinations
    }

    pub(crate) fn is_history_selected(&self, path: &Path) -> bool {
        matches!(&self.selection, DestinationSelection::Custom(selected) if selected == path)
    }

    pub(crate) fn kind_active(&self, kind: DestinationKind) -> bool {
        matches!(
            (kind, &self.selection),
            (
                DestinationKind::Temp,
                DestinationSelection::Root(DestinationRoot::Temp)
            ) | (
                DestinationKind::Downloads,
                DestinationSelection::Root(DestinationRoot::Downloads)
            ) | (
                DestinationKind::Addons,
                DestinationSelection::Root(DestinationRoot::Addons)
            ) | (DestinationKind::Browse, DestinationSelection::Custom(_))
        )
    }

    pub(crate) fn kind_enabled(&self, kind: DestinationKind) -> bool {
        match kind {
            DestinationKind::Temp | DestinationKind::Browse => true,
            DestinationKind::Downloads => self.roots.downloads.is_some(),
            DestinationKind::Addons => self.roots.addons.is_some(),
        }
    }

    pub(crate) fn selected_path(&self) -> Option<&Path> {
        model::selected_base_path(&self.selection, &self.roots)
    }

    /// Composes the path-input placeholder: the live selection's full path,
    /// else the extracted name.
    pub(crate) fn placeholder(&self) -> String {
        let name = self
            .context
            .extracted_name
            .as_deref()
            .unwrap_or(FALLBACK_EXTRACTED_NAME);

        if let Some(base) = model::selected_base_path(&self.selection, &self.roots) {
            let append = match &self.selection {
                DestinationSelection::Root(_) => true,
                DestinationSelection::Custom(_) => {
                    self.settings.create_folder_on_extract || self.context.force_create_folder
                }
                DestinationSelection::None => false,
            };
            return compose_placeholder(base, append, name);
        }

        name.to_owned()
    }

    pub(crate) fn can_confirm(&self) -> bool {
        !self.saving && self.persist_request().is_some()
    }

    pub(crate) fn initial_browse_directory(&self) -> PathBuf {
        self.selected_path()
            .map(Path::to_path_buf)
            .or_else(|| self.settings.destinations.first().cloned())
            .unwrap_or_else(fallback_current_dir)
    }

    /// Click-toggles a root tile: selecting when enabled, deselecting when
    /// already active. Browse toggling is handled by the update loop, which
    /// routes to the folder picker.
    pub(crate) fn toggle_kind(&mut self, kind: DestinationKind) {
        self.error = None;
        if self.kind_active(kind) {
            self.deselect();
            return;
        }
        let selection = match kind {
            DestinationKind::Temp if self.roots.temp.is_some() => {
                Some(DestinationSelection::Root(DestinationRoot::Temp))
            }
            DestinationKind::Downloads if self.roots.downloads.is_some() => {
                Some(DestinationSelection::Root(DestinationRoot::Downloads))
            }
            DestinationKind::Addons if self.roots.addons.is_some() => {
                Some(DestinationSelection::Root(DestinationRoot::Addons))
            }
            DestinationKind::Browse
            | DestinationKind::Temp
            | DestinationKind::Downloads
            | DestinationKind::Addons => None,
        };
        if let Some(selection) = selection {
            self.selection = selection;
            self.path_input.clear();
        }
    }

    pub(crate) fn deselect(&mut self) {
        self.selection = DestinationSelection::None;
        self.path_input.clear();
        self.error = None;
    }

    pub(crate) fn edit_path_input(&mut self, value: String) {
        self.error = None;
        self.path_input = value;
    }

    /// Enter in the path input: trims trailing separators and selects the
    /// typed path as a Browse destination; empty input deselects.
    pub(crate) fn accept_path_input(&mut self) {
        let trimmed = self
            .path_input
            .trim()
            .trim_end_matches(['/', '\\'])
            .to_owned();
        if trimmed.is_empty() {
            self.deselect();
        } else {
            self.select_custom(PathBuf::from(trimmed));
        }
    }

    pub(crate) fn select_custom(&mut self, path: PathBuf) {
        if model::valid_custom_path(&path) {
            self.selection = DestinationSelection::Custom(path);
            self.path_input.clear();
            self.error = None;
        } else {
            self.path_input = path_to_display(path);
            self.selection = DestinationSelection::None;
            self.error = Some(DestinationError::InvalidPath);
        }
    }

    pub(crate) fn set_create_folder(&mut self, enabled: bool) {
        self.settings.create_folder_on_extract = enabled;
        self.error = None;
    }

    pub(crate) fn set_error(&mut self, error: UiError) {
        self.error = Some(DestinationError::SaveFailed(error));
    }

    pub(crate) fn begin_save(&mut self) {
        self.saving = true;
        self.error = None;
    }

    pub(crate) fn apply_save_result(&mut self, result: Result<SettingsSnapshot, UiError>) {
        match result {
            Ok(snapshot) => self.reset_from_snapshot(snapshot),
            Err(error) => {
                self.saving = false;
                self.error = Some(DestinationError::SaveFailed(error));
            }
        }
    }

    pub(crate) fn persist_request(&self) -> Option<DestinationPersistRequest> {
        let destination = model::selection_to_extract_destination(
            &self.selection,
            self.settings.create_folder_on_extract || self.context.force_create_folder,
        )?;
        let history_path = match &self.selection {
            DestinationSelection::Custom(path) => Some(path.clone()),
            DestinationSelection::None | DestinationSelection::Root(_) => None,
        };

        Some(DestinationPersistRequest {
            destination,
            create_folder: self.settings.create_folder_on_extract,
            history_path,
        })
    }
}

fn compose_placeholder(base: &Path, append_name: bool, name: &str) -> String {
    if append_name {
        path_to_display(base.join(name))
    } else {
        path_to_display(base)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use crate::backend::ExtractDestination;

    use super::*;

    fn snapshot_with_roots(temp: &tempfile::TempDir) -> SettingsSnapshot {
        let temp_root = temp.path().join("temp");
        let downloads_root = temp.path().join("downloads");
        let gmod_root = temp.path().join("gmod");
        fs::create_dir_all(&temp_root).expect("temp root");
        fs::create_dir_all(&downloads_root).expect("downloads root");
        fs::create_dir_all(gmod_root.join("GarrysMod/addons")).expect("addons root");
        let paths = AppPaths {
            settings_file: temp.path().join("settings.json"),
            default_user_data_dir: temp.path().join("user-data-default"),
            default_temp_dir: temp_root.clone(),
            default_downloads_dir: Some(downloads_root.clone()),
            temp_dir: temp_root,
            user_data_dir: temp.path().join("user-data"),
            downloads_dir: Some(downloads_root),
            gmod_dir: Some(gmod_root),
        };
        SettingsSnapshot::new(Settings::default(), paths)
    }

    fn named_context(name: &str) -> OpenContext {
        OpenContext {
            extracted_name: Some(name.to_owned()),
            ..OpenContext::default()
        }
    }

    #[test]
    fn open_always_resets_to_an_unselected_chooser() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut state = State::default();
        let snapshot = snapshot_with_roots(&temp);

        state.open(snapshot.clone(), named_context("dachi_2575621404"));
        state.toggle_kind(DestinationKind::Temp);
        assert!(state.can_confirm());

        state.open(snapshot, named_context("dachi_2575621404"));
        assert!(!state.can_confirm());
        assert_eq!(state.path_input(), "");
        assert_eq!(state.placeholder(), "dachi_2575621404");
    }

    #[test]
    fn tile_clicks_toggle_selection() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut state = State::default();
        state.open(snapshot_with_roots(&temp), OpenContext::default());

        state.toggle_kind(DestinationKind::Downloads);
        assert!(state.kind_active(DestinationKind::Downloads));

        state.toggle_kind(DestinationKind::Downloads);
        assert!(!state.kind_active(DestinationKind::Downloads));
        assert!(!state.can_confirm());
    }

    #[test]
    fn placeholder_appends_the_name_for_root_tiles() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut state = State::default();
        state.open(snapshot_with_roots(&temp), named_context("my_addon"));

        state.toggle_kind(DestinationKind::Addons);

        let expected = temp
            .path()
            .join("gmod/GarrysMod/addons/my_addon")
            .to_string_lossy()
            .into_owned();
        assert_eq!(state.placeholder(), expected);
    }

    #[test]
    fn placeholder_for_browse_follows_the_create_folder_checkbox() {
        let temp = tempfile::tempdir().expect("tempdir");
        let custom = temp.path().join("custom");
        fs::create_dir(&custom).expect("custom dir");
        let mut state = State::default();
        state.open(snapshot_with_roots(&temp), named_context("my_addon"));
        state.select_custom(custom.clone());

        state.set_create_folder(false);
        assert_eq!(state.placeholder(), custom.to_string_lossy());

        state.set_create_folder(true);
        assert_eq!(
            state.placeholder(),
            custom.join("my_addon").to_string_lossy()
        );
    }

    #[test]
    fn forced_create_folder_appends_the_name_and_hides_the_checkbox() {
        let temp = tempfile::tempdir().expect("tempdir");
        let custom = temp.path().join("custom");
        fs::create_dir(&custom).expect("custom dir");
        let mut state = State::default();
        state.open(
            snapshot_with_roots(&temp),
            OpenContext {
                confirm_label_key: "destination-set-destination",
                extracted_name: None,
                force_create_folder: true,
            },
        );
        state.select_custom(custom.clone());
        state.set_create_folder(false);

        assert!(!state.shows_create_folder());
        assert_eq!(
            state.placeholder(),
            custom.join(FALLBACK_EXTRACTED_NAME).to_string_lossy()
        );
        assert_eq!(
            state.persist_request().map(|request| request.destination),
            Some(ExtractDestination::NamedDirectory(custom))
        );
    }

    #[test]
    fn create_folder_checkbox_shows_only_for_custom_selections() {
        let temp = tempfile::tempdir().expect("tempdir");
        let custom = temp.path().join("custom");
        fs::create_dir(&custom).expect("custom dir");
        let mut state = State::default();
        state.open(snapshot_with_roots(&temp), OpenContext::default());

        assert!(!state.shows_create_folder());
        state.toggle_kind(DestinationKind::Temp);
        assert!(!state.shows_create_folder());
        state.select_custom(custom);
        assert!(state.shows_create_folder());
    }

    #[test]
    fn accepted_input_trims_trailing_separators() {
        let temp = tempfile::tempdir().expect("tempdir");
        let custom = temp.path().join("typed");
        fs::create_dir(&custom).expect("typed dir");
        let mut state = State::default();
        state.open(snapshot_with_roots(&temp), OpenContext::default());

        state.edit_path_input(format!("{}//", custom.to_string_lossy()));
        state.accept_path_input();

        assert!(state.kind_active(DestinationKind::Browse));
        assert!(state.is_history_selected(&custom));
        assert!(state.error().is_none());
    }

    #[test]
    fn invalid_typed_paths_keep_the_error_and_block_confirm() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut state = State::default();
        state.open(snapshot_with_roots(&temp), OpenContext::default());

        state.edit_path_input("/definitely/not/a/real/dir".to_owned());
        state.accept_path_input();

        assert_eq!(state.error(), Some(&DestinationError::InvalidPath));
        assert!(!state.can_confirm());
    }

    #[test]
    fn history_click_selects_the_row_as_a_custom_destination() {
        let temp = tempfile::tempdir().expect("tempdir");
        let first = temp.path().join("first");
        let second = temp.path().join("second");
        fs::create_dir(&first).expect("first dir");
        fs::create_dir(&second).expect("second dir");
        let SettingsSnapshot { settings, paths } = snapshot_with_roots(&temp);
        let settings = Settings {
            destinations: vec![first.clone(), second.clone()],
            ..settings
        };
        let mut state = State::default();
        state.open(
            SettingsSnapshot::new(settings, paths),
            OpenContext::default(),
        );

        state.select_custom(second.clone());

        assert!(state.is_history_selected(&second));
        assert!(!state.is_history_selected(&first));
        assert!(state.kind_active(DestinationKind::Browse));
    }

    #[test]
    fn custom_selection_maps_create_folder_to_named_directory() {
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let destination = temp.path().join("custom");
        fs::create_dir(&destination).expect("destination should exist");
        let mut state = State::default();
        state.open(snapshot_with_roots(&temp), OpenContext::default());

        state.select_custom(destination.clone());
        state.set_create_folder(true);

        assert_eq!(
            state.persist_request().map(|request| request.destination),
            Some(ExtractDestination::NamedDirectory(destination))
        );
    }

    #[test]
    fn root_selections_persist_backend_extract_destinations() {
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let mut state = State::default();
        state.open(snapshot_with_roots(&temp), OpenContext::default());

        state.toggle_kind(DestinationKind::Temp);
        assert_eq!(
            state.persist_request().map(|request| request.destination),
            Some(ExtractDestination::Temp)
        );

        state.toggle_kind(DestinationKind::Downloads);
        assert_eq!(
            state.persist_request().map(|request| request.destination),
            Some(ExtractDestination::Downloads)
        );

        state.toggle_kind(DestinationKind::Addons);
        assert_eq!(
            state.persist_request().map(|request| request.destination),
            Some(ExtractDestination::Addons)
        );
    }

    #[test]
    fn absorb_snapshot_keeps_the_live_selection() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut state = State::default();
        state.open(snapshot_with_roots(&temp), OpenContext::default());
        state.toggle_kind(DestinationKind::Temp);

        state.absorb_snapshot(snapshot_with_roots(&temp));

        assert!(state.kind_active(DestinationKind::Temp));
        assert!(state.can_confirm());
    }
}
