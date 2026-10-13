//! 词法层回归测试。
//!
//! 每个测试对应 [`super`] 模块文档里列的一条**反直觉**行为。
//! 这些行为错了**不会报错**，只会静默产出不同的 token 序列 ——
//! 所以每条都必须有一条测试钉住，否则将来「顺手清理」时会被改掉。

use std::path::PathBuf;

use super::Lexer;

fn lex(text: &str) -> Lexer {
    let mut l = Lexer::new(PathBuf::from("."));
    l.load_from_memory(text, "<test>");
    l
}

/// 取全部 token（`crossline = true`）。
fn all(text: &str) -> Vec<String> {
    let mut l = lex(text);
    let mut out = Vec::new();
    loop {
        match l.next_token(true) {
            Ok(Some(t)) => out.push(t.text),
            Ok(None) => break,
            Err(e) => panic!("词法错误: {e}"),
        }
    }
    out
}

/// **行为 1：`\r` 只是空白，不是换行。**
///
/// 官方 `skipspace` 只对 `'\n'` 递增行号，`TokenAvailable()` 也只在
/// `'\n'` 处返回 false。所以 `\r` 之后同一「行」的命令会被吃掉。
///
/// 实测来源：`qc2toml_miku.js` 记录的
/// 「118 条 `$definebone` 只转出 117 条」——
/// `bones.qci` 的 `$bbox` 行以裸 `\r` 结尾，于是第一条 `$definebone`
/// 成了 `$bbox` 的参数。
#[test]
fn bare_cr_is_whitespace_not_newline() {
    // 裸 CR：`$bbox` 与 `$definebone` 在**同一行**。
    let toks = all("$bbox 1 2 3 4 5 6\r$definebone \"tip\" \"root\"\n");
    assert_eq!(
        toks,
        vec![
            "$bbox",
            "1",
            "2",
            "3",
            "4",
            "5",
            "6",
            "$definebone",
            "tip",
            "root"
        ],
        "裸 CR 不得结束行 —— $definebone 会被 $bbox 吃掉"
    );

    // CRLF：`\n` 在，所以正常分行。
    let toks = all("$bbox 1 2 3 4 5 6\r\n$definebone \"tip\"\n");
    assert_eq!(
        toks,
        vec!["$bbox", "1", "2", "3", "4", "5", "6", "$definebone", "tip"],
        "CRLF 必须正常分行"
    );
}

/// **行为 2：`;` 既结束 token 也起注释；`#` 与 `/` 只起注释。**
///
/// 官方常规 token 的扫描条件是 `> 32 && != ';'`，
/// 而注释起始符是 `;` / `#` / `//` 三者。
#[test]
fn semicolon_terminates_token_but_hash_does_not() {
    // `;` 立刻结束当前 token 并起注释。
    let toks = all("abc;def\nghi\n");
    assert_eq!(toks, vec!["abc", "ghi"], "`;` 必须结束 token 并起注释");

    // `#` **不**结束 token（它在 token 扫描条件之外）——
    // 所以 `abc#def` 是**一个** token。
    let toks = all("abc#def\nghi\n");
    assert_eq!(
        toks,
        vec!["abc#def", "ghi"],
        "`#` 不结束 token（只在本行**起始**处才起注释）"
    );

    // `#` 出现在行首时才是注释。
    let toks = all("# comment line\nreal\n");
    assert_eq!(toks, vec!["real"], "行首 `#` 起注释");

    // `//` 起注释。
    let toks = all("real // trailing\nnext\n");
    assert_eq!(toks, vec!["real", "next"], "`//` 起注释");
}

/// **行为 3：`$include` 的路径基准是 `qdir`，与 `$pushd` 无关。**
///
/// 本测试只验证「词法器把 `$include` 的文件名当相对 `qdir` 解析」——
/// 用一个真实存在的临时目录树来证明。
#[test]
fn include_resolves_against_qdir_not_pushd() {
    let dir = std::env::temp_dir().join("mdlc_qc_lex_test_inc");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("includes")).unwrap();
    std::fs::write(
        dir.join("includes").join("inner.qci"),
        "$surfaceprop \"metal\"\n",
    )
    .unwrap();

    let mut l = Lexer::new(dir.clone());
    l.load_from_memory(
        "$include includes/inner.qci\n$contents \"solid\"\n",
        "<test>",
    );
    let mut out = Vec::new();
    loop {
        match l.next_token(true) {
            Ok(Some(t)) => out.push(t.text),
            Ok(None) => break,
            Err(e) => panic!("{e}"),
        }
    }
    assert_eq!(
        out,
        vec!["$surfaceprop", "metal", "$contents", "solid"],
        "$include 应相对 qdir 解析并展开"
    );
    assert_eq!(l.loaded_files.len(), 1, "应记录被 include 的文件");

    let _ = std::fs::remove_dir_all(&dir);
}

/// **行为 4：`$definevariable` 的名字匹配只比 `len - 2` 个字符。**
///
/// 官方 `Q_strnicmp(param, tp, len - 2)`，`len` 是 `$...$` 之间名字长度。
/// 所以引用时**实际参与比较的字符数 = 引用名长度 − 2**：
///
/// | 引用 | 名字长 | 比较字符数 | 比较的前缀 |
/// |---|---|---|---|
/// | `$scale$` | 5 | 3 | `sca` |
/// | `$sca$` | 3 | 1 | `s` |
/// | `$s$` | 1 | **−1 → 0** | 空 ⟹ **第一个变量总命中** |
///
/// 实测影响：`anim_fix.qc` 里 `$scale$` 这类引用会与任何前 3 字符
/// 相同的变量**互相串**。这是上游 bug，但它决定实际行为，所以照抄。
///
/// ⚠️ **变量只在「裸 token」里展开，引号里不展开** —— 见
/// [`variable_is_not_expanded_inside_quotes`]。所以本测试用裸 token。
#[test]
fn variable_match_compares_only_len_minus_2_chars() {
    // 定义 `scale`，用裸 token `$sca$` 引用：只比 1 个字符 `s` ⟹ 命中。
    let toks = all("$definevariable scale 0.922246\n$contents $sca$\n");
    assert_eq!(
        toks,
        vec!["$contents", "0.922246"],
        "`$sca$` 只比 1 个字符，应与 `scale` 命中（上游 bug，照抄）"
    );

    // 定义 `scale`，用裸 token `$scale$` 引用：比 3 个字符 `sca` ⟹ 命中。
    let toks = all("$definevariable scale 0.922246\n$contents $scale$\n");
    assert_eq!(toks, vec!["$contents", "0.922246"], "`$scale$` 应正常命中");

    // 引用名长 **1** ⟹ `len - 2` 为负 ⟹ C 按 0 处理 ⟹ 不比较任何字符
    // ⟹ **第一个变量总命中**。
    let toks = all("$definevariable scale 0.5\n$contents $z$\n");
    assert_eq!(
        toks,
        vec!["$contents", "0.5"],
        "长度 < 2 的引用名按 0 字符比较 ⟹ 第一个变量总命中"
    );

    // ⚠️ 反例（钉住「n 由**引用名**长度决定，不是被查变量名」）：
    // 引用名很长（`$zzzzzzzzzzz$`，len 11 ⟹ n 9）时，
    // 被查的 `scale` 只有 5 字符，前 9 字符根本比不出来 ⟹ **不命中**。
    //
    // ⚠️ 下面两段都在 `next_token()` 里渲染文案，所以语言必须在那一刻生效。
    let e = crate::test_locale::with_english(|| {
        let mut l = lex("$definevariable scale 0.9\n$contents $zzzzzzzzzzz$\n");
        let mut err = None;
        loop {
            match l.next_token(true) {
                Ok(Some(_)) => {}
                Ok(None) => break,
                Err(e) => {
                    err = Some(e);
                    break;
                }
            }
        }
        err.expect("引用名过长必须不命中 ⟹ 报错")
    });
    assert!(
        e.message.contains("unknown variable"),
        "n 由引用名长度决定；引用名过长应不命中，实际：{}",
        e.message
    );

    // 前若干字符**不同** ⟹ 不命中 ⟹ 报错。
    let e = crate::test_locale::with_english(|| {
        let mut l = lex("$definevariable scale 0.9\n$contents $other$\n");
        let mut err = None;
        loop {
            match l.next_token(true) {
                Ok(Some(_)) => {}
                Ok(None) => break,
                Err(e) => {
                    err = Some(e);
                    break;
                }
            }
        }
        err.expect("前缀不同必须报错")
    });
    assert!(
        e.message.contains("unknown variable"),
        "错误信息应指出未知变量，实际：{}",
        e.message
    );
}

/// 变量在**非引号** token 里也要展开（官方在常规 token 循环里做）。
#[test]
fn variable_expands_inside_bare_token() {
    let toks =
        all("$definevariable Bone \"ValveBiped.Bip01_\"\n$bonesaveframe $Bone$Pelvis position\n");
    assert_eq!(
        toks,
        vec!["$bonesaveframe", "ValveBiped.Bip01_Pelvis", "position"],
        "变量应嵌进裸 token 里展开"
    );
}

/// **引号 token 里不展开变量**（官方只在常规 token 分支调
/// `ExpandVariableToken`，`scriplib.cpp:546-575`）。
///
/// 这条很容易搞反 —— 「引号里也能用变量」是直觉，但官方**不是**这样。
/// 实测 `anim_fix.qc` 的所有变量引用都是裸 token
/// （`$continue X scale $scale$`、`$modelname survivors/anim_$Name$.mdl`）。
#[test]
fn variable_is_not_expanded_inside_quotes() {
    let toks =
        all("$definevariable Bone \"ValveBiped.Bip01_\"\n$attachment \"a\" \"$Bone$Hand\"\n");
    assert_eq!(
        toks,
        vec!["$attachment", "a", "$Bone$Hand"],
        "引号里的 `$Bone$` 必须**原样保留**，不展开"
    );
}

/// **行为 4b（mdlc 的故意差异）：`$definevariable` 重复定义 ⟹ 覆盖。**
///
/// ⚠️ 官方**不是**这样：`g_definevariable.AddToTail( v )`
/// （L4D2 `scriplib.cpp:211`）是**追加**，查找 `:361-370` 取**第一个**命中
/// ⟹ **先到先得**。真 studiomdl 裁决
/// （`docs/_probe/oracle_definevariable.js` 变体 B）：
/// `$definevariable Name first` + `$definevariable Name second`
/// ⟹ 产出 **`first.mdl`**。
///
/// mdlc 按用户要求实现**覆盖**语义（NekoMDL 生态靠它表达
/// `$redefinevariable` 的意图，而官方**没有**那个命令 —— exe 串扫描 0 命中，
/// 官方对它报 `bad command $redefinevariable`）。
#[test]
fn definevariable_overrides_an_existing_variable() {
    // 官方会是 `first`；mdlc 取最后一次定义 `second`。
    let toks = all("$definevariable Name first\n$definevariable Name second\n$contents $Name$\n");
    assert_eq!(
        toks,
        vec!["$contents", "second"],
        "mdlc 的 `$definevariable` 必须覆盖同名变量（官方是先到先得，这是**故意差异**）"
    );

    // 单次定义仍然正常。
    let toks = all("$definevariable Name only\n$contents $Name$\n");
    assert_eq!(toks, vec!["$contents", "only"], "单次定义不受影响");
}

/// **行为 4c：覆盖是大小写不敏感的。**
///
/// 官方查找用 `Q_strnicmp`（`scriplib.cpp:363`），本来就不区分大小写；
/// 覆盖的「同名」判定必须与之一致，否则 `Scale` / `SCALE` 会变成两个变量。
#[test]
fn definevariable_override_is_case_insensitive() {
    let toks = all("$definevariable Scale 0.9\n$definevariable SCALE 0.5\n$contents $scale$\n");
    assert_eq!(
        toks,
        vec!["$contents", "0.5"],
        "`Scale` 与 `SCALE` 是同一个变量 ⟹ 覆盖生效"
    );
}

/// **行为 4d：覆盖必须**原地**替换，不能「删掉再追加」。**
///
/// ⚠️ 这是覆盖语义里唯一的实现陷阱。查找是按 `len - 2` 个字符的
/// **前缀**比较（上游 bug，见 [`variable_match_compares_only_len_minus_2_chars`]），
/// 命中顺序由**下标**决定：`len < 2` 的引用（如 `$b$`，n = 0）
/// **总是命中下标 0 的那个变量**。
///
/// 若把覆盖实现成「remove + push_back」，被覆盖的变量会被挪到尾部，
/// 于是 `$b$` 会改命中原本排第二的变量 ⟹ **静默改变产物**。
#[test]
fn definevariable_override_keeps_the_original_index() {
    // `alpha` 在下标 0、`beta` 在下标 1。
    // `$b$` 的名字长 1 ⟹ n = 0 ⟹ 不比较任何字符 ⟹ 命中下标 0 的 `alpha`。
    let toks = all("$definevariable alpha AAA\n$definevariable beta BBB\n$contents $b$\n");
    assert_eq!(
        toks,
        vec!["$contents", "AAA"],
        "n = 0 的引用应命中下标 0 的变量（`alpha`）"
    );

    // 覆盖 `alpha`：值变了，但**下标必须仍是 0** ⟹ `$b$` 仍命中它。
    // 「remove + push_back」会让 `beta` 顶到下标 0 ⟹ 这里会拿到 `BBB`。
    let toks = all("$definevariable alpha AAA\n$definevariable beta BBB\n\
         $definevariable alpha CCC\n$contents $b$\n");
    assert_eq!(
        toks,
        vec!["$contents", "CCC"],
        "覆盖后 `alpha` 必须仍在下标 0 ⟹ `$b$` 拿到覆盖后的新值；\
         若拿到 `BBB` 说明实现成了 remove + push_back"
    );
}

/// **行为 5：`\\`（两个反斜杠）不是通用续行符。**
///
/// 它只被 `Option_Flexrule` 识别（`studiomdl.cpp:3936`）。
/// 实测语料：`survivors_facerules.qci` 的 `\\` 是真续行。
///
/// ⚠️ **订正**：早先这里写着「`anims_fix.qci` 的 59 处 `\\` 全在注释里」——
/// 那是**错的**。实测那 59 处里 **50 处在代码里**，是 `IncapAimMacro`
/// （`anims_fix.qci:98-137`）与 `DebiddoChargerLoop`（`:144-153`）两个
/// **宏体**的续行符，由 `DefineMacro` 消费（见下一条测试）。
/// 注释里那 9 处（`:1-9` 画表格的）才是装饰。
///
/// 词法层必须把 `\\` 当**普通 token** 交出去（由 `define_macro` 与
/// flexrule 解析器各自处理），而不是自己拼接行。
#[test]
fn double_backslash_is_a_plain_token_not_a_continuation() {
    let toks = all("%mouth = %A * 0.5 \\\\\n+ %B * 0.35\n");
    assert_eq!(
        toks,
        vec![
            "%mouth", "=", "%A", "*", "0.5", "\\\\", "+", "%B", "*", "0.35"
        ],
        "`\\\\` 必须是普通 token（只有 flexrule 解析器赋予它续行语义）"
    );
}

/// ⚠️ **回归（R32）**：`$definemacro` 的宏体**跨 `\\` 续行**。
///
/// 官方 `DefineMacro`（`hl2sdk-l4d2\utils\common\scriplib.cpp:147-198`）：
///
/// ```c
/// char *cp = script->script_p;
/// while (*cp && *cp != '\n') {
///     if (*cp == '\\' && *(cp+1) == '\\') {
///         while (*cp && *cp != '\n') { *cp = ' '; cp++; }   // 到行尾改空格
///         if (*cp) { cp++; }                                 // 跨过换行
///     } else { cp++; }
/// }
/// int size = (cp - script->script_p);
/// ```
///
/// 修复前 mdlc **只取「本行剩余」**，于是
/// `$definemacro IncapAimMacro FileName \\` 的体变**空** ⟹
/// `anims_fix.qci:173`/`:174` 两次调用展开成空 ⟹
/// **26 条 `$animation` + 2 个 `$sequence` 静默消失**，
/// 随后 `anim_fix.qc:35` 的 `$continue` 指向不存在的序列。
///
/// 语义细节：宏体从**最后一个形参之后**开始；`\\` 与它所在行的**剩余部分**
/// 都被换成空格（所以体里不会出现 `\\` token）；第一个**没有** `\\`
/// 的换行结束宏体且**不属于**体。
#[test]
fn definemacro_body_continues_across_double_backslash() {
    // 体有三行：`$body $A$ x` 与 `$tail`，中间靠 `\\` 连接；
    // 末尾 `$tail` 后面是**普通**换行 ⟹ 体到此为止。
    let toks = all("$definemacro M A \\\\\n$body $A$ x \\\\\n$tail\n$M val\n");
    assert_eq!(
        toks,
        vec!["$body", "val", "x", "$tail"],
        "宏体必须跨 `\\\\` 续行（`$A$` 展开成实参 `val`），且不得吃掉体外的 `$M val`"
    );
}

/// `\\` 只终止**宏体**，不影响宏之后的普通行。
///
/// 若把 `\\` 做成**通用**续行符，宏定义之后的第一行代码会被吞进宏体。
#[test]
fn definemacro_stops_at_a_plain_newline() {
    let toks = all("$definemacro M \\\\\n$in_macro\n$out_of_macro\n");
    assert_eq!(
        toks,
        vec!["$out_of_macro"],
        "宏体到普通换行为止；宏定义本身不产出 token"
    );
}

/// 宏体里被 `\\` 吃掉的换行**必须计入行号**（`anims_fix.qci` 的宏有 39 行）。
///
/// ⚠️ 这是 mdlc 的**故意偏离**：官方那段手工扫描**不更新** `script->line`，
/// 所以多行宏之后的官方行号是偏的。mdlc 选择修正它 —— 报错定位才有意义。
#[test]
fn definemacro_counts_lines_across_continuations() {
    let mut l = lex("$definemacro M \\\\\n$a \\\\\n$b \\\\\n$c\n$surfaceprop \"metal\"\n");
    let t = match l.next_token(true) {
        Ok(Some(t)) => t,
        Ok(None) => panic!("应有 token"),
        Err(e) => panic!("词法错误: {e}"),
    };
    assert_eq!(t.text, "$surfaceprop");
    assert_eq!(
        t.line, 5,
        "跨过 3 个 `\\\\` 续行 ⟹ 下一个 token 在第 5 行（实际 {}）",
        t.line
    );
}

/// **行为 5b：注释里的 `\\` 不得影响后续解析。**
///
/// `anims_fix.qci` 顶部有 9 行用 `\\` 画表格的注释；
/// 若把 `\\` 做成通用续行，注释会被拼成一行并吃掉下一行代码。
#[test]
fn double_backslash_inside_comment_is_harmless() {
    let src = "// ====\\\\\n// table \\\\\n$surfaceprop \"metal\"\n";
    let toks = all(src);
    assert_eq!(
        toks,
        vec!["$surfaceprop", "metal"],
        "注释里的 `\\\\` 不得吃掉下一行"
    );
}

/// **行为 6（mdlc 的故意差异）：`$include` 找不到文件时报错。**
///
/// 官方静默跳过（`LoadFile` 返回空 → 立刻 `EndOfScript`）。
/// mdlc 报错 —— 静默跳过属于「静默吃数据」。
#[test]
fn missing_include_is_an_error_not_a_silent_skip() {
    // ⚠️ 文案在 `next_token()` 里就渲染好了，所以语言必须在那一刻生效。
    let err = crate::test_locale::with_english(|| {
        let mut l = lex("$include nope_missing_file.qci\n");
        loop {
            match l.next_token(true) {
                Ok(Some(_)) => continue,
                Ok(None) => panic!("必须报错，而不是静默结束"),
                Err(e) => break e,
            }
        }
    });
    assert!(
        err.message.contains("$include cannot find"),
        "应报 $include 找不到，实际：{}",
        err.message
    );
}

/// 引号 token 与裸 token 的区分。
#[test]
fn quoted_and_bare_tokens() {
    let mut l = lex("$modelname \"mymod/ip.mdl\"\n$cdmaterials models/mymod\n");
    let t1 = l.next_token(true).unwrap().unwrap();
    assert_eq!(t1.text, "$modelname");
    assert!(!t1.quoted);
    let t2 = l.next_token(true).unwrap().unwrap();
    assert_eq!(t2.text, "mymod/ip.mdl");
    assert!(t2.quoted, "引号 token 必须被标记");
    let t3 = l.next_token(true).unwrap().unwrap();
    assert_eq!(t3.text, "$cdmaterials");
    let t4 = l.next_token(true).unwrap().unwrap();
    assert_eq!(t4.text, "models/mymod");
    assert!(!t4.quoted);
}

/// 块注释（`/* */`）跨行且不产出 token。
#[test]
fn block_comment_spans_lines() {
    let toks = all("$a 1\n/* $b 2\n   $c 3 */\n$d 4\n");
    assert_eq!(toks, vec!["$a", "1", "$d", "4"], "块注释内容不得产出 token");
}

/// `TokenAvailable()` 的语义：本行是否还有 token。
#[test]
fn token_available_respects_line_boundary() {
    let mut l = lex("$a 1 2\n$b\n");
    assert!(l.token_available(), "行首应有 token");
    let _ = l.next_token(true).unwrap(); // $a
    assert!(l.token_available());
    let _ = l.next_token(true).unwrap(); // 1
    assert!(l.token_available());
    let _ = l.next_token(true).unwrap(); // 2
    assert!(!l.token_available(), "行尾（\\n 之后）不应再有 token");
}

/// 行号必须准确（错误信息靠它定位）。
#[test]
fn line_numbers_are_tracked() {
    let mut l = lex("$a\n$b\n\n$c\n");
    assert_eq!(l.next_token(true).unwrap().unwrap().line, 1);
    assert_eq!(l.next_token(true).unwrap().unwrap().line, 2);
    assert_eq!(l.next_token(true).unwrap().unwrap().line, 4);
}

/// 非 ASCII（UTF-8 多字节）不得被拆坏。
#[test]
fn utf8_tokens_survive() {
    let toks = all("$surfaceprop \"金属\"\n// 中文注释\n$contents \"solid\"\n");
    assert_eq!(toks, vec!["$surfaceprop", "金属", "$contents", "solid"]);
}

/// 文件末尾没有换行时也要正常结束（`bones.qci` 实测就是这个形态）。
#[test]
fn file_without_trailing_newline() {
    let toks = all("$surfaceprop \"metal\"");
    assert_eq!(toks, vec!["$surfaceprop", "metal"]);
}

/// 空文件不报错，直接结束。
#[test]
fn empty_input_is_fine() {
    assert!(all("").is_empty());
}

/// `unget` 必须让同一个 token 再被取到（官方 `UnGetToken`）。
#[test]
fn unget_returns_the_same_token() {
    let mut l = lex("$a $b\n");
    let t = l.next_token(true).unwrap().unwrap();
    l.unget(t.clone());
    let again = l.next_token(true).unwrap().unwrap();
    assert_eq!(t, again, "unget 后必须取到同一个 token");
}

/// ⭐⭐⭐ R37 回归：**连续注释不得消耗栈**。
///
/// `scan_token` 跳过注释后曾用 `return self.scan_token(crossline);` 重来。
/// 那是**尾调用**，但 **Rust 不做尾调用消除** ⟹ debug 下每跳过一行注释
/// 就多压一个 `0x11a0`（4512）字节的栈帧 ⟹ **连续 220 行注释即爆栈**
/// （`0xc00000fd`，实测阈值见 `docs/_probe/stack_threshold.js`）。
///
/// 真实触发源是用户工程的 `lods.qci`：前 **259 行**全是被注释掉的
/// `$lod 10 { … }` 块 ⟹ debug 二进制 3 秒即崩。
/// ⚠️ **release 不崩**（栈帧小得多），所以这个缺陷只在 debug 下显形 ——
/// 这正是它长期潜伏的原因，也是本测试存在的理由。
///
/// 用例取 **5000** 行：若退回递归，debug 下需要约 22 MB 栈，
/// 远超任何测试线程的栈（默认 2 MiB，`RUST_MIN_STACK` 也只到 8 MiB）
/// ⟹ **不存在「栈恰好够大所以侥幸通过」的可能**。
#[test]
fn long_comment_run_does_not_consume_stack() {
    const N: usize = 5000;

    let mut line_src = String::new();
    let mut block_src = String::new();
    for _ in 0..N {
        line_src.push_str("// x\n");
        block_src.push_str("/* x */\n");
    }
    line_src.push_str("$surfaceprop \"metal\"\n");
    block_src.push_str("$surfaceprop \"metal\"\n");

    for (label, src) in [("行注释", &line_src), ("块注释", &block_src)] {
        let mut l = lex(src);
        let t = l.next_token(true).unwrap().unwrap();
        assert_eq!(
            t.text, "$surfaceprop",
            "{label}：连续 {N} 行注释之后必须仍能取到 token"
        );
        // 顺带钉住「位置改在跳注释之后取」这一重构不改变行号。
        assert_eq!(t.line, N + 1, "{label}：注释跳过后行号必须仍然准确");
    }
}

#[test]
fn quoted_preprocessor_directives_and_macro_names_remain_literal() {
    let mut lexer = lex(
        "$definemacro emit value \\\\\n $value$\n\"$include\" \"missing.qci\" \"$definemacro\" \"$definevariable\" \"$emit\" \"$unbound$\"\n$emit expanded\n",
    );
    let mut tokens = Vec::new();
    while let Some(token) = lexer.next_token(true).unwrap() {
        tokens.push((token.text, token.quoted));
    }
    assert_eq!(
        tokens,
        [
            ("$include".to_owned(), true),
            ("missing.qci".to_owned(), true),
            ("$definemacro".to_owned(), true),
            ("$definevariable".to_owned(), true),
            ("$emit".to_owned(), true),
            ("$unbound$".to_owned(), true),
            ("expanded".to_owned(), false),
        ]
    );
}

#[test]
fn unquoted_include_accepts_quoted_paths_and_variables_still_expand() {
    let directory =
        std::env::temp_dir().join(format!("mdlc_qc_quoted_include_{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(directory.join("with spaces.qci"), "included\n").unwrap();
    let mut lexer = Lexer::new(directory.clone());
    lexer.load_from_memory(
        "$definevariable material brick\n$include \"with spaces.qci\"\n$material$/diffuse\n",
        "input.qc",
    );
    let mut tokens = Vec::new();
    while let Some(token) = lexer.next_token(true).unwrap() {
        tokens.push(token.text);
    }
    assert_eq!(tokens, ["included", "brick/diffuse"]);
    std::fs::remove_dir_all(directory).unwrap();
}
