//! Publish request construction and submission lifecycle.

use super::{
    Generation, Instant, Mode, Path, PublishIconSubmitRequestEnvelope, PublishIconSubmitResult,
    PublishSubmitContext, PublishSubmitMode, PublishSubmitPreview, PublishSubmitRequest,
    PublishSubmitRequestEnvelope, PublishSubmitResult, State, UiError, default_icon_path,
    publish_selected_preview, selected_tags,
};

impl State {
    pub(in super::super) fn begin_submit_at(
        &mut self,
        context: PublishSubmitContext,
        now: Instant,
    ) -> Option<PublishSubmitRequestEnvelope> {
        if !self.can_submit() {
            return None;
        }

        let verified = self.verified_path().cloned()?;
        let tags = selected_tags(&self.tags);
        if tags.is_empty() {
            return None;
        }
        let mode = self.submit_mode()?;
        let changelog = self.submit_changelog();
        let preview = self.submit_preview(&context.temp_dir);

        let generation = self.bump_submit_generation();
        self.begin_submit_pending(now);

        Some(PublishSubmitRequestEnvelope {
            generation,
            request: PublishSubmitRequest {
                mode,
                content_source_path: verified.path,
                title: self.title.trim().to_owned(),
                addon_type: self
                    .addon_type
                    .map_or_else(String::new, |value| value.as_str().to_owned()),
                tags,
                changelog,
                description: self.submit_description(),
                preview,
                ignore_globs: context.ignore_globs,
                total_size: verified.total_size,
                temp_dir: context.temp_dir,
            },
        })
    }

    pub(in super::super) fn apply_submit_completion(
        &mut self,
        generation: Generation,
        result: Result<PublishSubmitResult, UiError>,
    ) -> bool {
        self.apply_submit_outcome(generation, result.map(|_result| ()))
    }

    pub(in super::super) fn begin_publish_icon_at(
        &mut self,
        now: Instant,
    ) -> Option<PublishIconSubmitRequestEnvelope> {
        if !self.can_publish_icon() {
            return None;
        }
        let Mode::Update(target) = &self.mode else {
            return None;
        };
        let workshop_id = target.workshop_id;
        let (icon_source_path, upscale) = {
            let selected = self.verified_icon()?;
            (
                selected.icon.source_path.clone(),
                self.upscale_icon && selected.icon.can_upscale,
            )
        };

        let generation = self.bump_submit_generation();
        self.begin_submit_pending(now);

        Some(PublishIconSubmitRequestEnvelope {
            generation,
            icon_source_path,
            upscale,
            workshop_id,
        })
    }

    pub(in super::super) fn apply_publish_icon_completion(
        &mut self,
        generation: Generation,
        result: Result<PublishIconSubmitResult, UiError>,
    ) -> bool {
        self.apply_submit_outcome(generation, result.map(|_result| ()))
    }

    pub(in super::super) fn tick_submit_spinner(&mut self, now: Instant) -> bool {
        self.submit.advance(now)
    }

    fn begin_submit_pending(&mut self, now: Instant) {
        self.submit.start(now);
    }

    #[cfg(test)]
    pub(in super::super) fn begin_submit(
        &mut self,
        context: PublishSubmitContext,
    ) -> Option<PublishSubmitRequestEnvelope> {
        self.begin_submit_at(context, Instant::now())
    }

    #[cfg(test)]
    pub(in super::super) fn begin_publish_icon(
        &mut self,
    ) -> Option<PublishIconSubmitRequestEnvelope> {
        self.begin_publish_icon_at(Instant::now())
    }

    /// Failures are surfaced by the tasks overlay toast (and logged here);
    /// the modal itself only stops its spinner.
    fn apply_submit_outcome(
        &mut self,
        generation: Generation,
        result: Result<(), UiError>,
    ) -> bool {
        if !self.open || !self.submit_pending() || self.submit_generation != generation {
            return false;
        }

        self.submit.stop();
        if let Err(error) = result {
            log::warn!("Prepare Publish submit failed: {error}");
        }
        true
    }

    fn submit_mode(&self) -> Option<PublishSubmitMode> {
        match &self.mode {
            Mode::New => (!self.title.trim().is_empty()).then_some(PublishSubmitMode::New),
            Mode::Update(target) => {
                (!self.changelog_trimmed().is_empty()).then_some(PublishSubmitMode::Update {
                    workshop_id: target.workshop_id,
                })
            }
        }
    }

    /// Only a creation carries a staged description; updates edit theirs
    /// against the live item directly.
    fn submit_description(&self) -> Option<String> {
        match &self.mode {
            Mode::New => self.staged_description().map(str::to_owned),
            Mode::Update(_) => None,
        }
    }

    fn submit_changelog(&self) -> Option<String> {
        match &self.mode {
            Mode::New => None,
            Mode::Update(_) => Some(self.changelog_trimmed()),
        }
    }

    fn submit_preview(&self, temp_dir: &Path) -> Option<PublishSubmitPreview> {
        if let Some(icon) = self.verified_icon() {
            return Some(PublishSubmitPreview::Selected(publish_selected_preview(
                &icon.icon,
                self.upscale_icon,
            )));
        }

        match &self.mode {
            Mode::New => Some(PublishSubmitPreview::Default(default_icon_path(temp_dir))),
            Mode::Update(_) => None,
        }
    }
}
