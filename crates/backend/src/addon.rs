use crate::{GMAFile, WorkshopItem};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Addon {
    Installed(GMAFile),
    Workshop(WorkshopItem),
}

impl Addon {
    #[inline(always)]
    pub fn installed(&self) -> Option<&GMAFile> {
        match self {
            Self::Installed(addon) => Some(addon),
            Self::Workshop(_) => None,
        }
    }
}

impl From<GMAFile> for Addon {
    fn from(installed: GMAFile) -> Self {
        Self::Installed(installed)
    }
}

impl From<WorkshopItem> for Addon {
    fn from(item: WorkshopItem) -> Self {
        Self::Workshop(item)
    }
}
