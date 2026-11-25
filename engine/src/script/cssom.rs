// the CSS object model pages and CSS-in-JS libraries write through: a style element's sheet,
// document.styleSheets, CSSStyleSheet with insertRule, deleteRule and replace, the rules a
// sheet lists, and constructed sheets adopted by a document or a shadow root

use super::object::{JsObject, ObjectClass, ObjectHandle, Property, Slot, new_object};
use super::operations::{is_primitive, plain_object};
use super::realm::{Builder, Native};
use super::*;
use std::cell::Cell;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum CssomMember {
    SheetConstructor,
    RuleConstructor,
    CssRules,
    InsertRule,
    DeleteRule,
    AddRule,
    RemoveRule,
    Replace,
    ReplaceSync,
    OwnerNode,
    Disabled,
    SetDisabled,
    Media,
    Href,
    Title,
    Type,
    Nothing,
    RuleText,
    RuleType,
    RuleSelector,
    RuleStyle,
    RuleSheet,
}

pub(super) struct SheetState {
    // the style element the sheet belongs to; a constructed sheet has none
    owner: Option<NodeId>,
    // a constructed sheet's rules, and its own disabled flag
    rules: RefCell<Vec<String>>,
    disabled: Cell<bool>,
    media: RefCell<String>,
}

pub(super) struct RuleState {
    text: String,
    sheet: Value,
}

impl RuleState {
    pub(super) fn trace(&self, tracer: &mut super::heap::Tracer) {
        tracer.trace_value(&self.sheet);
    }
}

pub(super) struct CssomInterfaces {
    sheet: ObjectHandle,
    rules: Vec<(u16, ObjectHandle)>,
    rule: ObjectHandle,
}

// the type numbers of CSSRule, and the interface each rule kind answers to
const RULE_KINDS: &[(&str, u16, &str)] = &[
    ("", 1, "CSSStyleRule"),
    ("@charset", 2, "CSSCharsetRule"),
    ("@import", 3, "CSSImportRule"),
    ("@media", 4, "CSSMediaRule"),
    ("@font-face", 5, "CSSFontFaceRule"),
    ("@page", 6, "CSSPageRule"),
    ("@keyframes", 7, "CSSKeyframesRule"),
    ("@-webkit-keyframes", 7, "CSSKeyframesRule"),
    ("@namespace", 10, "CSSNamespaceRule"),
    ("@supports", 12, "CSSSupportsRule"),
    ("@layer", 0, "CSSLayerBlockRule"),
    ("@container", 0, "CSSContainerRule"),
];

fn rule_kind(text: &str) -> (u16, &'static str) {
    if !text.starts_with('@') {
        return (1, "CSSStyleRule");
    }
    let name = text
        .split(|character: char| character.is_whitespace() || matches!(character, '{' | ';' | '('))
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    RULE_KINDS
        .iter()
        .find(|(at, _, _)| *at == name)
        .map_or((0, "CSSRule"), |&(_, kind, interface)| (kind, interface))
}

// the top-level rules of a sheet's text: comments dropped, each rule through its closing
// brace, or through its semicolon for a statement such as @import
pub(super) fn split_rules(text: &str) -> Vec<String> {
    let mut rules = Vec::new();
    let mut current = String::new();
    let mut depth = 0_i32;
    let mut chars = text.chars().peekable();
    while let Some(character) = chars.next() {
        match character {
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut previous = ' ';
                for next in chars.by_ref() {
                    if previous == '*' && next == '/' {
                        break;
                    }
                    previous = next;
                }
                current.push(' ');
            }
            '"' | '\'' => {
                current.push(character);
                while let Some(next) = chars.next() {
                    current.push(next);
                    if next == '\\' {
                        if let Some(escaped) = chars.next() {
                            current.push(escaped);
                        }
                    } else if next == character {
                        break;
                    }
                }
            }
            '{' => {
                depth += 1;
                current.push(character);
            }
            '}' => {
                depth -= 1;
                current.push(character);
                if depth <= 0 {
                    depth = 0;
                    let rule = current.trim().to_string();
                    if !rule.is_empty() && rule != "}" {
                        rules.push(rule);
                    }
                    current.clear();
                }
            }
            ';' if depth == 0 => {
                let rule = current.trim().to_string();
                if rule.starts_with('@') {
                    rules.push(format!("{rule};"));
                }
                current.clear();
            }
            _ => current.push(character),
        }
    }
    let rest = current.trim();
    if rest.starts_with('@') && !rest.contains('{') {
        rules.push(format!("{rest};"));
    }
    rules
}

// a rule as browsers write it back: a style rule as `selector { property: value; }`
pub(super) fn serialize_rule(text: &str) -> String {
    let text = text.trim();
    if text.starts_with('@') {
        return text.to_string();
    }
    let Some((selector, rest)) = text.split_once('{') else {
        return text.to_string();
    };
    let body = rest.trim_end().strip_suffix('}').unwrap_or(rest);
    let selector = selector.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut declarations = Vec::new();
    let mut current = String::new();
    let mut nesting = 0;
    let mut quote = None;
    for character in body.chars() {
        match (character, quote) {
            (open @ ('"' | '\''), None) => {
                quote = Some(open);
                current.push(character);
            }
            (close, Some(open)) if close == open => {
                quote = None;
                current.push(character);
            }
            ('(', None) => {
                nesting += 1;
                current.push(character);
            }
            (')', None) => {
                nesting -= 1;
                current.push(character);
            }
            (';', None) if nesting == 0 => {
                declarations.push(std::mem::take(&mut current));
            }
            _ => current.push(character),
        }
    }
    declarations.push(current);
    let declarations: Vec<String> = declarations
        .iter()
        .filter_map(|declaration| {
            let (property, value) = declaration.split_once(':')?;
            let property = property.trim();
            let value = value.split_whitespace().collect::<Vec<_>>().join(" ");
            (!property.is_empty()).then(|| format!("{property}: {value};"))
        })
        .collect();
    if declarations.is_empty() {
        format!("{selector} {{ }}")
    } else {
        format!("{selector} {{ {} }}", declarations.join(" "))
    }
}

fn native(member: CssomMember) -> Native {
    Native::Cssom(member)
}

pub(super) fn create(
    build: &Builder,
    object_prototype: &ObjectHandle,
    globals: &mut Vec<(&'static str, Value)>,
) -> CssomInterfaces {
    use CssomMember as M;
    let plain = |prototype: &ObjectHandle| {
        new_object(JsObject::with_prototype(
            Some(prototype.clone()),
            ObjectClass::Ordinary,
        ))
    };
    let accessor = |target: &ObjectHandle, name: &str, get: M, set: Option<M>| {
        let get = build.function(native(get), &format!("get {name}"), 0);
        let set =
            set.map(|set| Value::Object(build.function(native(set), &format!("set {name}"), 1)));
        target.borrow_mut().define(
            name.to_string(),
            Property {
                slot: Slot::accessor(Some(Value::Object(get)), set),
                enumerable: true,
                writable: false,
                configurable: true,
            },
        );
    };
    let method = |target: &ObjectHandle, name: &str, member: M, length: u32| {
        let function = build.function(native(member), name, length);
        target.borrow_mut().define(
            name.to_string(),
            Property {
                enumerable: true,
                ..Property::data(Value::Object(function))
            },
        );
    };
    let tag = |target: &ObjectHandle, name: &str| {
        target.borrow_mut().define(
            "@@toStringTag".to_string(),
            Property {
                writable: false,
                ..Property::hidden(Value::String(name.into()))
            },
        );
    };
    let style_sheet = plain(object_prototype);
    for (name, member, setter) in [
        ("ownerNode", M::OwnerNode, None),
        ("disabled", M::Disabled, Some(M::SetDisabled)),
        ("media", M::Media, None),
        ("href", M::Href, None),
        ("title", M::Title, None),
        ("type", M::Type, None),
        ("parentStyleSheet", M::Nothing, None),
    ] {
        accessor(&style_sheet, name, member, setter);
    }
    tag(&style_sheet, "StyleSheet");
    let illegal = build.constructor(native(M::RuleConstructor), "StyleSheet", 0, &style_sheet);
    globals.push(("StyleSheet", Value::Object(illegal)));
    let sheet = plain(&style_sheet);
    accessor(&sheet, "cssRules", M::CssRules, None);
    accessor(&sheet, "rules", M::CssRules, None);
    accessor(&sheet, "ownerRule", M::Nothing, None);
    for (name, member, length) in [
        ("insertRule", M::InsertRule, 1),
        ("deleteRule", M::DeleteRule, 1),
        ("addRule", M::AddRule, 0),
        ("removeRule", M::RemoveRule, 0),
        ("replace", M::Replace, 1),
        ("replaceSync", M::ReplaceSync, 1),
    ] {
        method(&sheet, name, member, length);
    }
    tag(&sheet, "CSSStyleSheet");
    let constructor = build.constructor(native(M::SheetConstructor), "CSSStyleSheet", 0, &sheet);
    globals.push(("CSSStyleSheet", Value::Object(constructor)));
    let rule = plain(object_prototype);
    accessor(&rule, "cssText", M::RuleText, None);
    accessor(&rule, "type", M::RuleType, None);
    accessor(&rule, "parentStyleSheet", M::RuleSheet, None);
    accessor(&rule, "parentRule", M::Nothing, None);
    tag(&rule, "CSSRule");
    let rule_constructor = build.constructor(native(M::RuleConstructor), "CSSRule", 0, &rule);
    for (name, value) in [
        ("STYLE_RULE", 1.0),
        ("CHARSET_RULE", 2.0),
        ("IMPORT_RULE", 3.0),
        ("MEDIA_RULE", 4.0),
        ("FONT_FACE_RULE", 5.0),
        ("PAGE_RULE", 6.0),
        ("KEYFRAMES_RULE", 7.0),
        ("KEYFRAME_RULE", 8.0),
        ("NAMESPACE_RULE", 10.0),
        ("SUPPORTS_RULE", 12.0),
    ] {
        for target in [&rule_constructor, &rule] {
            target.borrow_mut().define(
                name.to_string(),
                Property {
                    slot: Slot::Data(Value::Number(value)),
                    enumerable: true,
                    writable: false,
                    configurable: false,
                },
            );
        }
    }
    globals.push(("CSSRule", Value::Object(rule_constructor)));
    let mut rules = Vec::new();
    let mut made: Vec<&str> = Vec::new();
    for &(_, kind, interface) in RULE_KINDS {
        if made.contains(&interface) {
            continue;
        }
        made.push(interface);
        let prototype = plain(&rule);
        if interface == "CSSStyleRule" {
            accessor(&prototype, "selectorText", M::RuleSelector, None);
            accessor(&prototype, "style", M::RuleStyle, None);
        }
        tag(&prototype, interface);
        let constructor = build.constructor(native(M::RuleConstructor), interface, 0, &prototype);
        globals.push((interface, Value::Object(constructor)));
        if kind != 0 || interface != "CSSRule" {
            rules.push((kind, prototype));
        }
    }
    CssomInterfaces { sheet, rules, rule }
}

impl ScriptRuntime {
    fn sheet_state(value: &Value) -> Option<Rc<SheetState>> {
        match value {
            Value::Object(object) => match &object.borrow().class {
                ObjectClass::Sheet(state) => Some(state.clone()),
                _ => None,
            },
            _ => None,
        }
    }

    fn new_sheet(&self, owner: Option<NodeId>, prototype: ObjectHandle) -> ObjectHandle {
        new_object(JsObject::with_prototype(
            Some(prototype),
            ObjectClass::Sheet(Rc::new(SheetState {
                owner,
                rules: RefCell::new(Vec::new()),
                disabled: Cell::new(false),
                media: RefCell::new(String::new()),
            })),
        ))
    }

    // the sheet of a style element in the document, the same object each time
    pub(super) fn style_sheet_of(&mut self, node: NodeId) -> Value {
        let connected = {
            let document = self.document.borrow();
            document
                .element(node)
                .is_some_and(|element| element.name == "style")
                && matches!(
                    document.kind(document.tree_root(node)),
                    Some(crate::dom::NodeKind::Document | crate::dom::NodeKind::Fragment)
                )
                && (document.tree_root(node) == document.root()
                    || document.shadow_host(document.tree_root(node)).is_some())
        };
        if !connected {
            return Value::Null;
        }
        if let Some(sheet) = self.sheet_objects.borrow().get(&node) {
            return Value::Object(sheet.clone());
        }
        let prototype = self.intrinsics.cssom.sheet.clone();
        let sheet = self.new_sheet(Some(node), prototype);
        self.sheet_objects.borrow_mut().insert(node, sheet.clone());
        Value::Object(sheet)
    }

    // the sheets of the style elements in a document or shadow root, in tree order
    pub(super) fn style_sheets_of(&mut self, root: NodeId) -> Value {
        let nodes: Vec<NodeId> = {
            let document = self.document.borrow();
            document
                .descendants(root)
                .filter(|&node| {
                    document
                        .element(node)
                        .is_some_and(|element| element.name == "style")
                })
                .collect()
        };
        let sheets = nodes
            .into_iter()
            .map(|node| self.style_sheet_of(node))
            .filter(|sheet| !matches!(sheet, Value::Null))
            .collect();
        Self::new_array(sheets)
    }

    fn sheet_rule_list(&self, state: &SheetState) -> Vec<String> {
        match state.owner {
            Some(node) => {
                let document = self.document.borrow();
                match document.cssom_rules(node) {
                    Some(rules) => rules.clone(),
                    None => split_rules(&document.text_content(node)),
                }
            }
            None => state.rules.borrow().clone(),
        }
    }

    fn store_rules(&mut self, state: &SheetState, rules: Vec<String>) {
        match state.owner {
            Some(node) => self.document.borrow_mut().set_cssom_rules(node, rules),
            None => {
                *state.rules.borrow_mut() = rules;
                let roots: Vec<NodeId> = self.adopted_sheets.borrow().keys().copied().collect();
                for root in roots {
                    self.refresh_adopted(root);
                }
            }
        }
    }

    // writes the text of the sheets a root adopted for the cascade to read
    fn refresh_adopted(&mut self, root: NodeId) {
        let texts: Vec<String> = self
            .adopted_sheets
            .borrow()
            .get(&root)
            .into_iter()
            .flatten()
            .filter_map(|sheet| Self::sheet_state(&Value::Object(sheet.clone())))
            .filter(|state| !state.disabled.get())
            .map(|state| state.rules.borrow().join("\n"))
            .collect();
        self.document.borrow_mut().set_adopted_sheets(root, texts);
    }

    pub(super) fn adopted_sheets_value(&self, root: NodeId) -> Value {
        let sheets = self
            .adopted_sheets
            .borrow()
            .get(&root)
            .into_iter()
            .flatten()
            .map(|sheet| Value::Object(sheet.clone()))
            .collect();
        Self::new_array(sheets)
    }

    pub(super) fn set_adopted_sheets(
        &mut self,
        root: NodeId,
        value: Value,
    ) -> Result<(), ScriptError> {
        let mut sheets = Vec::new();
        for sheet in self.iterate_to_vec(value)? {
            let constructed = Self::sheet_state(&sheet).is_some_and(|state| state.owner.is_none());
            let Value::Object(sheet) = sheet else {
                return Err(self.type_error("Failed to set the 'adoptedStyleSheets' property: The provided value is not of type 'CSSStyleSheet'."));
            };
            if !constructed {
                return Err(self.throw_value(dom_exception(
                    "NotAllowedError",
                    "Failed to set the 'adoptedStyleSheets' property: Can't adopt non-constructed stylesheets.",
                )));
            }
            sheets.push(sheet);
        }
        self.adopted_sheets.borrow_mut().insert(root, sheets);
        self.refresh_adopted(root);
        Ok(())
    }

    fn rule_value(&self, text: &str, sheet: &Value) -> Value {
        let (kind, _) = rule_kind(text);
        let prototype = self
            .intrinsics
            .cssom
            .rules
            .iter()
            .find(|(each, _)| *each == kind)
            .map_or_else(
                || self.intrinsics.cssom.rule.clone(),
                |(_, prototype)| prototype.clone(),
            );
        Value::Object(new_object(JsObject::with_prototype(
            Some(prototype),
            ObjectClass::CssRule(Rc::new(RuleState {
                text: serialize_rule(text),
                sheet: sheet.clone(),
            })),
        )))
    }

    fn insert_rule(
        &mut self,
        state: &SheetState,
        rule: &str,
        index: usize,
    ) -> Result<usize, ScriptError> {
        let parsed = split_rules(rule);
        let [single] = parsed.as_slice() else {
            return Err(self.throw_value(dom_exception(
                "SyntaxError",
                &format!("Failed to execute 'insertRule' on 'CSSStyleSheet': Failed to parse the rule '{rule}'."),
            )));
        };
        if single.starts_with("@import") && state.owner.is_none() {
            return Err(self.throw_value(dom_exception(
                "SyntaxError",
                "Failed to execute 'insertRule' on 'CSSStyleSheet': Can't insert @import rules into a constructed stylesheet.",
            )));
        }
        let mut rules = self.sheet_rule_list(state);
        if index > rules.len() {
            return Err(self.throw_value(dom_exception(
                "IndexSizeError",
                &format!(
                    "Failed to execute 'insertRule' on 'CSSStyleSheet': The index provided ({index}) is larger than the maximum index ({}).",
                    rules.len()
                ),
            )));
        }
        rules.insert(index, single.clone());
        self.store_rules(state, rules);
        Ok(index)
    }

    pub(super) fn construct_sheet(
        &mut self,
        arguments: Vec<Value>,
        new_target: Value,
    ) -> Result<Value, ScriptError> {
        let prototype = match &new_target {
            Value::Object(_) => self.prototype_from_constructor(&new_target)?,
            _ => self.intrinsics.cssom.sheet.clone(),
        };
        let sheet = self.new_sheet(None, prototype);
        if let (Some(options), Some(state)) = (
            arguments.first().filter(|options| !is_primitive(options)),
            Self::sheet_state(&Value::Object(sheet.clone())),
        ) {
            state
                .disabled
                .set(self.get_property(options, "disabled")?.truthy());
            let media = self.get_property(options, "media")?;
            if !matches!(media, Value::Undefined) {
                *state.media.borrow_mut() = self.coerce_string(media)?;
            }
        }
        Ok(Value::Object(sheet))
    }

    pub(super) fn call_cssom(
        &mut self,
        member: CssomMember,
        this: Value,
        arguments: Vec<Value>,
    ) -> Result<Value, ScriptError> {
        use CssomMember as M;
        let first = arguments.first().cloned().unwrap_or(Value::Undefined);
        if matches!(member, M::SheetConstructor | M::RuleConstructor) {
            return Err(self.type_error("Illegal constructor"));
        }
        if matches!(
            member,
            M::RuleText | M::RuleType | M::RuleSelector | M::RuleStyle | M::RuleSheet
        ) {
            let rule = match &this {
                Value::Object(object) => match &object.borrow().class {
                    ObjectClass::CssRule(rule) => Some(rule.clone()),
                    _ => None,
                },
                _ => None,
            };
            let Some(rule) = rule else {
                return Err(self.type_error("Illegal invocation"));
            };
            return Ok(match member {
                M::RuleText => Value::String(rule.text.clone().into()),
                M::RuleType => Value::Number(f64::from(rule_kind(&rule.text).0)),
                M::RuleSheet => rule.sheet.clone(),
                M::RuleSelector => Value::String(
                    rule.text
                        .split_once('{')
                        .map_or("", |(selector, _)| selector)
                        .trim()
                        .to_string()
                        .into(),
                ),
                _ => {
                    let body = rule
                        .text
                        .split_once('{')
                        .map_or("", |(_, rest)| rest)
                        .trim()
                        .trim_end_matches('}')
                        .trim()
                        .to_string();
                    plain_object([("cssText".to_string(), Value::String(body.into()))])
                }
            });
        }
        if member == M::Nothing {
            return Ok(Value::Null);
        }
        let Some(state) = Self::sheet_state(&this) else {
            return Err(self.type_error("Illegal invocation"));
        };
        match member {
            M::CssRules => {
                let rules = self.sheet_rule_list(&state);
                let values = rules
                    .iter()
                    .map(|rule| self.rule_value(rule, &this))
                    .collect();
                Ok(Self::new_array(values))
            }
            M::InsertRule => {
                let rule = self.coerce_string(first)?;
                let index = match arguments.get(1) {
                    None | Some(Value::Undefined) => 0,
                    Some(index) => self.coerce_number(index.clone())?.max(0.0) as usize,
                };
                Ok(Value::Number(self.insert_rule(&state, &rule, index)? as f64))
            }
            M::AddRule => {
                let selector = self.coerce_string(first)?;
                let block = match arguments.get(1) {
                    None | Some(Value::Undefined) => String::new(),
                    Some(block) => self.coerce_string(block.clone())?,
                };
                let length = self.sheet_rule_list(&state).len();
                let index = match arguments.get(2) {
                    None | Some(Value::Undefined) => length,
                    Some(index) => self.coerce_number(index.clone())?.max(0.0) as usize,
                };
                self.insert_rule(&state, &format!("{selector} {{ {block} }}"), index)?;
                Ok(Value::Number(-1.0))
            }
            M::DeleteRule | M::RemoveRule => {
                let index = match first {
                    Value::Undefined if member == M::RemoveRule => 0,
                    value => self.coerce_number(value)?.max(0.0) as usize,
                };
                let mut rules = self.sheet_rule_list(&state);
                if index >= rules.len() {
                    return Err(self.throw_value(dom_exception(
                        "IndexSizeError",
                        &format!("Failed to execute 'deleteRule' on 'CSSStyleSheet': The index provided ({index}) is larger than the maximum index ({}).", rules.len().saturating_sub(1)),
                    )));
                }
                rules.remove(index);
                self.store_rules(&state, rules);
                Ok(Value::Undefined)
            }
            M::Replace | M::ReplaceSync => {
                if state.owner.is_some() {
                    let error = dom_exception(
                        "NotAllowedError",
                        "Failed to execute 'replace' on 'CSSStyleSheet': Can't call replace on non-constructed CSSStyleSheets.",
                    );
                    if member == M::Replace {
                        return Ok(Value::Promise(self.rejected_promise_value(error)));
                    }
                    return Err(self.throw_value(error));
                }
                let text = self.coerce_string(first)?;
                // a constructed sheet drops the @import rules it is given
                let rules = split_rules(&text)
                    .into_iter()
                    .filter(|rule| !rule.starts_with("@import"))
                    .collect();
                self.store_rules(&state, rules);
                if member == M::Replace {
                    return Ok(Value::Promise(self.fulfilled_promise(this)));
                }
                Ok(Value::Undefined)
            }
            M::OwnerNode => Ok(state
                .owner
                .map_or(Value::Null, |node| self.node_value(node))),
            M::Disabled => Ok(Value::Bool(match state.owner {
                Some(node) => self.document.borrow().sheet_disabled(node),
                None => state.disabled.get(),
            })),
            M::SetDisabled => {
                let disabled = first.truthy();
                match state.owner {
                    Some(node) => self
                        .document
                        .borrow_mut()
                        .set_sheet_disabled(node, disabled),
                    None => {
                        state.disabled.set(disabled);
                        let roots: Vec<NodeId> =
                            self.adopted_sheets.borrow().keys().copied().collect();
                        for root in roots {
                            self.refresh_adopted(root);
                        }
                    }
                }
                Ok(Value::Undefined)
            }
            M::Media => {
                let text = match state.owner {
                    Some(node) => self
                        .document
                        .borrow()
                        .attribute(node, "media")
                        .unwrap_or_default()
                        .to_string(),
                    None => state.media.borrow().clone(),
                };
                Ok(plain_object([
                    ("mediaText".to_string(), Value::String(text.clone().into())),
                    (
                        "length".to_string(),
                        Value::Number(if text.is_empty() { 0.0 } else { 1.0 }),
                    ),
                ]))
            }
            M::Href => Ok(Value::Null),
            M::Title => Ok(state
                .owner
                .and_then(|node| {
                    self.document
                        .borrow()
                        .attribute(node, "title")
                        .map(str::to_string)
                })
                .map_or(Value::Null, |title| Value::String(title.into()))),
            M::Type => Ok(Value::String("text/css".into())),
            _ => Err(self.type_error("Illegal invocation")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules_split_and_serialize_as_browsers_write_them() {
        assert_eq!(
            split_rules(
                "/* a */ .x{color:red} @import url(a.css); @media (min-width: 1px) { .y { top: 0 } }"
            ),
            [
                ".x{color:red}",
                "@import url(a.css);",
                "@media (min-width: 1px) { .y { top: 0 } }"
            ]
        );
        assert_eq!(
            serialize_rule(".x{color:red;margin : 0  auto}"),
            ".x { color: red; margin: 0 auto; }"
        );
        assert_eq!(serialize_rule("a   b{}"), "a b { }");
        assert_eq!(
            serialize_rule("@media print{.a{top:0}}"),
            "@media print{.a{top:0}}"
        );
    }

    #[test]
    fn sheets_take_rules_from_script_and_style_the_page() {
        let document = crate::dom::Document::parse(
            r#"<html><head><style id="s">.a { color: red }</style></head><body><p class="a b" id="p">x</p><p id="out"></p></body></html>"#,
        )
        .expect("document should parse");
        let document = Rc::new(RefCell::new(document));
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(
                r#"
                const seen = [];
                const style = document.getElementById("s");
                const sheet = style.sheet;
                seen.push(sheet === style.sheet, sheet instanceof CSSStyleSheet, sheet.ownerNode === style, document.styleSheets.length);
                seen.push(sheet.cssRules.length, sheet.cssRules[0].cssText, sheet.cssRules[0].selectorText, sheet.cssRules[0] instanceof CSSStyleRule);
                seen.push(sheet.insertRule(".b{font-weight:bold}", 1), sheet.cssRules.length, sheet.cssRules[1].type);
                sheet.deleteRule(0);
                seen.push(sheet.cssRules[0].cssText);
                try { sheet.insertRule("not a rule", 0); } catch (error) { seen.push(error.name); }
                try { sheet.insertRule(".c{}", 9); } catch (error) { seen.push(error.name); }
                const made = new CSSStyleSheet();
                made.replaceSync(".a { color: blue } @import url(x.css);");
                document.adoptedStyleSheets = [made];
                seen.push(made.cssRules.length, document.adoptedStyleSheets[0] === made, made.ownerNode);
                try { document.adoptedStyleSheets = [sheet]; } catch (error) { seen.push(error.name); }
                const fresh = document.createElement("style");
                seen.push(fresh.sheet);
                document.getElementById("out").textContent = seen.join("|");
                "#,
            )
            .expect("script should run");
        let out = document.borrow().find_element_by_id("out").unwrap();
        assert_eq!(
            document.borrow().element_text_content(out).as_deref(),
            Some(
                "true|true|true|1|1|.a { color: red; }|.a|true|1|2|1|.b { font-weight: bold; }|SyntaxError|IndexSizeError|1|true||NotAllowedError|"
            )
        );
        let sheets = document.borrow().stylesheet_sources();
        assert!(
            matches!(&sheets[0], crate::dom::StylesheetSource::Cssom(text) if text == ".b{font-weight:bold}")
        );
        assert!(
            matches!(&sheets[1], crate::dom::StylesheetSource::Cssom(text) if text == ".a { color: blue }")
        );
    }
}
