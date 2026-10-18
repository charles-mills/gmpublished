//! 真实素材测试的路径解析（**仅 `#[cfg(test)]`**，不进产物）。

use std::path::PathBuf;

/// 取真实素材路径：**环境变量优先**，其次本机历史默认路径。
///
/// 两者都不存在时返回 `None`，并打印**可操作**的提示（该设哪个变量、
/// 从哪来），而不是一句「跳过」。
pub fn asset(env_var: &str, local_default: &str) -> Option<PathBuf> {
    if let Some(v) = std::env::var_os(env_var) {
        let p = PathBuf::from(v);
        if p.exists() {
            return Some(p);
        }
        panic!(
            "{env_var} 指向的路径不存在：{}\n\
             （环境变量已设置但路径无效 —— 这是配置错误，不是「素材缺失」，故直接失败）",
            p.display()
        );
    }
    let p = PathBuf::from(local_default);
    if p.exists() {
        return Some(p);
    }
    None
}

/// 取真实素材路径，取不到就 panic（带可操作提示）。
///
/// 用于「标了 `#[ignore]`、被显式要求跑」的测试 —— 既然用户明确要跑，
/// 素材缺失就是**失败**，不是跳过。这正是 `#[ignore]` 相比「静默 return」
/// 的价值：**跳过与失败不再混淆**。
pub fn require(env_var: &str, local_default: &str, what: &str) -> PathBuf {
    asset(env_var, local_default).unwrap_or_else(|| {
        panic!(
            "找不到{what}。\n\
             请设置环境变量 {env_var} 指向它，例如：\n\
             \x20   PowerShell:  $env:{env_var} = 'D:\\path\\to\\asset'\n\
             \x20   bash:        export {env_var}=/path/to/asset\n\
             本机默认路径（当前不存在）：{local_default}"
        )
    })
}
