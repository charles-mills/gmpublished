use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::Arc,
};

use arc_swap::ArcSwap;
use gmpublished_backend::{
    AppDataSnapshot as BackendAppDataSnapshot, Backend, SettingsEnvironment,
};
use parking_lot::Mutex;

use super::{
    AppPaths, ExtractDestination, Settings, SettingsPersistError, appdata_snapshot_from_backend,
};

const LEGACY_UI_SETTINGS_FILE_NAME: &str = "ui-settings.json";

/// One immutable, internally consistent view of persisted settings and every
/// path derived from them.
#[derive(Clone, Debug)]
pub(crate) struct ResolvedConfig {
    pub(crate) settings: Settings,
    pub(crate) paths: AppPaths,
    backend_revision: u64,
}

/// The app's sole configuration owner.
///
/// Readers load an `Arc` without locking. Writers serialize the complete
/// mutate -> resolve -> persist -> publish transaction under `writer`.
#[derive(Debug)]
pub(crate) struct ConfigStore {
    current: ArcSwap<ResolvedConfig>,
    writer: Mutex<()>,
    persist: bool,
}

impl ConfigStore {
    pub(crate) fn from_backend(backend: &Backend, persist: bool) -> Self {
        let snapshot = if persist {
            migrate_legacy_ui_settings(backend)
        } else {
            backend.app_data_snapshot()
        };
        let backend_revision = snapshot.settings_revision;
        let (settings, paths) = appdata_snapshot_from_backend(snapshot);

        Self {
            current: ArcSwap::from_pointee(ResolvedConfig {
                settings,
                paths,
                backend_revision,
            }),
            writer: Mutex::new(()),
            persist,
        }
    }

    #[must_use]
    pub(crate) fn snapshot(&self) -> Arc<ResolvedConfig> {
        self.current.load_full()
    }

    pub(crate) fn update(
        &self,
        backend: &Backend,
        update: impl FnOnce(&mut Settings),
    ) -> Result<(), SettingsPersistError> {
        let _writer = self.writer.lock();
        let current = self.current.load_full();

        if self.persist {
            backend.update_settings(|backend_settings| {
                if backend_settings.ui.is_none() {
                    backend_settings.ui = current.settings.to_backend().ui;
                }
                let mut candidate = Settings::from_backend(backend_settings.clone());
                update(&mut candidate);
                let resolved_paths = Self::resolve_paths(&candidate, &current, backend);
                let environment = SettingsEnvironment::new(
                    resolved_paths.downloads_dir.is_some(),
                    resolved_paths.gmod_dir.is_some(),
                );
                *backend_settings = candidate.to_backend();
                environment
            })?;
            let snapshot = backend.app_data_snapshot();
            let backend_revision = snapshot.settings_revision;
            let (settings, paths) = appdata_snapshot_from_backend(snapshot);
            self.current.store(Arc::new(ResolvedConfig {
                settings,
                paths,
                backend_revision,
            }));
        } else {
            let mut candidate = current.settings.clone();
            update(&mut candidate);
            let resolved_paths = Self::resolve_paths(&candidate, &current, backend);
            let environment = SettingsEnvironment::new(
                resolved_paths.downloads_dir.is_some(),
                resolved_paths.gmod_dir.is_some(),
            );
            let mut backend = candidate.to_backend();
            backend.sanitize(environment);
            candidate = Settings::from_backend(backend);
            self.current.store(Arc::new(ResolvedConfig {
                settings: candidate,
                paths: resolved_paths,
                backend_revision: current.backend_revision,
            }));
        }
        Ok(())
    }

    pub(crate) fn apply_backend_snapshot(
        &self,
        mut snapshot: BackendAppDataSnapshot,
    ) -> Arc<ResolvedConfig> {
        let _writer = self.writer.lock();
        let current = self.current.load_full();
        if snapshot.settings_revision < current.backend_revision {
            return current;
        }
        if snapshot.settings.ui.is_none() {
            snapshot.settings.ui = current.settings.to_backend().ui;
        }
        let backend_revision = snapshot.settings_revision;
        let (settings, paths) = appdata_snapshot_from_backend(snapshot);
        let resolved = Arc::new(ResolvedConfig {
            settings,
            paths,
            backend_revision,
        });
        self.current.store(Arc::clone(&resolved));
        resolved
    }

    pub(crate) fn publish_discovered_gmod(&self, discovered: Option<PathBuf>) {
        let _writer = self.writer.lock();
        let current = self.current.load_full();
        let mut next = ResolvedConfig::clone(&current);
        next.paths.gmod_dir = discovered;
        self.current.store(Arc::new(next));
    }

    fn resolve_paths(
        candidate: &Settings,
        current: &ResolvedConfig,
        backend: &Backend,
    ) -> AppPaths {
        let mut resolved = AppPaths::resolve_with_defaults(candidate, current.paths.clone());
        let previously_discovered = current
            .settings
            .backend
            .gmod
            .is_none()
            .then(|| current.paths.gmod_dir.clone())
            .flatten()
            .filter(|path| path.is_dir());
        resolved.gmod_dir = candidate
            .backend
            .gmod
            .as_ref()
            .filter(|path| path.is_dir())
            .cloned()
            .or(previously_discovered);
        if matches!(
            &candidate.backend.extract_destination,
            ExtractDestination::Addons
        ) {
            resolved.gmod_dir =
                backend.discover_gmod_dir_for_settings(candidate.backend.gmod.as_deref());
        }
        resolved
    }
}

/// Imports the pre-unification sibling UI document exactly once.
///
/// The backend's ordinary serialized settings transaction supplies the
/// atomic write and in-memory publication. The old file is retired only
/// after a fresh snapshot proves that the exact opaque document is present
/// in the authoritative settings file.
fn migrate_legacy_ui_settings(backend: &Backend) -> BackendAppDataSnapshot {
    let initial = backend.app_data_snapshot();
    let legacy_file = initial.paths.settings_file.parent().map_or_else(
        || PathBuf::from(LEGACY_UI_SETTINGS_FILE_NAME),
        |parent| parent.join(LEGACY_UI_SETTINGS_FILE_NAME),
    );
    let contents = match fs::read_to_string(&legacy_file) {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return initial,
        Err(error) => {
            log::warn!(
                "failed to read legacy UI settings {}: {error}",
                legacy_file.display()
            );
            return initial;
        }
    };
    let legacy_ui = match serde_json::from_str::<serde_json::Value>(&contents) {
        Ok(value) => value,
        Err(error) => {
            log::warn!(
                "failed to parse legacy UI settings {}: {error}",
                legacy_file.display()
            );
            return initial;
        }
    };

    if initial.settings.ui.is_none() {
        let environment = SettingsEnvironment::new(
            initial.paths.downloads_dir.is_some(),
            initial.paths.gmod_dir.is_some(),
        );
        if let Err(error) = backend.update_settings(|settings| {
            if settings.ui.is_none() {
                settings.ui = Some(legacy_ui.clone());
            }
            environment
        }) {
            log::warn!(
                "failed to migrate legacy UI settings {}: {error}",
                legacy_file.display()
            );
            return backend.app_data_snapshot();
        }
    }

    let current = backend.app_data_snapshot();
    if current.settings.ui.as_ref() == Some(&legacy_ui) {
        retire_legacy_ui_file(&legacy_file);
    }
    current
}

fn retire_legacy_ui_file(path: &Path) {
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return,
        Err(error) => {
            log::warn!(
                "unified settings were saved, but legacy UI settings {} could not be removed: {error}",
                path.display()
            );
            return;
        }
    }

    if let Some(parent) = path.parent()
        && let Err(error) = sync_directory(parent)
    {
        log::warn!(
            "legacy UI settings were removed, but directory {} could not be synced: {error}",
            parent.display()
        );
    }
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> io::Result<()> {
    fs::File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> io::Result<()> {
    Ok(())
}
