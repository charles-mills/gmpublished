//! 测试里切换语言（**仅 `#[cfg(test)]`**）。

use std::sync::Mutex;

/// 串行化所有「改语言 → 断言 → 还原」的测试。
static LOCALE_LOCK: Mutex<()> = Mutex::new(());

/// 出作用域时还原语言 —— **panic 也要还原**。
///
/// ⚠️ 不能写成 `let out = f(); set_locale(&saved); out`：`f()` 一旦 panic，
/// 还原那行就永远执行不到，语言会**留在 `zh-CN`**，此后同一个进程里所有断言
/// 英文的测试全部连环失败 —— 一条失败被放大成几十条，根因被埋在噪声里。
/// 用 `Drop` 还原，panic 展开时也会走到。
struct RestoreLocale(String);

impl Drop for RestoreLocale {
    fn drop(&mut self) {
        rust_i18n::set_locale(&self.0);
    }
}

/// 在指定语言下跑一段代码，跑完还原原语言。
///
/// 还原不只是卫生问题：同一个进程里还跑着几百条别的测试。
///
/// 中毒的锁取回内层 guard —— 一条测试失败不该让其余测试连环失败。
pub fn with_locale<R>(locale: &str, f: impl FnOnce() -> R) -> R {
    let _guard = LOCALE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _restore = RestoreLocale((*rust_i18n::locale()).to_string());
    rust_i18n::set_locale(locale);
    f()
}

/// 在**英文源文**下跑一段代码。
///
/// 英文是源语言（键就是英文原文），所以 `t!` 未命中 ⟹ 拿到的是键本身。
/// 断英文文案的测试都走这里，而不是假定「没别人改过语言」。
pub fn with_english<R>(f: impl FnOnce() -> R) -> R {
    with_locale("en", f)
}
