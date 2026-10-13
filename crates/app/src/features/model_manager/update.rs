use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use super::state::RunningExport;
use super::{Effect, MapChoice, Message, SourceKind, State};
use gmpublished_backend::model_project::{ModelSkin, ModelStage, SkinMaterial};

pub fn update(state: &mut State, message: Message) -> Vec<Effect> {
    if let Some(effects) = super::qc_editor::update(state, &message) {
        return effects;
    }
    if super::lods::edit(state, &message) {
        clear_outcome(state);
        return vec![];
    }
    match &message {
        Message::BodygroupEditorToggled(group, choice) if !state.busy() => {
            if matches!(
                state
                    .bodygroups
                    .get(*group)
                    .and_then(|group| group.choices.get(*choice)),
                Some(gmpublished_backend::model_compile::BodygroupChoice::Mesh { .. })
            ) {
                let selected = (*group, *choice);
                state.bodygroup_editor =
                    (state.bodygroup_editor != Some(selected)).then_some(selected);
                state.bodygroup_filter.clear();
            }
            return vec![];
        }
        Message::BodygroupFilterChanged(filter) if !state.busy() => {
            state.bodygroup_filter.clone_from(filter);
            return vec![];
        }
        _ => {}
    }
    if super::bodygroups::edit(state, &message) {
        state.stage_bodygroups.clear();
        clear_outcome(state);
        return vec![];
    }
    match message {
        Message::LodSourceRequested
            if !state.busy()
                && state.kind() == Some(SourceKind::Mesh)
                && state.lods.len() < 7
                && state.bodygroups.is_empty() =>
        {
            let generation = state.generation.bump();
            state.picker = Some(generation);
            return vec![Effect::PickLodSource(
                generation,
                state
                    .source
                    .as_ref()
                    .and_then(|source| source.parent())
                    .map(std::path::Path::to_path_buf),
            )];
        }
        Message::RouteExited => {
            state.picker = None;
            state.map_generation = None;
        }
        Message::ImportRequested if !state.busy() => {
            let generation = state.generation.bump();
            state.picker = Some(generation);
            return vec![Effect::PickSource(generation)];
        }
        Message::ImportSelected(generation, path) if state.picker == Some(generation) => {
            state.picker = None;
            if let Some(path) = path {
                return select_source(state, path);
            }
        }
        Message::SourceDropped(path) if !state.busy() => return select_source(state, path),
        Message::DestinationRequested if !state.busy() => {
            let generation = state.generation.bump();
            state.picker = Some(generation);
            return vec![Effect::PickDestination(
                generation,
                state.destination.clone(),
            )];
        }
        Message::DestinationSelected(generation, path) if state.picker == Some(generation) => {
            state.picker = None;
            if let Some(path) = path {
                state.destination = Some(path);
                clear_outcome(state);
            }
        }
        Message::ModelNameChanged(value) if !state.busy() => {
            state.model_name = value;
            clear_outcome(state);
        }
        Message::MaterialDirectoryChanged(value) if !state.busy() => {
            state.material_directory = value;
            clear_outcome(state);
        }
        Message::ScaleChanged(value) if !state.busy() => {
            state.scale = value;
            clear_outcome(state);
        }
        Message::OrientationChanged(axis, value) if !state.busy() && axis < 3 => {
            state.orientation[axis] = value;
            clear_outcome(state);
        }
        Message::AnimationIncluded(index, value) => {
            edit_animation(state, index, |animation| animation.included = value);
        }
        Message::AnimationNameChanged(index, value) => {
            edit_animation(state, index, |animation| animation.name = value);
        }
        Message::AnimationFpsChanged(index, value) => {
            edit_animation(state, index, |animation| animation.fps = value);
        }
        Message::AnimationLoopChanged(index, value) => {
            edit_animation(state, index, |animation| animation.looping = value);
        }
        Message::AnimationAdvancedToggled(index)
            if !state.busy() && state.kind() == Some(SourceKind::Mesh) && !state.static_prop =>
        {
            if let Some(animation) = state.animations.get_mut(index) {
                animation.advanced_expanded = !animation.advanced_expanded;
            }
        }
        Message::AnimationActivityChanged(index, value) => {
            edit_animation(state, index, |animation| animation.activity = value);
        }
        Message::AnimationActivityWeightChanged(index, value) => {
            edit_animation(state, index, |animation| animation.activity_weight = value);
        }
        Message::AnimationEventAdded(index) => {
            edit_animation(state, index, |animation| {
                if animation.events.len() < 1024 {
                    animation.events.push(super::state::AnimationEventEditor {
                        frame: "0".into(),
                        event: "5004".into(),
                        options: String::new(),
                    });
                }
            });
        }
        Message::AnimationEventRemoved(index, event) => {
            edit_animation(state, index, |animation| {
                if event < animation.events.len() {
                    animation.events.remove(event);
                }
            });
        }
        Message::AnimationEventFrameChanged(index, event, value) => {
            edit_animation(state, index, |animation| {
                if let Some(event) = animation.events.get_mut(event) {
                    event.frame = value;
                }
            });
        }
        Message::AnimationEventCodeChanged(index, event, value) => {
            edit_animation(state, index, |animation| {
                if let Some(event) = animation.events.get_mut(event) {
                    event.event = value;
                }
            });
        }
        Message::AnimationEventOptionsChanged(index, event, value) => {
            edit_animation(state, index, |animation| {
                if let Some(event) = animation.events.get_mut(event) {
                    event.options = value;
                }
            });
        }
        Message::Inspected(generation, result)
            if state.inspection_generation == Some(generation) =>
        {
            state.inspection_generation = None;
            state.qc_editor.validated = result.is_ok();
            state.qc_editor.diagnostic = None;
            match result {
                Ok(inspection) => {
                    state.animations = if state.kind() == Some(SourceKind::Mesh) {
                        inspection
                            .animations
                            .iter()
                            .map(super::state::AnimationEditor::from)
                            .collect()
                    } else {
                        Vec::new()
                    };
                    state.selected_material = inspection
                        .skins
                        .first()
                        .and_then(|skin| skin.first())
                        .cloned()
                        .or_else(|| inspection.materials.first().cloned());
                    state.inspection = Some(inspection);
                    sync_material_editor(state);
                }
                Err(error) if state.is_qc() => state.qc_editor.diagnostic = Some(error),
                Err(error) => state.error = Some(error),
            }
        }
        Message::SkinAdded(name) if !state.busy() => {
            state.selected_skin = Some(state.skins.len());
            state.skins.push(ModelSkin {
                name,
                replacements: std::collections::BTreeMap::new(),
            });
            state.stage_skin = state
                .inspection
                .as_ref()
                .map_or(1, |inspection| inspection.skins.len().max(1))
                + state.skins.len()
                - 1;
            clear_outcome(state);
            sync_material_editor(state);
        }
        Message::SkinSelected(index) if !state.busy() && index < state.skins.len() => {
            state.selected_skin = Some(index);
            state.stage_skin = state
                .inspection
                .as_ref()
                .map_or(1, |inspection| inspection.skins.len().max(1))
                + index;
            sync_material_editor(state);
        }
        Message::SkinNameChanged(name) if !state.busy() => {
            if let Some(skin) = state
                .selected_skin
                .and_then(|index| state.skins.get_mut(index))
            {
                skin.name = name;
                clear_outcome(state);
            }
        }
        Message::MaterialSelected(material) if !state.busy() => {
            state.selected_material = Some(material);
            sync_material_editor(state);
        }
        Message::MaterialTintChanged(value) if !state.busy() => state.material_tint = value,
        Message::ReplacementMaterialChanged(value) if !state.busy() => {
            state.replacement_material = value;
        }
        Message::ApplyMaterialTint if !state.busy() => {
            if let Some(color) = parse_color(&state.material_tint) {
                set_material(state, Some(SkinMaterial::Tint { color }));
            }
        }
        Message::ApplyReplacementMaterial if !state.busy() => {
            let name = state.replacement_material.trim().to_owned();
            if !name.is_empty() {
                set_material(state, Some(SkinMaterial::Material { name }));
            }
        }
        Message::MaterialReset if !state.busy() => set_material(state, None),
        Message::SkinTextureRequested
            if !state.busy()
                && state.selected_skin.is_some()
                && state.selected_material.is_some() =>
        {
            let generation = state.generation.bump();
            state.picker = Some(generation);
            return vec![Effect::PickSkinTexture(generation)];
        }
        Message::SkinTextureSelected(generation, path) if state.picker == Some(generation) => {
            state.picker = None;
            if let Some(path) = path {
                set_material(state, Some(SkinMaterial::Texture { path }));
            }
        }
        Message::SaveQcRequested if !state.busy() && state.kind() != Some(SourceKind::Compiled) => {
            if let Some(request) = state.qc_request() {
                let generation = state.generation.bump();
                state.picker = Some(generation);
                state.qc_saved = false;
                state.error = None;
                if let Some(path) = &state.qc_path {
                    return vec![Effect::SaveQc(generation, path.clone(), request)];
                }
                return vec![Effect::PickQcPath(generation, state.source.clone())];
            }
        }
        Message::QcPathSelected(generation, path) if state.picker == Some(generation) => {
            if let Some(path) = path
                && let Some(request) = state.qc_request()
            {
                return vec![Effect::SaveQc(generation, path, request)];
            }
            state.picker = None;
        }
        Message::QcSaved(generation, path, result) if state.picker == Some(generation) => {
            state.picker = None;
            match result {
                Ok(()) => {
                    state.qc_path = Some(path);
                    state.qc_saved = true;
                }
                Err(error) => state.error = Some(error),
            }
        }
        Message::MapsRequested => {
            let generation = state.generation.bump();
            state.map_generation = Some(generation);
            return vec![Effect::DiscoverMaps(generation)];
        }
        Message::MapsLoaded(generation, maps) if state.map_generation == Some(generation) => {
            state.map_generation = None;
            if state.selected_map.is_none() {
                state.selected_map = maps.first().cloned();
            }
            state.maps = maps;
        }
        Message::MapSelected(map) if !state.busy() => {
            state.selected_map = Some(map);
            state.stage = None;
            state.engine_test = None;
            state.engine_test_confirmation = false;
            state.qc_saved = false;
        }
        Message::MapBrowseRequested if !state.busy() => {
            let generation = state.generation.bump();
            state.picker = Some(generation);
            return vec![Effect::PickMap(generation)];
        }
        Message::MapBrowseSelected(generation, path) if state.picker == Some(generation) => {
            state.picker = None;
            if let Some(path) = path {
                let map = MapChoice(path);
                if !state.maps.contains(&map) {
                    state.maps.push(map.clone());
                }
                state.selected_map = Some(map);
                state.stage = None;
                state.engine_test = None;
                state.engine_test_confirmation = false;
                state.qc_saved = false;
            }
        }
        Message::StageRequested if state.selected_map.is_some() => {
            return request_preview(state, true);
        }
        Message::PreviewBuilt(generation, result) if state.picker == Some(generation) => {
            match result {
                Ok(preview) => {
                    let result = preview.result.clone();
                    state.preview = Some(preview);
                    if state.preview_for_stage {
                        if let Some(map) = &state.selected_map {
                            return vec![Effect::PrepareStage(generation, result, map.0.clone())];
                        }
                    } else {
                        return vec![Effect::Preview(generation, result)];
                    }
                }
                Err(error) => state.error = Some(error),
            }
            state.picker = None;
        }
        Message::StagePrepared(generation, result) if state.picker == Some(generation) => {
            state.picker = None;
            match result {
                Ok(prepared) => {
                    let (model, map) = *prepared;
                    return vec![Effect::OpenStage { model, map }];
                }
                Err(error) => state.error = Some(error),
            }
        }
        Message::StageSnapshotCaptured {
            position,
            angles,
            skin,
            bodygroups,
        } => {
            if let Some(map) = &state.selected_map {
                let stage = ModelStage {
                    map: map.0.clone(),
                    position,
                    angles,
                    selected_skin: skin,
                    bodygroups: bodygroups.clone(),
                };
                if state.stage.as_ref() != Some(&stage) {
                    state.qc_saved = false;
                }
                state.stage = Some(stage);
                state.stage_skin = skin;
                state.stage_bodygroups = bodygroups;
            }
        }
        Message::PreviewSelectionCaptured { skin, bodygroups } => {
            state.stage_skin = skin;
            state.stage_bodygroups = bodygroups;
        }
        Message::EngineTestRequested if !state.busy() => state.engine_test_confirmation = true,
        Message::EngineTestDismissed => state.engine_test_confirmation = false,
        Message::EngineTestConfirmed if !state.busy() && state.engine_test_confirmation => {
            state.engine_test_confirmation = false;
            if let Some(result) = state
                .preview
                .as_ref()
                .map(|preview| &preview.result)
                .or(state.result.as_ref())
                && let Some(map) = &state.selected_map
            {
                let generation = state.generation.bump();
                state.picker = Some(generation);
                return vec![Effect::TestInGame {
                    generation,
                    result: result.clone(),
                    map: map.0.clone(),
                    skin: state.stage_skin,
                    bodygroups: state.stage_bodygroups.clone(),
                    placement: state.stage.clone(),
                }];
            }
        }
        Message::EngineTestPrepared(generation, result) if state.picker == Some(generation) => {
            state.picker = None;
            match result {
                Ok(launch) => {
                    let url = launch.url.clone();
                    state.engine_test = Some(launch);
                    return vec![Effect::LaunchGame(url)];
                }
                Err(error) => state.error = Some(error),
            }
        }
        Message::StaticPropChanged(value) if !state.busy() => {
            state.static_prop = value;
            clear_outcome(state);
        }
        Message::AutomaticAxesChanged(value) if !state.busy() => {
            state.automatic_axes = value;
            clear_outcome(state);
        }
        Message::GenerateCollisionChanged(value) if !state.busy() => {
            state.generate_collision = value;
            clear_outcome(state);
        }
        Message::CollisionMassChanged(value) if !state.busy() => {
            state.collision_mass = value;
            clear_outcome(state);
        }
        Message::SurfacePropertyChanged(value) if !state.busy() => {
            state.surface_property = value;
            clear_outcome(state);
        }
        Message::ExportMaterialsChanged(value) if !state.busy() => {
            state.export_materials = value;
            clear_outcome(state);
        }
        Message::OverwriteChanged(value) if !state.busy() => {
            state.overwrite = value;
            clear_outcome(state);
        }
        Message::ExportRequested => {
            let Some(request) = state.request() else {
                return vec![];
            };
            clear_outcome(state);
            let generation = state.generation.bump();
            let cancel = Arc::new(AtomicBool::new(false));
            state.running = Some(RunningExport {
                generation,
                cancel: Arc::clone(&cancel),
                cancellation_requested: false,
            });
            return vec![Effect::Export(generation, request, cancel)];
        }
        Message::CancelRequested => {
            if let Some(running) = &mut state.running {
                running.cancel.store(true, Ordering::Release);
                running.cancellation_requested = true;
            }
        }
        Message::ExportCompleted(generation, result)
            if state
                .running
                .as_ref()
                .is_some_and(|running| running.generation == generation) =>
        {
            let running = state.running.take().expect("matching export");
            match result {
                Ok(result) => state.result = Some(result),
                Err(_) if running.cancellation_requested => state.cancelled = true,
                Err(error) => state.error = Some(error),
            }
        }
        Message::RevealRequested(path) => return vec![Effect::Reveal(path)],
        Message::CopyDiagnostics => {
            let diagnostics = state.error.clone().or_else(|| {
                state
                    .result
                    .as_ref()
                    .or_else(|| state.preview.as_ref().map(|preview| &preview.result))
                    .map(|result| result.warnings.join("\n"))
            });
            if let Some(diagnostics) = diagnostics {
                return vec![Effect::Copy(diagnostics)];
            }
        }
        Message::PreviewRequested => return request_preview(state, false),
        Message::PreviewPrepared(generation, result) if state.picker == Some(generation) => {
            state.picker = None;
            match result {
                Ok(request) => return vec![Effect::OpenPreview(request)],
                Err(error) => state.error = Some(error),
            }
        }
        _ => {}
    }
    vec![]
}

fn select_source(state: &mut State, path: std::path::PathBuf) -> Vec<Effect> {
    let Some(kind) = SourceKind::of(&path) else {
        return vec![];
    };
    state.model_name = if kind == SourceKind::Mesh {
        path.file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned()
            + ".mdl"
    } else {
        String::new()
    };
    state.material_directory.clear();
    state.scale = "1".into();
    state.overwrite = false;
    state.source = Some(path.clone());
    state.static_prop = false;
    state.automatic_axes = true;
    state.generate_collision = false;
    state.collision_mass = "1".into();
    state.surface_property.clear();
    state.stage_skin = 0;
    state.stage_bodygroups.clear();
    state.engine_test = None;
    state.inspection = None;
    state.animations.clear();
    state.skins.clear();
    state.bodygroups.clear();
    state.lods.clear();
    state.bodygroup_editor = None;
    state.bodygroup_filter.clear();
    state.selected_skin = None;
    state.selected_material = None;
    state.orientation = std::array::from_fn(|_| "0".into());
    state.qc_path = None;
    state.stage = None;
    clear_outcome(state);
    let generation = state.generation.bump();
    state.inspection_generation = Some(generation);
    state.qc_editor = super::qc_editor::State::default();
    let mut effects = vec![Effect::Inspect(generation, path.clone())];
    if state.is_qc() {
        state.qc_editor.load_generation = Some(generation);
        state.inspection_generation = None;
        effects = vec![Effect::LoadQc(generation, path)];
    }
    effects
}

fn edit_animation(
    state: &mut State,
    index: usize,
    edit: impl FnOnce(&mut super::state::AnimationEditor),
) {
    if state.busy() || state.kind() != Some(SourceKind::Mesh) || state.static_prop {
        return;
    }
    if let Some(animation) = state.animations.get_mut(index) {
        edit(animation);
        clear_outcome(state);
    }
}

pub(super) fn clear_outcome(state: &mut State) {
    state.result = None;
    state.error = None;
    state.cancelled = false;
    state.qc_saved = false;
    state.preview = None;
    state.engine_test = None;
    state.engine_test_confirmation = false;
}

fn request_preview(state: &mut State, stage: bool) -> Vec<Effect> {
    let Some(request) = state.preview_request() else {
        return vec![];
    };
    state.preview_for_stage = stage;
    state.error = None;
    let generation = state.generation.bump();
    state.picker = Some(generation);
    if let Some(result) = state
        .preview
        .as_ref()
        .map(|preview| &preview.result)
        .or(state.result.as_ref())
        && result.compiled
    {
        return if stage {
            state
                .selected_map
                .as_ref()
                .map(|map| Effect::PrepareStage(generation, result.clone(), map.0.clone()))
                .into_iter()
                .collect()
        } else {
            vec![Effect::Preview(generation, result.clone())]
        };
    }
    vec![Effect::BuildPreview(generation, request)]
}

fn set_material(state: &mut State, replacement: Option<SkinMaterial>) {
    if let Some(skin) = state
        .selected_skin
        .and_then(|index| state.skins.get_mut(index))
        && let Some(material) = &state.selected_material
    {
        if let Some(replacement) = replacement {
            skin.replacements.insert(material.clone(), replacement);
        } else {
            skin.replacements.remove(material);
        }
        clear_outcome(state);
        sync_material_editor(state);
    }
}

fn sync_material_editor(state: &mut State) {
    state.material_tint = "#ffffff".into();
    state.replacement_material.clear();
    if let Some(skin) = state.selected_skin.and_then(|index| state.skins.get(index))
        && let Some(material) = &state.selected_material
        && let Some(replacement) = skin.replacements.get(material)
    {
        match replacement {
            SkinMaterial::Tint { color } => {
                state.material_tint = format!("#{:02x}{:02x}{:02x}", color[0], color[1], color[2]);
            }
            SkinMaterial::Material { name } => state.replacement_material = name.clone(),
            SkinMaterial::Texture { .. } => {}
        }
    }
}

pub(super) fn parse_color(value: &str) -> Option<[u8; 3]> {
    let value = value.trim();
    let value = value.strip_prefix('#').unwrap_or(value);
    if value.len() != 6 {
        return None;
    }
    Some([
        u8::from_str_radix(value.get(0..2)?, 16).ok()?,
        u8::from_str_radix(value.get(2..4)?, 16).ok()?,
        u8::from_str_radix(value.get(4..6)?, 16).ok()?,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generation::Generation;

    fn ready() -> State {
        let mut state = State::default();
        update(&mut state, Message::SourceDropped("cube.smd".into()));
        state.destination = Some("out".into());
        state
    }

    fn load_qc(state: &mut State) {
        let generation = state.qc_editor.load_generation.unwrap();
        update(
            state,
            Message::QcLoaded(generation, Ok("$body body cube.smd\n".into())),
        );
    }

    #[test]
    fn edits_and_repeated_exports_cannot_supersede_running_work() {
        let mut state = ready();
        assert_eq!(update(&mut state, Message::ExportRequested).len(), 1);
        update(&mut state, Message::SourceDropped("other.mdl".into()));
        update(&mut state, Message::ModelNameChanged("other.mdl".into()));
        assert!(update(&mut state, Message::ExportRequested).is_empty());
        assert_eq!(
            state.source.as_deref(),
            Some(std::path::Path::new("cube.smd"))
        );
        assert_eq!(state.model_name, "cube.mdl");
        update(
            &mut state,
            Message::ExportCompleted(Generation::INITIAL, Err("stale".into())),
        );
        assert!(state.busy());
        assert!(state.error.is_none());
    }

    #[test]
    fn cancelling_waits_for_the_worker_before_accepting_another_export() {
        let mut state = ready();
        let effects = update(&mut state, Message::ExportRequested);
        let Effect::Export(generation, _, cancel) = &effects[0] else {
            panic!("export");
        };
        update(&mut state, Message::CancelRequested);
        assert!(cancel.load(Ordering::Acquire));
        assert!(!state.can_export());
        update(
            &mut state,
            Message::ExportCompleted(*generation, Err("cancelled".into())),
        );
        assert!(state.cancelled);
        assert!(state.can_export());
        assert!(state.error.is_none());
    }

    #[test]
    fn invalid_scales_block_mesh_exports_but_do_not_override_qc_options() {
        let mut state = ready();
        for scale in ["NaN", "inf", "0", "-1", ""] {
            update(&mut state, Message::ScaleChanged(scale.into()));
            assert!(!state.can_export());
        }
        update(&mut state, Message::SourceDropped("prop.qc".into()));
        load_qc(&mut state);
        update(&mut state, Message::ScaleChanged("invalid".into()));
        assert_eq!(state.request().expect("QC request").scale, 1.0);
        assert!(state.model_name.is_empty());
    }

    #[test]
    fn physics_edits_are_shared_by_export_preview_and_qc_save() {
        let mut state = ready();
        update(&mut state, Message::GenerateCollisionChanged(true));
        update(&mut state, Message::CollisionMassChanged(" 1250.5 ".into()));
        update(
            &mut state,
            Message::SurfacePropertyChanged(" metal ".into()),
        );
        for request in [state.request(), state.preview_request(), state.qc_request()] {
            let request = request.expect("valid physics options");
            assert_eq!(request.collision_mass, 1250.5);
            assert_eq!(request.surface_property.as_deref(), Some("metal"));
            assert!(request.generate_collision);
        }
        let effects = update(&mut state, Message::ExportRequested);
        assert!(matches!(&effects[..], [Effect::Export(_, _, _)]));
        update(&mut state, Message::CollisionMassChanged("5".into()));
        update(&mut state, Message::SurfacePropertyChanged("wood".into()));
        assert_eq!(state.collision_mass, " 1250.5 ");
        assert_eq!(state.surface_property, " metal ");
    }

    #[test]
    fn invalid_physics_blocks_all_builds_and_new_imports_clear_physics_edits() {
        let mut state = ready();
        update(&mut state, Message::GenerateCollisionChanged(true));
        for value in ["", "0", "-3", "NaN", "inf", "1e99"] {
            update(&mut state, Message::CollisionMassChanged(value.into()));
            assert!(!state.can_export(), "mass: {value}");
            assert!(update(&mut state, Message::PreviewRequested).is_empty());
            assert!(update(&mut state, Message::SaveQcRequested).is_empty());
        }
        update(&mut state, Message::GenerateCollisionChanged(false));
        assert_eq!(state.request().unwrap().collision_mass, 1.0);
        for value in ["metal wood", "metal\nwood", "\"metal\"", "x{}", "../metal"] {
            update(&mut state, Message::SurfacePropertyChanged(value.into()));
            assert!(!state.can_export(), "surface: {value}");
            assert!(state.preview_request().is_none());
            assert!(state.qc_request().is_none());
        }
        update(&mut state, Message::SourceDropped("authored.qc".into()));
        load_qc(&mut state);
        assert_eq!(state.collision_mass, "1");
        assert!(state.surface_property.is_empty());
        update(
            &mut state,
            Message::SurfacePropertyChanged("invalid input".into()),
        );
        update(&mut state, Message::CollisionMassChanged("invalid".into()));
        let request = state.request().expect("authored QC keeps its physics");
        assert_eq!(request.collision_mass, 1.0);
        assert!(request.surface_property.is_none());
    }

    #[test]
    fn old_picker_results_do_not_replace_newer_imports() {
        let mut state = State::default();
        let Effect::PickSource(generation) = update(&mut state, Message::ImportRequested)[0] else {
            panic!("picker");
        };
        update(
            &mut state,
            Message::ImportSelected(generation.next(), Some("stale.mdl".into())),
        );
        assert!(state.source.is_none());
        update(
            &mut state,
            Message::ImportSelected(generation, Some("prop.MDL".into())),
        );
        assert_eq!(state.kind(), Some(SourceKind::Compiled));
    }

    #[test]
    fn leaving_the_route_discards_pending_dialogs_and_preview_results() {
        let mut state = State::default();
        let Effect::PickSource(generation) = update(&mut state, Message::ImportRequested)[0] else {
            panic!("picker");
        };
        update(&mut state, Message::RouteExited);
        update(
            &mut state,
            Message::ImportSelected(generation, Some("prop.mdl".into())),
        );
        update(
            &mut state,
            Message::PreviewPrepared(generation, Err("old preview".into())),
        );
        assert!(state.source.is_none());
        assert!(state.error.is_none());
        assert!(!state.busy());
    }

    #[test]
    fn background_qc_loading_and_inspection_complete_after_route_exit() {
        let mut state = State::default();
        update(&mut state, Message::SourceDropped("source.qc".into()));
        let loading = state.qc_editor.load_generation.unwrap();
        update(&mut state, Message::RouteExited);
        let effects = update(
            &mut state,
            Message::QcLoaded(loading, Ok("$body body mesh.smd\n".into())),
        );
        let [Effect::ValidateQc(validation, source, text)] = effects.as_slice() else {
            panic!("loaded QC must be validated");
        };
        assert_eq!(source, std::path::Path::new("source.qc"));
        assert_eq!(text, "$body body mesh.smd\n");
        assert_eq!(state.qc_request().unwrap().qc_text.as_ref(), Some(text));
        update(&mut state, Message::RouteExited);
        update(
            &mut state,
            Message::Inspected(*validation, Ok(inspection_fixture())),
        );
        assert!(state.qc_editor.validated);
        assert_eq!(state.inspection.as_ref().unwrap().materials, ["paint"]);

        update(&mut state, Message::SourceDropped("source.glb".into()));
        let inspecting = state.inspection_generation.unwrap();
        update(&mut state, Message::RouteExited);
        update(
            &mut state,
            Message::Inspected(inspecting, Ok(inspection_fixture())),
        );
        assert_eq!(state.animations[0].name, "idle");
        assert_eq!(state.inspection.as_ref().unwrap().materials, ["paint"]);
    }

    fn inspection_fixture() -> gmpublished_backend::model_compile::ModelInspection {
        use gmpublished_backend::model_compile::{AnimationSettings, ModelInspection};
        ModelInspection {
            parts: vec![],
            materials: vec!["paint".into()],
            material_directories: vec![],
            skins: vec![vec!["paint".into()]],
            animations: vec![AnimationSettings {
                source: None,
                included: true,
                name: "idle".into(),
                fps: 30.0,
                looping: true,
                frame_count: 2,
                activity: None,
                activity_weight: 0,
                events: vec![],
            }],
        }
    }

    #[test]
    fn authored_skins_keep_indices_and_change_only_the_selected_material() {
        let mut state = ready();
        state.inspection = Some(gmpublished_backend::model_compile::ModelInspection {
            parts: vec![],
            animations: vec![],
            materials: vec!["paint".into(), "glass".into()],
            material_directories: vec![],
            skins: vec![
                vec!["paint".into(), "glass".into()],
                vec!["red".into(), "glass".into()],
            ],
        });
        update(&mut state, Message::SkinAdded("Black".into()));
        assert_eq!(state.stage_skin, 2);
        update(&mut state, Message::MaterialSelected("paint".into()));
        update(&mut state, Message::MaterialTintChanged("#123456".into()));
        update(&mut state, Message::ApplyMaterialTint);
        assert_eq!(
            state.skins[0].replacements.get("paint"),
            Some(&SkinMaterial::Tint {
                color: [0x12, 0x34, 0x56]
            })
        );
        assert!(!state.skins[0].replacements.contains_key("glass"));
        update(&mut state, Message::SkinAdded("Blue".into()));
        assert_eq!(state.stage_skin, 3);
        assert!(state.skins[1].replacements.is_empty());
        update(&mut state, Message::SkinSelected(0));
        assert_eq!(state.material_tint, "#123456");
        update(&mut state, Message::MaterialReset);
        assert!(state.skins[0].replacements.is_empty());
        assert_eq!(state.skins[1].name, "Blue");
    }

    #[test]
    fn preview_can_build_before_an_export_folder_is_chosen() {
        let mut state = State::default();
        update(&mut state, Message::SourceDropped("car.fbx".into()));
        assert!(!state.can_export());
        let effects = update(&mut state, Message::PreviewRequested);
        let Effect::BuildPreview(_, request) = &effects[0] else {
            panic!("preview build");
        };
        assert_eq!(request.source, std::path::PathBuf::from("car.fbx"));
        assert!(request.destination.as_os_str().is_empty());
        assert!(request.automatic_axes);
        assert!(state.busy());
        assert!(update(&mut state, Message::ExportRequested).is_empty());
    }

    #[test]
    fn qc_save_does_not_require_an_export_folder_and_reuses_its_selected_path() {
        let mut state = State::default();
        update(&mut state, Message::SourceDropped("car.fbx".into()));
        let Effect::PickQcPath(generation, _) = update(&mut state, Message::SaveQcRequested)[0]
        else {
            panic!("QC path picker");
        };
        assert!(state.busy());
        assert!(update(&mut state, Message::SaveQcRequested).is_empty());
        let effects = update(
            &mut state,
            Message::QcPathSelected(generation, Some("car.qc".into())),
        );
        let Effect::SaveQc(_, path, request) = &effects[0] else {
            panic!("QC save");
        };
        assert_eq!(path, std::path::Path::new("car.qc"));
        assert!(request.destination.as_os_str().is_empty());
        assert!(request.automatic_axes);
        update(
            &mut state,
            Message::QcSaved(generation.next(), "stale.qc".into(), Ok(())),
        );
        assert!(state.busy());
        assert!(state.qc_path.is_none());
        update(
            &mut state,
            Message::QcSaved(generation, "car.qc".into(), Ok(())),
        );
        assert!(state.qc_saved);
        update(&mut state, Message::ScaleChanged("2".into()));
        assert!(!state.qc_saved);
        let effects = update(&mut state, Message::SaveQcRequested);
        let Effect::SaveQc(_, path, request) = &effects[0] else {
            panic!("repeat QC save");
        };
        assert_eq!(path, std::path::Path::new("car.qc"));
        assert_eq!(request.scale, 2.0);
    }

    #[test]
    fn qc_import_uses_the_description_path_without_a_project_loader() {
        let mut state = State::default();
        let effects = update(&mut state, Message::SourceDropped("car.qc".into()));
        assert_eq!(state.kind(), Some(SourceKind::Description));
        assert!(
            matches!(&effects[0], Effect::LoadQc(_, path) if path == std::path::Path::new("car.qc"))
        );
        assert!(!state.busy());
        assert!(state.qc_request().is_none());
        load_qc(&mut state);
        assert!(state.qc_request().is_some());
        update(&mut state, Message::SourceDropped("car.mdl".into()));
        assert!(state.qc_request().is_none());
        assert!(update(&mut state, Message::SaveQcRequested).is_empty());
    }

    #[test]
    fn automatic_axes_can_be_overridden_and_new_imports_restore_the_default() {
        let mut state = ready();
        update(&mut state, Message::AutomaticAxesChanged(false));
        assert!(!state.request().unwrap().automatic_axes);
        assert!(!state.qc_request().unwrap().automatic_axes);
        update(&mut state, Message::SourceDropped("car.glb".into()));
        assert!(state.preview_request().unwrap().automatic_axes);
        assert_eq!(state.orientation, ["0", "0", "0"]);
    }

    #[test]
    fn scene_placement_and_skin_do_not_change_qc_or_export_orientation() {
        let mut state = ready();
        update(&mut state, Message::StaticPropChanged(true));
        update(&mut state, Message::OrientationChanged(2, "90".into()));
        update(&mut state, Message::GenerateCollisionChanged(true));
        update(
            &mut state,
            Message::MapSelected(MapChoice("gm_construct.bsp".into())),
        );
        update(
            &mut state,
            Message::StageSnapshotCaptured {
                position: [1.0, 2.0, 3.0],
                angles: [0.0, 45.0, 0.0],
                skin: 2,
                bodygroups: vec![1, 0],
            },
        );
        let request = state.qc_request().expect("QC request");
        assert_eq!(request.orientation, [0.0, 0.0, 90.0]);
        assert!(request.generate_collision);
        let scene = state.stage.as_ref().expect("scene");
        assert_eq!(scene.position, [1.0, 2.0, 3.0]);
        assert_eq!(scene.selected_skin, 2);
        assert_eq!(scene.bodygroups, vec![1, 0]);
        assert_eq!(
            state.request().expect("compile request").orientation,
            [0.0, 0.0, 90.0]
        );
    }

    #[test]
    fn stale_inspection_and_map_discovery_cannot_replace_newer_results() {
        let mut state = ready();
        let generation = state.inspection_generation.expect("inspection");
        update(&mut state, Message::SourceDropped("other.glb".into()));
        update(
            &mut state,
            Message::Inspected(generation, Err("old error".into())),
        );
        assert!(state.error.is_none());
        let Effect::DiscoverMaps(generation) = update(&mut state, Message::MapsRequested)[0] else {
            panic!("map discovery");
        };
        update(&mut state, Message::RouteExited);
        update(
            &mut state,
            Message::MapsLoaded(generation, vec![MapChoice("old.bsp".into())]),
        );
        assert!(state.maps.is_empty());
    }

    #[test]
    fn tint_fields_reject_invalid_or_incomplete_input() {
        for input in ["", "#fff", "#gg0000", "ffffff00", "#☃000"] {
            assert_eq!(parse_color(input), None);
        }
        assert_eq!(parse_color(" #aBcD09 "), Some([0xab, 0xcd, 9]));
    }

    fn inspect_actions(state: &mut State) {
        let generation = state.inspection_generation.unwrap();
        let animations = ["walk", "run"]
            .map(
                |name| gmpublished_backend::model_compile::AnimationSettings {
                    source: Some(name.into()),
                    included: true,
                    name: name.into(),
                    fps: 30.0,
                    looping: false,
                    frame_count: 31,
                    activity: None,
                    activity_weight: 0,
                    events: vec![],
                },
            )
            .to_vec();
        update(
            state,
            Message::Inspected(
                generation,
                Ok(gmpublished_backend::model_compile::ModelInspection {
                    parts: vec![],
                    animations,
                    materials: vec![],
                    material_directories: vec![],
                    skins: vec![],
                }),
            ),
        );
    }

    #[test]
    fn animation_edits_reach_export_preview_and_qc_and_clear_saved_outcomes() {
        let mut state = ready();
        inspect_actions(&mut state);
        state.qc_saved = true;
        update(&mut state, Message::AnimationIncluded(0, false));
        update(
            &mut state,
            Message::AnimationNameChanged(1, "run_fast".into()),
        );
        update(&mut state, Message::AnimationFpsChanged(1, "60".into()));
        update(&mut state, Message::AnimationLoopChanged(1, true));
        assert!(!state.qc_saved);
        for request in [state.request(), state.preview_request(), state.qc_request()] {
            let settings = request.unwrap().animation_settings;
            assert_eq!(settings.len(), 2);
            assert!(!settings[0].included);
            assert_eq!(settings[1].source.as_deref(), Some("run"));
            assert_eq!(settings[1].name, "run_fast");
            assert_eq!(settings[1].fps, 60.0);
            assert!(settings[1].looping);
        }
        update(&mut state, Message::StaticPropChanged(true));
        assert!(state.request().unwrap().animation_settings.is_empty());
        update(&mut state, Message::StaticPropChanged(false));
        assert_eq!(state.request().unwrap().animation_settings[1].fps, 60.0);
    }

    #[test]
    fn invalid_animation_edits_disable_every_build_path_and_busy_edits_are_ignored() {
        let mut state = ready();
        inspect_actions(&mut state);
        for value in ["", "0", "-1", "NaN", "inf", "1001"] {
            update(&mut state, Message::AnimationFpsChanged(1, value.into()));
            assert!(state.request().is_none());
            assert!(state.preview_request().is_none());
            assert!(state.qc_request().is_none());
        }
        update(&mut state, Message::AnimationIncluded(1, false));
        assert!(state.request().is_some());
        update(&mut state, Message::AnimationIncluded(0, false));
        assert!(state.request().is_none());
        update(&mut state, Message::AnimationIncluded(0, true));
        update(
            &mut state,
            Message::AnimationNameChanged(0, "bad name".into()),
        );
        assert!(state.request().is_none());
        update(&mut state, Message::AnimationNameChanged(0, "walk".into()));
        update(&mut state, Message::AnimationFpsChanged(1, "30".into()));
        update(&mut state, Message::AnimationIncluded(1, true));
        update(&mut state, Message::AnimationNameChanged(1, "WALK".into()));
        assert!(state.request().is_none());
        update(&mut state, Message::AnimationNameChanged(1, "run".into()));
        let original = state.animations.clone();
        update(&mut state, Message::ExportRequested);
        assert!(state.busy());
        update(
            &mut state,
            Message::AnimationNameChanged(0, "ignored".into()),
        );
        update(&mut state, Message::AnimationFpsChanged(0, "120".into()));
        update(&mut state, Message::AnimationIncluded(0, false));
        update(&mut state, Message::AnimationLoopChanged(0, true));
        assert_eq!(state.animations, original);
    }

    #[test]
    fn advanced_animation_edits_validate_share_requests_and_preserve_busy_state() {
        let mut state = ready();
        inspect_actions(&mut state);
        state.qc_saved = true;
        update(&mut state, Message::AnimationAdvancedToggled(0));
        assert!(state.animations[0].advanced_expanded);
        assert!(state.qc_saved);
        update(
            &mut state,
            Message::AnimationActivityChanged(0, "ACT_VM_RELOAD".into()),
        );
        update(
            &mut state,
            Message::AnimationActivityWeightChanged(0, "7".into()),
        );
        update(&mut state, Message::AnimationEventAdded(0));
        update(
            &mut state,
            Message::AnimationEventFrameChanged(0, 0, "15".into()),
        );
        update(
            &mut state,
            Message::AnimationEventCodeChanged(0, 0, "AE_CL_PLAYSOUND".into()),
        );
        update(
            &mut state,
            Message::AnimationEventOptionsChanged(0, 0, "reload sound".into()),
        );
        assert!(!state.qc_saved);
        for request in [state.request(), state.preview_request(), state.qc_request()] {
            let action = request.unwrap().animation_settings.remove(0);
            assert_eq!(action.activity.as_deref(), Some("ACT_VM_RELOAD"));
            assert_eq!(action.activity_weight, 7);
            assert_eq!(action.events[0].frame, 15);
            assert_eq!(action.events[0].event, "AE_CL_PLAYSOUND");
            assert_eq!(action.events[0].options, "reload sound");
        }
        update(&mut state, Message::AnimationFpsChanged(0, "60".into()));
        assert_eq!(
            state.request().unwrap().animation_settings[0].events[0].frame,
            15
        );
        for frame in ["-1", "31", "NaN", ""] {
            update(
                &mut state,
                Message::AnimationEventFrameChanged(0, 0, frame.into()),
            );
            assert!(state.request().is_none());
            assert!(state.preview_request().is_none());
            assert!(state.qc_request().is_none());
        }
        update(
            &mut state,
            Message::AnimationEventFrameChanged(0, 0, "30".into()),
        );
        update(
            &mut state,
            Message::AnimationActivityWeightChanged(0, "1.5".into()),
        );
        assert!(state.request().is_none());
        update(
            &mut state,
            Message::AnimationActivityChanged(0, String::new()),
        );
        assert_eq!(
            state.request().unwrap().animation_settings[0].activity_weight,
            0
        );
        update(
            &mut state,
            Message::AnimationEventOptionsChanged(0, 0, "bad\"options".into()),
        );
        assert!(state.request().is_none());
        update(&mut state, Message::AnimationIncluded(0, false));
        assert!(state.request().is_some());
        update(&mut state, Message::AnimationIncluded(0, true));
        update(&mut state, Message::AnimationEventRemoved(0, 0));
        assert!(state.request().is_some());
        let original = state.animations.clone();
        update(&mut state, Message::ExportRequested);
        assert!(state.busy());
        for message in [
            Message::AnimationAdvancedToggled(0),
            Message::AnimationEventAdded(0),
            Message::AnimationActivityChanged(0, "ACT_IDLE".into()),
            Message::AnimationEventRemoved(0, 0),
        ] {
            update(&mut state, message);
        }
        assert_eq!(state.animations, original);
    }

    #[test]
    fn stale_inspection_cannot_restore_action_settings_from_another_source() {
        let mut state = ready();
        let stale = state.inspection_generation.unwrap();
        update(&mut state, Message::SourceDropped("new.glb".into()));
        inspect_actions(&mut state);
        update(
            &mut state,
            Message::AnimationNameChanged(0, "new_name".into()),
        );
        update(
            &mut state,
            Message::Inspected(
                stale,
                Ok(gmpublished_backend::model_compile::ModelInspection {
                    parts: vec![],
                    animations: vec![],
                    materials: vec![],
                    material_directories: vec![],
                    skins: vec![],
                }),
            ),
        );
        assert_eq!(state.animations[0].name, "new_name");
        update(&mut state, Message::SourceDropped("authored.qc".into()));
        load_qc(&mut state);
        inspect_actions(&mut state);
        assert!(state.animations.is_empty());
        update(
            &mut state,
            Message::AnimationNameChanged(0, "ignored".into()),
        );
        assert!(state.request().unwrap().animation_settings.is_empty());
    }
}
