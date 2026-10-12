use std::{fmt, num::NonZeroU64};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct WorkshopId(NonZeroU64);

impl WorkshopId {
    /// `None` for zero, which is Steam's "no item" rather than an item.
    #[must_use]
    pub const fn new(id: u64) -> Option<Self> {
        match NonZeroU64::new(id) {
            Some(id) => Some(Self(id)),
            None => None,
        }
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }

    #[must_use]
    pub const fn get_nonzero(self) -> NonZeroU64 {
        self.0
    }
}

impl From<NonZeroU64> for WorkshopId {
    fn from(id: NonZeroU64) -> Self {
        Self(id)
    }
}

impl fmt::Display for WorkshopId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl From<WorkshopId> for steamworks::PublishedFileId {
    fn from(id: WorkshopId) -> Self {
        Self(id.get())
    }
}

impl TryFrom<steamworks::PublishedFileId> for WorkshopId {
    type Error = ZeroWorkshopId;

    fn try_from(id: steamworks::PublishedFileId) -> Result<Self, Self::Error> {
        Self::new(id.0).ok_or(ZeroWorkshopId)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("a workshop id of zero names no item")]
pub struct ZeroWorkshopId;

/// Settings files store IDs as bare integers. Keep this format compatible.
impl Serialize for WorkshopId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.get().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for WorkshopId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = u64::deserialize(deserializer)?;
        Self::new(raw).ok_or_else(|| serde::de::Error::custom(ZeroWorkshopId))
    }
}

#[cfg(test)]
#[must_use]
pub(crate) fn workshop_id(id: u64) -> WorkshopId {
    WorkshopId::new(id).expect("a fixture workshop id must be nonzero")
}

#[cfg(test)]
mod tests {
    use super::WorkshopId;

    #[test]
    fn zero_is_not_an_id() {
        assert!(WorkshopId::new(0).is_none());
        assert_eq!(WorkshopId::new(1).map(WorkshopId::get), Some(1));
    }

    #[test]
    fn it_round_trips_through_steamworks_spelling() {
        let id = WorkshopId::new(76_561_198_000_000_000).expect("nonzero");
        let steam: steamworks::PublishedFileId = id.into();

        assert_eq!(steam.0, id.get());
        assert_eq!(WorkshopId::try_from(steam), Ok(id));
        assert!(WorkshopId::try_from(steamworks::PublishedFileId(0)).is_err());
    }

    #[test]
    fn it_serializes_as_a_bare_integer() {
        let id = WorkshopId::new(12345).expect("nonzero");

        assert_eq!(serde_json::to_string(&id).expect("serialize"), "12345");
        assert_eq!(
            serde_json::from_str::<WorkshopId>("12345").expect("deserialize"),
            id
        );
        assert!(serde_json::from_str::<WorkshopId>("0").is_err());
    }
}
