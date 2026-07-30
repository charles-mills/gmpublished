//! Query, palette, and backend search-session lifecycle.

use super::*;

impl State {
    pub(crate) fn input(&self) -> &str {
        &self.input
    }

    pub(crate) const fn mode(&self) -> SearchMode {
        self.mode
    }

    pub(crate) fn rows(&self) -> &[Row] {
        &self.rows
    }

    pub(crate) const fn loading(&self) -> bool {
        self.session.loading()
    }

    pub(crate) const fn has_more(&self) -> bool {
        self.session.has_more()
    }

    pub(crate) fn should_begin_full_search(&self) -> bool {
        self.palette.expanded
            && self.query_active()
            && self.session.has_more()
            && self.session.active_full_task().is_none()
    }

    pub(crate) fn show_empty(&self) -> bool {
        self.query_active() && !self.loading() && self.rows.is_empty()
    }

    pub(crate) const fn palette_open(&self) -> bool {
        self.palette.expanded
    }

    pub(crate) const fn palette_visible(&self) -> bool {
        self.palette.visible
    }

    pub(crate) fn needs_motion_ticks(&self) -> bool {
        self.palette.presence.needs_ticks()
    }

    pub(crate) fn opacity(&self, now: Instant) -> f32 {
        self.palette.opacity(now)
    }

    pub(crate) fn scale(&self, now: Instant) -> f32 {
        self.palette.scale(now)
    }

    pub(crate) fn dropdown_open(&self) -> bool {
        self.palette.expanded
            && self.query_active()
            && (self.loading() || !self.rows.is_empty() || self.has_more() || self.show_empty())
    }

    pub(crate) fn query_active(&self) -> bool {
        !self.input.trim().is_empty()
    }

    pub(crate) fn edit_query(&mut self, input: String) -> QueryEditOutcome {
        if !self.palette.expanded {
            if self.palette.visible {
                return QueryEditOutcome {
                    quick_request: None,
                    cancel_task: None,
                };
            }
            self.palette.expanded = true;
            self.palette.visible = true;
            self.palette.presence.snap(true);
        }

        self.input = input;
        // Editing keeps the palette open even when the input empties out
        // (e.g. backspacing over a typo'd first character) — an empty query
        // hides the results section, not the palette itself.
        let change = self.session.begin_query(&self.input, self.mode);
        self.replace_rows(Vec::new());
        self.pending_quick.clone_from(&change.quick_request);

        QueryEditOutcome {
            quick_request: change.quick_request,
            cancel_task: change.cancel_task,
        }
    }

    pub(crate) fn clear(&mut self) -> Option<TaskId> {
        self.input.clear();
        self.palette.expanded = true;
        self.palette.visible = true;
        self.replace_rows(Vec::new());
        self.pending_quick = None;
        self.session.clear().cancel_task
    }

    pub(crate) fn focus(&mut self, now: Instant) -> bool {
        if self.palette.expanded {
            return false;
        }
        self.palette.expanded = true;
        self.palette.visible = true;
        self.palette.presence.go(true, now);
        true
    }

    pub(crate) fn focus_mode(&mut self, mode: SearchMode, now: Instant) -> FocusModeOutcome {
        let mode_changed = self.mode != mode;
        let cancel_task = if mode_changed {
            self.mode = mode;
            self.input.clear();
            self.replace_rows(Vec::new());
            self.pending_quick = None;
            self.session.clear().cancel_task
        } else {
            None
        };
        let opened = self.focus(now);
        FocusModeOutcome {
            opened,
            mode_changed,
            cancel_task,
        }
    }

    /// Starts closing the palette. The clean-slate reset happens on the
    /// final animation tick so the input stays mounted through the fade.
    /// While closing, edits are ignored; this preserves the ⌘F leak guard.
    pub(crate) fn dismiss(&mut self, now: Instant) -> Option<TaskId> {
        if !self.palette.expanded {
            return None;
        }
        let cancel_task = self.session.active_full_task();
        self.palette.expanded = false;
        self.pending_quick = None;
        self.palette.presence.go(false, now);
        cancel_task
    }

    /// Returns true when the close animation just settled and the palette
    /// fully reset — the caller should re-sweep thumbnail demands, since the
    /// fading rows kept theirs alive until this moment.
    pub(crate) fn tick_motion(&mut self, now: Instant) -> bool {
        if self.palette.presence.tick(now) && !self.palette.expanded && self.palette.visible {
            self.reset_after_close();
            self.palette.visible = false;
            return true;
        }
        false
    }

    fn reset_after_close(&mut self) {
        self.input.clear();
        self.replace_rows(Vec::new());
        self.pending_quick = None;
        let _clear = self.session.clear();
    }

    pub(crate) fn take_debounced_request(
        &mut self,
        request: &SearchQuickRequest,
    ) -> Option<SearchQuickRequest> {
        let current = self.pending_quick.as_ref()?;
        if current.key() != request.key()
            || !self
                .session
                .is_current(request.generation(), request.mode(), request.query())
        {
            return None;
        }

        self.pending_quick.take()
    }

    pub(crate) fn apply_quick_result(
        &mut self,
        key: &SearchRequestKey,
        result: Result<SearchQuickBatch, UiError>,
    ) -> bool {
        match result {
            Ok(batch) => {
                let Some(accepted) = self.session.accept_quick_batch(batch) else {
                    return false;
                };
                let (hits, _has_more) = accepted.into_parts();
                self.replace_rows(rows_from_hits(hits));
            }
            Err(error) => {
                if !self.session.fail_quick(key) {
                    return false;
                }
                self.replace_rows(Vec::new());
                log::warn!("quick search failed for `{}`: {error}", key.query());
            }
        }
        true
    }

    pub(crate) fn begin_full_search(&mut self, task_id: TaskId) -> Option<FullSearchStart> {
        let start = self.session.begin_full_search(task_id, self.mode)?;
        self.pending_quick = None;
        Some(FullSearchStart {
            request: start.request,
            cancel_task: start.cancel_task,
        })
    }

    pub(crate) fn apply_full_batch(&mut self, batch: SearchFullBatch) -> bool {
        let Some(accepted) = self.session.accept_full_batch(batch) else {
            return false;
        };
        let (mode, hits) = accepted.into_parts();

        match mode {
            SearchFullBatchMode::ReplaceQuickRows => {
                self.replace_rows(rows_from_full_hits(0, &hits));
            }
            SearchFullBatchMode::AppendRows => {
                let start = self.rows.len();
                self.rows.extend(rows_from_full_hits(start, &hits));
                self.thumbnail_generation = self.next_thumbnail_generation();
            }
        }
        true
    }

    pub(crate) fn finish_full_search(&mut self, request: &SearchFullRequest) -> bool {
        self.session.finish_full_search(request)
    }
}
