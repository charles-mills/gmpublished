use crate::{GmaFile, WorkshopItem};

#[derive(Debug, Clone)]
pub enum Addon {
    Installed(GmaFile),
    Workshop(WorkshopItem),
}

impl Addon {
    #[inline(always)]
    pub fn installed(&self) -> Option<&GmaFile> {
        match self {
            Self::Installed(addon) => Some(addon),
            Self::Workshop(_) => None,
        }
    }
}

impl From<GmaFile> for Addon {
    fn from(installed: GmaFile) -> Self {
        Self::Installed(installed)
    }
}

impl From<WorkshopItem> for Addon {
    fn from(item: WorkshopItem) -> Self {
        Self::Workshop(item)
    }
}
