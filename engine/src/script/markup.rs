// DOMParser, which parses markup into a document of its own beside the page's, and
// XMLSerializer, which writes a node back out as markup

use super::object::{JsObject, ObjectClass, ObjectHandle, Property, new_object};
use super::operations::is_primitive;
use super::realm::{Builder, Native};
use super::*;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum MarkupMember {
    Parser,
    ParseFromString,
    Serializer,
    SerializeToString,
}

pub(super) fn create(
    build: &Builder,
    object_prototype: &ObjectHandle,
    globals: &mut Vec<(&'static str, Value)>,
) {
    for (name, constructor, method, method_name, length) in [
        (
            "DOMParser",
            MarkupMember::Parser,
            MarkupMember::ParseFromString,
            "parseFromString",
            2,
        ),
        (
            "XMLSerializer",
            MarkupMember::Serializer,
            MarkupMember::SerializeToString,
            "serializeToString",
            1,
        ),
    ] {
        let prototype = new_object(JsObject::with_prototype(
            Some(object_prototype.clone()),
            ObjectClass::Ordinary,
        ));
        let function = build.function(Native::Markup(method), method_name, length);
        prototype.borrow_mut().define(
            method_name.to_string(),
            Property::hidden(Value::Object(function)),
        );
        prototype.borrow_mut().define(
            "@@toStringTag".to_string(),
            Property {
                writable: false,
                ..Property::hidden(Value::String(name.into()))
            },
        );
        let constructor = build.constructor(Native::Markup(constructor), name, 0, &prototype);
        globals.push((name, Value::Object(constructor)));
    }
}

impl ScriptRuntime {
    pub(super) fn construct_markup(&mut self, new_target: Value) -> Result<Value, ScriptError> {
        let prototype = self.prototype_from_constructor(&new_target)?;
        Ok(Value::Object(new_object(JsObject::with_prototype(
            Some(prototype),
            ObjectClass::Ordinary,
        ))))
    }

    pub(super) fn call_markup(
        &mut self,
        member: MarkupMember,
        this: Value,
        arguments: Vec<Value>,
    ) -> Result<Value, ScriptError> {
        let first = arguments.first().cloned().unwrap_or(Value::Undefined);
        match member {
            MarkupMember::Parser | MarkupMember::Serializer => {
                Err(self.type_error("Failed to construct: Please use the 'new' operator"))
            }
            _ if is_primitive(&this) => Err(self.type_error("Illegal invocation")),
            MarkupMember::ParseFromString => {
                let source = self.coerce_string(first)?;
                let kind =
                    self.coerce_string(arguments.get(1).cloned().unwrap_or(Value::Undefined))?;
                let html = match kind.as_str() {
                    "text/html" => true,
                    "text/xml" | "application/xml" | "application/xhtml+xml" | "image/svg+xml" => {
                        false
                    }
                    _ => {
                        return Err(self.type_error(format!(
                            "Failed to execute 'parseFromString' on 'DOMParser': The provided value '{kind}' is not a valid enum value of type DOMParserSupportedType."
                        )));
                    }
                };
                let parsed = self.document.borrow_mut().parse_document(&source, html);
                // markup that does not parse yields a document holding a parsererror element
                let root = match parsed {
                    Ok(root) => root,
                    Err(error) => {
                        let report = format!("<parsererror>{}</parsererror>", error.message);
                        let reparsed = self.document.borrow_mut().parse_document(&report, false);
                        reparsed.map_err(|error| self.type_error(error.message))?
                    }
                };
                Ok(self.node_value(root))
            }
            MarkupMember::SerializeToString => {
                let node = match first.resolve() {
                    Value::Element(handle) => handle.id,
                    Value::Document => self.document_node(),
                    _ => {
                        return Err(self.type_error(
                            "Failed to execute 'serializeToString' on 'XMLSerializer': parameter 1 is not of type 'Node'.",
                        ));
                    }
                };
                Ok(Value::String(
                    self.document.borrow().outer_html(node).into(),
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parsed_documents_answer_like_the_page_document() {
        let document =
            crate::dom::Document::parse(r#"<p id="out"></p>"#).expect("document should parse");
        let document = Rc::new(RefCell::new(document));
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(
                r#"
                const parsed = new DOMParser().parseFromString("<title>Notes</title><p id='a' class='x'>one</p><p class='x'>two</p>", "text/html");
                const seen = [
                    parsed instanceof Document,
                    parsed.title,
                    parsed.body.children.length,
                    parsed.getElementById("a").textContent,
                    parsed.querySelectorAll(".x").length,
                    parsed.documentElement.tagName,
                    parsed.head.firstChild.tagName,
                    parsed.getElementById("a").ownerDocument === parsed,
                    document.getElementById("a") === null,
                ];
                const svg = new DOMParser().parseFromString("<svg xmlns='http://www.w3.org/2000/svg'><circle r='2'/></svg>", "image/svg+xml");
                seen.push(svg.documentElement.tagName, svg.querySelector("circle").getAttribute("r"));
                const made = parsed.createElement("em");
                made.textContent = "moved";
                parsed.body.append(made);
                document.getElementById("out").append(document.adoptNode(parsed.querySelector("em")));
                seen.push(new XMLSerializer().serializeToString(parsed.querySelector("title")));
                try { new DOMParser().parseFromString("", "text/plain"); } catch (error) { seen.push(error.name); }
                document.getElementById("out").setAttribute("data-seen", seen.join("|"));
                "#,
            )
            .expect("script should run");
        let out = document.borrow().find_element_by_id("out").unwrap();
        assert_eq!(
            document
                .borrow()
                .element(out)
                .and_then(|element| element.attr("data-seen")),
            Some(
                "true|Notes|2|one|2|HTML|TITLE|true|true|svg|2|<title>Notes</title>|TypeError"
            )
        );
        assert_eq!(
            document.borrow().element_text_content(out).as_deref(),
            Some("moved")
        );
    }
}
