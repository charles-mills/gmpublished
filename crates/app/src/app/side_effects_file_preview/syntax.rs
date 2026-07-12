use std::ops::Range;

use iced::Color;

use crate::features::file_preview::{CodeLine, CodeSpan};
use crate::theme::{ThemeVariant, Tokens};

pub(super) fn glua_highlighted_lines(source_lines: &[String], tokens: &Tokens) -> Vec<CodeLine> {
    let palette = CodeHighlightPalette::from_tokens(tokens);
    let mut state = GluaHighlightState::default();
    source_lines
        .iter()
        .map(|line| glua_highlight_line(line, palette, &mut state))
        .collect()
}

fn glua_highlight_line(
    line: &str,
    palette: CodeHighlightPalette,
    state: &mut GluaHighlightState,
) -> CodeLine {
    let bytes = line.as_bytes();
    let mut spans = Vec::new();
    let mut index = 0;

    if let Some(long) = state.long {
        let color = match long.kind {
            GluaLongKind::Comment => palette.comment,
            GluaLongKind::String => palette.string,
        };
        if let Some(end) = glua_long_close_end(bytes, 0, long.equals) {
            push_code_span(&mut spans, line, 0..end, Some(color));
            state.long = None;
            index = end;
        } else {
            push_code_span(&mut spans, line, 0..bytes.len(), Some(color));
            return spans;
        }
    }

    let mut expect_function_name = false;
    while index < bytes.len() {
        let character = line[index..].chars().next().expect("valid UTF-8 boundary");
        if character.is_whitespace() {
            let start = index;
            index += character.len_utf8();
            while index < bytes.len() {
                let character = line[index..].chars().next().expect("valid UTF-8 boundary");
                if !character.is_whitespace() {
                    break;
                }
                index += character.len_utf8();
            }
            push_code_span(&mut spans, line, start..index, None);
            continue;
        }

        if bytes[index..].starts_with(b"--") {
            if let Some((equals, opener_end)) = glua_long_opener(bytes, index + 2) {
                if let Some(end) = glua_long_close_end(bytes, opener_end, equals) {
                    push_code_span(&mut spans, line, index..end, Some(palette.comment));
                    index = end;
                } else {
                    push_code_span(&mut spans, line, index..bytes.len(), Some(palette.comment));
                    state.long = Some(GluaLongState {
                        kind: GluaLongKind::Comment,
                        equals,
                    });
                    break;
                }
            } else {
                push_code_span(&mut spans, line, index..bytes.len(), Some(palette.comment));
                break;
            }
            continue;
        }

        if let Some((equals, opener_end)) = glua_long_opener(bytes, index) {
            if let Some(end) = glua_long_close_end(bytes, opener_end, equals) {
                push_code_span(&mut spans, line, index..end, Some(palette.string));
                index = end;
            } else {
                push_code_span(&mut spans, line, index..bytes.len(), Some(palette.string));
                state.long = Some(GluaLongState {
                    kind: GluaLongKind::String,
                    equals,
                });
                break;
            }
            continue;
        }

        if matches!(bytes[index], b'\'' | b'"') {
            let (end, closed) = quoted_string_end(bytes, index, bytes[index]);
            push_quoted_string_spans(&mut spans, line, index, end, closed, palette);
            index = end;
            continue;
        }

        if bytes[index].is_ascii_alphabetic() || bytes[index] == b'_' {
            let start = index;
            index += 1;
            while index < bytes.len()
                && (bytes[index].is_ascii_alphanumeric() || bytes[index] == b'_')
            {
                index += 1;
            }
            let word = &line[start..index];
            let color = if is_glua_literal(word) {
                palette.number
            } else if is_glua_keyword(word) {
                expect_function_name = word == "function";
                palette.keyword
            } else if expect_function_name {
                expect_function_name = false;
                palette.function
            } else {
                palette.identifier
            };
            push_code_span(&mut spans, line, start..index, Some(color));
            continue;
        }

        if bytes[index].is_ascii_digit()
            || (bytes[index] == b'.' && bytes.get(index + 1).is_some_and(u8::is_ascii_digit))
        {
            let end = glua_number_end(bytes, index);
            push_code_span(&mut spans, line, index..end, Some(palette.number));
            index = end;
            continue;
        }

        if let Some(end) = glua_operator_end(bytes, index) {
            push_code_span(&mut spans, line, index..end, Some(palette.operator));
            index = end;
            continue;
        }

        let end = index + character.len_utf8();
        push_code_span(&mut spans, line, index..end, None);
        index = end;
    }

    spans
}

pub(super) fn json_highlighted_lines(source_lines: &[String], tokens: &Tokens) -> Vec<CodeLine> {
    let palette = CodeHighlightPalette::from_tokens(tokens);
    source_lines
        .iter()
        .map(|line| json_highlight_line(line, palette))
        .collect()
}

fn json_highlight_line(line: &str, palette: CodeHighlightPalette) -> CodeLine {
    let bytes = line.as_bytes();
    let mut spans = Vec::new();
    let mut index = 0;

    while index < bytes.len() {
        let character = line[index..].chars().next().expect("valid UTF-8 boundary");
        if character.is_whitespace() {
            let start = index;
            index += character.len_utf8();
            while index < bytes.len() {
                let character = line[index..].chars().next().expect("valid UTF-8 boundary");
                if !character.is_whitespace() {
                    break;
                }
                index += character.len_utf8();
            }
            push_code_span(&mut spans, line, start..index, None);
            continue;
        }

        if bytes[index] == b'"' {
            let (end, closed) = quoted_string_end(bytes, index, b'"');
            push_quoted_string_spans(&mut spans, line, index, end, closed, palette);
            index = end;
            continue;
        }

        if bytes[index].is_ascii_digit()
            || (bytes[index] == b'-' && bytes.get(index + 1).is_some_and(u8::is_ascii_digit))
        {
            let end = json_number_end(bytes, index);
            push_code_span(&mut spans, line, index..end, Some(palette.number));
            index = end;
            continue;
        }

        if bytes[index].is_ascii_alphabetic() {
            let start = index;
            index += 1;
            while index < bytes.len() && bytes[index].is_ascii_alphabetic() {
                index += 1;
            }
            let color = match &line[start..index] {
                "true" | "false" | "null" => Some(palette.number),
                _ => Some(palette.identifier),
            };
            push_code_span(&mut spans, line, start..index, color);
            continue;
        }

        let end = index + character.len_utf8();
        push_code_span(&mut spans, line, index..end, None);
        index = end;
    }

    spans
}

fn push_quoted_string_spans(
    spans: &mut CodeLine,
    line: &str,
    start: usize,
    end: usize,
    closed: bool,
    palette: CodeHighlightPalette,
) {
    push_code_span(
        spans,
        line,
        start..start.saturating_add(1),
        Some(palette.string_quote),
    );
    let content_end = if closed { end.saturating_sub(1) } else { end };
    push_code_span(
        spans,
        line,
        start.saturating_add(1)..content_end,
        Some(palette.string),
    );
    if closed {
        push_code_span(spans, line, content_end..end, Some(palette.string_quote));
    }
}

fn quoted_string_end(bytes: &[u8], start: usize, quote: u8) -> (usize, bool) {
    let mut index = start.saturating_add(1);
    while index < bytes.len() {
        match bytes[index] {
            b'\\' => {
                index += 1;
                if index < bytes.len() {
                    index += utf8_character_len(bytes[index]);
                }
            }
            byte if byte == quote => return (index + 1, true),
            byte => index += utf8_character_len(byte),
        }
    }
    (bytes.len(), false)
}

fn utf8_character_len(first_byte: u8) -> usize {
    match first_byte {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf7 => 4,
        _ => 1,
    }
}

fn glua_long_opener(bytes: &[u8], start: usize) -> Option<(usize, usize)> {
    if bytes.get(start) != Some(&b'[') {
        return None;
    }
    let mut index = start + 1;
    while bytes.get(index) == Some(&b'=') {
        index += 1;
    }
    (bytes.get(index) == Some(&b'[')).then_some((index - start - 1, index + 1))
}

fn glua_long_close_end(bytes: &[u8], start: usize, equals: usize) -> Option<usize> {
    let mut index = start;
    while index < bytes.len() {
        if bytes[index] == b']' {
            let equals_end = index.saturating_add(1).saturating_add(equals);
            if equals_end < bytes.len()
                && bytes[index + 1..equals_end]
                    .iter()
                    .all(|byte| *byte == b'=')
                && bytes[equals_end] == b']'
            {
                return Some(equals_end + 1);
            }
        }
        index += 1;
    }
    None
}

fn glua_number_end(bytes: &[u8], start: usize) -> usize {
    let mut index = start;
    if bytes.get(index) == Some(&b'0') && matches!(bytes.get(index + 1), Some(b'x' | b'X')) {
        index += 2;
        while bytes.get(index).is_some_and(u8::is_ascii_hexdigit) {
            index += 1;
        }
        if bytes.get(index) == Some(&b'.') {
            index += 1;
            while bytes.get(index).is_some_and(u8::is_ascii_hexdigit) {
                index += 1;
            }
        }
        if matches!(bytes.get(index), Some(b'p' | b'P')) {
            index += 1;
            if matches!(bytes.get(index), Some(b'+' | b'-')) {
                index += 1;
            }
            while bytes.get(index).is_some_and(u8::is_ascii_digit) {
                index += 1;
            }
        }
        return index;
    }

    if bytes.get(index) == Some(&b'.') {
        index += 1;
    }
    while bytes.get(index).is_some_and(u8::is_ascii_digit) {
        index += 1;
    }
    if bytes.get(index) == Some(&b'.') && bytes.get(index + 1) != Some(&b'.') {
        index += 1;
        while bytes.get(index).is_some_and(u8::is_ascii_digit) {
            index += 1;
        }
    }
    if matches!(bytes.get(index), Some(b'e' | b'E')) {
        index += 1;
        if matches!(bytes.get(index), Some(b'+' | b'-')) {
            index += 1;
        }
        while bytes.get(index).is_some_and(u8::is_ascii_digit) {
            index += 1;
        }
    }
    index
}

fn json_number_end(bytes: &[u8], start: usize) -> usize {
    let mut index = start;
    if bytes.get(index) == Some(&b'-') {
        index += 1;
    }
    if bytes.get(index) == Some(&b'0') {
        index += 1;
    } else {
        while bytes.get(index).is_some_and(u8::is_ascii_digit) {
            index += 1;
        }
    }
    if bytes.get(index) == Some(&b'.') && bytes.get(index + 1).is_some_and(u8::is_ascii_digit) {
        index += 1;
        while bytes.get(index).is_some_and(u8::is_ascii_digit) {
            index += 1;
        }
    }
    if matches!(bytes.get(index), Some(b'e' | b'E')) {
        let exponent = index;
        index += 1;
        if matches!(bytes.get(index), Some(b'+' | b'-')) {
            index += 1;
        }
        let digits = index;
        while bytes.get(index).is_some_and(u8::is_ascii_digit) {
            index += 1;
        }
        if digits == index {
            return exponent;
        }
    }
    index
}

fn glua_operator_end(bytes: &[u8], start: usize) -> Option<usize> {
    [
        "...", "..", "==", "~=", "<=", ">=", "::", "&&", "||", "!=", "<<", ">>", "//", "+", "-",
        "*", "/", "%", "^", "#", "=", "<", ">", "!", "&", "|", "~", ".",
    ]
    .iter()
    .find_map(|operator| {
        bytes[start..]
            .starts_with(operator.as_bytes())
            .then_some(start + operator.len())
    })
}

fn is_glua_keyword(word: &str) -> bool {
    matches!(
        word,
        "and"
            | "break"
            | "continue"
            | "do"
            | "else"
            | "elseif"
            | "end"
            | "for"
            | "function"
            | "goto"
            | "if"
            | "in"
            | "local"
            | "not"
            | "or"
            | "repeat"
            | "return"
            | "then"
            | "until"
            | "while"
    )
}

fn is_glua_literal(word: &str) -> bool {
    matches!(word, "false" | "nil" | "true")
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct CodeHighlightPalette {
    pub(super) comment: [u8; 4],
    pub(super) keyword: [u8; 4],
    pub(super) identifier: [u8; 4],
    pub(super) function: [u8; 4],
    pub(super) string: [u8; 4],
    pub(super) string_quote: [u8; 4],
    pub(super) number: [u8; 4],
    pub(super) operator: [u8; 4],
}

impl CodeHighlightPalette {
    pub(super) fn from_tokens(tokens: &Tokens) -> Self {
        match tokens.variant {
            ThemeVariant::Light => Self {
                comment: [150, 152, 150, 255],
                keyword: [167, 29, 93, 255],
                identifier: [50, 50, 50, 255],
                function: [121, 93, 163, 255],
                string: [24, 54, 145, 255],
                string_quote: [24, 54, 145, 255],
                number: [0, 134, 179, 255],
                operator: [167, 29, 93, 255],
            },
            ThemeVariant::Dark | ThemeVariant::ClassicSource => Self {
                comment: [101, 115, 126, 255],
                keyword: [180, 142, 173, 255],
                identifier: [192, 197, 206, 255],
                function: [143, 161, 179, 255],
                string: [163, 190, 140, 255],
                string_quote: [192, 197, 206, 255],
                number: [208, 135, 112, 255],
                operator: [192, 197, 206, 255],
            },
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct GluaHighlightState {
    long: Option<GluaLongState>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct GluaLongState {
    kind: GluaLongKind,
    equals: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GluaLongKind {
    Comment,
    String,
}

pub(super) fn plain_lines(source_lines: &[String]) -> Vec<CodeLine> {
    source_lines
        .iter()
        .map(|line| {
            if line.is_empty() {
                Vec::new()
            } else {
                vec![CodeSpan {
                    text: line.clone(),
                    color: None,
                }]
            }
        })
        .collect()
}

pub(super) fn vmt_highlighted_lines(source_lines: &[String], tokens: &Tokens) -> Vec<CodeLine> {
    let palette = VmtHighlightPalette::from_tokens(tokens);
    let mut state = VmtHighlightState::default();
    source_lines
        .iter()
        .map(|line| vmt_highlight_line(line, palette, &mut state))
        .collect()
}

fn vmt_highlight_line(
    line: &str,
    palette: VmtHighlightPalette,
    state: &mut VmtHighlightState,
) -> CodeLine {
    let tokens = vmt_line_tokens(line);
    let mut spans = Vec::new();

    for (index, token) in tokens.iter().enumerate() {
        let color = match token.kind {
            VmtLineTokenKind::Whitespace => None,
            VmtLineTokenKind::Comment => Some(palette.comment),
            VmtLineTokenKind::OpenBrace => {
                state.brace_depth = state.brace_depth.saturating_add(1);
                state.expect_key = true;
                Some(palette.punctuation)
            }
            VmtLineTokenKind::CloseBrace => {
                state.brace_depth = state.brace_depth.saturating_sub(1);
                state.expect_key = state.brace_depth > 0;
                Some(palette.punctuation)
            }
            VmtLineTokenKind::Word => Some(vmt_word_color(line, &tokens, index, palette, state)),
        };
        push_code_span(&mut spans, line, token.range.clone(), color);
    }

    spans
}

fn vmt_word_color(
    line: &str,
    tokens: &[VmtLineToken],
    index: usize,
    palette: VmtHighlightPalette,
    state: &mut VmtHighlightState,
) -> [u8; 4] {
    if !state.shader_seen {
        state.shader_seen = true;
        return palette.shader;
    }

    if state.brace_depth == 0 {
        return palette.value;
    }

    let token_text = vmt_token_text(line, &tokens[index]).unwrap_or_default();
    // Same-line brace lookahead catches arbitrary groups; cross-line groups
    // fall back to the common VMT group names below.
    let next_is_open = next_vmt_significant_token(tokens, index)
        .is_some_and(|token| matches!(token.kind, VmtLineTokenKind::OpenBrace));

    if next_is_open || is_vmt_group_keyword(token_text) {
        state.expect_key = false;
        palette.group
    } else if state.expect_key {
        state.expect_key = false;
        palette.key
    } else {
        state.expect_key = true;
        palette.value
    }
}

fn next_vmt_significant_token(tokens: &[VmtLineToken], index: usize) -> Option<&VmtLineToken> {
    tokens.get(index.saturating_add(1)..)?.iter().find(|token| {
        !matches!(
            token.kind,
            VmtLineTokenKind::Whitespace | VmtLineTokenKind::Comment
        )
    })
}

fn is_vmt_group_keyword(text: &str) -> bool {
    ["insert", "replace", "proxies"]
        .iter()
        .any(|keyword| text.eq_ignore_ascii_case(keyword))
}

fn vmt_token_text<'a>(line: &'a str, token: &VmtLineToken) -> Option<&'a str> {
    let text = line.get(token.range.clone())?;
    let bytes = text.as_bytes();
    if bytes.len() >= 2
        && matches!(bytes.first().copied(), Some(b'"' | b'\''))
        && bytes.first() == bytes.last()
    {
        text.get(1..text.len().saturating_sub(1))
    } else {
        Some(text)
    }
}

/// Mirrors the byte-level pieces of the backend VMT tokenizer in
/// `crates/backend/src/scene/vmt.rs`. The backend parser is kept
/// semantic and span-less, so preview highlighting owns its local ranges.
fn vmt_line_tokens(line: &str) -> Vec<VmtLineToken> {
    let bytes = line.as_bytes();
    let mut tokens = Vec::new();
    let mut index = 0;

    while index < bytes.len() {
        match bytes[index] {
            byte if byte.is_ascii_whitespace() => {
                let start = index;
                while index < bytes.len() && bytes[index].is_ascii_whitespace() {
                    index += 1;
                }
                tokens.push(VmtLineToken {
                    range: start..index,
                    kind: VmtLineTokenKind::Whitespace,
                });
            }
            b'{' => {
                tokens.push(VmtLineToken {
                    range: index..index + 1,
                    kind: VmtLineTokenKind::OpenBrace,
                });
                index += 1;
            }
            b'}' => {
                tokens.push(VmtLineToken {
                    range: index..index + 1,
                    kind: VmtLineTokenKind::CloseBrace,
                });
                index += 1;
            }
            b'/' if bytes.get(index + 1) == Some(&b'/') => {
                tokens.push(VmtLineToken {
                    range: index..bytes.len(),
                    kind: VmtLineTokenKind::Comment,
                });
                break;
            }
            b'"' | b'\'' => {
                let start = index;
                let quote = bytes[index];
                index += 1;
                // Keep unterminated quotes local to the current preview line.
                // That preserves byte ranges and avoids corrupting later spans.
                while index < bytes.len() {
                    let byte = bytes[index];
                    index += 1;
                    if byte == quote {
                        break;
                    }
                }
                tokens.push(VmtLineToken {
                    range: start..index,
                    kind: VmtLineTokenKind::Word,
                });
            }
            _ => {
                let start = index;
                while index < bytes.len() {
                    let byte = bytes[index];
                    if byte.is_ascii_whitespace()
                        || byte == b'{'
                        || byte == b'}'
                        || (byte == b'/' && bytes.get(index + 1) == Some(&b'/'))
                    {
                        break;
                    }
                    index += 1;
                }
                if start == index {
                    index += 1;
                } else {
                    tokens.push(VmtLineToken {
                        range: start..index,
                        kind: VmtLineTokenKind::Word,
                    });
                }
            }
        }
    }

    tokens
}

fn push_code_span(spans: &mut CodeLine, line: &str, range: Range<usize>, color: Option<[u8; 4]>) {
    let Some(text) = line.get(range) else {
        return;
    };
    if text.is_empty() {
        return;
    }
    if let Some(last) = spans.last_mut()
        && last.color == color
    {
        last.text.push_str(text);
        return;
    }
    spans.push(CodeSpan {
        text: text.to_owned(),
        color,
    });
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct VmtHighlightPalette {
    pub(super) comment: [u8; 4],
    pub(super) punctuation: [u8; 4],
    pub(super) shader: [u8; 4],
    pub(super) group: [u8; 4],
    pub(super) key: [u8; 4],
    pub(super) value: [u8; 4],
}

impl VmtHighlightPalette {
    pub(super) fn from_tokens(tokens: &Tokens) -> Self {
        Self {
            comment: color_to_rgba(tokens.colors.text_dim.into()),
            punctuation: color_to_rgba(tokens.colors.text_dim.into()),
            shader: color_to_rgba(tokens.colors.link.into()),
            group: color_to_rgba(tokens.colors.success.into()),
            key: color_to_rgba(tokens.colors.neutral_dark.into()),
            value: color_to_rgba(tokens.colors.text.into()),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct VmtHighlightState {
    shader_seen: bool,
    brace_depth: usize,
    expect_key: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct VmtLineToken {
    range: Range<usize>,
    kind: VmtLineTokenKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum VmtLineTokenKind {
    Whitespace,
    Word,
    OpenBrace,
    CloseBrace,
    Comment,
}

fn color_to_rgba(color: Color) -> [u8; 4] {
    [
        color_channel(color.r),
        color_channel(color.g),
        color_channel(color.b),
        color_channel(color.a),
    ]
}

fn color_channel(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}
