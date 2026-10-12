use crate::bridge::tasks::BackendContext;
use crate::i18n::I18n;
use crate::theme::Tokens;
use std::cell::Cell;

#[derive(Clone, Copy)]
pub(super) struct Ports<'a> {
    pub(super) ctx: &'a BackendContext,
    pub(super) i18n: &'a I18n,
    pub(super) tokens: &'a Tokens,
    pub(super) workshop_snapshot_sequence: &'a Cell<u64>,
}
