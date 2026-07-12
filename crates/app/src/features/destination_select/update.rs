use super::model::DestinationKind;
use super::{Effect, Message, State};

pub fn update(state: &mut State, message: Message) -> Vec<Effect> {
    match message {
        Message::OpenRequested { snapshot, context } => {
            state.open(*snapshot, context);
            vec![Effect::ModalOpenRequested, Effect::SnapshotApplied]
        }
        Message::KindToggled(DestinationKind::Browse) => {
            // Browse toggles off like the other tiles, but selecting it goes
            // through the native folder picker.
            if state.kind_active(DestinationKind::Browse) {
                state.deselect();
                Vec::new()
            } else {
                vec![Effect::FolderPickerRequested]
            }
        }
        Message::KindToggled(kind) => {
            state.toggle_kind(kind);
            Vec::new()
        }
        Message::PathInputEdited(value) => {
            state.edit_path_input(value);
            Vec::new()
        }
        Message::PathAccepted => {
            state.accept_path_input();
            Vec::new()
        }
        Message::BrowseCompleted(Some(path)) => {
            state.select_custom(path);
            Vec::new()
        }
        Message::BrowseCompleted(None) => Vec::new(),
        Message::CreateFolderToggled(enabled) => {
            state.set_create_folder(enabled);
            vec![Effect::CreateFolderChanged(enabled)]
        }
        Message::CreateFolderSaved(result) => {
            match result {
                // Absorb without resetting: the user is mid-choice.
                Ok(snapshot) => state.absorb_snapshot(*snapshot),
                Err(error) => state.set_error(error),
            }
            Vec::new()
        }
        Message::HistorySelected(path) => {
            state.select_custom(path);
            Vec::new()
        }
        Message::ConfirmRequested => state.persist_request().map_or_else(Vec::new, |request| {
            state.begin_save();
            vec![Effect::DestinationPersistRequested(request)]
        }),
        Message::SaveCompleted(result) => {
            let persisted = result.is_ok();
            state.apply_save_result(result.map(|snapshot| *snapshot));
            if persisted {
                vec![Effect::DestinationPersisted]
            } else {
                Vec::new()
            }
        }
        Message::CloseFinished => vec![Effect::DestinationDismissed],
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::super::model::{DestinationKind, SettingsSnapshot};
    use super::super::state::OpenContext;
    use super::*;
    use crate::backend::{AppPaths, Settings};

    fn opened_state(temp: &tempfile::TempDir) -> State {
        let paths = AppPaths {
            settings_file: temp.path().join("settings.json"),
            default_user_data_dir: temp.path().join("user-data-default"),
            default_temp_dir: temp.path().join("temp"),
            default_downloads_dir: None,
            temp_dir: temp.path().join("temp"),
            user_data_dir: temp.path().join("user-data"),
            downloads_dir: None,
            gmod_dir: None,
        };
        fs::create_dir_all(temp.path().join("temp")).expect("temp dir");
        let mut state = State::default();
        let _task = update(
            &mut state,
            Message::OpenRequested {
                snapshot: Box::new(SettingsSnapshot::new(Settings::default(), paths)),
                context: OpenContext::default(),
            },
        );
        state
    }

    #[test]
    fn confirm_emits_save_request_for_valid_selection() {
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let custom = temp.path().join("custom");
        fs::create_dir(&custom).expect("custom destination should exist");
        let mut state = opened_state(&temp);
        state.select_custom(custom);

        let effects = update(&mut state, Message::ConfirmRequested);

        assert!(matches!(
            effects.as_slice(),
            [Effect::DestinationPersistRequested(request)]
                if matches!(
                    request.destination,
                    crate::backend::ExtractDestination::Directory(_)
                        | crate::backend::ExtractDestination::NamedDirectory(_)
                )
        ));
        assert!(!state.can_confirm());
    }

    #[test]
    fn browse_tile_requests_the_picker_then_toggles_off() {
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let custom = temp.path().join("picked");
        fs::create_dir(&custom).expect("picked destination should exist");
        let mut state = opened_state(&temp);

        let effects = update(&mut state, Message::KindToggled(DestinationKind::Browse));
        assert_eq!(effects, vec![Effect::FolderPickerRequested]);
        assert!(!state.kind_active(DestinationKind::Browse));

        let effects = update(&mut state, Message::BrowseCompleted(Some(custom)));
        assert!(effects.is_empty());
        assert!(state.kind_active(DestinationKind::Browse));

        let effects = update(&mut state, Message::KindToggled(DestinationKind::Browse));
        assert!(effects.is_empty());
        assert!(!state.kind_active(DestinationKind::Browse));
    }

    #[test]
    fn disabled_downloads_tile_keeps_the_chooser_unselected() {
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let mut state = opened_state(&temp);

        let effects = update(&mut state, Message::KindToggled(DestinationKind::Downloads));

        assert!(effects.is_empty());
        assert!(!state.kind_active(DestinationKind::Downloads));
        assert!(!state.can_confirm());
    }

    #[test]
    fn open_emits_modal_and_snapshot_effects() {
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let paths = AppPaths {
            settings_file: temp.path().join("settings.json"),
            default_user_data_dir: temp.path().join("user-data-default"),
            default_temp_dir: temp.path().join("temp"),
            default_downloads_dir: None,
            temp_dir: temp.path().join("temp"),
            user_data_dir: temp.path().join("user-data"),
            downloads_dir: None,
            gmod_dir: None,
        };
        fs::create_dir_all(temp.path().join("temp")).expect("temp dir");
        let mut state = State::default();

        let effects = update(
            &mut state,
            Message::OpenRequested {
                snapshot: Box::new(SettingsSnapshot::new(Settings::default(), paths)),
                context: OpenContext::default(),
            },
        );

        assert_eq!(
            effects,
            vec![Effect::ModalOpenRequested, Effect::SnapshotApplied]
        );
    }

    #[test]
    fn create_folder_toggle_emits_persistence_effect() {
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let mut state = opened_state(&temp);

        let effects = update(&mut state, Message::CreateFolderToggled(true));

        assert_eq!(effects, vec![Effect::CreateFolderChanged(true)]);
        assert!(state.create_folder());
    }

    #[test]
    fn save_success_emits_persisted_but_failure_keeps_overlay_open() {
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let mut state = opened_state(&temp);
        let snapshot = SettingsSnapshot::new(Settings::default(), state.paths().clone());

        let effects = update(&mut state, Message::SaveCompleted(Ok(Box::new(snapshot))));

        assert_eq!(effects, vec![Effect::DestinationPersisted]);

        let effects = update(
            &mut state,
            Message::SaveCompleted(Err(crate::backend::ui_error::UiError::detailed(
                gmpublished_backend::error_key::keys::IO_ERROR,
                Some("failed".to_owned()),
            ))),
        );

        assert!(effects.is_empty());
        assert!(matches!(
            state.error(),
            Some(crate::features::destination_select::DestinationError::SaveFailed(_))
        ));
    }
}
