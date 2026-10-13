use iced::{Settings, Size};

use super::super::{Message, State, update};
use super::view;
use crate::i18n::I18n;
use crate::theme::{Tokens, ViewCtx};

#[test]
fn empty_manager_import_is_clickable_without_a_source() {
    let state = State::default();
    let tokens = Tokens::dark();
    let i18n = I18n::for_locale(Some("en"));
    let mut ui = iced_test::Simulator::with_size(
        Settings::default(),
        Size::new(560.0, 560.0),
        view(&state, ViewCtx::new(&tokens, &i18n)),
    );
    ui.click(i18n.tr("models-import").as_str()).unwrap();
    assert!(
        ui.into_messages()
            .any(|message| matches!(message, Message::ImportRequested))
    );
}

#[test]
fn busy_cancel_stays_within_small_route_viewports_in_every_locale() {
    let mut state = State::default();
    update(&mut state, Message::SourceDropped("prop.mdl".into()));
    state.destination = Some("export".into());
    update(&mut state, Message::ExportRequested);
    let tokens = Tokens::dark();
    for locale in [
        "en", "de", "es", "fr", "kr", "nl", "pl", "pt-BR", "ru", "tr", "uk", "zh-cn",
    ] {
        let i18n = I18n::for_locale(Some(locale));
        let mut ui = iced_test::Simulator::with_size(
            Settings::default(),
            Size::new(560.0, 560.0),
            view(&state, ViewCtx::new(&tokens, &i18n)),
        );
        let cancel = ui.find(i18n.tr("cancel").as_str()).unwrap();
        assert!(
            cancel.bounds().x >= 0.0
                && cancel.bounds().x + cancel.bounds().width <= 560.0
                && cancel.bounds().y >= 0.0
                && cancel.bounds().y + cancel.bounds().height <= 560.0,
            "cancel exceeds route viewport in {locale}: {:?}",
            cancel.bounds()
        );
    }
}

#[test]
fn compile_stays_visible_with_mesh_options_in_a_small_viewport() {
    let mut state = State::default();
    update(&mut state, Message::SourceDropped("prop.fbx".into()));
    state.destination = Some("export".into());
    let tokens = Tokens::dark();
    let i18n = I18n::for_locale(Some("en"));
    let mut ui = iced_test::Simulator::with_size(
        Settings::default(),
        Size::new(560.0, 400.0),
        view(&state, ViewCtx::new(&tokens, &i18n)),
    );
    let compile = ui.find(i18n.tr("models-compile").as_str()).unwrap();
    assert!(compile.bounds().y >= 0.0);
    assert!(compile.bounds().y + compile.bounds().height <= 400.0);
    ui.click(i18n.tr("models-compile").as_str()).unwrap();
    assert!(
        ui.into_messages()
            .any(|message| matches!(message, Message::ExportRequested))
    );
}

#[test]
fn compiled_models_offer_distinct_blender_and_qc_exports_in_every_locale() {
    let mut state = State::default();
    update(&mut state, Message::SourceDropped("prop.mdl".into()));
    state.destination = Some("export".into());
    let tokens = Tokens::dark();
    for locale in [
        "en", "de", "es", "fr", "kr", "nl", "pl", "pt-BR", "ru", "tr", "uk", "zh-cn",
    ] {
        let i18n = I18n::for_locale(Some(locale));
        let mut ui = iced_test::Simulator::with_size(
            Settings::default(),
            Size::new(560.0, 560.0),
            view(&state, ViewCtx::new(&tokens, &i18n)),
        );
        for key in ["models-export-blender", "models-decompile"] {
            let label = i18n.tr(key);
            let action = ui.find(label.as_str()).unwrap();
            assert!(
                action.bounds().x >= 0.0
                    && action.bounds().x + action.bounds().width <= 560.0
                    && action.bounds().y >= 0.0
                    && action.bounds().y + action.bounds().height <= 560.0,
                "{key} exceeds viewport in {locale}"
            );
            ui.click(label.as_str()).unwrap();
        }
        let messages: Vec<_> = ui.into_messages().collect();
        assert!(matches!(
            messages.as_slice(),
            [Message::BlenderExportRequested, Message::ExportRequested]
        ));
    }
}

#[test]
fn model_editor_actions_fit_in_every_locale() {
    let mut state = State::default();
    update(&mut state, Message::SourceDropped("Mercedes.fbx".into()));
    state.destination = Some("export".into());
    update(&mut state, Message::GenerateCollisionChanged(true));
    update(&mut state, Message::CollisionMassChanged("1250.5".into()));
    update(&mut state, Message::SurfacePropertyChanged("metal".into()));
    state.inspection = Some(gmpublished_backend::model_compile::ModelInspection {
        parts: vec![],
        animations: vec![],
        materials: vec!["paint".into(), "glass".into()],
        material_directories: vec![],
        skins: vec![vec!["paint".into(), "glass".into()]],
    });
    update(&mut state, Message::SkinAdded("Black".into()));
    update(&mut state, Message::MaterialSelected("paint".into()));
    update(
        &mut state,
        Message::MapsLoaded(crate::generation::Generation::INITIAL, vec![]),
    );
    let tokens = Tokens::dark();
    for locale in [
        "en", "de", "es", "fr", "kr", "nl", "pl", "pt-BR", "ru", "tr", "uk", "zh-cn",
    ] {
        let i18n = I18n::for_locale(Some(locale));
        let mut ui = iced_test::Simulator::with_size(
            Settings::default(),
            Size::new(560.0, 560.0),
            view(&state, ViewCtx::new(&tokens, &i18n)),
        );
        for key in ["models-compile", "models-preview", "models-save-qc"] {
            let widget = ui.find(i18n.tr(key).as_str()).unwrap();
            let bounds = widget.bounds();
            assert!(
                bounds.x >= 0.0
                    && bounds.x + bounds.width <= 560.0
                    && bounds.y >= 0.0
                    && bounds.y + bounds.height <= 560.0,
                "{locale}: {key} overflows: {bounds:?}"
            );
        }
        ui.click(i18n.tr("models-save-qc").as_str()).unwrap();
        assert!(
            ui.into_messages()
                .any(|message| matches!(message, Message::SaveQcRequested))
        );
    }
}

#[test]
fn decompiled_source_opens_directly_for_editing_and_recompilation() {
    let mut state = State::default();
    update(&mut state, Message::SourceDropped("model.mdl".into()));
    state.destination = Some("export".into());
    state.result = Some(super::super::ExportResult {
        directory: "export/model-source".into(),
        files: vec!["export/model-source/model.qc".into()],
        bytes: 1024,
        warnings: vec![],
        compiled: false,
    });
    let tokens = Tokens::dark();
    let i18n = I18n::for_locale(Some("en"));
    let mut ui = iced_test::Simulator::with_size(
        Settings::default(),
        Size::new(900.0, 1200.0),
        view(&state, ViewCtx::new(&tokens, &i18n)),
    );
    ui.click(i18n.tr("models-edit-exported").as_str()).unwrap();
    let message = ui.into_messages().next().unwrap();
    assert!(
        matches!(&message, Message::SourceDropped(path) if path == std::path::Path::new("export/model-source/model.qc"))
    );
    update(&mut state, message);
    assert_eq!(state.kind(), Some(super::super::SourceKind::Description));
    assert!(!state.can_export());
    let generation = state.qc_editor.load_generation.unwrap();
    update(
        &mut state,
        Message::QcLoaded(generation, Ok("$body body mesh.smd\n".into())),
    );
    assert!(state.can_export());
    assert_eq!(state.destination, Some("export".into()));
}

#[test]
fn animation_controls_are_localized_clickable_and_gated_while_busy() {
    let mut state = State::default();
    update(&mut state, Message::SourceDropped("character.glb".into()));
    state.destination = Some("export".into());
    let animation = gmpublished_backend::model_compile::AnimationSettings {
        source: Some("Walk Cycle".into()),
        included: true,
        name: "walk".into(),
        fps: 30.0,
        looping: false,
        frame_count: 31,
        activity: None,
        activity_weight: 0,
        events: vec![],
    };
    state.animations = vec![super::super::state::AnimationEditor::from(&animation)];
    let tokens = Tokens::dark();
    for locale in [
        "en", "de", "es", "fr", "kr", "nl", "pl", "pt-BR", "ru", "tr", "uk", "zh-cn",
    ] {
        let i18n = I18n::for_locale(Some(locale));
        let mut ui = iced_test::Simulator::with_size(
            Settings::default(),
            Size::new(560.0, 560.0),
            super::animations_card(&state, ViewCtx::new(&tokens, &i18n)),
        );
        for key in [
            "models-animations",
            "models-animation-name",
            "models-animation-fps",
            "models-animation-loop",
        ] {
            let label = i18n.tr(key);
            assert_ne!(label, key);
            let widget = ui.find(label.as_str()).unwrap();
            let bounds = widget.bounds();
            assert!(
                bounds.x >= 0.0 && bounds.x + bounds.width <= 560.0,
                "{locale} {key}: {bounds:?}"
            );
        }
        ui.click(i18n.tr("models-animation-include").as_str())
            .unwrap();
        assert!(
            ui.into_messages()
                .any(|message| matches!(message, Message::AnimationIncluded(0, false)))
        );
    }
    update(&mut state, Message::ExportRequested);
    let i18n = I18n::for_locale(Some("en"));
    let mut ui = iced_test::Simulator::with_size(
        Settings::default(),
        Size::new(560.0, 560.0),
        super::animations_card(&state, ViewCtx::new(&tokens, &i18n)),
    );
    ui.click(i18n.tr("models-animation-include").as_str())
        .unwrap();
    assert!(ui.into_messages().next().is_none());
}

#[test]
fn advanced_animation_fields_and_events_are_localized_and_busy_gated() {
    use gmpublished_backend::model_compile::{AnimationEvent, AnimationSettings};
    let mut state = State::default();
    update(&mut state, Message::SourceDropped("character.glb".into()));
    state.destination = Some("export".into());
    let animation = AnimationSettings {
        source: Some("walking".into()),
        included: true,
        name: "walk".into(),
        fps: 30.0,
        looping: false,
        frame_count: 31,
        activity: Some("ACT_WALK".into()),
        activity_weight: 1,
        events: vec![AnimationEvent {
            frame: 15,
            event: "5004".into(),
            options: "step".into(),
        }],
    };
    state.animations = vec![super::super::state::AnimationEditor::from(&animation)];
    update(&mut state, Message::AnimationAdvancedToggled(0));
    let tokens = Tokens::dark();
    for locale in [
        "en", "de", "es", "fr", "kr", "nl", "pl", "pt-BR", "ru", "tr", "uk", "zh-cn",
    ] {
        let i18n = I18n::for_locale(Some(locale));
        let mut ui = iced_test::Simulator::with_size(
            Settings::default(),
            Size::new(600.0, 1000.0),
            super::animations_card(&state, ViewCtx::new(&tokens, &i18n)),
        );
        for key in [
            "models-animation-advanced",
            "models-animation-activity",
            "models-animation-activity-weight",
            "models-animation-events",
            "models-animation-event-frame",
            "models-animation-event-code",
            "models-animation-event-options",
            "models-animation-event-add",
            "models-animation-event-remove",
        ] {
            let label = i18n.tr(key);
            assert_ne!(label, key);
            let bounds = ui.find(label.as_str()).unwrap().bounds();
            assert!(
                bounds.x >= 0.0 && bounds.x + bounds.width <= 600.0,
                "{locale} {key}: {bounds:?}"
            );
        }
        ui.click(i18n.tr("models-animation-event-add").as_str())
            .unwrap();
        assert!(
            ui.into_messages()
                .any(|message| matches!(message, Message::AnimationEventAdded(0)))
        );
    }
    update(&mut state, Message::ExportRequested);
    assert!(state.busy());
    let i18n = I18n::for_locale(Some("en"));
    let mut ui = iced_test::Simulator::with_size(
        Settings::default(),
        Size::new(600.0, 1000.0),
        super::animations_card(&state, ViewCtx::new(&tokens, &i18n)),
    );
    ui.click(i18n.tr("models-animation-event-add").as_str())
        .unwrap();
    ui.click(i18n.tr("models-animation-event-remove").as_str())
        .unwrap();
    ui.click(i18n.tr("models-animation-advanced").as_str())
        .unwrap();
    assert!(ui.into_messages().next().is_none());
}

#[test]
fn saved_mesh_qc_opens_in_the_integrated_editor() {
    let mut state = State::default();
    update(&mut state, Message::SourceDropped("vehicle.fbx".into()));
    state.qc_saved = true;
    state.qc_path = Some("export/vehicle.qc".into());
    let tokens = Tokens::dark();
    let mut message = None;
    for locale in [
        "en", "de", "es", "fr", "kr", "nl", "pl", "pt-BR", "ru", "tr", "uk", "zh-cn",
    ] {
        let i18n = I18n::for_locale(Some(locale));
        let mut ui = iced_test::Simulator::with_size(
            Settings::default(),
            Size::new(560.0, 560.0),
            view(&state, ViewCtx::new(&tokens, &i18n)),
        );
        let control = ui.find(i18n.tr("models-qc-editor").as_str()).unwrap();
        let bounds = control.bounds();
        assert!(
            bounds.x >= 0.0
                && bounds.x + bounds.width <= 560.0
                && bounds.y >= 0.0
                && bounds.y + bounds.height <= 560.0,
            "{locale}: {bounds:?}"
        );
        ui.click(i18n.tr("models-qc-editor").as_str()).unwrap();
        message = ui.into_messages().next();
        assert!(
            matches!(&message, Some(Message::SourceDropped(path)) if path == std::path::Path::new("export/vehicle.qc"))
        );
    }
    let effects = update(&mut state, message.unwrap());
    assert!(state.is_qc());
    assert!(!state.can_export());
    assert!(
        effects
            .iter()
            .any(|effect| matches!(effect, super::super::Effect::LoadQc(_, _)))
    );
}
