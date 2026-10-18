use crate::i18n::I18n;
use crate::theme::Tokens;

#[derive(Clone, Copy)]
pub struct ViewCtx<'a> {
    pub(crate) cpu_executor: Option<&'a gmpublished_backend::CpuExecutor>,
    pub(crate) tokens: &'a Tokens,
    pub(crate) i18n: &'a I18n,
}

impl<'a> ViewCtx<'a> {
    pub(crate) fn with_cpu_executor(
        mut self,
        executor: &'a gmpublished_backend::CpuExecutor,
    ) -> Self {
        self.cpu_executor = Some(executor);
        self
    }

    pub(crate) const fn new(tokens: &'a Tokens, i18n: &'a I18n) -> Self {
        Self {
            tokens,
            i18n,
            cpu_executor: None,
        }
    }
}
