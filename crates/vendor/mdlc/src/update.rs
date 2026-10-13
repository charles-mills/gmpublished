//! 更新检测：**后台静默**，永不阻塞编译。
//!
//! ```text
//! 第 1 次运行  ──► 缓存过期 ⟹ 派生子进程（立即返回，不等）
//!                        └─► 子进程请求 GitHub，写缓存
//! 第 2 次运行  ──► 读缓存 ⟹ 有新版本？打印一行
//! ```

use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// 仓库（`owner/name`），用于拼 GitHub URL。
pub const REPO: &str = "charles-mills/mdlc";

/// `releases/latest` 会 **302** 到 `releases/tag/<tag>`。
///
/// 这里刻意**不跟随重定向**（`max_redirects(0)`）—— 直接从 `Location`
/// 响应头里取 tag，比下载整个 release 页面（几十 KB HTML）再解析便宜得多。
pub const LATEST_URL: &str = "https://github.com/charles-mills/mdlc/releases/latest";

/// 两次网络检测之间的最小间隔。
pub const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// 关掉更新检测的环境变量（**任意非空值**即生效）。
pub const DISABLE_ENV: &str = "MDLC_NO_UPDATE_CHECK";

/// 打开排错输出的环境变量（默认完全静默）。
pub const DEBUG_ENV: &str = "MDLC_UPDATE_DEBUG";

/// 内部子命令名 —— 既是 `spawn_check` 派生出来的子进程入口，
/// 也是手动触发的入口。**不出现在 `--help` 里**（`main` 在 clap 之前拦截）。
pub const HIDDEN_SUBCOMMAND: &str = "__update-check";

/// 单次请求的总超时。宁可放弃检测，也不能留一个挂死的后台进程。
pub const TIMEOUT: Duration = Duration::from_secs(5);

/// 缓存文件名（放在平台缓存目录下的 `mdlc/` 子目录里）。
const CACHE_FILE: &str = "update-check.json";

/// 缓存内容。
///
/// 用 JSON 而不是「纯文本两行」：`serde_json` 已是直接依赖
/// （`src/gltf.rs` 读 `extras.targetNames` 用的），不新增包。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Cache {
    /// 上次**成功**检测的时间（Unix 秒）。
    ///
    /// 只在请求成功时更新 —— 网络不通不刷新它，否则一次断网会把
    /// 检测推迟整整一天。
    pub last_check: u64,
    /// 上次检测到的最新 tag（原样，含 `v` 前缀）。
    pub latest: String,
}

/// 当前时间（Unix 秒）。系统时钟早于 1970 时返回 0（不 panic）。
fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 缓存文件的完整路径。取不到平台缓存目录时返回 `None`（此时功能静默停用）。
///
/// 不引入 `dirs` / `directories` crate —— 那会为一个 `PathBuf` 多加依赖。
pub fn cache_path() -> Option<PathBuf> {
    let base = if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
    }?;
    Some(base.join("mdlc").join(CACHE_FILE))
}

/// 读缓存。文件不存在 / 读不动 / JSON 坏了都返回 `None`（当作「没有缓存」）。
pub fn read_cache() -> Option<Cache> {
    let path = cache_path()?;
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// 写缓存（先写临时文件再改名，避免读到写了一半的内容）。
///
/// 失败一律忽略 —— 缓存写不进去只意味着「下次还要再检测一遍」，
/// 不该让任何人的编译报错。
pub fn write_cache(cache: &Cache) {
    let Some(path) = cache_path() else { return };
    let Some(dir) = path.parent() else { return };
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let Ok(text) = serde_json::to_string(cache) else {
        return;
    };
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, text).is_err() {
        return;
    }
    // Windows 的 `rename` 目标存在时会失败，所以先删目标。
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::rename(&tmp, &path);
}

/// 更新检测是否被关闭。
///
/// 两条判据：显式环境变量（任意非空值），以及 `CI`（CI 里检测毫无意义，
/// 而且会让构建日志多一行噪声）。
pub fn is_disabled() -> bool {
    if std::env::var_os(DISABLE_ENV).is_some_and(|v| !v.is_empty()) {
        return true;
    }
    std::env::var_os("CI").is_some_and(|v| !v.is_empty())
}

/// 排错输出是否打开。
pub fn is_debug() -> bool {
    std::env::var_os(DEBUG_ENV).is_some_and(|v| !v.is_empty())
}

/// 现在该不该发起网络检测？
///
/// 判据 = 「没有缓存」或「距上次成功检测已超过 [`CHECK_INTERVAL`]」。
/// **纯函数**（`now` 由调用方给），所以时间相关的分支可以直接测。
pub fn should_check(cache: Option<&Cache>, now: u64) -> bool {
    match cache {
        None => true,
        Some(c) => now.saturating_sub(c.last_check) >= CHECK_INTERVAL.as_secs(),
    }
}

/// 把 `v1.2.3` / `1.2.3` 解析成三段数字。解析不了返回 `None`。
///
/// 预发布后缀（`-rc1`）按规范**截掉**：`v0.2.0-rc1` 解析成 `(0,2,0)`。
/// 这样做的后果是「rc 与正式版同号」—— 见 [`is_newer`] 的说明。
pub fn parse_version(tag: &str) -> Option<(u64, u64, u64)> {
    let s = tag.trim().strip_prefix('v').unwrap_or(tag.trim());
    let s = s.split(['-', '+']).next()?;
    let mut it = s.split('.');
    let major = it.next()?.parse().ok()?;
    let minor = it.next().unwrap_or("0").parse().ok()?;
    let patch = it.next().unwrap_or("0").parse().ok()?;
    Some((major, minor, patch))
}

/// `latest` 是否比 `current` 新？
///
/// 两侧都解析不了、或**数字段相等**时返回 `false` —— 保守起见只认
/// 「数字上严格更大」。所以 `v0.2.0-rc1` 相对 `0.2.0` **不会**触发提示，
/// 这是有意的：宁可漏报一个预发布版，也不要对着正式版用户喊「有新版」。
pub fn is_newer(latest: &str, current: &str) -> bool {
    match (parse_version(latest), parse_version(current)) {
        (Some(l), Some(c)) => l > c,
        _ => false,
    }
}

/// 有更新时返回要打印的那一行，否则 `None`。**纯函数**，便于直接测文案。
///
/// 文案走译文表（源文英文，见 `locales/app.yml`）；`REPO` 也当占位符传，
/// 免得把仓库名在键里写死一遍。
pub fn notice(cache: &Cache, current: &str) -> Option<String> {
    if !is_newer(&cache.latest, current) {
        return None;
    }
    Some(crate::tr_fmt!(
        "Note: mdlc %{latest} is available (you have v%{current}) — https://github.com/%{repo}/releases/latest",
        latest = cache.latest,
        current = current,
        repo = REPO
    ))
}

/// 发起一次网络检测，返回最新 tag。
///
/// 更新检测失败的原因。
///
/// # 为什么没有 `Clone` / `PartialEq`
///
/// [`ureq::Error`] 只有 `Debug`（`ureq-3.4.2/src/error.rs:7-9` 是
/// `#[derive(Debug)] #[non_exhaustive]`），所以包着它的枚举也拿不到这两个
/// trait。这不影响使用 —— 本类型只在 `run_check` 里被打印，从不参与比较。
///
/// # 为什么保留 `Request` 的 `#[source]`
///
/// 网络失败的原因（DNS、TLS、超时、代理）对排查很关键，改造前它们被
/// `format!("请求失败：{e}")` 压成了一行字符串；现在 `source()` 能拿到
/// 原始的 `ureq::Error` 链。
#[derive(Debug, thiserror::Error)]
pub enum UpdateError {
    /// HTTP 请求本身失败（DNS / 连接 / TLS / 超时）。
    #[error("{}", crate::tr_fmt!("Request failed: %{err}", err = .0))]
    Request(#[from] ureq::Error),
    /// 拿到了响应，但没有 `Location` 头 —— 仓库可能还没有 Release。
    #[error(
        "{}",
        crate::tr_fmt!(
            "HTTP %{status} but there is no Location header (the repository may not have a release yet)",
            status = status
        )
    )]
    NoLocation { status: u16 },
    /// `Location` 的末段是空的。
    #[error(
        "{}",
        crate::tr_fmt!("No tag in the Location header: %{location}", location = location)
    )]
    EmptyTag { location: String },
    /// `Location` 的末段不像版本号。
    #[error(
        "{}",
        crate::tr_fmt!(
            "The tag in the Location header does not look like a version: %{tag}",
            tag = tag
        )
    )]
    NotAVersion { tag: String },
}

/// 拉取最新 release 的 tag。
///
/// 走 `releases/latest` 的 302：**不跟随重定向**，直接从 `Location` 头
/// 取最后一段路径。实测本机 300~650 ms。
pub fn fetch_latest_tag() -> Result<String, UpdateError> {
    let config = ureq::Agent::config_builder()
        // 不跟随重定向 —— 302 的 Location 就是答案。
        .max_redirects(0)
        .max_redirects_will_error(false)
        // 自己判状态码，让错误信息能带上 URL 与状态。
        .http_status_as_error(false)
        .timeout_global(Some(TIMEOUT))
        .user_agent(concat!("mdlc/", env!("CARGO_PKG_VERSION")))
        .build();
    let agent = ureq::Agent::new_with_config(config);

    let resp = agent.get(LATEST_URL).call()?;
    let status = resp.status().as_u16();

    // 200 = 没有 release（GitHub 把 /releases/latest 落到列表页）；
    // 302 = 有 release，Location 里就是 tag。
    let loc = resp
        .headers()
        .get("location")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let Some(loc) = loc else {
        return Err(UpdateError::NoLocation { status });
    };
    let tag = loc
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| UpdateError::EmptyTag {
            location: loc.clone(),
        })?;
    // 只接受看起来像版本号的 tag，避免把别的路径段当成版本。
    if parse_version(tag).is_none() {
        return Err(UpdateError::NotAVersion {
            tag: tag.to_string(),
        });
    }
    Ok(tag.to_string())
}

/// **子进程侧**：跑一次检测并把结果写进缓存。
///
/// 这是 `mdlc __update-check` 的全部实现，也是派生出去的子进程的入口 ——
/// 两者共用，所以手动跑与后台跑的行为永远一致。
pub fn run_check() {
    match fetch_latest_tag() {
        Ok(tag) => {
            write_cache(&Cache {
                last_check: now_unix(),
                latest: tag.clone(),
            });
            // ⚠️ 下面三处（含 `startup` 里那处）**故意留 `eprintln!`**，不换成
            // `log::debug!`：它们由显式的开发者开关 `MDLC_UPDATE_DEBUG` 打开，
            // 而 `log` 的默认级别是 `Info`（见 [`crate::diag::init`]），换成
            // `debug!` 会被直接过滤掉、等于把这个开关废掉。且它们**必须**留在
            // stderr —— 走 `log::info!` 的话官方兼容形态下会落到 stdout，
            // 往 Crowbar 的日志里塞与编译无关的行。
            if is_debug() {
                eprintln!("mdlc 更新检测：最新 tag = {tag}");
            }
        }
        Err(msg) => {
            if is_debug() {
                eprintln!("mdlc 更新检测失败：{msg}");
            }
        }
    }
}

/// Windows：在派生期间摘掉 std 句柄的可继承位。
///
/// **为什么需要它**：`std::process::Command` 在 Windows 上**无条件**用
/// `bInheritHandles = TRUE` 调 `CreateProcessW`（`inherit_handles` 默认为
/// `true`，而且 `std` 没有用 `PROC_THREAD_ATTRIBUTE_HANDLE_LIST` 收窄集合），
/// 于是子进程会复制**本进程里所有**可继承句柄 —— 包括调用方为了让
/// `ReadToEnd()` 拿到 EOF 而建的 stdout 管道。调用方等的是「管道关闭」，
/// 而我们的子进程要活几百毫秒（网络请求）⟹ 调用方白等那几百毫秒。
///
/// `Stdio::null()` **挡不住**这件事：它只换掉子进程的 0/1/2 三个槽，
/// 管道句柄仍在可继承集合里。唯一在 stable 上的解法就是把我们自己的
/// std 句柄临时改成不可继承。
///
/// **不会破坏子进程的 stdio**：`std` 的 `Stdio::Null` 是自己打开
/// `\\.\NUL` 并显式设 `inherit_handle(true)` 的，用的不是我们这个句柄；
/// 而 `Stdio::inherit()` 走 `DuplicateHandle(.., bInheritHandle = TRUE, ..)`，
/// 复制出来的新句柄的可继承性由**参数**决定，与源句柄无关。
///
/// 用 `windows`（而不是 `windows-sys`）的理由，正好落在本模块最容易写错的
/// 三处：`HANDLE` 是 newtype 而非 `isize`，`HANDLE::is_invalid()` 一次覆盖
/// `NULL` 与 `INVALID_HANDLE_VALUE` 两个哨兵（`windows-sys` 要手写
/// `h == 0 || h == INVALID_HANDLE_VALUE`），`GetStdHandle` 直接返回
/// `Result` 而不必比对裸 `BOOL`。
#[cfg(windows)]
mod no_inherit {
    use windows::Win32::Foundation::{
        GetHandleInformation, HANDLE, HANDLE_FLAG_INHERIT, HANDLE_FLAGS, SetHandleInformation,
    };
    use windows::Win32::System::Console::{
        GetStdHandle, STD_ERROR_HANDLE, STD_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
    };

    /// 要处理的三个 std 槽。
    const STD_IDS: [STD_HANDLE; 3] = [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE];

    /// 摘掉 std 句柄的可继承位，离开作用域时**逐句柄恢复原状**。
    ///
    /// 用 `Drop` 而不是「`spawn` 之后手动恢复」：`spawn` 可能失败，中间也可能
    /// panic，而**忘了恢复会让本进程后续派生的进程拿不到 stdout** —— 那种
    /// bug 比原本多等几百毫秒严重得多。`Drop` 让「恢复」不依赖任何控制流。
    ///
    /// 只动「本来带这个位」的句柄；拿不到信息的句柄一个字都不动。
    pub struct NoInheritStdHandles {
        /// `(句柄, 原本是否可继承)` —— 只含我们真的改过的。
        restore: Vec<(HANDLE, bool)>,
    }

    impl NoInheritStdHandles {
        /// 读当前状态并清位。返回的守卫一旦析构就会恢复。
        pub fn new() -> Self {
            let mut restore: Vec<(HANDLE, bool)> = Vec::new();
            for id in STD_IDS {
                // SAFETY: `GetStdHandle` 只读本进程的 std 槽，不涉及内存安全。
                // 没有控制台且没有重定向时它返回 Err 或一个无效句柄。
                let Ok(h) = (unsafe { GetStdHandle(id) }) else {
                    continue;
                };
                // 一次覆盖 NULL 与 INVALID_HANDLE_VALUE 两个哨兵。
                if h.is_invalid() {
                    continue;
                }
                // stdout 与 stderr 常指向同一个句柄 ⟹ 只处理一次，
                // 否则第二次读到的已经是清干净的值，恢复时反而会把
                // 「本来就不可继承」的句柄改成可继承。
                if restore.iter().any(|(prev, _)| *prev == h) {
                    continue;
                }
                let mut flags = HANDLE_FLAGS(0);
                // SAFETY: `h` 刚由 `GetStdHandle` 返回且已排除两个哨兵值；
                // `&mut flags.0` 是合法的可写指针（`HANDLE_FLAGS` 是
                // `#[repr(transparent)]` 的 `u32` newtype）。
                if unsafe { GetHandleInformation(h, &mut flags.0) }.is_err() {
                    continue;
                }
                let was_inheritable = flags.contains(HANDLE_FLAG_INHERIT);
                if was_inheritable {
                    // SAFETY: 同上；只清 `HANDLE_FLAG_INHERIT` 这一位。
                    if unsafe { SetHandleInformation(h, HANDLE_FLAG_INHERIT.0, HANDLE_FLAGS(0)) }
                        .is_err()
                    {
                        continue; // 清不掉就当没碰过它。
                    }
                }
                restore.push((h, was_inheritable));
            }
            Self { restore }
        }
    }

    impl Drop for NoInheritStdHandles {
        fn drop(&mut self) {
            for (h, was_inheritable) in self.restore.drain(..) {
                if was_inheritable {
                    // SAFETY: 句柄在 `new` 里验证过且仍然有效（std 句柄的
                    // 生命周期由进程持有）；这里只是把位设回去。
                    unsafe {
                        let _ = SetHandleInformation(h, HANDLE_FLAG_INHERIT.0, HANDLE_FLAG_INHERIT);
                    }
                }
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// 下面两个测试都要改**进程级**的 std 句柄可继承位 —— 那是全局状态，
        /// 不是每个测试私有的。并行跑时，一个测试的守卫析构会把位恢复成
        /// `true`，正好插在另一个测试「断言已清掉」的窗口里。
        ///
        /// 实测：不加这把锁时 `cargo test --lib update::` 每 5 次里约 1 次失败
        /// （`--test-threads=1` 则 10/10 通过）。锁用 `unwrap_or_else` 容忍
        /// 中毒：一个测试 panic 之后另一个仍能跑完并报自己的结论。
        static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

        fn serialize() -> std::sync::MutexGuard<'static, ()> {
            SERIAL.lock().unwrap_or_else(|e| e.into_inner())
        }

        /// 读一个句柄当前的可继承位；句柄无效时 `None`。
        fn inheritable(h: HANDLE) -> Option<bool> {
            let mut flags = HANDLE_FLAGS(0);
            // SAFETY: `h` 由调用方保证是本进程的有效句柄；`&mut flags.0`
            // 是合法的可写指针。
            if unsafe { GetHandleInformation(h, &mut flags.0) }.is_err() {
                return None;
            }
            Some(flags.contains(HANDLE_FLAG_INHERIT))
        }

        /// 设一个句柄的可继承位。
        fn set_inheritable(h: HANDLE, yes: bool) -> bool {
            let flags = if yes {
                HANDLE_FLAG_INHERIT
            } else {
                HANDLE_FLAGS(0)
            };
            // SAFETY: 同 `inheritable`，只改 `HANDLE_FLAG_INHERIT` 这一位。
            unsafe { SetHandleInformation(h, HANDLE_FLAG_INHERIT.0, flags) }.is_ok()
        }

        fn stdout_handle() -> Option<HANDLE> {
            // SAFETY: 只读本进程的 std 槽，不涉及内存安全。
            let h = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) }.ok()?;
            (!h.is_invalid()).then_some(h)
        }

        /// 守卫要**真的**把位清掉，并且析构后**真的**恢复。
        ///
        /// 先手工把 stdout 置成可继承，否则（比如句柄本来就不带这个位时）
        /// 守卫什么都不做，测试就成了空转 —— 这个 `assert` 是防空洞的闸门：
        /// 若它失败，说明在当前环境下守卫**根本没生效**，那正是要立刻知道的事。
        #[test]
        fn guard_clears_then_restores_the_inherit_bit() {
            let _serial = serialize();
            let Some(h) = stdout_handle() else {
                panic!("拿不到 stdout 句柄，测试无法进行");
            };
            let original = inheritable(h).expect("读不到 stdout 的句柄信息");
            assert!(
                set_inheritable(h, true),
                "无法把 stdout 置成可继承 —— 守卫在这个环境下必然空转"
            );
            assert_eq!(inheritable(h), Some(true), "前置条件没建立起来");

            {
                let _guard = NoInheritStdHandles::new();
                assert_eq!(
                    inheritable(h),
                    Some(false),
                    "守卫活着的时候可继承位必须已被清掉"
                );
            }

            assert_eq!(
                inheritable(h),
                Some(true),
                "守卫析构后必须把可继承位恢复原状"
            );

            // 还原成进来时的样子，别给后续测试留状态。
            set_inheritable(h, original);
            assert_eq!(inheritable(h), Some(original), "未能还原初始状态");
        }

        /// `new()` 在 std 槽不可用时（无控制台、句柄被关掉）必须安静地
        /// 什么也不做，而不是 panic。
        #[test]
        fn guard_is_harmless_when_there_is_nothing_to_do() {
            let _serial = serialize();
            let guard = NoInheritStdHandles::new();
            // 只断言「没炸」，并确认记录表里没有 NULL 之类的垃圾。
            for (h, _) in &guard.restore {
                assert!(!h.is_invalid(), "记录表里有非法句柄");
            }
        }
    }
}

/// **父进程侧**：派生一个后台子进程去跑检测。立即返回，不等它。
///
/// 子进程的 stdin/stdout/stderr 全部接到 `NUL`：
/// - 它不该污染调用方的输出（Crowbar 会逐行抓 stdout）；
/// - 父进程退出后它还活着，任何继承来的句柄都可能让管道迟迟不关闭。
///
/// Windows 上额外加 `DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP`：
/// 保证子进程不挂在父进程的控制台上（否则关掉控制台会连带杀掉它）。
///
/// ⚠️ **光有 `Stdio::null()` 不够** —— `std` 在 Windows 上仍然会用
/// `bInheritHandles = TRUE` 派生，子进程照样拿到调用方 stdout 管道的一份
/// 副本。所以还要在 `spawn` 前后套一层 `no_inherit::NoInheritStdHandles`
/// （私有模块，所以这里不写成文档链接）。
pub fn spawn_check() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let mut cmd = std::process::Command::new(exe);
    cmd.arg(HIDDEN_SUBCOMMAND)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        /// `DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP`。
        const FLAGS: u32 = 0x0000_0008 | 0x0000_0200;
        cmd.creation_flags(FLAGS);
    }
    // 守卫必须活到 `spawn` 之后 —— 绑到具名变量（`let _ = ` 会立刻析构）。
    #[cfg(windows)]
    let _guard = no_inherit::NoInheritStdHandles::new();
    // 只 spawn、不 wait —— 这就是「后台」的全部含义。
    let _ = cmd.spawn();
}

/// **入口钩子**：读缓存并（必要时）报一行 + 派生检测。
///
/// 开销：读一个约 100 字节的文件（微秒级）。**只有缓存过期时**才会
/// 额外派生一个进程 —— 也就是 24 小时内最多一次。
///
/// 由 `main` 在最开头调用，早于 clap 解析，所以 mdlc 自有形态与
/// 官方兼容形态**都**享受同一套行为。
pub fn startup() {
    if is_disabled() {
        return;
    }
    let t0 = std::time::Instant::now();
    let cache = read_cache();
    if let Some(c) = &cache
        && let Some(msg) = notice(c, env!("CARGO_PKG_VERSION"))
    {
        log::info!("{msg}");
    }
    let spawned = should_check(cache.as_ref(), now_unix());
    if spawned {
        spawn_check();
    }
    if is_debug() {
        eprintln!(
            "mdlc 更新检测：startup 耗时 {:.1} ms（{}）",
            t0.elapsed().as_secs_f64() * 1000.0,
            if spawned {
                "已派生子进程"
            } else {
                "命中缓存，未派生"
            }
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 造一个缓存，`last_check` 由调用方给。
    fn cache(last_check: u64, latest: &str) -> Cache {
        Cache {
            last_check,
            latest: latest.to_string(),
        }
    }

    #[test]
    fn parse_version_accepts_the_forms_github_hands_out() {
        assert_eq!(parse_version("v0.1.0"), Some((0, 1, 0)));
        assert_eq!(parse_version("0.1.0"), Some((0, 1, 0)));
        assert_eq!(parse_version("v12.34.56"), Some((12, 34, 56)));
        // 预发布 / 构建元数据后缀被截掉。
        assert_eq!(parse_version("v1.2.3-rc1"), Some((1, 2, 3)));
        assert_eq!(parse_version("v1.2.3+build7"), Some((1, 2, 3)));
        // 缺段按 0 补。
        assert_eq!(parse_version("v2"), Some((2, 0, 0)));
        assert_eq!(parse_version("v2.5"), Some((2, 5, 0)));
        // 不是版本号的一律 None。
        assert_eq!(parse_version("latest"), None);
        assert_eq!(parse_version(""), None);
        assert_eq!(parse_version("v"), None);
        assert_eq!(parse_version("v1.x.0"), None);
    }

    #[test]
    fn is_newer_only_fires_on_a_strictly_greater_number() {
        assert!(is_newer("v0.2.0", "0.1.0"));
        assert!(is_newer("v0.1.1", "0.1.0"));
        assert!(is_newer("v1.0.0", "0.99.99"));
        // 相等 ⟹ 不报（含「同一个 tag 带不带 v」）。
        assert!(!is_newer("v0.1.0", "0.1.0"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        // 更旧 ⟹ 不报（开发版跑在旧 tag 上的情形）。
        assert!(!is_newer("v0.0.9", "0.1.0"));
        // 解析不了 ⟹ 保守不报。
        assert!(!is_newer("nightly", "0.1.0"));
        assert!(!is_newer("v0.2.0", "not-a-version"));
        // ⚠️ 预发布版**不会**触发提示（见 `is_newer` 的文档）。
        assert!(!is_newer("v0.2.0-rc1", "0.2.0"));
    }

    #[test]
    fn notice_mentions_both_versions_and_the_url() {
        // 这几条断言在两种语言下都成立（版本号、仓库名、URL 都是值，
        // 不是文案），但仍显式钉住语言 —— 免得日后有人加一条只对
        // 某一门语言成立的断言，测试就随机器语言时红时绿。
        let msg = crate::test_locale::with_english(|| {
            notice(&cache(0, "v0.2.0"), "0.1.0").expect("应触发提示")
        });
        assert!(msg.contains("v0.2.0"), "缺新版本号：{msg}");
        assert!(msg.contains("v0.1.0"), "缺当前版本号：{msg}");
        assert!(msg.contains(REPO), "缺仓库：{msg}");
        assert!(msg.contains("releases/latest"), "缺 URL：{msg}");
    }

    /// 提示文案会跟着语言走（中文系统上不该再看到英文源文）。
    #[test]
    fn notice_is_translated_for_chinese() {
        let msg = crate::test_locale::with_locale("zh-CN", || {
            notice(&cache(0, "v0.2.0"), "0.1.0").expect("应触发提示")
        });
        assert!(msg.contains("有新版本"), "{msg}");
        assert!(msg.contains("v0.2.0"), "占位符要替换：{msg}");
        assert!(msg.contains(REPO), "占位符要替换：{msg}");
        assert!(!msg.contains("%{"), "译文里不该残留未替换的占位符：{msg}");
    }

    #[test]
    fn notice_is_silent_when_there_is_nothing_to_say() {
        assert_eq!(notice(&cache(0, "v0.1.0"), "0.1.0"), None);
        assert_eq!(notice(&cache(0, "v0.0.1"), "0.1.0"), None);
    }

    #[test]
    fn should_check_is_true_without_a_cache_and_false_within_the_interval() {
        let now = 1_700_000_000;
        assert!(should_check(None, now), "没有缓存时必须检测");
        // 刚检测过 ⟹ 不检测。
        assert!(!should_check(Some(&cache(now, "v0.1.0")), now));
        assert!(!should_check(Some(&cache(now - 60, "v0.1.0")), now));
        // 恰好到间隔 ⟹ 检测（`>=` 而不是 `>`）。
        assert!(should_check(
            Some(&cache(now - CHECK_INTERVAL.as_secs(), "v0.1.0")),
            now
        ));
        assert!(should_check(
            Some(&cache(now - CHECK_INTERVAL.as_secs() - 1, "v0.1.0")),
            now
        ));
    }

    #[test]
    fn should_check_survives_a_clock_that_went_backwards() {
        // 缓存里的时间在未来（改过系统时钟 / 从别的机器拷来的缓存）
        // ⟹ `saturating_sub` 给 0 ⟹ 不检测，而不是溢出 panic。
        let now = 1_000;
        assert!(!should_check(Some(&cache(u64::MAX, "v0.1.0")), now));
    }

    #[test]
    fn cache_round_trips_through_json() {
        let c = cache(1_700_000_000, "v9.9.9");
        let text = serde_json::to_string(&c).expect("序列化");
        let back: Cache = serde_json::from_str(&text).expect("反序列化");
        assert_eq!(c, back);
    }

    #[test]
    fn cache_path_ends_with_the_module_directory_and_file() {
        // 只在能取到平台缓存目录的机器上断言（Windows 有 LOCALAPPDATA，
        // Linux/macOS 有 HOME，CI 上两者都有）。
        if let Some(p) = cache_path() {
            assert!(p.ends_with(PathBuf::from("mdlc").join(CACHE_FILE)), "{p:?}");
        }
    }
}
