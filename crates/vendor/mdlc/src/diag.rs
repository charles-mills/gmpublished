//! 诊断输出的**流路由**与 **`log` 后端**。
//!
//! ```text
//! studiomdl 的 ERROR 在 stdout 里 → True
//! studiomdl 的 ERROR 在 stderr 里 → False
//! ```

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};

/// 本包名。`log` 的默认 target 是 `module_path!()`，本包发出的记录
/// 一律以它开头（bin 根模块正好等于包名，lib 模块是 `mdlc::xxx`）。
const CRATE: &str = env!("CARGO_PKG_NAME");

/// 诊断是否改走 stdout。默认 `false`（stderr）。
static TO_STDOUT: AtomicBool = AtomicBool::new(false);

/// 设置诊断流。**必须在任何诊断输出之前调用一次**（`run_official` 入口）。
pub fn set_to_stdout(v: bool) {
    TO_STDOUT.store(v, Ordering::Relaxed);
}

/// 诊断当前是否走 stdout。
#[inline]
pub fn to_stdout() -> bool {
    TO_STDOUT.load(Ordering::Relaxed)
}

/// 这条记录是不是**本 crate 自己**发的。
///
/// **为什么必须过滤**：装了 logger 之后，依赖里的 `log` 调用会**第一次**
/// 变得可见 —— `i18n-embed` 有 25 处，其中两条 `info!`
/// （`requester.rs:299` 的 `Current Locale: …`、`lib.rs:602` 的
/// `Selecting translations for domain …`）**每次启动都会走**；
/// `parry3d` / `ureq` / `rustls` 另有 11 处。
/// 它们在本模块之前从不可见，放出来会污染 Crowbar 的日志视图。
fn is_own_target(target: &str) -> bool {
    target == CRATE
        || target
            .strip_prefix(CRATE)
            .is_some_and(|rest| rest.starts_with("::"))
}

/// [`log`] 的后端：把记录按 [`to_stdout`] 路由到 stdout 或 stderr。
///
/// 库代码**不需要**知道它的存在，直接 `log::info!(…)` 即可。
pub struct DiagLogger;

impl log::Log for DiagLogger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        is_own_target(metadata.target())
    }

    fn log(&self, record: &log::Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        // `writeln!` 而不是 `println!`：后者写失败（例如下游关掉了管道）
        // 会 panic，而诊断输出的失败不该掀掉一次编译。
        //
        // `StdoutLock` 底层仍是 `LineWriter`，尾换行会触发 flush ——
        // 与 `println!` 的可见行为一致。
        if to_stdout() {
            let _ = writeln!(std::io::stdout().lock(), "{}", record.args());
        } else {
            let _ = writeln!(std::io::stderr().lock(), "{}", record.args());
        }
    }

    fn flush(&self) {
        let _ = std::io::stdout().flush();
        let _ = std::io::stderr().flush();
    }
}

/// 唯一的 logger 实例。用 `static` 而不是 `Box`：`set_logger` 收
/// `&'static dyn Log`，不要求 `alloc` feature。
static LOGGER: DiagLogger = DiagLogger;

/// 装 logger、定路由、定级别。**进程内调用一次**（`main` 的最前面）。
///
/// 重复调用无害：`set_logger` 第二次返回 `Err`，这里忽略。
pub fn init(to_stdout: bool) {
    set_to_stdout(to_stdout);
    let _ = log::set_logger(&LOGGER);
    // 默认级别 `Info`：`error!` / `warn!` / `info!` 全放，与迁移前
    // `diagln!` 无条件输出等价。`debug!` / `trace!` 留给将来的
    // `-verbose`（`log::set_max_level(LevelFilter::Debug)` 一行即可，
    // 该 flag 目前被官方形态解析但无人读取）。
    log::set_max_level(log::LevelFilter::Info);
}

#[cfg(test)]
mod tests {
    use super::{CRATE, DiagLogger, is_own_target, set_to_stdout, to_stdout};
    use log::Log as _;

    #[test]
    fn own_targets_are_accepted_and_foreign_ones_are_not() {
        // bin 根模块的 `module_path!()` 正好是包名。
        assert!(is_own_target("mdlc"));
        // lib 的模块是 `mdlc::xxx`。
        assert!(is_own_target("mdlc::compile"));
        assert!(is_own_target("mdlc::qc::parse"));
        // 依赖里带同名前缀的**必须**被拒（`mdlcx` 不是 `mdlc::`）。
        assert!(!is_own_target("mdlcx"));
        assert!(!is_own_target("mdlc_extra"));
        // 实测会走到的几条依赖 target。
        assert!(!is_own_target("i18n_embed::requester"));
        assert!(!is_own_target("parry3d::query::epa3"));
        assert!(!is_own_target("ureq::cookies"));
        assert!(!is_own_target(""));
    }

    #[test]
    fn enabled_follows_the_target_filter() {
        let logger = DiagLogger;
        let own = log::Metadata::builder()
            .level(log::Level::Info)
            .target("mdlc::update")
            .build();
        let foreign = log::Metadata::builder()
            .level(log::Level::Info)
            .target("i18n_embed::requester")
            .build();
        assert!(logger.enabled(&own));
        assert!(!logger.enabled(&foreign));
    }

    #[test]
    fn routing_flag_round_trips() {
        // 默认必须是 stderr —— 官方兼容形态会显式翻成 true。
        let before = to_stdout();
        set_to_stdout(true);
        assert!(to_stdout());
        set_to_stdout(false);
        assert!(!to_stdout());
        set_to_stdout(before);
        assert_eq!(to_stdout(), before);
    }

    #[test]
    fn crate_constant_is_the_package_name() {
        assert_eq!(CRATE, "mdlc");
    }
}
