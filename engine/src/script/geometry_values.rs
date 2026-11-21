// DOMRect and DOMPoint with their read-only parents: the rectangles getBoundingClientRect and
// the observers hand out are DOMRect objects, and pages can make their own

use super::object::{JsObject, ObjectClass, ObjectHandle, Property, Slot, new_object};
use super::realm::{Builder, Native};
use super::*;
use crate::layout::Rect;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ShapeMember {
    Top,
    Right,
    Bottom,
    Left,
    RectToJson,
    PointToJson,
    FromRect(bool),
    FromPoint(bool),
}

pub(super) struct ShapeInterfaces {
    rect: ObjectHandle,
    rect_read_only: ObjectHandle,
    point: ObjectHandle,
    point_read_only: ObjectHandle,
}

const RECT: [&str; 4] = ["x", "y", "width", "height"];
const POINT: [&str; 4] = ["x", "y", "z", "w"];

fn method(build: &Builder, target: &ObjectHandle, name: &str, member: ShapeMember, length: u32) {
    let function = build.function(Native::Shape(member), name, length);
    target.borrow_mut().define(
        name.to_string(),
        Property {
            enumerable: true,
            ..Property::data(Value::Object(function))
        },
    );
}

fn tag(target: &ObjectHandle, name: &str) {
    target.borrow_mut().define(
        "@@toStringTag".to_string(),
        Property::hidden(Value::String(name.into())),
    );
}

pub(super) fn create(
    build: &Builder,
    object_prototype: &ObjectHandle,
    globals: &mut Vec<(&'static str, Value)>,
) -> ShapeInterfaces {
    let under = |parent: &ObjectHandle| {
        new_object(JsObject::with_prototype(
            Some(parent.clone()),
            ObjectClass::Ordinary,
        ))
    };
    let rect_read_only = under(object_prototype);
    for (name, member) in [
        ("top", ShapeMember::Top),
        ("right", ShapeMember::Right),
        ("bottom", ShapeMember::Bottom),
        ("left", ShapeMember::Left),
    ] {
        let get = build.function(Native::Shape(member), &format!("get {name}"), 0);
        rect_read_only.borrow_mut().define(
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
    }
    method(build, &rect_read_only, "toJSON", ShapeMember::RectToJson, 0);
    tag(&rect_read_only, "DOMRectReadOnly");
    let rect = under(&rect_read_only);
    tag(&rect, "DOMRect");
    let point_read_only = under(object_prototype);
    method(
        build,
        &point_read_only,
        "toJSON",
        ShapeMember::PointToJson,
        0,
    );
    tag(&point_read_only, "DOMPointReadOnly");
    let point = under(&point_read_only);
    tag(&point, "DOMPoint");
    for (name, prototype, native, from) in [
        (
            "DOMRectReadOnly",
            &rect_read_only,
            Native::RectConstructor(true),
            ShapeMember::FromRect(true),
        ),
        (
            "DOMRect",
            &rect,
            Native::RectConstructor(false),
            ShapeMember::FromRect(false),
        ),
        (
            "DOMPointReadOnly",
            &point_read_only,
            Native::PointConstructor(true),
            ShapeMember::FromPoint(true),
        ),
        (
            "DOMPoint",
            &point,
            Native::PointConstructor(false),
            ShapeMember::FromPoint(false),
        ),
    ] {
        let constructor = build.constructor(native, name, 0, prototype);
        let static_name = if name.contains("Rect") {
            "fromRect"
        } else {
            "fromPoint"
        };
        method(build, &constructor, static_name, from, 0);
        globals.push((name, Value::Object(constructor)));
    }
    ShapeInterfaces {
        rect,
        rect_read_only,
        point,
        point_read_only,
    }
}

impl ScriptRuntime {
    // a DOMRect as layout measured it, its edges kept as values of their own
    pub(super) fn rect_object(&self, rect: Rect) -> Value {
        let object = new_object(JsObject::with_prototype(
            Some(self.intrinsics.shapes.rect.clone()),
            ObjectClass::Ordinary,
        ));
        for (name, value) in [
            ("x", rect.x),
            ("y", rect.y),
            ("width", rect.width),
            ("height", rect.height),
            ("top", rect.y),
            ("right", rect.x + rect.width),
            ("bottom", rect.y + rect.height),
            ("left", rect.x),
        ] {
            object.borrow_mut().define(
                name.to_string(),
                Property::data(Value::Number(f64::from(value))),
            );
        }
        Value::Object(object)
    }

    // four numbers, each from an argument or a dictionary member, with defaults
    fn shape_numbers(
        &mut self,
        names: [&str; 4],
        defaults: [f64; 4],
        given: &[Value],
        dictionary: bool,
    ) -> Result<[f64; 4], ScriptError> {
        let mut values = defaults;
        for (index, name) in names.into_iter().enumerate() {
            let value = if dictionary {
                match given.first().map(Value::resolve) {
                    Some(source @ Value::Object(_)) => self.get_property(&source, name)?,
                    _ => Value::Undefined,
                }
            } else {
                given.get(index).cloned().unwrap_or(Value::Undefined)
            };
            if !matches!(value, Value::Undefined) {
                values[index] = self.coerce_number(value)?;
            }
        }
        Ok(values)
    }

    fn shape_object(&self, prototype: ObjectHandle, names: [&str; 4], values: [f64; 4]) -> Value {
        let object = new_object(JsObject::with_prototype(
            Some(prototype),
            ObjectClass::Ordinary,
        ));
        for (name, value) in names.into_iter().zip(values) {
            object
                .borrow_mut()
                .define(name.to_string(), Property::data(Value::Number(value)));
        }
        Value::Object(object)
    }

    fn shape_prototype(
        &mut self,
        new_target: &Value,
        fallback: ObjectHandle,
    ) -> Result<ObjectHandle, ScriptError> {
        match new_target {
            Value::Object(_) => self.prototype_from_constructor(new_target),
            _ => Ok(fallback),
        }
    }

    pub(super) fn construct_rect(
        &mut self,
        read_only: bool,
        arguments: Vec<Value>,
        new_target: Value,
    ) -> Result<Value, ScriptError> {
        let fallback = if read_only {
            self.intrinsics.shapes.rect_read_only.clone()
        } else {
            self.intrinsics.shapes.rect.clone()
        };
        let prototype = self.shape_prototype(&new_target, fallback)?;
        let values = self.shape_numbers(RECT, [0.0; 4], &arguments, false)?;
        Ok(self.shape_object(prototype, RECT, values))
    }

    pub(super) fn construct_point(
        &mut self,
        read_only: bool,
        arguments: Vec<Value>,
        new_target: Value,
    ) -> Result<Value, ScriptError> {
        let fallback = if read_only {
            self.intrinsics.shapes.point_read_only.clone()
        } else {
            self.intrinsics.shapes.point.clone()
        };
        let prototype = self.shape_prototype(&new_target, fallback)?;
        let values = self.shape_numbers(POINT, [0.0, 0.0, 0.0, 1.0], &arguments, false)?;
        Ok(self.shape_object(prototype, POINT, values))
    }

    pub(super) fn call_shape(
        &mut self,
        member: ShapeMember,
        this: Value,
        arguments: Vec<Value>,
    ) -> Result<Value, ScriptError> {
        let number = |runtime: &mut Self, name: &str| -> Result<f64, ScriptError> {
            let value = runtime.get_property(&this, name)?;
            runtime.coerce_number(value)
        };
        Ok(match member {
            // edges of a rectangle whose width or height runs negative still bound it
            ShapeMember::Top | ShapeMember::Bottom => {
                let (y, height) = (number(self, "y")?, number(self, "height")?);
                let (low, high) = (y.min(y + height), y.max(y + height));
                Value::Number(if member == ShapeMember::Top {
                    low
                } else {
                    high
                })
            }
            ShapeMember::Left | ShapeMember::Right => {
                let (x, width) = (number(self, "x")?, number(self, "width")?);
                let (low, high) = (x.min(x + width), x.max(x + width));
                Value::Number(if member == ShapeMember::Left {
                    low
                } else {
                    high
                })
            }
            ShapeMember::RectToJson => {
                let mut entries = Vec::new();
                for name in RECT.into_iter().chain(["top", "right", "bottom", "left"]) {
                    entries.push((name.to_string(), self.get_property(&this, name)?));
                }
                plain_object(entries)
            }
            ShapeMember::PointToJson => {
                let mut entries = Vec::new();
                for name in POINT {
                    entries.push((name.to_string(), self.get_property(&this, name)?));
                }
                plain_object(entries)
            }
            ShapeMember::FromRect(read_only) => {
                let values = self.shape_numbers(RECT, [0.0; 4], &arguments, true)?;
                let prototype = if read_only {
                    self.intrinsics.shapes.rect_read_only.clone()
                } else {
                    self.intrinsics.shapes.rect.clone()
                };
                self.shape_object(prototype, RECT, values)
            }
            ShapeMember::FromPoint(read_only) => {
                let values = self.shape_numbers(POINT, [0.0, 0.0, 0.0, 1.0], &arguments, true)?;
                let prototype = if read_only {
                    self.intrinsics.shapes.point_read_only.clone()
                } else {
                    self.intrinsics.shapes.point.clone()
                };
                self.shape_object(prototype, POINT, values)
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rects_and_points_are_made_measured_and_serialized() {
        let document =
            crate::dom::Document::parse(r#"<html><body><p id="status"></p></body></html>"#)
                .expect("document should parse");
        let document = Rc::new(RefCell::new(document));
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(
                r##"
                const r = new DOMRect(10, 20, -4, 6);
                const seen = [r.left, r.right, r.top, r.bottom, r instanceof DOMRectReadOnly, String(r)];
                r.x = 0;
                seen.push(r.left, JSON.stringify(r));
                const copy = DOMRect.fromRect({ width: 3 });
                seen.push(copy.x, copy.width, copy.height, copy instanceof DOMRect);
                const p = new DOMPoint(1, 2);
                seen.push(p.z, p.w, JSON.stringify(DOMPointReadOnly.fromPoint({ y: 5 })), String(p));
                const box = document.body.getBoundingClientRect();
                seen.push(box instanceof DOMRect, typeof box.toJSON);
                document.getElementById("status").textContent = seen.join("|");
                "##,
            )
            .expect("script should run");
        let status = document.borrow().find_element_by_id("status").unwrap();
        assert_eq!(
            document.borrow().element_text_content(status).as_deref(),
            Some(
                r#"6|10|20|26|true|[object DOMRect]|-4|{"x":0,"y":20,"width":-4,"height":6,"top":20,"right":0,"bottom":26,"left":-4}|0|3|0|true|0|1|{"x":0,"y":5,"z":0,"w":1}|[object DOMPoint]|true|function"#
            )
        );
    }
}
