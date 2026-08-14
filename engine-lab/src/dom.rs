use std::collections::BTreeMap;
use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Document {
    pub root: Element,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccessibilityNode {
    pub role: String,
    pub name: String,
    pub value: Option<String>,
    pub path: NodePath,
    pub disabled: bool,
    pub focusable: bool,
    pub children: Vec<Self>,
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
pub enum ScriptSource {
    Inline(String),
    External(String),
    InlineModule(String),
    ExternalModule(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StylesheetSource {
    Inline(String),
    External(String),
}

pub type NodePath = Vec<usize>;

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

    pub fn script_text(&self) -> Vec<String> {
        let mut scripts = Vec::new();
        collect_script_text(&self.root, &mut scripts);
        scripts
    }

    pub fn script_sources(&self) -> Vec<ScriptSource> {
        let mut scripts = Vec::new();
        collect_script_sources(&self.root, &mut scripts);
        scripts
    }

    pub fn stylesheet_sources(&self) -> Vec<StylesheetSource> {
        let mut stylesheets = Vec::new();
        collect_stylesheet_sources(&self.root, &mut stylesheets);
        stylesheets
    }

    pub fn accessibility_tree(&self) -> AccessibilityNode {
        let children = self
            .root
            .children
            .iter()
            .enumerate()
            .filter_map(|(index, node)| match node {
                Node::Element(element) => accessibility_node(element, vec![index]),
                Node::Text(_) => None,
            })
            .collect();
        AccessibilityNode {
            role: "document".to_string(),
            name: String::new(),
            value: None,
            path: Vec::new(),
            disabled: false,
            focusable: false,
            children,
        }
    }

    pub fn find_element_by_id(&self, id: &str) -> Option<NodePath> {
        find_element_by_id(&self.root, id, &mut Vec::new())
    }

    pub fn autofocus_paths(&self) -> Vec<NodePath> {
        let mut paths = Vec::new();
        let mut path = Vec::new();
        collect_autofocus_paths(&self.root, &mut path, &mut paths);
        paths
    }

    pub fn document_element_path(&self) -> Option<NodePath> {
        self.root
            .children
            .iter()
            .enumerate()
            .find_map(|(index, node)| matches!(node, Node::Element(_)).then_some(vec![index]))
    }

    pub fn form_control_paths(&self, form_path: &[usize]) -> Option<Vec<NodePath>> {
        let form = self.element_at(form_path)?;
        (form.name == "form").then(|| {
            let mut paths = Vec::new();
            collect_form_control_paths(&self.root, &mut Vec::new(), &mut paths);
            paths
                .into_iter()
                .filter(|path| self.form_owner_path(path).as_deref() == Some(form_path))
                .collect()
        })
    }

    pub fn form_owner_path(&self, path: &[usize]) -> Option<NodePath> {
        let element = self.element_at(path)?;
        if !matches!(element.name.as_str(), "button" | "input" | "select" | "textarea") {
            return None;
        }
        if let Some(form_id) = element.attr("form") {
            let candidate = self.find_element_by_id(form_id)?;
            return self
                .element_at(&candidate)
                .is_some_and(|element| element.name == "form")
                .then_some(candidate);
        }
        self.form_ancestor_path(path)
    }

    pub fn form_ancestor_path(&self, path: &[usize]) -> Option<NodePath> {
        let mut candidate = path.to_vec();
        while !candidate.is_empty() {
            candidate.pop();
            if self.element_at(&candidate).is_some_and(|element| element.name == "form") {
                return Some(candidate);
            }
        }
        None
    }

    pub fn option_paths(&self, select_path: &[usize]) -> Option<Vec<NodePath>> {
        let select = self.element_at(select_path)?;
        (select.name == "select").then(|| {
            let mut paths = Vec::new();
            let mut path = select_path.to_vec();
            collect_option_paths(select, &mut path, &mut paths);
            paths
        })
    }

    pub fn parent_path(&self, path: &[usize]) -> Option<NodePath> {
        if path.is_empty() {
            return None;
        }
        let parent = path[..path.len() - 1].to_vec();
        self.element_at(path).is_some().then_some(parent)
    }

    pub fn select_ancestor_path(&self, path: &[usize]) -> Option<NodePath> {
        let mut candidate = path.to_vec();
        while !candidate.is_empty() {
            candidate.pop();
            if self
                .element_at(&candidate)
                .is_some_and(|element| element.name == "select")
            {
                return Some(candidate);
            }
        }
        None
    }

    pub fn label_ancestor_path(&self, path: &[usize]) -> Option<NodePath> {
        let mut candidate = path.to_vec();
        while !candidate.is_empty() {
            candidate.pop();
            if self
                .element_at(&candidate)
                .is_some_and(|element| element.name == "label")
            {
                return Some(candidate);
            }
        }
        None
    }

    pub fn label_control_path(&self, label_path: &[usize]) -> Option<NodePath> {
        let label = self.element_at(label_path)?;
        if label.name != "label" {
            return None;
        }
        if let Some(id) = label.attr("for") {
            let candidate = self.find_element_by_id(id)?;
            return self
                .element_at(&candidate)
                .filter(|element| is_labelable(element))
                .map(|_| candidate);
        }
        let mut path = label_path.to_vec();
        find_first_labelable_descendant(label, &mut path)
    }

    pub fn radio_group_paths(&self, path: &[usize]) -> Option<Vec<NodePath>> {
        let radio = self.element_at(path)?;
        if radio.name != "input"
            || !radio
                .attr("type")
                .is_some_and(|kind| kind.eq_ignore_ascii_case("radio"))
        {
            return None;
        }
        let name = radio.attr("name")?;
        let form = self.form_owner_path(path)?;
        Some(
            self.form_control_paths(&form)?
                .into_iter()
                .filter(|candidate| {
                    self.element_at(candidate).is_some_and(|element| {
                        element.name == "input"
                            && element
                                .attr("type")
                                .is_some_and(|kind| kind.eq_ignore_ascii_case("radio"))
                            && element.attr("name") == Some(name)
                    })
                })
                .collect(),
        )
    }

    pub fn link_reference(&self, path: &[usize]) -> Option<String> {
        let mut candidate = path.to_vec();
        loop {
            let element = self.element_at(&candidate)?;
            if element.name == "a"
                && let Some(href) = element.attr("href")
            {
                return Some(href.to_string());
            }
            candidate.pop()?;
        }
    }

    pub fn find_element_by_selector(&self, selector: &str) -> Option<NodePath> {
        self.find_element_by_selector_within(&[], selector)
    }

    pub fn find_elements_by_selector(&self, selector: &str) -> Option<Vec<NodePath>> {
        self.find_elements_by_selector_within(&[], selector)
    }

    pub fn find_element_by_selector_within(
        &self,
        parent: &[usize],
        selector: &str,
    ) -> Option<NodePath> {
        let selectors = crate::css::Selector::parse_list(selector).ok()?;
        let parent_element = self.element_at(parent)?;
        let mut ancestors = self.ancestors_for_path(parent)?;
        let mut path = parent.to_vec();
        find_element_by_selector(parent_element, &selectors, &mut ancestors, &mut path)
    }

    pub fn find_elements_by_selector_within(
        &self,
        parent: &[usize],
        selector: &str,
    ) -> Option<Vec<NodePath>> {
        let selectors = crate::css::Selector::parse_list(selector).ok()?;
        let parent_element = self.element_at(parent)?;
        let mut ancestors = self.ancestors_for_path(parent)?;
        let mut path = parent.to_vec();
        let mut matches = Vec::new();
        collect_elements_by_selector(
            parent_element,
            &selectors,
            &mut ancestors,
            &mut path,
            &mut matches,
        );
        Some(matches)
    }

    pub fn element_text_content(&self, path: &[usize]) -> Option<String> {
        let element = self.element_at(path)?;
        let mut text = String::new();
        collect_text_nodes(element, &mut text);
        Some(text)
    }

    pub fn element_inner_html(&self, path: &[usize]) -> Option<String> {
        self.element_at(path).map(Element::inner_html)
    }

    pub fn set_element_inner_html(
        &mut self,
        path: &[usize],
        source: &str,
    ) -> Result<bool, ParseError> {
        let fragment = Self::parse(source)?;
        let Some(element) = self.element_at_mut(path) else {
            return Ok(false);
        };
        element.children = fragment.root.children;
        Ok(true)
    }

    pub fn set_element_text_content(&mut self, path: &[usize], text: String) -> bool {
        let Some(element) = self.element_at_mut(path) else {
            return false;
        };
        element.children = vec![Node::Text(text)];
        true
    }

    pub fn append_element(&mut self, parent: &[usize], element: Element) -> Option<NodePath> {
        let mut path = parent.to_vec();
        let parent = self.element_at_mut(parent)?;
        let index = parent.children.len();
        parent.children.push(Node::Element(element));
        path.push(index);
        Some(path)
    }

    pub fn remove_child(&mut self, parent: &[usize], child: &[usize]) -> Option<Element> {
        if child.len() != parent.len().saturating_add(1)
            || child[..parent.len()] != *parent
        {
            return None;
        }
        let index = *child.last()?;
        let parent = self.element_at_mut(parent)?;
        let node = parent.children.get_mut(index)?;
        match std::mem::replace(node, Node::Text(String::new())) {
            Node::Element(element) => Some(element),
            Node::Text(text) => {
                *node = Node::Text(text);
                None
            }
        }
    }

    pub fn element_clone(&self, path: &[usize]) -> Option<Element> {
        self.element_at(path).cloned()
    }

    pub fn element_attribute(&self, path: &[usize], name: &str) -> Option<String> {
        self.element_at(path)
            .and_then(|element| element.attr(name).map(str::to_string))
    }

    pub fn set_element_attribute(&mut self, path: &[usize], name: &str, value: String) -> bool {
        let Some(element) = self.element_at_mut(path) else {
            return false;
        };
        element.attrs.insert(name.to_ascii_lowercase(), value);
        true
    }

    pub fn remove_element_attribute(&mut self, path: &[usize], name: &str) -> bool {
        self.element_at_mut(path)
            .and_then(|element| element.attrs.remove(&name.to_ascii_lowercase()))
            .is_some()
    }

    pub fn element_has_class(&self, path: &[usize], class: &str) -> bool {
        self.element_at(path)
            .is_some_and(|element| element.has_class(class))
    }

    pub fn add_element_class(&mut self, path: &[usize], class: &str) -> bool {
        self.element_at_mut(path)
            .is_some_and(|element| element.add_class(class))
    }

    pub fn remove_element_class(&mut self, path: &[usize], class: &str) -> bool {
        self.element_at_mut(path)
            .is_some_and(|element| element.remove_class(class))
    }

    pub fn toggle_element_class(
        &mut self,
        path: &[usize],
        class: &str,
        force: Option<bool>,
    ) -> Option<bool> {
        self.element_at_mut(path)
            .map(|element| element.toggle_class(class, force))
    }

    pub fn style_property(&self, path: &[usize], property: &str) -> Option<String> {
        self.element_at(path)?.style_property(property)
    }

    pub fn set_style_property(&mut self, path: &[usize], property: &str, value: String) -> bool {
        let Some(element) = self.element_at_mut(path) else {
            return false;
        };
        element.set_style_property(property, value)
    }

    fn element_at(&self, path: &[usize]) -> Option<&Element> {
        let mut element = &self.root;
        for &index in path {
            element = match element.children.get(index)? {
                Node::Element(child) => child,
                Node::Text(_) => return None,
            };
        }
        Some(element)
    }

    fn element_at_mut(&mut self, path: &[usize]) -> Option<&mut Element> {
        let mut element = &mut self.root;
        for &index in path {
            element = match element.children.get_mut(index)? {
                Node::Element(child) => child,
                Node::Text(_) => return None,
            };
        }
        Some(element)
    }

    fn ancestors_for_path(&self, path: &[usize]) -> Option<Vec<&Element>> {
        let mut ancestors = Vec::new();
        let mut element = &self.root;
        for &index in path {
            element = match element.children.get(index)? {
                Node::Element(child) => child,
                Node::Text(_) => return None,
            };
            ancestors.push(element);
        }
        Some(ancestors)
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
        let Some(class) = class_token(class) else {
            return false;
        };
        self.classes().any(|candidate| candidate == class)
    }

    pub fn add_class(&mut self, class: &str) -> bool {
        let Some(class) = class_token(class) else {
            return false;
        };
        if self.has_class(class) {
            return false;
        }
        let classes = self
            .classes()
            .chain(std::iter::once(class))
            .collect::<Vec<_>>()
            .join(" ");
        self.attrs.insert("class".to_string(), classes);
        true
    }

    pub fn remove_class(&mut self, class: &str) -> bool {
        let Some(class) = class_token(class) else {
            return false;
        };
        if !self.has_class(class) {
            return false;
        }
        let classes = self
            .classes()
            .filter(|candidate| *candidate != class)
            .collect::<Vec<_>>()
            .join(" ");
        self.attrs.insert("class".to_string(), classes);
        true
    }

    pub fn toggle_class(&mut self, class: &str, force: Option<bool>) -> bool {
        let present = self.has_class(class);
        let enabled = force.unwrap_or(!present);
        if enabled {
            self.add_class(class);
        } else {
            self.remove_class(class);
        }
        enabled
    }

    pub fn inner_html(&self) -> String {
        let mut output = String::new();
        serialize_nodes(&self.children, &mut output);
        output
    }

    pub fn set_inner_html(&mut self, source: &str) -> Result<(), ParseError> {
        let fragment = Document::parse(source)?;
        self.children = fragment.root.children;
        Ok(())
    }

    pub fn style_property(&self, property: &str) -> Option<String> {
        let style = self.attr("style")?;
        let property = css_property_name(property);
        style.split(';').find_map(|declaration| {
            let (name, value) = declaration.split_once(':')?;
            (name.trim().eq_ignore_ascii_case(&property)).then(|| value.trim().to_string())
        })
    }

    pub fn set_style_property(&mut self, property: &str, value: String) -> bool {
        let property = css_property_name(property);
        let mut declarations = self
            .attr("style")
            .unwrap_or("")
            .split(';')
            .filter_map(|declaration| {
                let (name, value) = declaration.split_once(':')?;
                Some((name.trim().to_ascii_lowercase(), value.trim().to_string()))
            })
            .collect::<Vec<_>>();
        if let Some(declaration) = declarations
            .iter_mut()
            .find(|(name, _)| name.eq_ignore_ascii_case(&property))
        {
            declaration.1 = value;
        } else {
            declarations.push((property, value));
        }
        let style = declarations
            .into_iter()
            .map(|(name, value)| format!("{name}: {value}"))
            .collect::<Vec<_>>()
            .join("; ");
        self.attrs.insert("style".to_string(), style);
        true
    }
}

fn accessibility_node(element: &Element, path: NodePath) -> Option<AccessibilityNode> {
    if is_accessibility_hidden(element) {
        return None;
    }
    let children = element
        .children
        .iter()
        .enumerate()
        .filter_map(|(index, node)| match node {
            Node::Element(element) => {
                let mut child_path = path.clone();
                child_path.push(index);
                accessibility_node(element, child_path)
            }
            Node::Text(_) => None,
        })
        .collect::<Vec<_>>();
    let role = accessibility_role(element);
    let name = accessibility_name(element);
    let value = accessibility_value(element);
    Some(AccessibilityNode {
        role,
        name,
        value,
        path,
        disabled: is_accessibility_disabled(element),
        focusable: is_accessibility_focusable(element),
        children,
    })
}

fn accessibility_role(element: &Element) -> String {
    if let Some(role) = element.attr("role").filter(|role| !role.trim().is_empty()) {
        return role.trim().to_ascii_lowercase();
    }
    match element.name.as_str() {
        "main" => "main".to_string(),
        "nav" => "navigation".to_string(),
        "header" => "banner".to_string(),
        "footer" => "contentinfo".to_string(),
        "aside" => "complementary".to_string(),
        "form" => "form".to_string(),
        "button" => "button".to_string(),
        "a" if element.attr("href").is_some() => "link".to_string(),
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => "heading".to_string(),
        "img" => "img".to_string(),
        "input" => input_accessibility_role(element),
        "textarea" => "textbox".to_string(),
        "select" => "combobox".to_string(),
        "option" => "option".to_string(),
        "ul" | "ol" => "list".to_string(),
        "li" => "listitem".to_string(),
        "table" => "table".to_string(),
        "tr" => "row".to_string(),
        "th" => "columnheader".to_string(),
        "td" => "cell".to_string(),
        "label" => "label".to_string(),
        _ => "generic".to_string(),
    }
}

fn input_accessibility_role(element: &Element) -> String {
    match element
        .attr("type")
        .unwrap_or("text")
        .to_ascii_lowercase()
        .as_str()
    {
        "button" | "submit" | "reset" | "image" => "button".to_string(),
        "checkbox" => "checkbox".to_string(),
        "radio" => "radio".to_string(),
        "range" => "slider".to_string(),
        "hidden" => "none".to_string(),
        _ => "textbox".to_string(),
    }
}

fn accessibility_name(element: &Element) -> String {
    if let Some(label) = element.attr("aria-label").filter(|label| !label.trim().is_empty()) {
        return label.trim().to_string();
    }
    if element.name == "img"
        && let Some(alt) = element.attr("alt")
    {
        return alt.to_string();
    }
    if let Some(title) = element.attr("title").filter(|title| !title.trim().is_empty()) {
        return title.trim().to_string();
    }
    if matches!(
        element.name.as_str(),
        "a" | "button" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "label" | "li" | "option"
    ) || !element_has_element_child(element)
    {
        return element_text_content(element).trim().to_string();
    }
    if matches!(element.name.as_str(), "input" | "textarea") {
        return element
            .attr("placeholder")
            .or_else(|| element.attr("value"))
            .unwrap_or_default()
            .trim()
            .to_string();
    }
    String::new()
}

fn accessibility_value(element: &Element) -> Option<String> {
    element
        .attr("aria-valuenow")
        .or_else(|| element.attr("value"))
        .map(str::to_string)
        .or_else(|| {
            (element.name == "textarea")
                .then(|| element_text_content(element).trim().to_string())
                .filter(|value| !value.is_empty())
        })
}

fn is_accessibility_disabled(element: &Element) -> bool {
    element.attr("disabled").is_some()
        || element
            .attr("aria-disabled")
            .is_some_and(|value| value.eq_ignore_ascii_case("true"))
}

fn is_accessibility_focusable(element: &Element) -> bool {
    if is_accessibility_disabled(element)
        || (element.name == "input"
            && element
                .attr("type")
                .is_some_and(|kind| kind.eq_ignore_ascii_case("hidden")))
    {
        return false;
    }
    if let Some(tabindex) = element.attr("tabindex") {
        return tabindex.parse::<i32>().is_ok_and(|value| value >= 0);
    }
    matches!(
        element.name.as_str(),
        "a" if element.attr("href").is_some()
    ) || matches!(
        element.name.as_str(),
        "button" | "input" | "select" | "textarea"
    )
}

fn is_accessibility_hidden(element: &Element) -> bool {
    if matches!(
        element.name.as_str(),
        "head" | "link" | "meta" | "script" | "style" | "template" | "title"
    ) || (element.name == "input"
        && element
            .attr("type")
            .is_some_and(|kind| kind.eq_ignore_ascii_case("hidden")))
    {
        return true;
    }
    if element.attr("hidden").is_some()
        || element
            .attr("aria-hidden")
            .is_some_and(|value| value.eq_ignore_ascii_case("true"))
    {
        return true;
    }
    let style = element
        .attr("style")
        .unwrap_or_default()
        .to_ascii_lowercase()
        .replace(' ', "");
    style.contains("display:none") || style.contains("visibility:hidden")
}

fn element_has_element_child(element: &Element) -> bool {
    element
        .children
        .iter()
        .any(|node| matches!(node, Node::Element(_)))
}

fn element_text_content(element: &Element) -> String {
    let mut text = String::new();
    collect_text_nodes(element, &mut text);
    text
}

fn class_token(class: &str) -> Option<&str> {
    let class = class.trim();
    (!class.is_empty()
        && !class
            .chars()
            .any(|character| character.is_ascii_whitespace()))
    .then_some(class)
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

fn collect_script_text(element: &Element, scripts: &mut Vec<String>) {
    if element.name == "script" && element.attr("src").is_none() {
        let mut script = String::new();
        collect_text_nodes(element, &mut script);
        scripts.push(script);
    }
    for child in &element.children {
        if let Node::Element(child) = child {
            collect_script_text(child, scripts);
        }
    }
}

fn collect_script_sources(element: &Element, scripts: &mut Vec<ScriptSource>) {
    if element.name == "script" {
        let is_module = element.attr("type").is_some_and(|kind| {
            kind.trim().eq_ignore_ascii_case("module")
        });
        if let Some(source) = element.attr("src") {
            scripts.push(if is_module {
                ScriptSource::ExternalModule(source.to_string())
            } else {
                ScriptSource::External(source.to_string())
            });
        } else {
            let mut script = String::new();
            collect_text_nodes(element, &mut script);
            scripts.push(if is_module {
                ScriptSource::InlineModule(script)
            } else {
                ScriptSource::Inline(script)
            });
        }
    }
    for child in &element.children {
        if let Node::Element(child) = child {
            collect_script_sources(child, scripts);
        }
    }
}

fn collect_stylesheet_sources(element: &Element, stylesheets: &mut Vec<StylesheetSource>) {
    if element.name == "style" {
        let mut stylesheet = String::new();
        collect_text_nodes(element, &mut stylesheet);
        stylesheets.push(StylesheetSource::Inline(stylesheet));
    } else if element.name == "link"
        && element.attr("rel").is_some_and(|rel| {
            rel.split_ascii_whitespace()
                .any(|token| token.eq_ignore_ascii_case("stylesheet"))
        })
        && let Some(href) = element.attr("href")
    {
        stylesheets.push(StylesheetSource::External(href.to_string()));
    }
    for child in &element.children {
        if let Node::Element(child) = child {
            collect_stylesheet_sources(child, stylesheets);
        }
    }
}

fn find_element_by_id(element: &Element, id: &str, path: &mut NodePath) -> Option<NodePath> {
    for (index, child) in element.children.iter().enumerate() {
        let Node::Element(child) = child else {
            continue;
        };
        path.push(index);
        if child.attr("id") == Some(id) {
            return Some(path.clone());
        }
        if let Some(found) = find_element_by_id(child, id, path) {
            return Some(found);
        }
        path.pop();
    }
    None
}

fn collect_autofocus_paths(element: &Element, path: &mut NodePath, paths: &mut Vec<NodePath>) {
    for (index, child) in element.children.iter().enumerate() {
        let Node::Element(child) = child else {
            continue;
        };
        path.push(index);
        if child.attrs.contains_key("autofocus") {
            paths.push(path.clone());
        }
        collect_autofocus_paths(child, path, paths);
        path.pop();
    }
}

fn collect_form_control_paths(element: &Element, path: &mut NodePath, paths: &mut Vec<NodePath>) {
    for (index, child) in element.children.iter().enumerate() {
        let Node::Element(child) = child else {
            continue;
        };
        path.push(index);
        if matches!(child.name.as_str(), "button" | "input" | "select" | "textarea") {
            paths.push(path.clone());
        }
        collect_form_control_paths(child, path, paths);
        path.pop();
    }
}

fn collect_option_paths(element: &Element, path: &mut NodePath, paths: &mut Vec<NodePath>) {
    for (index, child) in element.children.iter().enumerate() {
        let Node::Element(child) = child else {
            continue;
        };
        path.push(index);
        if child.name == "option" {
            paths.push(path.clone());
        } else if child.name != "select" {
            collect_option_paths(child, path, paths);
        }
        path.pop();
    }
}

fn find_first_labelable_descendant(element: &Element, path: &mut NodePath) -> Option<NodePath> {
    for (index, child) in element.children.iter().enumerate() {
        let Node::Element(child) = child else {
            continue;
        };
        path.push(index);
        if is_labelable(child) {
            return Some(path.clone());
        }
        if let Some(found) = find_first_labelable_descendant(child, path) {
            return Some(found);
        }
        path.pop();
    }
    None
}

fn is_labelable(element: &Element) -> bool {
    match element.name.as_str() {
        "button" | "meter" | "output" | "progress" | "select" | "textarea" => true,
        "input" => !element
            .attr("type")
            .is_some_and(|kind| kind.eq_ignore_ascii_case("hidden")),
        _ => false,
    }
}

fn find_element_by_selector<'a>(
    element: &'a Element,
    selectors: &[crate::css::Selector],
    ancestors: &mut Vec<&'a Element>,
    path: &mut NodePath,
) -> Option<NodePath> {
    for (index, child) in element.children.iter().enumerate() {
        let Node::Element(child) = child else {
            continue;
        };
        path.push(index);
        if selectors.iter().any(|selector| {
            selector.matches(child, path, ancestors, &crate::css::StyleState::default())
        }) {
            return Some(path.clone());
        }
        ancestors.push(child);
        if let Some(found) = find_element_by_selector(child, selectors, ancestors, path) {
            return Some(found);
        }
        ancestors.pop();
        path.pop();
    }
    None
}

fn collect_elements_by_selector<'a>(
    element: &'a Element,
    selectors: &[crate::css::Selector],
    ancestors: &mut Vec<&'a Element>,
    path: &mut NodePath,
    matches: &mut Vec<NodePath>,
) {
    for (index, child) in element.children.iter().enumerate() {
        let Node::Element(child) = child else {
            continue;
        };
        path.push(index);
        if selectors.iter().any(|selector| {
            selector.matches(child, path, ancestors, &crate::css::StyleState::default())
        }) {
            matches.push(path.clone());
        }
        ancestors.push(child);
        collect_elements_by_selector(child, selectors, ancestors, path, matches);
        ancestors.pop();
        path.pop();
    }
}

fn css_property_name(property: &str) -> String {
    let mut output = String::with_capacity(property.len() + 2);
    for character in property.chars() {
        if character.is_ascii_uppercase() {
            output.push('-');
            output.push(character.to_ascii_lowercase());
        } else {
            output.push(character);
        }
    }
    output
}

fn collect_text_nodes(element: &Element, output: &mut String) {
    for child in &element.children {
        match child {
            Node::Element(child) => collect_text_nodes(child, output),
            Node::Text(text) => output.push_str(text),
        }
    }
}

fn serialize_nodes(nodes: &[Node], output: &mut String) {
    for node in nodes {
        match node {
            Node::Element(element) => serialize_element(element, output),
            Node::Text(text) => escape_html(text, output),
        }
    }
}

fn serialize_element(element: &Element, output: &mut String) {
    output.push('<');
    output.push_str(&element.name);
    for (name, value) in &element.attrs {
        output.push(' ');
        output.push_str(name);
        output.push_str("=\"");
        escape_html_attribute(value, output);
        output.push('"');
    }
    output.push('>');
    if is_void_element(&element.name) {
        return;
    }
    serialize_nodes(&element.children, output);
    output.push_str("</");
    output.push_str(&element.name);
    output.push('>');
}

fn escape_html(text: &str, output: &mut String) {
    for character in text.chars() {
        match character {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            _ => output.push(character),
        }
    }
}

fn escape_html_attribute(text: &str, output: &mut String) {
    for character in text.chars() {
        match character {
            '&' => output.push_str("&amp;"),
            '"' => output.push_str("&quot;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            _ => output.push(character),
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
                name: name.clone(),
                attrs,
                self_closing,
            });
            if !self_closing && matches!(name.as_str(), "script" | "style") {
                let rest = &source[end + 1..];
                let marker = format!("</{name}");
                let lower = rest.to_ascii_lowercase();
                if let Some(close_start) = lower.find(&marker) {
                    let raw_text = &rest[..close_start];
                    if !raw_text.is_empty() {
                        tokens.push(Token::Text(raw_text.to_string()));
                    }
                    cursor = end + 1 + close_start;
                    continue;
                }
            }
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
    fn builds_accessibility_roles_names_and_stable_paths() {
        let document = Document::parse(
            r#"<main><h1>Settings</h1><button aria-label="Save changes">Save</button><input type="checkbox" aria-label="Enable sync" disabled><a href="/help">Help</a><p hidden>secret</p></main>"#,
        )
        .expect("document should parse");
        let tree = document.accessibility_tree();
        assert_eq!(tree.role, "document");
        let main = &tree.children[0];
        assert_eq!((main.role.as_str(), main.path.as_slice()), ("main", [0].as_slice()));
        assert_eq!(main.children.len(), 4);
        assert_eq!(
            (
                main.children[0].role.as_str(),
                main.children[0].name.as_str(),
                main.children[0].path.as_slice(),
                main.children[0].focusable,
            ),
            ("heading", "Settings", [0, 0].as_slice(), false)
        );
        assert_eq!(
            (
                main.children[1].role.as_str(),
                main.children[1].name.as_str(),
                main.children[1].path.as_slice(),
                main.children[1].focusable,
            ),
            ("button", "Save changes", [0, 1].as_slice(), true)
        );
        assert_eq!(
            (
                main.children[2].role.as_str(),
                main.children[2].name.as_str(),
                main.children[2].path.as_slice(),
                main.children[2].disabled,
                main.children[2].focusable,
            ),
            ("checkbox", "Enable sync", [0, 2].as_slice(), true, false)
        );
        assert_eq!(
            (
                main.children[3].role.as_str(),
                main.children[3].name.as_str(),
                main.children[3].path.as_slice(),
                main.children[3].focusable,
            ),
            ("link", "Help", [0, 3].as_slice(), true)
        );
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

    #[test]
    fn preserves_script_text_that_contains_comparison_operators() {
        let document = Document::parse(
            "<script>if (count < 2) { count = count + 1; }</script><button id='go'>go</button>",
        )
        .expect("document should parse");
        assert_eq!(
            document.script_text(),
            vec!["if (count < 2) { count = count + 1; }".to_string()]
        );
    }

    #[test]
    fn mutates_text_and_style_through_a_stable_path() {
        let mut document = Document::parse("<button id='go' style='color: red'>go</button>")
            .expect("document should parse");
        let path = document
            .find_element_by_id("go")
            .expect("button should exist");
        assert!(document.set_element_text_content(&path, "done".to_string()));
        assert!(document.set_style_property(&path, "backgroundColor", "#fff".to_string()));
        assert_eq!(
            document.element_text_content(&path).as_deref(),
            Some("done")
        );
        assert_eq!(
            document
                .style_property(&path, "background-color")
                .as_deref(),
            Some("#fff")
        );
    }

    #[test]
    fn finds_simple_selectors() {
        let document =
            Document::parse("<main><p class='accent'>first</p><p id='second'>second</p></main>")
                .expect("document should parse");
        assert!(document.find_element_by_selector("p").is_some());
        assert!(document.find_element_by_selector(".accent").is_some());
        assert!(document.find_element_by_selector("#second").is_some());
        assert!(document.find_element_by_selector(".missing").is_none());
    }

    #[test]
    fn collects_selector_matches_in_document_order() {
        let document = Document::parse(
            "<main><p data-check='first'>one</p><section><p data-check='second'>two</p></section><p data-check='third'>three</p></main>",
        )
        .expect("document should parse");
        assert_eq!(
            document.find_elements_by_selector("[data-check]"),
            Some(vec![vec![0, 0], vec![0, 1, 0], vec![0, 2]])
        );
        let section = document
            .find_element_by_selector("section")
            .expect("section should exist");
        assert_eq!(
            document.find_elements_by_selector_within(&section, "[data-check]"),
            Some(vec![vec![0, 1, 0]])
        );
    }

    #[test]
    fn collects_form_controls_in_document_order() {
        let document = Document::parse(
            "<form id='profile'><input name='name'><fieldset><textarea name='note'></textarea></fieldset><button name='save'>save</button></form>",
        )
        .expect("document should parse");
        let form = document
            .find_element_by_id("profile")
            .expect("form should exist");
        assert_eq!(
            document.form_control_paths(&form),
            Some(vec![vec![0, 0], vec![0, 1, 0], vec![0, 2]])
        );
        assert_eq!(
            document.form_ancestor_path(&[0, 1, 0]),
            Some(vec![0])
        );
    }

    #[test]
    fn resolves_explicit_and_wrapped_label_controls() {
        let document = Document::parse(
            "<label id='explicit' for='name'>Name</label><input id='name'><label id='wrapped'><span>Remember</span><input id='remember' type='checkbox'></label><label id='hidden' for='secret'>Secret</label><input id='secret' type='hidden'>",
        )
        .expect("labels should parse");
        let explicit = document
            .find_element_by_id("explicit")
            .expect("explicit label should exist");
        let wrapped = document
            .find_element_by_id("wrapped")
            .expect("wrapped label should exist");
        let hidden = document
            .find_element_by_id("hidden")
            .expect("hidden label should exist");
        assert_eq!(
            document.label_control_path(&explicit),
            Some(vec![1])
        );
        assert_eq!(
            document.label_control_path(&wrapped),
            Some(vec![2, 1])
        );
        assert!(document.label_control_path(&hidden).is_none());
        assert_eq!(
            document.label_ancestor_path(&[2, 0]),
            Some(vec![2])
        );
    }

    #[test]
    fn collects_autofocus_candidates_in_document_order() {
        let document = Document::parse(
            "<main autofocus><input id='first' autofocus><section><button id='second' autofocus>go</button></section></main>",
        )
        .expect("autofocus document should parse");
        assert_eq!(
            document.autofocus_paths(),
            vec![vec![0], vec![0, 0], vec![0, 1, 0]]
        );
    }

    #[test]
    fn preserves_document_order_for_external_sources() {
        let document = Document::parse(
            "<link rel='StyleSheet' href='/theme.css'><style>p { color: red; }</style><script src='/boot.js'></script><script>console.log('inline');</script><script type='module' src='/entry.js'></script><script type='module'>console.log('inline module');</script>",
        )
        .expect("document should parse");
        assert_eq!(
            document.stylesheet_sources(),
            vec![
                StylesheetSource::External("/theme.css".to_string()),
                StylesheetSource::Inline("p { color: red; }".to_string()),
            ]
        );
        assert_eq!(
            document.script_sources(),
            vec![
                ScriptSource::External("/boot.js".to_string()),
                ScriptSource::Inline("console.log('inline');".to_string()),
                ScriptSource::ExternalModule("/entry.js".to_string()),
                ScriptSource::InlineModule("console.log('inline module');".to_string()),
            ]
        );
    }

    #[test]
    fn appends_an_element_and_returns_its_stable_path() {
        let mut document =
            Document::parse("<main id='root'><p>before</p></main>").expect("document should parse");
        let root = document
            .find_element_by_id("root")
            .expect("root should exist");
        let path = document
            .append_element(&root, Element::new("section"))
            .expect("child should append");
        assert_eq!(path, vec![0, 1]);
        assert_eq!(
            document.element_clone(&path).map(|element| element.name),
            Some("section".to_string())
        );
    }

    #[test]
    fn removes_a_direct_child_without_reindexing_survivors() {
        let mut document = Document::parse(
            "<main><p id='first'>first</p><p id='gone'>gone</p><p id='last'>last</p></main>",
        )
        .expect("document should parse");
        let main = document
            .find_element_by_selector("main")
            .expect("main should exist");
        let gone = document
            .find_element_by_id("gone")
            .expect("removed child should exist");
        let last = document
            .find_element_by_id("last")
            .expect("survivor should exist");
        assert_eq!(document.remove_child(&main, &gone).map(|element| element.name), Some("p".to_string()));
        assert!(document.find_element_by_id("gone").is_none());
        assert_eq!(
            document.element_text_content(&last).as_deref(),
            Some("last")
        );
        let reattached = document
            .append_element(&main, Element::new("aside"))
            .expect("append should keep the removed slot behind the new child");
        assert_eq!(reattached, vec![0, 3]);
    }

    #[test]
    fn queries_compound_descendant_and_child_selectors() {
        let document = Document::parse(
            "<main class='shell'><section><p class='card'>nested</p></section><p class='direct'>direct</p></main>",
        )
        .expect("document should parse");
        assert_eq!(
            document
                .find_element_by_selector(".shell .card")
                .and_then(|path| document.element_text_content(&path)),
            Some("nested".to_string())
        );
        assert!(
            document
                .find_element_by_selector(".shell > .direct")
                .is_some()
        );
        assert!(document.find_element_by_selector("section.card").is_none());
        let shell = document
            .find_element_by_selector(".shell")
            .expect("shell should exist");
        assert_eq!(
            document
                .find_element_by_selector_within(&shell, ".card")
                .and_then(|path| document.element_text_content(&path)),
            Some("nested".to_string())
        );
    }

    #[test]
    fn queries_selector_lists_and_not_pseudo_classes() {
        let document = Document::parse(
            "<main><p class='muted'>skip</p><p class='card'>first</p><section data-label='a,b'>second</section></main>",
        )
        .expect("document should parse");
        assert_eq!(
            document.find_element_by_selector(".muted, .card:not(.muted)"),
            Some(vec![0, 0])
        );
        assert_eq!(
            document.find_element_by_selector(".card:not(.muted)"),
            Some(vec![0, 1])
        );
        assert_eq!(
            document.find_elements_by_selector("[data-label='a,b'], .card:not(.muted)"),
            Some(vec![vec![0, 1], vec![0, 2]])
        );
    }
}
