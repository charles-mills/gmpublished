//! QC **flexrule 表达式**解析。

use std::collections::VecDeque;

use crate::model::{FlexOp, FlexOpKind, FlexRule};
use crate::qc::QcError;
use crate::qc::lexer::{Lexer, Token};

/// `MAX_OPS`（`studiomdl.h:945`）—— `stream[]` / `stack[]` / `op[]`
/// 的定长容量。C 里超了就是缓冲区溢出，这里用来报错。
const MAX_OPS: usize = 512;

/// 一个**表达式** token（`GetExprToken` 的产物）。
///
/// 与 [`Token`] 的区别：它已经按 C 运算符规则切过，
/// 所以 `%b*0.5` 会变成 `%` `b` `*` `0` `.` `5` 六个。
///
/// 不记录「是否来自引号」—— C 的 `Option_Flexrule` 只看 `token[]`
/// 的内容，引号信息在 `GetExprToken` 返回前就丢了。
#[derive(Debug, Clone, PartialEq, Eq)]
struct ExprTok {
    /// token 文本。
    text: String,
}

impl ExprTok {
    /// 首字节（官方到处写 `token[0]`；空串给 `0`，与 C 的 NUL 一致）。
    fn first(&self) -> u8 {
        *self.text.as_bytes().first().unwrap_or(&0)
    }
}

/// 运算符优先级（`studiomdl.cpp:3880-3893`）。
///
/// 数值本身不重要，**只有相对大小**参与比较。
/// `EXP` 的表项照抄（值为 3），但没有任何 token 能产生它。
fn precedence(kind: FlexOpKind) -> u8 {
    match kind {
        // `CONST`/`FETCH1`/`FETCH2` 与 `OPEN`/`CLOSE`/`COMMA` 都是 0。
        FlexOpKind::Const
        | FlexOpKind::Fetch1
        | FlexOpKind::Fetch2
        | FlexOpKind::Open
        | FlexOpKind::Close
        | FlexOpKind::Comma => 0,
        FlexOpKind::Add | FlexOpKind::Sub => 1,
        FlexOpKind::Mul | FlexOpKind::Div => 2,
        FlexOpKind::Exp => 3,
        FlexOpKind::Neg => 4,
        FlexOpKind::Max | FlexOpKind::Min => 5,
        // `2way`/`nway`/… 不会出现在中缀流里；给 0 以免 panic。
        _ => 0,
    }
}

/// C 的 `atof` 语义：解析**最长合法前缀**，非法则 0.0。
///
/// 为什么不能直接用 Rust 的 `str::parse`：表达式词法器把
/// `isdigit || .` 连续吞成一个 token，所以 `1.2.3` 是**一个** token。
/// `atof("1.2.3")` 得 1.2（在第二个 `.` 处停），而
/// `"1.2.3".parse::<f32>()` 直接 `Err`。
///
/// 又因为 token 只可能含 `[0-9.]`（`e` 是字母，会另起一个 token），
/// 指数形式到不了这里，所以只需处理「数字 + 可选单个 `.` + 数字」。
fn c_atof_prefix(text: &str) -> f32 {
    let b = text.as_bytes();
    let mut i = 0usize;
    // 符号：`verify_atof` 允许 `-`，但调用点有 `isdigit(token[0])` 守卫，
    // 所以实际到不了。留着不影响结果。
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        i += 1;
    }
    let mut end = i;
    while end < b.len() && b[end].is_ascii_digit() {
        end += 1;
    }
    if end < b.len() && b[end] == b'.' {
        end += 1;
        while end < b.len() && b[end].is_ascii_digit() {
            end += 1;
        }
    }
    // 无有效数字（如 `"."`）时 `atof` 返回 0.0。
    let s = &text[..end];
    s.parse::<f32>()
        .or_else(|_| s.strip_suffix('.').unwrap_or("").parse::<f32>())
        .unwrap_or(0.0)
}

/// 把一个**普通** token 裂成表达式 token，追加到 `out`。
///
/// 见模块文档「我们怎么在不改词法器的情况下实现它」。
fn split_expr_tokens(tok: &Token, out: &mut VecDeque<ExprTok>) {
    // 引号 token 在 `GetExprToken` 里也是整体一个（含空白与 `#`）。
    if tok.quoted {
        out.push_back(ExprTok {
            text: tok.text.clone(),
        });
        return;
    }

    let b = tok.text.as_bytes();
    let mut i = 0usize;
    while i < b.len() {
        // `#` 与 `//` 起注释 —— 在 C 里表达式到此为止（`TokenAvailable()`
        // 返回 false），余下部分被外层当注释。所以截断而不是产出 token。
        if b[i] == b'#' || (b[i] == b'/' && b.get(i + 1) == Some(&b'/')) {
            break;
        }
        let start = i;
        if b[i].is_ascii_alphabetic() || b[i] == b'_' {
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
        } else if b[i].is_ascii_digit() || b[i] == b'.' {
            while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'.') {
                i += 1;
            }
        } else {
            // 单字符 token（运算符、括号、`%`、`\`、`"` …）。
            i += 1;
        }
        // 切点只落在 ASCII 字节上，所以切片一定在 UTF-8 边界。
        out.push_back(ExprTok {
            text: String::from_utf8_lossy(&b[start..i]).into_owned(),
        });
    }
}

/// 表达式 token 流 —— `GetExprToken` 的替身。
///
/// 内部缓存「已从 [`Lexer`] 拉出、但还没被表达式解析消费」的 token。
struct ExprStream<'a> {
    /// 底层词法器。
    lex: &'a mut Lexer,
    /// 待消费的表达式 token。
    buf: VecDeque<ExprTok>,
}

impl<'a> ExprStream<'a> {
    fn new(lex: &'a mut Lexer) -> Self {
        Self {
            lex,
            buf: VecDeque::new(),
        }
    }

    /// 构造一条带当前位置的错误。
    fn error(&self, message: impl Into<String>) -> QcError {
        self.lex.error(message)
    }

    /// `TokenAvailable()` 的等价物。
    ///
    /// ⚠️ 不能只用 `lex.token_available()` —— 普通 token 是**整段**
    /// 被词法器消费掉的，所以当我们只消费了它的前半截（`%b+%c` 里的
    /// `%`）时，词法器的读指针已经越过整段，`token_available()` 会
    /// 错误地报「本行没有了」。缓存非空即等价于 C 里
    /// 「读指针停在 token 中间，`TokenAvailable()` 为真」。
    fn has_more(&mut self) -> bool {
        !self.buf.is_empty() || self.lex.token_available()
    }

    /// 拉一个普通 token 并切分进缓存。
    fn fill(&mut self, crossline: bool) -> Result<(), QcError> {
        let t = self.lex.expect_token(crossline)?;
        split_expr_tokens(&t, &mut self.buf);
        Ok(())
    }

    /// `GetExprToken`：取一个表达式 token。
    fn next(&mut self, crossline: bool) -> Result<ExprTok, QcError> {
        while self.buf.is_empty() {
            self.fill(crossline)?;
        }
        Ok(self.buf.pop_front().expect("循环保证缓存非空"))
    }

    /// `\\` 续行的确认读 —— C 里的 `GetToken(false)`。
    ///
    /// 语义是「**再读一个普通 token**，它的首字符必须是 `\`」。
    /// 两种来源：
    ///
    /// * 缓存里还有东西 —— 说明当前 `\` 与确认字符来自**同一个**
    ///   普通 token（即源码里的 `\\`、`\\foo`）。把它们拼起来正是
    ///   C 的 `GetToken` 会读到的那串（`\\` → `\`，`\\foo` → `\foo`，
    ///   两者首字符都是 `\` ⟹ 合法）。
    /// * 缓存空 —— 真的再拉一个普通 token（源码里的 `\ \`）。
    ///
    /// 无论哪种，读完都**丢弃**该 run 的剩余部分 —— C 的 `GetToken`
    /// 就是把整个 run 吃掉的。
    ///
    /// 反例：`\foo`（单个 `\` 后紧跟字母）拼出 `foo`，首字符不是 `\`
    /// → 报 `unknown expression token '\foo`，与 C 一致。
    fn take_continuation_confirm(&mut self) -> Result<String, QcError> {
        if !self.buf.is_empty() {
            return Ok(self.buf.drain(..).map(|t| t.text).collect());
        }
        Ok(self.lex.expect_token(false)?.text)
    }
}

/// 压栈并检查 `MAX_OPS`。
///
/// C 在这里是 `stack[j++] = …`，超了就是栈溢出（UB）。这里改成报错 ——
/// 两者在「能编译出模型」的输入上不会触发。
fn push_stack(stack: &mut Vec<FlexOp>, op: FlexOp, s: &ExprStream<'_>) -> Result<(), QcError> {
    if stack.len() >= MAX_OPS {
        return Err(s.error(format!("expression too complicated (max {MAX_OPS} ops)")));
    }
    stack.push(op);
    Ok(())
}

/// 把 QC 的 `%<flex> = <中缀表达式>` 解析成后缀 op 序列。
///
/// 对应 `Option_Flexrule`（`studiomdl.cpp:3877-4221`）。
///
/// # 调用约定
///
/// 调用方必须**已经**消费掉 `%<flex>` 那个 token（官方是
/// `token[0] == '%'` 分支里 `Option_Flexrule(pmodel, &token[1])`）。
/// 本函数接着消费**一个** token 当作 `=`（官方 `GetToken(false)`，
/// **不校验**它真的是 `=`），然后读到行尾为止。
///
/// `flex_name` 是 `%` 之后的名字（已剥掉 `%`）。
///
/// # 参数
///
/// * `controllers` —— 已注册的 flexcontroller 名，**按注册顺序**
///   （顺序即下标，`FETCH1` 的下标来自它）。
/// * `flexdescs` —— 已注册的 flexdesc 名，同样按注册顺序
///   （`FETCH2` 用）。名字匹配**大小写不敏感**（C 用 `stricmp`）。
///
/// # 与 C 的一处差异
///
/// C 会先在 `g_flexdesc` 里查 `name`，查不到就
/// `Rule for unknown flex %s`。本函数**不做这个检查** ——
/// 返回的 [`FlexRule::flex`] 就是 `flex_name` 原样，下标解析留给
/// 写出阶段（[`crate::model::FlexRule`] 的字段本来就是**名字**不是
/// 下标）。是否要求「flexdesc 必须先注册」由调用方决定。
pub fn parse_flexrule(
    lex: &mut Lexer,
    flex_name: &str,
    controllers: &[String],
    flexdescs: &[String],
) -> Result<FlexRule, QcError> {
    // ---- `=` ----
    //
    // 官方 `GetToken(false);`，**不检查**读到的是不是 `=`。
    let _eq = lex.expect_token(false)?;

    let mut s = ExprStream::new(lex);

    // ---- 第一遍：token 流 → 中缀 op 流 ----
    //
    // `stream[i]`，对应 C 的 `s_flexop_t stream[MAX_OPS]`。
    let mut stream: Vec<FlexOp> = Vec::new();
    // `while ( linecontinue || TokenAvailable() )`
    let mut linecontinue = false;

    while linecontinue || s.has_more() {
        let tok = s.next(linecontinue)?;
        linecontinue = false;

        if tok.first() == b'\\' {
            // `\\` —— 续行。C 要求紧接着的**普通** token 也以 `\` 开头。
            let confirm = s.take_continuation_confirm()?;
            if !confirm.starts_with('\\') {
                return Err(s.error(format!("unknown expression token '\\{confirm}")));
            }
            linecontinue = true;
            continue;
        }

        if stream.len() >= MAX_OPS {
            return Err(s.error(format!("expression for \"{flex_name}\" too complicated")));
        }

        let none = || FlexOp {
            op: FlexOpKind::Const,
            value: None,
            controller: None,
            flexdesc: None,
        };

        let op = if tok.first() == b'(' {
            FlexOp {
                op: FlexOpKind::Open,
                ..none()
            }
        } else if tok.first() == b')' {
            FlexOp {
                op: FlexOpKind::Close,
                ..none()
            }
        } else if tok.first() == b'+' {
            FlexOp {
                op: FlexOpKind::Add,
                ..none()
            }
        } else if tok.first() == b'-' {
            // ⚠️ 怪癖 1：`i == 0` 时**整段一元判定被跳过**，
            // 所以第一个 `-` 永远是二元 `SUB`。
            let mut kind = FlexOpKind::Sub;
            if let Some(prev) = stream.last() {
                match prev.op {
                    // "it's a unary if it's preceded by a ( + - * / ,"
                    FlexOpKind::Open
                    | FlexOpKind::Add
                    | FlexOpKind::Sub
                    | FlexOpKind::Mul
                    | FlexOpKind::Div
                    | FlexOpKind::Comma => kind = FlexOpKind::Neg,
                    _ => {}
                }
            }
            FlexOp { op: kind, ..none() }
        } else if tok.first() == b'*' {
            FlexOp {
                op: FlexOpKind::Mul,
                ..none()
            }
        } else if tok.first() == b'/' {
            FlexOp {
                op: FlexOpKind::Div,
                ..none()
            }
        } else if tok.first().is_ascii_digit() {
            // ⚠️ 怪癖 2：**只有** `isdigit(token[0])` 走这里。`.5` 到不了。
            FlexOp {
                op: FlexOpKind::Const,
                value: Some(c_atof_prefix(&tok.text)),
                controller: None,
                flexdesc: None,
            }
        } else if tok.first() == b',' {
            FlexOp {
                op: FlexOpKind::Comma,
                ..none()
            }
        } else if tok.text.eq_ignore_ascii_case("max") {
            FlexOp {
                op: FlexOpKind::Max,
                ..none()
            }
        } else if tok.text.eq_ignore_ascii_case("min") {
            FlexOp {
                op: FlexOpKind::Min,
                ..none()
            }
        } else if tok.first() == b'%' {
            // `%name` —— 下一个表达式 token 是 flexdesc 名。
            // 官方用 `GetExprToken(false)`：不跨行。
            let name = s.next(false)?;
            let Some(idx) = flexdescs
                .iter()
                .position(|d| d.eq_ignore_ascii_case(&name.text))
            else {
                return Err(s.error(format!("unknown flex {}", name.text)));
            };
            FlexOp {
                op: FlexOpKind::Fetch2,
                value: None,
                controller: None,
                flexdesc: Some(flexdescs[idx].clone()),
            }
        } else {
            // 裸标识符 —— flexcontroller。首个命中即停（C 的 `break`）。
            let Some(idx) = controllers
                .iter()
                .position(|c| c.eq_ignore_ascii_case(&tok.text))
            else {
                return Err(s.error(format!("unknown controller {}", tok.text)));
            };
            FlexOp {
                op: FlexOpKind::Fetch1,
                value: None,
                controller: Some(controllers[idx].clone()),
                flexdesc: None,
            }
        };

        stream.push(op);
    }

    // ---- 第二遍：调度场算法（中缀 → 后缀）----
    let mut stack: Vec<FlexOp> = Vec::new();
    let mut out: Vec<FlexOp> = Vec::new();

    // 用 `while` + 下标而不是 `for`：`NEG` 分支需要读写 `stream[k+1]`。
    let mut k = 0usize;
    while k < stream.len() {
        if stack.len() >= MAX_OPS {
            return Err(s.error(format!("expression {flex_name} too complicated")));
        }
        let op = stream[k].clone();
        match op.op {
            // 操作数直接进输出。
            FlexOpKind::Const | FlexOpKind::Fetch1 | FlexOpKind::Fetch2 => out.push(op),
            FlexOpKind::Open => push_stack(&mut stack, op, &s)?,
            FlexOpKind::Close => {
                // 弹到 `OPEN` 为止（不吃掉它）。
                while stack.last().is_some_and(|t| t.op != FlexOpKind::Open) {
                    out.push(stack.pop().expect("非空"));
                }
                if stack.is_empty() {
                    return Err(s.error("unmatched closed parentheses"));
                }
                // 丢弃这个 `OPEN`。
                stack.pop();
            }
            FlexOpKind::Comma => {
                // 弹到 `OPEN` 为止（不吃掉它），然后压 `COMMA`。
                while stack.last().is_some_and(|t| t.op != FlexOpKind::Open) {
                    out.push(stack.pop().expect("非空"));
                }
                push_stack(&mut stack, op, &s)?;
            }
            FlexOpKind::Add | FlexOpKind::Sub | FlexOpKind::Mul | FlexOpKind::Div => {
                // 弹掉所有**优先级 ≥** 当前运算符的（`<=` 而非 `<`，
                // 所以同级左结合）。
                while stack
                    .last()
                    .is_some_and(|t| precedence(op.op) <= precedence(t.op))
                {
                    out.push(stack.pop().expect("非空"));
                }
                push_stack(&mut stack, op, &s)?;
            }
            FlexOpKind::Neg => {
                // ⚠️ 怪癖 3：下一个是 `CONST` 就**就地取负并丢弃 NEG**。
                //
                // C 读 `stream[k+1]`；`k` 是最后一个时会越界读未初始化
                // 内存（UB）。这里 `get()` 给 `None` → 走 else 分支压栈。
                if stream.get(k + 1).is_some_and(|n| n.op == FlexOpKind::Const) {
                    let slot = &mut stream[k + 1];
                    slot.value = Some(-slot.value.unwrap_or(0.0));
                } else {
                    push_stack(&mut stack, op, &s)?;
                }
            }
            // ⚠️ 怪癖 4：`MAX`/`MIN` 不弹栈，直接压。
            FlexOpKind::Max | FlexOpKind::Min => push_stack(&mut stack, op, &s)?,
            // `EXP` 及其余不可达（没有 token 能产生它们）。
            _ => {}
        }
        if out.len() >= MAX_OPS {
            return Err(s.error(format!("expression for \"{flex_name}\" too complicated")));
        }
        k += 1;
    }

    // 把栈里剩下的全部弹出。
    while let Some(op) = stack.pop() {
        out.push(op);
        if out.len() >= MAX_OPS {
            return Err(s.error(format!("expression for \"{flex_name}\" too complicated")));
        }
    }

    // ---- 第三遍：吃逗号（怪癖 5）----
    //
    // 遇到 `MAX`/`MIN` 时，它前面必须紧挨着一个 `COMMA`；把那个
    // `COMMA` **替换**成 `MAX`/`MIN`（`j` 不前进），于是逗号消失。
    let mut ops: Vec<FlexOp> = Vec::with_capacity(out.len());
    let mut num_commas: i32 = 0;
    for op in out {
        match op.op {
            FlexOpKind::Max | FlexOpKind::Min => {
                // C 读 `op[j-1]`；`j == 0` 时是 `op[-1]`（UB）。
                // 这里 `last_mut()` 给 `None` → 报 missing comma。
                match ops.last_mut() {
                    Some(last) if last.op == FlexOpKind::Comma => *last = op,
                    _ => return Err(s.error("missing comma")),
                }
                num_commas -= 1;
            }
            FlexOpKind::Comma => {
                num_commas += 1;
                ops.push(op);
            }
            _ => ops.push(op),
        }
    }
    if num_commas != 0 {
        return Err(s.error("too many comma's"));
    }
    if ops.len() > MAX_OPS {
        return Err(s.error(format!("expression {flex_name} too complicated")));
    }

    Ok(FlexRule {
        flex: flex_name.to_string(),
        ops,
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    // ---------------------------------------------------------------
    // 夹具构造：**用显式字段**构造期望值，不复用被测代码的任何 helper，
    // 否则「实现错 + 测试跟着错」会一起跑偏。
    // ---------------------------------------------------------------

    /// `STUDIO_CONST`。
    fn c(v: f32) -> FlexOp {
        FlexOp {
            op: FlexOpKind::Const,
            value: Some(v),
            controller: None,
            flexdesc: None,
        }
    }

    /// `STUDIO_FETCH1`（flexcontroller）。
    fn f1(name: &str) -> FlexOp {
        FlexOp {
            op: FlexOpKind::Fetch1,
            value: None,
            controller: Some(name.to_string()),
            flexdesc: None,
        }
    }

    /// `STUDIO_FETCH2`（flexdesc）。
    fn f2(name: &str) -> FlexOp {
        FlexOp {
            op: FlexOpKind::Fetch2,
            value: None,
            controller: None,
            flexdesc: Some(name.to_string()),
        }
    }

    /// 无操作数的运算符。
    fn o(kind: FlexOpKind) -> FlexOp {
        FlexOp {
            op: kind,
            value: None,
            controller: None,
            flexdesc: None,
        }
    }

    /// 跑一次解析。`%a` 由调用方消费（与官方 `ParseScript` 一致）。
    fn run(text: &str, controllers: &[&str], flexdescs: &[&str]) -> Result<FlexRule, QcError> {
        let mut lex = Lexer::new(PathBuf::from("."));
        lex.load_from_memory(text, "<test>");
        // 证明夹具真的以 `%a` 开头 —— 否则下面的断言可能在看别的东西。
        let head = lex.expect_token(false).expect("夹具应能取到 %name");
        assert_eq!(head.text, "%a", "夹具必须真的以 `%a` 开头");
        let ctrls: Vec<String> = controllers.iter().map(|s| s.to_string()).collect();
        let descs: Vec<String> = flexdescs.iter().map(|s| s.to_string()).collect();
        parse_flexrule(&mut lex, "a", &ctrls, &descs)
    }

    /// **断言 op 序列完全相等**，并先证明夹具非空。
    ///
    /// 项目纪律：「一个恒真的测试比没有测试更危险」。所以这里
    /// ① `want` 必须非空 ② 解析结果必须非空 ③ 必须逐项相等。
    #[track_caller]
    fn expect_ops(text: &str, controllers: &[&str], flexdescs: &[&str], want: &[FlexOp]) {
        assert!(
            !want.is_empty(),
            "测试夹具本身不能是空序列（否则断言恒真）：{text}"
        );
        let r = run(text, controllers, flexdescs)
            .unwrap_or_else(|e| panic!("`{text}` 应解析成功，实际报错：{e}"));
        assert_eq!(r.flex, "a", "flex 名应原样回填");
        assert!(
            !r.ops.is_empty(),
            "`{text}` 必须产出非空 op 序列（空序列会让下面的断言失去意义）"
        );
        assert_eq!(r.ops, want, "`{text}` 的 op 序列不符");
    }

    /// 断言解析**失败**，且错误信息包含 `needle`。
    #[track_caller]
    fn expect_err(text: &str, controllers: &[&str], flexdescs: &[&str], needle: &str) {
        match run(text, controllers, flexdescs) {
            Ok(r) => panic!(
                "`{text}` 应报错，实际得到 {} 个 op：{:?}",
                r.ops.len(),
                r.ops
            ),
            Err(e) => assert!(
                e.message.contains(needle),
                "`{text}` 的错误信息应含 {needle:?}，实际：{}",
                e.message
            ),
        }
    }

    /// 所有测试共用的注册表。
    const CTRLS: &[&str] = &["ctrl"];
    const DESCS: &[&str] = &["b", "c", "d"];

    // ---------------------------------------------------------------
    // 基本形态
    // ---------------------------------------------------------------

    /// `%a = 0.5` → 一个常量。
    #[test]
    fn plain_constant() {
        expect_ops("%a = 0.5\n", CTRLS, DESCS, &[c(0.5)]);
    }

    /// `%a = %b * 0.5` → `b 0.5 *`。
    #[test]
    fn fetch2_times_constant() {
        expect_ops(
            "%a = %b * 0.5\n",
            CTRLS,
            DESCS,
            &[f2("b"), c(0.5), o(FlexOpKind::Mul)],
        );
    }

    /// `%a = %b + %c` → `b c +`。
    #[test]
    fn fetch2_plus_fetch2() {
        expect_ops(
            "%a = %b + %c\n",
            CTRLS,
            DESCS,
            &[f2("b"), f2("c"), o(FlexOpKind::Add)],
        );
    }

    /// 优先级：`*` 高于 `+`，所以 `*` 先输出。
    #[test]
    fn multiplication_binds_tighter_than_addition() {
        expect_ops(
            "%a = %b + %c * 0.5\n",
            CTRLS,
            DESCS,
            &[
                f2("b"),
                f2("c"),
                c(0.5),
                o(FlexOpKind::Mul),
                o(FlexOpKind::Add),
            ],
        );
    }

    /// **左结合**：同级比较用 `<=`，所以先压的先弹 → `b c - d -`。
    #[test]
    fn subtraction_is_left_associative() {
        expect_ops(
            "%a = %b - %c - %d\n",
            CTRLS,
            DESCS,
            &[
                f2("b"),
                f2("c"),
                o(FlexOpKind::Sub),
                f2("d"),
                o(FlexOpKind::Sub),
            ],
        );
    }

    /// 除法同理。
    #[test]
    fn division_is_left_associative() {
        expect_ops(
            "%a = %b / %c / 0.5\n",
            CTRLS,
            DESCS,
            &[
                f2("b"),
                f2("c"),
                o(FlexOpKind::Div),
                c(0.5),
                o(FlexOpKind::Div),
            ],
        );
    }

    // ---------------------------------------------------------------
    // 怪癖 1：`i == 0` 的 `-` 永远是二元 `SUB`
    // ---------------------------------------------------------------

    /// ⚠️ **这是最容易写错的一条。**
    ///
    /// C 的一元判定是
    /// ```c
    /// stream[i].op = STUDIO_SUB;
    /// if (i > 0) { switch (stream[i-1].op) { … stream[i].op = STUDIO_NEG; } }
    /// ```
    /// `i == 0` 时**整个 switch 被跳过**，所以开头的 `-` 保持 `SUB`。
    ///
    /// 于是 `%a = -%b` 是 `[fetch2 b, sub]` —— 一个**语法上就缺左操作数**
    /// 的二元减法，而**不是** `[fetch2 b, neg]`。
    ///
    /// 这是官方的真实行为，不是笔误：任务书里原本假设它是
    /// `[fetch2 b, neg]`，读源码后确认**相反**，此处按源码钉死。
    #[test]
    fn leading_minus_stays_binary_sub_not_neg() {
        expect_ops("%a = -%b\n", CTRLS, DESCS, &[f2("b"), o(FlexOpKind::Sub)]);
    }

    /// 同一条怪癖作用于常量：`%a = -0.5` → `[const 0.5, sub]`，
    /// **不是** `[const -0.5]`（`NEG` 分支根本没被选中，所以不会取负）。
    #[test]
    fn leading_minus_on_constant_is_not_negated() {
        expect_ops("%a = -0.5\n", CTRLS, DESCS, &[c(0.5), o(FlexOpKind::Sub)]);
    }

    // ---------------------------------------------------------------
    // 怪癖 3：NEG 后紧跟 CONST → 就地取负、丢弃 NEG
    // ---------------------------------------------------------------

    /// `%a = 0 + -0.5`：`-` 前面是 `+`（`stream[i-1] == ADD`），
    /// 所以这次**真的**是 `NEG`；它后面紧跟 `CONST`，于是常量就地
    /// 变成 `-0.5`，`NEG` **不压栈也不输出**。
    ///
    /// 结果 `[const 0, const -0.5, add]` —— 中间**没有** `neg`。
    #[test]
    fn neg_before_constant_folds_into_the_constant() {
        expect_ops(
            "%a = 0 + -0.5\n",
            CTRLS,
            DESCS,
            &[c(0.0), c(-0.5), o(FlexOpKind::Add)],
        );
    }

    /// 同上，但取负的是乘法右侧 —— 证明折叠与优先级无关。
    #[test]
    fn neg_folding_works_after_mul() {
        expect_ops(
            "%a = %b * -0.5\n",
            CTRLS,
            DESCS,
            &[f2("b"), c(-0.5), o(FlexOpKind::Mul)],
        );
    }

    /// `NEG` 后面**不是**常量时，它照常压栈并输出。
    /// `%a = %b * -%c` → `b c neg *`。
    #[test]
    fn neg_is_emitted_when_followed_by_a_fetch() {
        expect_ops(
            "%a = %b * -%c\n",
            CTRLS,
            DESCS,
            &[f2("b"), f2("c"), o(FlexOpKind::Neg), o(FlexOpKind::Mul)],
        );
    }

    /// `NEG` 的优先级是 4（高于 `*` 的 2），所以后面的 `*` 会把它
    /// **弹出来**，`%c` 再输出。
    #[test]
    fn neg_is_popped_by_a_lower_precedence_operator() {
        expect_ops(
            "%a = 0 + -%b * %c\n",
            CTRLS,
            DESCS,
            &[
                c(0.0),
                f2("b"),
                o(FlexOpKind::Neg),
                f2("c"),
                o(FlexOpKind::Mul),
                o(FlexOpKind::Add),
            ],
        );
    }

    // ---------------------------------------------------------------
    // 怪癖 4/5：`max` / `min` 与「吃逗号」
    // ---------------------------------------------------------------

    /// `%a = max(%b, %c)` —— 收尾遍把 `COMMA` **替换**成 `MAX`，
    /// 于是逗号从输出里消失：`b c max`。
    #[test]
    fn max_eats_its_comma() {
        expect_ops(
            "%a = max(%b, %c)\n",
            CTRLS,
            DESCS,
            &[f2("b"), f2("c"), o(FlexOpKind::Max)],
        );
    }

    /// `min` 与 `max` 同构。
    #[test]
    fn min_eats_its_comma() {
        expect_ops(
            "%a = min(%b, %c)\n",
            CTRLS,
            DESCS,
            &[f2("b"), f2("c"), o(FlexOpKind::Min)],
        );
    }

    /// `max` / `min` 是**大小写不敏感**关键字（C 用 `stricmp`）。
    #[test]
    fn max_keyword_is_case_insensitive() {
        expect_ops(
            "%a = MAX(%b, %c)\n",
            CTRLS,
            DESCS,
            &[f2("b"), f2("c"), o(FlexOpKind::Max)],
        );
        expect_ops(
            "%a = MiN(%b, %c)\n",
            CTRLS,
            DESCS,
            &[f2("b"), f2("c"), o(FlexOpKind::Min)],
        );
    }

    /// `max` 与别的运算符混用：`MAX` 压栈时会把**优先级 ≥5** 的弹掉，
    /// 但栈里只有 `(`（优先级 0），所以它老老实实待在栈上，
    /// 直到遇 `CLOSE` 才输出。
    #[test]
    fn max_combined_with_arithmetic() {
        expect_ops(
            "%a = max(%b, %c) * 0.5\n",
            CTRLS,
            DESCS,
            &[
                f2("b"),
                f2("c"),
                o(FlexOpKind::Max),
                c(0.5),
                o(FlexOpKind::Mul),
            ],
        );
    }

    /// 嵌套：内层 `min` 的逗号先被吃掉，外层 `max` 同理。
    #[test]
    fn nested_max_eats_both_commas() {
        expect_ops(
            "%a = max(min(%b, %c), %d)\n",
            CTRLS,
            DESCS,
            &[
                f2("b"),
                f2("c"),
                o(FlexOpKind::Min),
                f2("d"),
                o(FlexOpKind::Max),
            ],
        );
    }

    /// **`max` 后面没有逗号 → `missing comma`。**
    ///
    /// `%a = max(%b)` 的输出是 `b max`，收尾遍看到 `MAX` 时
    /// `out[j-1]` 是 `b` 而不是 `COMMA` → 报错。
    #[test]
    fn max_without_comma_is_missing_comma() {
        expect_err("%a = max(%b)\n", CTRLS, DESCS, "missing comma");
    }

    /// **多余的逗号 → `too many comma's`。**
    ///
    /// `max(%b,%c,%d)` 输出两个 `COMMA`，但只有一个 `MAX` 去吃掉其中一个，
    /// 计数残留 1 → 报错。（注意官方消息里那个撇号是真的。）
    #[test]
    fn surplus_comma_is_rejected() {
        expect_err("%a = max(%b, %c, %d)\n", CTRLS, DESCS, "too many comma's");
    }

    /// 不在 `max`/`min` 里的逗号同样残留 → 报错。
    #[test]
    fn bare_comma_is_rejected() {
        expect_err("%a = %b, %c\n", CTRLS, DESCS, "too many comma's");
    }

    // ---------------------------------------------------------------
    // 括号
    // ---------------------------------------------------------------

    /// 括号改变结合顺序：`(b + c) * 0.5` → `b c + 0.5 *`。
    #[test]
    fn parentheses_override_precedence() {
        expect_ops(
            "%a = (%b + %c) * 0.5\n",
            CTRLS,
            DESCS,
            &[
                f2("b"),
                f2("c"),
                o(FlexOpKind::Add),
                c(0.5),
                o(FlexOpKind::Mul),
            ],
        );
    }

    /// 嵌套括号。
    #[test]
    fn nested_parentheses() {
        expect_ops(
            "%a = ((%b + %c) * %d)\n",
            CTRLS,
            DESCS,
            &[
                f2("b"),
                f2("c"),
                o(FlexOpKind::Add),
                f2("d"),
                o(FlexOpKind::Mul),
            ],
        );
    }

    /// 多余的右括号 → `unmatched closed parentheses`。
    #[test]
    fn unmatched_close_paren_is_rejected() {
        expect_err("%a = %b)\n", CTRLS, DESCS, "unmatched closed parentheses");
    }

    /// 未闭合的左括号**不报错** —— C 在收尾时把栈里剩下的全部弹出，
    /// `OPEN` 也一并被当成 op 输出。这是官方的真实行为。
    #[test]
    fn unclosed_open_paren_is_emitted_as_an_op() {
        expect_ops(
            "%a = (%b + %c\n",
            CTRLS,
            DESCS,
            &[f2("b"), f2("c"), o(FlexOpKind::Add), o(FlexOpKind::Open)],
        );
    }

    // ---------------------------------------------------------------
    // flexcontroller（FETCH1）
    // ---------------------------------------------------------------

    /// 裸标识符 = flexcontroller，名字从 `controllers` 里**按解析出的
    /// 下标**回填（保证与注册顺序一致，而不是原样抄 token 文本）。
    #[test]
    fn bare_identifier_is_a_flexcontroller_fetch() {
        expect_ops(
            "%a = ctrl * 0.5\n",
            CTRLS,
            DESCS,
            &[f1("ctrl"), c(0.5), o(FlexOpKind::Mul)],
        );
    }

    /// 控制器名匹配**大小写不敏感**（C 用 `stricmp`），
    /// 但回填的是**注册表里的原名**。
    #[test]
    fn controller_lookup_is_case_insensitive_but_returns_registered_name() {
        expect_ops(
            "%a = CTRL * 0.5\n",
            CTRLS,
            DESCS,
            &[f1("ctrl"), c(0.5), o(FlexOpKind::Mul)],
        );
    }

    /// flexdesc 名同理。
    #[test]
    fn flexdesc_lookup_is_case_insensitive() {
        expect_ops(
            "%a = %B * 0.5\n",
            CTRLS,
            DESCS,
            &[f2("b"), c(0.5), o(FlexOpKind::Mul)],
        );
    }

    /// 下标按**注册顺序**回填 —— 这里 `controllers` 有三个名字，
    /// 取中间的 `mid`，验证回填的是 `mid` 而不是第 0 个。
    #[test]
    fn controller_index_follows_registration_order() {
        expect_ops(
            "%a = mid * 0.5\n",
            &["first", "mid", "last"],
            DESCS,
            &[f1("mid"), c(0.5), o(FlexOpKind::Mul)],
        );
    }

    /// 未知的 flexcontroller → `unknown controller`。
    #[test]
    fn unknown_controller_is_rejected() {
        expect_err("%a = nope * 0.5\n", CTRLS, DESCS, "unknown controller nope");
    }

    /// 未知的 flexdesc → `unknown flex`。
    #[test]
    fn unknown_flex_is_rejected() {
        expect_err("%a = %nope * 0.5\n", CTRLS, DESCS, "unknown flex nope");
    }

    // ---------------------------------------------------------------
    // 怪癖 2：只有 `isdigit(token[0])` 才是常量
    // ---------------------------------------------------------------

    /// `.5` 被表达式词法器切成**一个** token，但因为首字符是 `.`
    /// 而不是数字，C 的 `isdigit(token[0])` 不成立，于是掉进
    /// 「flexcontroller 名字」分支 → `unknown controller .5`。
    ///
    /// （`verify_atof` 自己接受 `.`，但那个分支根本到不了。）
    #[test]
    fn leading_dot_number_is_not_a_constant() {
        expect_err("%a = .5\n", CTRLS, DESCS, "unknown controller .5");
    }

    /// 多个小数点仍然是一个 token，`atof` 取**最长合法前缀**：
    /// `atof("1.2.3") == 1.2`（Rust 的 `parse` 在这里会直接失败，
    /// 所以必须有 [`c_atof_prefix`]）。
    #[test]
    fn malformed_number_takes_the_longest_valid_prefix() {
        expect_ops("%a = 1.2.3\n", CTRLS, DESCS, &[c(1.2)]);
    }

    /// 尾随小数点：`atof("12.") == 12.0`。
    #[test]
    fn trailing_dot_is_accepted() {
        expect_ops("%a = 12.\n", CTRLS, DESCS, &[c(12.0)]);
    }

    /// 整数常量。
    #[test]
    fn integer_constant() {
        expect_ops("%a = 3\n", CTRLS, DESCS, &[c(3.0)]);
    }

    // ---------------------------------------------------------------
    // 表达式切分（`GetExprToken` 的替身）
    // ---------------------------------------------------------------

    /// **没有空格的表达式**也必须切对 —— 这是「普通 token 是整段
    /// 消费掉的」那个坑的直接回归：`%b+%c` 是**一个**普通 token。
    #[test]
    fn expression_without_spaces_is_split_by_c_operator_rules() {
        expect_ops(
            "%a = %b+%c\n",
            CTRLS,
            DESCS,
            &[f2("b"), f2("c"), o(FlexOpKind::Add)],
        );
    }

    /// 连写的常量与运算符：`%b*0.5` → `% b * 0.5` 四个表达式 token。
    #[test]
    fn no_space_between_operator_and_number() {
        expect_ops(
            "%a = %b*0.5\n",
            CTRLS,
            DESCS,
            &[f2("b"), c(0.5), o(FlexOpKind::Mul)],
        );
    }

    /// 字母与数字在同一个普通 token 里也要切开：
    /// `max(%b,%c)` 整段无空格 → `max` `(` `%` `b` `,` `%` `c` `)`。
    #[test]
    fn fully_unspaced_max_call() {
        expect_ops(
            "%a = max(%b,%c)\n",
            CTRLS,
            DESCS,
            &[f2("b"), f2("c"), o(FlexOpKind::Max)],
        );
    }

    /// `#` 起注释：表达式在它之前结束。
    ///
    /// 关键在于 `%b#c` 是**一个**普通 token（`#` 不结束普通 token），
    /// 但 C 的 `TokenAvailable()` 看到 `#` 就返回 false。
    /// 切分器必须在此截断，否则 `#` 会被当成 controller 名而报错。
    #[test]
    fn hash_terminates_the_expression_mid_token() {
        expect_ops("%a = %b#c\n", CTRLS, DESCS, &[f2("b")]);
    }

    /// `//` 同理。
    #[test]
    fn double_slash_terminates_the_expression() {
        expect_ops("%a = %b//c\n", CTRLS, DESCS, &[f2("b")]);
    }

    /// 空格分隔的 `#` 注释（更常见的写法）。
    #[test]
    fn spaced_hash_comment_is_ignored() {
        expect_ops("%a = %b # 随便写点什么\n", CTRLS, DESCS, &[f2("b")]);
    }

    /// 表达式在**行尾**结束 —— 下一行不属于这条规则。
    ///
    /// 这里第二行是一个独立的命令；解析完第一行后，词法器应停在
    /// 第二行的开头。
    #[test]
    fn expression_ends_at_end_of_line() {
        let mut lex = Lexer::new(PathBuf::from("."));
        lex.load_from_memory("%a = %b * 0.5\n$modelname \"x\"\n", "<test>");
        assert_eq!(lex.expect_token(false).unwrap().text, "%a");
        let r = parse_flexrule(&mut lex, "a", &[], &["b".to_string()]).unwrap();
        assert!(!r.ops.is_empty(), "必须产出非空 op 序列");
        assert_eq!(r.ops, vec![f2("b"), c(0.5), o(FlexOpKind::Mul)]);
        // 下一行的命令必须还在。
        //
        // ⚠️ 必须 `crossline = true`：解析结束时读指针停在 `\n` 上，
        // `false` 会直接报「行不完整」（这正是官方 `GetToken(false)`
        // 的行为，所以调用方取下一个命令时用的是跨行读）。
        assert_eq!(lex.expect_token(true).unwrap().text, "$modelname");
    }

    // ---------------------------------------------------------------
    // 续行 `\\`
    // ---------------------------------------------------------------

    /// 两行用 `\\` 接起来 —— 等价于一行。
    ///
    /// 注意源码里是**两个**反斜杠：`GetExprToken` 读第一个，
    /// 随后 `GetToken(false)` 必须再读到一个以 `\` 开头的 token。
    #[test]
    fn backslash_backslash_continues_the_expression() {
        expect_ops(
            "%a = %b + \\\\\n     %c\n",
            CTRLS,
            DESCS,
            &[f2("b"), f2("c"), o(FlexOpKind::Add)],
        );
    }

    /// 续行后还能再接一行（链式 `\\`）。
    #[test]
    fn chained_continuations() {
        expect_ops(
            "%a = %b + \\\\\n     %c * \\\\\n     0.5\n",
            CTRLS,
            DESCS,
            &[
                f2("b"),
                f2("c"),
                c(0.5),
                o(FlexOpKind::Mul),
                o(FlexOpKind::Add),
            ],
        );
    }

    /// 单个 `\`（同一行后面不是 `\`）不是续行 → `unknown expression token`。
    ///
    /// 官方报错时打印的是 `GetToken` 读到的那个 token，所以消息形如
    /// `unknown expression token '\%c`。
    #[test]
    fn single_backslash_is_not_a_continuation() {
        expect_err(
            "%a = %b + \\ %c\n",
            CTRLS,
            DESCS,
            "unknown expression token",
        );
    }

    /// `\foo`（`\` 与字母连写）同样不是续行 —— C 的 `GetToken` 会读到
    /// `foo`，首字符不是 `\`。
    #[test]
    fn backslash_letter_is_not_a_continuation() {
        expect_err(
            "%a = %b + \\foo\n",
            CTRLS,
            DESCS,
            "unknown expression token",
        );
    }

    /// 同一个普通 token 内的 `\\` 也是续行：`%b+\\` 是一个普通 token
    /// （`+` 与两个 `\` 之间没有空白），切出来是 `+` `\` `\`。
    #[test]
    fn continuation_inside_a_shared_token() {
        expect_ops(
            "%a = %b+\\\\\n     %c\n",
            CTRLS,
            DESCS,
            &[f2("b"), f2("c"), o(FlexOpKind::Add)],
        );
    }

    /// `\ \`（两个反斜杠之间有空格）同样是续行 —— C 里是两次
    /// `GetToken`，都拿到 `\`。
    #[test]
    fn continuation_with_space_between_backslashes() {
        expect_ops(
            "%a = %b + \\ \\\n     %c\n",
            CTRLS,
            DESCS,
            &[f2("b"), f2("c"), o(FlexOpKind::Add)],
        );
    }

    // ---------------------------------------------------------------
    // 空表达式 / 上限
    // ---------------------------------------------------------------

    /// `%a =` 后面什么都没有：C 的主循环一次都不进（`TokenAvailable()`
    /// 在行尾为 false），于是产出 **0 个 op** 且**不报错**。
    ///
    /// 这条测试断言的是「**恰好为空**」这个具体事实，不是「没崩」——
    /// 它钉住的是「官方在这里不报错」。
    #[test]
    fn empty_expression_yields_zero_ops_without_error() {
        let r = run("%a =\n", CTRLS, DESCS).expect("官方在空表达式处不报错");
        assert_eq!(r.ops, Vec::<FlexOp>::new(), "空表达式应恰好产出 0 个 op");
        assert_eq!(r.flex, "a");
    }

    /// 深括号嵌套（未闭合）会把栈越推越高，超过 `MAX_OPS` 应报错
    /// 而不是无限增长。
    #[test]
    fn runaway_open_parens_are_capped() {
        let text = format!("%a = {}%b\n", "(".repeat(MAX_OPS + 10));
        match run(&text, CTRLS, DESCS) {
            Ok(r) => panic!("应报 too complicated，实际成功并产出 {} 个 op", r.ops.len()),
            Err(e) => assert!(
                e.message.contains("too complicated"),
                "应报 too complicated，实际：{}",
                e.message
            ),
        }
    }

    /// 名字里的 `%` 前缀：`%b` 与 `% b`（有空格）等价 ——
    /// `%` 是单字符表达式 token，名字是**下一个** token。
    #[test]
    fn percent_and_name_may_be_separated_by_space() {
        expect_ops(
            "%a = % b * 0.5\n",
            CTRLS,
            DESCS,
            &[f2("b"), c(0.5), o(FlexOpKind::Mul)],
        );
    }

    /// `c_atof_prefix` 的边界：只有 `"."` 时 `atof` 返回 0.0。
    #[test]
    fn atof_prefix_edge_cases() {
        assert_eq!(c_atof_prefix("0.5"), 0.5);
        assert_eq!(c_atof_prefix("12."), 12.0);
        assert_eq!(c_atof_prefix("1.2.3"), 1.2);
        assert_eq!(c_atof_prefix("."), 0.0);
        assert_eq!(c_atof_prefix("007"), 7.0);
    }

    // ---------------------------------------------------------------
    // 端到端：走**真实调用方**（`parse.rs` 的 `%<flex>` 分支）
    // ---------------------------------------------------------------

    /// 一条完整的 QC 走 `parse_qc_str`，验证：
    ///
    /// ① `%<flex>` 被识别成 flexrule（而不是「未知的 model 选项」）；
    /// ② `flex_rules` 里真的出现了那条规则且 op 序列正确；
    /// ③ 名字表来自**已注册**的 flexcontroller / flexdesc。
    ///
    /// 这条比直接调 [`parse_flexrule`] 强 —— 它同时钉住了
    /// 「调用方在 `%` 分支里正确取到了两张名字表」这个契约。
    ///
    /// ⚠️ `parse_qc_str` 在收尾时会**真的去磁盘读** `$model` 引用的
    /// SMD（`parse.rs:2330`），所以夹具必须落一个最小 SMD 到临时目录。
    #[test]
    fn end_to_end_through_parse_qc() {
        // 最小可用 SMD（`smd.rs` 的 `MINIMAL` 同构：1 根骨骼 + 1 个三角形）。
        //
        // ⚠️ 顶点行必须 **12** 个 token：
        // `parentBone pos3 nrm3 uv2 links bone weight`（`smd.rs:257`）。
        const MIN_SMD: &str = "\
version 1
nodes
0 \"root\" -1
end
skeleton
time 0
0 0.0 0.0 0.0 0.0 0.0 0.0
end
triangles
mat
0 0.0 0.0 0.0 0.0 0.0 1.0 0.0 1.0 1 0 1.0
0 1.0 0.0 0.0 0.0 0.0 1.0 1.0 1.0 1 0 1.0
0 0.0 1.0 0.0 0.0 0.0 1.0 0.0 0.0 1 0 1.0
end
";
        // 用**本进程唯一**的临时目录，避免并行测试互踩。
        let dir = std::env::temp_dir().join(format!("mdlc_flexrule_e2e_{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("应能建临时目录");
        std::fs::write(dir.join("test.smd"), MIN_SMD).expect("应能写临时 SMD");

        // `localvar` / `flexcontroller` / `%<flex>` 都只在 `$model`
        // 块内合法（`Cmd_Model` 的 switch），所以夹具必须包一层。
        //
        // ⚠️ 是 `localvar` 而**不是** `$localvar` —— 官方分发表里写的是
        // `stricmp("localvar", token)`（`studiomdl.cpp:4348`），无 `$`。
        // `$model` 需要 `<名> <smd>` 两个参数才到 `{`。
        let qc = "\
$modelname \"test.mdl\"\n\
$model \"body\" \"test.smd\" {\n\
localvar b c\n\
flexcontroller lid range 0 1 ctrl\n\
%a = %b * 0.5 + ctrl\n\
}\n";
        let result = crate::qc::parse_qc_str(qc, &dir);
        // 先清理再断言 —— 断言 panic 时也不留垃圾。
        let _ = std::fs::remove_dir_all(&dir);
        let desc = result.unwrap_or_else(|errs| panic!("QC 应解析成功，实际：{errs:?}"));

        // 先证明夹具真的注册了名字表 —— 否则下面的 `unknown controller`
        // 会让这个测试以**另一种**方式失败，掩盖真正的问题。
        assert_eq!(
            desc.flex_descriptors
                .iter()
                .map(|d| d.name.as_str())
                .collect::<Vec<_>>(),
            vec!["b", "c"],
            "`$localvar` 应注册两个 flexdesc"
        );
        assert_eq!(
            desc.flex_controllers
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>(),
            vec!["ctrl"],
            "`flexcontroller` 应注册一个控制器"
        );

        // 核心断言：规则存在、目标正确、op 序列正确。
        assert_eq!(desc.flex_rules.len(), 1, "应恰好解析出 1 条 flexrule");
        let rule = &desc.flex_rules[0];
        assert_eq!(rule.flex, "a", "目标 flex 名应为 `a`（`%` 已剥掉）");
        assert!(!rule.ops.is_empty(), "op 序列必须非空");
        assert_eq!(
            rule.ops,
            vec![
                f2("b"),
                c(0.5),
                o(FlexOpKind::Mul),
                f1("ctrl"),
                o(FlexOpKind::Add)
            ],
            "`%b * 0.5 + ctrl` 的后缀序列不符"
        );
    }
}
