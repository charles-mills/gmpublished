//! Generic Valve KeyValues (VDF) parsing — the text format behind VMT
//! materials, soundscripts, BSP entity lumps, gameinfo.txt, and Steam's
//! .vdf/.acf manifests. Dialect modules ([`crate::vmt`],
//! [`crate::soundscript`]) are thin layers over this parser.
//!
//! Tolerance semantics, matched to how the engine and real workshop
//! content behave rather than to any formal grammar:
//!
//! - `//` comments run to end of line; there are no block comments.
//! - Tokens quote with `"` or `'` (real content uses both); an
//!   unterminated quote takes the rest of the input as one token.
//! - No escape sequences: quoted text is verbatim, so parsing is
//!   zero-copy (`Cow::Borrowed` throughout today; `Cow` keeps room for
//!   an escape-processing option later).
//! - Bare tokens end at whitespace, `{`, `}`, or `//`.
//! - A key followed by `}` or end-of-input is dropped; a stray `{`
//!   skips its whole balanced block; a stray `}` at the top level is
//!   ignored; an unterminated block closes at end of input.
//! - Bare `[...]` tokens are platform conditionals (`[$WIN32]`,
//!   `[!$X360]`); the parser drops the marker and keeps the pair it
//!   gates — Valve's parser would evaluate it, and naive parsers corrupt
//!   the pairing; quoted values like `"[1 1 1]"` are never treated as
//!   conditionals.
//!
//! Duplicate keys are legal and preserved in document order. Lookups
//! walk from the first pair (engine lookup order) and compare keys
//! ASCII-case-insensitively, as the engine does.

use std::borrow::Cow;
use std::fmt;

use crate::Limits;

/// A parsed KeyValues block: the pairs between one `{`/`}` (or the whole
/// document at the top level), in document order, duplicates preserved.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KvDocument<'a> {
    /// The block's pairs in document order.
    pub pairs: Vec<KvPair<'a>>,
}

/// One `key value` or `key { ... }` pair.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KvPair<'a> {
    /// The key as written (case preserved; lookups are case-insensitive).
    pub key: Cow<'a, str>,
    /// The string value or nested block.
    pub value: KvValue<'a>,
}

/// The value side of a pair.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KvValue<'a> {
    /// A quoted or bare string token, verbatim.
    String(Cow<'a, str>),
    /// A nested `{ ... }` block.
    Block(KvDocument<'a>),
}

impl<'a> KvValue<'a> {
    /// The string value, if this is a string pair.
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(value) => Some(value),
            Self::Block(_) => None,
        }
    }

    /// The nested block, if this is a block pair.
    #[must_use]
    pub fn as_block(&self) -> Option<&KvDocument<'a>> {
        match self {
            Self::String(_) => None,
            Self::Block(block) => Some(block),
        }
    }
}

impl<'a> KvDocument<'a> {
    /// First pair matching `key` (ASCII case-insensitive), string or block.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&KvValue<'a>> {
        self.pairs
            .iter()
            .find(|pair| pair.key.eq_ignore_ascii_case(key))
            .map(|pair| &pair.value)
    }

    /// First *string* pair matching `key` (ASCII case-insensitive).
    ///
    /// A block with the same name does not shadow a later string pair;
    /// this matches engine parameter lookup (e.g. VMT `$basetexture`).
    #[must_use]
    pub fn get_str(&self, key: &str) -> Option<&str> {
        self.pairs
            .iter()
            .find(|pair| {
                pair.key.eq_ignore_ascii_case(key) && matches!(pair.value, KvValue::String(_))
            })
            .and_then(|pair| pair.value.as_str())
    }

    /// All nested blocks named `key` (ASCII case-insensitive), in order.
    pub fn blocks<'s>(&'s self, key: &'s str) -> impl Iterator<Item = &'s KvDocument<'a>> {
        self.pairs
            .iter()
            .filter(move |pair| pair.key.eq_ignore_ascii_case(key))
            .filter_map(|pair| pair.value.as_block())
    }
}

/// KeyValues parse failure. Malformed-but-tolerable input never errors
/// (see the module docs); these are resource-limit violations only.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum KvError {
    /// Input exceeds [`Limits::max_input_bytes`].
    InputTooLarge {
        /// Input length in bytes.
        len: u64,
        /// The configured cap.
        max: u64,
    },
    /// Nesting exceeds [`Limits::max_kv_depth`].
    TooDeep {
        /// The configured cap.
        max: usize,
    },
    /// Pair count exceeds [`Limits::max_entries`].
    TooManyPairs {
        /// The configured cap.
        max: usize,
    },
}

impl fmt::Display for KvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InputTooLarge { len, max } => {
                write!(
                    f,
                    "keyvalues input is {len} bytes, over the {max}-byte limit"
                )
            }
            Self::TooDeep { max } => {
                write!(f, "keyvalues nesting exceeds the depth limit of {max}")
            }
            Self::TooManyPairs { max } => {
                write!(f, "keyvalues pair count exceeds the limit of {max}")
            }
        }
    }
}

impl std::error::Error for KvError {}

/// Parse a whole KeyValues document.
///
/// Callers with raw bytes should convert with `String::from_utf8_lossy`
/// first — real workshop files contain invalid UTF-8, and doing the
/// lossy step at the boundary keeps this parser allocation-transparent.
pub fn parse<'a>(text: &'a str, limits: &Limits) -> Result<KvDocument<'a>, KvError> {
    if text.len() as u64 > limits.max_input_bytes {
        return Err(KvError::InputTooLarge {
            len: text.len() as u64,
            max: limits.max_input_bytes,
        });
    }
    let tokens = tokenize(text, limits)?;
    let mut parser = Parser::new(&tokens, limits);
    parser.parse_block(0)
}

// ---------------------------------------------------------------
// Crate-internal machinery, shared with the dialect modules.
// ---------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Token<'a> {
    Word { text: &'a str, quoted: bool },
    Open,
    Close,
}

impl Token<'_> {
    fn is_conditional(&self) -> bool {
        match self {
            Token::Word {
                text,
                quoted: false,
            } => text.len() >= 2 && text.starts_with('[') && text.ends_with(']'),
            _ => false,
        }
    }
}

/// Tokenize the whole input before parsing begins, so [`Parser`] never
/// re-scans text. Capped at a small multiple of
/// [`Limits::max_entries`] — well-formed KeyValues need at most a
/// handful of tokens per pair, and structural tokens (`{`/`}`, dropped
/// keys with no value) that never become an accepted pair would
/// otherwise let a crafted file grow this `Vec` without the
/// pair-count limit ever tripping.
pub(crate) fn tokenize<'a>(text: &'a str, limits: &Limits) -> Result<Vec<Token<'a>>, KvError> {
    let max_tokens = limits.max_entries.saturating_mul(8);
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < text.len() {
        let c = text[i..].chars().next().expect("i is on a char boundary");
        if c.is_whitespace() {
            i += c.len_utf8();
            continue;
        }
        match c {
            '{' => {
                tokens.push(Token::Open);
                i += 1;
            }
            '}' => {
                tokens.push(Token::Close);
                i += 1;
            }
            '/' if text.as_bytes().get(i + 1) == Some(&b'/') => {
                i = text[i..].find('\n').map_or(text.len(), |offset| i + offset);
            }
            '"' | '\'' => {
                let start = i + 1;
                if let Some(offset) = text[start..].find(c) {
                    tokens.push(Token::Word {
                        text: &text[start..start + offset],
                        quoted: true,
                    });
                    i = start + offset + 1;
                } else {
                    // Unterminated quote: the rest of the input is the token.
                    tokens.push(Token::Word {
                        text: &text[start..],
                        quoted: true,
                    });
                    i = text.len();
                }
            }
            _ => {
                let start = i;
                let mut end = i;
                while end < text.len() {
                    let ch = text[end..].chars().next().expect("char boundary");
                    if ch.is_whitespace() || ch == '{' || ch == '}' {
                        break;
                    }
                    if ch == '/' && text.as_bytes().get(end + 1) == Some(&b'/') {
                        break;
                    }
                    end += ch.len_utf8();
                }
                debug_assert!(end > start, "bare token consumed no input");
                tokens.push(Token::Word {
                    text: &text[start..end],
                    quoted: false,
                });
                i = end;
            }
        }
        if tokens.len() > max_tokens {
            return Err(KvError::TooManyPairs {
                max: limits.max_entries,
            });
        }
    }
    Ok(tokens)
}

pub(crate) struct Parser<'t, 'a> {
    tokens: &'t [Token<'a>],
    index: usize,
    pairs_remaining: usize,
    max_pairs: usize,
    max_depth: usize,
}

impl<'t, 'a> Parser<'t, 'a> {
    pub(crate) fn new(tokens: &'t [Token<'a>], limits: &Limits) -> Self {
        Self {
            tokens,
            index: 0,
            pairs_remaining: limits.max_entries,
            max_pairs: limits.max_entries,
            max_depth: limits.max_kv_depth,
        }
    }

    fn peek(&mut self) -> Option<Token<'a>> {
        while let Some(token) = self.tokens.get(self.index) {
            if token.is_conditional() {
                self.index += 1;
                continue;
            }
            return Some(*token);
        }
        None
    }

    fn bump(&mut self) {
        self.index += 1;
    }

    /// Parse pairs until the block's `}` (or end of input). At depth 0 a
    /// stray `}` is ignored and parsing continues; at depth > 0 it ends
    /// the block. `depth` is the nesting level of this block's contents.
    pub(crate) fn parse_block(&mut self, depth: usize) -> Result<KvDocument<'a>, KvError> {
        let mut pairs = Vec::new();
        while let Some(token) = self.peek() {
            match token {
                Token::Close => {
                    self.bump();
                    if depth > 0 {
                        break;
                    }
                }
                Token::Open => {
                    // Stray block with no key: skip it wholesale.
                    self.bump();
                    self.skip_balanced();
                }
                Token::Word { text: key, .. } => {
                    self.bump();
                    match self.peek() {
                        Some(Token::Open) => {
                            self.bump();
                            if depth + 1 >= self.max_depth {
                                return Err(KvError::TooDeep {
                                    max: self.max_depth,
                                });
                            }
                            let block = self.parse_block(depth + 1)?;
                            self.push_pair(&mut pairs, key, KvValue::Block(block))?;
                        }
                        Some(Token::Word { text: value, .. }) => {
                            self.bump();
                            self.push_pair(&mut pairs, key, KvValue::String(Cow::Borrowed(value)))?;
                        }
                        // Key with no value before `}`/end: dropped. The
                        // Close is NOT consumed here — the next iteration
                        // terminates the block with it.
                        Some(Token::Close) | None => {}
                    }
                }
            }
        }
        Ok(KvDocument { pairs })
    }

    fn push_pair(
        &mut self,
        pairs: &mut Vec<KvPair<'a>>,
        key: &'a str,
        value: KvValue<'a>,
    ) -> Result<(), KvError> {
        if self.pairs_remaining == 0 {
            return Err(KvError::TooManyPairs {
                max: self.max_pairs,
            });
        }
        self.pairs_remaining -= 1;
        pairs.push(KvPair {
            key: Cow::Borrowed(key),
            value,
        });
        Ok(())
    }

    /// After a stray `{` has been consumed, skip to its matching `}`.
    pub(crate) fn skip_balanced(&mut self) {
        let mut depth = 1usize;
        while let Some(token) = self.tokens.get(self.index) {
            self.index += 1;
            match token {
                Token::Open => depth = depth.saturating_add(1),
                Token::Close => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                Token::Word { .. } => {}
            }
        }
    }

    /// Next non-conditional token, consuming it; used by dialect parsers.
    #[cfg(any(feature = "soundscript", feature = "bsp"))]
    pub(crate) fn next_token(&mut self) -> Option<Token<'a>> {
        let token = self.peek();
        if token.is_some() {
            self.bump();
        }
        token
    }

    /// Next non-conditional word, consuming it; used by dialect parsers.
    #[cfg(any(feature = "vmt", feature = "soundscript"))]
    pub(crate) fn next_word(&mut self) -> Option<&'a str> {
        match self.peek() {
            Some(Token::Word { text, .. }) => {
                self.bump();
                Some(text)
            }
            _ => None,
        }
    }

    /// Consume an `{` if it is next; used by dialect parsers.
    #[cfg(feature = "vmt")]
    pub(crate) fn consume_open(&mut self) -> bool {
        if matches!(self.peek(), Some(Token::Open)) {
            self.bump();
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(text: &str) -> KvDocument<'_> {
        parse(text, &Limits::default()).expect("parse")
    }

    #[test]
    fn parses_nested_documents_with_duplicates_in_order() {
        let document = doc(r#"
            "AppState"
            {
                "name"  "Garry's Mod"
                "child" { "k" "v1" }
                "child" { "k" "v2" }
                name    "shadowed duplicate"
            }
            "#);
        let app = document
            .get("appstate")
            .and_then(KvValue::as_block)
            .unwrap();
        assert_eq!(app.get_str("NAME"), Some("Garry's Mod"));
        assert_eq!(app.blocks("child").count(), 2);
        assert_eq!(app.blocks("child").nth(1).unwrap().get_str("k"), Some("v2"));
        // Duplicates preserved; lookup returns the first.
        assert_eq!(
            app.pairs
                .iter()
                .filter(|p| p.key.eq_ignore_ascii_case("name"))
                .count(),
            2
        );
    }

    #[test]
    fn tolerates_real_world_malformation() {
        // Unterminated quote, key with no value, stray braces, comments.
        let document = doc(r#"
            } // stray close ignored at top level
            { "orphan" "block skipped" }
            "key" "value" // comment
            dangling
            "#);
        assert_eq!(document.pairs.len(), 1);
        assert_eq!(document.get_str("key"), Some("value"));

        let unterminated = doc("\"key\" \"runs to end");
        assert_eq!(unterminated.get_str("key"), Some("runs to end"));
    }

    #[test]
    fn key_before_close_does_not_eat_the_close() {
        // A key immediately before a `}` must not consume the close brace.
        let document = doc(r#"outer { orphan } "after" "block""#);
        assert!(document.get("outer").and_then(KvValue::as_block).is_some());
        assert_eq!(document.get_str("after"), Some("block"));
    }

    #[test]
    fn strips_conditionals_but_keeps_gated_pairs() {
        let document = doc(r#""$basetexture" "some/tex" [$X360] "$other" "v""#);
        assert_eq!(document.get_str("$basetexture"), Some("some/tex"));
        assert_eq!(document.get_str("$other"), Some("v"));

        // Quoted bracket values are data, not conditionals.
        let color = doc(r#""$color" "[ 1 1 1 ]""#);
        assert_eq!(document.pairs.len(), 2);
        assert_eq!(color.get_str("$color"), Some("[ 1 1 1 ]"));
    }

    #[test]
    fn enforces_limits() {
        let deep = "a {".repeat(100) + &"}".repeat(100);
        assert!(matches!(
            parse(&deep, &Limits::default()),
            Err(KvError::TooDeep { .. })
        ));

        let tiny = Limits {
            max_input_bytes: 4,
            ..Limits::default()
        };
        assert!(matches!(
            parse("\"key\" \"value\"", &tiny),
            Err(KvError::InputTooLarge { .. })
        ));

        let two_pairs = Limits {
            max_entries: 2,
            ..Limits::default()
        };
        assert!(matches!(
            parse("a 1 b 2 c 3", &two_pairs),
            Err(KvError::TooManyPairs { .. })
        ));
    }

    #[test]
    fn tokenize_caps_itself_regardless_of_accepted_pair_count() {
        // A run of unmatched `{` is a keyless stray block: the parser
        // skips it wholesale and never calls push_pair, so pairs_remaining
        // never moves. Without a cap inside tokenize itself, this would
        // grow the token buffer unbounded no matter how tight max_entries
        // is.
        let tight = Limits {
            max_entries: 4,
            ..Limits::default()
        };
        let text = "{ ".repeat(1000);
        assert!(matches!(
            parse(&text, &tight),
            Err(KvError::TooManyPairs { .. })
        ));
    }

    #[test]
    fn multibyte_and_garbage_input_does_not_panic() {
        let text = String::from_utf8_lossy(b"\xff\xfe \"k\xc3\xa9y\" caf\xc3\xa9 {");
        let document = parse(&text, &Limits::default()).unwrap();
        // Replacement chars land in a bare token; the trailing block is
        // unterminated and empty — nothing panics, nothing is lost.
        assert!(!document.pairs.is_empty());
    }
}
