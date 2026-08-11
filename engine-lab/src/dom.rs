use std::collections::BTreeMap;
use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Document {
    pub root: Element,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Element {
    pub name: String,
    pub attrs: BTreeMap<String, String>,
    pub children: Vec<Node>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Node {
    Element(Element),
    Text(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseError {
    pub offset: usize,
    pub message: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "parse error at byte {}: {}",
            self.offset, self.message
        )
    }
}

impl std::error::Error for ParseError {}

impl Document {
    pub fn parse(source: &str) -> Result<Self, ParseError> {
        let tokens = tokenize(source)?;
        let mut root = Element::new("#document");
        let mut stack = Vec::new();
        for token in tokens {
            match token {
                Token::Start {
                    name,
                    attrs,
                    self_closing,
                } => {
                    let element = Element {
                        name,
                        attrs,
                        children: Vec::new(),
                    };
                    if self_closing || is_void_element(&element.name) {
                        append_node(&mut root, &mut stack, Node::Element(element));
                    } else {
                        stack.push(element);
                    }
                }
                Token::End { name, offset } => {
                    let Some(index) = stack.iter().rposition(|element| element.name == name) else {
                        return Err(ParseError {
                            offset,
                            message: format!("closing tag </{name}> has no open element"),
                        });
                    };
                    while stack.len() > index {
                        let element = stack.pop().expect("stack index was checked");
                        append_node(&mut root, &mut stack, Node::Element(element));
                    }
                }
                Token::Text(text) => {
                    if !text.is_empty() {
                        append_node(&mut root, &mut stack, Node::Text(text));
                    }
                }
            }
        }
        while let Some(element) = stack.pop() {
            append_node(&mut root, &mut stack, Node::Element(element));
        }
        Ok(Self { root })
    }

    pub fn style_text(&self) -> String {
        let mut output = String::new();
        collect_style_text(&self.root, &mut output);
        output
    }
}

impl Element {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_ascii_lowercase(),
            attrs: BTreeMap::new(),
            children: Vec::new(),
        }
    }

    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .get(&name.to_ascii_lowercase())
            .map(String::as_str)
    }

    pub fn classes(&self) -> impl Iterator<Item = &str> {
        self.attr("class").unwrap_or("").split_ascii_whitespace()
    }

    pub fn has_class(&self, class: &str) -> bool {
        self.classes().any(|candidate| candidate == class)
    }
}

fn append_node(root: &mut Element, stack: &mut [Element], node: Node) {
    if let Some(parent) = stack.last_mut() {
        parent.children.push(node);
    } else {
        root.children.push(node);
    }
}

fn collect_style_text(element: &Element, output: &mut String) {
    if element.name == "style" {
        collect_text_nodes(element, output);
    }
    for child in &element.children {
        if let Node::Element(child) = child {
            collect_style_text(child, output);
        }
    }
}

fn collect_text_nodes(element: &Element, output: &mut String) {
    for child in &element.children {
        match child {
            Node::Element(child) => collect_text_nodes(child, output),
            Node::Text(text) => output.push_str(text),
        }
    }
}

fn tokenize(source: &str) -> Result<Vec<Token>, ParseError> {
    let mut tokens = Vec::new();
    let mut cursor = 0;
    while cursor < source.len() {
        let Some(relative_start) = source[cursor..].find('<') else {
            let text = decode_entities(&source[cursor..]);
            if !text.is_empty() {
                tokens.push(Token::Text(text));
            }
            break;
        };
        let start = cursor + relative_start;
        if start > cursor {
            let text = decode_entities(&source[cursor..start]);
            if !text.is_empty() {
                tokens.push(Token::Text(text));
            }
        }
        if source[start..].starts_with("<!--") {
            let Some(end) = source[start + 4..].find("-->") else {
                return Err(ParseError {
                    offset: start,
                    message: "unterminated comment".to_string(),
                });
            };
            cursor = start + 4 + end + 3;
            continue;
        }
        let Some(end) = find_tag_end(source, start + 1) else {
            return Err(ParseError {
                offset: start,
                message: "unterminated tag".to_string(),
            });
        };
        let raw = source[start + 1..end].trim();
        if raw.starts_with('!') || raw.starts_with('?') {
            cursor = end + 1;
            continue;
        }
        if let Some(raw) = raw.strip_prefix('/') {
            let name = parse_name(raw).ok_or_else(|| ParseError {
                offset: start,
                message: "closing tag has no name".to_string(),
            })?;
            tokens.push(Token::End {
                name: name.to_ascii_lowercase(),
                offset: start,
            });
        } else {
            let (name, attrs, self_closing) = parse_start_tag(raw, start)?;
            tokens.push(Token::Start {
                name,
                attrs,
                self_closing,
            });
        }
        cursor = end + 1;
    }
    Ok(tokens)
}

fn parse_start_tag(
    raw: &str,
    offset: usize,
) -> Result<(String, BTreeMap<String, String>, bool), ParseError> {
    let mut cursor = 0;
    let name = parse_name_at(raw, &mut cursor).ok_or_else(|| ParseError {
        offset,
        message: "start tag has no name".to_string(),
    })?;
    let mut attrs = BTreeMap::new();
    let mut self_closing = false;
    while cursor < raw.len() {
        skip_ascii_whitespace(raw, &mut cursor);
        if cursor >= raw.len() {
            break;
        }
        if raw[cursor..].starts_with('/') {
            self_closing = true;
            cursor += 1;
            continue;
        }
        let Some(attr_name) = parse_name_at(raw, &mut cursor) else {
            return Err(ParseError {
                offset: offset + cursor,
                message: "invalid attribute name".to_string(),
            });
        };
        skip_ascii_whitespace(raw, &mut cursor);
        let mut value = String::new();
        if raw[cursor..].starts_with('=') {
            cursor += 1;
            skip_ascii_whitespace(raw, &mut cursor);
            if cursor >= raw.len() {
                return Err(ParseError {
                    offset: offset + cursor,
                    message: "attribute has no value".to_string(),
                });
            }
            let quote = raw.as_bytes()[cursor];
            if quote == b'\'' || quote == b'"' {
                cursor += 1;
                let value_start = cursor;
                while cursor < raw.len() && raw.as_bytes()[cursor] != quote {
                    cursor += 1;
                }
                if cursor >= raw.len() {
                    return Err(ParseError {
                        offset: offset + value_start,
                        message: "unterminated attribute value".to_string(),
                    });
                }
                value = decode_entities(&raw[value_start..cursor]);
                cursor += 1;
            } else {
                let value_start = cursor;
                while cursor < raw.len() && !raw.as_bytes()[cursor].is_ascii_whitespace() {
                    cursor += 1;
                }
                value = decode_entities(&raw[value_start..cursor]);
            }
        }
        attrs.insert(attr_name, value);
    }
    Ok((name, attrs, self_closing))
}

fn parse_name(raw: &str) -> Option<&str> {
    let end = raw
        .char_indices()
        .find_map(|(index, character)| character.is_ascii_whitespace().then_some(index))
        .unwrap_or(raw.len());
    let name = raw[..end].trim_end_matches('/');
    (!name.is_empty() && name.chars().all(is_name_character)).then_some(name)
}

fn parse_name_at(raw: &str, cursor: &mut usize) -> Option<String> {
    let start = *cursor;
    while *cursor < raw.len() && is_name_character(raw.as_bytes()[*cursor] as char) {
        *cursor += 1;
    }
    (*cursor > start).then(|| raw[start..*cursor].to_ascii_lowercase())
}

fn is_name_character(character: char) -> bool {
    character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | ':' | '.')
}

fn skip_ascii_whitespace(raw: &str, cursor: &mut usize) {
    while *cursor < raw.len() && raw.as_bytes()[*cursor].is_ascii_whitespace() {
        *cursor += 1;
    }
}

fn find_tag_end(source: &str, mut cursor: usize) -> Option<usize> {
    let mut quote = None;
    while cursor < source.len() {
        let byte = source.as_bytes()[cursor];
        if quote == Some(byte) {
            quote = None;
        } else if quote.is_none() && (byte == b'\'' || byte == b'"') {
            quote = Some(byte);
        } else if quote.is_none() && byte == b'>' {
            return Some(cursor);
        }
        cursor += 1;
    }
    None
}

fn is_void_element(name: &str) -> bool {
    matches!(
        name,
        "area"
            | "base"
            | "br"
            | "col"
            | "embed"
            | "hr"
            | "img"
            | "input"
            | "link"
            | "meta"
            | "param"
            | "source"
            | "track"
            | "wbr"
    )
}

#[derive(Debug)]
enum Token {
    Start {
        name: String,
        attrs: BTreeMap<String, String>,
        self_closing: bool,
    },
    End {
        name: String,
        offset: usize,
    },
    Text(String),
}

fn decode_entities(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut cursor = 0;
    while cursor < input.len() {
        let Some(relative_ampersand) = input[cursor..].find('&') else {
            output.push_str(&input[cursor..]);
            break;
        };
        let ampersand = cursor + relative_ampersand;
        output.push_str(&input[cursor..ampersand]);
        let Some(relative_semicolon) = input[ampersand + 1..].find(';') else {
            output.push('&');
            cursor = ampersand + 1;
            continue;
        };
        let semicolon = ampersand + 1 + relative_semicolon;
        let entity = &input[ampersand + 1..semicolon];
        if let Some(decoded) = decode_entity(entity) {
            output.push(decoded);
            cursor = semicolon + 1;
        } else {
            output.push_str(&input[ampersand..=semicolon]);
            cursor = semicolon + 1;
        }
    }
    output
}

fn decode_entity(entity: &str) -> Option<char> {
    match entity {
        "amp" => Some('&'),
        "apos" => Some('\''),
        "gt" => Some('>'),
        "lt" => Some('<'),
        "nbsp" => Some('\u{a0}'),
        "quot" => Some('"'),
        _ => {
            let number = entity
                .strip_prefix("#x")
                .or_else(|| entity.strip_prefix("#X"));
            let number = number
                .map(|number| u32::from_str_radix(number, 16).ok())
                .or_else(|| entity.strip_prefix('#').map(|number| number.parse().ok()))??;
            char::from_u32(number)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nested_elements_and_attributes() {
        let document =
            Document::parse(r#"<main data-kind='demo'><p>Hello <span>world</span></p></main>"#)
                .expect("document should parse");
        let Node::Element(main) = &document.root.children[0] else {
            panic!("expected element");
        };
        assert_eq!(main.name, "main");
        assert_eq!(main.attr("data-kind"), Some("demo"));
        assert_eq!(main.children.len(), 1);
    }

    #[test]
    fn decodes_entities_and_collects_styles() {
        let document =
            Document::parse("<style>.accent { color: #fff; }</style><p>A &amp; B &#x21;</p>")
                .expect("document should parse");
        assert_eq!(document.style_text(), ".accent { color: #fff; }");
        let Node::Element(paragraph) = &document.root.children[1] else {
            panic!("expected paragraph");
        };
        assert_eq!(paragraph.children, vec![Node::Text("A & B !".to_string())]);
    }

    #[test]
    fn rejects_unterminated_tags() {
        let error = Document::parse("<main").expect_err("unterminated tag should fail");
        assert!(error.message.contains("unterminated tag"));
    }
}
