use super::*;
use crate::events::BackendEventCollector;
use std::{fs, sync::Arc};

fn test_app_data(temp: &tempfile::TempDir) -> AppData {
    let paths = AppDataPaths::for_test_root(temp.path());
    let transactions = Transactions::new(Arc::new(BackendEventCollector::default()), false);
    AppData::load(paths, transactions)
}

fn test_app_data_with_transactions(
    temp: &tempfile::TempDir,
    transactions: Transactions,
) -> AppData {
    let paths = AppDataPaths::for_test_root(temp.path());
    AppData::load(paths, transactions)
}

fn test_steam() -> Steam {
    Steam::new(Transactions::new(
        Arc::new(BackendEventCollector::default()),
        false,
    ))
}

#[test]
fn settings_load_falls_back_to_legacy_gmpublisher_path_read_only() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = AppDataPaths::for_test_root(temp.path());

    let legacy = Settings {
        language: Some("legacy-marker".to_owned()),
        ..Settings::default()
    };
    if let Some(parent) = paths.legacy_settings_file.parent() {
        fs::create_dir_all(parent).expect("legacy settings dir");
    }
    fs::write(
        &paths.legacy_settings_file,
        serde_json::to_string(&legacy).expect("serialize legacy settings"),
    )
    .expect("write legacy settings");
    let legacy_bytes = fs::read(&paths.legacy_settings_file).expect("legacy bytes");

    let loaded = Settings::load(&paths).expect("load falls back to legacy path");
    assert_eq!(loaded.language.as_deref(), Some("legacy-marker"));

    // The fallback is read-only: loading must not create our file or
    // touch the legacy one.
    assert!(!paths.settings_file.exists());
    assert_eq!(
        fs::read(&paths.legacy_settings_file).expect("legacy bytes after"),
        legacy_bytes
    );

    // load_or_default() migrates: settings persist to our path, legacy untouched.
    let migrated = Settings::load_or_default(&paths);
    assert_eq!(migrated.language.as_deref(), Some("legacy-marker"));
    assert!(paths.settings_file.exists());
    assert_eq!(
        fs::read(&paths.legacy_settings_file).expect("legacy bytes after init"),
        legacy_bytes
    );
}

#[test]
fn appdata_send_emits_typed_snapshot_without_window() {
    let temp = tempfile::tempdir().expect("tempdir");
    let collector = BackendEventCollector::default();
    let transactions = Transactions::new(Arc::new(collector.clone()), false);
    let app_data = test_app_data_with_transactions(&temp, transactions);

    let temp_dir = temp.path().join("temp");
    let user_data_dir = temp.path().join("user-data");
    let downloads_dir = temp.path().join("downloads");
    let gmod_dir = temp.path().join("Garrys Mod");
    for path in [&temp_dir, &user_data_dir, &downloads_dir, &gmod_dir] {
        fs::create_dir_all(path).expect("fixture dir");
    }
    app_data.mutate_settings(|settings| {
        settings.temp = Some(temp_dir.clone());
        settings.user_data = Some(user_data_dir.clone());
        settings.downloads = Some(downloads_dir.clone());
        settings.gmod = Some(gmod_dir.clone());
        settings.language = Some("en-US".to_owned());
    });
    app_data.open_count.set(7);

    app_data.send();

    let events = collector.drain();
    assert_eq!(events.len(), 1);
    let BackendEvent::AppDataUpdated(snapshot) = &events[0] else {
        panic!("expected appdata event");
    };
    assert_eq!(snapshot.open_count, 7);
    assert_eq!(snapshot.settings.language.as_deref(), Some("en-US"));
    assert_eq!(snapshot.paths.temp_dir, temp_dir);
    assert_eq!(snapshot.paths.user_data_dir, user_data_dir);
    assert_eq!(snapshot.paths.downloads_dir, Some(downloads_dir));
    assert_eq!(snapshot.paths.gmod_dir, Some(gmod_dir));
}

#[test]
fn steam_init_appdata_send_decision_matches_gmod_override() {
    let temp = tempfile::tempdir().expect("tempdir");
    let app_data = test_app_data(&temp);
    assert!(app_data.should_send_after_steam_init_if_gmod_unset());

    app_data.mutate_settings(|settings| {
        settings.gmod = Some(PathBuf::from("/configured/gmod"));
    });
    assert!(!app_data.should_send_after_steam_init_if_gmod_unset());
}

#[test]
fn steam_init_appdata_send_skips_when_gmod_configured() {
    let temp = tempfile::tempdir().expect("tempdir");
    let gmod_dir = temp.path().join("Garrys Mod");
    fs::create_dir_all(&gmod_dir).expect("gmod dir");

    let collector = BackendEventCollector::default();
    let transactions = Transactions::new(Arc::new(collector.clone()), false);
    let app_data = test_app_data_with_transactions(&temp, transactions);
    app_data.mutate_settings(|settings| {
        settings.gmod = Some(gmod_dir.clone());
    });

    let steam = test_steam();
    app_data.send_after_steam_init_if_gmod_unset(&steam);

    assert!(
        collector.drain().is_empty(),
        "configured settings.gmod should not emit UpdateAppData after Steam init"
    );
}

#[test]
fn update_settings_saves_and_emits_appdata_snapshot() {
    let temp = tempfile::tempdir().expect("tempdir");
    let collector = BackendEventCollector::default();
    let transactions = Transactions::new(Arc::new(collector.clone()), false);
    let app_data = test_app_data_with_transactions(&temp, transactions);
    let steam = test_steam();

    let temp_dir = temp.path().join("temp");
    fs::create_dir_all(&temp_dir).expect("temp dir");

    let mut updated = app_data.settings.load().as_ref().clone();
    updated.temp = Some(temp_dir.clone());
    updated.sounds = !updated.sounds;
    updated.language = Some("en-US".to_owned());

    app_data
        .update_settings(updated.clone(), &steam)
        .expect("settings save");

    let stored = Settings::load(&app_data.paths).expect("stored settings");
    assert_eq!(stored.temp, Some(temp_dir.clone()));
    assert_eq!(stored.sounds, updated.sounds);
    assert_eq!(stored.language.as_deref(), Some("en-US"));

    let events = collector.drain();
    assert_eq!(events.len(), 1);
    let BackendEvent::AppDataUpdated(snapshot) = &events[0] else {
        panic!("expected appdata event");
    };
    assert_eq!(snapshot.settings.temp, Some(temp_dir.clone()));
    assert_eq!(snapshot.settings.sounds, updated.sounds);
    assert_eq!(snapshot.paths.temp_dir, temp_dir);
}

#[test]
#[cfg(unix)]
fn update_settings_failed_save_leaves_live_state_and_disk_untouched() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().expect("tempdir");
    let app_data = test_app_data(&temp);
    let steam = test_steam();

    let mut previous = app_data.settings.load().as_ref().clone();
    previous.language = Some("previous".to_owned());
    app_data
        .update_settings(previous.clone(), &steam)
        .expect("seed settings save");

    let settings_dir = app_data.paths.settings_file.parent().expect("settings dir");
    let original_mode = fs::metadata(settings_dir)
        .expect("settings dir metadata")
        .permissions();
    fs::set_permissions(settings_dir, fs::Permissions::from_mode(0o555))
        .expect("lock down settings dir");

    let mut attempted = previous;
    attempted.language = Some("unsaved".to_owned());
    let result = app_data.update_settings(attempted, &steam);

    // Restore before asserting so a failed assertion doesn't leave the
    // tempdir behind in a state the OS refuses to clean up.
    fs::set_permissions(settings_dir, original_mode).expect("restore settings dir permissions");

    assert!(
        result.is_err(),
        "save into an unwritable directory must fail"
    );
    assert_eq!(
        app_data.settings.load().language.as_deref(),
        Some("previous"),
        "live settings must be untouched by a failed save"
    );

    let on_disk = Settings::load(&app_data.paths).expect("settings file must still parse");
    assert_eq!(on_disk.language.as_deref(), Some("previous"));
}

#[test]
fn settings_snapshot_without_titlebar_field_defaults_to_auto() {
    let mut value = serde_json::to_value(Settings::default()).expect("settings json");
    value
        .as_object_mut()
        .expect("settings should be an object")
        .remove("titlebar");

    let settings: Settings =
        serde_json::from_value(value).expect("missing titlebar should deserialize");

    assert_eq!(settings.titlebar, TitlebarPreference::Auto);
}

#[test]
fn appdata_path_accessors_use_valid_overrides_and_default_fallbacks() {
    let temp = tempfile::tempdir().expect("tempdir");
    let app_data = test_app_data(&temp);
    let temp_dir = temp.path().join("temp");
    let user_data_dir = temp.path().join("user-data");
    let downloads_dir = temp.path().join("downloads");
    fs::create_dir_all(&temp_dir).expect("temp dir");
    fs::create_dir_all(&user_data_dir).expect("user data dir");
    fs::create_dir_all(&downloads_dir).expect("downloads dir");

    app_data.mutate_settings(|settings| {
        settings.temp = Some(temp_dir.clone());
        settings.user_data = Some(user_data_dir.clone());
        settings.downloads = Some(downloads_dir.clone());
    });
    assert_eq!(&*app_data.temp_dir(), &temp_dir);
    assert_eq!(&*app_data.user_data_dir(), &user_data_dir);
    assert_eq!(&app_data.downloads_dir(), &Some(downloads_dir));

    let missing_temp = temp.path().join("missing-temp");
    let missing_user_data = temp.path().join("missing-user-data");
    let missing_downloads = temp.path().join("missing-downloads");

    app_data.mutate_settings(|settings| {
        settings.temp = Some(missing_temp.clone());
        settings.user_data = Some(missing_user_data.clone());
        settings.downloads = Some(missing_downloads.clone());
    });
    assert_eq!(&*app_data.temp_dir(), &app_data.paths.default_temp_dir);
    assert_ne!(&*app_data.user_data_dir(), &missing_user_data);
    assert_ne!(&app_data.downloads_dir(), &Some(missing_downloads));
}

#[test]
fn logging_logs_dir_uses_current_temp_dir() {
    let temp = tempfile::tempdir().expect("tempdir");
    let app_data = test_app_data(&temp);
    let temp_dir = temp.path().join("temp");
    fs::create_dir_all(&temp_dir).expect("temp dir");

    app_data.mutate_settings(|settings| {
        settings.temp = Some(temp_dir.clone());
    });
    assert_eq!(app_data.logging_logs_dir(), temp_dir.join("logs"));
}

#[test]
fn gma_extraction_context_snapshots_paths_and_preserves_gmod_laziness() {
    let temp = tempfile::tempdir().expect("tempdir");
    let app_data = test_app_data(&temp);
    let steam = test_steam();
    let temp_dir = temp.path().join("temp");
    let downloads_dir = temp.path().join("downloads");
    let gmod_dir = temp.path().join("gmod");
    for path in [&temp_dir, &downloads_dir, &gmod_dir] {
        fs::create_dir_all(path).expect("fixture dir");
    }

    app_data.mutate_settings(|settings| {
        settings.temp = Some(temp_dir.clone());
        settings.downloads = Some(downloads_dir.clone());
        settings.gmod = Some(gmod_dir.clone());
        settings.extract_overwrite_mode = ExtractionOverwriteMode::Delete;
    });

    let without_gmod = app_data.extraction_context(&steam, false);
    assert_eq!(without_gmod.temp_dir, temp_dir);
    assert_eq!(without_gmod.downloads_dir, Some(downloads_dir));
    assert_eq!(without_gmod.gmod_dir, None);
    assert_eq!(without_gmod.overwrite_mode, ExtractionOverwriteMode::Delete);

    let with_gmod = app_data.extraction_context(&steam, true);
    assert_eq!(with_gmod.gmod_dir, Some(gmod_dir));
}

#[test]
fn appdata_downloads_fallback_snapshot_uses_default_for_missing_or_invalid_override() {
    let temp = tempfile::tempdir().expect("tempdir");
    let app_data = test_app_data(&temp);
    let downloads_dir = temp.path().join("downloads");
    let missing_downloads = temp.path().join("missing-downloads");
    let gmod_dir = temp.path().join("gmod");
    fs::create_dir_all(&downloads_dir).expect("downloads dir");
    fs::create_dir_all(&gmod_dir).expect("gmod dir");

    app_data.mutate_settings(|settings| {
        settings.downloads = Some(downloads_dir.clone());
        settings.gmod = Some(gmod_dir.clone());
    });
    let snapshot = app_data.snapshot();
    assert_eq!(snapshot.settings.downloads, Some(downloads_dir.clone()));
    assert_eq!(snapshot.paths.downloads_dir, Some(downloads_dir));

    app_data.mutate_settings(|settings| {
        settings.downloads = Some(missing_downloads.clone());
        settings.gmod = Some(gmod_dir.clone());
    });
    let snapshot = app_data.snapshot();
    assert_eq!(snapshot.settings.downloads, Some(missing_downloads.clone()));
    assert_ne!(
        snapshot.paths.downloads_dir.as_ref(),
        Some(&missing_downloads),
        "invalid settings.downloads override must not be returned as the resolved downloads directory"
    );
    assert_eq!(
        snapshot.paths.downloads_dir, snapshot.paths.default_downloads_dir,
        "invalid settings.downloads override must fall back to the imported default downloads directory"
    );

    app_data.mutate_settings(|settings| {
        settings.downloads = None;
        settings.gmod = Some(gmod_dir.clone());
    });
    let snapshot = app_data.snapshot();
    assert_eq!(
        snapshot.paths.downloads_dir, snapshot.paths.default_downloads_dir,
        "missing settings.downloads override must use the imported default downloads directory"
    );
}

#[test]
fn appdata_sanitize_retains_valid_paths_and_normalizes_extract_destination() {
    let temp = tempfile::tempdir().expect("tempdir");
    let valid_a = temp.path().join("valid-a");
    let valid_b = temp.path().join("valid-b");
    let missing = temp.path().join("missing");
    fs::create_dir_all(&valid_a).expect("valid a");
    fs::create_dir_all(&valid_b).expect("valid b");

    let mut settings = Settings {
        destinations: vec![valid_a.clone(), missing.clone(), PathBuf::from("relative")],
        create_folder_on_extract: true,
        extract_destination: ExtractDestination::Directory(valid_a.clone()),
        ..Settings::default()
    };
    settings
        .my_workshop_local_paths
        .insert(PublishedFileId(10), valid_b.clone());
    settings
        .my_workshop_local_paths
        .insert(PublishedFileId(20), missing);
    settings.destinations.extend((0..25).map(|idx| {
        let path = temp.path().join(format!("extra-{idx}"));
        fs::create_dir_all(&path).expect("extra destination");
        path
    }));

    settings.sanitize_with_context(&SettingsSanitizeContext::default());

    assert!(
        settings
            .destinations
            .iter()
            .all(|path| path.is_absolute() && path.is_dir())
    );
    assert_eq!(settings.destinations.len(), 20);
    assert_eq!(
        settings.my_workshop_local_paths.get(&PublishedFileId(10)),
        Some(&valid_b)
    );
    assert!(
        !settings
            .my_workshop_local_paths
            .contains_key(&PublishedFileId(20))
    );
    assert!(
        matches!(&settings.extract_destination, ExtractDestination::NamedDirectory(path) if path == &valid_a)
    );

    settings.create_folder_on_extract = false;
    settings.extract_destination = ExtractDestination::NamedDirectory(valid_a.clone());
    settings.sanitize_with_context(&SettingsSanitizeContext::default());
    assert!(
        matches!(&settings.extract_destination, ExtractDestination::Directory(path) if path == &valid_a)
    );
}

#[test]
fn appdata_sanitize_context_defaults_unavailable_downloads_and_addons() {
    let unavailable = SettingsSanitizeContext::default();

    let mut downloads = Settings {
        extract_destination: ExtractDestination::Downloads,
        ..Settings::default()
    };
    downloads.sanitize_with_context(&unavailable);
    assert!(matches!(
        downloads.extract_destination,
        ExtractDestination::Temp
    ));

    let mut addons = Settings {
        extract_destination: ExtractDestination::Addons,
        ..Settings::default()
    };
    addons.sanitize_with_context(&unavailable);
    assert!(matches!(
        addons.extract_destination,
        ExtractDestination::Temp
    ));
}

#[test]
fn appdata_sanitize_context_retains_available_downloads_and_addons() {
    let available = SettingsSanitizeContext {
        downloads_dir_available: true,
        gmod_dir_available: true,
    };

    let mut downloads = Settings {
        extract_destination: ExtractDestination::Downloads,
        ..Settings::default()
    };
    downloads.sanitize_with_context(&available);
    assert!(matches!(
        downloads.extract_destination,
        ExtractDestination::Downloads
    ));

    let mut addons = Settings {
        extract_destination: ExtractDestination::Addons,
        ..Settings::default()
    };
    addons.sanitize_with_context(&available);
    assert!(matches!(
        addons.extract_destination,
        ExtractDestination::Addons
    ));
}

#[test]
fn appdata_validate_gmod_requires_absolute_garrysmod_addons_dir() {
    let dir = tempfile::tempdir().expect("tempdir");
    let gmod = dir.path().join("Garrys Mod");
    fs::create_dir_all(gmod.join("GarrysMod/addons")).expect("addons dir");

    assert!(validate_gmod(gmod));
    assert!(!validate_gmod(dir.path().join("missing")));
    assert!(!validate_gmod(PathBuf::from("relative")));
}
