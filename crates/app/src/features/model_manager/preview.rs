use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use super::state::PreviewJob;
use super::{Effect, Message, SourceKind, State};

pub(super) fn schedule(state: &mut State) -> Vec<Effect> {
    state.preview_pending = true;
    let ready = (state.inspection.is_some() && state.can_preview())
        || (state.is_qc() && state.qc_editor_ready() && !state.qc_editor.validated);
    if state.route_active && ready {
        state.preview_due = None;
        vec![Effect::DebouncePreview(state.preview_revision)]
    } else {
        state.preview_due = Some(state.preview_revision);
        vec![]
    }
}

pub(super) fn resume(state: &mut State) -> Vec<Effect> {
    if state.install_pending && !state.busy() && !state.preview_busy() {
        return install_in_game(state);
    }
    if !state.preview_pending
        || state.preview_due != Some(state.preview_revision)
        || !state.route_active
        || state.busy()
        || state.preview_busy()
    {
        return vec![];
    }
    if state.is_qc() && !state.qc_editor.validated {
        if state.qc_editor_ready() && state.inspection_generation.is_none() {
            let generation = state.generation.bump();
            state.inspection_generation = Some(generation);
            state.qc_editor.diagnostic = None;
            return vec![Effect::ValidateQc(
                generation,
                state.source.clone().unwrap(),
                state.qc_editor.text().unwrap(),
            )];
        }
        return vec![];
    }
    if state.inspection.is_none() {
        return vec![];
    }
    request(state, state.preview_for_stage)
}

pub(super) fn request(state: &mut State, stage: bool) -> Vec<Effect> {
    let mode_changed = state.preview_for_stage != stage;
    let bake = stage || state.install_pending;
    state.preview_for_stage = stage;
    if let Some(job) = &state.preview_job {
        let changed_transform = bake
            && state.desired_preview_transform()
                != (job.request.kind == SourceKind::Mesh)
                    .then_some((job.request.scale, job.request.orientation));
        if mode_changed || changed_transform || !state.preview_accepts_presentation() {
            job.cancel.store(true, Ordering::Release);
            state.preview_pending = true;
            state.preview_due = Some(state.preview_revision);
        }
        return vec![];
    }
    let Some(mut request) = state.preview_request() else {
        return vec![];
    };
    let transform_changed = bake && state.desired_preview_transform() != state.displayed_transform;
    let reuse = !state.preview_dirty
        && !transform_changed
        && (!bake || !state.preview_export_dirty)
        && state.preview.is_some();
    if reuse && let Some((scale, orientation)) = state.displayed_transform {
        request.scale = scale;
        request.orientation = orientation;
    }
    let generation = state.generation.bump();
    let cancel = Arc::new(AtomicBool::new(false));
    state.preview_job = Some(PreviewJob {
        generation,
        revision: state.preview_revision,
        export_revision: state.export_revision,
        request: request.clone(),
        cancel: Arc::clone(&cancel),
        replaced_preview: false,
    });
    state.preview_pending = false;
    state.preview_due = None;
    state.error = None;
    if reuse {
        prepare(
            state,
            generation,
            state.preview.as_ref().unwrap().result.clone(),
        )
    } else {
        vec![Effect::BuildPreview(generation, request, cancel)]
    }
}

fn prepare(
    state: &State,
    generation: crate::generation::Generation,
    result: super::ExportResult,
) -> Vec<Effect> {
    if state.preview_for_stage {
        state
            .selected_map
            .as_ref()
            .map(|map| Effect::PrepareStage(generation, result, map.0.clone()))
            .into_iter()
            .collect()
    } else {
        vec![Effect::Preview(generation, result)]
    }
}

fn matches_job(state: &State, generation: crate::generation::Generation) -> bool {
    state
        .preview_job
        .as_ref()
        .is_some_and(|job| job.generation == generation)
}

fn restore(state: &mut State, job: &PreviewJob) {
    if job.replaced_preview {
        state.preview = state.previous_preview.take();
    }
}

fn finish_stale(state: &mut State) -> Vec<Effect> {
    if let Some(job) = state.preview_job.take() {
        restore(state, &job);
    }
    resume(state)
}

pub(super) fn update(state: &mut State, message: &Message) -> Option<Vec<Effect>> {
    match message {
        Message::PreviewUpdateDue(revision) => {
            if *revision == state.preview_revision {
                state.preview_due = Some(*revision);
            }
            Some(resume(state))
        }
        Message::PreviewBuilt(generation, result) if matches_job(state, *generation) => {
            if !state.preview_accepts_presentation() {
                return Some(finish_stale(state));
            }
            match result {
                Ok(preview) => {
                    let result = preview.result.clone();
                    state.previous_preview = state.preview.replace(preview.clone());
                    state.preview_job.as_mut().unwrap().replaced_preview = true;
                    Some(prepare(state, *generation, result))
                }
                Err(error) => update(state, &Message::PreviewFailed(error.clone())),
            }
        }
        Message::PreviewPrepared(generation, result) if matches_job(state, *generation) => {
            if !state.preview_accepts_presentation() {
                return Some(finish_stale(state));
            }
            match result {
                Ok(request) => Some(vec![Effect::OpenPreview(request.clone())]),
                Err(error) => update(state, &Message::PreviewFailed(error.clone())),
            }
        }
        Message::StagePrepared(generation, result) if matches_job(state, *generation) => {
            if !state.preview_accepts_presentation() {
                return Some(finish_stale(state));
            }
            match result {
                Ok(prepared) => {
                    let (model, map) = prepared.as_ref();
                    Some(vec![Effect::OpenStage {
                        model: model.clone(),
                        map: map.clone(),
                    }])
                }
                Err(error) => update(state, &Message::PreviewFailed(error.clone())),
            }
        }
        Message::PreviewLoadCancelled(generation) if matches_job(state, *generation) => {
            state
                .preview_job
                .as_ref()
                .unwrap()
                .cancel
                .store(true, Ordering::Release);
            state.preview_dirty = true;
            Some(finish_stale(state))
        }
        Message::PreviewPresented if state.preview_busy() => {
            if !state.preview_accepts_presentation() {
                return Some(finish_stale(state));
            }
            let job = state.preview_job.take().unwrap();
            state.displayed_transform = (job.request.kind == SourceKind::Mesh)
                .then_some((job.request.scale, job.request.orientation));
            state.previous_preview = None;
            state.preview_dirty = false;
            if job.replaced_preview {
                state.preview_export_dirty = job.export_revision != state.export_revision;
            }
            if state.install_pending {
                state.install_pending = false;
                return Some(install_in_game(state));
            }
            Some(vec![])
        }
        Message::PreviewFailed(error) if state.preview_busy() => {
            if !state.preview_accepts_presentation() {
                return Some(finish_stale(state));
            }
            let job = state.preview_job.take().unwrap();
            restore(state, &job);
            state.preview_dirty = true;
            state.preview_pending = false;
            state.install_pending = false;
            state.error = Some(error.clone());
            Some(vec![])
        }
        _ => None,
    }
}

pub(super) fn install_in_game(state: &mut State) -> Vec<Effect> {
    if state.busy() {
        state.install_pending = true;
        return vec![];
    }
    if !state.can_preview() {
        state.install_pending = false;
        state.install_error = Some(
            "Installation stopped because the model settings are invalid. Correct them and choose Install in Game again."
                .to_owned(),
        );
        return vec![];
    }
    if state.preview_dirty
        || state.preview_export_dirty
        || state.preview.is_none()
        || state.preview_busy()
        || state.desired_preview_transform() != state.displayed_transform
    {
        state.install_pending = true;
        return request(state, state.preview_for_stage);
    }
    state.install_pending = false;
    let generation = state.generation.bump();
    state.install_generation = Some(generation);
    state.installed_models_generation = None;
    state.install_error = None;
    vec![Effect::InstallModel {
        generation,
        result: state.preview.as_ref().unwrap().result.clone(),
    }]
}

#[cfg(test)]
mod tests {
    use super::super::{ExportResult, PreviewBuild};
    use super::*;
    use crate::generation::Generation;

    fn ready() -> State {
        let mut state = State::default();
        super::super::update(&mut state, Message::SourceDropped("prop.smd".into()));
        state.inspection = Some(gmpublished_backend::model_compile::ModelInspection {
            source_model: None,
            collision_parts: vec![],
            collision_physics: None,
            parts: vec![],
            animations: vec![],
            materials: vec!["paint".into()],
            material_directories: vec![],
            skins: vec![vec!["paint".into()]],
        });
        state.preview_pending = false;
        state.preview_due = None;
        state
    }

    fn preview() -> PreviewBuild {
        let directory = Arc::new(tempfile::tempdir().unwrap());
        PreviewBuild {
            result: ExportResult {
                directory: directory.path().to_owned(),
                files: vec![directory.path().join("prop.mdl")],
                bytes: 1,
                warnings: vec![],
                compiled: true,
            },
            _directory: Some(directory),
        }
    }

    fn active_generation(state: &State) -> Generation {
        state.preview_job.as_ref().unwrap().generation
    }

    #[test]
    fn source_inspection_starts_initial_preview_without_an_extra_timer() {
        let mut state = State::default();
        let effects = super::super::update(&mut state, Message::SourceDropped("prop.glb".into()));
        let [Effect::Inspect(generation, _)] = effects.as_slice() else {
            panic!("an uninspected source must only request inspection");
        };
        let mut inspection = ready().inspection.unwrap();
        inspection.collision_parts = vec!["@node:1".into()];
        inspection.parts = ["visible", "physics"]
            .into_iter()
            .enumerate()
            .map(
                |(index, name)| gmpublished_backend::model_compile::SourcePart {
                    selector: format!("@node:{index}"),
                    name: name.into(),
                },
            )
            .collect();
        let effects = super::super::update(
            &mut state,
            Message::Inspected(*generation, Ok(Box::new(inspection))),
        );
        assert!(
            matches!(effects.as_slice(), [Effect::BuildPreview(_, request, _)]
            if matches!(&request.collision, gmpublished_backend::model_compile::ModelCollision::Custom { source, parts, repair: None }
                if source == std::path::Path::new("prop.glb") && parts == &["@node:1"]))
        );
        assert!(state.preview_busy());
        assert!(!state.busy());
    }

    #[test]
    fn opening_and_cancelling_collision_picker_preserves_the_inflight_preview() {
        let mut state = ready();
        super::super::update(&mut state, Message::PreviewRequested);
        let generation = active_generation(&state);
        let revision = state.preview_revision;
        let cancel = Arc::clone(&state.preview_job.as_ref().unwrap().cancel);
        let effects = super::super::update(&mut state, Message::CollisionSourceRequested);
        let [Effect::PickCollisionSource(picker, _)] = effects.as_slice() else {
            panic!("opening a picker must not change the model");
        };
        assert!(
            super::super::update(&mut state, Message::CollisionSourceSelected(*picker, None))
                .is_empty()
        );
        assert_eq!(state.preview_revision, revision);
        assert_eq!(active_generation(&state), generation);
        assert!(!cancel.load(Ordering::Acquire));
    }

    #[test]
    fn edits_remain_responsive_and_only_latest_revision_runs_after_cancelled_worker() {
        let mut state = ready();
        super::super::update(&mut state, Message::PreviewRequested);
        let first = active_generation(&state);
        let cancel = Arc::clone(&state.preview_job.as_ref().unwrap().cancel);
        super::super::update(&mut state, Message::AutomaticAxesChanged(false));
        let superseded = state.preview_revision;
        super::super::update(&mut state, Message::StaticPropChanged(true));
        let latest = state.preview_revision;
        assert!(cancel.load(Ordering::Acquire));
        assert!(!state.busy());
        assert!(super::super::update(&mut state, Message::PreviewUpdateDue(superseded)).is_empty());
        assert!(super::super::update(&mut state, Message::PreviewUpdateDue(latest)).is_empty());
        assert_eq!(active_generation(&state), first);
        let effects = super::super::update(
            &mut state,
            Message::PreviewBuilt(first, Err("cancelled".into())),
        );
        let [Effect::BuildPreview(second, request, _)] = effects.as_slice() else {
            panic!("latest pending revision must start after worker completion")
        };
        assert_ne!(*second, first);
        assert!(request.static_prop);
        assert!(!request.automatic_axes);
        assert!(state.error.is_none());
        assert!(
            super::super::update(&mut state, Message::PreviewBuilt(first, Ok(preview())))
                .is_empty()
        );
        assert_eq!(active_generation(&state), *second);
    }

    #[test]
    fn live_transform_and_tint_edits_keep_inflight_geometry_and_its_original_baseline() {
        let mut state = ready();
        super::super::update(&mut state, Message::PreviewRequested);
        let generation = active_generation(&state);
        let cancel = Arc::clone(&state.preview_job.as_ref().unwrap().cancel);
        super::super::update(&mut state, Message::ScaleChanged("2".into()));
        super::super::update(&mut state, Message::OrientationChanged(2, "90".into()));
        assert!(super::super::update(&mut state, Message::SkinAdded("Paint".into())).is_empty());
        state.selected_material = Some("paint".into());
        assert!(
            super::super::update(&mut state, Message::MaterialTintChanged("#123456".into()))
                .is_empty()
        );
        assert!(!cancel.load(Ordering::Acquire));
        assert_eq!(active_generation(&state), generation);
        assert!(matches!(
            super::super::update(&mut state, Message::PreviewBuilt(generation, Ok(preview())))
                .as_slice(),
            [Effect::Preview(..)]
        ));
        super::super::update(&mut state, Message::PreviewPresented);
        assert_eq!(state.displayed_preview_transform(), Some((1.0, [0.0; 3])));
        assert_eq!(
            state.desired_preview_transform(),
            Some((2.0, [0.0, 0.0, 90.0]))
        );
        assert_eq!(
            state.skins[0].replacements["paint"],
            gmpublished_backend::model_project::SkinMaterial::Tint {
                color: [0x12, 0x34, 0x56]
            }
        );
        assert!(state.preview_export_dirty);
        assert!(!state.preview_dirty);
    }

    #[test]
    fn stale_decode_keeps_last_good_model_and_does_not_drop_pending_edits() {
        let mut state = ready();
        let original = preview();
        state.preview = Some(original.clone());
        state.displayed_transform = Some((1.0, [0.0; 3]));
        super::super::update(&mut state, Message::PreviewRequested);
        let generation = active_generation(&state);
        super::super::update(&mut state, Message::PreviewBuilt(generation, Ok(preview())));
        super::super::update(&mut state, Message::AutomaticAxesChanged(false));
        let revision = state.preview_revision;
        super::super::update(&mut state, Message::PreviewUpdateDue(revision));
        assert!(!state.preview_accepts_presentation());
        let effects = super::super::update(&mut state, Message::PreviewPresented);
        assert_eq!(state.preview.as_ref().unwrap().result, original.result);
        assert_eq!(state.displayed_preview_transform(), Some((1.0, [0.0; 3])));
        assert!(
            matches!(effects.as_slice(), [Effect::BuildPreview(_, request, _)] if !request.automatic_axes)
        );
    }

    #[test]
    fn staging_bakes_live_changes_instead_of_reusing_old_exported_geometry() {
        let mut state = ready();
        state.preview = Some(preview());
        state.preview_dirty = false;
        state.preview_export_dirty = false;
        state.displayed_transform = Some((1.0, [0.0; 3]));
        state.selected_map = Some(super::super::MapChoice("gm_construct.bsp".into()));
        assert!(super::super::update(&mut state, Message::ScaleChanged("3".into())).is_empty());
        let effects = super::super::update(&mut state, Message::StageRequested);
        assert!(
            matches!(effects.as_slice(), [Effect::BuildPreview(_, request, _)] if request.scale == 3.0)
        );
    }

    #[test]
    fn installation_bakes_latest_live_edits_without_requiring_a_map_or_export_folder() {
        let mut state = ready();
        state.preview = Some(preview());
        state.preview_dirty = false;
        state.preview_export_dirty = false;
        state.displayed_transform = Some((1.0, [0.0; 3]));
        super::super::update(&mut state, Message::ScaleChanged("2".into()));
        super::super::update(&mut state, Message::SkinAdded("Paint".into()));
        super::super::update(&mut state, Message::MaterialSelected("paint".into()));
        super::super::update(&mut state, Message::MaterialTintChanged("#123456".into()));
        super::super::update(&mut state, Message::InstallRequested);
        let effects = super::super::update(&mut state, Message::InstallConfirmed);
        let [Effect::BuildPreview(first, request, _)] = effects.as_slice() else {
            panic!("live changes must be baked before installation");
        };
        assert_eq!(request.scale, 2.0);
        assert!(state.selected_map.is_none());
        assert!(state.destination.is_none());
        assert!(!state.preview_for_stage);
        super::super::update(&mut state, Message::ScaleChanged("3".into()));
        super::super::update(&mut state, Message::MaterialTintChanged("#abcdef".into()));
        super::super::update(&mut state, Message::PreviewBuilt(*first, Ok(preview())));
        let effects = super::super::update(&mut state, Message::PreviewPresented);
        let [Effect::BuildPreview(second, request, _)] = effects.as_slice() else {
            panic!("edits made during compilation must be baked before copying");
        };
        assert_ne!(first, second);
        assert_eq!(request.scale, 3.0);
        assert_eq!(
            request.skins[0].replacements["paint"],
            gmpublished_backend::model_project::SkinMaterial::Tint {
                color: [0xab, 0xcd, 0xef],
            }
        );
        assert!(state.install_generation.is_none());
        let latest = preview();
        let expected = latest.result.clone();
        super::super::update(&mut state, Message::PreviewBuilt(*second, Ok(latest)));
        let effects = super::super::update(&mut state, Message::PreviewPresented);
        assert!(matches!(
            effects.as_slice(),
            [Effect::InstallModel { result, .. }] if result == &expected
        ));
    }
}
