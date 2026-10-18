use std::collections::HashMap;

const MAX_INPUT: usize = 128 * 1024 * 1024;
const MAX_DECODED: usize = 256 * 1024 * 1024;
const MAX_ITEMS: usize = 8_000_000;
const MAX_NODES: usize = 200_000;
const MAX_DEPTH: usize = 128;
const MAX_STRING: usize = 1024 * 1024;

#[derive(Debug)]
pub struct Document {
    pub nodes: Vec<Node>,
    pub root: usize,
    pub format: String,
    pub version: u32,
}

#[derive(Debug)]
pub struct Node {
    pub id: Option<String>,
    pub name: String,
    pub class: String,
    pub attributes: HashMap<String, Value>,
}

#[derive(Debug, PartialEq)]
pub enum Value {
    Element(Option<usize>),
    Int(i32),
    Float(f32),
    Bool(bool),
    String(String),
    Time(i32),
    Color([u8; 4]),
    Vector2([f32; 2]),
    Vector3([f32; 3]),
    Vector4([f32; 4]),
    Quaternion([f32; 4]),
    Angle([f32; 3]),
    Matrix([f32; 16]),
    Binary(Vec<u8>),
    UInt64(u64),
    UInt8(u8),
    ElementArray(Vec<Option<usize>>),
    IntArray(Vec<i32>),
    FloatArray(Vec<f32>),
    BoolArray(Vec<bool>),
    StringArray(Vec<String>),
    TimeArray(Vec<i32>),
    ColorArray(Vec<[u8; 4]>),
    Vector2Array(Vec<[f32; 2]>),
    Vector3Array(Vec<[f32; 3]>),
    Vector4Array(Vec<[f32; 4]>),
    QuaternionArray(Vec<[f32; 4]>),
    AngleArray(Vec<[f32; 3]>),
    MatrixArray(Vec<[f32; 16]>),
    BinaryArray(Vec<Vec<u8>>),
    UInt64Array(Vec<u64>),
    UInt8Array(Vec<u8>),
}

#[derive(Default)]
struct Budget {
    bytes: usize,
    items: usize,
}

impl Budget {
    fn claim(&mut self, items: usize, size: usize) -> Result<(), String> {
        self.items = self
            .items
            .checked_add(items)
            .ok_or("DMX item count overflows")?;
        self.bytes = self
            .bytes
            .checked_add(
                items
                    .checked_mul(size)
                    .ok_or("DMX allocation size overflows")?,
            )
            .ok_or("DMX decoded size overflows")?;
        if self.items > MAX_ITEMS || self.bytes > MAX_DECODED {
            return Err("DMX decoded data exceeds the memory limit".into());
        }
        Ok(())
    }
    fn bytes(&mut self, bytes: usize) -> Result<(), String> {
        self.bytes = self
            .bytes
            .checked_add(bytes)
            .ok_or("DMX decoded size overflows")?;
        if self.bytes > MAX_DECODED {
            return Err("DMX decoded data exceeds the memory limit".into());
        }
        Ok(())
    }
    fn string(&mut self, text: &str) -> Result<String, String> {
        if text.len() > MAX_STRING {
            return Err("DMX string exceeds the length limit".into());
        }
        self.bytes(text.len())?;
        Ok(text.to_owned())
    }
}

pub fn parse(bytes: &[u8]) -> Result<Document, String> {
    if bytes.len() > MAX_INPUT {
        return Err("DMX file exceeds the 128 MiB limit".into());
    }
    let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
    let end = bytes
        .windows(3)
        .take(1024)
        .position(|w| w == b"-->")
        .ok_or("DMX header is missing or exceeds 1024 bytes")?
        + 3;
    let header = std::str::from_utf8(&bytes[..end]).map_err(|_| "DMX header is not UTF-8")?;
    let tokens: Vec<_> = header.split_whitespace().collect();
    let (encoding, encoding_version, format, version) = if tokens.len() == 9
        && tokens[0] == "<!--"
        && tokens[1] == "dmx"
        && tokens[2] == "encoding"
        && tokens[5] == "format"
        && tokens[8] == "-->"
    {
        (
            tokens[3],
            tokens[4]
                .parse::<u32>()
                .map_err(|_| "invalid DMX encoding version")?,
            tokens[6].to_owned(),
            tokens[7]
                .parse::<u32>()
                .map_err(|_| "invalid DMX model version")?,
        )
    } else {
        return Err("unsupported or invalid DMX header".into());
    };
    let mut offset = end;
    if encoding == "binary" {
        while bytes
            .get(offset)
            .is_some_and(|b| matches!(b, b'\r' | b'\n'))
        {
            offset += 1;
        }
        if bytes.get(offset) != Some(&0) {
            return Err("binary DMX header is missing its NUL terminator".into());
        }
        offset += 1;
        if !(1..=9).contains(&encoding_version) {
            return Err(format!(
                "unsupported binary DMX encoding {encoding_version}"
            ));
        }
        Binary::new(&bytes[offset..], encoding_version).parse(format, version)
    } else if matches!(encoding, "keyvalues2" | "keyvalues2_flat")
        && (1..=4).contains(&encoding_version)
    {
        let text =
            std::str::from_utf8(&bytes[offset..]).map_err(|_| "keyvalues2 DMX is not UTF-8")?;
        TextParser::new(text).parse(format, version)
    } else {
        Err(format!(
            "unsupported DMX encoding {encoding} {encoding_version}"
        ))
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
    budget: Budget,
}

impl<'a> Reader<'a> {
    fn take(&mut self, count: usize) -> Result<&'a [u8], String> {
        let end = self
            .offset
            .checked_add(count)
            .ok_or("DMX offset overflows")?;
        let result = self
            .bytes
            .get(self.offset..end)
            .ok_or_else(|| format!("DMX is truncated at byte {}", self.offset))?;
        self.offset = end;
        Ok(result)
    }
    fn byte(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }
    fn short(&mut self) -> Result<i16, String> {
        Ok(i16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn int(&mut self) -> Result<i32, String> {
        Ok(i32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn uint64(&mut self) -> Result<u64, String> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn float(&mut self) -> Result<f32, String> {
        let value = f32::from_le_bytes(self.take(4)?.try_into().unwrap());
        if !value.is_finite() {
            return Err("DMX contains a nonfinite float".into());
        }
        Ok(value)
    }
    fn vector<const N: usize>(&mut self) -> Result<[f32; N], String> {
        let mut values = [0.0; N];
        for value in &mut values {
            *value = self.float()?;
        }
        Ok(values)
    }
    fn color(&mut self) -> Result<[u8; 4], String> {
        Ok(self.take(4)?.try_into().unwrap())
    }
    fn count(&mut self) -> Result<usize, String> {
        let value = self.int()?;
        if value < 0 || value as usize > MAX_ITEMS {
            return Err(format!("invalid DMX count {value}"));
        }
        Ok(value as usize)
    }
    fn string(&mut self) -> Result<String, String> {
        let tail = &self.bytes[self.offset..];
        let length = tail
            .iter()
            .take(MAX_STRING + 1)
            .position(|b| *b == 0)
            .ok_or("DMX string is unterminated or exceeds the length limit")?;
        let text = std::str::from_utf8(self.take(length + 1)?.split_last().unwrap().1)
            .map_err(|_| "DMX string is not UTF-8")?;
        self.budget.string(text)
    }
    fn uuid(&mut self) -> Result<String, String> {
        let bytes = self.take(16)?;
        Ok(format!(
            "{:08x}{:04x}{:04x}{}",
            u32::from_le_bytes(bytes[0..4].try_into().unwrap()),
            u16::from_le_bytes(bytes[4..6].try_into().unwrap()),
            u16::from_le_bytes(bytes[6..8].try_into().unwrap()),
            bytes[8..]
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        ))
    }
}

struct Binary<'a> {
    reader: Reader<'a>,
    version: u32,
    strings: Vec<String>,
    nodes: Vec<Node>,
    table_count: usize,
    external: HashMap<String, usize>,
}

impl<'a> Binary<'a> {
    fn new(bytes: &'a [u8], version: u32) -> Self {
        Self {
            reader: Reader {
                bytes,
                offset: 0,
                budget: Budget::default(),
            },
            version,
            strings: Vec::new(),
            nodes: Vec::new(),
            table_count: 0,
            external: HashMap::new(),
        }
    }
    fn indexed_string(&mut self) -> Result<String, String> {
        if self.version < 2 {
            return self.reader.string();
        }
        let index = if self.version >= 5 {
            self.reader.int()?
        } else {
            i32::from(self.reader.short()?)
        };
        if index == -1 {
            return Ok(String::new());
        }
        let text = usize::try_from(index)
            .ok()
            .and_then(|i| self.strings.get(i))
            .ok_or_else(|| format!("invalid DMX string index {index}"))?;
        self.reader.budget.string(text)
    }
    fn reference(&mut self) -> Result<Option<usize>, String> {
        let index = self.reader.int()?;
        if index == -1 {
            return Ok(None);
        }
        if index == -2 {
            let id = id_key(&self.reader.string()?)?;
            if let Some(&node) = self.external.get(&id) {
                return Ok(Some(node));
            }
            let node = push_node(&mut self.nodes, &mut self.reader.budget, "DmElement".into())?;
            self.nodes[node].id = Some(id.clone());
            self.external.insert(id, node);
            return Ok(Some(node));
        }
        if index < 0 || index as usize >= self.table_count {
            return Err(format!(
                "invalid DMX element index {index} ({} elements)",
                self.table_count
            ));
        }
        Ok(Some(index as usize))
    }
    fn array<T>(
        &mut self,
        count: usize,
        mut read: impl FnMut(&mut Self) -> Result<T, String>,
    ) -> Result<Vec<T>, String> {
        if count > self.reader.bytes.len() - self.reader.offset {
            return Err("DMX array is truncated".into());
        }
        self.reader.budget.claim(count, std::mem::size_of::<T>())?;
        let mut result = Vec::with_capacity(count);
        for _ in 0..count {
            result.push(read(self)?);
        }
        Ok(result)
    }
    fn blob(&mut self) -> Result<Vec<u8>, String> {
        let length = self.reader.count()?;
        self.reader.budget.bytes(length)?;
        Ok(self.reader.take(length)?.to_vec())
    }
    fn value(&mut self, id: u8, prefix: bool) -> Result<Value, String> {
        let offset = if self.version >= 9 { 32 } else { 14 };
        let (kind, array) = if id > offset {
            (id - offset, true)
        } else {
            (id, false)
        };
        if array {
            let count = self.reader.count()?;
            macro_rules! array {
                ($variant:ident,$read:expr) => {
                    Ok(Value::$variant(self.array(count, $read)?))
                };
            }
            return match kind {
                1 if !prefix => array!(ElementArray, |s| s.reference()),
                2 => array!(IntArray, |s| s.reader.int()),
                3 => array!(FloatArray, |s| s.reader.float()),
                4 => array!(BoolArray, |s| Ok(s.reader.byte()? != 0)),
                5 => array!(StringArray, |s| s.reader.string()),
                6 => array!(BinaryArray, |s| s.blob()),
                7 if self.version < 3 => array!(StringArray, |s| s.reader.uuid()),
                7 => array!(TimeArray, |s| s.reader.int()),
                8 => array!(ColorArray, |s| s.reader.color()),
                9 => array!(Vector2Array, |s| s.reader.vector()),
                10 => array!(Vector3Array, |s| s.reader.vector()),
                11 => array!(Vector4Array, |s| s.reader.vector()),
                12 => array!(AngleArray, |s| s.reader.vector()),
                13 => array!(QuaternionArray, |s| s.reader.vector()),
                14 => array!(MatrixArray, |s| s.reader.vector()),
                15 if self.version >= 9 => array!(UInt64Array, |s| s.reader.uint64()),
                16 if self.version >= 9 => array!(UInt8Array, |s| s.reader.byte()),
                _ => Err(format!("unsupported DMX attribute type {id}")),
            };
        }
        Ok(match kind {
            1 if !prefix => Value::Element(self.reference()?),
            2 => Value::Int(self.reader.int()?),
            3 => Value::Float(self.reader.float()?),
            4 => Value::Bool(self.reader.byte()? != 0),
            5 => Value::String(if self.version >= 4 && !prefix {
                self.indexed_string()?
            } else {
                self.reader.string()?
            }),
            6 => Value::Binary(self.blob()?),
            7 if self.version < 3 => Value::String(self.reader.uuid()?),
            7 => Value::Time(self.reader.int()?),
            8 => Value::Color(self.reader.color()?),
            9 => Value::Vector2(self.reader.vector()?),
            10 => Value::Vector3(self.reader.vector()?),
            11 => Value::Vector4(self.reader.vector()?),
            12 => Value::Angle(self.reader.vector()?),
            13 => Value::Quaternion(self.reader.vector()?),
            14 => Value::Matrix(self.reader.vector()?),
            15 if self.version >= 9 => Value::UInt64(self.reader.uint64()?),
            16 if self.version >= 9 => Value::UInt8(self.reader.byte()?),
            _ => return Err(format!("unsupported DMX attribute type {id}")),
        })
    }
    fn parse(mut self, format: String, version: u32) -> Result<Document, String> {
        if self.version >= 7 {
            let prefix_count = self.reader.count()?;
            for _ in 0..prefix_count {
                let count = self.reader.count()?;
                self.reader.budget.claim(count, 64)?;
                for _ in 0..count {
                    self.reader.string()?;
                    let kind = self.reader.byte()?;
                    self.value(kind, true)?;
                }
            }
        }
        if self.version >= 2 {
            let count = if self.version >= 4 {
                self.reader.count()?
            } else {
                let n = self.reader.short()?;
                if n < 0 {
                    return Err("invalid DMX string table size".into());
                }
                n as usize
            };
            self.reader
                .budget
                .claim(count, std::mem::size_of::<String>())?;
            for _ in 0..count {
                self.strings.push(self.reader.string()?);
            }
        }
        self.table_count = self.reader.count()?;
        if self.table_count == 0 || self.table_count > MAX_NODES {
            return Err("DMX element table is empty or exceeds the node limit".into());
        }
        let mut ids = std::collections::HashSet::new();
        for _ in 0..self.table_count {
            let class = self.indexed_string()?;
            if (6..8).contains(&self.version) {
                self.reader.int()?;
            }
            let name = if self.version >= 4 {
                self.indexed_string()?
            } else {
                self.reader.string()?
            };
            let id = self.reader.uuid()?;
            if !ids.insert(id.clone()) {
                return Err("DMX contains duplicate element IDs".into());
            }
            let node = push_node(&mut self.nodes, &mut self.reader.budget, class)?;
            self.nodes[node].name = name;
            self.external.insert(id.clone(), node);
            self.nodes[node].id = Some(id);
        }
        for node in 0..self.table_count {
            let count = self.reader.count()?;
            self.reader.budget.claim(count, 64)?;
            for _ in 0..count {
                let name = self.indexed_string()?;
                let kind = self.reader.byte()?;
                let value = self.value(kind, false)?;
                if self.nodes[node]
                    .attributes
                    .insert(name.clone(), value)
                    .is_some()
                {
                    return Err(format!("duplicate DMX attribute {name:?}"));
                }
            }
        }
        if self.reader.bytes[self.reader.offset..]
            .iter()
            .any(|b| *b != 0)
        {
            return Err("DMX contains unexpected trailing binary data".into());
        }
        Ok(Document {
            nodes: self.nodes,
            root: 0,
            format,
            version,
        })
    }
}

fn push_node(nodes: &mut Vec<Node>, budget: &mut Budget, class: String) -> Result<usize, String> {
    if nodes.len() >= MAX_NODES {
        return Err("DMX exceeds the element limit".into());
    }
    budget.claim(1, std::mem::size_of::<Node>())?;
    let index = nodes.len();
    nodes.push(Node {
        id: None,
        name: String::new(),
        class,
        attributes: HashMap::new(),
    });
    Ok(index)
}

fn id_key(text: &str) -> Result<String, String> {
    let value = text
        .trim()
        .trim_matches(['{', '}'])
        .replace('-', "")
        .to_ascii_lowercase();
    if value.len() != 32 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!("invalid DMX element UUID {text:?}"));
    }
    Ok(value)
}

#[derive(Debug, PartialEq)]
enum Token {
    Text(String),
    Open,
    Close,
    Array,
    EndArray,
    Comma,
}

struct TextParser<'a> {
    text: &'a str,
    offset: usize,
    lookahead: Option<Token>,
    budget: Budget,
    nodes: Vec<Node>,
    ids: HashMap<String, usize>,
    pending: Vec<Reference>,
}
struct Reference {
    node: usize,
    key: String,
    slot: Option<usize>,
    id: String,
}

impl<'a> TextParser<'a> {
    fn new(text: &'a str) -> Self {
        Self {
            text,
            offset: 0,
            lookahead: None,
            budget: Budget::default(),
            nodes: Vec::new(),
            ids: HashMap::new(),
            pending: Vec::new(),
        }
    }
    fn token(&mut self) -> Result<Option<Token>, String> {
        if self.lookahead.is_some() {
            return Ok(self.lookahead.take());
        }
        let bytes = self.text.as_bytes();
        loop {
            while bytes.get(self.offset).is_some_and(u8::is_ascii_whitespace) {
                self.offset += 1;
            }
            if bytes.get(self.offset..self.offset + 2) == Some(b"//") {
                while bytes.get(self.offset).is_some_and(|b| *b != b'\n') {
                    self.offset += 1;
                }
            } else if bytes.get(self.offset..self.offset + 2) == Some(b"/*") {
                let end = self.text[self.offset + 2..]
                    .find("*/")
                    .ok_or("unterminated DMX comment")?;
                self.offset += end + 4;
            } else {
                break;
            }
        }
        let Some(&byte) = bytes.get(self.offset) else {
            return Ok(None);
        };
        self.offset += 1;
        let token = match byte {
            b'{' => Token::Open,
            b'}' => Token::Close,
            b'[' => Token::Array,
            b']' => Token::EndArray,
            b',' => Token::Comma,
            b'"' => {
                let mut result = String::new();
                let mut start = self.offset;
                loop {
                    let byte = *bytes.get(self.offset).ok_or("unterminated DMX string")?;
                    if matches!(byte, b'"' | b'\\') {
                        result.push_str(&self.text[start..self.offset]);
                        self.offset += 1;
                        if byte == b'"' {
                            break;
                        }
                        let escape = *bytes.get(self.offset).ok_or("unterminated DMX escape")?;
                        self.offset += 1;
                        result.push(match escape {
                            b'n' => '\n',
                            b'r' => '\r',
                            b't' => '\t',
                            b'"' => '"',
                            b'\\' => '\\',
                            _ => return Err("invalid DMX escape".into()),
                        });
                        start = self.offset;
                    } else {
                        self.offset += 1;
                    }
                    if self.offset - start + result.len() > MAX_STRING {
                        return Err("DMX string exceeds the length limit".into());
                    }
                }
                self.budget.bytes(result.len())?;
                Token::Text(result)
            }
            _ => {
                let start = self.offset - 1;
                while bytes
                    .get(self.offset)
                    .is_some_and(|b| !b.is_ascii_whitespace() && !b"{}[],".contains(b))
                {
                    self.offset += 1;
                }
                Token::Text(self.budget.string(&self.text[start..self.offset])?)
            }
        };
        Ok(Some(token))
    }
    fn peek(&mut self) -> Result<Option<&Token>, String> {
        if self.lookahead.is_none() {
            self.lookahead = self.token()?;
        }
        Ok(self.lookahead.as_ref())
    }
    fn expect(&mut self, expected: Token) -> Result<(), String> {
        let value = self.token()?;
        if value != Some(expected) {
            return Err(format!("unexpected DMX token {value:?}"));
        }
        Ok(())
    }
    fn string(&mut self) -> Result<String, String> {
        match self.token()? {
            Some(Token::Text(s)) => Ok(s),
            token => Err(format!("expected DMX value, got {token:?}")),
        }
    }
    fn node(&mut self, class: String, depth: usize) -> Result<usize, String> {
        if depth > MAX_DEPTH {
            return Err("DMX element nesting exceeds 128 levels".into());
        }
        self.expect(Token::Open)?;
        let index = push_node(&mut self.nodes, &mut self.budget, class)?;
        loop {
            if self.peek()? == Some(&Token::Close) {
                self.token()?;
                break;
            }
            let key = self.string()?;
            let kind = self.string()?;
            self.budget.claim(1, 64)?;
            let value = self.value(&kind, index, &key, depth)?;
            if key == "name" {
                if let Value::String(name) = &value {
                    self.nodes[index].name = self.budget.string(name)?;
                } else {
                    return Err("DMX element name must be a string".into());
                }
            }
            if key == "id" {
                if let Value::String(id) = &value {
                    let id = id_key(id)?;
                    self.nodes[index].id = Some(id.clone());
                    if self.ids.insert(id, index).is_some() {
                        return Err("duplicate DMX element ID".into());
                    }
                } else {
                    return Err("DMX element ID must be an elementid".into());
                }
            }
            if self.nodes[index]
                .attributes
                .insert(key.clone(), value)
                .is_some()
            {
                return Err(format!("duplicate DMX attribute {key:?}"));
            }
        }
        Ok(index)
    }
    fn reference(
        &mut self,
        node: usize,
        key: &str,
        slot: Option<usize>,
        id: String,
    ) -> Result<Option<usize>, String> {
        if id.is_empty() {
            return Ok(None);
        }
        self.budget.claim(1, std::mem::size_of::<Reference>())?;
        self.pending.push(Reference {
            node,
            key: self.budget.string(key)?,
            slot,
            id: id_key(&id)?,
        });
        Ok(None)
    }
    fn value(&mut self, kind: &str, node: usize, key: &str, depth: usize) -> Result<Value, String> {
        if let Some(base) = kind.strip_suffix("_array") {
            self.expect(Token::Array)?;
            let mut values = Vec::new();
            loop {
                if self.peek()? == Some(&Token::EndArray) {
                    self.token()?;
                    break;
                }
                if self.peek()? == Some(&Token::Comma) {
                    self.token()?;
                    continue;
                }
                if values.len() >= MAX_ITEMS {
                    return Err("DMX array exceeds the item limit".into());
                }
                self.budget.claim(1, std::mem::size_of::<Value>())?;
                let value = if base == "element" {
                    let class = self.string()?;
                    if self.peek()? == Some(&Token::Open) {
                        Value::Element(Some(self.node(class, depth + 1)?))
                    } else {
                        let id = self.string()?;
                        Value::Element(self.reference(node, key, Some(values.len()), id)?)
                    }
                } else {
                    let text = self.string()?;
                    text_value(base, &text)?
                };
                values.push(value);
            }
            return pack_array(base, values);
        }
        if self.peek()? == Some(&Token::Open) {
            return Ok(Value::Element(Some(self.node(kind.to_owned(), depth + 1)?)));
        }
        let text = self.string()?;
        if kind == "element" {
            Ok(Value::Element(self.reference(node, key, None, text)?))
        } else {
            text_value(kind, &text)
        }
    }
    fn parse(mut self, format: String, version: u32) -> Result<Document, String> {
        let mut root = None;
        while self.peek()?.is_some() {
            let class = self.string()?;
            let prefix = class == "$prefix_element$";
            let node = self.node(class, 0)?;
            if !prefix && root.is_none() {
                root = Some(node);
            }
        }
        let root = root.ok_or("DMX contains no elements")?;
        for reference in self.pending {
            let target = if let Some(&target) = self.ids.get(&reference.id) {
                target
            } else {
                let node = push_node(&mut self.nodes, &mut self.budget, "DmElement".into())?;
                self.nodes[node].id = Some(reference.id.clone());
                self.ids.insert(reference.id, node);
                node
            };
            let value = self.nodes[reference.node]
                .attributes
                .get_mut(&reference.key)
                .ok_or("DMX reference lost its attribute")?;
            match (value, reference.slot) {
                (Value::Element(value), None) => *value = Some(target),
                (Value::ElementArray(values), Some(slot)) => {
                    *values
                        .get_mut(slot)
                        .ok_or("DMX element array reference is out of range")? = Some(target)
                }
                _ => return Err("DMX reference has an incompatible attribute type".into()),
            }
        }
        Ok(Document {
            nodes: self.nodes,
            root,
            format,
            version,
        })
    }
}

fn vector<const N: usize>(text: &str) -> Result<[f32; N], String> {
    let mut words = text.split_whitespace();
    let mut result = [0.0_f32; N];
    for value in &mut result {
        *value = words
            .next()
            .ok_or("DMX vector has too few components")?
            .parse()
            .map_err(|_| "invalid DMX vector component")?;
        if !value.is_finite() {
            return Err("DMX vector contains nonfinite values".into());
        }
    }
    if words.next().is_some() {
        return Err("DMX vector has too many components".into());
    }
    Ok(result)
}

fn text_value(kind: &str, text: &str) -> Result<Value, String> {
    Ok(match kind {
        "int" => Value::Int(text.parse().map_err(|_| "invalid DMX integer")?),
        "float" => Value::Float(vector::<1>(text)?[0]),
        "bool" => Value::Bool(match text.to_ascii_lowercase().as_str() {
            "1" | "true" => true,
            "0" | "false" => false,
            _ => return Err("invalid DMX boolean".into()),
        }),
        "string" | "elementid" => Value::String(text.to_owned()),
        "time" => {
            let seconds = vector::<1>(text)?[0];
            let ticks = (seconds * 10000.0 + 0.5).floor();
            if f64::from(ticks) < i32::MIN as f64 || f64::from(ticks) > i32::MAX as f64 {
                return Err("DMX time is out of range".into());
            }
            Value::Time(ticks as i32)
        }
        "color" => {
            let mut words = text.split_whitespace();
            let mut values = [0; 4];
            for value in &mut values {
                *value = words
                    .next()
                    .ok_or("DMX color has too few components")?
                    .parse()
                    .map_err(|_| "invalid DMX color")?;
            }
            if words.next().is_some() {
                return Err("DMX color has too many components".into());
            }
            Value::Color(values)
        }
        "vector2" => Value::Vector2(vector(text)?),
        "vector3" => Value::Vector3(vector(text)?),
        "vector4" => Value::Vector4(vector(text)?),
        "angle" | "qangle" => Value::Angle(vector(text)?),
        "quaternion" => Value::Quaternion(vector(text)?),
        "matrix" => Value::Matrix(vector(text)?),
        "binary" => {
            let chars: Vec<_> = text.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
            if chars.len() % 2 != 0 {
                return Err("DMX binary string has an odd number of hex digits".into());
            }
            let mut bytes = Vec::new();
            for pair in chars.chunks_exact(2) {
                let value = std::str::from_utf8(pair).map_err(|_| "invalid DMX binary data")?;
                bytes.push(
                    u8::from_str_radix(value, 16).map_err(|_| "invalid DMX binary hex digit")?,
                );
            }
            Value::Binary(bytes)
        }
        "uint64" => Value::UInt64(
            if let Some(hex) = text.strip_prefix("0x") {
                u64::from_str_radix(hex, 16)
            } else {
                text.parse()
            }
            .map_err(|_| "invalid DMX uint64")?,
        ),
        "uint8" => Value::UInt8(text.parse().map_err(|_| "invalid DMX uint8")?),
        _ => return Err(format!("unsupported DMX attribute type {kind:?}")),
    })
}

fn pack_array(kind: &str, values: Vec<Value>) -> Result<Value, String> {
    macro_rules! pack {
        ($scalar:ident,$array:ident) => {{
            let mut result = Vec::with_capacity(values.len());
            for value in values {
                match value {
                    Value::$scalar(value) => result.push(value),
                    _ => return Err("DMX array has mixed attribute types".into()),
                }
            }
            Value::$array(result)
        }};
    }
    Ok(match kind {
        "element" => pack!(Element, ElementArray),
        "int" => pack!(Int, IntArray),
        "float" => pack!(Float, FloatArray),
        "bool" => pack!(Bool, BoolArray),
        "string" | "elementid" => pack!(String, StringArray),
        "time" => pack!(Time, TimeArray),
        "color" => pack!(Color, ColorArray),
        "vector2" => pack!(Vector2, Vector2Array),
        "vector3" => pack!(Vector3, Vector3Array),
        "vector4" => pack!(Vector4, Vector4Array),
        "angle" | "qangle" => pack!(Angle, AngleArray),
        "quaternion" => pack!(Quaternion, QuaternionArray),
        "matrix" => pack!(Matrix, MatrixArray),
        "binary" => pack!(Binary, BinaryArray),
        "uint64" => pack!(UInt64, UInt64Array),
        "uint8" => pack!(UInt8, UInt8Array),
        _ => return Err(format!("unsupported DMX array type {kind:?}")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binary_fixture(version: u32) -> (Vec<u8>, usize, usize) {
        fn int(bytes: &mut Vec<u8>, value: i32) {
            bytes.extend(value.to_le_bytes());
        }
        fn raw(bytes: &mut Vec<u8>, value: &str) {
            bytes.extend(value.as_bytes());
            bytes.push(0);
        }
        fn indexed(bytes: &mut Vec<u8>, version: u32, index: i32, strings: &[&str]) {
            if version == 1 {
                raw(bytes, strings[index as usize]);
            } else if version >= 5 {
                int(bytes, index);
            } else {
                bytes.extend((index as i16).to_le_bytes());
            }
        }
        let strings = [
            "DmElement",
            "root",
            "child",
            "childRef",
            "positions",
            "labels",
            "self",
            "enabled",
        ];
        let mut bytes =
            format!("<!-- dmx encoding binary {version} format model 22 -->\n\0").into_bytes();
        if version >= 7 {
            int(&mut bytes, 0);
        }
        if version >= 2 {
            if version >= 4 {
                int(&mut bytes, strings.len() as i32);
            } else {
                bytes.extend((strings.len() as i16).to_le_bytes());
            }
            for string in strings {
                raw(&mut bytes, string);
            }
        }
        int(&mut bytes, 2);
        for index in [1, 2] {
            indexed(&mut bytes, version, 0, &strings);
            if (6..8).contains(&version) {
                int(&mut bytes, -1);
            }
            if version >= 4 {
                indexed(&mut bytes, version, index, &strings);
            } else {
                raw(&mut bytes, strings[index as usize]);
            }
            bytes.extend([index as u8; 16]);
        }
        int(&mut bytes, 4);
        indexed(&mut bytes, version, 3, &strings);
        bytes.push(1);
        let reference = bytes.len();
        int(&mut bytes, 1);
        indexed(&mut bytes, version, 4, &strings);
        bytes.push(if version >= 9 { 42 } else { 24 });
        let count = bytes.len();
        int(&mut bytes, 2);
        for value in [1.0_f32, 2.0, 3.0, 4.0, 5.0, 6.0] {
            bytes.extend(value.to_le_bytes());
        }
        indexed(&mut bytes, version, 5, &strings);
        bytes.push(if version >= 9 { 37 } else { 19 });
        int(&mut bytes, 2);
        raw(&mut bytes, "one");
        raw(&mut bytes, "two");
        indexed(&mut bytes, version, 6, &strings);
        bytes.push(1);
        int(&mut bytes, 0);
        int(&mut bytes, 1);
        indexed(&mut bytes, version, 7, &strings);
        bytes.push(4);
        bytes.push(1);
        (bytes, reference, count)
    }

    #[test]
    fn binary_versions_preserve_typed_arrays_names_and_cyclic_references() {
        for version in 1..=9 {
            let (bytes, _, _) = binary_fixture(version);
            let document = parse(&bytes).unwrap();
            assert_eq!(document.format, "model");
            assert_eq!(document.version, 22);
            assert_eq!(document.nodes.len(), 2);
            assert_eq!(document.nodes[0].name, "root");
            assert_eq!(
                document.nodes[0].attributes["childRef"],
                Value::Element(Some(1))
            );
            assert_eq!(
                document.nodes[0].attributes["self"],
                Value::Element(Some(0))
            );
            assert_eq!(
                document.nodes[0].attributes["positions"],
                Value::Vector3Array(vec![[1.0, 2.0, 3.0], [4.0, 5.0, 6.0]])
            );
            assert_eq!(
                document.nodes[0].attributes["labels"],
                Value::StringArray(vec!["one".into(), "two".into()])
            );
            assert_eq!(document.nodes[1].attributes["enabled"], Value::Bool(true));
        }
    }

    #[test]
    fn binary_truncation_invalid_references_and_count_bombs_fail_before_allocation() {
        for version in [1, 5, 9] {
            let (bytes, reference, count) = binary_fixture(version);
            for end in 0..bytes.len() {
                assert!(parse(&bytes[..end]).is_err(), "version={version},end={end}");
            }
            let mut invalid = bytes.clone();
            invalid[reference..reference + 4].copy_from_slice(&2_i32.to_le_bytes());
            assert!(parse(&invalid).unwrap_err().contains("element index"));
            invalid = bytes.clone();
            invalid[count..count + 4].copy_from_slice(&i32::MAX.to_le_bytes());
            assert!(parse(&invalid).is_err());
            invalid = bytes;
            invalid[count..count + 4].copy_from_slice(&(-1_i32).to_le_bytes());
            assert!(parse(&invalid).is_err());
        }
    }

    #[test]
    fn keyvalues2_resolves_forward_inline_and_self_references_without_owning_cycles() {
        let text = r#"<!-- dmx encoding keyvalues2 1 format model 22 -->
            "DmElement" {
                "id" "elementid" "00112233-4455-6677-8899-aabbccddeeff"
                "name" "string" "Café"
                "self" "element" "00112233-4455-6677-8899-aabbccddeeff"
                "children" "element_array" [
                    "element" "11112233-4455-6677-8899-aabbccddeeff",
                    "DmeDag" {
                        "id" "elementid" "22112233-4455-6677-8899-aabbccddeeff"
                        "name" "string" "inline"
                        "owner" "element" "00112233-4455-6677-8899-aabbccddeeff"
                    }
                ]
                "positions" "vector3_array" ["1 2 3", "4 5 6"]
                "rotation" "quaternion" "0 0 0 1"
                "times" "time_array" ["0.00005", "-0.00005", "1.25"]
                "data" "binary" "
                    00 ff 7e
                "
            }
            "DmeDag" {
                "id" "elementid" "11112233-4455-6677-8899-aabbccddeeff"
                "name" "string" "forward"
            }
        "#;
        let document = parse(text.as_bytes()).unwrap();
        assert_eq!(document.nodes.len(), 3);
        assert_eq!(document.nodes[0].name, "Café");
        assert_eq!(
            document.nodes[0].attributes["self"],
            Value::Element(Some(0))
        );
        assert_eq!(
            document.nodes[0].attributes["children"],
            Value::ElementArray(vec![Some(2), Some(1)])
        );
        assert_eq!(
            document.nodes[1].attributes["owner"],
            Value::Element(Some(0))
        );
        assert_eq!(document.nodes[2].name, "forward");
        assert_eq!(
            document.nodes[0].attributes["times"],
            Value::TimeArray(vec![1, 0, 12500])
        );
        assert_eq!(
            document.nodes[0].attributes["data"],
            Value::Binary(vec![0, 255, 126])
        );
    }

    #[test]
    fn keyvalues2_rejects_duplicate_ids_nonfinite_values_and_deep_nesting() {
        let header = "<!-- dmx encoding keyvalues2 1 format model 22 -->\n";
        for body in [
            r#""DmElement" { "id" "elementid" "00112233-4455-6677-8899-aabbccddeeff" } "DmElement" { "id" "elementid" "00112233-4455-6677-8899-aabbccddeeff" }"#,
            r#""DmElement" { "position" "vector3" "NaN 0 0" }"#,
            r#""DmElement" { "position" "vector3" "1 2" }"#,
            r#""DmElement" { "position" "vector3" "1 2 3 4" }"#,
            r#""DmElement" { "position" "float_array" ["inf"] }"#,
            r#""DmElement" { "position" "float" "0" "position" "float" "1" }"#,
        ] {
            assert!(
                parse(format!("{header}{body}").as_bytes()).is_err(),
                "{body}"
            );
        }
        let mut text = format!("{header}\"DmElement\"{{");
        for _ in 0..MAX_DEPTH + 1 {
            text.push_str("\"child\" \"DmElement\" {");
        }
        for _ in 0..MAX_DEPTH + 2 {
            text.push('}');
        }
        assert!(parse(text.as_bytes()).unwrap_err().contains("nesting"));
    }
}
