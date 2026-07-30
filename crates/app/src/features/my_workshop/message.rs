use std::collections::HashMap;
use std::time::Instant;

use crate::bridge::domain::PublishedFileId;
use crate::bridge::ui_error::UiError;
use crate::widgets::addon_grid;

use super::model::PageResult;
use crate::generation::Generation;

#[derive(Clone, Debug, PartialEq)]
pub enum Message {
    RouteEntered,
    RouteExited,
    PageCompleted(Generation, u32, Result<PageResult, UiError>),
    StatsRefreshTick,
    StatsRefreshCompleted(Generation, Result<HashMap<PublishedFileId, u64>, UiError>),
    CountRollTick(Instant),
    AnimationTick(Instant),
    #[cfg(feature = "debug")]
    DebugSubscribersAdjusted {
        workshop_id: PublishedFileId,
        delta: i64,
    },
    Grid(addon_grid::Message),
}
