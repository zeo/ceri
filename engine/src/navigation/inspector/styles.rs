// the CSS domain: the page's sheets, the rules that match a node with where each was written,
// its computed style, edits to sheets and style attributes, and pseudo-classes held on

use super::{Failure, dom, frame_id, post};
use crate::css::{
    self, FORCEABLE_PSEUDO_CLASSES, MatchedRule, PseudoElement, Stylesheet, USER_AGENT_CSS,
};
use crate::dom::{NodeId, StylesheetSource};
use crate::navigation::{AppUrl, Browser, Page, joined_sheets, load_web_fonts};
use crate::script::NodeArgument;
use serde_json::{Value as Json, json};
use std::collections::BTreeSet;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::ops::Range;
use std::rc::Rc;

// the sheets the sessions were told of, as of a document version in an execution context
#[derive(Default)]
pub(super) struct Sheets {
    entries: Vec<Sheet>,
    context: Option<u64>,
    version: Option<u64>,
    next: u64,
}

struct Sheet {
    id: u64,
    owner: NodeId,
    // a hash of its text, which tells a rewritten sheet from the one before
    print: u64,
    // its styleSheetAdded header, less the owner node each session numbers itself
    header: Json,
}

// a style element or stylesheet link, with the text the page's stylesheet was built from
struct Listed {
    owner: NodeId,
    inline: bool,
    url: String,
    text: String,
}

// a sheet's text where a rule's places can be read, and where it starts in the joined text
// the page's stylesheet was parsed from
struct Source {
    id: Option<String>,
    text: String,
    lines: Lines,
    start: Option<usize>,
}

enum Target {
    Sheet(usize, NodeId),
    Attribute(NodeId),
}

const PSEUDO_ELEMENTS: [PseudoElement; 10] = [
    PseudoElement::Before,
    PseudoElement::After,
    PseudoElement::Marker,
    PseudoElement::Backdrop,
    PseudoElement::Selection,
    PseudoElement::Placeholder,
    PseudoElement::Scrollbar,
    PseudoElement::ScrollbarThumb,
    PseudoElement::ScrollbarTrack,
    PseudoElement::ScrollbarCorner,
];

const COMPUTED_STYLE: &str = "function () {
    const style = getComputedStyle(this);
    return Array.from(style, (name) => [name, style.getPropertyValue(name)]);
}";

const SET_STYLE_ATTRIBUTE: &str = "function (text) { this.setAttribute('style', text); }";

fn unavailable() -> Failure {
    (-32000, "Document is not available".to_string())
}

fn sheet_id(id: u64) -> String {
    format!("sheet-{id}")
}

fn fingerprint(text: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

impl Browser {
    // tells the css sessions which sheets came and went since the document last changed
    pub(super) fn sync_sheets(&mut self) {
        let Some(page) = &self.page else {
            return;
        };
        let context = page.runtime.inspector_context();
        let version = page.document.borrow().version();
        let held = &self.inspection.sheets;
        if held.context == Some(context) && held.version == Some(version) {
            return;
        }
        let listed = self.listed_sheets(page);
        let inspection = &mut self.inspection;
        let sheets = &mut inspection.sheets;
        if sheets.context != Some(context) {
            // the frontend drops a replaced document's sheets on its own
            sheets.entries.clear();
            sheets.context = Some(context);
        }
        sheets.version = Some(version);
        let mut entries = Vec::with_capacity(listed.len());
        let mut added = Vec::new();
        for sheet in listed {
            let print = fingerprint(&sheet.text);
            let kept = sheets
                .entries
                .iter()
                .position(|held| held.owner == sheet.owner && held.print == print);
            match kept {
                Some(index) => entries.push(sheets.entries.remove(index)),
                None => {
                    sheets.next += 1;
                    added.push(entries.len());
                    entries.push(Sheet {
                        id: sheets.next,
                        owner: sheet.owner,
                        print,
                        header: header(sheets.next, &sheet),
                    });
                }
            }
        }
        let removed = std::mem::replace(&mut sheets.entries, entries);
        for session in inspection.sessions.iter_mut().filter(|held| held.css) {
            for sheet in &removed {
                post(
                    session,
                    "CSS.styleSheetRemoved",
                    json!({ "styleSheetId": sheet_id(sheet.id) }),
                );
            }
            for &index in &added {
                let sheet = &sheets.entries[index];
                let mut header = sheet.header.clone();
                header["ownerNode"] = session.dom.id(sheet.owner).into();
                post(session, "CSS.styleSheetAdded", json!({ "header": header }));
            }
        }
    }

    fn listed_sheets(&self, page: &Page) -> Vec<Listed> {
        self.stylesheet(page);
        let document = page.document.borrow();
        let inputs = page.layout_inputs.borrow();
        document
            .stylesheet_elements()
            .into_iter()
            .zip(&inputs.sheets)
            .map(|((owner, source), (_, text))| {
                let (inline, url) = match source {
                    StylesheetSource::Inline(_) => (true, self.document_url()),
                    StylesheetSource::External(href) => {
                        let resolved = self.current_url.as_ref().and_then(|base| {
                            base.resolve(&href).ok().map(|url: AppUrl| url.to_string())
                        });
                        (false, resolved.unwrap_or(href))
                    }
                };
                Listed {
                    owner,
                    inline,
                    url,
                    text: text.clone(),
                }
            })
            .collect()
    }

    fn sources(&self, page: &Page) -> Vec<Source> {
        let mut start = 0;
        self.listed_sheets(page)
            .into_iter()
            .map(|sheet| {
                let at = (!sheet.text.is_empty()).then(|| {
                    let at = start;
                    start += sheet.text.len() + 1;
                    at
                });
                let id = self
                    .inspection
                    .sheets
                    .entries
                    .iter()
                    .find(|held| held.owner == sheet.owner)
                    .map(|held| sheet_id(held.id));
                Source {
                    id,
                    lines: Lines::new(&sheet.text),
                    text: sheet.text,
                    start: at,
                }
            })
            .collect()
    }

    // answers one CSS method
    pub(super) fn answer_css(
        &mut self,
        session: u64,
        method: &str,
        params: &Json,
    ) -> Result<Json, Failure> {
        if self.page.is_none() {
            return Err(unavailable());
        }
        self.sync_sheets();
        match method {
            "CSS.enable" => {
                let headers: Vec<_> = self
                    .inspection
                    .sheets
                    .entries
                    .iter()
                    .map(|sheet| (sheet.owner, sheet.header.clone()))
                    .collect();
                let held = self.session(session).ok_or_else(unavailable)?;
                if !held.css {
                    held.css = true;
                    for (owner, mut header) in headers {
                        header["ownerNode"] = held.dom.id(owner).into();
                        post(held, "CSS.styleSheetAdded", json!({ "header": header }));
                    }
                }
                Ok(json!({}))
            }
            "CSS.disable" => {
                if let Some(held) = self.session(session) {
                    held.css = false;
                }
                if let Some(page) = &mut self.page {
                    page.runtime.forced_pseudo_classes.clear();
                }
                Ok(json!({}))
            }
            "CSS.getMatchedStylesForNode" => self.matched_styles(session, params),
            "CSS.getInlineStylesForNode" => {
                let node = self.css_node(session, params)?;
                let id = self.session(session).ok_or_else(unavailable)?.dom.id(node);
                let page = self.page.as_ref().ok_or_else(unavailable)?;
                let document = page.document.borrow();
                let text = document.attribute(node, "style").unwrap_or_default();
                Ok(json!({ "inlineStyle": attribute_style(text, id) }))
            }
            "CSS.getComputedStyleForNode" => {
                let node = self.css_node(session, params)?;
                let page = self.page.as_mut().ok_or_else(unavailable)?;
                let called = dom::call(page, node, COMPUTED_STYLE, Vec::new())?;
                let computed: Vec<Json> = called
                    .value
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|pair| {
                        let (name, value) = (pair[0].as_str()?, pair[1].as_str()?);
                        Some(json!({ "name": name, "value": value }))
                    })
                    .collect();
                Ok(json!({
                    "computedStyle": computed,
                    "extraFields": { "isAppearanceBase": false },
                }))
            }
            "CSS.getBackgroundColors" => {
                // the colours are optional, and leaving them out tells the frontend they are
                // unknown
                self.css_node(session, params)?;
                Ok(json!({}))
            }
            // the engine has no env() variables to report
            "CSS.getEnvironmentVariables" => Ok(json!({ "environmentVariables": {} })),
            "CSS.getStyleSheetText" => {
                let target = self.css_target(session, &params["styleSheetId"])?;
                Ok(json!({ "text": self.target_text(&target) }))
            }
            "CSS.setStyleSheetText" => {
                let target = self.css_target(session, &params["styleSheetId"])?;
                let text = params["text"].as_str().unwrap_or_default();
                self.set_target_text(&target, text.to_string())?;
                Ok(json!({}))
            }
            "CSS.setStyleTexts" => {
                let mut styles = Vec::new();
                for edit in params["edits"].as_array().into_iter().flatten() {
                    let id = edit["styleSheetId"].as_str().unwrap_or_default();
                    let target = self.css_target(session, &edit["styleSheetId"])?;
                    let text = self.target_text(&target);
                    let span = Lines::new(&text).span(&text, &edit["range"])?;
                    let replacement = edit["text"].as_str().unwrap_or_default();
                    if !balanced(replacement) {
                        return Err((-32000, "Style text is not valid".into()));
                    }
                    let edited = splice(&text, &span, replacement);
                    self.set_target_text(&target, edited.clone())?;
                    let block = span.start..span.start + replacement.len();
                    let lines = Lines::new(&edited);
                    styles.push(style(&edited, block, Some((id, &lines))));
                }
                Ok(json!({ "styles": styles }))
            }
            "CSS.setRuleSelector" => {
                let target = self.css_target(session, &params["styleSheetId"])?;
                if matches!(target, Target::Attribute(_)) {
                    return Err((-32000, "An inline style has no selector".into()));
                }
                let text = self.target_text(&target);
                let span = Lines::new(&text).span(&text, &params["range"])?;
                let selector = params["selector"].as_str().unwrap_or_default().trim();
                let rule = format!("{selector} {{ color: red }}");
                if selector.contains(['{', '}', ';']) || Stylesheet::parse(&rule).is_err() {
                    return Err((-32000, "Selector parse error".into()));
                }
                let edited = splice(&text, &span, selector);
                self.set_target_text(&target, edited.clone())?;
                let header = span.start..span.start + selector.len();
                let lines = Lines::new(&edited);
                Ok(json!({ "selectorList": selector_list(&edited, header, Some(&lines)) }))
            }
            "CSS.forcePseudoState" => {
                let node = self.css_node(session, params)?;
                let classes: BTreeSet<&'static str> = params["forcedPseudoClasses"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Json::as_str)
                    .filter_map(|name| {
                        FORCEABLE_PSEUDO_CLASSES
                            .iter()
                            .copied()
                            .find(|class| *class == name)
                    })
                    .collect();
                let page = self.page.as_mut().ok_or_else(unavailable)?;
                let forced = &mut page.runtime.forced_pseudo_classes;
                match classes.is_empty() {
                    true => forced.remove(&node),
                    false => forced.insert(node, classes),
                };
                Ok(json!({}))
            }
            _ => Err((-32601, format!("'{method}' wasn't found"))),
        }
    }

    fn css_node(&self, session: u64, params: &Json) -> Result<NodeId, Failure> {
        let held = self
            .inspection
            .sessions
            .iter()
            .find(|held| held.id == session)
            .ok_or_else(unavailable)?;
        let page = self.page.as_ref().ok_or_else(unavailable)?;
        dom::target(&held.dom, page, params)
    }

    // a sheet by the id its header gave, or a style attribute by its element's node id
    fn css_target(&self, session: u64, id: &Json) -> Result<Target, Failure> {
        let missing = || (-32000, "No style sheet with given id found".to_string());
        let id = id.as_str().unwrap_or_default();
        if let Some(node) = id.strip_prefix("style-") {
            let node = self.css_node(session, &json!({ "nodeId": node.parse::<u64>().ok() }))?;
            return Ok(Target::Attribute(node));
        }
        let number = id
            .strip_prefix("sheet-")
            .and_then(|n| n.parse::<u64>().ok());
        let owner = self
            .inspection
            .sheets
            .entries
            .iter()
            .find(|sheet| Some(sheet.id) == number)
            .ok_or_else(missing)?
            .owner;
        let page = self.page.as_ref().ok_or_else(unavailable)?;
        let index = page
            .document
            .borrow()
            .stylesheet_elements()
            .iter()
            .position(|(node, _)| *node == owner)
            .ok_or_else(missing)?;
        Ok(Target::Sheet(index, owner))
    }

    fn target_text(&self, target: &Target) -> String {
        let Some(page) = &self.page else {
            return String::new();
        };
        match *target {
            Target::Sheet(index, _) => page
                .layout_inputs
                .borrow()
                .sheets
                .get(index)
                .map(|(_, text)| text.clone())
                .unwrap_or_default(),
            Target::Attribute(node) => page
                .document
                .borrow()
                .attribute(node, "style")
                .unwrap_or_default()
                .to_string(),
        }
    }

    // a style attribute is set the way page script sets it; a sheet keeps the edited text until
    // its element or link changes, and the page restyles from it
    fn set_target_text(&mut self, target: &Target, text: String) -> Result<(), Failure> {
        let page = self.page.as_mut().ok_or_else(unavailable)?;
        let (index, owner) = match *target {
            Target::Attribute(node) => {
                let argument = vec![NodeArgument::Text(text)];
                return dom::call(page, node, SET_STYLE_ATTRIBUTE, argument).map(|_| ());
            }
            Target::Sheet(index, owner) => (index, owner),
        };
        let page = self.page.as_ref().ok_or_else(unavailable)?;
        let print = fingerprint(&text);
        let mut inputs = page.layout_inputs.borrow_mut();
        let Some(sheet) = inputs.sheets.get_mut(index) else {
            return Err((-32000, "No style sheet with given id found".into()));
        };
        sheet.1 = text;
        let (stylesheet, _) = Stylesheet::parse_recovering(&joined_sheets(&inputs.sheets));
        if stylesheet.font_faces() != inputs.stylesheet.font_faces()
            && let Some(base) = &self.current_url
        {
            let policies = page.document.borrow().content_security_policy().clone();
            let load = |url: &AppUrl| self.load_subresource(url);
            load_web_fonts(&load, &inputs.fonts, base, &stylesheet, &policies);
        }
        inputs.stylesheet = Rc::new(stylesheet);
        let text = inputs.sheets[index].1.clone();
        drop(inputs);
        if let Some(sheet) = self
            .inspection
            .sheets
            .entries
            .iter_mut()
            .find(|sheet| sheet.owner == owner)
        {
            let url = sheet.header["sourceURL"].as_str().unwrap_or_default();
            let listed = Listed {
                owner,
                inline: sheet.header["isInline"] == true,
                url: url.to_string(),
                text,
            };
            sheet.print = print;
            sheet.header = header(sheet.id, &listed);
        }
        Ok(())
    }

    fn matched_styles(&mut self, session: u64, params: &Json) -> Result<Json, Failure> {
        let node = self.css_node(session, params)?;
        let page = self.page.as_ref().ok_or_else(unavailable)?;
        let chain: Vec<NodeId> = {
            let document = page.document.borrow();
            std::iter::successors(Some(node), |node| document.parent(*node))
                .take_while(|node| document.element(*node).is_some())
                .collect()
        };
        if chain.is_empty() {
            return Err((-32000, "Node is not an element".into()));
        }
        let held = self.session(session).ok_or_else(unavailable)?;
        let ids: Vec<u64> = chain.iter().map(|node| held.dom.id(*node)).collect();
        let page = self.page.as_ref().ok_or_else(unavailable)?;
        let stylesheet = self.stylesheet(page);
        let state = page.runtime.current_style_state().unwrap_or_default();
        let sources = self.sources(page);
        let document = page.document.borrow();
        let rules = |node: NodeId, pseudo: Option<PseudoElement>| -> Vec<Json> {
            let matched = stylesheet.matched_rules(&document, node, pseudo, &state);
            matched
                .iter()
                .filter_map(|rule| rule_match(rule, &sources))
                .collect()
        };
        let pseudo_elements: Vec<Json> = PSEUDO_ELEMENTS
            .into_iter()
            .filter(|pseudo| stylesheet.styles_pseudo(*pseudo))
            .filter_map(|pseudo| {
                let matches = rules(node, Some(pseudo));
                let name = pseudo.tag().trim_start_matches("::");
                let name = name.trim_start_matches("-webkit-");
                (!matches.is_empty()).then(|| json!({ "pseudoType": name, "matches": matches }))
            })
            .collect();
        let inherited: Vec<Json> = chain
            .iter()
            .zip(&ids)
            .skip(1)
            .map(|(&ancestor, &id)| {
                let mut entry = json!({ "matchedCSSRules": rules(ancestor, None) });
                if let Some(text) = document.attribute(ancestor, "style") {
                    entry["inlineStyle"] = attribute_style(text, id);
                }
                entry
            })
            .collect();
        let text = document.attribute(node, "style").unwrap_or_default();
        Ok(json!({
            "inlineStyle": attribute_style(text, ids[0]),
            "matchedCSSRules": rules(node, None),
            "pseudoElements": pseudo_elements,
            "inherited": inherited,
            "inheritedPseudoElements": [],
            "cssKeyframesRules": [],
        }))
    }
}

fn header(id: u64, sheet: &Listed) -> Json {
    let lines = Lines::new(&sheet.text);
    let (end_line, end_column) = lines.position(&sheet.text, sheet.text.len());
    json!({
        "styleSheetId": sheet_id(id),
        "frameId": frame_id(),
        "sourceURL": sheet.url,
        "origin": "regular",
        "title": "",
        "disabled": false,
        "hasSourceURL": false,
        "isInline": sheet.inline,
        "isMutable": false,
        "isConstructed": false,
        "startLine": 0,
        "startColumn": 0,
        "length": sheet.text.encode_utf16().count(),
        "endLine": end_line,
        "endColumn": end_column,
    })
}

fn splice(text: &str, span: &Range<usize>, replacement: &str) -> String {
    [&text[..span.start], replacement, &text[span.end..]].concat()
}

// whether text spliced into a block leaves the sheet's braces, strings and comments closed
fn balanced(text: &str) -> bool {
    let bytes = text.as_bytes();
    let (mut depth, mut quote, mut index) = (0usize, None, 0);
    while index < bytes.len() {
        let byte = bytes[index];
        match quote {
            Some(open) if byte == open => quote = None,
            Some(_) if byte == b'\\' => index += 1,
            Some(_) => {}
            None if text[index..].starts_with("/*") => match text[index + 2..].find("*/") {
                Some(end) => index += end + 3,
                None => return false,
            },
            None => match byte {
                b'"' | b'\'' => quote = Some(byte),
                b'{' => depth += 1,
                b'}' if depth == 0 => return false,
                b'}' => depth -= 1,
                _ => {}
            },
        }
        index += 1;
    }
    depth == 0 && quote.is_none()
}

fn attribute_style(text: &str, node: u64) -> Json {
    let id = format!("style-{node}");
    style(text, 0..text.len(), Some((&id, &Lines::new(text))))
}

fn rule_match(rule: &MatchedRule, sources: &[Source]) -> Option<Json> {
    let json = rule_json(rule, sources)?;
    let count = json["selectorList"]["selectors"]
        .as_array()
        .map_or(0, Vec::len);
    let matching: Vec<_> = rule.matching.iter().filter(|&&at| at < count).collect();
    Some(json!({ "rule": json, "matchingSelectors": matching }))
}

fn rule_json(rule: &MatchedRule, sources: &[Source]) -> Option<Json> {
    if rule.user_agent {
        return Some(json!({
            "selectorList": selector_list(USER_AGENT_CSS, rule.header.clone(), None),
            "origin": "user-agent",
            "style": style(USER_AGENT_CSS, rule.block.clone(), None),
        }));
    }
    let (source, start) = sources.iter().find_map(|source| {
        let start = source.start?;
        let inside = start <= rule.header.start && rule.block.end <= start + source.text.len();
        inside.then_some((source, start))
    })?;
    let local = |span: &Range<usize>| span.start - start..span.end - start;
    let written = source.id.as_deref().map(|id| (id, &source.lines));
    let mut json = json!({
        "selectorList": selector_list(&source.text, local(&rule.header), written.map(|(_, lines)| lines)),
        "origin": "regular",
        "style": style(&source.text, local(&rule.block), written),
    });
    if let Some(id) = &source.id {
        json["styleSheetId"] = id.as_str().into();
    }
    Some(json)
}

fn selector_list(text: &str, header: Range<usize>, lines: Option<&Lines>) -> Json {
    let written = text[header.clone()].trim();
    let base = header.start + (written.as_ptr() as usize - text[header].as_ptr() as usize);
    let parts = css::split_selector_list(written).unwrap_or_else(|_| vec![written]);
    let selectors: Vec<Json> = parts
        .into_iter()
        .map(|part| {
            let mut selector = json!({ "text": part });
            if let Some(lines) = lines {
                let start = base + (part.as_ptr() as usize - written.as_ptr() as usize);
                selector["range"] = lines.range(text, start..start + part.len());
            }
            selector
        })
        .collect();
    json!({ "selectors": selectors, "text": written })
}

// a declaration block as the protocol shows it; one with an id and lines can be edited, with
// each declaration's place in the text and the ones a comment turned off
fn style(text: &str, block: Range<usize>, written: Option<(&str, &Lines)>) -> Json {
    let properties: Vec<Json> = declarations(text, block.clone())
        .into_iter()
        .filter(|declaration| written.is_some() || !declaration.disabled)
        .map(|declaration| {
            let mut property = json!({
                "name": declaration.name,
                "value": declaration.value,
                "important": declaration.important,
            });
            if let Some((_, lines)) = written {
                let parsed = declaration.disabled
                    || css::supports_declaration(declaration.name, declaration.value);
                property["text"] = declaration.text.into();
                property["disabled"] = declaration.disabled.into();
                property["implicit"] = false.into();
                property["parsedOk"] = parsed.into();
                property["range"] = lines.range(text, declaration.span);
            }
            property
        })
        .collect();
    let mut style = json!({ "cssProperties": properties, "shorthandEntries": [] });
    if let Some((id, lines)) = written {
        style["styleSheetId"] = id.into();
        style["cssText"] = text[block.clone()].into();
        style["range"] = lines.range(text, block);
    }
    style
}

// a declaration as written in a block, or one a comment turned off, with its place in the text
#[derive(Debug, PartialEq)]
struct Declaration<'a> {
    name: &'a str,
    value: &'a str,
    important: bool,
    disabled: bool,
    text: &'a str,
    span: Range<usize>,
}

fn declaration(text: &str) -> Option<(&str, &str, bool)> {
    let (name, value) = text.trim().trim_end_matches(';').split_once(':')?;
    let (name, value) = (name.trim(), value.trim());
    let named = !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'));
    if !named {
        return None;
    }
    Some(match value.rfind('!') {
        Some(at) if value[at + 1..].trim().eq_ignore_ascii_case("important") => {
            (name, value[..at].trim_end(), true)
        }
        _ => (name, value, false),
    })
}

// the declarations of a block in the order written, nested rules skipped; a comment of its own
// between declarations that reads as one is a declaration turned off
fn declarations(text: &str, block: Range<usize>) -> Vec<Declaration<'_>> {
    let body = &text[block.clone()];
    let bytes = body.as_bytes();
    let mut found = Vec::new();
    let mut push = |from: usize, to: usize, disabled: bool| {
        let segment = &body[from..to];
        let written = segment.trim();
        let start = block.start + from + (segment.len() - segment.trim_start().len());
        let inner = match disabled {
            true => written
                .strip_prefix("/*")
                .map(|inner| inner.strip_suffix("*/").unwrap_or(inner)),
            false => Some(written),
        };
        if let Some((name, value, important)) = inner.and_then(declaration) {
            found.push(Declaration {
                name,
                value,
                important,
                disabled,
                text: written,
                span: start..start + written.len(),
            });
        }
    };
    let (mut start, mut index, mut parens, mut braces, mut quote) = (0, 0, 0usize, 0usize, None);
    while index < bytes.len() {
        let byte = bytes[index];
        match quote {
            Some(open) if byte == open => quote = None,
            Some(_) if byte == b'\\' => index += 1,
            Some(_) => {}
            None if body[index..].starts_with("/*") => {
                let end = body[index + 2..]
                    .find("*/")
                    .map_or(body.len(), |at| index + at + 4);
                if parens == 0 && braces == 0 && body[start..index].trim().is_empty() {
                    push(index, end, true);
                    start = end;
                }
                index = end;
                continue;
            }
            None => match byte {
                b'"' | b'\'' => quote = Some(byte),
                b'(' => parens += 1,
                b')' => parens = parens.saturating_sub(1),
                b'{' => braces += 1,
                b'}' => {
                    braces = braces.saturating_sub(1);
                    if braces == 0 {
                        start = index + 1;
                    }
                }
                b';' if parens == 0 && braces == 0 => {
                    push(start, index + 1, false);
                    start = index + 1;
                }
                _ => {}
            },
        }
        index += 1;
    }
    if braces == 0 {
        push(start, body.len(), false);
    }
    found
}

// where each line of a text starts, for the protocol's lines and utf-16 columns
struct Lines(Vec<usize>);

impl Lines {
    fn new(text: &str) -> Self {
        let starts = text.match_indices('\n').map(|(at, _)| at + 1);
        Self(std::iter::once(0).chain(starts).collect())
    }

    fn position(&self, text: &str, offset: usize) -> (usize, usize) {
        let line = self.0.partition_point(|&start| start <= offset) - 1;
        (line, text[self.0[line]..offset].encode_utf16().count())
    }

    fn range(&self, text: &str, span: Range<usize>) -> Json {
        let (start_line, start_column) = self.position(text, span.start);
        let (end_line, end_column) = self.position(text, span.end);
        json!({
            "startLine": start_line,
            "startColumn": start_column,
            "endLine": end_line,
            "endColumn": end_column,
        })
    }

    // the byte offset of a line and utf-16 column, or none past its line or inside a character
    fn offset(&self, text: &str, line: u64, column: u64) -> Option<usize> {
        let start = *self.0.get(usize::try_from(line).ok()?)?;
        let row = &text[start..];
        let row = &row[..row.find('\n').unwrap_or(row.len())];
        let mut units = 0;
        for (at, character) in row.char_indices() {
            if units == column {
                return Some(start + at);
            }
            units += character.len_utf16() as u64;
        }
        (units == column).then_some(start + row.len())
    }

    fn span(&self, text: &str, range: &Json) -> Result<Range<usize>, Failure> {
        let at = |line: &str, column: &str| {
            self.offset(text, range[line].as_u64()?, range[column].as_u64()?)
        };
        match (at("startLine", "startColumn"), at("endLine", "endColumn")) {
            (Some(start), Some(end)) if start <= end => Ok(start..end),
            _ => Err((-32000, "Specified range is out of bounds".into())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declarations_are_found_with_their_places_and_comments_turn_them_off() {
        let text =
            "a { color: red ; /* margin: 0; */ b { x: y } width: calc(1px; 2px) !IMPORTANT }";
        let open = text.find('{').unwrap() + 1;
        let block = open..text.rfind('}').unwrap();
        let found = declarations(text, block);
        let summary: Vec<_> = found
            .iter()
            .map(|d| {
                (
                    d.name,
                    d.value,
                    d.important,
                    d.disabled,
                    &text[d.span.clone()],
                )
            })
            .collect();
        assert_eq!(
            summary,
            [
                ("color", "red", false, false, "color: red ;"),
                ("margin", "0", false, true, "/* margin: 0; */"),
                (
                    "width",
                    "calc(1px; 2px)",
                    true,
                    false,
                    "width: calc(1px; 2px) !IMPORTANT"
                ),
            ]
        );
        assert!(declarations("/* a note */ color: red", 0..23)[0].name == "color");
    }

    #[test]
    fn protocol_positions_count_utf16_columns_per_line() {
        let text = "a{}\n\u{1F600}b { c: d }";
        let lines = Lines::new(text);
        let at = text.find('b').unwrap();
        assert_eq!(lines.position(text, at), (1, 2));
        assert_eq!(lines.offset(text, 1, 2), Some(at));
        assert_eq!(lines.offset(text, 1, 1), None);
        assert_eq!(lines.offset(text, 0, 3), Some(3));
        assert_eq!(lines.offset(text, 0, 4), None);
        let range = json!({ "startLine": 1, "startColumn": 2, "endLine": 1, "endColumn": 3 });
        assert_eq!(lines.span(text, &range), Ok(at..at + 1));
        assert!(balanced("color: red; content: \"}\" /* { */"));
        assert!(!balanced("color: red } a {"));
        assert!(!balanced("color: red /*"));
    }
}
