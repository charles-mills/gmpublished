use std::path::PathBuf;

use super::{Effect, Message, State, update};
use crate::features::file_preview;
use crate::generation::Generation;

#[derive(Debug, Default)]
pub(super) struct Workspace {
    pub skin: usize,
    pub bodygroups: Vec<usize>,
    pub placement: Option<Placement>,
}

#[derive(Debug, PartialEq)]
pub(super) struct Placement {
    pub map: PathBuf,
    pub position: [f32; 3],
    pub angles: [f32; 3],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Presentation {
    pub request: Generation,
    owner: Option<Generation>,
    pending_quality: Option<bool>,
}

impl State {
    #[cfg(test)]
    pub(crate) fn presentation_request(&self) -> Option<Generation> {
        self.presentation.map(|load| load.request)
    }

    pub(crate) fn set_workspace_selection(&mut self, skin: usize, bodygroups: Vec<usize>) {
        if self.workspace.placement.is_some()
            && (self.workspace.skin != skin || self.workspace.bodygroups != bodygroups)
        {
            self.qc_saved = false;
        }
        self.workspace.skin = skin;
        self.workspace.bodygroups = bodygroups;
    }

    fn observe_viewer_placement(&mut self, viewer: &file_preview::State) {
        let Some(map) = self.selected_map_path() else {
            return;
        };
        let displayed = viewer
            .request()
            .and_then(|request| request.archive.entry_disk_path(&request.entry_path));
        if displayed != Some(map) {
            return;
        }
        if let Some((position, angles)) = viewer.staged_placement() {
            let placement = Placement {
                map: map.to_owned(),
                position,
                angles,
            };
            if self.workspace.placement.as_ref() != Some(&placement) {
                self.workspace.placement = Some(placement);
                self.qc_saved = false;
            }
        }
    }
}

pub fn update_viewer(
    state: &mut State,
    viewer: &mut file_preview::State,
    message: file_preview::Message,
) -> (Vec<file_preview::Effect>, Vec<Effect>) {
    use file_preview::Message as ViewerMessage;
    if let ViewerMessage::MapReducedTextureDetailToggled(reduced) = &message
        && let Some(load) = &mut state.presentation
        && load.owner.is_some()
    {
        load.pending_quality = Some(*reduced);
        return (vec![], vec![]);
    }
    let completed = match &message {
        ViewerMessage::Loaded(generation, result)
            if state
                .presentation
                .is_some_and(|load| load.request == *generation) =>
        {
            Some(
                result
                    .as_ref()
                    .as_ref()
                    .map(|_| ())
                    .map_err(ToString::to_string),
            )
        }
        ViewerMessage::StageLoaded(generation, result)
            if state
                .presentation
                .is_some_and(|load| load.request == *generation) =>
        {
            Some(
                result
                    .as_ref()
                    .as_ref()
                    .map(|_| ())
                    .map_err(ToString::to_string),
            )
        }
        _ => None,
    };
    let finished = completed.as_ref().and_then(|_| state.presentation.take());
    if let Some(owner) = finished.and_then(|load| load.owner)
        && (state.preview_job_generation() != Some(owner) || !state.preview_accepts_presentation())
    {
        let mut effects = file_preview::update(viewer, ViewerMessage::WorkspaceLoadCancelled);
        let mut manager = update(state, Message::PreviewLoadCancelled(owner));
        if let Some(reduced) = finished.and_then(|load| load.pending_quality) {
            let (next, work) = update_viewer(
                state,
                viewer,
                ViewerMessage::MapReducedTextureDetailToggled(reduced),
            );
            effects.extend(next);
            manager.extend(work);
        }
        return (effects, manager);
    }
    let closing = matches!(
        message,
        ViewerMessage::CloseFinished
            | ViewerMessage::BackRequested
            | ViewerMessage::WorkspaceLoadCancelled
    );
    if closing {
        state.observe_viewer_placement(viewer);
    }
    let cancelled = closing.then(|| state.presentation.take()).flatten();
    let managed = matches!(
        message,
        ViewerMessage::WorkspaceOpenRequested(_) | ViewerMessage::WorkspaceStageRequested { .. }
    );
    let skin_changed = matches!(message, ViewerMessage::SkinSelected(_));
    let bodygroups_changed = matches!(message, ViewerMessage::BodygroupChoiceSelected { .. });
    let mut effects = file_preview::update(viewer, message);
    if let Some(request) = effects.iter().find_map(|effect| match effect {
        file_preview::Effect::LoadRequested(request) => Some(request.request_id),
        file_preview::Effect::LoadStage { generation, .. } => Some(*generation),
        _ => None,
    }) {
        state.presentation = Some(Presentation {
            request,
            owner: managed.then(|| state.preview_job_generation()).flatten(),
            pending_quality: None,
        });
    }
    if (skin_changed || bodygroups_changed) && viewer.current_model().is_some() {
        let skin = if skin_changed {
            viewer.selected_skin()
        } else {
            state.workspace.skin
        };
        let bodygroups = if bodygroups_changed {
            viewer.bodygroup_choices().to_vec()
        } else {
            state.workspace.bodygroups.clone()
        };
        state.set_workspace_selection(skin, bodygroups);
    }
    let mut manager = vec![];
    if let Some(owner) = cancelled.and_then(|load| load.owner) {
        manager.extend(update(state, Message::PreviewLoadCancelled(owner)));
    }
    if completed.as_ref().is_some_and(Result::is_ok) {
        if let Some((position, angles)) = state.saved_stage_placement() {
            viewer.restore_stage_placement(position, angles);
        }
        viewer.apply_workspace_selection(state.workspace.skin, &state.workspace.bodygroups);
    }
    state.observe_viewer_placement(viewer);
    if finished.is_some_and(|load| load.owner.is_some())
        && let Some(result) = completed
    {
        manager.extend(update(
            state,
            match result {
                Ok(()) => Message::PreviewPresented,
                Err(error) => Message::PreviewFailed(error),
            },
        ));
    }
    if let Some(reduced) = finished.and_then(|load| load.pending_quality) {
        let (next, work) = update_viewer(
            state,
            viewer,
            ViewerMessage::MapReducedTextureDetailToggled(reduced),
        );
        effects.extend(next);
        manager.extend(work);
    }
    (effects, manager)
}
