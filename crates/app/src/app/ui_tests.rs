use super::*;
use crate::generation::Generation;

#[test]
fn density_transition_updates_geometry_continuously_and_stops_after_settling() {
    let mut app = App::new_for_test();
    let started = Instant::now();
    app.update_context.now = started;
    let _task = app
        .apply_settings_mutation_runtime(&settings::SettingsMutation::Density(UiDensity::Compact));
    assert_eq!(app.state.tokens.spacing.gap, 24.0);
    assert!(app.state.density_motion.needs_ticks());
    let _task = app.update(RootMessage::AnimationTick(
        started + Duration::from_millis(90),
    ));
    let halfway = app.state.tokens.spacing.gap;
    assert!(halfway > 16.0 && halfway < 24.0);
    assert_eq!(app.state.tokens.typography.body, 14.0);
    assert_eq!(
        app.state.features.my_workshop.grid().metrics_for_test(),
        app.state.tokens.layout()
    );
    let _task = app.update(RootMessage::AnimationTick(
        started + Duration::from_millis(200),
    ));
    assert_eq!(app.state.tokens.spacing.gap, 16.0);
    assert!(!app.state.density_motion.needs_ticks());
    assert!(!app.state.tokens.motion.layout_animating);
    let settled = app.state.tokens;
    let _task = app.update(RootMessage::AnimationTick(started + Duration::from_secs(1)));
    assert_eq!(app.state.tokens, settled);
}

#[test]
fn density_reversal_is_continuous_and_reduced_motion_snaps_to_the_target() {
    let mut app = App::new_for_test();
    let started = Instant::now();
    app.state
        .transition_density(UiDensity::Compact, started, true);
    assert!(
        app.state
            .tick_density_motion(started + Duration::from_millis(60))
    );
    let before = app.state.tokens.spacing.gap;
    app.state.transition_density(
        UiDensity::Comfortable,
        started + Duration::from_millis(60),
        true,
    );
    app.state.resolve_ui_tokens();
    assert!((before - app.state.tokens.spacing.gap).abs() < 0.01);
    assert!(
        app.state
            .tick_density_motion(started + Duration::from_millis(90))
    );
    assert!(app.state.tokens.spacing.gap > before);
    app.update_context.now = started + Duration::from_millis(90);
    let _task =
        app.apply_settings_mutation_runtime(&settings::SettingsMutation::ReduceMotion(true));
    assert_eq!(app.state.tokens.spacing.gap, 24.0);
    assert!(!app.state.density_motion.needs_ticks());
    let _task = app
        .apply_settings_mutation_runtime(&settings::SettingsMutation::Density(UiDensity::Compact));
    assert_eq!(app.state.tokens.spacing.gap, 16.0);
    assert!(!app.state.density_motion.needs_ticks());
    assert!(!app.state.tokens.motion.enabled());
}

fn open_settings(app: &mut App) -> settings::SettingsSnapshot {
    let (settings, paths) = app.environment.ctx.settings_and_paths_snapshot();
    let snapshot = settings::SettingsSnapshot::new(settings, paths, app.state.system_scheme);
    let _task =
        app.apply_settings_message(settings::Message::OpenRequested(Box::new(snapshot.clone())));
    snapshot
}

#[test]
fn incoming_snapshots_preserve_active_and_queued_non_display_preferences() {
    let mut app = App::new_for_test();
    let original = open_settings(&mut app);
    let _task = app.apply_settings_message(settings::Message::SoundsToggled(
        !original.settings.backend.sounds,
    ));
    let _task = app.apply_settings_message(settings::Message::ThemeSelected(ThemePreset::Light));
    let _task = app.apply_settings_message(settings::Message::LanguageSelected(Some("fr".into())));
    let _task = app.apply_settings_snapshot_runtime(&original);
    assert_eq!(app.state.theme_preset, ThemePreset::Light);
    assert_eq!(app.state.i18n.locale_id(), "fr");
    let generation = app.settings_save_in_flight.as_ref().unwrap().0;
    let _task = app.update(RootMessage::Settings(settings::Message::SaveCompleted(
        generation,
        Ok(Box::new(original.clone())),
    )));
    let _task = app.apply_settings_snapshot_runtime(&original);
    assert_eq!(app.state.theme_preset, ThemePreset::Light);
    assert_eq!(app.state.i18n.locale_id(), "fr");
}

#[test]
fn delayed_backend_completion_uses_current_published_settings_after_a_save() {
    let mut app = App::new_for_test();
    let original = open_settings(&mut app);
    let _task = app.apply_settings_message(settings::Message::ThemeSelected(ThemePreset::Light));
    let (generation, mutation) = app.settings_save_in_flight.clone().unwrap();
    app.environment
        .ctx
        .update_settings_snapshot_for_test(|settings| {
            settings::apply_settings_mutation(settings, mutation);
        })
        .unwrap();
    let latest = app.settings_snapshot();
    let _task = app.update(RootMessage::Settings(settings::Message::SaveCompleted(
        generation,
        Ok(latest),
    )));
    assert!(app.settings_save_in_flight.is_none());
    assert!(app.settings_save_queue.is_empty());
    let _task = app.update(RootMessage::AppDataSnapshotApplied(Ok(Box::new(original))));
    assert_eq!(app.state.theme_preset, ThemePreset::Light);
    assert_eq!(app.state.tokens.variant, theme::ThemeVariant::Light);
}

#[test]
fn live_display_preferences_survive_incoming_snapshots_until_saved() {
    let mut app = App::new_for_test();
    let original = open_settings(&mut app);
    let _task = app.apply_settings_message(settings::Message::TextSizeChanged(150));
    let _task = app.apply_settings_snapshot_runtime(&original);
    assert_eq!(app.state.text_size_percent, 150);
    assert_eq!(app.state.tokens.typography.body, 21.0);
    let _task = app.apply_settings_message(settings::Message::TextSizeCommitted);
    let _task = app.apply_settings_snapshot_runtime(&original);
    assert_eq!(app.state.text_size_percent, 150);
    assert!(
        app.state
            .features
            .settings
            .display_preferences_override()
            .is_some()
    );
}

#[test]
fn matching_an_older_save_does_not_acknowledge_a_newer_display_edit() {
    let mut app = App::new_for_test();
    let original = open_settings(&mut app);
    for percent in [125, 150, 125] {
        let _task = app.apply_settings_message(settings::Message::TextSizeChanged(percent));
        let _task = app.apply_settings_message(settings::Message::TextSizeCommitted);
    }
    let first = app.settings_save_in_flight.as_ref().unwrap().0;
    let mut old = original.clone();
    old.settings.ui.text_size_percent = 125;
    let _task = app.update(RootMessage::Settings(settings::Message::SaveCompleted(
        first,
        Ok(Box::new(old)),
    )));
    assert!(
        app.state
            .features
            .settings
            .display_preferences_override()
            .is_some()
    );
    let mut intermediate = original;
    intermediate.settings.ui.text_size_percent = 150;
    let _task = app.apply_settings_snapshot_runtime(&intermediate);
    assert_eq!(app.state.text_size_percent, 125);
    assert_eq!(
        app.state.features.settings.settings().ui.text_size_percent,
        125
    );
}

#[test]
fn settings_saves_are_serialized_in_the_order_they_were_requested() {
    let mut app = App::new_for_test();
    let first = Generation::from_raw(12);
    let second = Generation::from_raw(13);
    let _task = app.enqueue_settings_save(first, settings::SettingsMutation::TextSize(125));
    let _task = app.enqueue_settings_save(second, settings::SettingsMutation::TextSize(150));
    assert_eq!(
        app.settings_save_in_flight
            .as_ref()
            .map(|(generation, _)| *generation),
        Some(first)
    );
    assert_eq!(app.settings_save_queue.len(), 1);
    let _task = app.update(RootMessage::Settings(settings::Message::SaveCompleted(
        first,
        Err(UiError::detailed(
            gmpublished_backend::error_keys::IO_ERROR,
            None,
        )),
    )));
    assert_eq!(
        app.settings_save_in_flight
            .as_ref()
            .map(|(generation, _)| *generation),
        Some(second)
    );
    assert!(app.settings_save_queue.is_empty());
}

#[test]
fn changing_theme_and_density_preserves_text_size_and_editor_content() {
    let mut app = App::new_for_test();
    let _effects = description_editor::update_at(
        &mut app.state.features.description_editor,
        description_editor::Message::OpenDraftRequested {
            title: "A local draft".into(),
            initial: "Keep this text while the UI reflows.".into(),
        },
        Instant::now(),
    );
    let _task = app.apply_settings_mutation_runtime(&settings::SettingsMutation::TextSize(150));
    let _task = app
        .apply_settings_mutation_runtime(&settings::SettingsMutation::Density(UiDensity::Compact));
    let _task = app.apply_settings_mutation_runtime(&settings::SettingsMutation::Theme {
        preset: ThemePreset::Light,
        neutral: 0x336699,
        success: 0x339966,
        error: 0x993333,
    });
    let _task = app.update(RootMessage::AnimationTick(
        Instant::now() + Duration::from_secs(1),
    ));
    assert_eq!(app.state.tokens.typography.body, 21.0);
    assert_eq!(app.state.tokens.spacing.gap, 16.0);
    assert_eq!(
        app.state
            .features
            .description_editor
            .source()
            .unwrap()
            .text(),
        "Keep this text while the UI reflows."
    );
}

#[test]
#[ignore = "opt-in real UI screenshots"]
fn capture_app_shell_display_fixtures() {
    use crate::bridge::tasks::{TaskId, WorkshopDownloadTaskKind};
    for (density, percent, size, name) in crate::test_support::capture_display_cases() {
        let mut app = App::new_for_test();
        app.state.viewport_size = size;
        let _task =
            app.apply_settings_mutation_runtime(&settings::SettingsMutation::Density(density));
        let _task =
            app.apply_settings_mutation_runtime(&settings::SettingsMutation::TextSize(percent));
        let _task = app.update(RootMessage::AnimationTick(
            Instant::now() + Duration::from_secs(1),
        ));
        let _task = app.apply_shell_message(
            shell::Message::Navigate(shell::Route::Downloader),
            app.update_context,
        );
        for index in 1..=8 {
            let _task = app.apply_downloader_message(
                downloader::Message::EventReceived(downloader::DownloaderEvent::TaskStarted {
                    kind: WorkshopDownloadTaskKind::Download,
                    item_id: PublishedFileId::fixture(index),
                    task_id: TaskId::from_raw(index),
                }),
                app.update_context,
            );
        }
        crate::test_support::capture_ui_surface(&format!("app-shell-{name}"), size, app.view());
    }
    for (theme, density, percent, size, scale, name) in [
        (
            ThemePreset::Light,
            UiDensity::Comfortable,
            90,
            Size::new(3440.0, 1440.0),
            1.25,
            "light-ultrawide-90-125scale",
        ),
        (
            ThemePreset::ClassicSource,
            UiDensity::Compact,
            125,
            Size::new(1920.0, 600.0),
            1.5,
            "classic-shallow-125-150scale",
        ),
        (
            ThemePreset::Dark,
            UiDensity::Comfortable,
            150,
            Size::new(800.0, 600.0),
            1.0,
            "dark-minimum-150-100scale",
        ),
    ] {
        let mut app = App::new_for_test();
        app.state.viewport_size = size;
        let (mut preferences, paths) = app.environment.ctx.settings_and_paths_snapshot();
        preferences.ui.theme_preset = theme;
        preferences.ui.density = density;
        preferences.ui.text_size_percent = percent;
        let snapshot = settings::SettingsSnapshot::new(preferences, paths, app.state.system_scheme);
        let _task = app.apply_settings_snapshot_runtime(&snapshot);
        let _task = app.update(RootMessage::AnimationTick(
            Instant::now() + Duration::from_secs(1),
        ));
        let _task =
            app.apply_settings_message(settings::Message::OpenRequested(Box::new(snapshot)));
        app.state.features.modal_stack = modal_stack::State::default();
        let _effects = modal_stack::update_at(
            &mut app.state.features.modal_stack,
            &modal_stack::Message::OpenSettings,
            Instant::now() - Duration::from_secs(2),
        );
        let _closed = app.state.features.modal_stack.tick(Instant::now());
        assert!(app.state.features.settings.open());
        assert_eq!(
            app.state.features.modal_stack.active(),
            Some(modal_stack::ActiveModal::Settings)
        );
        assert_eq!(app.state.features.modal_stack.scale(Instant::now()), 1.0);
        crate::test_support::capture_ui_surface_at_scale(
            &format!("app-settings-{name}"),
            size,
            scale,
            if theme == ThemePreset::Light {
                &iced::Theme::Light
            } else {
                &iced::Theme::Dark
            },
            app.view(),
        );
    }
}

#[test]
#[ignore = "opt-in real UI motion frames"]
fn capture_density_motion_fixtures() {
    let mut app = App::new_for_test();
    let size = Size::new(1280.0, 800.0);
    app.state.viewport_size = size;
    let original = open_settings(&mut app);
    let _task =
        app.apply_settings_message(settings::Message::TabSelected(settings::Tab::Accessibility));
    app.state.features.modal_stack = modal_stack::State::default();
    let _effects = modal_stack::update_at(
        &mut app.state.features.modal_stack,
        &modal_stack::Message::OpenSettings,
        Instant::now() - Duration::from_secs(2),
    );
    let _closed = app.state.features.modal_stack.tick(Instant::now());
    let started = Instant::now();
    app.update_context.now = started;
    let _task = app.apply_settings_message(settings::Message::DensitySelected(UiDensity::Compact));
    for elapsed in [0, 30, 60, 90, 120, 150, 180, 210] {
        let _task = app.update(RootMessage::AnimationTick(
            started + Duration::from_millis(elapsed),
        ));
        crate::test_support::capture_ui_surface_at_scale(
            &format!("density-to-compact-{elapsed:03}"),
            size,
            1.0,
            &iced::Theme::Dark,
            app.view(),
        );
    }
    let compact = app.state.tokens;
    assert_eq!(compact.spacing.gap, 16.0);
    assert_eq!(compact.typography.body, 14.0);
    assert!(!app.state.density_motion.needs_ticks());
    let _task = app.apply_settings_snapshot_runtime(&original);
    assert_eq!(app.state.density, UiDensity::Compact);
    let started = Instant::now();
    app.update_context.now = started;
    let _task =
        app.apply_settings_message(settings::Message::DensitySelected(UiDensity::Comfortable));
    for elapsed in [0, 30, 60, 90, 120, 150, 180, 210] {
        let _task = app.update(RootMessage::AnimationTick(
            started + Duration::from_millis(elapsed),
        ));
        crate::test_support::capture_ui_surface_at_scale(
            &format!("density-to-comfortable-{elapsed:03}"),
            size,
            1.0,
            &iced::Theme::Dark,
            app.view(),
        );
    }
    assert_eq!(app.state.tokens.spacing.gap, 24.0);
}
