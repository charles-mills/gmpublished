use super::*;
use crate::events::BackendEventCollector;
use crate::workshop_id::workshop_id;
use std::{fs, sync::Arc};

fn test_app_data(temp: &tempfile::TempDir) -> AppData {
    let paths = AppDataPaths::for_test_root(temp.path());
    let transactions = Transactions::new(Arc::new(BackendEventCollector::default()));
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
    ))
}

#[test]
fn a_post_rename_directory_sync_failure_does_not_report_the_commit_as_failed() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = AppDataPaths::for_test_root(temp.path());
    let settings = Settings {
        language: Some("committed".to_owned()),
        ..Settings::default()
    };

    settings
        .save_with_directory_sync(&paths, |_| {
            Err(std::io::Error::other("injected directory sync failure"))
        })
        .expect("rename is the logical commit point");

    let loaded = Settings::load(&paths).expect("committed settings remain readable");
    assert_eq!(loaded.language.as_deref(), Some("committed"));
}

#[test]
fn appdata_send_emits_typed_snapshot_without_window() {
    let temp = tempfile::tempdir().expect("tempdir");
    let collector = BackendEventCollector::default();
    let transactions = Transactions::new(Arc::new(collector.clone()));
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

    app_data.send();

    let events = collector.drain();
    assert_eq!(events.len(), 1);
    let BackendEvent::AppDataUpdated(snapshot) = &events[0] else {
        panic!("expected appdata event");
    };
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
    let transactions = Transactions::new(Arc::new(collector.clone()));
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
    let transactions = Transactions::new(Arc::new(collector.clone()));
    let app_data = test_app_data_with_transactions(&temp, transactions);

    let temp_dir = temp.path().join("temp");
    fs::create_dir_all(&temp_dir).expect("temp dir");

    let mut updated = app_data.settings.load().as_ref().clone();
    updated.temp = Some(temp_dir.clone());
    updated.sounds = !updated.sounds;
    updated.language = Some("en-US".to_owned());
    updated.ui = Some(serde_json::json!({
        "version": 7,
        "backend_must_preserve_this": true,
    }));

    app_data
        .update_settings(|settings| {
            settings.clone_from(&updated);
            SettingsEnvironment::default()
        })
        .expect("settings save");

    let stored = Settings::load(&app_data.paths).expect("stored settings");
    assert_eq!(stored.temp, Some(temp_dir.clone()));
    assert_eq!(stored.sounds, updated.sounds);
    assert_eq!(stored.language.as_deref(), Some("en-US"));
    assert_eq!(stored.ui, updated.ui);

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
fn concurrent_backend_mutations_cannot_erase_each_other() {
    let temp = tempfile::tempdir().expect("tempdir");
    let app_data = Arc::new(test_app_data(&temp));
    let barrier = Arc::new(std::sync::Barrier::new(3));

    let sounds_writer = {
        let app_data = Arc::clone(&app_data);
        let barrier = Arc::clone(&barrier);
        std::thread::spawn(move || {
            barrier.wait();
            app_data
                .update_settings(|settings| {
                    settings.sounds = false;
                    SettingsEnvironment::default()
                })
                .expect("sounds update");
        })
    };
    let language_writer = {
        let app_data = Arc::clone(&app_data);
        let barrier = Arc::clone(&barrier);
        std::thread::spawn(move || {
            barrier.wait();
            app_data
                .update_settings(|settings| {
                    settings.language = Some("fr".to_owned());
                    SettingsEnvironment::default()
                })
                .expect("language update");
        })
    };

    barrier.wait();
    sounds_writer.join().expect("sounds writer");
    language_writer.join().expect("language writer");

    let settings = app_data.settings();
    assert!(!settings.sounds);
    assert_eq!(settings.language.as_deref(), Some("fr"));
}

#[test]
fn a_failed_settings_save_does_not_publish_the_attempted_values() {
    let temp = tempfile::tempdir().expect("tempdir");
    let app_data = test_app_data(&temp);
    app_data.mutate_settings(|settings| settings.language = Some("previous".to_owned()));
    fs::create_dir_all(&app_data.paths.settings_file).expect("directory blocks the settings file");

    let result = app_data.update_settings(|settings| {
        settings.language = Some("unsaved".to_owned());
        SettingsEnvironment::default()
    });

    assert!(result.is_err(), "saving over a directory must fail");
    assert_eq!(
        app_data.settings.load().language.as_deref(),
        Some("previous")
    );
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
        .insert(workshop_id(10), valid_b.clone());
    settings
        .my_workshop_local_paths
        .insert(workshop_id(20), missing);
    settings.destinations.extend((0..25).map(|idx| {
        let path = temp.path().join(format!("extra-{idx}"));
        fs::create_dir_all(&path).expect("extra destination");
        path
    }));

    settings.sanitize(SettingsEnvironment::default());

    assert!(
        settings
            .destinations
            .iter()
            .all(|path| path.is_absolute() && path.is_dir())
    );
    assert_eq!(settings.destinations.len(), 20);
    assert_eq!(
        settings.my_workshop_local_paths.get(&workshop_id(10)),
        Some(&valid_b)
    );
    assert!(
        !settings
            .my_workshop_local_paths
            .contains_key(&workshop_id(20))
    );
    assert!(
        matches!(&settings.extract_destination, ExtractDestination::NamedDirectory(path) if path == &valid_a)
    );

    settings.create_folder_on_extract = false;
    settings.extract_destination = ExtractDestination::NamedDirectory(valid_a.clone());
    settings.sanitize(SettingsEnvironment::default());
    assert!(
        matches!(&settings.extract_destination, ExtractDestination::Directory(path) if path == &valid_a)
    );
}

#[test]
fn appdata_sanitize_context_defaults_unavailable_downloads_and_addons() {
    let unavailable = SettingsEnvironment::default();

    let mut downloads = Settings {
        extract_destination: ExtractDestination::Downloads,
        ..Settings::default()
    };
    downloads.sanitize(unavailable);
    assert!(matches!(
        downloads.extract_destination,
        ExtractDestination::Temp
    ));

    let mut addons = Settings {
        extract_destination: ExtractDestination::Addons,
        ..Settings::default()
    };
    addons.sanitize(unavailable);
    assert!(matches!(
        addons.extract_destination,
        ExtractDestination::Temp
    ));
}

#[test]
fn appdata_sanitize_context_retains_available_downloads_and_addons() {
    let available = SettingsEnvironment {
        downloads_dir_available: true,
        gmod_dir_available: true,
    };

    let mut downloads = Settings {
        extract_destination: ExtractDestination::Downloads,
        ..Settings::default()
    };
    downloads.sanitize(available);
    assert!(matches!(
        downloads.extract_destination,
        ExtractDestination::Downloads
    ));

    let mut addons = Settings {
        extract_destination: ExtractDestination::Addons,
        ..Settings::default()
    };
    addons.sanitize(available);
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

#[test]
fn unreadable_settings_are_preserved_before_defaults_replace_them() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = AppDataPaths::for_test_root(temp.path());
    fs::create_dir_all(paths.settings_file.parent().expect("settings dir")).expect("dir");
    let original = r#"{"gmod": "/games/gmod", this is not json"#;
    fs::write(&paths.settings_file, original).expect("write corrupt settings");

    let settings = Settings::load_or_default(&paths);

    assert_eq!(settings.gmod, Settings::default().gmod);
    let backup = paths.settings_file.with_extension("json.bak");
    assert_eq!(
        fs::read_to_string(&backup).expect("the unreadable file is kept"),
        original
    );
    assert!(
        !paths.settings_file.exists(),
        "the unreadable file is moved, not copied"
    );
}

#[test]
fn workshop_local_paths_round_trip_as_bare_integer_keys() {
    let mut settings = Settings::default();
    settings
        .my_workshop_local_paths
        .insert(workshop_id(12345), PathBuf::from("/addons/mine"));

    let json = serde_json::to_value(&settings).expect("settings serialize");
    assert_eq!(
        json["my_workshop_local_paths"],
        serde_json::json!({ "12345": "/addons/mine" })
    );

    let reloaded: Settings = serde_json::from_value(json).expect("settings deserialize");
    assert_eq!(
        reloaded.my_workshop_local_paths.get(&workshop_id(12345)),
        Some(&PathBuf::from("/addons/mine"))
    );
}

#[test]
fn a_stored_workshop_id_of_zero_drops_its_entry_without_failing_the_file() {
    let json = serde_json::json!({
        "my_workshop_local_paths": { "0": "/addons/nowhere", "7": "/addons/real" }
    });

    let settings: Settings = serde_json::from_value(json).expect("settings should still load");

    assert_eq!(settings.my_workshop_local_paths.len(), 1);
    assert_eq!(
        settings.my_workshop_local_paths.get(&workshop_id(7)),
        Some(&PathBuf::from("/addons/real"))
    );
}

#[test]
fn settings_json_shape_is_pinned_to_the_schema() {
    let json = serde_json::to_value(Settings::default()).expect("settings serialize");
    let object = json.as_object().expect("settings serialize to an object");

    assert_eq!(
        object.keys().map(String::as_str).collect::<Vec<_>>(),
        [
            "color_error",
            "color_neutral",
            "color_success",
            "create_folder_on_extract",
            "destinations",
            "downloads",
            "extract_destination",
            "extract_overwrite_mode",
            "gmod",
            "ignore_globs",
            "language",
            "my_workshop_local_paths",
            "schema",
            "sounds",
            "temp",
            "titlebar",
            "upscale_addon_icon",
            "user_data",
            "window_maximized",
            "window_size",
        ]
    );
    assert_eq!(object["schema"], serde_json::json!(SETTINGS_SCHEMA));

    for (destination, tag) in [
        (ExtractDestination::Temp, serde_json::json!("Temp")),
        (
            ExtractDestination::Downloads,
            serde_json::json!("Downloads"),
        ),
        (ExtractDestination::Addons, serde_json::json!("Addons")),
        (
            ExtractDestination::Directory(PathBuf::from("/d")),
            serde_json::json!({ "Directory": "/d" }),
        ),
        (
            ExtractDestination::NamedDirectory(PathBuf::from("/d")),
            serde_json::json!({ "NamedDirectory": "/d" }),
        ),
    ] {
        assert_eq!(serde_json::to_value(&destination).expect("serialize"), tag);
    }
    for (mode, tag) in [
        (ExtractionOverwriteMode::Overwrite, "Overwrite"),
        (ExtractionOverwriteMode::Recycle, "Recycle"),
        (ExtractionOverwriteMode::Delete, "Delete"),
    ] {
        assert_eq!(
            serde_json::to_value(&mode).expect("serialize"),
            serde_json::json!(tag)
        );
    }
    for (titlebar, tag) in [
        (TitlebarPreference::Auto, "Auto"),
        (TitlebarPreference::System, "System"),
    ] {
        assert_eq!(
            serde_json::to_value(titlebar).expect("serialize"),
            serde_json::json!(tag)
        );
    }
}

#[test]
fn saving_settings_writes_the_current_schema() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = AppDataPaths::for_test_root(temp.path());
    let stale = Settings {
        schema: 0,
        ..Settings::default()
    };

    stale.save(&paths).expect("settings save");

    let written: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&paths.settings_file).expect("written settings"))
            .expect("written settings parse");
    assert_eq!(written["schema"], serde_json::json!(SETTINGS_SCHEMA));
}

#[test]
fn settings_without_a_schema_load_as_the_original_shape() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = AppDataPaths::for_test_root(temp.path());
    fs::create_dir_all(paths.settings_file.parent().expect("settings dir")).expect("dir");
    fs::write(
        &paths.settings_file,
        r#"{"gmod": "/games/gmod", "sounds": false}"#,
    )
    .expect("write pre-versioning settings");

    let settings = Settings::load_or_default(&paths);

    assert_eq!(
        serde_json::from_str::<Settings>("{}")
            .expect("an empty object is a pre-versioning file")
            .schema,
        1,
        "a missing schema is the original shape, not whatever this build writes"
    );
    assert_eq!(settings.schema, SETTINGS_SCHEMA);
    assert_eq!(settings.gmod, Some(PathBuf::from("/games/gmod")));
    assert!(!settings.sounds);
    assert!(
        !paths.settings_file.with_extension("json.bak").exists(),
        "a pre-versioning file is readable, not corrupt"
    );
}

#[test]
fn schema_one_settings_explicitly_migrate_to_the_combined_schema() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = AppDataPaths::for_test_root(temp.path());
    fs::create_dir_all(paths.settings_file.parent().expect("settings parent"))
        .expect("settings parent");
    let mut value = serde_json::to_value(Settings::default()).expect("settings JSON");
    value["schema"] = serde_json::json!(1);
    fs::write(
        &paths.settings_file,
        serde_json::to_vec(&value).expect("settings bytes"),
    )
    .expect("schema-one settings");

    let settings = Settings::load(&paths).expect("schema-one settings load");

    assert_eq!(settings.schema, SETTINGS_SCHEMA);
    assert_eq!(SETTINGS_SCHEMA, 2);
    assert!(settings.ui.is_none());
}

#[test]
fn settings_from_a_newer_schema_are_kept_aside_rather_than_downgraded() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = AppDataPaths::for_test_root(temp.path());
    fs::create_dir_all(paths.settings_file.parent().expect("settings dir")).expect("dir");
    let original = format!(
        r#"{{"schema": {}, "gmod": "/games/gmod"}}"#,
        SETTINGS_SCHEMA + 1
    );
    fs::write(&paths.settings_file, &original).expect("write newer settings");

    let settings = Settings::load_or_default(&paths);

    assert_eq!(settings.gmod, None, "the newer file must not be adopted");
    assert_eq!(
        fs::read_to_string(paths.settings_file.with_extension("json.bak"))
            .expect("the newer file is kept"),
        original
    );
}

#[test]
fn a_second_unreadable_settings_file_does_not_replace_the_first_backup() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = AppDataPaths::for_test_root(temp.path());
    fs::create_dir_all(paths.settings_file.parent().expect("settings dir")).expect("dir");
    let backup = paths.settings_file.with_extension("json.bak");

    fs::write(&paths.settings_file, "the user's real settings, corrupted").expect("write");
    let _ = Settings::load_or_default(&paths);
    fs::write(&paths.settings_file, "a later, less valuable corruption").expect("write");
    let _ = Settings::load_or_default(&paths);

    assert_eq!(
        fs::read_to_string(&backup).expect("the first backup survives"),
        "the user's real settings, corrupted"
    );
}

#[test]
fn a_missing_settings_file_leaves_no_backup() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = AppDataPaths::for_test_root(temp.path());

    let _settings = Settings::load_or_default(&paths);

    assert!(!paths.settings_file.with_extension("json.bak").exists());
}
