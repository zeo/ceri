// GLSL ES tokens and the preprocessor before them: comments and line continuations go,
// directives choose lines and define object-like and function-like macros, and the lines
// kept become tokens with macros expanded

use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Tok {
    Ident(String),
    Int(u32, bool),
    Float(f32),
    Bool(bool),
    Punct(&'static str),
}

#[derive(Clone, Debug)]
pub(crate) struct Token {
    pub(crate) tok: Tok,
    pub(crate) line: u32,
}

const PUNCTUATION: [&str; 47] = [
    "<<=", ">>=", "++", "--", "<<", ">>", "<=", ">=", "==", "!=", "&&", "||", "^^", "*=", "/=",
    "%=", "+=", "-=", "&=", "^=", "|=", "(", ")", "[", "]", "{", "}", ".", ",", ";", ":", "?", "=",
    "+", "-", "*", "/", "%", "<", ">", "!", "~", "&", "^", "|", "#", "@",
];

fn punctuation(text: &str) -> Option<&'static str> {
    PUNCTUATION
        .iter()
        .copied()
        .find(|punct| text.starts_with(punct))
}

// the tokens of one line of text
fn tokens_of(text: &str, line: u32) -> Result<Vec<Token>, String> {
    let bytes = text.as_bytes();
    let mut tokens = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        let byte = bytes[at];
        if byte.is_ascii_whitespace() {
            at += 1;
            continue;
        }
        if byte.is_ascii_alphabetic() || byte == b'_' {
            let start = at;
            while at < bytes.len() && (bytes[at].is_ascii_alphanumeric() || bytes[at] == b'_') {
                at += 1;
            }
            let word = &text[start..at];
            let tok = match word {
                "true" => Tok::Bool(true),
                "false" => Tok::Bool(false),
                _ => Tok::Ident(word.to_string()),
            };
            tokens.push(Token { tok, line });
            continue;
        }
        if byte.is_ascii_digit()
            || (byte == b'.' && bytes.get(at + 1).is_some_and(u8::is_ascii_digit))
        {
            let start = at;
            let hex = byte == b'0' && matches!(bytes.get(at + 1), Some(b'x' | b'X'));
            if hex {
                at += 2;
                while at < bytes.len() && bytes[at].is_ascii_hexdigit() {
                    at += 1;
                }
            } else {
                while at < bytes.len() && bytes[at].is_ascii_digit() {
                    at += 1;
                }
            }
            let mut float = false;
            if !hex && at < bytes.len() && bytes[at] == b'.' {
                float = true;
                at += 1;
                while at < bytes.len() && bytes[at].is_ascii_digit() {
                    at += 1;
                }
            }
            if !hex && at < bytes.len() && matches!(bytes[at], b'e' | b'E') {
                float = true;
                at += 1;
                if at < bytes.len() && matches!(bytes[at], b'+' | b'-') {
                    at += 1;
                }
                if !bytes.get(at).is_some_and(u8::is_ascii_digit) {
                    return Err(format!("{line}: malformed number"));
                }
                while at < bytes.len() && bytes[at].is_ascii_digit() {
                    at += 1;
                }
            }
            let digits = &text[start..at];
            if !hex && at < bytes.len() && matches!(bytes[at], b'f' | b'F') {
                float = true;
                at += 1;
            }
            let unsigned = !float && at < bytes.len() && matches!(bytes[at], b'u' | b'U');
            if unsigned {
                at += 1;
            }
            let tok = if float {
                Tok::Float(
                    digits
                        .parse::<f32>()
                        .map_err(|_| format!("{line}: malformed number"))?,
                )
            } else {
                let value = if hex {
                    u64::from_str_radix(&digits[2..], 16)
                } else if digits.len() > 1 && digits.starts_with('0') {
                    u64::from_str_radix(&digits[1..], 8)
                } else {
                    digits.parse::<u64>()
                }
                .map_err(|_| format!("{line}: malformed number"))?;
                if value > u64::from(u32::MAX) {
                    return Err(format!("{line}: integer constant is too large"));
                }
                Tok::Int(value as u32, unsigned)
            };
            if at < bytes.len() && (bytes[at].is_ascii_alphanumeric() || bytes[at] == b'_') {
                return Err(format!("{line}: malformed number"));
            }
            tokens.push(Token { tok, line });
            continue;
        }
        match punctuation(&text[at..]) {
            Some(punct) => {
                at += punct.len();
                tokens.push(Token {
                    tok: Tok::Punct(punct),
                    line,
                });
            }
            None => {
                return Err(format!(
                    "{line}: unexpected character '{}'",
                    text[at..].chars().next().unwrap_or('?')
                ));
            }
        }
    }
    Ok(tokens)
}

// the source with comments made spaces, lines kept so line numbers hold
fn strip_comments(source: &str) -> Result<String, String> {
    let mut output = String::with_capacity(source.len());
    let mut chars = source.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '/' && chars.peek() == Some(&'/') {
            for next in chars.by_ref() {
                if next == '\n' {
                    output.push('\n');
                    break;
                }
            }
        } else if c == '/' && chars.peek() == Some(&'*') {
            chars.next();
            let mut closed = false;
            let mut last = ' ';
            for next in chars.by_ref() {
                if next == '\n' {
                    output.push('\n');
                }
                if last == '*' && next == '/' {
                    closed = true;
                    break;
                }
                last = next;
            }
            if !closed {
                return Err("unterminated comment".into());
            }
            output.push(' ');
        } else {
            output.push(c);
        }
    }
    Ok(output)
}

#[derive(Clone)]
struct Macro {
    parameters: Option<Vec<String>>,
    body: Vec<Token>,
}

pub(crate) struct Preprocessed {
    pub(crate) tokens: Vec<Token>,
    pub(crate) version: u32,
    pub(crate) extensions: Vec<String>,
}

// the tokens of a shader's source after preprocessing
pub(crate) fn preprocess(
    source: &str,
    extensions_allowed: &[&str],
) -> Result<Preprocessed, String> {
    let stripped = strip_comments(source)?;
    // a backslash before a line end joins the lines, counted as two for line numbers
    let joined = stripped.replace("\\\r\n", "\u{1}").replace("\\\n", "\u{1}");
    let mut macros: HashMap<String, Macro> = HashMap::new();
    let mut output = Vec::new();
    let mut version = 100;
    let mut extensions = Vec::new();
    // per nesting level: whether this branch is live, and whether a live branch was taken
    let mut stack: Vec<(bool, bool)> = Vec::new();
    let mut seen_code = false;
    for (index, raw_line) in joined.split('\n').enumerate() {
        let line_number = index as u32 + 1 + raw_line.matches('\u{1}').count() as u32;
        let line = raw_line.replace('\u{1}', " ");
        let trimmed = line.trim_start();
        let live = stack.iter().all(|(live, _)| *live);
        if let Some(directive) = trimmed.strip_prefix('#') {
            let directive = directive.trim_start();
            let (name, rest) = directive
                .split_once(|c: char| c.is_whitespace())
                .unwrap_or((directive, ""));
            let rest = rest.trim();
            match name {
                "if" | "ifdef" | "ifndef" => {
                    let value = live
                        && match name {
                            "ifdef" => macros.contains_key(rest),
                            "ifndef" => !macros.contains_key(rest),
                            _ => evaluate_condition(rest, &macros, line_number)? != 0,
                        };
                    stack.push((value, value));
                }
                "elif" => {
                    let parent_live =
                        stack.len() < 2 || stack[..stack.len() - 1].iter().all(|(live, _)| *live);
                    let Some(top) = stack.last_mut() else {
                        return Err(format!("{line_number}: #elif without #if"));
                    };
                    let value = parent_live
                        && !top.1
                        && evaluate_condition(rest, &macros, line_number)? != 0;
                    top.0 = value;
                    top.1 |= value;
                }
                "else" => {
                    let parent_live =
                        stack.len() < 2 || stack[..stack.len() - 1].iter().all(|(live, _)| *live);
                    let Some(top) = stack.last_mut() else {
                        return Err(format!("{line_number}: #else without #if"));
                    };
                    top.0 = parent_live && !top.1;
                    top.1 = true;
                }
                "endif" => {
                    if stack.pop().is_none() {
                        return Err(format!("{line_number}: #endif without #if"));
                    }
                }
                _ if !live => {}
                "" => {}
                "version" => {
                    if seen_code || !output.is_empty() {
                        return Err(format!("{line_number}: #version must come first"));
                    }
                    version = match rest {
                        "100" => 100,
                        "300 es" => 300,
                        _ => return Err(format!("{line_number}: unsupported #version {rest}")),
                    };
                }
                "define" => {
                    let name_end = rest
                        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                        .unwrap_or(rest.len());
                    let name = &rest[..name_end];
                    if name.is_empty() {
                        return Err(format!("{line_number}: #define needs a name"));
                    }
                    if name.starts_with("GL_") || name.contains("__") {
                        return Err(format!("{line_number}: macro name {name} is reserved"));
                    }
                    let after = &rest[name_end..];
                    let (parameters, body) = if let Some(list) = after.strip_prefix('(') {
                        let close = list.find(')').ok_or_else(|| {
                            format!("{line_number}: unterminated macro parameters")
                        })?;
                        let parameters = list[..close]
                            .split(',')
                            .map(|parameter| parameter.trim().to_string())
                            .filter(|parameter| !parameter.is_empty())
                            .collect();
                        (Some(parameters), &list[close + 1..])
                    } else {
                        (None, after)
                    };
                    let body = tokens_of(body, line_number)?;
                    macros.insert(name.to_string(), Macro { parameters, body });
                }
                "undef" => {
                    macros.remove(rest);
                }
                "error" => return Err(format!("{line_number}: #error {rest}")),
                "pragma" | "line" => {}
                "extension" => {
                    let (extension, behavior) = rest
                        .split_once(':')
                        .map(|(name, behavior)| (name.trim(), behavior.trim()))
                        .ok_or_else(|| format!("{line_number}: malformed #extension"))?;
                    let known = extension == "all" || extensions_allowed.contains(&extension);
                    if !known && matches!(behavior, "require") {
                        return Err(format!(
                            "{line_number}: extension {extension} is not supported"
                        ));
                    }
                    if known && behavior != "disable" {
                        extensions.push(extension.to_string());
                    }
                }
                other => return Err(format!("{line_number}: unknown directive #{other}")),
            }
            continue;
        }
        if !live {
            continue;
        }
        let tokens = tokens_of(&line, line_number)?;
        if !tokens.is_empty() {
            seen_code = true;
        }
        output.extend(tokens);
    }
    if !stack.is_empty() {
        return Err("unterminated #if".into());
    }
    let mut builtins = macros;
    builtins.insert(
        "GL_ES".into(),
        Macro {
            parameters: None,
            body: vec![Token {
                tok: Tok::Int(1, false),
                line: 0,
            }],
        },
    );
    builtins.insert(
        "GL_FRAGMENT_PRECISION_HIGH".into(),
        Macro {
            parameters: None,
            body: vec![Token {
                tok: Tok::Int(1, false),
                line: 0,
            }],
        },
    );
    builtins.insert(
        "__VERSION__".into(),
        Macro {
            parameters: None,
            body: vec![Token {
                tok: Tok::Int(version, false),
                line: 0,
            }],
        },
    );
    let tokens = expand(output, &builtins, &mut Vec::new())?;
    Ok(Preprocessed {
        tokens,
        version,
        extensions,
    })
}

// macros expanded in a token list; active holds the macros being expanded, which do not expand
// again inside themselves
fn expand(
    tokens: Vec<Token>,
    macros: &HashMap<String, Macro>,
    active: &mut Vec<String>,
) -> Result<Vec<Token>, String> {
    let mut output = Vec::with_capacity(tokens.len());
    let mut index = 0;
    while index < tokens.len() {
        let token = &tokens[index];
        let Tok::Ident(name) = &token.tok else {
            output.push(token.clone());
            index += 1;
            continue;
        };
        if name == "__LINE__" {
            output.push(Token {
                tok: Tok::Int(token.line, false),
                line: token.line,
            });
            index += 1;
            continue;
        }
        if name == "__FILE__" {
            output.push(Token {
                tok: Tok::Int(0, false),
                line: token.line,
            });
            index += 1;
            continue;
        }
        let Some(definition) = macros.get(name).filter(|_| !active.contains(name)) else {
            output.push(token.clone());
            index += 1;
            continue;
        };
        let line = token.line;
        let body = match &definition.parameters {
            None => {
                index += 1;
                definition.body.clone()
            }
            Some(parameters) => {
                if tokens.get(index + 1).map(|token| &token.tok) != Some(&Tok::Punct("(")) {
                    output.push(token.clone());
                    index += 1;
                    continue;
                }
                let mut arguments: Vec<Vec<Token>> = vec![Vec::new()];
                let mut depth = 0;
                let mut at = index + 2;
                loop {
                    let Some(next) = tokens.get(at) else {
                        return Err(format!("{line}: unterminated macro call"));
                    };
                    match next.tok {
                        Tok::Punct("(") => depth += 1,
                        Tok::Punct(")") if depth == 0 => break,
                        Tok::Punct(")") => depth -= 1,
                        Tok::Punct(",") if depth == 0 => {
                            arguments.push(Vec::new());
                            at += 1;
                            continue;
                        }
                        _ => {}
                    }
                    arguments
                        .last_mut()
                        .expect("at least one argument")
                        .push(next.clone());
                    at += 1;
                }
                if parameters.is_empty() && arguments.len() == 1 && arguments[0].is_empty() {
                    arguments.clear();
                }
                if arguments.len() != parameters.len() {
                    return Err(format!(
                        "{line}: macro {name} takes {} arguments",
                        parameters.len()
                    ));
                }
                let arguments: Vec<Vec<Token>> = arguments
                    .into_iter()
                    .map(|argument| expand(argument, macros, active))
                    .collect::<Result<_, _>>()?;
                index = at + 1;
                definition
                    .body
                    .iter()
                    .flat_map(|token| match &token.tok {
                        Tok::Ident(word) => {
                            match parameters.iter().position(|parameter| parameter == word) {
                                Some(position) => arguments[position].clone(),
                                None => vec![token.clone()],
                            }
                        }
                        _ => vec![token.clone()],
                    })
                    .collect()
            }
        };
        active.push(name.clone());
        let expanded = expand(
            body.into_iter()
                .map(|token| Token { line, ..token })
                .collect(),
            macros,
            active,
        )?;
        active.pop();
        output.extend(expanded);
    }
    Ok(output)
}

// a #if expression: integer arithmetic over literals, defined() and macros
fn evaluate_condition(
    text: &str,
    macros: &HashMap<String, Macro>,
    line: u32,
) -> Result<i64, String> {
    let raw = tokens_of(text, line)?;
    let mut resolved = Vec::new();
    let mut index = 0;
    while index < raw.len() {
        if raw[index].tok == Tok::Ident("defined".into()) {
            let (name, skip) = match (
                raw.get(index + 1).map(|token| &token.tok),
                raw.get(index + 2).map(|token| &token.tok),
            ) {
                (Some(Tok::Punct("(")), Some(Tok::Ident(name))) => (name.clone(), 4),
                (Some(Tok::Ident(name)), _) => (name.clone(), 2),
                _ => return Err(format!("{line}: malformed defined()")),
            };
            resolved.push(Token {
                tok: Tok::Int(
                    u32::from(macros.contains_key(&name) || name == "GL_ES"),
                    false,
                ),
                line,
            });
            index += skip;
            continue;
        }
        resolved.push(raw[index].clone());
        index += 1;
    }
    let mut with_builtins = macros.clone();
    with_builtins.insert(
        "GL_ES".into(),
        Macro {
            parameters: None,
            body: vec![Token {
                tok: Tok::Int(1, false),
                line: 0,
            }],
        },
    );
    let expanded = expand(resolved, &with_builtins, &mut Vec::new())?;
    let mut parser = Condition {
        tokens: &expanded,
        at: 0,
        line,
    };
    let value = parser.binary(0)?;
    if parser.at != expanded.len() {
        return Err(format!("{line}: malformed #if expression"));
    }
    Ok(value)
}

struct Condition<'a> {
    tokens: &'a [Token],
    at: usize,
    line: u32,
}

impl Condition<'_> {
    fn unary(&mut self) -> Result<i64, String> {
        let token = self
            .tokens
            .get(self.at)
            .ok_or_else(|| format!("{}: malformed #if expression", self.line))?;
        self.at += 1;
        Ok(match &token.tok {
            Tok::Int(value, _) => i64::from(*value),
            Tok::Bool(value) => i64::from(*value),
            Tok::Ident(_) => 0,
            Tok::Punct("(") => {
                let value = self.binary(0)?;
                if self.tokens.get(self.at).map(|token| &token.tok) != Some(&Tok::Punct(")")) {
                    return Err(format!("{}: missing ) in #if", self.line));
                }
                self.at += 1;
                value
            }
            Tok::Punct("-") => -self.unary()?,
            Tok::Punct("+") => self.unary()?,
            Tok::Punct("!") => i64::from(self.unary()? == 0),
            Tok::Punct("~") => !self.unary()?,
            _ => return Err(format!("{}: malformed #if expression", self.line)),
        })
    }

    fn binary(&mut self, minimum: u8) -> Result<i64, String> {
        let mut left = self.unary()?;
        loop {
            let Some(Tok::Punct(op)) = self.tokens.get(self.at).map(|token| &token.tok) else {
                return Ok(left);
            };
            let precedence = match *op {
                "||" => 1,
                "&&" => 2,
                "|" => 3,
                "^" => 4,
                "&" => 5,
                "==" | "!=" => 6,
                "<" | ">" | "<=" | ">=" => 7,
                "<<" | ">>" => 8,
                "+" | "-" => 9,
                "*" | "/" | "%" => 10,
                _ => return Ok(left),
            };
            if precedence < minimum {
                return Ok(left);
            }
            let op = *op;
            self.at += 1;
            let right = self.binary(precedence + 1)?;
            left = match op {
                "||" => i64::from(left != 0 || right != 0),
                "&&" => i64::from(left != 0 && right != 0),
                "|" => left | right,
                "^" => left ^ right,
                "&" => left & right,
                "==" => i64::from(left == right),
                "!=" => i64::from(left != right),
                "<" => i64::from(left < right),
                ">" => i64::from(left > right),
                "<=" => i64::from(left <= right),
                ">=" => i64::from(left >= right),
                "<<" => left.wrapping_shl(right as u32),
                ">>" => left.wrapping_shr(right as u32),
                "+" => left.wrapping_add(right),
                "-" => left.wrapping_sub(right),
                "*" => left.wrapping_mul(right),
                "/" if right == 0 => return Err(format!("{}: division by zero in #if", self.line)),
                "/" => left / right,
                "%" if right == 0 => return Err(format!("{}: division by zero in #if", self.line)),
                _ => left % right,
            };
        }
    }
}
