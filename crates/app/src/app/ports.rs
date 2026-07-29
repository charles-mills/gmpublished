//! What an effect runner is allowed to reach.
//!
//! A runner taking [`Ports`] instead of `&App` cannot see [`State`], so the
//! publish runner cannot touch search state. One that needs its own feature's
//! state takes it as an argument, where the dependency is visible.
//!
//! [`State`]: super::State

use crate::bridge::tasks::BackendContext;
use crate::i18n::I18n;
use crate::theme::Tokens;

#[derive(Clone, Copy)]
pub(super) struct Ports<'a> {
    pub(super) ctx: &'a BackendContext,
    pub(super) i18n: &'a I18n,
    /// By reference: `Tokens` is ~1 KB, and every runner only reads from it.
    pub(super) tokens: &'a Tokens,
}
