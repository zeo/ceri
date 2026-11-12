// content security policy (W3C CSP Level 3): which loads, inline blocks and string compilation
// a document's policies allow

use std::cell::RefCell;
use std::collections::HashSet;

// the protected document's origin; documents only ever load from the app origin
const SELF_SCHEME: &str = "https";
const SELF_HOST: &str = "app.ceri.invalid";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Directive {
    ScriptElem,
    ScriptAttr,
    StyleElem,
    StyleAttr,
    Img,
    Media,
    Font,
    Connect,
    Worker,
    // eval, new Function and string timers
    Eval,
}

impl Directive {
    // most to least specific, per the spec's fallback list
    fn fallback(self) -> &'static [&'static str] {
        match self {
            Self::ScriptElem => &["script-src-elem", "script-src", "default-src"],
            Self::ScriptAttr => &["script-src-attr", "script-src", "default-src"],
            Self::StyleElem => &["style-src-elem", "style-src", "default-src"],
            Self::StyleAttr => &["style-src-attr", "style-src", "default-src"],
            Self::Worker => &["worker-src", "child-src", "script-src", "default-src"],
            Self::Img => &["img-src", "default-src"],
            Self::Media => &["media-src", "default-src"],
            Self::Font => &["font-src", "default-src"],
            Self::Connect => &["connect-src", "default-src"],
            Self::Eval => &["script-src", "default-src"],
        }
    }

    fn is_script(self) -> bool {
        matches!(self, Self::ScriptElem | Self::ScriptAttr)
    }

    fn is_attribute(self) -> bool {
        matches!(self, Self::ScriptAttr | Self::StyleAttr)
    }

    // how chromium names a refused load of this kind
    fn refusal(self) -> &'static str {
        match self {
            Self::ScriptElem | Self::ScriptAttr | Self::Eval => "load the script",
            Self::StyleElem | Self::StyleAttr => "load the stylesheet",
            Self::Img => "load the image",
            Self::Media => "load media from",
            Self::Font => "load the font",
            Self::Connect => "connect to",
            Self::Worker => "create a worker from",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Source {
    None,
    Self_,
    UnsafeInline,
    UnsafeEval,
    WasmUnsafeEval,
    UnsafeHashes,
    StrictDynamic,
    Nonce(String),
    // the algorithm's digest length in bytes and the digest as unpadded standard base64
    Hash(usize, String),
    Star,
    Scheme(String),
    Host {
        scheme: Option<String>,
        host: String,
        port: Option<String>,
        path: String,
    },
    // keywords this engine has no use for, such as 'report-sample'
    Other,
}

impl Source {
    fn parse(token: &str) -> Self {
        let lower = token.to_ascii_lowercase();
        match lower.as_str() {
            "'none'" => return Self::None,
            "'self'" => return Self::Self_,
            "'unsafe-inline'" => return Self::UnsafeInline,
            "'unsafe-eval'" => return Self::UnsafeEval,
            "'wasm-unsafe-eval'" => return Self::WasmUnsafeEval,
            "'unsafe-hashes'" => return Self::UnsafeHashes,
            "'strict-dynamic'" => return Self::StrictDynamic,
            "*" => return Self::Star,
            _ => {}
        }
        if let Some(quoted) = token.strip_prefix('\'').and_then(|t| t.strip_suffix('\'')) {
            if let Some(nonce) = strip_prefix_ignore_case(quoted, "nonce-") {
                return Self::Nonce(nonce.to_string());
            }
            for (prefix, length) in [("sha256-", 32), ("sha384-", 48), ("sha512-", 64)] {
                if let Some(value) = strip_prefix_ignore_case(quoted, prefix) {
                    return Self::Hash(length, normalize_base64(value));
                }
            }
            return Self::Other;
        }
        if let Some(scheme) = lower.strip_suffix(':')
            && is_scheme(scheme)
        {
            return Self::Scheme(scheme.to_string());
        }
        parse_host_source(&lower, token).unwrap_or(Self::Other)
    }
}

fn strip_prefix_ignore_case<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    text.get(..prefix.len())
        .filter(|head| head.eq_ignore_ascii_case(prefix))
        .map(|_| &text[prefix.len()..])
}

fn is_scheme(text: &str) -> bool {
    let mut characters = text.chars();
    characters.next().is_some_and(|c| c.is_ascii_alphabetic())
        && characters.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

// both base64 and base64url spellings compare equal
fn normalize_base64(value: &str) -> String {
    value
        .trim_end_matches('=')
        .replace('-', "+")
        .replace('_', "/")
}

// [scheme "://"] host [":" port] [path]; the path keeps its case
fn parse_host_source(lower: &str, original: &str) -> Option<Source> {
    let (scheme, rest, original_rest) = match lower.split_once("://") {
        Some((scheme, rest)) if is_scheme(scheme) => (
            Some(scheme.to_string()),
            rest,
            &original[scheme.len() + 3..],
        ),
        Some(_) => return None,
        None => (None, lower, original),
    };
    let authority_end = rest.find('/').unwrap_or(rest.len());
    let (authority, path) = (&rest[..authority_end], &original_rest[authority_end..]);
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) if port == "*" || port.bytes().all(|b| b.is_ascii_digit()) => {
            (host, Some(port.to_string()))
        }
        Some(_) => return None,
        None => (authority, None),
    };
    let labels = host.strip_prefix("*.").unwrap_or(host);
    let valid = host == "*"
        || (!labels.is_empty()
            && labels.split('.').all(|label| {
                !label.is_empty()
                    && label
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            }));
    valid.then(|| Source::Host {
        scheme,
        host: host.to_string(),
        port,
        path: path.to_string(),
    })
}

// the parts of a request URL that source expressions look at
struct Target<'a> {
    scheme: String,
    host: Option<String>,
    port: Option<u16>,
    path: &'a str,
}

impl<'a> Target<'a> {
    fn parse(url: &'a str) -> Option<Self> {
        let (scheme, rest) = url.split_once(':')?;
        if !is_scheme(scheme) {
            return None;
        }
        let scheme = scheme.to_ascii_lowercase();
        let Some(rest) = rest.strip_prefix("//") else {
            return Some(Self {
                scheme,
                host: None,
                port: None,
                path: rest,
            });
        };
        let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let authority = rest[..end]
            .rsplit_once('@')
            .map_or(&rest[..end], |(_, a)| a);
        let (host, port) = match authority.rsplit_once(':') {
            Some((host, port)) if !authority.ends_with(']') => (host, Some(port.parse().ok()?)),
            _ => (authority, None),
        };
        let path = &rest[end..];
        let path = &path[..path.find(['?', '#']).unwrap_or(path.len())];
        Some(Self {
            scheme,
            host: Some(host.trim_end_matches('.').to_ascii_lowercase()),
            port,
            path,
        })
    }

    fn effective_port(&self) -> Option<u16> {
        self.port.or_else(|| default_port(&self.scheme))
    }
}

fn default_port(scheme: &str) -> Option<u16> {
    match scheme {
        "http" | "ws" => Some(80),
        "https" | "wss" => Some(443),
        "ftp" => Some(21),
        _ => None,
    }
}

// a scheme written in a policy also takes the secure upgrades of itself
fn scheme_matches(expression: &str, url: &str) -> bool {
    expression == url
        || matches!(
            (expression, url),
            ("http", "https") | ("ws", "wss" | "http" | "https") | ("wss", "https")
        )
}

fn path_matches(expression: &str, path: &str) -> bool {
    if expression.is_empty() || (expression == "/" && path.is_empty()) {
        return true;
    }
    let exact = !expression.ends_with('/');
    let expression = expression.strip_suffix('/').unwrap_or(expression);
    let wanted: Vec<_> = expression.split('/').collect();
    let given: Vec<_> = path.split('/').collect();
    if wanted.len() > given.len() || (exact && wanted.len() != given.len()) {
        return false;
    }
    wanted
        .iter()
        .zip(&given)
        .all(|(wanted, given)| percent_decode(wanted) == percent_decode(given))
}

fn percent_decode(text: &str) -> Vec<u8> {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let hex = bytes
            .get(index + 1..index + 3)
            .and_then(|pair| std::str::from_utf8(pair).ok())
            .and_then(|pair| u8::from_str_radix(pair, 16).ok());
        match (bytes[index], hex) {
            (b'%', Some(byte)) => {
                decoded.push(byte);
                index += 3;
            }
            (byte, _) => {
                decoded.push(byte);
                index += 1;
            }
        }
    }
    decoded
}

impl Source {
    fn matches_url(&self, target: &Target<'_>) -> bool {
        match self {
            Self::Star => {
                matches!(target.scheme.as_str(), "http" | "https") || target.scheme == SELF_SCHEME
            }
            Self::Scheme(scheme) => scheme_matches(scheme, &target.scheme),
            Self::Self_ => {
                target.scheme != "blob"
                    && target.host.as_deref() == Some(SELF_HOST)
                    && target.effective_port() == Some(443)
                    && matches!(target.scheme.as_str(), "https" | "wss")
            }
            Self::Host {
                scheme,
                host,
                port,
                path,
            } => {
                let Some(url_host) = target.host.as_deref() else {
                    return false;
                };
                if !scheme_matches(scheme.as_deref().unwrap_or(SELF_SCHEME), &target.scheme) {
                    return false;
                }
                let host_matches = match host.strip_prefix('*') {
                    Some("") => true,
                    Some(suffix) => url_host.ends_with(suffix),
                    None => url_host == host,
                };
                let port_matches = match port.as_deref() {
                    Some("*") => true,
                    Some(port) => {
                        let port = port.parse::<u16>().ok();
                        port == target.effective_port()
                            || (port == Some(80)
                                && matches!(target.scheme.as_str(), "https" | "wss")
                                && target.effective_port() == Some(443))
                    }
                    None => target.port.is_none() || target.port == default_port(&target.scheme),
                };
                host_matches && port_matches && path_matches(path, target.path)
            }
            _ => false,
        }
    }
}

#[derive(Clone, Debug)]
struct Policy {
    text: String,
    directives: Vec<(String, Vec<Source>)>,
    // digests of the inline blocks the app bundled, which this policy lets run as the app's own
    // code; only the policy the app configured carries them
    bundled: HashSet<[u8; 32]>,
}

impl Policy {
    fn parse(text: &str) -> Option<Self> {
        let mut directives: Vec<(String, Vec<Source>)> = Vec::new();
        for directive in text.split(';') {
            let mut tokens = directive.split_ascii_whitespace();
            let Some(name) = tokens.next() else {
                continue;
            };
            let name = name.to_ascii_lowercase();
            // a repeated directive is ignored
            if directives.iter().any(|(existing, _)| *existing == name) {
                continue;
            }
            directives.push((name, tokens.map(Source::parse).collect()));
        }
        (!directives.is_empty()).then(|| Self {
            text: text.trim().to_string(),
            directives,
            bundled: HashSet::new(),
        })
    }

    // the directive in force for a check and its sources; none allows everything
    fn governing(&self, directive: Directive) -> Option<(&str, &[Source])> {
        directive.fallback().iter().find_map(|name| {
            self.directives
                .iter()
                .find(|(existing, _)| existing == name)
                .map(|(name, sources)| (name.as_str(), sources.as_slice()))
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Violation {
    // the directive the check fell under, and the one that refused it
    effective: &'static str,
    violated: String,
}

impl Violation {
    // the console line chromium writes for the same refusal, with detail after the directive
    fn message(&self, refused: &str, detail: &str) -> String {
        let fallback = match self.violated.split_ascii_whitespace().next() {
            Some(name) if name != self.effective => format!(
                " Note that '{}' was not explicitly set, so '{name}' is used as a fallback.",
                self.effective
            ),
            _ => String::new(),
        };
        format!(
            "Refused to {refused} because it violates the following Content Security Policy directive: \"{}\".{detail}{fallback}",
            self.violated
        )
    }
}

// the policies a document enforces; a load must pass every one
#[derive(Clone, Debug, Default)]
pub(crate) struct Policies {
    list: Vec<Policy>,
    // refusals found where no console was at hand, each once, until the page logs them
    refused: RefCell<Vec<String>>,
    seen: RefCell<HashSet<String>>,
}

// what an element brings to an inline or load check
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Initiator<'a> {
    pub(crate) nonce: Option<&'a str>,
    pub(crate) parser_inserted: bool,
}

impl Policies {
    // a header value may hold several policies separated by commas
    pub(crate) fn add(&mut self, text: &str) {
        self.list.extend(text.split(',').filter_map(Policy::parse));
    }

    // a policy the app configured, whose bundled inline blocks count as the app's own code
    pub(crate) fn add_bundled<'a>(
        &mut self,
        text: &str,
        inline: impl IntoIterator<Item = &'a str>,
    ) {
        let start = self.list.len();
        self.add(text);
        let digests: HashSet<_> = inline
            .into_iter()
            .map(|text| sha256(text.as_bytes()))
            .collect();
        for policy in &mut self.list[start..] {
            policy.bundled.clone_from(&digests);
        }
    }

    pub(crate) fn url(
        &self,
        directive: Directive,
        url: &str,
        element: Initiator<'_>,
    ) -> Result<(), String> {
        self.allows_url(directive, url, element)
            .map_err(|violation| violation.message(&format!("{} '{url}'", directive.refusal()), ""))
    }

    pub(crate) fn inline(
        &self,
        directive: Directive,
        text: &str,
        element: Initiator<'_>,
    ) -> Result<(), String> {
        self.allows_inline(directive, text, element)
            .map_err(|violation| {
                let refused = match directive {
                    Directive::ScriptAttr => "execute inline event handler",
                    Directive::StyleElem | Directive::StyleAttr => "apply inline style",
                    _ => "execute inline script",
                };
                let hashes = match directive.is_attribute() {
                    true => {
                        " Note that hashes do not apply to event handlers, style attributes and \
                         javascript: navigations unless the 'unsafe-hashes' keyword is present."
                    }
                    false => "",
                };
                violation.message(
                    refused,
                    &format!(
                        " Either the 'unsafe-inline' keyword, a hash ({}), or a nonce \
                         ('nonce-...') is required to enable inline execution.{hashes}",
                        hash_source(text)
                    ),
                )
            })
    }

    pub(crate) fn eval(&self) -> Result<(), String> {
        self.allows_eval().map_err(|violation| {
            format!(
                "Refused to evaluate a string as JavaScript because 'unsafe-eval' is not an \
                 allowed source of script in the following Content Security Policy directive: \
                 \"{}\".",
                violation.violated
            )
        })
    }

    pub(crate) fn wasm(&self) -> Result<(), String> {
        self.allows_wasm().map_err(|violation| {
            format!(
                "Refused to compile or instantiate WebAssembly module because \
                 'wasm-unsafe-eval' is not an allowed source of script in the following \
                 Content Security Policy directive: \"{}\".",
                violation.violated
            )
        })
    }

    // keeps a refusal for the console when the check ran where none is at hand
    pub(crate) fn permits(&self, check: Result<(), String>) -> bool {
        check
            .map_err(|message| {
                if self.seen.borrow_mut().insert(message.clone()) {
                    self.refused.borrow_mut().push(message);
                }
            })
            .is_ok()
    }

    pub(crate) fn take_refused(&self) -> Vec<String> {
        std::mem::take(&mut *self.refused.borrow_mut())
    }

    fn check(
        &self,
        directive: Directive,
        mut allowed: impl FnMut(&Policy, &[Source]) -> bool,
    ) -> Result<(), Violation> {
        for policy in &self.list {
            if let Some((name, sources)) = policy.governing(directive)
                && !allowed(policy, sources)
            {
                let effective = directive.fallback()[0];
                let violated = policy
                    .text
                    .split(';')
                    .map(str::trim)
                    .find(|part| {
                        part.split_ascii_whitespace()
                            .next()
                            .is_some_and(|first| first.eq_ignore_ascii_case(name))
                    })
                    .unwrap_or(name)
                    .to_string();
                return Err(Violation {
                    effective,
                    violated,
                });
            }
        }
        Ok(())
    }

    // a request for url on behalf of an element, or of script when there is none
    fn allows_url(
        &self,
        directive: Directive,
        url: &str,
        element: Initiator<'_>,
    ) -> Result<(), Violation> {
        let target = Target::parse(url);
        self.check(directive, |_, sources| {
            if nonce_matches(sources, element.nonce) {
                return true;
            }
            if directive.is_script() && sources.contains(&Source::StrictDynamic) {
                return !element.parser_inserted;
            }
            target
                .as_ref()
                .is_some_and(|target| sources.iter().any(|source| source.matches_url(target)))
        })
    }

    // an inline script, style, event handler or style attribute
    fn allows_inline(
        &self,
        directive: Directive,
        text: &str,
        element: Initiator<'_>,
    ) -> Result<(), Violation> {
        let mut digests: [Option<String>; 3] = Default::default();
        self.check(directive, |policy, sources| {
            if allows_all_inline(sources, directive) || nonce_matches(sources, element.nonce) {
                return true;
            }
            if element.parser_inserted
                && !directive.is_attribute()
                && !policy.bundled.is_empty()
                && policy.bundled.contains(&sha256(text.as_bytes()))
            {
                return true;
            }
            if directive.is_attribute() && !sources.contains(&Source::UnsafeHashes) {
                return false;
            }
            sources.iter().any(|source| match source {
                Source::Hash(length, value) => {
                    let slot = match length {
                        32 => 0,
                        48 => 1,
                        _ => 2,
                    };
                    let digest = digests[slot].get_or_insert_with(|| {
                        let bytes = text.as_bytes();
                        let digest = match length {
                            32 => sha256(bytes).to_vec(),
                            48 => sha512(bytes, true)[..48].to_vec(),
                            _ => sha512(bytes, false).to_vec(),
                        };
                        normalize_base64(&base64(&digest))
                    });
                    digest == value
                }
                _ => false,
            })
        })
    }

    // eval, new Function and string timers
    fn allows_eval(&self) -> Result<(), Violation> {
        self.check(Directive::Eval, |_, sources| {
            sources.contains(&Source::UnsafeEval)
        })
    }

    fn allows_wasm(&self) -> Result<(), Violation> {
        self.check(Directive::Eval, |_, sources| {
            sources.contains(&Source::UnsafeEval) || sources.contains(&Source::WasmUnsafeEval)
        })
    }
}

fn nonce_matches(sources: &[Source], nonce: Option<&str>) -> bool {
    nonce.is_some_and(|nonce| {
        !nonce.is_empty()
            && sources
                .iter()
                .any(|source| matches!(source, Source::Nonce(value) if value == nonce))
    })
}

// 'unsafe-inline' stops counting once a nonce or hash, or 'strict-dynamic' for script, is listed
fn allows_all_inline(sources: &[Source], directive: Directive) -> bool {
    sources.contains(&Source::UnsafeInline)
        && !sources.iter().any(|source| {
            matches!(source, Source::Nonce(_) | Source::Hash(..))
                || (*source == Source::StrictDynamic && directive.is_script())
        })
}

// the source expression that would let this inline text run, as chromium suggests it
fn hash_source(text: &str) -> String {
    let mut digest = base64(&sha256(text.as_bytes()));
    while !digest.len().is_multiple_of(4) {
        digest.push('=');
    }
    format!("'sha256-{digest}'")
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut text = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let word = chunk
            .iter()
            .zip([16, 8, 0])
            .fold(0_u32, |word, (&byte, shift)| {
                word | u32::from(byte) << shift
            });
        for index in 0..=chunk.len() {
            text.push(char::from(
                ALPHABET[(word >> (18 - 6 * index) & 63) as usize],
            ));
        }
    }
    text
}

const SHA256_ROUNDS: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

const SHA512_ROUNDS: [u64; 80] = [
    0x428a2f98d728ae22,
    0x7137449123ef65cd,
    0xb5c0fbcfec4d3b2f,
    0xe9b5dba58189dbbc,
    0x3956c25bf348b538,
    0x59f111f1b605d019,
    0x923f82a4af194f9b,
    0xab1c5ed5da6d8118,
    0xd807aa98a3030242,
    0x12835b0145706fbe,
    0x243185be4ee4b28c,
    0x550c7dc3d5ffb4e2,
    0x72be5d74f27b896f,
    0x80deb1fe3b1696b1,
    0x9bdc06a725c71235,
    0xc19bf174cf692694,
    0xe49b69c19ef14ad2,
    0xefbe4786384f25e3,
    0x0fc19dc68b8cd5b5,
    0x240ca1cc77ac9c65,
    0x2de92c6f592b0275,
    0x4a7484aa6ea6e483,
    0x5cb0a9dcbd41fbd4,
    0x76f988da831153b5,
    0x983e5152ee66dfab,
    0xa831c66d2db43210,
    0xb00327c898fb213f,
    0xbf597fc7beef0ee4,
    0xc6e00bf33da88fc2,
    0xd5a79147930aa725,
    0x06ca6351e003826f,
    0x142929670a0e6e70,
    0x27b70a8546d22ffc,
    0x2e1b21385c26c926,
    0x4d2c6dfc5ac42aed,
    0x53380d139d95b3df,
    0x650a73548baf63de,
    0x766a0abb3c77b2a8,
    0x81c2c92e47edaee6,
    0x92722c851482353b,
    0xa2bfe8a14cf10364,
    0xa81a664bbc423001,
    0xc24b8b70d0f89791,
    0xc76c51a30654be30,
    0xd192e819d6ef5218,
    0xd69906245565a910,
    0xf40e35855771202a,
    0x106aa07032bbd1b8,
    0x19a4c116b8d2d0c8,
    0x1e376c085141ab53,
    0x2748774cdf8eeb99,
    0x34b0bcb5e19b48a8,
    0x391c0cb3c5c95a63,
    0x4ed8aa4ae3418acb,
    0x5b9cca4f7763e373,
    0x682e6ff3d6b2b8a3,
    0x748f82ee5defb2fc,
    0x78a5636f43172f60,
    0x84c87814a1f0ab72,
    0x8cc702081a6439ec,
    0x90befffa23631e28,
    0xa4506cebde82bde9,
    0xbef9a3f7b2c67915,
    0xc67178f2e372532b,
    0xca273eceea26619c,
    0xd186b8c721c0c207,
    0xeada7dd6cde0eb1e,
    0xf57d4f7fee6ed178,
    0x06f067aa72176fba,
    0x0a637dc5a2c898a6,
    0x113f9804bef90dae,
    0x1b710b35131c471b,
    0x28db77f523047d84,
    0x32caab7b40c72493,
    0x3c9ebe0a15c9bebc,
    0x431d67c49c100d4c,
    0x4cc5d4becb3e42b6,
    0x597f299cfc657e2a,
    0x5fcb6fab3ad6faec,
    0x6c44198c4a475817,
];

// the message, the 0x80 marker, zeros, then the bit length in length_bytes big-endian bytes
fn padded(data: &[u8], block: usize, length_bytes: usize) -> Vec<u8> {
    let mut message = data.to_vec();
    message.push(0x80);
    while message.len() % block != block - length_bytes {
        message.push(0);
    }
    let bits = (data.len() as u128 * 8).to_be_bytes();
    message.extend_from_slice(&bits[16 - length_bytes..]);
    message
}

fn sha256(data: &[u8]) -> [u8; 32] {
    let mut state: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    for block in padded(data, 64, 8).as_chunks::<64>().0 {
        let mut words = [0_u32; 64];
        for (word, bytes) in words.iter_mut().zip(block.as_chunks::<4>().0) {
            *word = u32::from_be_bytes(*bytes);
        }
        for i in 16..64 {
            let (a, b) = (words[i - 15], words[i - 2]);
            let s0 = a.rotate_right(7) ^ a.rotate_right(18) ^ (a >> 3);
            let s1 = b.rotate_right(17) ^ b.rotate_right(19) ^ (b >> 10);
            words[i] = words[i - 16]
                .wrapping_add(s0)
                .wrapping_add(words[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = state;
        for (round, word) in SHA256_ROUNDS.iter().zip(words) {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let choice = (e & f) ^ (!e & g);
            let t1 = h
                .wrapping_add(s1)
                .wrapping_add(choice)
                .wrapping_add(*round)
                .wrapping_add(word);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            (h, g, f, e, d, c, b, a) = (
                g,
                f,
                e,
                d.wrapping_add(t1),
                c,
                b,
                a,
                t1.wrapping_add(s0.wrapping_add(majority)),
            );
        }
        for (value, add) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *value = value.wrapping_add(add);
        }
    }
    let mut digest = [0; 32];
    for (bytes, value) in digest.as_chunks_mut::<4>().0.iter_mut().zip(state) {
        *bytes = value.to_be_bytes();
    }
    digest
}

// sha-512, or with short the sha-384 starting state, whose digest is the first 48 bytes
fn sha512(data: &[u8], short: bool) -> [u8; 64] {
    let mut state: [u64; 8] = match short {
        false => [
            0x6a09e667f3bcc908,
            0xbb67ae8584caa73b,
            0x3c6ef372fe94f82b,
            0xa54ff53a5f1d36f1,
            0x510e527fade682d1,
            0x9b05688c2b3e6c1f,
            0x1f83d9abfb41bd6b,
            0x5be0cd19137e2179,
        ],
        true => [
            0xcbbb9d5dc1059ed8,
            0x629a292a367cd507,
            0x9159015a3070dd17,
            0x152fecd8f70e5939,
            0x67332667ffc00b31,
            0x8eb44a8768581511,
            0xdb0c2e0d64f98fa7,
            0x47b5481dbefa4fa4,
        ],
    };
    for block in padded(data, 128, 16).as_chunks::<128>().0 {
        let mut words = [0_u64; 80];
        for (word, bytes) in words.iter_mut().zip(block.as_chunks::<8>().0) {
            *word = u64::from_be_bytes(*bytes);
        }
        for i in 16..80 {
            let (a, b) = (words[i - 15], words[i - 2]);
            let s0 = a.rotate_right(1) ^ a.rotate_right(8) ^ (a >> 7);
            let s1 = b.rotate_right(19) ^ b.rotate_right(61) ^ (b >> 6);
            words[i] = words[i - 16]
                .wrapping_add(s0)
                .wrapping_add(words[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = state;
        for (round, word) in SHA512_ROUNDS.iter().zip(words) {
            let s1 = e.rotate_right(14) ^ e.rotate_right(18) ^ e.rotate_right(41);
            let choice = (e & f) ^ (!e & g);
            let t1 = h
                .wrapping_add(s1)
                .wrapping_add(choice)
                .wrapping_add(*round)
                .wrapping_add(word);
            let s0 = a.rotate_right(28) ^ a.rotate_right(34) ^ a.rotate_right(39);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            (h, g, f, e, d, c, b, a) = (
                g,
                f,
                e,
                d.wrapping_add(t1),
                c,
                b,
                a,
                t1.wrapping_add(s0.wrapping_add(majority)),
            );
        }
        for (value, add) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *value = value.wrapping_add(add);
        }
    }
    let mut digest = [0; 64];
    for (bytes, value) in digest.as_chunks_mut::<8>().0.iter_mut().zip(state) {
        *bytes = value.to_be_bytes();
    }
    digest
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    #[test]
    fn digests_match_the_published_vectors() {
        assert_eq!(
            hex(&sha256(b"")),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            hex(&sha256(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let long = b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq";
        assert_eq!(
            hex(&sha256(long)),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        assert_eq!(
            hex(&sha512(b"abc", false)),
            "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a\
             2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f"
        );
        assert_eq!(
            hex(&sha512(b"abc", true)[..48]),
            "cb00753f45a35e8bb5a03d699ac65007272c32ab0eded1631a8b605a43ff5bed\
             8086072ba1e7cc2358baeca134c825a7"
        );
        assert_eq!(
            base64(&sha256(b"")),
            "47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU"
        );
    }

    fn policies(text: &str) -> Policies {
        let mut policies = Policies::default();
        policies.add(text);
        policies
    }

    const PAGE: Initiator<'static> = Initiator {
        nonce: None,
        parser_inserted: true,
    };

    #[test]
    fn urls_fall_back_through_the_directive_list() {
        let csp = policies(
            "default-src 'self'; img-src 'self' data: https://files.ceri.invalid; connect-src https://*.example.com:*/api/",
        );
        let img = |url| csp.allows_url(Directive::Img, url, PAGE).is_ok();
        assert!(img("https://app.ceri.invalid/a.png"));
        assert!(img("data:image/png;base64,AAAA"));
        assert!(img("https://files.ceri.invalid/x/y.png"));
        assert!(!img("blob:https://app.ceri.invalid/1"));
        assert!(!img("https://other.example/a.png"));
        let media = |url| csp.allows_url(Directive::Media, url, PAGE).is_ok();
        assert!(media("https://app.ceri.invalid/v.mp4"));
        assert!(
            !media("https://files.ceri.invalid/v.mp4"),
            "default-src applies"
        );
        let connect = |url| csp.allows_url(Directive::Connect, url, PAGE).is_ok();
        assert!(connect("https://a.b.example.com:8443/api/x"));
        assert!(
            !connect("https://example.com/api/x"),
            "the wildcard needs a subdomain"
        );
        assert!(!connect("https://a.example.com/other"));
        assert!(
            !connect("https://app.ceri.invalid/x"),
            "connect-src does not fall back"
        );
        assert_eq!(
            csp.url(Directive::Media, "https://files.ceri.invalid/v.mp4", PAGE)
                .unwrap_err(),
            "Refused to load media from 'https://files.ceri.invalid/v.mp4' because it violates \
             the following Content Security Policy directive: \"default-src 'self'\". Note that \
             'media-src' was not explicitly set, so 'default-src' is used as a fallback."
        );
        assert!(
            policies("font-src 'self'")
                .allows_url(Directive::Img, "https://x.test/", PAGE)
                .is_ok()
        );
    }

    #[test]
    fn stars_schemes_and_paths_follow_the_matching_rules() {
        let csp =
            policies("img-src *; script-src https://app.ceri.invalid/js/app.js http://cdn.test");
        assert!(
            csp.allows_url(Directive::Img, "https://any.test/a", PAGE)
                .is_ok()
        );
        assert!(
            csp.allows_url(Directive::Img, "data:,x", PAGE).is_err(),
            "* leaves out data:"
        );
        let script = |url| csp.allows_url(Directive::ScriptElem, url, PAGE).is_ok();
        assert!(script("https://app.ceri.invalid/js/app.js?v=2"));
        assert!(!script("https://app.ceri.invalid/js/app.js/more"));
        assert!(script("https://cdn.test/lib.js"), "http upgrades to https");
        assert!(!script("https://cdn.test:8443/lib.js"));
        assert!(
            policies("img-src 'none'")
                .allows_url(Directive::Img, "https://app.ceri.invalid/", PAGE)
                .is_err()
        );
    }

    #[test]
    fn inline_code_needs_unsafe_inline_a_nonce_or_a_hash() {
        let text = "alert(1)";
        let digest = base64(&sha256(text.as_bytes()));
        let inline = |policy: &str, element| {
            policies(policy)
                .allows_inline(Directive::ScriptElem, text, element)
                .is_ok()
        };
        assert!(!inline("default-src 'self'; style-src 'none'", PAGE));
        assert!(inline("script-src 'unsafe-inline'", PAGE));
        assert!(
            inline("img-src 'none'", PAGE),
            "no script directive allows it"
        );
        assert!(inline(&format!("script-src 'sha256-{digest}'"), PAGE));
        let url_safe = digest.replace('+', "-").replace('/', "_");
        assert!(inline(&format!("script-src 'SHA256-{url_safe}='"), PAGE));
        assert!(
            !inline("script-src 'unsafe-inline' 'nonce-abc'", PAGE),
            "a nonce turns it off"
        );
        let nonced = Initiator {
            nonce: Some("abc"),
            parser_inserted: true,
        };
        assert!(inline("script-src 'unsafe-inline' 'nonce-abc'", nonced));
        assert!(!inline("script-src 'strict-dynamic' 'unsafe-inline'", PAGE));
        assert!(
            policies("style-src 'strict-dynamic' 'unsafe-inline'")
                .allows_inline(Directive::StyleElem, "a{}", PAGE)
                .is_ok()
        );
        let attribute = |policy: &str| {
            policies(policy)
                .allows_inline(Directive::ScriptAttr, text, PAGE)
                .is_ok()
        };
        assert!(!attribute(&format!("script-src 'sha256-{digest}'")));
        assert!(attribute(&format!(
            "script-src 'unsafe-hashes' 'sha256-{digest}'"
        )));
        assert!(!attribute("script-src 'self'; script-src-attr 'none'"));
        assert!(attribute(
            "script-src 'self'; script-src-attr 'unsafe-inline'"
        ));
        let sha384 = base64(&sha512(text.as_bytes(), true)[..48]);
        assert!(inline(&format!("script-src 'sha384-{sha384}'"), PAGE));
        let sha512 = base64(&sha512(text.as_bytes(), false));
        assert!(inline(&format!("script-src 'sha512-{sha512}'"), PAGE));
    }

    #[test]
    fn every_policy_must_allow_and_bundled_blocks_run() {
        let mut csp = Policies::default();
        csp.add_bundled("script-src 'self'", ["boot()"]);
        assert!(
            csp.allows_inline(Directive::ScriptElem, "boot()", PAGE)
                .is_ok()
        );
        assert!(
            csp.allows_inline(Directive::ScriptElem, "evil()", PAGE)
                .is_err()
        );
        let inserted = Initiator {
            nonce: None,
            parser_inserted: false,
        };
        assert!(
            csp.allows_inline(Directive::ScriptElem, "boot()", inserted)
                .is_err(),
            "only the parser's copy is the bundled one"
        );
        csp.add("script-src 'none', img-src 'self'");
        assert!(
            csp.allows_inline(Directive::ScriptElem, "boot()", PAGE)
                .is_err()
        );
        assert!(csp.allows_eval().is_err());
        assert!(policies("script-src 'unsafe-eval'").allows_eval().is_ok());
        assert!(
            policies("default-src 'wasm-unsafe-eval'")
                .allows_wasm()
                .is_ok()
        );
        assert!(
            policies("default-src 'wasm-unsafe-eval'")
                .allows_eval()
                .is_err()
        );
        assert_eq!(
            policies("default-src 'self'").eval().unwrap_err(),
            "Refused to evaluate a string as JavaScript because 'unsafe-eval' is not an allowed \
             source of script in the following Content Security Policy directive: \
             \"default-src 'self'\"."
        );
    }

    #[test]
    fn strict_dynamic_trusts_script_built_scripts() {
        let csp = policies("script-src 'strict-dynamic' 'nonce-n' 'self'");
        let url = "https://app.ceri.invalid/a.js";
        assert!(
            csp.allows_url(Directive::ScriptElem, url, PAGE).is_err(),
            "'self' is ignored"
        );
        let nonced = Initiator {
            nonce: Some("n"),
            parser_inserted: true,
        };
        assert!(csp.allows_url(Directive::ScriptElem, url, nonced).is_ok());
        let inserted = Initiator {
            nonce: None,
            parser_inserted: false,
        };
        assert!(csp.allows_url(Directive::ScriptElem, url, inserted).is_ok());
        assert!(csp.allows_url(Directive::Img, url, PAGE).is_ok());
    }
}
