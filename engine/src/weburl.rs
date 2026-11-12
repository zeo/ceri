// the WHATWG URL standard's basic URL parser and serializer, which the URL class of pages
// stands on; hosts outside ASCII are lowercased and punycoded without the full UTS 46 mapping

use std::fmt;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Url {
    scheme: String,
    username: String,
    password: String,
    // the serialized host: a domain, an IPv4 address, a bracketed IPv6 address or an opaque host
    host: Option<String>,
    port: Option<u16>,
    path: Vec<String>,
    opaque_path: Option<String>,
    query: Option<String>,
    fragment: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    SchemeStart,
    Scheme,
    NoScheme,
    SpecialRelativeOrAuthority,
    PathOrAuthority,
    Relative,
    RelativeSlash,
    SpecialAuthoritySlashes,
    SpecialAuthorityIgnoreSlashes,
    Authority,
    Host,
    Hostname,
    Port,
    File,
    FileSlash,
    FileHost,
    PathStart,
    Path,
    OpaquePath,
    Query,
    Fragment,
}

#[derive(Clone, Copy)]
enum EncodeSet {
    C0,
    Fragment,
    Query,
    SpecialQuery,
    Path,
    Userinfo,
}

fn encodes(set: EncodeSet, byte: u8) -> bool {
    let c0 = !(0x20..=0x7e).contains(&byte);
    let query = c0 || matches!(byte, b' ' | b'"' | b'#' | b'<' | b'>');
    let path = query || matches!(byte, b'?' | b'^' | b'`' | b'{' | b'}');
    match set {
        EncodeSet::C0 => c0,
        EncodeSet::Fragment => c0 || matches!(byte, b' ' | b'"' | b'<' | b'>' | b'`'),
        EncodeSet::Query => query,
        EncodeSet::SpecialQuery => query || byte == b'\'',
        EncodeSet::Path => path,
        EncodeSet::Userinfo => {
            path || matches!(
                byte,
                b'/' | b':' | b';' | b'=' | b'@' | b'[' | b'\\' | b']' | b'^' | b'|'
            )
        }
    }
}

fn encode_into(out: &mut String, text: &str, set: EncodeSet) {
    for character in text.chars() {
        let mut buffer = [0; 4];
        for &byte in character.encode_utf8(&mut buffer).as_bytes() {
            if encodes(set, byte) {
                out.push_str(&format!("%{byte:02X}"));
            } else {
                out.push(char::from(byte));
            }
        }
    }
}

fn encoded(text: &str, set: EncodeSet) -> String {
    let mut out = String::new();
    encode_into(&mut out, text, set);
    out
}

pub(crate) fn percent_decode(text: &str) -> Vec<u8> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let hex = |offset: usize| {
            bytes
                .get(index + offset)
                .and_then(|byte| char::from(*byte).to_digit(16))
        };
        match (bytes[index], hex(1), hex(2)) {
            (b'%', Some(high), Some(low)) => {
                out.push((high * 16 + low) as u8);
                index += 3;
            }
            (byte, _, _) => {
                out.push(byte);
                index += 1;
            }
        }
    }
    out
}

pub(crate) fn default_port(scheme: &str) -> Option<u16> {
    match scheme {
        "ftp" => Some(21),
        "http" | "ws" => Some(80),
        "https" | "wss" => Some(443),
        _ => None,
    }
}

fn special(scheme: &str) -> bool {
    matches!(scheme, "ftp" | "file" | "http" | "https" | "ws" | "wss")
}

fn drive_letter(text: &[char]) -> bool {
    text.len() == 2 && text[0].is_ascii_alphabetic() && matches!(text[1], ':' | '|')
}

fn normalized_drive_letter(segment: &str) -> bool {
    let characters: Vec<char> = segment.chars().collect();
    drive_letter(&characters) && characters[1] == ':'
}

fn starts_with_drive_letter(text: &[char]) -> bool {
    text.len() >= 2
        && drive_letter(&text[..2])
        && text
            .get(2)
            .is_none_or(|next| matches!(next, '/' | '\\' | '?' | '#'))
}

fn single_dot(segment: &str) -> bool {
    segment == "." || segment.eq_ignore_ascii_case("%2e")
}

fn double_dot(segment: &str) -> bool {
    matches!(
        segment.to_ascii_lowercase().as_str(),
        ".." | ".%2e" | "%2e." | "%2e%2e"
    )
}

impl Url {
    pub(crate) fn parse(input: &str, base: Option<&Url>) -> Option<Url> {
        let mut url = Url::default();
        let trimmed = input.trim_matches(|character: char| character <= ' ');
        url.run(trimmed, base, None).then_some(url)
    }

    pub(crate) fn scheme(&self) -> &str {
        &self.scheme
    }

    pub(crate) fn is_special(&self) -> bool {
        special(&self.scheme)
    }

    pub(crate) fn username(&self) -> &str {
        &self.username
    }

    pub(crate) fn password(&self) -> &str {
        &self.password
    }

    pub(crate) fn host(&self) -> String {
        match (&self.host, self.port) {
            (None, _) => String::new(),
            (Some(host), None) => host.clone(),
            (Some(host), Some(port)) => format!("{host}:{port}"),
        }
    }

    pub(crate) fn hostname(&self) -> &str {
        self.host.as_deref().unwrap_or_default()
    }

    pub(crate) fn port(&self) -> Option<u16> {
        self.port
    }

    pub(crate) fn pathname(&self) -> String {
        match &self.opaque_path {
            Some(path) => path.clone(),
            None => self
                .path
                .iter()
                .map(|segment| format!("/{segment}"))
                .collect(),
        }
    }

    pub(crate) fn query(&self) -> Option<&str> {
        self.query.as_deref()
    }

    pub(crate) fn search(&self) -> String {
        match self.query.as_deref() {
            None | Some("") => String::new(),
            Some(query) => format!("?{query}"),
        }
    }

    pub(crate) fn hash(&self) -> String {
        match self.fragment.as_deref() {
            None | Some("") => String::new(),
            Some(fragment) => format!("#{fragment}"),
        }
    }

    pub(crate) fn origin(&self) -> String {
        match self.scheme.as_str() {
            "http" | "https" | "ws" | "wss" | "ftp" => {
                format!("{}://{}", self.scheme, self.host())
            }
            "blob" => match Url::parse(&self.pathname(), None) {
                Some(inner) if matches!(inner.scheme.as_str(), "http" | "https") => inner.origin(),
                _ => "null".to_string(),
            },
            _ => "null".to_string(),
        }
    }

    fn cannot_have_credentials(&self) -> bool {
        matches!(self.host.as_deref(), None | Some("")) || self.scheme == "file"
    }

    // the URL setters of the URL class; a value the setter cannot take leaves the URL as it was
    pub(crate) fn set_protocol(&mut self, value: &str) {
        self.update(&format!("{value}:"), State::SchemeStart);
    }

    pub(crate) fn set_username(&mut self, value: &str) {
        if !self.cannot_have_credentials() {
            self.username = encoded(value, EncodeSet::Userinfo);
        }
    }

    pub(crate) fn set_password(&mut self, value: &str) {
        if !self.cannot_have_credentials() {
            self.password = encoded(value, EncodeSet::Userinfo);
        }
    }

    pub(crate) fn set_host(&mut self, value: &str) {
        if self.opaque_path.is_none() {
            self.update(value, State::Host);
        }
    }

    pub(crate) fn set_hostname(&mut self, value: &str) {
        if self.opaque_path.is_none() {
            self.update(value, State::Hostname);
        }
    }

    pub(crate) fn set_port(&mut self, value: &str) {
        if self.cannot_have_credentials() {
            return;
        }
        if value.is_empty() {
            self.port = None;
        } else {
            self.update(value, State::Port);
        }
    }

    pub(crate) fn set_pathname(&mut self, value: &str) {
        if self.opaque_path.is_some() {
            return;
        }
        let mut url = self.clone();
        url.path.clear();
        if url.run(value, None, Some(State::PathStart)) {
            *self = url;
        }
    }

    pub(crate) fn set_search(&mut self, value: &str) {
        if value.is_empty() {
            self.query = None;
            return;
        }
        let mut url = self.clone();
        url.query = Some(String::new());
        if url.run(
            value.strip_prefix('?').unwrap_or(value),
            None,
            Some(State::Query),
        ) {
            *self = url;
        }
    }

    pub(crate) fn set_query(&mut self, query: Option<String>) {
        self.query = query;
    }

    pub(crate) fn set_hash(&mut self, value: &str) {
        if value.is_empty() {
            self.fragment = None;
            return;
        }
        let mut url = self.clone();
        url.fragment = Some(String::new());
        if url.run(
            value.strip_prefix('#').unwrap_or(value),
            None,
            Some(State::Fragment),
        ) {
            *self = url;
        }
    }

    fn update(&mut self, value: &str, state: State) {
        let mut url = self.clone();
        if url.run(value, None, Some(state)) {
            *self = url;
        }
    }

    fn shorten_path(&mut self) {
        if self.scheme == "file" && self.path.len() == 1 && normalized_drive_letter(&self.path[0]) {
            return;
        }
        self.path.pop();
    }

    // the basic URL parser; false is failure, and a state override that stops early is not one
    fn run(&mut self, input: &str, base: Option<&Url>, state_override: Option<State>) -> bool {
        let input: Vec<char> = input
            .chars()
            .filter(|character| !matches!(character, '\t' | '\n' | '\r'))
            .collect();
        let overridden = state_override.is_some();
        let mut state = state_override.unwrap_or(State::SchemeStart);
        let mut buffer = String::new();
        let mut at_sign_seen = false;
        let mut inside_brackets = false;
        let mut password_token_seen = false;
        let mut pointer: isize = 0;
        loop {
            let at = |offset: isize| -> Option<char> {
                usize::try_from(pointer + offset)
                    .ok()
                    .and_then(|index| input.get(index).copied())
            };
            let c = at(0);
            let rest = || -> &[char] {
                let start = usize::try_from(pointer + 1).unwrap_or(0).min(input.len());
                &input[start..]
            };
            let here = || -> &[char] {
                let start = usize::try_from(pointer).unwrap_or(0).min(input.len());
                &input[start..]
            };
            match state {
                State::SchemeStart => match c {
                    Some(character) if character.is_ascii_alphabetic() => {
                        buffer.push(character.to_ascii_lowercase());
                        state = State::Scheme;
                    }
                    _ if !overridden => {
                        state = State::NoScheme;
                        pointer -= 1;
                    }
                    _ => return false,
                },
                State::Scheme => match c {
                    Some(character)
                        if character.is_ascii_alphanumeric()
                            || matches!(character, '+' | '-' | '.') =>
                    {
                        buffer.push(character.to_ascii_lowercase());
                    }
                    Some(':') => {
                        if overridden {
                            if special(&self.scheme) != special(&buffer) {
                                return true;
                            }
                            if (!self.username.is_empty()
                                || !self.password.is_empty()
                                || self.port.is_some())
                                && buffer == "file"
                            {
                                return true;
                            }
                            if self.scheme == "file" && self.host.as_deref() == Some("") {
                                return true;
                            }
                        }
                        self.scheme = std::mem::take(&mut buffer);
                        if overridden {
                            if self.port.is_some() && self.port == default_port(&self.scheme) {
                                self.port = None;
                            }
                            return true;
                        }
                        if self.scheme == "file" {
                            state = State::File;
                        } else if self.is_special()
                            && base.is_some_and(|base| base.scheme == self.scheme)
                        {
                            state = State::SpecialRelativeOrAuthority;
                        } else if self.is_special() {
                            state = State::SpecialAuthoritySlashes;
                        } else if rest().first() == Some(&'/') {
                            state = State::PathOrAuthority;
                            pointer += 1;
                        } else {
                            self.opaque_path = Some(String::new());
                            state = State::OpaquePath;
                        }
                    }
                    _ if !overridden => {
                        buffer.clear();
                        state = State::NoScheme;
                        pointer = -1;
                    }
                    _ => return false,
                },
                State::NoScheme => {
                    let Some(base) = base else { return false };
                    match (&base.opaque_path, c) {
                        (Some(_), Some('#')) => {
                            self.scheme = base.scheme.clone();
                            self.opaque_path = base.opaque_path.clone();
                            self.query = base.query.clone();
                            self.fragment = Some(String::new());
                            state = State::Fragment;
                        }
                        (Some(_), _) => return false,
                        (None, _) if base.scheme != "file" => {
                            state = State::Relative;
                            pointer -= 1;
                        }
                        (None, _) => {
                            state = State::File;
                            pointer -= 1;
                        }
                    }
                }
                State::SpecialRelativeOrAuthority => {
                    if c == Some('/') && rest().first() == Some(&'/') {
                        state = State::SpecialAuthorityIgnoreSlashes;
                        pointer += 1;
                    } else {
                        state = State::Relative;
                        pointer -= 1;
                    }
                }
                State::PathOrAuthority => {
                    if c == Some('/') {
                        state = State::Authority;
                    } else {
                        state = State::Path;
                        pointer -= 1;
                    }
                }
                State::Relative => {
                    let base = base.expect("a relative URL has a base");
                    self.scheme = base.scheme.clone();
                    if c == Some('/') || (self.is_special() && c == Some('\\')) {
                        state = State::RelativeSlash;
                    } else {
                        self.username = base.username.clone();
                        self.password = base.password.clone();
                        self.host = base.host.clone();
                        self.port = base.port;
                        self.path = base.path.clone();
                        self.query = base.query.clone();
                        match c {
                            Some('?') => {
                                self.query = Some(String::new());
                                state = State::Query;
                            }
                            Some('#') => {
                                self.fragment = Some(String::new());
                                state = State::Fragment;
                            }
                            Some(_) => {
                                self.query = None;
                                self.shorten_path();
                                state = State::Path;
                                pointer -= 1;
                            }
                            None => {}
                        }
                    }
                }
                State::RelativeSlash => {
                    if self.is_special() && matches!(c, Some('/' | '\\')) {
                        state = State::SpecialAuthorityIgnoreSlashes;
                    } else if c == Some('/') {
                        state = State::Authority;
                    } else {
                        let base = base.expect("a relative URL has a base");
                        self.username = base.username.clone();
                        self.password = base.password.clone();
                        self.host = base.host.clone();
                        self.port = base.port;
                        state = State::Path;
                        pointer -= 1;
                    }
                }
                State::SpecialAuthoritySlashes => {
                    state = State::SpecialAuthorityIgnoreSlashes;
                    if c == Some('/') && rest().first() == Some(&'/') {
                        pointer += 1;
                    } else {
                        pointer -= 1;
                    }
                }
                State::SpecialAuthorityIgnoreSlashes => {
                    if !matches!(c, Some('/' | '\\')) {
                        state = State::Authority;
                        pointer -= 1;
                    }
                }
                State::Authority => match c {
                    Some('@') => {
                        if at_sign_seen {
                            buffer.insert_str(0, "%40");
                        }
                        at_sign_seen = true;
                        for character in std::mem::take(&mut buffer).chars() {
                            if character == ':' && !password_token_seen {
                                password_token_seen = true;
                                continue;
                            }
                            let target = if password_token_seen {
                                &mut self.password
                            } else {
                                &mut self.username
                            };
                            encode_into(target, &character.to_string(), EncodeSet::Userinfo);
                        }
                    }
                    None | Some('/' | '?' | '#') => {
                        if at_sign_seen && buffer.is_empty() {
                            return false;
                        }
                        pointer -= buffer.chars().count() as isize + 1;
                        buffer.clear();
                        state = State::Host;
                    }
                    Some('\\') if self.is_special() => {
                        if at_sign_seen && buffer.is_empty() {
                            return false;
                        }
                        pointer -= buffer.chars().count() as isize + 1;
                        buffer.clear();
                        state = State::Host;
                    }
                    Some(character) => buffer.push(character),
                },
                State::Host | State::Hostname => {
                    let ends = matches!(c, None | Some('/' | '?' | '#'))
                        || (self.is_special() && c == Some('\\'));
                    if overridden && self.scheme == "file" {
                        pointer -= 1;
                        state = State::FileHost;
                    } else if c == Some(':') && !inside_brackets {
                        if buffer.is_empty() {
                            return false;
                        }
                        if state_override == Some(State::Hostname) {
                            return true;
                        }
                        let Some(host) = parse_host(&buffer, !self.is_special()) else {
                            return false;
                        };
                        self.host = Some(host);
                        buffer.clear();
                        state = State::Port;
                    } else if ends {
                        pointer -= 1;
                        if self.is_special() && buffer.is_empty() {
                            return false;
                        }
                        if overridden
                            && buffer.is_empty()
                            && (!self.username.is_empty()
                                || !self.password.is_empty()
                                || self.port.is_some())
                        {
                            return true;
                        }
                        let Some(host) = parse_host(&buffer, !self.is_special()) else {
                            return false;
                        };
                        self.host = Some(host);
                        buffer.clear();
                        state = State::PathStart;
                        if overridden {
                            return true;
                        }
                    } else if let Some(character) = c {
                        match character {
                            '[' => inside_brackets = true,
                            ']' => inside_brackets = false,
                            _ => {}
                        }
                        buffer.push(character);
                    }
                }
                State::Port => match c {
                    Some(digit) if digit.is_ascii_digit() => buffer.push(digit),
                    _ if overridden
                        || matches!(c, None | Some('/' | '?' | '#'))
                        || (self.is_special() && c == Some('\\')) =>
                    {
                        if !buffer.is_empty() {
                            let Ok(port) =
                                buffer
                                    .trim_start_matches('0')
                                    .parse::<u32>()
                                    .or_else(|error| {
                                        if buffer.chars().all(|digit| digit == '0') {
                                            Ok(0)
                                        } else {
                                            Err(error)
                                        }
                                    })
                            else {
                                return false;
                            };
                            let Ok(port) = u16::try_from(port) else {
                                return false;
                            };
                            self.port = (default_port(&self.scheme) != Some(port)).then_some(port);
                            buffer.clear();
                        }
                        if overridden {
                            return true;
                        }
                        state = State::PathStart;
                        pointer -= 1;
                    }
                    _ => return false,
                },
                State::File => {
                    self.scheme = "file".to_string();
                    self.host = Some(String::new());
                    if matches!(c, Some('/' | '\\')) {
                        state = State::FileSlash;
                    } else if let Some(base) = base.filter(|base| base.scheme == "file") {
                        self.host = base.host.clone();
                        self.path = base.path.clone();
                        self.query = base.query.clone();
                        match c {
                            Some('?') => {
                                self.query = Some(String::new());
                                state = State::Query;
                            }
                            Some('#') => {
                                self.fragment = Some(String::new());
                                state = State::Fragment;
                            }
                            Some(_) => {
                                self.query = None;
                                if starts_with_drive_letter(here()) {
                                    self.path.clear();
                                } else {
                                    self.shorten_path();
                                }
                                state = State::Path;
                                pointer -= 1;
                            }
                            None => {}
                        }
                    } else {
                        state = State::Path;
                        pointer -= 1;
                    }
                }
                State::FileSlash => {
                    if matches!(c, Some('/' | '\\')) {
                        state = State::FileHost;
                    } else {
                        if let Some(base) = base.filter(|base| base.scheme == "file") {
                            self.host = base.host.clone();
                            if !starts_with_drive_letter(here())
                                && base
                                    .path
                                    .first()
                                    .is_some_and(|segment| normalized_drive_letter(segment))
                            {
                                self.path.push(base.path[0].clone());
                            }
                        }
                        state = State::Path;
                        pointer -= 1;
                    }
                }
                State::FileHost => match c {
                    None | Some('/' | '\\' | '?' | '#') => {
                        pointer -= 1;
                        let characters: Vec<char> = buffer.chars().collect();
                        if !overridden && drive_letter(&characters) {
                            state = State::Path;
                        } else if buffer.is_empty() {
                            self.host = Some(String::new());
                            if overridden {
                                return true;
                            }
                            state = State::PathStart;
                        } else {
                            let Some(mut host) = parse_host(&buffer, !self.is_special()) else {
                                return false;
                            };
                            if host == "localhost" {
                                host.clear();
                            }
                            self.host = Some(host);
                            if overridden {
                                return true;
                            }
                            buffer.clear();
                            state = State::PathStart;
                        }
                    }
                    Some(character) => buffer.push(character),
                },
                State::PathStart => {
                    if self.is_special() {
                        state = State::Path;
                        if !matches!(c, Some('/' | '\\')) {
                            pointer -= 1;
                        }
                    } else if !overridden && c == Some('?') {
                        self.query = Some(String::new());
                        state = State::Query;
                    } else if !overridden && c == Some('#') {
                        self.fragment = Some(String::new());
                        state = State::Fragment;
                    } else if c.is_some() {
                        state = State::Path;
                        if c != Some('/') {
                            pointer -= 1;
                        }
                    } else if overridden && self.host.is_none() {
                        self.path.push(String::new());
                    }
                }
                State::Path => {
                    let slash = c == Some('/') || (self.is_special() && c == Some('\\'));
                    if c.is_none() || slash || (!overridden && matches!(c, Some('?' | '#'))) {
                        if double_dot(&buffer) {
                            self.shorten_path();
                            if !slash {
                                self.path.push(String::new());
                            }
                        } else if single_dot(&buffer) && !slash {
                            self.path.push(String::new());
                        } else if !single_dot(&buffer) {
                            if self.scheme == "file"
                                && self.path.is_empty()
                                && drive_letter(&buffer.chars().collect::<Vec<_>>())
                            {
                                buffer = format!("{}:", &buffer[..1]);
                            }
                            self.path.push(std::mem::take(&mut buffer));
                        }
                        buffer.clear();
                        match c {
                            Some('?') => {
                                self.query = Some(String::new());
                                state = State::Query;
                            }
                            Some('#') => {
                                self.fragment = Some(String::new());
                                state = State::Fragment;
                            }
                            _ => {}
                        }
                    } else if let Some(character) = c {
                        encode_into(&mut buffer, &character.to_string(), EncodeSet::Path);
                    }
                }
                State::OpaquePath => {
                    let path = self.opaque_path.get_or_insert_with(String::new);
                    match c {
                        Some('?') => {
                            self.query = Some(String::new());
                            state = State::Query;
                        }
                        Some('#') => {
                            self.fragment = Some(String::new());
                            state = State::Fragment;
                        }
                        Some(' ') if matches!(rest().first(), Some('?' | '#')) => {
                            path.push_str("%20")
                        }
                        Some(character) => encode_into(path, &character.to_string(), EncodeSet::C0),
                        None => {}
                    }
                }
                State::Query => {
                    if c.is_none() || (!overridden && c == Some('#')) {
                        let set = if self.is_special() {
                            EncodeSet::SpecialQuery
                        } else {
                            EncodeSet::Query
                        };
                        let query = self.query.get_or_insert_with(String::new);
                        encode_into(query, &std::mem::take(&mut buffer), set);
                        if c == Some('#') {
                            self.fragment = Some(String::new());
                            state = State::Fragment;
                        }
                    } else if let Some(character) = c {
                        buffer.push(character);
                    }
                }
                State::Fragment => {
                    if let Some(character) = c {
                        let fragment = self.fragment.get_or_insert_with(String::new);
                        encode_into(fragment, &character.to_string(), EncodeSet::Fragment);
                    }
                }
            }
            if c.is_none() && usize::try_from(pointer).is_ok_and(|index| index >= input.len()) {
                return true;
            }
            pointer += 1;
        }
    }
}

impl fmt::Display for Url {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}:", self.scheme)?;
        if let Some(host) = &self.host {
            formatter.write_str("//")?;
            if !self.username.is_empty() || !self.password.is_empty() {
                formatter.write_str(&self.username)?;
                if !self.password.is_empty() {
                    write!(formatter, ":{}", self.password)?;
                }
                formatter.write_str("@")?;
            }
            formatter.write_str(host)?;
            if let Some(port) = self.port {
                write!(formatter, ":{port}")?;
            }
        } else if self.opaque_path.is_none() && self.path.len() > 1 && self.path[0].is_empty() {
            formatter.write_str("/.")?;
        }
        formatter.write_str(&self.pathname())?;
        if let Some(query) = &self.query {
            write!(formatter, "?{query}")?;
        }
        if let Some(fragment) = &self.fragment {
            write!(formatter, "#{fragment}")?;
        }
        Ok(())
    }
}

fn forbidden_host(character: char) -> bool {
    matches!(
        character,
        '\0' | '\t'
            | '\n'
            | '\r'
            | ' '
            | '#'
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
    )
}

fn parse_host(input: &str, opaque: bool) -> Option<String> {
    if let Some(inner) = input.strip_prefix('[') {
        let address = parse_ipv6(&inner.strip_suffix(']')?.chars().collect::<Vec<_>>())?;
        return Some(format!("[{}]", serialize_ipv6(&address)));
    }
    if opaque {
        if input
            .chars()
            .any(|character| character != '%' && forbidden_host(character))
        {
            return None;
        }
        return Some(encoded(input, EncodeSet::C0));
    }
    let domain = String::from_utf8_lossy(&percent_decode(input)).into_owned();
    let ascii = domain_to_ascii(&domain)?;
    if ascii.chars().any(|character| {
        forbidden_host(character) || character.is_ascii_control() || character == '%'
    }) {
        return None;
    }
    if ends_in_number(&ascii) {
        return parse_ipv4(&ascii).map(|address| {
            let [a, b, c, d] = address.to_be_bytes();
            format!("{a}.{b}.{c}.{d}")
        });
    }
    Some(ascii)
}

fn domain_to_ascii(domain: &str) -> Option<String> {
    let lowered: String = domain
        .chars()
        .map(|character| match character {
            '\u{3002}' | '\u{ff0e}' | '\u{ff61}' => '.',
            character => character,
        })
        .flat_map(char::to_lowercase)
        .collect();
    let labels: Vec<String> = lowered
        .split('.')
        .map(|label| {
            if label.is_ascii() {
                label.to_string()
            } else {
                format!("xn--{}", punycode(label))
            }
        })
        .collect();
    let ascii = labels.join(".");
    (!ascii.is_empty()).then_some(ascii)
}

// rfc 3492 punycode for one label
fn punycode(label: &str) -> String {
    const BASE: u32 = 36;
    const T_MIN: u32 = 1;
    const T_MAX: u32 = 26;
    let code_points: Vec<u32> = label.chars().map(u32::from).collect();
    let mut output: String = label.chars().filter(char::is_ascii).collect();
    let basic = output.len() as u32;
    let mut handled = basic;
    if basic > 0 {
        output.push('-');
    }
    let digit = |value: u32| {
        char::from(if value < 26 {
            b'a' + value as u8
        } else {
            b'0' + (value - 26) as u8
        })
    };
    let adapt = |mut delta: u32, points: u32, first: bool| {
        delta /= if first { 700 } else { 2 };
        delta += delta / points;
        let mut k = 0;
        while delta > ((BASE - T_MIN) * T_MAX) / 2 {
            delta /= BASE - T_MIN;
            k += BASE;
        }
        k + (BASE - T_MIN + 1) * delta / (delta + 38)
    };
    let (mut n, mut delta, mut bias) = (128u32, 0u32, 72u32);
    while (handled as usize) < code_points.len() {
        let next = code_points
            .iter()
            .copied()
            .filter(|&point| point >= n)
            .min()
            .unwrap_or(n);
        delta = delta.saturating_add((next - n).saturating_mul(handled + 1));
        n = next;
        for &point in &code_points {
            if point < n {
                delta = delta.saturating_add(1);
            }
            if point == n {
                let mut q = delta;
                let mut k = BASE;
                loop {
                    let t = if k <= bias {
                        T_MIN
                    } else if k >= bias + T_MAX {
                        T_MAX
                    } else {
                        k - bias
                    };
                    if q < t {
                        break;
                    }
                    output.push(digit(t + (q - t) % (BASE - t)));
                    q = (q - t) / (BASE - t);
                    k += BASE;
                }
                output.push(digit(q));
                bias = adapt(delta, handled + 1, handled == basic);
                delta = 0;
                handled += 1;
            }
        }
        delta += 1;
        n += 1;
    }
    output
}

fn ends_in_number(domain: &str) -> bool {
    let mut parts: Vec<&str> = domain.split('.').collect();
    if parts.last() == Some(&"") {
        if parts.len() == 1 {
            return false;
        }
        parts.pop();
    }
    let last = parts.last().copied().unwrap_or_default();
    (!last.is_empty() && last.bytes().all(|byte| byte.is_ascii_digit()))
        || parse_ipv4_number(last).is_some()
}

fn parse_ipv4_number(mut input: &str) -> Option<u64> {
    if input.is_empty() {
        return None;
    }
    let mut radix = 10;
    if let Some(hex) = input
        .strip_prefix("0x")
        .or_else(|| input.strip_prefix("0X"))
    {
        input = hex;
        radix = 16;
    } else if input.len() > 1 && input.starts_with('0') {
        input = &input[1..];
        radix = 8;
    }
    if input.is_empty() {
        return Some(0);
    }
    if !input.chars().all(|character| character.is_digit(radix)) {
        return None;
    }
    // a number past what an address holds fails the same way an overflowing one would
    Some(u64::from_str_radix(input, radix).unwrap_or(u64::MAX))
}

fn parse_ipv4(input: &str) -> Option<u32> {
    let mut parts: Vec<&str> = input.split('.').collect();
    if parts.last() == Some(&"") && parts.len() > 1 {
        parts.pop();
    }
    if parts.len() > 4 {
        return None;
    }
    let numbers = parts
        .iter()
        .map(|part| parse_ipv4_number(part))
        .collect::<Option<Vec<u64>>>()?;
    let (last, leading) = numbers.split_last()?;
    if leading.iter().any(|&number| number > 255) || *last >= 256u64.pow(5 - numbers.len() as u32) {
        return None;
    }
    let mut address = *last;
    for (index, &number) in leading.iter().enumerate() {
        address += number * 256u64.pow(3 - index as u32);
    }
    u32::try_from(address).ok()
}

fn parse_ipv6(input: &[char]) -> Option<[u16; 8]> {
    let mut address = [0u16; 8];
    let mut piece = 0usize;
    let mut compress = None;
    let mut pointer = 0usize;
    let at = |index: usize| input.get(index).copied();
    if at(0) == Some(':') {
        if at(1) != Some(':') {
            return None;
        }
        pointer = 2;
        piece = 1;
        compress = Some(1);
    }
    while at(pointer).is_some() {
        if piece == 8 {
            return None;
        }
        if at(pointer) == Some(':') {
            if compress.is_some() {
                return None;
            }
            pointer += 1;
            piece += 1;
            compress = Some(piece);
            continue;
        }
        let mut value = 0u32;
        let mut length = 0;
        while length < 4
            && let Some(digit) = at(pointer).and_then(|character| character.to_digit(16))
        {
            value = value * 16 + digit;
            pointer += 1;
            length += 1;
        }
        if at(pointer) == Some('.') {
            if length == 0 || piece > 6 {
                return None;
            }
            pointer -= length;
            let mut numbers_seen = 0;
            while at(pointer).is_some() {
                if numbers_seen > 0 {
                    if at(pointer) == Some('.') && numbers_seen < 4 {
                        pointer += 1;
                    } else {
                        return None;
                    }
                }
                at(pointer).filter(char::is_ascii_digit)?;
                let mut number: Option<u32> = None;
                while let Some(digit) = at(pointer).and_then(|character| character.to_digit(10)) {
                    number = match number {
                        None => Some(digit),
                        Some(0) => return None,
                        Some(number) => Some(number * 10 + digit),
                    };
                    if number > Some(255) {
                        return None;
                    }
                    pointer += 1;
                }
                address[piece] = address[piece] * 0x100 + number? as u16;
                numbers_seen += 1;
                if numbers_seen == 2 || numbers_seen == 4 {
                    piece += 1;
                }
            }
            if numbers_seen != 4 {
                return None;
            }
            break;
        } else if at(pointer) == Some(':') {
            pointer += 1;
            at(pointer)?;
        } else if at(pointer).is_some() {
            return None;
        }
        address[piece] = value as u16;
        piece += 1;
    }
    match compress {
        Some(compress) => {
            let mut swaps = piece - compress;
            piece = 7;
            while piece != 0 && swaps > 0 {
                address.swap(piece, compress + swaps - 1);
                piece -= 1;
                swaps -= 1;
            }
        }
        None if piece != 8 => return None,
        None => {}
    }
    Some(address)
}

fn serialize_ipv6(address: &[u16; 8]) -> String {
    let mut best: Option<(usize, usize)> = None;
    let mut index = 0;
    while index < 8 {
        if address[index] == 0 {
            let start = index;
            while index < 8 && address[index] == 0 {
                index += 1;
            }
            let length = index - start;
            if length > 1 && best.is_none_or(|(_, best)| length > best) {
                best = Some((start, length));
            }
        } else {
            index += 1;
        }
    }
    let mut out = String::new();
    let mut index = 0;
    while index < 8 {
        if let Some((start, length)) = best
            && index == start
        {
            out.push_str(if index == 0 { "::" } else { ":" });
            index += length;
            continue;
        }
        out.push_str(&format!("{:x}", address[index]));
        if index < 7 {
            out.push(':');
        }
        index += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::Url;

    fn href(input: &str, base: Option<&str>) -> Option<String> {
        let base = base.map(|base| Url::parse(base, None).expect("base"));
        Url::parse(input, base.as_ref()).map(|url| url.to_string())
    }

    #[test]
    fn parses_and_resolves_the_shapes_pages_hand_it() {
        for (input, base, expected) in [
            (
                "HTTP://User:Pa ss@EXAMPLE.com:80/a/./b/../c?q=1 2#f g",
                None,
                "http://User:Pa%20ss@example.com/a/c?q=1%202#f%20g",
            ),
            (
                "ws://localhost:1420/?token=x",
                None,
                "ws://localhost:1420/?token=x",
            ),
            (
                "wss://app.ceri.invalid:/?token=x",
                None,
                "wss://app.ceri.invalid/?token=x",
            ),
            (
                "../x?y",
                Some("http://vite.dev/a/b/c"),
                "http://vite.dev/a/x?y",
            ),
            ("//other:8080", Some("https://a.b/c"), "https://other:8080/"),
            ("?v=2", Some("https://a.b/c?v=1#h"), "https://a.b/c?v=2"),
            ("#top", Some("https://a.b/c?v=1"), "https://a.b/c?v=1#top"),
            ("\\\\x\\y", Some("http://a/"), "http://x/y"),
            ("blob:https://a.b/uuid", None, "blob:https://a.b/uuid"),
            ("data:text/plain,hi there", None, "data:text/plain,hi there"),
            ("mailto:me@x.org", None, "mailto:me@x.org"),
            ("file:///C:/a/../../b", None, "file:///C:/b"),
            ("file://localhost/etc", None, "file:///etc"),
            ("http://0x7f.1/", None, "http://127.0.0.1/"),
            ("http://[0:0:0:0:0:0:0:1]:8080/", None, "http://[::1]:8080/"),
            (
                "http://[::ffff:192.168.0.1]/",
                None,
                "http://[::ffff:c0a8:1]/",
            ),
            ("http://b\u{fc}cher.de/", None, "http://xn--bcher-kva.de/"),
            ("non-spec:/.//p", None, "non-spec:/.//p"),
            ("foo://host/a b", None, "foo://host/a%20b"),
            (
                "https://a.b/%7Bx%7D/{y}",
                None,
                "https://a.b/%7Bx%7D/%7By%7D",
            ),
        ] {
            assert_eq!(href(input, base).as_deref(), Some(expected), "{input}");
        }
        for (input, base) in [
            ("/relative", None),
            ("http://", None),
            ("http://a b/", None),
            ("http://[::1/", None),
            ("http://1.2.3.256/", None),
            ("https://a:99999/", None),
            ("x", Some("data:text/plain,hi")),
        ] {
            assert_eq!(href(input, base), None, "{input}");
        }
    }

    #[test]
    fn exposes_components_and_origins() {
        let url = Url::parse("https://u:p@Example.com:8443/p/q?x=1#h", None).expect("url");
        assert_eq!(url.host(), "example.com:8443");
        assert_eq!(url.hostname(), "example.com");
        assert_eq!(url.pathname(), "/p/q");
        assert_eq!((url.search().as_str(), url.hash().as_str()), ("?x=1", "#h"));
        assert_eq!(url.origin(), "https://example.com:8443");
        assert_eq!(
            Url::parse("blob:https://a.b/x", None)
                .expect("blob")
                .origin(),
            "https://a.b"
        );
        assert_eq!(Url::parse("data:,x", None).expect("data").origin(), "null");
    }

    #[test]
    fn setters_take_what_the_standard_lets_them() {
        let mut url = Url::parse("http://a.b/c?d#e", None).expect("url");
        url.set_protocol("https");
        url.set_host("x.y:8080");
        url.set_pathname("/one two/../three");
        url.set_search("q=a b");
        url.set_hash("");
        assert_eq!(url.to_string(), "https://x.y:8080/three?q=a%20b");
        url.set_port("443");
        url.set_hostname("z.w:1");
        url.set_username("me");
        assert_eq!(url.to_string(), "https://me@x.y/three?q=a%20b");
        url.set_protocol("mailto");
        url.set_port("99999");
        assert_eq!(url.to_string(), "https://me@x.y/three?q=a%20b");
        url.set_hostname("p.q");
        url.set_port("8080abc");
        assert_eq!(url.to_string(), "https://me@p.q:8080/three?q=a%20b");
        let mut opaque = Url::parse("mailto:a@b", None).expect("opaque");
        opaque.set_pathname("/x");
        opaque.set_host("c");
        assert_eq!(opaque.to_string(), "mailto:a@b");
    }
}
