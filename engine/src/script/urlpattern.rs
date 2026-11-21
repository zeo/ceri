// URLPattern as the URL Pattern standard gives it: each of the eight URL components is a
// pattern string tokenized, parsed into parts, canonicalized through the URL parser and
// compiled to a regular expression whose groups name what matched

use super::object::{JsObject, ObjectClass, ObjectHandle, Property, Slot, new_object};
use super::operations::{is_primitive, plain_object};
use super::pattern::{Flags, Regex};
use super::realm::{Builder, Native};
use super::*;
use crate::weburl::Url;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum PatternMember {
    Constructor,
    Test,
    Exec,
    Component(u8),
    HasRegExpGroups,
}

const COMPONENTS: [&str; 8] = [
    "protocol", "username", "password", "hostname", "port", "pathname", "search", "hash",
];

const SPECIAL_SCHEMES: [&str; 6] = ["ftp", "file", "http", "https", "ws", "wss"];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum TokenKind {
    Open,
    Close,
    Regexp,
    Name,
    Char,
    EscapedChar,
    OtherModifier,
    Asterisk,
    End,
    InvalidChar,
}

#[derive(Clone, Debug)]
struct Token {
    kind: TokenKind,
    // where it starts in the input, in code points
    index: usize,
    value: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Modifier {
    None,
    Optional,
    ZeroOrMore,
    OneOrMore,
}

impl Modifier {
    fn text(self) -> &'static str {
        match self {
            Modifier::None => "",
            Modifier::Optional => "?",
            Modifier::ZeroOrMore => "*",
            Modifier::OneOrMore => "+",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PartKind {
    FixedText,
    Regexp,
    SegmentWildcard,
    FullWildcard,
}

#[derive(Clone, Debug)]
struct Part {
    kind: PartKind,
    value: String,
    modifier: Modifier,
    name: String,
    prefix: String,
    suffix: String,
}

#[derive(Clone, Copy)]
struct Options {
    delimiter: Option<char>,
    prefix: Option<char>,
    ignore_case: bool,
}

const DEFAULT: Options = Options {
    delimiter: None,
    prefix: None,
    ignore_case: false,
};

#[derive(Clone, Copy)]
enum Canonical {
    Protocol,
    Username,
    Password,
    Hostname,
    Ipv6Hostname,
    Port,
    Pathname,
    OpaquePathname,
    Search,
    Hash,
}

pub(super) struct Component {
    pattern: String,
    regex: Regex,
    names: Vec<String>,
    has_groups: bool,
}

pub(super) struct UrlPattern {
    components: Vec<Component>,
}

pub(super) struct PatternInterfaces {
    prototype: ObjectHandle,
}

pub(super) fn create(
    build: &Builder,
    object_prototype: &ObjectHandle,
    globals: &mut Vec<(&'static str, Value)>,
) -> PatternInterfaces {
    let prototype = new_object(JsObject::with_prototype(
        Some(object_prototype.clone()),
        ObjectClass::Ordinary,
    ));
    let getter = |name: &str, member: PatternMember| {
        let get = build.function(Native::UrlPattern(member), &format!("get {name}"), 0);
        prototype.borrow_mut().define(
            name.to_string(),
            Property {
                slot: Slot::Accessor {
                    get: Some(Value::Object(get)),
                    set: None,
                },
                enumerable: true,
                writable: false,
                configurable: true,
            },
        );
    };
    for (index, name) in COMPONENTS.iter().enumerate() {
        getter(name, PatternMember::Component(index as u8));
    }
    getter("hasRegExpGroups", PatternMember::HasRegExpGroups);
    for (name, member) in [("test", PatternMember::Test), ("exec", PatternMember::Exec)] {
        let function = build.function(Native::UrlPattern(member), name, 0);
        prototype.borrow_mut().define(
            name.to_string(),
            Property {
                enumerable: true,
                ..Property::data(Value::Object(function))
            },
        );
    }
    prototype.borrow_mut().define(
        "@@toStringTag".to_string(),
        Property {
            writable: false,
            ..Property::hidden(Value::String("URLPattern".into()))
        },
    );
    let constructor = build.constructor(
        Native::UrlPattern(PatternMember::Constructor),
        "URLPattern",
        0,
        &prototype,
    );
    globals.push(("URLPattern", Value::Object(constructor)));
    PatternInterfaces { prototype }
}

fn type_error(message: impl Into<String>) -> String {
    message.into()
}

fn is_name_char(character: char, first: bool) -> bool {
    if first {
        super::lexer::is_identifier_start(character)
    } else {
        super::lexer::is_identifier_part(character) || matches!(character, '\u{200C}' | '\u{200D}')
    }
}

// the standard's tokenizer; a lenient run turns what does not fit into invalid-char tokens
fn tokenize(input: &[char], strict: bool) -> Result<Vec<Token>, String> {
    let mut tokens = Vec::new();
    let mut index = 0;
    let push = |tokens: &mut Vec<Token>, kind, at: usize, value: String| {
        tokens.push(Token {
            kind,
            index: at,
            value,
        });
    };
    let invalid =
        |tokens: &mut Vec<Token>, at: usize, next: usize, message: &str| -> Result<(), String> {
            if strict {
                return Err(type_error(format!("Invalid pattern: {message}")));
            }
            tokens.push(Token {
                kind: TokenKind::InvalidChar,
                index: at,
                value: input[at..next].iter().collect(),
            });
            Ok(())
        };
    while index < input.len() {
        let character = input[index];
        match character {
            '*' => {
                push(&mut tokens, TokenKind::Asterisk, index, "*".into());
                index += 1;
            }
            '+' | '?' => {
                push(
                    &mut tokens,
                    TokenKind::OtherModifier,
                    index,
                    character.to_string(),
                );
                index += 1;
            }
            '\\' => {
                if index + 1 == input.len() {
                    invalid(&mut tokens, index, index + 1, "a trailing backslash")?;
                    index += 1;
                } else {
                    push(
                        &mut tokens,
                        TokenKind::EscapedChar,
                        index,
                        input[index + 1].to_string(),
                    );
                    index += 2;
                }
            }
            '{' => {
                push(&mut tokens, TokenKind::Open, index, "{".into());
                index += 1;
            }
            '}' => {
                push(&mut tokens, TokenKind::Close, index, "}".into());
                index += 1;
            }
            ':' => {
                let mut end = index + 1;
                while end < input.len() && is_name_char(input[end], end == index + 1) {
                    end += 1;
                }
                if end == index + 1 {
                    invalid(
                        &mut tokens,
                        index,
                        end,
                        "a name with nothing after the colon",
                    )?;
                    index = end;
                    continue;
                }
                push(
                    &mut tokens,
                    TokenKind::Name,
                    index,
                    input[index + 1..end].iter().collect(),
                );
                index = end;
            }
            '(' => {
                let mut depth = 1;
                let mut end = index + 1;
                let mut error = false;
                if input.get(end) == Some(&'?') {
                    error = true;
                }
                while !error && end < input.len() {
                    let current = input[end];
                    if !current.is_ascii() {
                        error = true;
                        break;
                    }
                    if current == '\\' {
                        if end + 1 == input.len() || !input[end + 1].is_ascii() {
                            error = true;
                            break;
                        }
                        end += 2;
                        continue;
                    }
                    if current == ')' {
                        depth -= 1;
                        if depth == 0 {
                            end += 1;
                            break;
                        }
                    } else if current == '(' {
                        depth += 1;
                        if end + 1 == input.len() || input[end + 1] != '?' {
                            error = true;
                            break;
                        }
                    }
                    end += 1;
                }
                if error || depth != 0 || end - index <= 2 {
                    invalid(&mut tokens, index, index + 1, "a regular expression group")?;
                    index += 1;
                    continue;
                }
                push(
                    &mut tokens,
                    TokenKind::Regexp,
                    index,
                    input[index + 1..end - 1].iter().collect(),
                );
                index = end;
            }
            _ => {
                push(&mut tokens, TokenKind::Char, index, character.to_string());
                index += 1;
            }
        }
    }
    push(&mut tokens, TokenKind::End, input.len(), String::new());
    Ok(tokens)
}

fn escape_regexp(text: &str) -> String {
    let mut escaped = String::new();
    for character in text.chars() {
        if ".+*?^${}()[]|/\\".contains(character) {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

fn escape_pattern(text: &str) -> String {
    let mut escaped = String::new();
    for character in text.chars() {
        if "+*?:{}()\\".contains(character) {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

fn segment_wildcard(options: Options) -> String {
    match options.delimiter {
        Some(delimiter) => format!("[^{}]+?", escape_regexp(&delimiter.to_string())),
        None => ".+?".to_string(),
    }
}

// parse a pattern string into parts, each fixed text run through encode
fn parse_pattern(
    input: &str,
    options: Options,
    encode: &dyn Fn(&str) -> Result<String, String>,
) -> Result<Vec<Part>, String> {
    let characters: Vec<char> = input.chars().collect();
    let tokens = tokenize(&characters, true)?;
    let wildcard = segment_wildcard(options);
    let mut parts: Vec<Part> = Vec::new();
    let mut pending = String::new();
    let mut index = 0;
    let mut next_name = 0;
    let take = |index: &mut usize, kinds: &[TokenKind]| -> Option<Token> {
        let token = &tokens[*index];
        if kinds.contains(&token.kind) {
            *index += 1;
            Some(token.clone())
        } else {
            None
        }
    };
    let flush = |pending: &mut String, parts: &mut Vec<Part>| -> Result<(), String> {
        if pending.is_empty() {
            return Ok(());
        }
        let value = encode(pending)?;
        pending.clear();
        parts.push(Part {
            kind: PartKind::FixedText,
            value,
            modifier: Modifier::None,
            name: String::new(),
            prefix: String::new(),
            suffix: String::new(),
        });
        Ok(())
    };
    let modifier_of = |token: Option<Token>| match token.as_ref().map(|token| token.value.as_str())
    {
        Some("?") => Modifier::Optional,
        Some("*") => Modifier::ZeroOrMore,
        Some("+") => Modifier::OneOrMore,
        _ => Modifier::None,
    };
    let mut add = |parts: &mut Vec<Part>,
                   pending: &mut String,
                   prefix: String,
                   name: Option<Token>,
                   regexp: Option<Token>,
                   suffix: String,
                   modifier: Modifier|
     -> Result<(), String> {
        if name.is_none() && regexp.is_none() && modifier == Modifier::None {
            pending.push_str(&prefix);
            return Ok(());
        }
        flush(pending, parts)?;
        if name.is_none() && regexp.is_none() {
            if !prefix.is_empty() {
                parts.push(Part {
                    kind: PartKind::FixedText,
                    value: encode(&prefix)?,
                    modifier,
                    name: String::new(),
                    prefix: String::new(),
                    suffix: String::new(),
                });
            }
            return Ok(());
        }
        let value = match &regexp {
            None => wildcard.clone(),
            Some(token) if token.kind == TokenKind::Asterisk => ".*".to_string(),
            Some(token) => token.value.clone(),
        };
        let kind = if value == wildcard {
            PartKind::SegmentWildcard
        } else if value == ".*" {
            PartKind::FullWildcard
        } else {
            PartKind::Regexp
        };
        let name = match name {
            Some(token) => token.value,
            None => {
                let numbered = next_name.to_string();
                next_name += 1;
                numbered
            }
        };
        if parts
            .iter()
            .any(|part| part.kind != PartKind::FixedText && part.name == name)
        {
            return Err(type_error(format!(
                "Invalid pattern: the name {name} is used twice"
            )));
        }
        parts.push(Part {
            kind,
            value,
            modifier,
            name,
            prefix: encode(&prefix)?,
            suffix: encode(&suffix)?,
        });
        Ok(())
    };
    let text = |index: &mut usize| {
        let mut result = String::new();
        while let TokenKind::Char | TokenKind::EscapedChar = tokens[*index].kind {
            result.push_str(&tokens[*index].value);
            *index += 1;
        }
        result
    };
    while index < tokens.len() {
        let char_token = take(&mut index, &[TokenKind::Char]);
        let name_token = take(&mut index, &[TokenKind::Name]);
        let regexp_token = take(&mut index, &[TokenKind::Regexp, TokenKind::Asterisk]);
        if name_token.is_some() || regexp_token.is_some() {
            let mut prefix = char_token.map(|token| token.value).unwrap_or_default();
            if !prefix.is_empty()
                && Some(prefix.as_str()) != options.prefix.map(|c| c.to_string()).as_deref()
            {
                pending.push_str(&prefix);
                prefix.clear();
            }
            flush(&mut pending, &mut parts)?;
            let modifier = modifier_of(take(
                &mut index,
                &[TokenKind::OtherModifier, TokenKind::Asterisk],
            ));
            add(
                &mut parts,
                &mut pending,
                prefix,
                name_token,
                regexp_token,
                String::new(),
                modifier,
            )?;
            continue;
        }
        let fixed = char_token.or_else(|| take(&mut index, &[TokenKind::EscapedChar]));
        if let Some(fixed) = fixed {
            pending.push_str(&fixed.value);
            continue;
        }
        if take(&mut index, &[TokenKind::Open]).is_some() {
            let prefix = text(&mut index);
            let name_token = take(&mut index, &[TokenKind::Name]);
            let regexp_token = take(&mut index, &[TokenKind::Regexp, TokenKind::Asterisk]);
            let suffix = text(&mut index);
            if take(&mut index, &[TokenKind::Close]).is_none() {
                return Err(type_error("Invalid pattern: a group is not closed"));
            }
            let modifier = modifier_of(take(
                &mut index,
                &[TokenKind::OtherModifier, TokenKind::Asterisk],
            ));
            add(
                &mut parts,
                &mut pending,
                prefix,
                name_token,
                regexp_token,
                suffix,
                modifier,
            )?;
            continue;
        }
        flush(&mut pending, &mut parts)?;
        if take(&mut index, &[TokenKind::End]).is_none() {
            return Err(type_error("Invalid pattern: unexpected character"));
        }
    }
    Ok(parts)
}

fn regexp_source(parts: &[Part], options: Options) -> String {
    let mut source = String::from("^");
    let wildcard = segment_wildcard(options);
    for part in parts {
        let modifier = part.modifier.text();
        if part.kind == PartKind::FixedText {
            if part.modifier == Modifier::None {
                source.push_str(&escape_regexp(&part.value));
            } else {
                source.push_str(&format!("(?:{}){modifier}", escape_regexp(&part.value)));
            }
            continue;
        }
        let value = match part.kind {
            PartKind::SegmentWildcard => wildcard.clone(),
            PartKind::FullWildcard => ".*".to_string(),
            _ => part.value.clone(),
        };
        let (prefix, suffix) = (escape_regexp(&part.prefix), escape_regexp(&part.suffix));
        let repeated = matches!(part.modifier, Modifier::ZeroOrMore | Modifier::OneOrMore);
        if part.prefix.is_empty() && part.suffix.is_empty() {
            if repeated {
                source.push_str(&format!("((?:{value}){modifier})"));
            } else {
                source.push_str(&format!("({value}){modifier}"));
            }
            continue;
        }
        if !repeated {
            source.push_str(&format!("(?:{prefix}({value}){suffix}){modifier}"));
            continue;
        }
        source.push_str(&format!(
            "(?:{prefix}((?:{value})(?:{suffix}{prefix}(?:{value}))*){suffix}){}",
            if part.modifier == Modifier::ZeroOrMore {
                "?"
            } else {
                ""
            }
        ));
    }
    source.push('$');
    source
}

fn numeric_name(name: &str) -> bool {
    name.starts_with(|character: char| character.is_ascii_digit())
}

// the canonical pattern string a component reports, as the standard writes parts back out
fn pattern_string(parts: &[Part], options: Options) -> String {
    let wildcard = segment_wildcard(options);
    let prefix_text = options.prefix.map(|c| c.to_string()).unwrap_or_default();
    let mut result = String::new();
    for (index, part) in parts.iter().enumerate() {
        let previous = index.checked_sub(1).and_then(|at| parts.get(at));
        let next = parts.get(index + 1);
        if part.kind == PartKind::FixedText {
            if part.modifier == Modifier::None {
                result.push_str(&escape_pattern(&part.value));
            } else {
                result.push_str(&format!(
                    "{{{}}}{}",
                    escape_pattern(&part.value),
                    part.modifier.text()
                ));
            }
            continue;
        }
        let custom = !numeric_name(&part.name);
        let mut grouped =
            !part.suffix.is_empty() || (!part.prefix.is_empty() && part.prefix != prefix_text);
        if !grouped
            && custom
            && part.kind == PartKind::SegmentWildcard
            && part.modifier == Modifier::None
            && let Some(next) = next
            && next.prefix.is_empty()
            && next.suffix.is_empty()
        {
            grouped = if next.kind == PartKind::FixedText {
                next.value
                    .chars()
                    .next()
                    .is_some_and(|first| is_name_char(first, false))
            } else {
                numeric_name(&next.name)
            };
        }
        if !grouped
            && part.prefix.is_empty()
            && let Some(previous) = previous
            && previous.kind == PartKind::FixedText
            && !prefix_text.is_empty()
            && previous.value.ends_with(prefix_text.as_str())
        {
            grouped = true;
        }
        if grouped {
            result.push('{');
        }
        result.push_str(&escape_pattern(&part.prefix));
        if custom {
            result.push(':');
            result.push_str(&part.name);
        }
        match part.kind {
            PartKind::Regexp => result.push_str(&format!("({})", part.value)),
            PartKind::SegmentWildcard if !custom => result.push_str(&format!("({wildcard})")),
            PartKind::FullWildcard => {
                let plain = !custom
                    && (previous.is_none_or(|previous| {
                        previous.kind == PartKind::FixedText || previous.modifier != Modifier::None
                    }) || grouped
                        || !part.prefix.is_empty());
                result.push_str(if plain { "*" } else { "(.*)" });
            }
            _ => {}
        }
        if part.kind == PartKind::SegmentWildcard
            && custom
            && part
                .suffix
                .chars()
                .next()
                .is_some_and(|first| is_name_char(first, false))
        {
            result.push('\\');
        }
        result.push_str(&escape_pattern(&part.suffix));
        if grouped {
            result.push('}');
        }
        result.push_str(part.modifier.text());
    }
    result
}

fn dummy() -> Url {
    Url::parse("https://dummy.invalid/", None).expect("the dummy URL parses")
}

// the canonical form of a piece of a component, through the URL parser as the standard does
fn canonicalize(kind: Canonical, value: &str, protocol: Option<&str>) -> Result<String, String> {
    if value.is_empty() {
        return Ok(String::new());
    }
    let invalid = || type_error(format!("Invalid URL pattern component: {value}"));
    Ok(match kind {
        Canonical::Protocol => {
            let parsed =
                Url::parse(&format!("{value}://dummy.invalid"), None).ok_or_else(invalid)?;
            parsed.scheme().to_string()
        }
        Canonical::Username | Canonical::Password => {
            let mut url = dummy();
            if matches!(kind, Canonical::Username) {
                url.set_username(value);
                url.username().to_string()
            } else {
                url.set_password(value);
                url.password().to_string()
            }
        }
        Canonical::Hostname => {
            if value.chars().any(|character| {
                matches!(
                    character,
                    ' ' | '#'
                        | '/'
                        | ':'
                        | '<'
                        | '>'
                        | '?'
                        | '@'
                        | '['
                        | '\\'
                        | ']'
                        | '^'
                        | '|'
                        | '%'
                ) || character.is_control()
            }) {
                return Err(invalid());
            }
            let mut url = dummy();
            url.set_hostname(value);
            url.hostname().to_string()
        }
        Canonical::Ipv6Hostname => {
            if !value.chars().all(|character| {
                character.is_ascii_hexdigit() || matches!(character, '[' | ']' | ':')
            }) {
                return Err(invalid());
            }
            value.to_ascii_lowercase()
        }
        Canonical::Port => {
            if !value.chars().all(|character| character.is_ascii_digit()) {
                return Err(invalid());
            }
            let port: u32 = value.parse().map_err(|_| invalid())?;
            if port > 65535 {
                return Err(invalid());
            }
            if protocol.and_then(crate::weburl::default_port) == Some(port as u16) {
                String::new()
            } else {
                port.to_string()
            }
        }
        Canonical::Pathname => {
            let leading = value.starts_with('/');
            let modified = if leading {
                value.to_string()
            } else {
                format!("/-{value}")
            };
            let mut url = dummy();
            url.set_pathname(&modified);
            let path = url.pathname();
            if leading {
                path
            } else {
                path.chars().skip(2).collect()
            }
        }
        Canonical::OpaquePathname => {
            let parsed = Url::parse(&format!("data:{value}"), None).ok_or_else(invalid)?;
            parsed.pathname()
        }
        Canonical::Search => {
            let mut url = dummy();
            url.set_search(value);
            url.search().trim_start_matches('?').to_string()
        }
        Canonical::Hash => {
            let mut url = dummy();
            url.set_hash(value);
            url.hash().trim_start_matches('#').to_string()
        }
    })
}

fn compile(
    pattern: &str,
    kind: Canonical,
    options: Options,
    protocol: Option<&str>,
) -> Result<Component, String> {
    let encode = |text: &str| canonicalize(kind, text, protocol);
    let parts = parse_pattern(pattern, options, &encode)?;
    let source = regexp_source(&parts, options);
    let flags = Flags {
        unicode: true,
        ignore_case: options.ignore_case,
        ..Flags::default()
    };
    let regex = Regex::new(&source, flags)
        .map_err(|error| type_error(format!("Invalid pattern: {error}")))?;
    let names = parts
        .iter()
        .filter(|part| part.kind != PartKind::FixedText)
        .map(|part| part.name.clone())
        .collect();
    let has_groups = parts.iter().any(|part| part.kind == PartKind::Regexp);
    Ok(Component {
        pattern: pattern_string(&parts, options),
        regex,
        names,
        has_groups,
    })
}

impl Component {
    fn matches(&self, input: &str) -> Option<Vec<(String, Option<String>)>> {
        let units: Vec<u16> = input.encode_utf16().collect();
        let found = self.regex.find(&units, 0, true).ok()??;
        Some(
            self.names
                .iter()
                .enumerate()
                .map(|(index, name)| {
                    let capture = found.captures.get(index + 1).copied().flatten();
                    (
                        name.clone(),
                        capture.map(|(start, end)| String::from_utf16_lossy(&units[start..end])),
                    )
                })
                .collect(),
        )
    }
}

// the constructor string parser's states
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    Init,
    Protocol,
    Authority,
    Username,
    Password,
    Hostname,
    Port,
    Pathname,
    Search,
    Hash,
    Done,
}

impl State {
    fn component(self) -> Option<usize> {
        Some(match self {
            State::Protocol => 0,
            State::Username => 1,
            State::Password => 2,
            State::Hostname => 3,
            State::Port => 4,
            State::Pathname => 5,
            State::Search => 6,
            State::Hash => 7,
            _ => return None,
        })
    }
}

type Init = [Option<String>; 8];

struct StringParser {
    input: Vec<char>,
    tokens: Vec<Token>,
    result: Init,
    component_start: usize,
    index: usize,
    increment: usize,
    group_depth: usize,
    bracket_depth: usize,
    special: bool,
    state: State,
}

impl StringParser {
    fn token(&self, index: usize) -> &Token {
        self.tokens
            .get(index)
            .unwrap_or_else(|| self.tokens.last().expect("an end token"))
    }

    fn plain(&self, index: usize, value: &str) -> bool {
        let token = self.token(index);
        token.value == value
            && matches!(
                token.kind,
                TokenKind::Char | TokenKind::EscapedChar | TokenKind::InvalidChar
            )
    }

    fn component_string(&self) -> String {
        let start = self.token(self.component_start).index;
        let end = self.token(self.index).index;
        self.input[start..end].iter().collect()
    }

    fn search_prefix(&self) -> bool {
        if self.plain(self.index, "?") {
            return true;
        }
        if self.token(self.index).value != "?" {
            return false;
        }
        let Some(previous) = self.index.checked_sub(1) else {
            return true;
        };
        !matches!(
            self.token(previous).kind,
            TokenKind::Name | TokenKind::Regexp | TokenKind::Close | TokenKind::Asterisk
        )
    }

    fn change(&mut self, state: State, skip: usize) {
        if let Some(component) = self.state.component() {
            self.result[component] = Some(self.component_string());
        }
        if self.state != State::Init && state != State::Done {
            let before = |states: &[State]| states.contains(&self.state);
            use State::*;
            if before(&[Protocol, Authority, Username, Password])
                && matches!(state, Port | Pathname | Search | Hash)
                && self.result[3].is_none()
            {
                self.result[3] = Some(String::new());
            }
            if before(&[Protocol, Authority, Username, Password, Hostname, Port])
                && matches!(state, Search | Hash)
                && self.result[5].is_none()
            {
                self.result[5] = Some(if self.special { "/" } else { "" }.to_string());
            }
            if before(&[
                Protocol, Authority, Username, Password, Hostname, Port, Pathname,
            ]) && state == Hash
                && self.result[6].is_none()
            {
                self.result[6] = Some(String::new());
            }
        }
        self.state = state;
        self.index += skip;
        self.component_start = self.index;
        self.increment = 0;
    }

    fn rewind(&mut self, state: State) {
        self.index = self.component_start;
        self.increment = 0;
        self.state = state;
    }
}

fn parse_constructor_string(input: &str) -> Result<Init, String> {
    let characters: Vec<char> = input.chars().collect();
    let tokens = tokenize(&characters, false)?;
    let mut parser = StringParser {
        input: characters,
        tokens,
        result: Default::default(),
        component_start: 0,
        index: 0,
        increment: 1,
        group_depth: 0,
        bracket_depth: 0,
        special: false,
        state: State::Init,
    };
    while parser.index < parser.tokens.len() {
        parser.increment = 1;
        if parser.tokens[parser.index].kind == TokenKind::End {
            match parser.state {
                State::Init => {
                    parser.rewind(State::Init);
                    if parser.plain(parser.index, "#") {
                        parser.change(State::Hash, 1);
                    } else if parser.search_prefix() {
                        parser.change(State::Search, 1);
                    } else {
                        parser.change(State::Pathname, 0);
                    }
                    parser.index += parser.increment;
                    continue;
                }
                State::Authority => {
                    parser.rewind(State::Hostname);
                    parser.index += parser.increment;
                    continue;
                }
                _ => {
                    parser.change(State::Done, 0);
                    break;
                }
            }
        }
        if parser.tokens[parser.index].kind == TokenKind::Open {
            parser.group_depth += 1;
            parser.index += parser.increment;
            continue;
        }
        if parser.group_depth > 0 {
            if parser.tokens[parser.index].kind == TokenKind::Close {
                parser.group_depth -= 1;
            } else {
                parser.index += parser.increment;
                continue;
            }
        }
        let at = parser.index;
        match parser.state {
            State::Init => {
                if parser.plain(at, ":") {
                    parser.rewind(State::Protocol);
                }
            }
            State::Protocol => {
                if parser.plain(at, ":") {
                    let protocol = parser.component_string();
                    let component = compile(&protocol, Canonical::Protocol, DEFAULT, None)?;
                    parser.special = SPECIAL_SCHEMES
                        .iter()
                        .any(|scheme| component.matches(scheme).is_some());
                    if parser.plain(at + 1, "/") && parser.plain(at + 2, "/") {
                        parser.change(State::Authority, 3);
                    } else if parser.special {
                        parser.change(State::Authority, 1);
                    } else {
                        parser.change(State::Pathname, 1);
                    }
                }
            }
            State::Authority => {
                if parser.plain(at, "@") {
                    parser.rewind(State::Username);
                } else if parser.plain(at, "/") || parser.search_prefix() || parser.plain(at, "#") {
                    parser.rewind(State::Hostname);
                }
            }
            State::Username => {
                if parser.plain(at, ":") {
                    parser.change(State::Password, 1);
                } else if parser.plain(at, "@") {
                    parser.change(State::Hostname, 1);
                }
            }
            State::Password => {
                if parser.plain(at, "@") {
                    parser.change(State::Hostname, 1);
                }
            }
            State::Hostname => {
                if parser.plain(at, "[") {
                    parser.bracket_depth += 1;
                } else if parser.plain(at, "]") {
                    parser.bracket_depth = parser.bracket_depth.saturating_sub(1);
                } else if parser.plain(at, ":") && parser.bracket_depth == 0 {
                    parser.change(State::Port, 1);
                } else if parser.plain(at, "/") {
                    parser.change(State::Pathname, 0);
                } else if parser.search_prefix() {
                    parser.change(State::Search, 1);
                } else if parser.plain(at, "#") {
                    parser.change(State::Hash, 1);
                }
            }
            State::Port => {
                if parser.plain(at, "/") {
                    parser.change(State::Pathname, 0);
                } else if parser.search_prefix() {
                    parser.change(State::Search, 1);
                } else if parser.plain(at, "#") {
                    parser.change(State::Hash, 1);
                }
            }
            State::Pathname => {
                if parser.search_prefix() {
                    parser.change(State::Search, 1);
                } else if parser.plain(at, "#") {
                    parser.change(State::Hash, 1);
                }
            }
            State::Search => {
                if parser.plain(at, "#") {
                    parser.change(State::Hash, 1);
                }
            }
            State::Hash | State::Done => {}
        }
        parser.index += parser.increment;
    }
    if parser.result[3].is_some() && parser.result[4].is_none() {
        parser.result[4] = Some(String::new());
    }
    Ok(parser.result)
}

fn absolute_pathname(input: &str, pattern: bool) -> bool {
    input.starts_with('/') || (pattern && (input.starts_with("\\/") || input.starts_with("{/")))
}

// process a URLPatternInit, as patterns (left as written) or as URLs (canonicalized)
fn process_init(init: &Init, base: Option<&str>, pattern: bool) -> Result<Init, String> {
    let mut result: Init = if pattern {
        Default::default()
    } else {
        std::array::from_fn(|_| Some(String::new()))
    };
    let based = |text: &str| {
        if pattern {
            escape_pattern(text)
        } else {
            text.to_string()
        }
    };
    let mut base_url = None;
    if let Some(base) = base {
        let parsed = Url::parse(base, None)
            .ok_or_else(|| type_error(format!("Invalid base URL: {base}")))?;
        let has = |index: usize| init[index].is_some();
        if !has(0) {
            result[0] = Some(based(parsed.scheme()));
        }
        if !pattern && !has(0) && !has(3) && !has(4) && !has(1) {
            result[1] = Some(based(parsed.username()));
        }
        if !pattern && !has(0) && !has(3) && !has(4) && !has(1) && !has(2) {
            result[2] = Some(based(parsed.password()));
        }
        if !has(0) && !has(3) {
            result[3] = Some(based(parsed.hostname()));
        }
        if !has(0) && !has(3) && !has(4) {
            result[4] = Some(parsed.port().map_or(String::new(), |port| port.to_string()));
        }
        if !has(0) && !has(3) && !has(4) && !has(5) {
            result[5] = Some(based(&parsed.pathname()));
        }
        if !has(0) && !has(3) && !has(4) && !has(5) && !has(6) {
            result[6] = Some(based(parsed.query().unwrap_or("")));
        }
        if !has(0) && !has(3) && !has(4) && !has(5) && !has(6) && !has(7) {
            result[7] = Some(based(parsed.hash().trim_start_matches('#')));
        }
        base_url = Some(parsed);
    }
    let canon = |kind, value: &str, protocol: Option<&str>| {
        if pattern {
            Ok(value.to_string())
        } else {
            canonicalize(kind, value, protocol)
        }
    };
    if let Some(protocol) = &init[0] {
        result[0] = Some(canon(
            Canonical::Protocol,
            protocol.strip_suffix(':').unwrap_or(protocol),
            None,
        )?);
    }
    if let Some(username) = &init[1] {
        result[1] = Some(canon(Canonical::Username, username, None)?);
    }
    if let Some(password) = &init[2] {
        result[2] = Some(canon(Canonical::Password, password, None)?);
    }
    if let Some(hostname) = &init[3] {
        let kind = if hostname.starts_with('[') {
            Canonical::Ipv6Hostname
        } else {
            Canonical::Hostname
        };
        result[3] = Some(canon(kind, hostname, None)?);
    }
    let protocol = result[0].clone();
    if let Some(port) = &init[4] {
        result[4] = Some(canon(Canonical::Port, port, protocol.as_deref())?);
    }
    if let Some(pathname) = &init[5] {
        let mut path = pathname.clone();
        if let Some(base) = &base_url
            && base.is_special()
            && !absolute_pathname(&path, pattern)
        {
            let base_path = based(&base.pathname());
            if let Some(slash) = base_path.rfind('/') {
                path = format!("{}{path}", &base_path[..=slash]);
            }
        }
        let special = protocol
            .as_deref()
            .is_none_or(|protocol| SPECIAL_SCHEMES.contains(&protocol) || protocol.is_empty());
        let kind = if special {
            Canonical::Pathname
        } else {
            Canonical::OpaquePathname
        };
        result[5] = Some(canon(kind, &path, None)?);
    }
    if let Some(search) = &init[6] {
        result[6] = Some(canon(
            Canonical::Search,
            search.strip_prefix('?').unwrap_or(search),
            None,
        )?);
    }
    if let Some(hash) = &init[7] {
        result[7] = Some(canon(
            Canonical::Hash,
            hash.strip_prefix('#').unwrap_or(hash),
            None,
        )?);
    }
    Ok(result)
}

impl ScriptRuntime {
    fn pattern_state(value: &Value) -> Option<Rc<UrlPattern>> {
        match value {
            Value::Object(object) => match &object.borrow().class {
                ObjectClass::UrlPattern(pattern) => Some(pattern.clone()),
                _ => None,
            },
            _ => None,
        }
    }

    // a URLPatternInit dictionary's members and its baseURL
    fn pattern_init(&mut self, value: &Value) -> Result<(Init, Option<String>), ScriptError> {
        let mut init: Init = Default::default();
        for (index, name) in COMPONENTS.iter().enumerate() {
            init[index] = self.option_text(value, name)?;
        }
        let base = self.option_text(value, "baseURL")?;
        Ok((init, base))
    }

    pub(super) fn construct_url_pattern(
        &mut self,
        arguments: Vec<Value>,
        new_target: Value,
    ) -> Result<Value, ScriptError> {
        if matches!(new_target, Value::Undefined) {
            return Err(self.type_error("Constructor URLPattern requires 'new'"));
        }
        let input = arguments.first().cloned().unwrap_or(Value::Undefined);
        let (base, options) = match arguments.get(1) {
            Some(Value::String(base)) => (Some(base.to_string()), arguments.get(2).cloned()),
            Some(other) if !is_primitive(other) => (None, Some(other.clone())),
            Some(Value::Undefined) | None => (None, arguments.get(2).cloned()),
            Some(other) => (
                Some(self.coerce_string(other.clone())?),
                arguments.get(2).cloned(),
            ),
        };
        let ignore_case = match &options {
            Some(options @ Value::Object(_)) => self.get_property(options, "ignoreCase")?.truthy(),
            _ => false,
        };
        let built = match &input {
            Value::String(text) => {
                let init = parse_constructor_string(text);
                init.and_then(|init| {
                    if base.is_none() && init[0].is_none() {
                        return Err(type_error("A relative pattern needs a base URL"));
                    }
                    process_init(&init, base.as_deref(), true)
                })
            }
            Value::Undefined => Ok(Default::default()),
            value if !is_primitive(value) => {
                if base.is_some() {
                    return Err(
                        self.type_error("A base URL goes inside the URLPatternInit, not beside it")
                    );
                }
                let (init, base) = self.pattern_init(value)?;
                process_init(&init, base.as_deref(), true)
            }
            other => {
                let text = self.coerce_string(other.clone())?;
                parse_constructor_string(&text)
                    .and_then(|init| process_init(&init, base.as_deref(), true))
            }
        };
        let processed = match built {
            Ok(processed) => processed,
            Err(message) => return Err(self.type_error(message)),
        };
        let mut processed: Vec<String> = processed
            .into_iter()
            .map(|part| part.unwrap_or_else(|| "*".to_string()))
            .collect();
        let protocol = processed[0].clone();
        if SPECIAL_SCHEMES.contains(&protocol.as_str())
            && crate::weburl::default_port(&protocol).map(|port| port.to_string())
                == Some(processed[4].clone())
        {
            processed[4] = String::new();
        }
        let compiled = (|| -> Result<Vec<Component>, String> {
            let protocol_component = compile(&processed[0], Canonical::Protocol, DEFAULT, None)?;
            let special = SPECIAL_SCHEMES
                .iter()
                .any(|scheme| protocol_component.matches(scheme).is_some());
            let hostname = &processed[3];
            let ipv6 = hostname.starts_with('[')
                || hostname.starts_with("{[")
                || hostname.starts_with("\\[");
            let with_case = Options {
                ignore_case,
                ..DEFAULT
            };
            let mut components = vec![
                protocol_component,
                compile(&processed[1], Canonical::Username, DEFAULT, None)?,
                compile(&processed[2], Canonical::Password, DEFAULT, None)?,
                compile(
                    hostname,
                    if ipv6 {
                        Canonical::Ipv6Hostname
                    } else {
                        Canonical::Hostname
                    },
                    Options {
                        delimiter: Some('.'),
                        ..DEFAULT
                    },
                    None,
                )?,
                compile(
                    &processed[4],
                    Canonical::Port,
                    DEFAULT,
                    Some(protocol.as_str()),
                )?,
            ];
            components.push(if special {
                compile(
                    &processed[5],
                    Canonical::Pathname,
                    Options {
                        delimiter: Some('/'),
                        prefix: Some('/'),
                        ignore_case,
                    },
                    None,
                )?
            } else {
                compile(&processed[5], Canonical::OpaquePathname, with_case, None)?
            });
            components.push(compile(&processed[6], Canonical::Search, with_case, None)?);
            components.push(compile(&processed[7], Canonical::Hash, with_case, None)?);
            Ok(components)
        })();
        let components = match compiled {
            Ok(components) => components,
            Err(message) => return Err(self.type_error(message)),
        };
        let prototype = match &new_target {
            Value::Object(_) => self.prototype_from_constructor(&new_target)?,
            _ => self.intrinsics.url_patterns.prototype.clone(),
        };
        Ok(Value::Object(new_object(JsObject::with_prototype(
            Some(prototype),
            ObjectClass::UrlPattern(Rc::new(UrlPattern { components })),
        ))))
    }

    // the eight components of what exec or test is given, or none when it is not a URL
    fn pattern_input(
        &mut self,
        input: &Value,
        base: &Value,
    ) -> Result<Option<[String; 8]>, ScriptError> {
        if !is_primitive(input) {
            if !matches!(base, Value::Undefined) {
                return Err(
                    self.type_error("A base URL goes inside the URLPatternInit, not beside it")
                );
            }
            let (init, base) = self.pattern_init(input)?;
            return Ok(process_init(&init, base.as_deref(), false)
                .ok()
                .map(|result| result.map(Option::unwrap_or_default)));
        }
        let text = match input {
            Value::Undefined => String::new(),
            value => self.coerce_string(value.clone())?,
        };
        let base = match base {
            Value::Undefined => None,
            value => {
                let text = self.coerce_string(value.clone())?;
                match Url::parse(&text, None) {
                    Some(url) => Some(url),
                    None => return Ok(None),
                }
            }
        };
        let Some(url) = Url::parse(&text, base.as_ref()) else {
            return Ok(None);
        };
        Ok(Some([
            url.scheme().to_string(),
            url.username().to_string(),
            url.password().to_string(),
            url.hostname().to_string(),
            url.port().map_or(String::new(), |port| port.to_string()),
            url.pathname(),
            url.query().unwrap_or("").to_string(),
            url.hash().trim_start_matches('#').to_string(),
        ]))
    }

    pub(super) fn call_url_pattern(
        &mut self,
        member: PatternMember,
        this: Value,
        arguments: Vec<Value>,
    ) -> Result<Value, ScriptError> {
        if member == PatternMember::Constructor {
            return Err(self.type_error("Constructor URLPattern requires 'new'"));
        }
        let Some(pattern) = Self::pattern_state(&this) else {
            return Err(self.type_error("Illegal invocation"));
        };
        match member {
            PatternMember::Component(index) => {
                return Ok(Value::String(
                    pattern.components[usize::from(index)]
                        .pattern
                        .clone()
                        .into(),
                ));
            }
            PatternMember::HasRegExpGroups => {
                return Ok(Value::Bool(
                    pattern
                        .components
                        .iter()
                        .any(|component| component.has_groups),
                ));
            }
            _ => {}
        }
        let input = arguments.first().cloned().unwrap_or(Value::Undefined);
        let base = arguments.get(1).cloned().unwrap_or(Value::Undefined);
        let Some(values) = self.pattern_input(&input, &base)? else {
            return Ok(if member == PatternMember::Test {
                Value::Bool(false)
            } else {
                Value::Null
            });
        };
        let mut matched = Vec::new();
        for (component, value) in pattern.components.iter().zip(values.iter()) {
            let Some(groups) = component.matches(value) else {
                return Ok(if member == PatternMember::Test {
                    Value::Bool(false)
                } else {
                    Value::Null
                });
            };
            matched.push(groups);
        }
        if member == PatternMember::Test {
            return Ok(Value::Bool(true));
        }
        let mut inputs = vec![input];
        if !matches!(base, Value::Undefined) {
            inputs.push(base);
        }
        let mut entries = vec![("inputs".to_string(), Self::new_array(inputs))];
        for ((name, value), groups) in COMPONENTS.iter().zip(values).zip(matched) {
            let groups = plain_object(
                groups
                    .into_iter()
                    .map(|(name, value)| {
                        (
                            name,
                            value.map_or(Value::Undefined, |value| Value::String(value.into())),
                        )
                    })
                    .collect::<Vec<_>>(),
            );
            entries.push((
                name.to_string(),
                plain_object([
                    ("input".to_string(), Value::String(value.into())),
                    ("groups".to_string(), groups),
                ]),
            ));
        }
        Ok(plain_object(entries))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patterns_compile_and_write_back_as_the_standard_does() {
        let path = Options {
            delimiter: Some('/'),
            prefix: Some('/'),
            ignore_case: false,
        };
        let component =
            compile("/books/:id(\\d+)/*?", Canonical::Pathname, path, None).expect("compiles");
        assert_eq!(component.pattern, "/books/:id(\\d+)/*?");
        assert!(component.matches("/books/12/").is_some());
        assert!(component.matches("/books/12").is_some());
        assert!(component.matches("/books/x").is_none());
        let groups = compile("/:a/:b?", Canonical::Pathname, path, None).expect("compiles");
        assert_eq!(
            groups.matches("/x").expect("matches"),
            vec![
                ("a".to_string(), Some("x".to_string())),
                ("b".to_string(), None)
            ]
        );
        let init =
            parse_constructor_string("https://*.example.com:8080/a/:b/c?q=1#top").expect("parses");
        assert_eq!(init[0].as_deref(), Some("https"));
        assert_eq!(init[3].as_deref(), Some("*.example.com"));
        assert_eq!(init[4].as_deref(), Some("8080"));
        assert_eq!(init[5].as_deref(), Some("/a/:b/c"));
        assert_eq!(init[6].as_deref(), Some("q=1"));
        assert_eq!(init[7].as_deref(), Some("top"));
        assert!(compile("/:a/:a", Canonical::Pathname, path, None).is_err());
        assert!(compile("/(foo", Canonical::Pathname, path, None).is_err());
    }

    #[test]
    fn url_patterns_test_and_exec_urls() {
        let document =
            crate::dom::Document::parse(r#"<p id="out"></p>"#).expect("document should parse");
        let document = Rc::new(RefCell::new(document));
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(
                r#"
                const seen = [];
                const books = new URLPattern({ pathname: "/books/:id" });
                seen.push(books.test("https://example.com/books/12"), books.test("https://example.com/films/12"));
                const found = books.exec("https://example.com/books/12?x");
                seen.push(found.pathname.groups.id, found.pathname.input, found.search.input, found.inputs[0]);
                const full = new URLPattern("https://:sub.example.com/*");
                const result = full.exec({ protocol: "https", hostname: "api.example.com", pathname: "/v1/x" });
                seen.push(result.hostname.groups.sub, result.pathname.groups[0], full.protocol, full.hostname, full.pathname, full.port);
                const relative = new URLPattern("/items/:item", "https://shop.test");
                seen.push(relative.test("/items/5", "https://shop.test"), relative.hostname, relative.hasRegExpGroups);
                seen.push(new URLPattern({ pathname: "/:n(\\d+)" }).hasRegExpGroups, new URLPattern({ pathname: "/A" }, { ignoreCase: true }).test("https://x.test/a"));
                seen.push(new URLPattern().pathname, books.exec("not a url"));
                for (make of [() => new URLPattern("/relative"), () => new URLPattern({ pathname: "/(x" }), () => URLPattern()]) {
                    try { make(); } catch (error) { seen.push(error.name); }
                }
                document.getElementById("out").textContent = seen.join("|");
                "#,
            )
            .expect("script should run");
        let out = document.borrow().find_element_by_id("out").unwrap();
        assert_eq!(
            document.borrow().element_text_content(out).as_deref(),
            Some(
                "true|false|12|/books/12|x|https://example.com/books/12?x|api|v1/x|https|:sub.example.com|/*||\
                 true|shop.test|false|true|true|*||TypeError|TypeError|TypeError"
            )
        );
    }
}
