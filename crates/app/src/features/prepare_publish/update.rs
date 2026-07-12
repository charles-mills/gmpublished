use std::time::Instant;

use crate::backend::domain::WORKSHOP_LEGAL_URL;

use super::{Effect, Message, State};

pub fn update(state: &mut State, message: Message) -> Vec<Effect> {
    match message {
        Message::OpenRequested {
            target,
            ignored_patterns,
            upscale_icon_default,
        } => {
            let request = state.open_target(target, ignored_patterns, upscale_icon_default);
            let mut effects = vec![Effect::ModalOpenRequested];
            if let Some(request) = request {
                effects.push(Effect::PathVerificationRequested(request));
            }
            effects.push(Effect::ThumbnailDemandsChanged);
            effects
        }
        Message::CloseRequested => {
            state.close();
            vec![Effect::ThumbnailDemandsChanged]
        }
        Message::AddonPathEdited(value) => {
            state.edit_addon_path(value);
            Vec::new()
        }
        Message::AddonPathAccepted => state
            .begin_accepted_path_verification()
            .map_or_else(Vec::new, |request| {
                vec![Effect::PathVerificationRequested(request)]
            }),
        Message::WorkshopLinkRequested => state
            .workshop_url()
            .map_or_else(Vec::new, |url| vec![Effect::OpenUrlRequested(url)]),
        Message::AddonPathBrowseRequested => vec![Effect::ContentPickerRequested],
        Message::AddonPathBrowseCompleted(path) => path
            .map(|path| path.to_string_lossy().into_owned())
            .and_then(|path| state.begin_content_path_verification(&path))
            .map_or_else(Vec::new, |request| {
                vec![Effect::PathVerificationRequested(request)]
            }),
        Message::IconBrowseRequested => vec![Effect::IconPickerRequested],
        Message::IconBrowseCompleted {
            path,
            temp_dir,
            well_rgb,
        } => path
            .and_then(|path| state.begin_icon_verification(path, temp_dir, well_rgb))
            .map_or_else(Vec::new, |request| {
                vec![Effect::IconVerificationRequested(request)]
            }),
        Message::IconVerificationCompleted(generation, result) => {
            let _applied = state.apply_icon_verification_result(generation, result);
            Vec::new()
        }
        Message::IconRemoveRequested => {
            let _changed = state.remove_icon();
            Vec::new()
        }
        Message::IconUpscaleToggled(value) => {
            state.toggle_upscale_icon(value);
            Vec::new()
        }
        Message::IconAnimationTick(now) => {
            let _changed = state.tick_icon_animation(now);
            Vec::new()
        }
        Message::AddonTypeSelected(option) => {
            state.set_addon_type(&option.value);
            Vec::new()
        }
        Message::TagSelected(index, option) => {
            state.set_tag(index, &option.value);
            Vec::new()
        }
        Message::IgnorePatternEdited(value) => {
            state.edit_ignore_pattern(value);
            Vec::new()
        }
        Message::IgnorePatternAccepted => state
            .accept_ignore_pattern()
            .map_or_else(Vec::new, |mutation| {
                vec![Effect::IgnorePatternMutationRequested(mutation)]
            }),
        Message::IgnorePatternRemoveRequested(pattern) => state
            .remove_ignore_pattern(&pattern)
            .map_or_else(Vec::new, |mutation| {
                vec![Effect::IgnorePatternMutationRequested(mutation)]
            }),
        Message::IgnorePatternMutationCompleted(result) => state
            .apply_ignore_pattern_mutation_result(result)
            .map_or_else(Vec::new, |request| {
                vec![Effect::PathVerificationRequested(request)]
            }),
        Message::PathVerificationCompleted(generation, result) => {
            let _changed = state.apply_verification_result(generation, result);
            Vec::new()
        }
        Message::BrowserSelectHoverChanged(hovered) => {
            state.set_browser_select_hover(hovered, Instant::now());
            Vec::new()
        }
        Message::DirectoryOpened(path) => {
            let _changed = state.open_directory(&path);
            Vec::new()
        }
        #[cfg(feature = "asset-studio")]
        Message::PreviewEntryRequested(path) => state
            .entry_preview_request(&path)
            .map_or_else(Vec::new, |request| {
                vec![Effect::EntryPreviewRequested(request)]
            }),
        #[cfg(feature = "asset-studio")]
        Message::FilePreview(_) => Vec::new(),
        Message::UpRequested => {
            let _changed = state.go_up();
            Vec::new()
        }
        Message::TitleEdited(value) => {
            state.edit_title(value);
            Vec::new()
        }
        Message::ChangelogActionPerformed(action) => {
            state.perform_changelog_action(action);
            Vec::new()
        }
        Message::SubmitRequested => vec![Effect::SubmitContextRequested],
        Message::PublishIconRequested => {
            state.begin_publish_icon().map_or_else(Vec::new, |request| {
                vec![Effect::PublishIconSubmitRequested(request)]
            })
        }
        Message::PublishIconSubmitCompleted(generation, result) => {
            let effects = if matches!(&result, Ok(result) if result.legal_agreement_required) {
                vec![Effect::OpenUrlRequested(WORKSHOP_LEGAL_URL.to_owned())]
            } else {
                Vec::new()
            };
            let _changed = state.apply_publish_icon_completion(generation, result);
            effects
        }
        Message::SubmitSpinnerTick(now) => {
            let _changed = state.tick_submit_spinner(now);
            Vec::new()
        }
        Message::SubmitContextLoaded(Ok(context)) => state
            .begin_submit(context)
            .map_or_else(Vec::new, |request| {
                vec![Effect::PublishSubmitRequested(request)]
            }),
        Message::SubmitContextLoaded(Err(error)) => {
            log::warn!("Prepare Publish submit context load failed: {error}");
            Vec::new()
        }
        Message::PublishSubmitCompleted(generation, result) => {
            let effects = result.as_ref().map_or_else(
                |_| Vec::new(),
                |result| vec![Effect::PublishSuccessUrlsRequested(*result)],
            );
            let _changed = state.apply_submit_completion(generation, result);
            effects
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Message, State, update};
    use crate::features::prepare_publish::OpenTarget;

    #[test]
    fn close_resets_modal_state() {
        let mut state = State::default();
        let _effects = update(
            &mut state,
            Message::OpenRequested {
                target: OpenTarget::New,
                ignored_patterns: Vec::new(),
                upscale_icon_default: true,
            },
        );

        let _effects = update(&mut state, Message::CloseRequested);

        assert!(!state.open());
    }
}
