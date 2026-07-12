//! Tolerant parser for the Steam Community BBCode dialect.
//!
//! The document tree is UI-independent so display surfaces and a future
//! description editor can share exactly the same interpretation of markup.

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Document {
    nodes: Vec<Node>,
}

impl Document {
    #[must_use]
    pub fn parse(source: &str) -> Self {
        Parser::new(source).parse()
    }

    #[must_use]
    pub fn nodes(&self) -> &[Node] {
        &self.nodes
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nodes.iter().all(Node::is_empty)
    }

    #[must_use]
    pub fn plain_text(&self) -> String {
        self.nodes.iter().map(Node::plain_text).collect()
    }
}

impl From<&str> for Document {
    fn from(source: &str) -> Self {
        Self::parse(source)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Node {
    Text(String),
    Element(Element),
}

impl Node {
    fn is_empty(&self) -> bool {
        match self {
            Self::Text(text) => text.trim().is_empty(),
            Self::Element(element) => {
                !matches!(
                    element.kind,
                    ElementKind::HorizontalRule | ElementKind::Image { .. }
                ) && element.children.iter().all(Self::is_empty)
            }
        }
    }

    #[must_use]
    pub fn plain_text(&self) -> String {
        match self {
            Self::Text(text) => text.clone(),
            Self::Element(element) => match &element.kind {
                ElementKind::Image { source } => source.clone(),
                _ => element
                    .children
                    .iter()
                    .map(Self::plain_text)
                    .collect::<String>(),
            },
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Element {
    kind: ElementKind,
    children: Vec<Node>,
}

impl Element {
    #[must_use]
    pub const fn kind(&self) -> &ElementKind {
        &self.kind
    }

    #[must_use]
    pub fn children(&self) -> &[Node] {
        &self.children
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ElementKind {
    Heading(u8),
    Bold,
    Underline,
    Italic,
    Strikethrough,
    Spoiler(SpoilerId),
    HorizontalRule,
    Link { target: String },
    Image { source: String },
    List { ordered: bool },
    ListItem,
    Quote { author: Option<String> },
    Code,
    Table { bordered: bool, equal_cells: bool },
    TableRow,
    TableHeader,
    TableCell,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SpoilerId(u32);

impl SpoilerId {
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

#[derive(Debug)]
struct Parser<'a> {
    source: &'a str,
    cursor: usize,
    next_spoiler_id: u32,
    stack: Vec<Frame>,
}

#[derive(Debug)]
struct Frame {
    kind: Option<PendingKind>,
    opener: String,
    nodes: Vec<Node>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum PendingKind {
    Heading(u8),
    Bold,
    Underline,
    Italic,
    Strikethrough,
    Spoiler(SpoilerId),
    Link(Option<String>),
    List { ordered: bool, implicit: bool },
    ListItem,
    Quote(Option<String>),
    Table { bordered: bool, equal_cells: bool },
    TableRow,
    TableHeader,
    TableCell,
}

impl PendingKind {
    fn name(&self) -> &'static str {
        match self {
            Self::Heading(1) => "h1",
            Self::Heading(2) => "h2",
            Self::Heading(_) => "h3",
            Self::Bold => "b",
            Self::Underline => "u",
            Self::Italic => "i",
            Self::Strikethrough => "strike",
            Self::Spoiler(_) => "spoiler",
            Self::Link(_) => "url",
            Self::List { ordered: false, .. } => "list",
            Self::List { ordered: true, .. } => "olist",
            Self::ListItem => "*",
            Self::Quote(_) => "quote",
            Self::Table { .. } => "table",
            Self::TableRow => "tr",
            Self::TableHeader => "th",
            Self::TableCell => "td",
        }
    }

    fn finish(self, children: Vec<Node>) -> Element {
        let kind = match self {
            Self::Heading(level) => ElementKind::Heading(level),
            Self::Bold => ElementKind::Bold,
            Self::Underline => ElementKind::Underline,
            Self::Italic => ElementKind::Italic,
            Self::Strikethrough => ElementKind::Strikethrough,
            Self::Spoiler(id) => ElementKind::Spoiler(id),
            Self::Link(target) => ElementKind::Link {
                target: target
                    .unwrap_or_else(|| children.iter().map(Node::plain_text).collect::<String>()),
            },
            Self::List { ordered, .. } => ElementKind::List { ordered },
            Self::ListItem => ElementKind::ListItem,
            Self::Quote(author) => ElementKind::Quote { author },
            Self::Table {
                bordered,
                equal_cells,
            } => ElementKind::Table {
                bordered,
                equal_cells,
            },
            Self::TableRow => ElementKind::TableRow,
            Self::TableHeader => ElementKind::TableHeader,
            Self::TableCell => ElementKind::TableCell,
        };
        Element { kind, children }
    }
}

impl<'a> Parser<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            source,
            cursor: 0,
            next_spoiler_id: 0,
            stack: vec![Frame {
                kind: None,
                opener: String::new(),
                nodes: Vec::new(),
            }],
        }
    }

    fn parse(mut self) -> Document {
        while self.cursor < self.source.len() {
            let Some(relative_open) = self.source[self.cursor..].find('[') else {
                self.push_text(&self.source[self.cursor..]);
                self.cursor = self.source.len();
                break;
            };
            let open = self.cursor + relative_open;
            if open > self.cursor {
                self.push_text(&self.source[self.cursor..open]);
            }
            let Some(relative_close) = self.source[open..].find(']') else {
                self.push_text(&self.source[open..]);
                self.cursor = self.source.len();
                break;
            };
            let close = open + relative_close;
            let raw = &self.source[open..=close];
            let token = &self.source[open + 1..close];
            self.cursor = close + 1;

            if !self.consume_token(raw, token) {
                self.push_text(raw);
            }
        }

        self.finish_implicit_list();
        while self.stack.len() > 1 {
            let frame = self.stack.pop().expect("non-root frame");
            self.push_text(&frame.opener);
            self.current_nodes().extend(frame.nodes);
        }

        let nodes = self.stack.pop().expect("root frame").nodes;
        Document {
            nodes: linkify_nodes(nodes),
        }
    }

    fn consume_token(&mut self, raw: &str, token: &str) -> bool {
        let token = token.trim();
        if let Some(name) = token.strip_prefix('/') {
            return self.close(name.trim());
        }

        let lowercase_token = token.to_ascii_lowercase();
        if lowercase_token == "table" || lowercase_token.starts_with("table ") {
            let bordered = !table_option_enabled(&lowercase_token, "noborder");
            let equal_cells = table_option_enabled(&lowercase_token, "equalcells");
            self.open_frame(
                raw,
                PendingKind::Table {
                    bordered,
                    equal_cells,
                },
            );
            return true;
        }

        let (name, value) = token
            .split_once('=')
            .map_or((token, None), |(name, value)| {
                (name.trim(), Some(value.trim()))
            });
        let name = name.to_ascii_lowercase();

        if name == "img" {
            return self.consume_image(raw);
        }
        if name == "noparse" || name == "code" {
            return self.consume_raw_block(raw, &name);
        }
        if name == "hr" {
            self.current_nodes().push(Node::Element(Element {
                kind: ElementKind::HorizontalRule,
                children: Vec::new(),
            }));
            self.consume_immediate_close("hr");
            return true;
        }
        if name == "*" {
            return self.open_list_item(raw);
        }
        if name == "tr" {
            return self.open_table_row(raw);
        }
        if name == "th" || name == "td" {
            return self.open_table_cell(raw, name == "th");
        }

        let kind = match name.as_str() {
            "h1" => PendingKind::Heading(1),
            "h2" => PendingKind::Heading(2),
            "h3" => PendingKind::Heading(3),
            "b" => PendingKind::Bold,
            "u" => PendingKind::Underline,
            "i" => PendingKind::Italic,
            "strike" => PendingKind::Strikethrough,
            "spoiler" => {
                let id = SpoilerId(self.next_spoiler_id);
                self.next_spoiler_id = self.next_spoiler_id.saturating_add(1);
                PendingKind::Spoiler(id)
            }
            "url" => PendingKind::Link(value.filter(|value| !value.is_empty()).map(str::to_owned)),
            "list" => PendingKind::List {
                ordered: false,
                implicit: false,
            },
            "olist" => PendingKind::List {
                ordered: true,
                implicit: false,
            },
            "quote" => {
                PendingKind::Quote(value.filter(|value| !value.is_empty()).map(str::to_owned))
            }
            _ => return false,
        };
        self.open_frame(raw, kind);
        true
    }

    fn consume_raw_block(&mut self, opener: &str, name: &str) -> bool {
        let closing = format!("[/{name}]");
        let Some(relative_end) = find_ascii_case_insensitive(&self.source[self.cursor..], &closing)
        else {
            return false;
        };
        let end = self.cursor + relative_end;
        let raw_content = &self.source[self.cursor..end];
        self.cursor = end + closing.len();
        if name == "code" {
            self.current_nodes().push(Node::Element(Element {
                kind: ElementKind::Code,
                children: vec![Node::Text(raw_content.to_owned())],
            }));
        } else {
            self.push_text(raw_content);
        }
        let _ = opener;
        true
    }

    fn consume_image(&mut self, opener: &str) -> bool {
        let closing = "[/img]";
        let Some(relative_end) = find_ascii_case_insensitive(&self.source[self.cursor..], closing)
        else {
            return false;
        };
        let end = self.cursor + relative_end;
        let source = self.source[self.cursor..end].trim();
        if source.is_empty() {
            return false;
        }
        self.cursor = end + closing.len();
        self.current_nodes().push(Node::Element(Element {
            kind: ElementKind::Image {
                source: source.to_owned(),
            },
            children: Vec::new(),
        }));
        let _ = opener;
        true
    }

    fn consume_immediate_close(&mut self, name: &str) {
        let closing = format!("[/{name}]");
        let tail = &self.source[self.cursor..];
        if tail
            .get(..closing.len())
            .is_some_and(|candidate| candidate.eq_ignore_ascii_case(&closing))
        {
            self.cursor += closing.len();
        }
    }

    fn open_list_item(&mut self, raw: &str) -> bool {
        if self.stack.last().and_then(|frame| frame.kind.as_ref()) == Some(&PendingKind::ListItem) {
            self.finish_top();
        }
        let in_list = self
            .stack
            .iter()
            .rev()
            .any(|frame| matches!(frame.kind, Some(PendingKind::List { .. })));
        if !in_list {
            self.stack.push(Frame {
                kind: Some(PendingKind::List {
                    ordered: false,
                    implicit: true,
                }),
                opener: String::new(),
                nodes: Vec::new(),
            });
        }
        self.stack.push(Frame {
            kind: Some(PendingKind::ListItem),
            opener: raw.to_owned(),
            nodes: Vec::new(),
        });
        true
    }

    fn open_table_row(&mut self, raw: &str) -> bool {
        self.close_open_table_cell();
        if self.stack.last().and_then(|frame| frame.kind.as_ref()) == Some(&PendingKind::TableRow) {
            self.finish_top();
        }
        if !self
            .stack
            .iter()
            .rev()
            .any(|frame| matches!(frame.kind, Some(PendingKind::Table { .. })))
        {
            return false;
        }
        self.open_frame(raw, PendingKind::TableRow);
        true
    }

    fn open_table_cell(&mut self, raw: &str, header: bool) -> bool {
        self.close_open_table_cell();
        if !self
            .stack
            .iter()
            .rev()
            .any(|frame| matches!(frame.kind, Some(PendingKind::TableRow)))
        {
            return false;
        }
        self.open_frame(
            raw,
            if header {
                PendingKind::TableHeader
            } else {
                PendingKind::TableCell
            },
        );
        true
    }

    fn close_open_table_cell(&mut self) {
        if matches!(
            self.stack.last().and_then(|frame| frame.kind.as_ref()),
            Some(PendingKind::TableHeader | PendingKind::TableCell)
        ) {
            self.finish_top();
        }
    }

    fn close(&mut self, name: &str) -> bool {
        let name = name.to_ascii_lowercase();
        if matches!(name.as_str(), "list" | "olist")
            && self.stack.last().and_then(|frame| frame.kind.as_ref())
                == Some(&PendingKind::ListItem)
        {
            self.finish_top();
        }
        if matches!(name.as_str(), "tr" | "table") {
            self.close_open_table_cell();
        }
        if name == "table"
            && self.stack.last().and_then(|frame| frame.kind.as_ref())
                == Some(&PendingKind::TableRow)
        {
            self.finish_top();
        }
        let Some(kind) = self.stack.last().and_then(|frame| frame.kind.as_ref()) else {
            return false;
        };
        if kind.name() != name {
            return false;
        }
        self.finish_top();
        true
    }

    fn finish_top(&mut self) {
        let frame = self.stack.pop().expect("non-root frame");
        let element = frame.kind.expect("non-root kind").finish(frame.nodes);
        self.current_nodes().push(Node::Element(element));
    }

    fn open_frame(&mut self, raw: &str, kind: PendingKind) {
        self.stack.push(Frame {
            kind: Some(kind),
            opener: raw.to_owned(),
            nodes: Vec::new(),
        });
    }

    fn push_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        if self.in_implicit_list_item()
            && let Some(boundary) = paragraph_boundary(text)
        {
            self.push_text(&text[..boundary]);
            self.finish_implicit_list();
            self.push_text(text[boundary..].trim_start_matches(['\r', '\n']));
            return;
        }
        match self.current_nodes().last_mut() {
            Some(Node::Text(current)) => current.push_str(text),
            _ => self.current_nodes().push(Node::Text(text.to_owned())),
        }
    }

    fn current_nodes(&mut self) -> &mut Vec<Node> {
        &mut self.stack.last_mut().expect("root frame").nodes
    }

    fn in_implicit_list_item(&self) -> bool {
        self.stack
            .last()
            .is_some_and(|frame| matches!(frame.kind, Some(PendingKind::ListItem)))
            && self
                .stack
                .iter()
                .rev()
                .any(|frame| matches!(frame.kind, Some(PendingKind::List { implicit: true, .. })))
    }

    fn finish_implicit_list(&mut self) {
        if self.in_implicit_list_item() {
            self.finish_top();
        }
        if matches!(
            self.stack.last().and_then(|frame| frame.kind.as_ref()),
            Some(PendingKind::List { implicit: true, .. })
        ) {
            self.finish_top();
        }
    }
}

fn paragraph_boundary(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    bytes
        .windows(2)
        .position(|window| window == b"\n\n")
        .or_else(|| bytes.windows(4).position(|window| window == b"\r\n\r\n"))
}

fn table_option_enabled(token: &str, option: &str) -> bool {
    token
        .split_ascii_whitespace()
        .skip(1)
        .filter_map(|attribute| attribute.split_once('='))
        .any(|(name, value)| name == option && value == "1")
}

fn find_ascii_case_insensitive(haystack: &str, needle: &str) -> Option<usize> {
    haystack
        .as_bytes()
        .windows(needle.len())
        .position(|window| window.eq_ignore_ascii_case(needle.as_bytes()))
}

fn linkify_nodes(nodes: Vec<Node>) -> Vec<Node> {
    let mut linked = Vec::new();
    for node in nodes {
        match node {
            Node::Text(text) => linkify_text(&text, &mut linked),
            Node::Element(mut element) => {
                if !matches!(
                    element.kind,
                    ElementKind::Code | ElementKind::Link { .. } | ElementKind::Image { .. }
                ) {
                    element.children = linkify_nodes(element.children);
                }
                linked.push(Node::Element(element));
            }
        }
    }
    linked
}

fn linkify_text(text: &str, nodes: &mut Vec<Node>) {
    let mut cursor = 0;
    while let Some(start) = next_url_start(text, cursor) {
        push_text_node(nodes, &text[cursor..start]);
        let whitespace_end = text[start..]
            .find(char::is_whitespace)
            .map_or(text.len(), |offset| start + offset);
        let mut end = whitespace_end;
        while end > start
            && text[..end].chars().next_back().is_some_and(|character| {
                matches!(
                    character,
                    '.' | ',' | ';' | ':' | '!' | '?' | ')' | ']' | '}'
                )
            })
        {
            end -= text[..end].chars().next_back().map_or(0, char::len_utf8);
        }
        if end == start {
            push_text_node(nodes, &text[start..whitespace_end]);
        } else {
            let target = text[start..end].to_owned();
            nodes.push(Node::Element(Element {
                kind: ElementKind::Link {
                    target: target.clone(),
                },
                children: vec![Node::Text(target)],
            }));
            push_text_node(nodes, &text[end..whitespace_end]);
        }
        cursor = whitespace_end;
    }
    push_text_node(nodes, &text[cursor..]);
}

fn next_url_start(text: &str, from: usize) -> Option<usize> {
    text[from..]
        .match_indices("http://")
        .chain(text[from..].match_indices("https://"))
        .map(|(offset, _)| from + offset)
        .filter(|&index| {
            index == 0
                || text[..index].chars().next_back().is_some_and(|character| {
                    character.is_whitespace() || matches!(character, '(' | '<')
                })
        })
        .min()
}

fn push_text_node(nodes: &mut Vec<Node>, text: &str) {
    if text.is_empty() {
        return;
    }
    match nodes.last_mut() {
        Some(Node::Text(current)) => current.push_str(text),
        _ => nodes.push(Node::Text(text.to_owned())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nested_inline_tags_and_links() {
        let document = Document::parse(
            "[h1]Title[/h1][b]Bold [i]and italic[/i][/b] [url=example.com]site[/url]",
        );

        assert!(matches!(
            document.nodes(),
            [
                Node::Element(Element { kind: ElementKind::Heading(1), .. }),
                Node::Element(Element { kind: ElementKind::Bold, .. }),
                Node::Text(_),
                Node::Element(Element { kind: ElementKind::Link { target }, .. }),
            ] if target == "example.com"
        ));
    }

    #[test]
    fn parses_steam_lists_without_explicit_item_closers() {
        let document = Document::parse("[list][*]One[*][b]Two[/b][/list]");
        let Node::Element(list) = &document.nodes()[0] else {
            panic!("expected list");
        };
        assert_eq!(list.kind(), &ElementKind::List { ordered: false });
        assert_eq!(list.children().len(), 2);
        assert!(list.children().iter().all(|node| matches!(
            node,
            Node::Element(Element {
                kind: ElementKind::ListItem,
                ..
            })
        )));
    }

    #[test]
    fn noparse_and_code_preserve_inner_markup() {
        let document = Document::parse("[noparse][b]raw[/b][/noparse][code][i]code[/i][/code]");
        assert_eq!(document.nodes()[0], Node::Text("[b]raw[/b]".to_owned()));
        assert!(matches!(
            &document.nodes()[1],
            Node::Element(Element { kind: ElementKind::Code, children })
                if children == &[Node::Text("[i]code[/i]".to_owned())]
        ));
    }

    #[test]
    fn unknown_and_unclosed_tags_remain_visible() {
        let document = Document::parse("[wat]text[/wat] [b]unfinished");
        assert_eq!(
            document.nodes(),
            &[Node::Text("[wat]text[/wat] [b]unfinished".to_owned())]
        );
    }

    #[test]
    fn links_plain_http_urls_without_swallowing_punctuation() {
        let document = Document::parse("See https://example.com/path, now.");
        assert!(matches!(
            document.nodes(),
            [Node::Text(before), Node::Element(Element { kind: ElementKind::Link { target }, .. }), Node::Text(after)]
                if before == "See " && target == "https://example.com/path" && after == ", now."
        ));
    }

    #[test]
    fn assigns_stable_spoiler_ids_in_source_order() {
        let document = Document::parse("[spoiler]one[/spoiler] [spoiler]two[/spoiler]");
        assert!(matches!(
            document.nodes(),
            [
                Node::Element(Element {
                    kind: ElementKind::Spoiler(SpoilerId(0)),
                    ..
                }),
                Node::Text(_),
                Node::Element(Element {
                    kind: ElementKind::Spoiler(SpoilerId(1)),
                    ..
                }),
            ]
        ));
    }

    #[test]
    fn recognizes_the_documented_steam_text_tag_set() {
        let source = "[h1]H1[/h1][h2]H2[/h2][h3]H3[/h3][b]b[/b][u]u[/u][i]i[/i]".to_owned()
            + "[strike]s[/strike][spoiler]x[/spoiler][hr][/hr]"
            + "[url]https://example.com[/url][list][*]a[/list]"
            + "[olist][*]b[/olist][quote=author]q[/quote][code]c[/code]"
            + "[noparse][b]literal[/b][/noparse]";
        let document = Document::parse(&source);

        assert!(document.nodes().iter().any(|node| matches!(
            node,
            Node::Element(Element {
                kind: ElementKind::HorizontalRule,
                ..
            })
        )));
        assert_eq!(
            document.plain_text(),
            "H1H2H3buisxhttps://example.comabqc[b]literal[/b]"
        );
    }

    #[test]
    fn parses_steam_tables_and_options() {
        let source = "[table equalcells=1][tr][th]Name[/th][th]Age[/th][/tr]".to_owned()
            + "[tr][td]Ada[/td][td]37[/td][/tr][/table]"
            + "[table noborder=1][tr][td]Borderless[/td][/tr][/table]";
        let document = Document::parse(&source);

        let Node::Element(first) = &document.nodes()[0] else {
            panic!("expected first table");
        };
        assert_eq!(
            first.kind(),
            &ElementKind::Table {
                bordered: true,
                equal_cells: true,
            }
        );
        assert_eq!(first.children().len(), 2);
        let Node::Element(second) = &document.nodes()[1] else {
            panic!("expected second table");
        };
        assert_eq!(
            second.kind(),
            &ElementKind::Table {
                bordered: false,
                equal_cells: false,
            }
        );
        assert_eq!(document.plain_text(), "NameAgeAda37Borderless");
    }

    #[test]
    fn table_rows_and_cells_tolerate_omitted_closing_tags() {
        let document = Document::parse("[table][tr][th]A[th]B[tr][td]1[td]2[/table]");
        let Node::Element(table) = &document.nodes()[0] else {
            panic!("expected table");
        };

        assert_eq!(table.children().len(), 2);
        assert!(table.children().iter().all(|row| matches!(
            row,
            Node::Element(Element {
                kind: ElementKind::TableRow,
                children,
            }) if children.len() == 2
        )));
    }

    #[test]
    fn parses_steam_image_as_a_reusable_media_node() {
        let document = Document::parse("[img] https://i.imgur.com/example.png [/IMG]");

        assert!(matches!(
            document.nodes(),
            [Node::Element(Element {
                kind: ElementKind::Image { source },
                children,
            })] if source == "https://i.imgur.com/example.png" && children.is_empty()
        ));
        assert_eq!(document.plain_text(), "https://i.imgur.com/example.png");
        assert!(!document.is_empty());
    }

    #[test]
    fn groups_standalone_steam_bullets_into_an_implicit_list() {
        let document = Document::parse("Libraries used:\n[*] RNDX\n[*] easymask");

        assert!(matches!(
            document.nodes(),
            [Node::Text(heading), Node::Element(Element {
                kind: ElementKind::List { ordered: false },
                children,
            })] if heading == "Libraries used:\n" && children.len() == 2
        ));
        assert_eq!(document.plain_text(), "Libraries used:\n RNDX\n easymask");
    }

    #[test]
    fn blank_line_ends_an_implicit_list() {
        let document = Document::parse("[*] one\n[*] two\n\nAfter");

        assert!(matches!(
            document.nodes(),
            [Node::Element(Element {
                kind: ElementKind::List { ordered: false },
                children,
            }), Node::Text(after)] if children.len() == 2 && after == "After"
        ));
    }
}
