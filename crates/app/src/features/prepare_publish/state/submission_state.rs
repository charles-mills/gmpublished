//! Submission editor values and prerequisite state.

use super::text_editor;

/// Changelog editor content with value semantics for state snapshots.
#[derive(Debug, Default)]
pub struct ChangelogContent(pub(super) text_editor::Content);

impl ChangelogContent {
    pub(super) fn from_text(text: &str) -> Self {
        Self(text_editor::Content::with_text(text))
    }

    pub(crate) const fn content(&self) -> &text_editor::Content {
        &self.0
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub(super) fn text(&self) -> String {
        self.0.text()
    }

    pub(super) fn perform(&mut self, action: text_editor::Action) {
        self.0.perform(action);
    }
}

impl Clone for ChangelogContent {
    fn clone(&self) -> Self {
        Self::from_text(&self.text())
    }
}

impl PartialEq for ChangelogContent {
    fn eq(&self, other: &Self) -> bool {
        self.text() == other.text()
    }
}

mapped_enum_with_all! {
    /// A submit prerequisite the user supplies themselves, ordered as the modal
    /// lays them out.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    #[repr(u8)]
    pub enum Requirement {
        AddonPath => "prepare-publish-needs-addon-path",
        Title => "prepare-publish-needs-title",
        AddonType => "prepare-publish-needs-addon-type",
        Tag => "prepare-publish-needs-tag",
        Changelog => "prepare-publish-needs-changelog",
    }
    label_key -> &'static str
}

impl Requirement {
    pub(super) const fn bit(self) -> u8 {
        1 << self as u8
    }
}

/// Everything standing between the modal's contents and a submit.
///
/// The two halves read differently, so they stay apart: `pending` work clears
/// itself and only ever earns a "hold on" line, while `missing` requirements
/// wait on the user and are the ones that redden a control.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Blockers {
    pub(super) pending: bool,
    pub(super) missing: u8,
}

impl Blockers {
    pub(crate) const fn is_empty(self) -> bool {
        !self.pending && self.missing == 0
    }

    pub(crate) const fn pending(self) -> bool {
        self.pending
    }

    pub(crate) const fn contains(self, requirement: Requirement) -> bool {
        self.missing & requirement.bit() != 0
    }

    pub(crate) fn missing(self) -> impl Iterator<Item = Requirement> {
        Requirement::ALL
            .into_iter()
            .filter(move |requirement| self.contains(*requirement))
    }
}
