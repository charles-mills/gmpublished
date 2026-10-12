use crate::i18n::I18n;
use crate::theme::Tokens;

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
