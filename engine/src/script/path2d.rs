// Path2D: a path kept in user space as the calls that made it, replayed into a canvas under
// whatever transform is in force where it is filled, stroked, clipped or hit tested

use super::object::{JsObject, ObjectClass, ObjectHandle, Property, new_object};
use super::realm::{Builder, Native};
use super::*;
use crate::svg::{Affine, Segment};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum PathMember {
    Constructor,
    AddPath,
    ClosePath,
    MoveTo,
    LineTo,
    BezierCurveTo,
    QuadraticCurveTo,
    Arc,
    ArcTo,
    Ellipse,
    Rect,
    RoundRect,
}

#[derive(Clone)]
pub(super) enum PathOp {
    Move((f32, f32)),
    Line((f32, f32)),
    Cubic((f32, f32), (f32, f32), (f32, f32)),
    Quadratic((f32, f32), (f32, f32)),
    Close,
    Rect((f32, f32, f32, f32)),
    // the centre, radii, rotation, start and end angles, and whether it runs counterclockwise
    Arc((f32, f32), (f32, f32), f32, (f32, f32), bool),
    ArcTo((f32, f32), (f32, f32), f32),
    // a rectangle and the radii of its corners, top left first and clockwise
    RoundRect((f32, f32, f32, f32), [(f32, f32); 4]),
    // another path added under a matrix of its own
    Transformed(Affine, Vec<PathOp>),
}

pub(super) type PathHandle = Rc<RefCell<Vec<PathOp>>>;

pub(super) struct PathInterfaces {
    prototype: ObjectHandle,
}

pub(super) fn create(
    build: &Builder,
    object_prototype: &ObjectHandle,
    globals: &mut Vec<(&'static str, Value)>,
) -> PathInterfaces {
    let prototype = new_object(JsObject::with_prototype(
        Some(object_prototype.clone()),
        ObjectClass::Ordinary,
    ));
    for (name, member, length) in [
        ("addPath", PathMember::AddPath, 1),
        ("closePath", PathMember::ClosePath, 0),
        ("moveTo", PathMember::MoveTo, 2),
        ("lineTo", PathMember::LineTo, 2),
        ("bezierCurveTo", PathMember::BezierCurveTo, 6),
        ("quadraticCurveTo", PathMember::QuadraticCurveTo, 4),
        ("arc", PathMember::Arc, 5),
        ("arcTo", PathMember::ArcTo, 5),
        ("ellipse", PathMember::Ellipse, 7),
        ("rect", PathMember::Rect, 4),
        ("roundRect", PathMember::RoundRect, 4),
    ] {
        let function = build.function(Native::Path(member), name, length);
        prototype
            .borrow_mut()
            .define(name.to_string(), Property::hidden(Value::Object(function)));
    }
    prototype.borrow_mut().define(
        "@@toStringTag".to_string(),
        Property {
            writable: false,
            ..Property::hidden(Value::String("Path2D".into()))
        },
    );
    let constructor = build.constructor(
        Native::Path(PathMember::Constructor),
        "Path2D",
        0,
        &prototype,
    );
    globals.push(("Path2D", Value::Object(constructor)));
    PathInterfaces { prototype }
}

// the radii roundRect takes: one number or point for every corner, or a list of one to four
// in the order css gives border-radius
pub(super) fn corner_radii(radii: &[(f32, f32)]) -> Option<[(f32, f32); 4]> {
    if radii
        .iter()
        .any(|&(x, y)| !x.is_finite() || !y.is_finite() || x < 0.0 || y < 0.0)
    {
        return None;
    }
    Some(match *radii {
        [] => [(0.0, 0.0); 4],
        [all] => [all; 4],
        [main, cross] => [main, cross, main, cross],
        [first, cross, last] => [first, cross, last, cross],
        [a, b, c, d] => [a, b, c, d],
        _ => return None,
    })
}

impl ScriptRuntime {
    pub(super) fn path_of(value: &Value) -> Option<PathHandle> {
        match value {
            Value::Object(object) => match &object.borrow().class {
                ObjectClass::Path(path) => Some(path.clone()),
                _ => None,
            },
            _ => None,
        }
    }

    pub(super) fn construct_path(
        &mut self,
        arguments: Vec<Value>,
        new_target: Value,
    ) -> Result<Value, ScriptError> {
        let ops = match arguments.first() {
            None | Some(Value::Undefined) => Vec::new(),
            Some(value) => match Self::path_of(value) {
                Some(path) => path.borrow().clone(),
                None => {
                    let data = self.coerce_string(value.clone())?;
                    crate::svg::parse_path(&data)
                        .into_iter()
                        .map(|segment| match segment {
                            Segment::Move(point) => PathOp::Move(point),
                            Segment::Line(point) => PathOp::Line(point),
                            Segment::Cubic(first, second, end) => PathOp::Cubic(first, second, end),
                            Segment::Close => PathOp::Close,
                        })
                        .collect()
                }
            },
        };
        let prototype = match &new_target {
            Value::Object(_) => self.prototype_from_constructor(&new_target)?,
            _ => self.intrinsics.paths.prototype.clone(),
        };
        Ok(Value::Object(new_object(JsObject::with_prototype(
            Some(prototype),
            ObjectClass::Path(Rc::new(RefCell::new(ops))),
        ))))
    }

    // the radii argument of roundRect, as numbers or DOMPointInit objects
    pub(super) fn round_rect_radii(
        &mut self,
        method: &str,
        radii: Option<&Value>,
    ) -> Result<Option<[(f32, f32); 4]>, ScriptError> {
        let listed = match radii {
            None | Some(Value::Undefined) => vec![Value::Number(0.0)],
            Some(value @ (Value::Array(_) | Value::Object(_)))
                if !matches!(self.get_property(value, "@@iterator")?, Value::Undefined) =>
            {
                self.iterate_to_vec(value.clone())?
            }
            Some(value) => vec![value.clone()],
        };
        let mut radii = Vec::new();
        for value in listed {
            radii.push(match &value {
                Value::Object(_) => {
                    let x = self.get_property(&value, "x")?;
                    let y = self.get_property(&value, "y")?;
                    (self.coerce_number(x)? as f32, self.coerce_number(y)? as f32)
                }
                _ => {
                    let radius = self.coerce_number(value)? as f32;
                    (radius, radius)
                }
            });
        }
        if radii.is_empty() || radii.len() > 4 {
            return Err(self.range_error(format!(
                "Failed to execute '{method}': {} radii provided. Between one and four radii are necessary.",
                radii.len()
            )));
        }
        if radii.iter().any(|&(x, y)| x < 0.0 || y < 0.0) {
            return Err(self.range_error(format!(
                "Failed to execute '{method}': Radius value is negative."
            )));
        }
        Ok(corner_radii(&radii))
    }

    pub(super) fn call_path(
        &mut self,
        member: PathMember,
        this: Value,
        arguments: Vec<Value>,
    ) -> Result<Value, ScriptError> {
        if member == PathMember::Constructor {
            return Err(self.type_error("Failed to construct 'Path2D': Please use the 'new' operator, this DOM object constructor cannot be called as a function."));
        }
        let Some(path) = Self::path_of(&this) else {
            return Err(self.type_error("Illegal invocation"));
        };
        if member == PathMember::AddPath {
            let Some(added) = arguments.first().and_then(Self::path_of) else {
                return Err(self.type_error(
                    "Failed to execute 'addPath' on 'Path2D': parameter 1 is not of type 'Path2D'.",
                ));
            };
            let mut matrix = crate::svg::IDENTITY;
            if let Some(transform) = arguments.get(1).filter(|value| !is_primitive_value(value)) {
                for (index, key) in ["a", "b", "c", "d", "e", "f"].into_iter().enumerate() {
                    let value = match self.get_property(transform, key)? {
                        Value::Undefined => {
                            Value::Number(if matches!(index, 0 | 3) { 1.0 } else { 0.0 })
                        }
                        value => value,
                    };
                    matrix[index] = self.coerce_number(value)? as f32;
                }
            }
            let ops = added.borrow().clone();
            if matrix.iter().all(|value| value.is_finite()) {
                path.borrow_mut().push(PathOp::Transformed(matrix, ops));
            }
            return Ok(Value::Undefined);
        }
        let takes = match member {
            PathMember::ClosePath => 0,
            PathMember::MoveTo | PathMember::LineTo => 2,
            PathMember::QuadraticCurveTo | PathMember::Rect | PathMember::RoundRect => 4,
            PathMember::Arc | PathMember::ArcTo => 5,
            PathMember::BezierCurveTo => 6,
            _ => 7,
        };
        let mut numbers = Vec::with_capacity(takes);
        for index in 0..takes {
            let value = arguments.get(index).cloned().unwrap_or(Value::Undefined);
            numbers.push(self.coerce_number(value)? as f32);
        }
        // a call with a value that is not finite is ignored, as the canvas methods are
        if numbers.iter().any(|value| !value.is_finite()) {
            return Ok(Value::Undefined);
        }
        let n = |index: usize| numbers[index];
        let counterclockwise = arguments.get(takes).is_some_and(Value::truthy);
        let op = match member {
            PathMember::ClosePath => PathOp::Close,
            PathMember::MoveTo => PathOp::Move((n(0), n(1))),
            PathMember::LineTo => PathOp::Line((n(0), n(1))),
            PathMember::BezierCurveTo => PathOp::Cubic((n(0), n(1)), (n(2), n(3)), (n(4), n(5))),
            PathMember::QuadraticCurveTo => PathOp::Quadratic((n(0), n(1)), (n(2), n(3))),
            PathMember::Rect => PathOp::Rect((n(0), n(1), n(2), n(3))),
            PathMember::Arc | PathMember::Ellipse => {
                let (radii, rotation, angles) = match member {
                    PathMember::Arc => ((n(2), n(2)), 0.0, (n(3), n(4))),
                    _ => ((n(2), n(3)), n(4), (n(5), n(6))),
                };
                if radii.0 < 0.0 || radii.1 < 0.0 {
                    return Err(self.throw_value(dom_exception(
                        "IndexSizeError",
                        "The radius provided is negative.",
                    )));
                }
                PathOp::Arc((n(0), n(1)), radii, rotation, angles, counterclockwise)
            }
            PathMember::ArcTo => {
                if n(4) < 0.0 {
                    return Err(self.throw_value(dom_exception(
                        "IndexSizeError",
                        "The radius provided is negative.",
                    )));
                }
                PathOp::ArcTo((n(0), n(1)), (n(2), n(3)), n(4))
            }
            _ => {
                let Some(radii) = self.round_rect_radii("roundRect", arguments.get(4))? else {
                    return Ok(Value::Undefined);
                };
                PathOp::RoundRect((n(0), n(1), n(2), n(3)), radii)
            }
        };
        path.borrow_mut().push(op);
        Ok(Value::Undefined)
    }
}

fn is_primitive_value(value: &Value) -> bool {
    super::operations::is_primitive(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_replay_under_the_transform_where_they_are_drawn() {
        let document = crate::dom::Document::parse(
            r#"<html><body><canvas id="c" width="40" height="40"></canvas><p id="status"></p></body></html>"#,
        )
        .expect("document should parse");
        let document = Rc::new(RefCell::new(document));
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(
                r##"
                const c = document.getElementById("c").getContext("2d");
                const at = (x, y) => c.getImageData(x, y, 1, 1).data[3] > 0 ? "x" : ".";
                const seen = [];
                const square = new Path2D("M0 0 H10 V10 H0 Z");
                c.fillStyle = "#000";
                c.translate(20, 20);
                c.fill(square);
                c.resetTransform();
                seen.push(at(25, 25), at(15, 15), c.isPointInPath(square, 5, 5), c.isPointInPath(square, 25, 5));
                const copy = new Path2D(square);
                const moved = new Path2D();
                moved.addPath(copy, { e: 2, f: 30 });
                c.fill(moved);
                seen.push(at(5, 35), at(1, 35));
                c.clearRect(0, 0, 40, 40);
                c.beginPath();
                c.roundRect(0, 0, 20, 20, 8);
                c.fill();
                seen.push(at(0, 0), at(10, 10), at(19, 10), c.isPointInPath(1, 1), c.isPointInPath(10, 1));
                c.clearRect(0, 0, 40, 40);
                c.beginPath();
                c.moveTo(0, 30);
                c.arcTo(30, 30, 30, 0, 10);
                c.lineTo(30, 0);
                c.lineWidth = 2;
                c.stroke();
                seen.push(at(10, 30), at(29, 30), at(30, 15), c.isPointInStroke(10, 30), c.isPointInStroke(10, 20));
                const ring = new Path2D();
                ring.arc(20, 20, 10, 0, Math.PI * 2);
                ring.arc(20, 20, 5, 0, Math.PI * 2);
                seen.push(c.isPointInPath(ring, 20, 20), c.isPointInPath(ring, 20, 20, "evenodd"));
                try { ring.arc(0, 0, -1, 0, 1); } catch (error) { seen.push(error.name); }
                try { c.roundRect(0, 0, 5, 5, [1, 2, 3, 4, 5]); } catch (error) { seen.push(error.name); }
                document.getElementById("status").textContent = seen.join("|");
                "##,
            )
            .expect("script should run");
        let status = document.borrow().find_element_by_id("status").unwrap();
        assert_eq!(
            document.borrow().element_text_content(status).as_deref(),
            Some(
                "x|.|true|false|x|.|.|x|x|false|true|x|.|x|true|false|true|false|IndexSizeError|RangeError"
            )
        );
    }
}
