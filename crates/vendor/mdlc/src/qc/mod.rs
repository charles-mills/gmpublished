//! **QC 前端** —— 把 `studiomdl` 的 `.qc` 脚本解析成 [`crate::model::ModelDesc`]。

pub mod flexrule;
pub mod lexer;
pub mod parse;

use std::path::{Path, PathBuf};

/// QC 解析错误。
///
/// 带**文件名与行号** —— 官方 `TokenError` 会打印 `GetTokenizerStatus`
/// 给的文件/行，这里保持一致，否则用户面对一个 41 KB 的 `.qci` 无从下手。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{file}:{line}: {message}")]
pub struct QcError {
    /// 出错位置所在文件（`$include` 进来的文件会显示它自己的名字）。
    pub file: String,
    /// 行号（从 1 起）。
    pub line: usize,
    /// 说明。
    pub message: String,
}

impl QcError {
    /// 构造一条错误。
    pub fn new(file: impl Into<String>, line: usize, message: impl Into<String>) -> Self {
        Self {
            file: file.into(),
            line,
            message: message.into(),
        }
    }
}

/// 解析一份 `.qc`，得到 [`crate::model::ModelDesc`]。
///
/// `path` 是主 QC 文件；它的**父目录**就是官方的 `qdir`
/// （`$include` 与网格相对路径都以它为基准）。
///
/// 返回的 `ModelDesc` 里所有路径都是**相对 QC 所在目录**的字符串
/// （`$pushd` 前缀已经拼进去），所以调用方可以直接拿
/// `path.parent()` 当 `base_dir` 去 `compile()`。
pub fn parse_qc_file(path: &Path) -> Result<crate::model::ModelDesc, Vec<QcError>> {
    let qdir = path.parent().unwrap_or(Path::new(".")).to_path_buf();
    let mut lex = lexer::Lexer::new(qdir);
    let mut errors = Vec::new();

    if let Err(e) = lex.load(path) {
        errors.push(e);
        return Err(errors);
    }

    let mut parser = parse::Parser::new(lex, path);
    match parser.run() {
        Ok(desc) => Ok(desc),
        Err(mut errs) => {
            errors.append(&mut errs);
            Err(errors)
        }
    }
}

/// 解析**内存里**的 QC 文本（单元测试用）。
///
/// `qdir` 是 `$include` 与网格路径的基准目录。
pub fn parse_qc_str(text: &str, qdir: &Path) -> Result<crate::model::ModelDesc, Vec<QcError>> {
    let mut lex = lexer::Lexer::new(qdir.to_path_buf());
    lex.load_from_memory(text, "<memory>");
    let mem_path = PathBuf::from("<memory>");
    let mut parser = parse::Parser::new(lex, &mem_path);
    parser.run()
}

pub fn parse_qc_text(text: &str, path: &Path) -> Result<crate::model::ModelDesc, Vec<QcError>> {
    let qdir = path.parent().unwrap_or(Path::new("."));
    let mut lex = lexer::Lexer::new(qdir.to_path_buf());
    lex.load_from_memory(text, &path.display().to_string());
    let mut parser = parse::Parser::new(lex, path);
    parser.run()
}
