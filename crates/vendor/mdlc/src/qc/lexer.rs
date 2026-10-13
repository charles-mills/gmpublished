//! QC 词法层 —— `scriplib.cpp` 的忠实移植。

use std::path::{Path, PathBuf};

use super::QcError;

/// `MAXTOKEN`（`scriplib.h:27`）。
pub const MAX_TOKEN: usize = 1024;

/// `MAX_INCLUDES`（`scriplib.cpp:39`）。
pub const MAX_INCLUDES: usize = 16;

/// 一个 token。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    /// token 文本（引号已剥掉）。
    pub text: String,
    /// 该 token 是否来自**引号**包裹。
    ///
    /// 官方只把「剥引号后的字符串」放进 `token[]`，**不留来源信息**。
    /// 但两处语义需要它：
    ///
    /// * `$modelname` 的 `token[0] == '/' || token[0] == '\\'` 判断
    ///   （`studiomdl.cpp:895`）—— 对引号内容同样成立；
    /// * 我们的 TOML 写出需要知道哪些名字该加引号。
    ///
    /// 记录它**不改变**任何解析分支（所有分支只看 `text`），
    /// 所以不构成与官方的行为差异。
    pub quoted: bool,
    /// 该 token 所在文件（用于报错）。
    pub file: String,
    /// 该 token 所在行号（从 1 起）。
    pub line: usize,
}

impl Token {
    /// token 的首字节（官方到处写 `token[0]`）。
    ///
    /// 空 token 返回 `'\0'` —— 官方 `token` 是 NUL 结尾的 C 串，
    /// 空串的 `token[0]` 就是 `'\0'`。
    pub fn first(&self) -> u8 {
        *self.text.as_bytes().first().unwrap_or(&0)
    }
}

/// 一个脚本栈帧（`script_t`，`scriplib.cpp:26-37`）。
struct Frame {
    /// 展示用文件名（官方是 `ExpandPath` 后的绝对路径）。
    filename: String,
    /// 文件内容。
    ///
    /// ⚠️ **必须以 `\0` 结尾**。官方 `GetToken` 的 `skipspace` 循环
    /// （`while (*script->script_p <= 32)`）**没有边界检查** —— 它依赖
    /// `LoadFile` 分配的 `buffer[length] = 0` 来终止（`cmdlib.cpp` 的
    /// `((char *)buffer)[length] = 0;`）。少了这个 NUL，越界读会让
    /// 循环停不下来。这里用 `Vec<u8>` 且保证末尾有 0。
    buffer: Vec<u8>,
    /// 读指针（字节下标）。
    pos: usize,
    /// 当前行号（从 1 起）。
    line: usize,
    /// `$definemacro` 定义的形参名（`macroparam[]`）。
    macro_params: Vec<String>,
    /// 本次展开的实参值（`macrovalue[]`），与 `macro_params` 等长。
    macro_values: Vec<String>,
}

impl Frame {
    fn new(filename: String, text: &[u8]) -> Self {
        // 剥掉 UTF-8 BOM（`EF BB BF`）。
        //
        // # 为什么必须剥（oracle 实测）
        //
        // `docs/_probe/smdl/phyname_ref.qc` 是**唯一**一个带 BOM 的用例
        // （853 个里 1 个），官方 studiomdl **正常编译**（1720 字节）。
        //
        // 不剥的后果：BOM 的 `EF BB BF` 三个字节都 `> 32`，会被当成
        // 普通 token 字节，于是首个 token 变成 `"\u{feff}$modelname"`
        // —— 报「未知的 QC 命令」，而官方不报。
        //
        // 注：官方 `scriplib.cpp` 并不显式处理 BOM，但它的
        // `skipspace` 是 `while (*script_p <= 32)` —— `0xEF` 是**有符号
        // char 的负值**（`-17`），在 C 里 `-17 <= 32` 成立，
        // 所以 BOM 被**当成空白**跳过了。Rust 用 `u8` 比较（`239 > 32`）
        // 不会跳过，所以要显式剥。
        let text = text.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(text);
        let mut buffer = text.to_vec();
        // 官方 `LoadFile` 无条件补一个 NUL（分配 length+1）。
        buffer.push(0);
        Self {
            filename,
            buffer,
            pos: 0,
            line: 1,
            macro_params: Vec::new(),
            macro_values: Vec::new(),
        }
    }

    /// 当前字节（越界返回 0，与官方读 NUL 一致）。
    fn cur(&self) -> u8 {
        *self.buffer.get(self.pos).unwrap_or(&0)
    }

    /// 下一个字节（越界返回 0）。
    fn peek1(&self) -> u8 {
        *self.buffer.get(self.pos + 1).unwrap_or(&0)
    }

    /// 是否已到有效内容末尾。
    ///
    /// 官方的 `script_p >= end_p` 用的是**不含 NUL 的长度**，
    /// 所以这里用 `buffer.len() - 1`。
    fn at_end(&self) -> bool {
        self.pos >= self.buffer.len().saturating_sub(1)
    }
}

/// 一条已定义宏（`script_t` 复用，`scriplib.cpp:104`）。
struct Macro {
    name: String,
    /// 宏体原文（含末尾换行，官方是从 `script_p` 截到行尾）。
    body: Vec<u8>,
    /// 形参名。
    params: Vec<String>,
    /// 定义处的行号（展开后行号沿用）。
    line: usize,
}

/// 一条已定义变量（`variable_t`，L4D2 `scriplib.cpp:49-53`）。
struct Variable {
    param: String,
    value: String,
}

/// QC 词法器。
pub struct Lexer {
    /// 脚本栈。`stack[0]` 永远是主 QC。
    stack: Vec<Frame>,
    /// `qdir` —— **主 QC 所在目录**，`$include` 与网格路径的基准。
    pub qdir: PathBuf,
    pub search_paths: Vec<PathBuf>,
    /// 已定义宏。
    macros: Vec<Macro>,
    /// 已定义变量。
    variables: Vec<Variable>,
    /// `unget` 缓存（`tokenready`，`scriplib.cpp:46`）。
    pending: Option<Token>,
    /// 是否已到脚本末尾（`endofscript`）。
    pub end_of_script: bool,
    /// 所有被加载过的文件（含 `$include`），用于依赖追踪/测试。
    pub loaded_files: Vec<String>,
}

impl Lexer {
    /// 新建一个词法器。`qdir` 是 `$include` 与网格路径的基准目录。
    pub fn new(qdir: PathBuf) -> Self {
        Self {
            stack: Vec::new(),
            qdir,
            search_paths: Vec::new(),
            macros: Vec::new(),
            variables: Vec::new(),
            pending: None,
            end_of_script: false,
            loaded_files: Vec::new(),
        }
    }

    /// 从磁盘加载主 QC（对应 `LoadScriptFile`）。
    pub fn load(&mut self, path: &Path) -> Result<(), QcError> {
        let bytes = std::fs::read(path).map_err(|e| {
            QcError::new(
                path.display().to_string(),
                0,
                crate::tr_fmt!("cannot read the QC file: %{err}", err = e),
            )
        })?;
        let name = path.display().to_string();
        self.loaded_files.push(name.clone());
        self.stack.clear();
        self.stack.push(Frame::new(name, &bytes));
        self.end_of_script = false;
        self.pending = None;
        Ok(())
    }

    /// 从内存加载（单元测试 / 字符串输入）。
    pub fn load_from_memory(&mut self, text: &str, name: &str) {
        self.stack.clear();
        self.stack
            .push(Frame::new(name.to_string(), text.as_bytes()));
        self.end_of_script = false;
        self.pending = None;
    }

    /// 当前帧。
    fn frame(&self) -> &Frame {
        self.stack.last().expect("脚本栈不应为空")
    }

    fn frame_mut(&mut self) -> &mut Frame {
        self.stack.last_mut().expect("脚本栈不应为空")
    }

    /// 当前 `(文件名, 行号)`，用于报错。
    pub fn location(&self) -> (String, usize) {
        match self.stack.last() {
            Some(f) => (f.filename.clone(), f.line),
            None => (self.qdir.display().to_string(), 0),
        }
    }

    /// 构造一条带当前位置的错误。
    pub fn error(&self, message: impl Into<String>) -> QcError {
        let (file, line) = self.location();
        QcError::new(file, line, message)
    }

    /// 把 token 放回（`UnGetToken`，`scriplib.cpp:330`）。
    pub fn unget(&mut self, tok: Token) {
        self.pending = Some(tok);
    }

    /// 取下一个 token（`GetToken`，`scriplib.cpp:365`）。
    ///
    /// `crossline = false` 时越过行尾会报错
    /// （官方 `Error("Line %i is incomplete")`）。
    ///
    /// 返回 `Ok(None)` 表示**脚本正常结束**（官方 `endofscript` 为真）。
    /// 这不是错误 —— `ParseScript` 正是靠它退出主循环
    /// （`studiomdl.cpp:6737`：`if (endofscript) return;`）。
    pub fn next_token(&mut self, crossline: bool) -> Result<Option<Token>, QcError> {
        if let Some(t) = self.pending.take() {
            return Ok(Some(t));
        }
        loop {
            if self.stack.is_empty() {
                self.end_of_script = true;
                return Ok(None);
            }
            match self.scan_token(crossline)? {
                Some(t) => return Ok(Some(t)),
                None => {
                    if self.end_of_script {
                        return Ok(None);
                    }
                    // `$include` / 宏 / 变量已展开，或刚弹出一层 include，
                    // 两种情况都继续取下一个 token。
                    continue;
                }
            }
        }
    }

    /// 取一个 token，**不允许**脚本结束（脚本结束时给出可读的错误）。
    ///
    /// 解析层几乎总是用这个 —— 官方在需要参数处直接调 `GetToken(false)`，
    /// 越界会走 `Error("Line %i is incomplete")`。
    pub fn expect_token(&mut self, crossline: bool) -> Result<Token, QcError> {
        match self.next_token(crossline)? {
            Some(t) => Ok(t),
            None => Err(self.error(crate::tr(
                "the script ended unexpectedly (missing argument)",
            ))),
        }
    }

    /// `TokenAvailable()`（`scriplib.cpp:730`）。
    ///
    /// 判断「本行还有 token」—— 跳过一切 `<= 32` 的字节，遇 `'\n'` 返回 false。
    pub fn token_available(&mut self) -> bool {
        if self.pending.is_some() {
            return true;
        }
        let Some(f) = self.stack.last() else {
            return false;
        };
        let mut p = f.pos;
        if p >= f.buffer.len().saturating_sub(1) {
            return false;
        }
        while f.buffer.get(p).copied().unwrap_or(0) <= 32 {
            if f.buffer.get(p).copied().unwrap_or(0) == b'\n' {
                return false;
            }
            p += 1;
            if p >= f.buffer.len().saturating_sub(1) {
                return false;
            }
        }
        // `;` / `#` / `//` 起注释 ⟹ 本行没有更多 token。
        let c = f.buffer.get(p).copied().unwrap_or(0);
        if c == b';' || c == b'#' {
            return false;
        }
        if c == b'/' && f.buffer.get(p + 1).copied().unwrap_or(0) == b'/' {
            return false;
        }
        true
    }

    /// 扫一个 token。
    ///
    /// 返回 `Ok(None)` 表示「本次没有产出 token，但也没错」——
    /// 即碰到了 `$include` / 宏 / 变量展开，调用方应继续循环。
    fn scan_token(&mut self, crossline: bool) -> Result<Option<Token>, QcError> {
        // ⚠️ 官方用 `goto skipspace`（单行注释）与**递归调用**（块注释）
        // 表达「跳过注释后重来」；本实现写成 `loop` + `continue`。
        //
        // # 为什么必须写成循环（R37）
        //
        // 曾写成 `return self.scan_token(crossline);` —— 那是**尾调用**，
        // 但 Rust **不做尾调用消除**。debug 构建下每次递归压一个
        // `0x11a0`（4512）字节的栈帧，1 MB 默认栈只够约 232 层
        // ⟹ **连续 220 行注释就爆栈**（`c00000fd`，实测阈值恰好 220；
        // 见 `docs/_probe/stack_threshold.js`）。
        //
        // 真实触发源：用户工程的 `lods.qci` 前 **259 行全是注释**
        // （整个 `$lod 10 { … }` 块被注释掉）⟹ debug 下
        // `mdlc qc2toml survivor_teenangst.qc` 3 秒即崩。
        // release 的栈帧小得多，恰好躲过 —— 所以这个缺陷**只在 debug
        // 二进制上显形**，release 冒烟测试发现不了它。
        loop {
            // ---- skipspace（scriplib.cpp:385）----
            //
            // ⚠️ 只对 `'\n'` 递增行号；`'\r'` 是**普通空白**。
            loop {
                if self.frame().at_end() {
                    return self.end_of_script_inner(crossline).map(|_| None);
                }
                let c = self.frame().cur();
                if c > 32 {
                    break;
                }
                self.frame_mut().pos += 1;
                if c == b'\n' {
                    if !crossline {
                        return Err(self.error(crate::tr(
                            "incomplete line (missing argument at the end of the line)",
                        )));
                    }
                    let f = self.frame_mut();
                    f.line += 1;
                }
            }

            if self.frame().at_end() {
                return self.end_of_script_inner(crossline).map(|_| None);
            }

            // ---- 单行注释（scriplib.cpp:408）----
            //
            // `;` `#` `//` 三者都起注释。注意 `#` 与 `/` **不**结束 token，
            // 只有 `;` 会（见下面的常规 token 扫描条件）。
            let c = self.frame().cur();
            let is_comment = c == b';' || c == b'#' || (c == b'/' && self.frame().peek1() == b'/');
            if is_comment {
                if !crossline {
                    return Err(
                        self.error(crate::tr("incomplete line (the line ends in a comment)"))
                    );
                }
                // `while (*script->script_p++ != '\n')` —— 注意是**先取后判**，
                // 所以 `\n` 本身也被消费掉。
                loop {
                    let ch = self.frame().cur();
                    let at_end = self.frame().at_end();
                    self.frame_mut().pos += 1;
                    if ch == b'\n' {
                        break;
                    }
                    if at_end {
                        return self.end_of_script_inner(crossline).map(|_| None);
                    }
                }
                self.frame_mut().line += 1;
                // `goto skipspace`
                continue;
            }

            // ---- 块注释（scriplib.cpp:425）----
            //
            // ⚠️ 官方这段的换行计数**写错了**：
            // ```c
            // if (*script->script_p++ != '\n')
            //     scriptline = ++script->line;
            // ```
            // 是「**不是**换行时才递增行号」—— 与直觉相反。
            // 这里**故意修正**为「是换行时递增」，因为：
            // ① 它是上游 bug，官方自己的行号在块注释后就飘了；
            // ② 行号只用于报错，不影响任何解析分支。
            // 记在这里以免将来「对照源码时发现不一致」又被改回去。
            if c == b'/' && self.frame().peek1() == b'*' {
                self.frame_mut().pos += 2;
                loop {
                    if self.frame().at_end() {
                        return self.end_of_script_inner(crossline).map(|_| None);
                    }
                    let a = self.frame().cur();
                    let b = self.frame().peek1();
                    if a == b'*' && b == b'/' {
                        self.frame_mut().pos += 2;
                        break;
                    }
                    self.frame_mut().pos += 1;
                    if a == b'\n' {
                        self.frame_mut().line += 1;
                    }
                }
                continue;
            }

            break;
        }

        // 位置在注释跳过后重算。与「原实现在注释处理**之前**取一次」对
        // 真实 token 完全等价：注释分支原先直接递归返回，那份
        // `(file, line)` 被丢弃；`filename` 在跳注释期间不会变。
        let (file, line) = self.location();

        // ---- 拷贝 token（scriplib.cpp:543）----
        //
        // ⚠️ 收集用的是 **`Vec<u8>`**，不是 `String`。
        // 官方 `token` 是 `char[]`（字节数组），而 QC 里确实有非 ASCII
        // （中文注释、带重音的材质名）。按 byte 收集、最后一次性
        // 按 UTF-8 解码，多字节序列才不会被破坏。
        let mut out: Vec<u8> = Vec::new();
        let quoted;

        if self.frame().cur() == b'"' {
            // 引号 token：官方**没有**处理转义（`while (*p != '"')`），
            // 也不把 `\` 当转义符。照抄。
            quoted = true;
            self.frame_mut().pos += 1;
            loop {
                if self.frame().at_end() {
                    break;
                }
                let ch = self.frame().cur();
                if ch == b'"' {
                    self.frame_mut().pos += 1;
                    break;
                }
                if out.len() >= MAX_TOKEN {
                    return Err(
                        self.error(crate::tr_fmt!("token too long (>%{max})", max = MAX_TOKEN))
                    );
                }
                out.push(ch);
                self.frame_mut().pos += 1;
            }
        } else {
            quoted = false;
            // 常规 token：`> 32 && != ';'`
            loop {
                if self.frame().at_end() {
                    break;
                }
                let ch = self.frame().cur();
                if ch <= 32 || ch == b';' {
                    break;
                }
                // 宏参数展开（`ExpandMacroToken`）优先于变量展开。
                if self.try_expand_macro(&mut out)? {
                    continue;
                }
                if self.try_expand_variable(&mut out)? {
                    continue;
                }
                if out.len() >= MAX_TOKEN {
                    return Err(
                        self.error(crate::tr_fmt!("token too long (>%{max})", max = MAX_TOKEN))
                    );
                }
                out.push(ch);
                self.frame_mut().pos += 1;
            }
        }

        let text = String::from_utf8_lossy(&out).into_owned();

        if quoted {
            return Ok(Some(Token {
                text,
                quoted,
                file,
                line,
            }));
        }

        // ---- 词法器内部命令（scriplib.cpp:580）----
        if text.eq_ignore_ascii_case("$include") {
            let name = self.expect_token(false)?;
            self.push_include(&name)?;
            return Ok(None);
        }
        if text.eq_ignore_ascii_case("$definemacro") {
            let name = self.expect_token(false)?;
            self.define_macro(&name)?;
            return Ok(None);
        }
        if text.eq_ignore_ascii_case("$definevariable") {
            let name = self.expect_token(false)?;
            self.define_variable(&name)?;
            return Ok(None);
        }
        if self.push_macro(&text)? {
            return Ok(None);
        }

        Ok(Some(Token {
            text,
            quoted,
            file,
            line,
        }))
    }

    /// `EndOfScript`（`scriplib.cpp:336`）。
    ///
    /// 弹出一层 `$include` 并让调用方**继续取 token**；
    /// 若已在最外层则置 `end_of_script`（正常结束）。
    fn end_of_script_inner(&mut self, crossline: bool) -> Result<(), QcError> {
        if !crossline {
            return Err(self.error(crate::tr("incomplete line (the file ended unexpectedly)")));
        }
        if self.stack.len() <= 1 {
            self.end_of_script = true;
            self.stack.clear();
            return Ok(());
        }
        self.stack.pop();
        // 官方此处 `return GetToken(crossline);` —— 弹栈后继续读上一层。
        Ok(())
    }

    /// `AddScriptToStack`（`scriplib.cpp:53`）。
    ///
    /// ⚠️ 路径基准是 **`qdir`（主 QC 目录）**，不是当前文件目录、
    /// 也不是 `$pushd` 之后的目录 —— 见 [`super`] 的说明第 3 条。
    fn push_include(&mut self, name: &Token) -> Result<(), QcError> {
        if self.stack.len() + 1 >= MAX_INCLUDES {
            return Err(self.error(crate::tr_fmt!(
                "$include nesting exceeds %{max} levels",
                max = MAX_INCLUDES
            )));
        }
        let normalized = name.text.replace('\\', "/");
        let rel = Path::new(&normalized);
        let mut full = if rel.is_absolute() {
            rel.to_path_buf()
        } else {
            self.qdir.join(rel)
        };
        if !rel.is_absolute()
            && !full.is_file()
            && let Some(candidate) = self
                .search_paths
                .iter()
                .map(|directory| directory.join(rel))
                .find(|path| path.is_file())
        {
            full = candidate;
        }
        let bytes = std::fs::read(&full).map_err(|e| {
            // 官方此处**静默跳过**；mdlc 故意报错（见模块文档）。
            QcError::new(
                name.file.clone(),
                name.line,
                crate::tr_fmt!(
                    "$include cannot find %{name} (%{path}): %{err}",
                    name = name.text,
                    path = full.display(),
                    err = e
                ),
            )
        })?;
        let display = full.display().to_string();
        self.loaded_files.push(display.clone());
        self.stack.push(Frame::new(display, &bytes));
        Ok(())
    }

    /// `DefineMacro`（`scriplib.cpp:132-198`）。
    ///
    /// ⚠️ **`\\` 是宏体的续行符。** 它不是通用续行符 —— 只在这一个函数里
    /// 生效（`Option_Flexrule` 的 `\\` 是另一套机制，见模块文档）。
    ///
    /// 官方流程（逐字对照）：
    ///
    /// ```c
    /// char *cp = script->script_p;
    /// while (TokenAvailable()) {
    ///     GetToken(false);
    ///     if (token[0] == '\\' && token[1] == '\\') break;
    ///     cp = script->script_p;          // ← 只在**非** `\\` 时前进
    ///     pmacro->macroparam[...] = ...;
    /// }
    /// script->script_p = cp;              // ← 回退到 `\\` 之前
    /// while (*cp && *cp != '\n') {
    ///     if (*cp == '\\' && *(cp+1) == '\\') {
    ///         while (*cp && *cp != '\n') { *cp = ' '; cp++; }  // 原地改空格
    ///         if (*cp) { cp++; }                                // 跨过换行
    ///     } else { cp++; }
    /// }
    /// ```
    ///
    /// 语义：宏体从**最后一个形参之后**开始；遇到 `\\` 就把「`\\` 到行尾」
    /// 全替换成**空格**并**跨过**换行继续，直到第一个**没有** `\\` 的换行。
    /// 那个换行本身**保留在体内**（`size = cp - script->script_p` 已含它）。
    ///
    /// ⚠️ **修复前 mdlc 只取「本行剩余」**（`:552-558` 的注释还声称两者
    /// 等价 —— 该声称**不成立**）。于是
    /// `$definemacro IncapAimMacro FileName \\` 的体为空 ⟹
    /// `anims_fix.qci:173`/`:174` 两次调用展开成空 ⟹ **26 条
    /// `$animation` 与 2 个 `$sequence` 静默消失**，随后 `anim_fix.qc:35`
    /// 的 `$continue` 指向不存在的序列（报 `unknown continue animation`）。
    ///
    /// ⚠️ 注意行首的 `+` 会被 Markdown 当成列表标记（clippy 的
    /// `doc_lazy_continuation`），所以这里把它写成「与」。
    ///
    /// ⚠️ 一处**故意偏离**：官方的手工扫描**不更新** `script->line`，所以
    /// 多行宏之后的官方行号是偏的。mdlc 把跨过的换行计入 `f.line`。
    fn define_macro(&mut self, name: &Token) -> Result<(), QcError> {
        let mut params = Vec::new();
        // 官方 `cp` 的对应物：**最后一次成功取到形参之后**的位置
        // （即 `\\` 之前）。取不到任何形参时它停在名字之后。
        let f0 = self.frame_mut();
        let mut body_start = f0.pos;
        let def_line = f0.line;
        loop {
            if !self.token_available() {
                break;
            }
            let t = self.expect_token(false)?;
            // `\\` 终止形参表，且**不**推进 `body_start`（`scriplib.cpp:122`）。
            if t.text == "\\\\" {
                break;
            }
            params.push(t.text);
            body_start = self.frame_mut().pos;
        }
        let f = self.frame_mut();
        f.pos = body_start;
        let len = f.buffer.len();
        let mut cp = body_start;
        let mut newlines = 0usize;
        while cp < len && f.buffer[cp] != 0 && f.buffer[cp] != b'\n' {
            if f.buffer[cp] == b'\\' && f.buffer.get(cp + 1) == Some(&b'\\') {
                // 「`\\` 到行尾」原地改空格（官方 `*cp = ' '`）。
                while cp < len && f.buffer[cp] != 0 && f.buffer[cp] != b'\n' {
                    f.buffer[cp] = b' ';
                    cp += 1;
                }
                if cp < len && f.buffer[cp] == b'\n' {
                    cp += 1;
                    newlines += 1;
                }
            } else {
                cp += 1;
            }
        }
        let body = f.buffer[body_start..cp].to_vec();
        f.pos = cp;
        f.line += newlines;
        self.macros.push(Macro {
            name: name.text.clone(),
            body,
            params,
            line: def_line,
        });
        Ok(())
    }

    /// `DefineVariable`（L4D2 `scriplib.cpp:201`）。
    ///
    /// ⚠️ **这里故意偏离官方**：官方是
    /// `g_definevariable.AddToTail( v )`（`scriplib.cpp:211`）—— **追加**，
    /// 而查找是 `for (…) if (Q_strnicmp(…) == 0) break;`（`:361-370`）
    /// —— 取**第一个**命中 ⟹ **「先到先得」，重复定义不覆盖**。
    /// 真 studiomdl 裁决（`docs/_probe/oracle_definevariable.js` 变体 B）：
    /// `$definevariable Name first` + `$definevariable Name second`
    /// ⟹ 产出 `first.mdl`。
    ///
    /// 但 NekoMDL 生态把变量当「可重定义」用，并为此发明了扩展命令
    /// `$redefinevariable` —— 官方**没有**该命令（exe 串扫描 0 命中，
    /// 且官方对它报 `bad command $redefinevariable`，见
    /// `docs/_probe/oracle_redefinevariable.js`）。
    /// 用户明确要求：「不要支持 `$redefinevariable`，只需让
    /// `$definevariable` 也能覆盖已经定义的变量即可」⟹ 此处按**覆盖**语义
    /// 实现：同名（大小写不敏感）则**原地替换值**。
    ///
    /// ⚠️ 必须是**原地**替换，不能「删掉再追加」：查找是按 `len - 2` 个字符
    /// 的**前缀**比较（上游 bug，见 [`super`] 的说明），下标决定谁先命中。
    /// 例：`scale`(0) + `sca`(1) 时 `$sca$` 只比 1 个字符 `s` ⟹ 命中 `scale`；
    /// 若把 `scale` 移到尾部，`$sca$` 就会改命中 `sca` ⟹ **静默改变产物**。
    ///
    /// 影响面：parity 语料 901 个 QC + 用户工程 14 个 QC 里同名重复定义
    /// **0 处**（`docs/_probe/dup_vars.js`）⟹ 对既有产物零影响。
    fn define_variable(&mut self, name: &Token) -> Result<(), QcError> {
        let value = self.expect_token(false)?;
        let key = name.text.to_ascii_lowercase();
        if let Some(v) = self
            .variables
            .iter_mut()
            .find(|v| v.param.to_ascii_lowercase() == key)
        {
            v.value = value.text;
            return Ok(());
        }
        self.variables.push(Variable {
            param: name.text.clone(),
            value: value.text,
        });
        Ok(())
    }

    /// `AddMacroToStack`（`scriplib.cpp:220`）。
    fn push_macro(&mut self, name: &str) -> Result<bool, QcError> {
        if !name.starts_with('$') {
            return Ok(false);
        }
        let bare = &name[1..];
        let Some(m) = self
            .macros
            .iter()
            .position(|m| m.name.eq_ignore_ascii_case(bare))
        else {
            return Ok(false);
        };
        if self.stack.len() + 1 >= MAX_INCLUDES {
            return Err(self.error(crate::tr_fmt!(
                "macro expansion nesting exceeds %{max} levels",
                max = MAX_INCLUDES
            )));
        }
        let params = self.macros[m].params.clone();
        let body = self.macros[m].body.clone();
        let line = self.macros[m].line;
        let mut values = Vec::with_capacity(params.len());
        for _ in 0..params.len() {
            let t = self.expect_token(false)?;
            values.push(t.text);
        }
        let mut f = Frame::new(name.to_string(), &body);
        f.macro_params = params;
        f.macro_values = values;
        f.line = line;
        self.stack.push(f);
        Ok(true)
    }

    /// `ExpandMacroToken`（`scriplib.cpp:281`）。
    ///
    /// 返回 `true` 表示**展开成功且已写入 `out`**。
    fn try_expand_macro(&mut self, out: &mut Vec<u8>) -> Result<bool, QcError> {
        let f = self.frame();
        if f.macro_params.is_empty() || f.cur() != b'$' {
            return Ok(false);
        }
        // `while (*cp > 32 && *cp != '$') cp++;`
        let mut cp = f.pos + 1;
        while f.buffer.get(cp).copied().unwrap_or(0) > 32
            && f.buffer.get(cp).copied().unwrap_or(0) != b'$'
        {
            cp += 1;
        }
        if f.buffer.get(cp).copied().unwrap_or(0) != b'$' {
            return Ok(false);
        }
        let name_start = f.pos + 1;
        let name = String::from_utf8_lossy(&f.buffer[name_start..cp]).into_owned();
        let Some(idx) = f
            .macro_params
            .iter()
            .position(|p| p.eq_ignore_ascii_case(&name))
        else {
            let (file, line) = self.location();
            return Err(QcError::new(
                file,
                line,
                crate::tr_fmt!("unknown macro parameter \"%{name}\"", name = name),
            ));
        };
        let value = f.macro_values[idx].clone();
        out.extend_from_slice(value.as_bytes());
        self.frame_mut().pos = cp + 1;
        Ok(true)
    }

    /// `ExpandVariableToken`（L4D2 `scriplib.cpp:338`）。
    ///
    /// ⚠️ **名字匹配只比 `len - 2` 个字符** —— 上游 bug，但它决定实际行为。
    /// 见 [`super`] 说明第 4 条。
    fn try_expand_variable(&mut self, out: &mut Vec<u8>) -> Result<bool, QcError> {
        let f = self.frame();
        if f.cur() != b'$' {
            return Ok(false);
        }
        let mut cp = f.pos + 1;
        while f.buffer.get(cp).copied().unwrap_or(0) > 32
            && f.buffer.get(cp).copied().unwrap_or(0) != b'$'
        {
            cp += 1;
        }
        if f.buffer.get(cp).copied().unwrap_or(0) != b'$' {
            return Ok(false);
        }
        let name_start = f.pos + 1;
        let name = String::from_utf8_lossy(&f.buffer[name_start..cp]).into_owned();
        // `Q_strnicmp(param, tp, len - 2)`：len = cp - tp = name.len()。
        // 注意 `len - 2` 是 **isize**，len < 2 时会变成负数 —— C 的
        // `Q_strnicmp` 收到负数 n 时按 0 处理（不比较任何字符），
        // 于是**第一个变量**总命中。这里如实模拟。
        let n = (name.len() as isize - 2).max(0) as usize;
        let prefix: String = name.chars().take(n).collect();
        let found = self.variables.iter().position(|v| {
            let head: String = v.param.chars().take(n).collect();
            head.eq_ignore_ascii_case(&prefix)
        });
        let Some(idx) = found else {
            let (file, line) = self.location();
            return Err(QcError::new(
                file,
                line,
                crate::tr_fmt!("unknown variable \"$%{name}$\"", name = name),
            ));
        };
        let value = self.variables[idx].value.clone();
        out.extend_from_slice(value.as_bytes());
        self.frame_mut().pos = cp + 1;
        Ok(true)
    }
}

#[cfg(test)]
#[path = "lexer_tests.rs"]
mod tests;
