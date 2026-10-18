use super::*;
use gmpublished_backend::model_compile::{BodygroupChoice, SourcePart};

fn ready() -> State {
    let mut state = State {
        route_active: false,
        ..State::default()
    };
    super::super::update(&mut state, Message::SourceDropped("prop.glb".into()));
    state.destination = Some("export".into());
    let generation = state.inspection_generation.unwrap();
    super::super::update(
        &mut state,
        Message::Inspected(generation, Ok(Box::new(inspection(false)))),
    );
    state
}

fn inspection(tagged: bool) -> ModelInspection {
    ModelInspection {
        source_model: None,
        parts: ["visible", "physics"]
            .into_iter()
            .enumerate()
            .map(|(index, name)| SourcePart {
                selector: format!("@node:{index}"),
                name: name.into(),
            })
            .collect(),
        collision_physics: None,
        collision_parts: if tagged {
            vec!["@node:1".into()]
        } else {
            Vec::new()
        },
        animations: Vec::new(),
        materials: Vec::new(),
        material_directories: Vec::new(),
        skins: Vec::new(),
    }
}

#[test]
fn source_authoring_preserves_model_type_and_uses_authored_physics() {
    let mut state = State {
        route_active: false,
        ..State::default()
    };
    super::super::update(&mut state, Message::SourceDropped("edited.glb".into()));
    state.destination = Some("export".into());
    let generation = state.inspection_generation.unwrap();
    let mut source = inspection(true);
    source.source_model = Some(gmpublished_backend::model_compile::ImportedModelSettings {
        model_name: "models/props/chair.mdl".into(),
        static_prop: true,
    });
    super::super::update(
        &mut state,
        Message::Inspected(generation, Ok(Box::new(source))),
    );
    for request in [state.request(), state.preview_request(), state.qc_request()] {
        let request = request.unwrap();
        assert_eq!(
            request.model_name.as_deref(),
            Some("models/props/chair.mdl")
        );
        assert!(request.static_prop);
        assert_eq!(request.collision, ModelCollision::Existing);
    }
    super::super::update(
        &mut state,
        Message::CollisionModeChanged(CollisionMode::Automatic),
    );
    assert!(matches!(
        state.request().unwrap().collision,
        ModelCollision::Automatic(_)
    ));
    super::super::update(&mut state, Message::SourceDropped("edited.glb".into()));
    let generation = state.inspection_generation.unwrap();
    super::super::update(&mut state, Message::StaticPropChanged(false));
    super::super::update(&mut state, Message::ModelNameChanged("custom.mdl".into()));
    let mut source = inspection(true);
    source.source_model = Some(gmpublished_backend::model_compile::ImportedModelSettings {
        model_name: "models/props/chair.mdl".into(),
        static_prop: true,
    });
    super::super::update(
        &mut state,
        Message::Inspected(generation, Ok(Box::new(source))),
    );
    assert!(!state.static_prop);
    assert_eq!(state.model_name, "custom.mdl");
}

#[test]
fn imported_collision_is_preserved_before_any_build_can_use_the_automatic_default() {
    let mut state = State {
        route_active: false,
        ..State::default()
    };
    super::super::update(&mut state, Message::SourceDropped("prop.glb".into()));
    state.destination = Some("export".into());
    assert!(!state.can_export());
    assert!(state.preview_request().is_none());
    let generation = state.inspection_generation.unwrap();
    super::super::update(
        &mut state,
        Message::Inspected(generation, Ok(Box::new(inspection(true)))),
    );
    for request in [state.request(), state.preview_request(), state.qc_request()] {
        assert!(
            matches!(request.unwrap().collision, ModelCollision::Custom {ref source,ref parts,..} if source == &PathBuf::from("prop.glb") && parts == &["@node:1"])
        );
    }
    super::super::update(
        &mut state,
        Message::CollisionModeChanged(CollisionMode::Existing),
    );
    assert!(
        matches!(state.request().unwrap().collision, ModelCollision::Custom {ref parts,repair:None,..} if parts == &["@node:1"])
    );
    super::super::update(
        &mut state,
        Message::CollisionModeChanged(CollisionMode::Automatic),
    );
    super::super::update(
        &mut state,
        Message::CollisionDetailChanged(CollisionDetail::Detailed),
    );
    assert!(matches!(
        state.request().unwrap().collision,
        ModelCollision::Automatic(CollisionDetail::Detailed)
    ));
}

#[test]
fn custom_parts_are_shared_by_all_builds_and_cannot_also_belong_to_bodygroups() {
    let mut state = ready();
    super::super::update(&mut state, Message::CollisionUseSource);
    assert!(!state.can_export());
    super::super::update(
        &mut state,
        Message::CollisionPartToggled("@node:1".into(), true),
    );
    for request in [state.request(), state.preview_request(), state.qc_request()] {
        assert!(
            matches!(request.unwrap().collision, ModelCollision::Custom {ref parts,..} if parts == &["@node:1"])
        );
    }
    super::super::update(&mut state, Message::BodygroupAdded);
    assert!(
        matches!(&state.bodygroups[0].choices[0], BodygroupChoice::Mesh {parts,..} if parts == &["@node:0"])
    );
    super::super::update(
        &mut state,
        Message::BodygroupPartToggled(0, 0, "@node:1".into(), true),
    );
    assert!(
        matches!(&state.bodygroups[0].choices[0], BodygroupChoice::Mesh {parts,..} if parts == &["@node:0"])
    );
    super::super::update(
        &mut state,
        Message::CollisionPartToggled("@node:0".into(), true),
    );
    assert_eq!(state.collision.parts, ["@node:1"]);
    state.bodygroups.clear();
    super::super::update(
        &mut state,
        Message::CollisionPartToggled("@node:0".into(), true),
    );
    assert!(!state.can_export());
    assert!(state.preview_request().is_none());
    assert!(state.qc_request().is_none());
}

#[test]
fn collision_file_inspection_is_gated_and_stale_replies_do_not_replace_a_new_model() {
    let mut state = ready();
    super::super::update(
        &mut state,
        Message::CollisionModeChanged(CollisionMode::Custom),
    );
    let effects = super::super::update(&mut state, Message::CollisionSourceRequested);
    let [Effect::PickCollisionSource(generation, _)] = effects.as_slice() else {
        panic!("collision picker");
    };
    let picker = *generation;
    super::super::update(
        &mut state,
        Message::CollisionModeChanged(CollisionMode::None),
    );
    assert_eq!(state.collision.mode, CollisionMode::Custom);
    let effects = super::super::update(
        &mut state,
        Message::CollisionSourceSelected(picker, Some("physics.fbx".into())),
    );
    let [Effect::InspectCollision(generation, _)] = effects.as_slice() else {
        panic!("collision inspection");
    };
    let inspecting = *generation;
    assert!(state.request().is_none());
    assert!(state.preview_request().is_none());
    super::super::update(&mut state, Message::SourceDropped("new.smd".into()));
    super::super::update(
        &mut state,
        Message::CollisionSourceInspected(inspecting, Ok(Box::new(inspection(true)))),
    );
    assert_eq!(state.collision.mode, CollisionMode::Automatic);
    assert!(state.collision.source.is_none());
    assert!(matches!(
        state.request().unwrap().collision,
        ModelCollision::Automatic(CollisionDetail::Balanced)
    ));
}

#[test]
fn custom_file_errors_block_builds_and_successful_inspection_selects_its_parts() {
    let mut state = ready();
    for error in [true, false] {
        super::super::update(&mut state, Message::CollisionSourceRequested);
        let picker = state.picker.unwrap();
        super::super::update(
            &mut state,
            Message::CollisionSourceSelected(picker, Some("physics.fbx".into())),
        );
        let generation = state.collision.inspection_generation.unwrap();
        super::super::update(
            &mut state,
            Message::CollisionSourceInspected(
                generation,
                if error {
                    Err("invalid collision".into())
                } else {
                    Ok(Box::new(inspection(false)))
                },
            ),
        );
        if error {
            assert!(state.request().is_none());
            assert!(state.preview_request().is_none());
            assert!(state.qc_request().is_none());
        } else {
            assert!(
                matches!(state.request().unwrap().collision,ModelCollision::Custom {ref source,ref parts,..} if source == &PathBuf::from("physics.fbx") && parts == &["@node:0","@node:1"])
            );
        }
    }
}

#[test]
fn single_mesh_collision_files_do_not_require_object_selectors() {
    let mut state = ready();
    super::super::update(&mut state, Message::CollisionSourceRequested);
    let picker = state.picker.unwrap();
    super::super::update(
        &mut state,
        Message::CollisionSourceSelected(picker, Some("physics.smd".into())),
    );
    let generation = state.collision.inspection_generation.unwrap();
    let mut inspected = inspection(false);
    inspected.parts.clear();
    super::super::update(
        &mut state,
        Message::CollisionSourceInspected(generation, Ok(Box::new(inspected))),
    );
    for request in [state.request(), state.preview_request(), state.qc_request()] {
        assert!(
            matches!(request.unwrap().collision, ModelCollision::Custom {ref source,ref parts,..} if source == &PathBuf::from("physics.smd") && parts.is_empty())
        );
    }
}

#[test]
fn custom_repair_is_explicit_uses_selected_geometry_and_resets_for_new_sources() {
    let mut state = ready();
    super::super::update(&mut state, Message::CollisionUseSource);
    super::super::update(
        &mut state,
        Message::CollisionPartToggled("@node:1".into(), true),
    );
    assert!(matches!(
        state.request().unwrap().collision,
        ModelCollision::Custom { repair: None, .. }
    ));
    super::super::update(&mut state, Message::CollisionRepairChanged(true));
    super::super::update(
        &mut state,
        Message::CollisionDetailChanged(CollisionDetail::Detailed),
    );
    for request in [state.request(), state.preview_request(), state.qc_request()] {
        assert!(
            matches!(request.unwrap().collision, ModelCollision::Custom {ref source,ref parts,repair:Some(CollisionDetail::Detailed)} if source == &PathBuf::from("prop.glb") && parts == &["@node:1"])
        );
    }
    assert!(state.is_collision_part("@node:1"));
    assert!(!state.is_collision_part("@node:0"));
    super::super::update(&mut state, Message::CollisionSourceRequested);
    let picker = state.picker.unwrap();
    super::super::update(
        &mut state,
        Message::CollisionSourceSelected(picker, Some("replacement.fbx".into())),
    );
    assert!(!state.collision.repair_custom);
    let generation = state.collision.inspection_generation.unwrap();
    super::super::update(
        &mut state,
        Message::CollisionSourceInspected(generation, Ok(Box::new(inspection(false)))),
    );
    assert!(matches!(
        state.request().unwrap().collision,
        ModelCollision::Custom { repair: None, .. }
    ));
    super::super::update(&mut state, Message::CollisionRepairChanged(true));
    super::super::update(&mut state, Message::SourceDropped("other.glb".into()));
    assert!(!state.collision.repair_custom);
}

#[test]
fn custom_collision_actions_fit_and_are_clickable_in_every_locale() {
    let mut state = ready();
    super::super::update(
        &mut state,
        Message::CollisionModeChanged(CollisionMode::Custom),
    );
    let tokens = crate::theme::Tokens::dark();
    for locale in [
        "en", "de", "es", "fr", "kr", "nl", "pl", "pt-BR", "ru", "tr", "uk", "zh-cn",
    ] {
        let i18n = crate::i18n::I18n::for_locale(Some(locale));
        let mut ui = iced_test::Simulator::with_size(
            iced::Settings::default(),
            iced::Size::new(500.0, 1200.0),
            view(&state, ViewCtx::new(&tokens, &i18n)),
        );
        for key in [
            "models-collision-custom-file",
            "models-collision-use-source",
            "models-collision-repair",
        ] {
            let bounds = ui.find(i18n.tr(key).as_str()).unwrap().bounds();
            assert!(
                bounds.x >= 0.0 && bounds.x + bounds.width <= 500.0,
                "{locale}: {key}: {bounds:?}"
            );
            ui.click(i18n.tr(key).as_str()).unwrap();
        }
        assert!(matches!(
            ui.into_messages().collect::<Vec<_>>().as_slice(),
            [
                Message::CollisionSourceRequested,
                Message::CollisionUseSource,
                Message::CollisionRepairChanged(true)
            ]
        ));
    }
}

#[test]
fn authored_physics_only_allows_preserving_or_removing_collision() {
    let mut state = ready();
    super::super::update(&mut state, Message::SourceDropped("authored.toml".into()));
    for mode in [CollisionMode::Automatic, CollisionMode::Custom] {
        super::super::update(&mut state, Message::CollisionModeChanged(mode));
        assert!(matches!(
            state.request().unwrap().collision,
            ModelCollision::Existing
        ));
    }
    super::super::update(
        &mut state,
        Message::CollisionModeChanged(CollisionMode::None),
    );
    for request in [state.request(), state.preview_request(), state.qc_request()] {
        assert!(matches!(request.unwrap().collision, ModelCollision::None));
    }
    super::super::update(
        &mut state,
        Message::CollisionModeChanged(CollisionMode::Existing),
    );
    assert!(matches!(
        state.request().unwrap().collision,
        ModelCollision::Existing
    ));
}

#[test]
fn imported_physics_preserves_mass_and_surface_without_overwriting_early_edits() {
    use gmpublished_backend::model_compile::CollisionPhysics;
    let mut state = ready();
    for (mass_edit, surface_edit) in [(None, None), (Some("42"), None), (None, Some("wood"))] {
        super::super::update(&mut state, Message::SourceDropped("imported.glb".into()));
        if let Some(mass) = mass_edit {
            super::super::update(&mut state, Message::CollisionMassChanged(mass.into()));
        }
        if let Some(surface) = surface_edit {
            super::super::update(&mut state, Message::SurfacePropertyChanged(surface.into()));
        }
        let generation = state.inspection_generation.unwrap();
        let mut inspected = inspection(true);
        inspected.collision_physics = Some(CollisionPhysics {
            mass: 25.0,
            surface_property: Some("metal".into()),
        });
        super::super::update(
            &mut state,
            Message::Inspected(generation, Ok(Box::new(inspected))),
        );
        super::super::update(
            &mut state,
            Message::CollisionModeChanged(CollisionMode::Existing),
        );
        for request in [state.request(), state.preview_request(), state.qc_request()] {
            let request = request.unwrap();
            assert_eq!(
                request.collision_mass,
                if mass_edit.is_some() { 42.0 } else { 25.0 }
            );
            assert_eq!(
                request.surface_property.as_deref(),
                Some(surface_edit.unwrap_or("metal"))
            );
            assert!(matches!(
                request.collision,
                ModelCollision::Custom { repair: None, .. }
            ));
        }
    }
}
