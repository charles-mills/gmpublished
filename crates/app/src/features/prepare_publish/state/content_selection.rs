//! Content selection, Workshop tags, and browser interaction value types.

use super::{Easing, PathBuf, PublishedFileId, WorkshopSnapshotId, motion, theme};

mapped_enum_with_all! {
    /// Steam Workshop addon-type tag. The wire value (`as_str`) is the exact
    /// string the backend and workshop tags expect, not a Rust-cased rendering.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub enum AddonType {
        ServerContent => "ServerContent",
        Gamemode => "gamemode",
        Map => "map",
        Weapon => "weapon",
        Vehicle => "vehicle",
        Npc => "npc",
        Tool => "tool",
        Effects => "effects",
        Model => "model",
        Entity => "entity",
    }
    as_str -> &'static str
}

impl AddonType {
    pub(super) fn from_workshop_tag(tag: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|candidate| candidate.as_str().eq_ignore_ascii_case(tag))
    }
}

mapped_enum_with_all! {
    /// Steam Workshop content tag (up to three per addon).
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub enum AddonTag {
        Fun => "fun",
        Roleplay => "roleplay",
        Scenic => "scenic",
        Movie => "movie",
        Realism => "realism",
        Cartoon => "cartoon",
        Water => "water",
        Comic => "comic",
        Build => "build",
    }
    as_str -> &'static str
}

impl AddonTag {
    pub(super) fn from_workshop_tag(tag: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|candidate| candidate.as_str().eq_ignore_ascii_case(tag))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OpenTarget {
    New,
    Update(UpdateTarget),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UpdateTarget {
    pub(crate) workshop_id: PublishedFileId,
    pub(crate) title: String,
    pub(crate) tags: Vec<String>,
    pub(crate) preview_url: Option<String>,
    pub(crate) snapshot_request_id: WorkshopSnapshotId,
    pub(crate) snapshot_destination: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Mode {
    New,
    Update(UpdateTarget),
}

/// Hover presence of the file-browser empty state, with value semantics.
#[derive(Clone, Debug)]
pub struct BrowserSelectHover(pub(super) motion::Presence<bool>);

impl Default for BrowserSelectHover {
    fn default() -> Self {
        Self(motion::asymmetric(
            false,
            theme::invariant().motion.hover_in_duration(),
            theme::invariant().motion.hover_out_duration(),
            Easing::EaseOut,
        ))
    }
}

impl PartialEq for BrowserSelectHover {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}
