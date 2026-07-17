use std::collections::HashMap;
use std::time::Instant;

use crate::bridge::domain::PublishedFileId;
use crate::bridge::ui_error::UiError;
use crate::widgets::addon_grid;

use super::model::PageResult;

#[derive(Clone, Debug, PartialEq)]
pub enum Message {
    RouteEntered,
    RouteExited,
    PageCompleted(u64, u32, Result<PageResult, UiError>),
    StatsRefreshTick,
    StatsRefreshCompleted(u64, Result<HashMap<PublishedFileId, u64>, UiError>),
    CountRollTick(Instant),
    AnimationTick(Instant),
    #[cfg(feature = "debug")]
    DebugSubscribersAdjusted {
        workshop_id: PublishedFileId,
        delta: i64,
    },
    Grid(addon_grid::Message),
}
