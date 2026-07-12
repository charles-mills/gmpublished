use crate::i18n::I18n;
use crate::theme::Tokens;

/// Bundles the tokens/i18n pair that view helpers pass down their whole
/// call tree, so functions take one small `Copy` reference instead of a
/// repeated positional tail.
#[derive(Clone, Copy)]
pub struct ViewCtx<'a> {
    pub(crate) tokens: &'a Tokens,
    pub(crate) i18n: &'a I18n,
}

impl<'a> ViewCtx<'a> {
    pub(crate) const fn new(tokens: &'a Tokens, i18n: &'a I18n) -> Self {
        Self { tokens, i18n }
    }
}
