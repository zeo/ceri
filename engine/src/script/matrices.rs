// DOMMatrix and DOMMatrixReadOnly: a 4x4 matrix in the order CSS matrix3d() lists it, with
// whether it is still a 2D one, the transforms that make new ones, and the forms that change
// it in place

use super::object::{JsObject, ObjectClass, ObjectHandle, Property, Slot, new_object};
use super::operations::{is_primitive, plain_object};
use super::realm::{Builder, Native};
use super::*;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum MatrixMember {
    Get(usize),
    Set(usize),
    // a to f are names for six of the sixteen
    Is2D,
    IsIdentity,
    Translate(bool),
    Scale(bool),
    Scale3d(bool),
    Rotate(bool),
    RotateFromVector(bool),
    RotateAxisAngle(bool),
    SkewX(bool),
    SkewY(bool),
    Multiply(bool),
    PreMultiplySelf,
    FlipX,
    FlipY,
    Inverse(bool),
    SetMatrixValue,
    TransformPoint,
    ToFloat32Array,
    ToFloat64Array,
    ToJson,
    ToString,
    FromMatrix(bool),
    FromFloat32Array(bool),
    FromFloat64Array(bool),
    // DOMPointReadOnly.matrixTransform
    PointTransform,
}

pub(super) type Values = [f64; 16];

pub(super) struct MatrixState {
    pub(super) values: Values,
    pub(super) is_2d: bool,
}

const IDENTITY: Values = [
    1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
];

// the names of the sixteen in order, and where a to f sit among them
const NAMES: [&str; 16] = [
    "m11", "m12", "m13", "m14", "m21", "m22", "m23", "m24", "m31", "m32", "m33", "m34", "m41",
    "m42", "m43", "m44",
];
const SHORT: [(&str, usize); 6] = [("a", 0), ("b", 1), ("c", 4), ("d", 5), ("e", 12), ("f", 13)];
// the entries a 2D matrix keeps at their identity values
const THREE_D: [usize; 10] = [2, 3, 6, 7, 8, 9, 10, 11, 14, 15];

pub(super) struct MatrixInterfaces {
    read_only: ObjectHandle,
    matrix: ObjectHandle,
}

fn multiply(left: &Values, right: &Values) -> Values {
    let mut output = [0.0; 16];
    for column in 0..4 {
        for row in 0..4 {
            output[column * 4 + row] = (0..4)
                .map(|index| left[index * 4 + row] * right[column * 4 + index])
                .sum();
        }
    }
    output
}

fn invert(m: &Values) -> Option<Values> {
    let mut inverse = [0.0; 16];
    inverse[0] = m[5] * m[10] * m[15] - m[5] * m[11] * m[14] - m[9] * m[6] * m[15]
        + m[9] * m[7] * m[14]
        + m[13] * m[6] * m[11]
        - m[13] * m[7] * m[10];
    inverse[4] = -m[4] * m[10] * m[15] + m[4] * m[11] * m[14] + m[8] * m[6] * m[15]
        - m[8] * m[7] * m[14]
        - m[12] * m[6] * m[11]
        + m[12] * m[7] * m[10];
    inverse[8] = m[4] * m[9] * m[15] - m[4] * m[11] * m[13] - m[8] * m[5] * m[15]
        + m[8] * m[7] * m[13]
        + m[12] * m[5] * m[11]
        - m[12] * m[7] * m[9];
    inverse[12] = -m[4] * m[9] * m[14] + m[4] * m[10] * m[13] + m[8] * m[5] * m[14]
        - m[8] * m[6] * m[13]
        - m[12] * m[5] * m[10]
        + m[12] * m[6] * m[9];
    inverse[1] = -m[1] * m[10] * m[15] + m[1] * m[11] * m[14] + m[9] * m[2] * m[15]
        - m[9] * m[3] * m[14]
        - m[13] * m[2] * m[11]
        + m[13] * m[3] * m[10];
    inverse[5] = m[0] * m[10] * m[15] - m[0] * m[11] * m[14] - m[8] * m[2] * m[15]
        + m[8] * m[3] * m[14]
        + m[12] * m[2] * m[11]
        - m[12] * m[3] * m[10];
    inverse[9] = -m[0] * m[9] * m[15] + m[0] * m[11] * m[13] + m[8] * m[1] * m[15]
        - m[8] * m[3] * m[13]
        - m[12] * m[1] * m[11]
        + m[12] * m[3] * m[9];
    inverse[13] = m[0] * m[9] * m[14] - m[0] * m[10] * m[13] - m[8] * m[1] * m[14]
        + m[8] * m[2] * m[13]
        + m[12] * m[1] * m[10]
        - m[12] * m[2] * m[9];
    inverse[2] = m[1] * m[6] * m[15] - m[1] * m[7] * m[14] - m[5] * m[2] * m[15]
        + m[5] * m[3] * m[14]
        + m[13] * m[2] * m[7]
        - m[13] * m[3] * m[6];
    inverse[6] = -m[0] * m[6] * m[15] + m[0] * m[7] * m[14] + m[4] * m[2] * m[15]
        - m[4] * m[3] * m[14]
        - m[12] * m[2] * m[7]
        + m[12] * m[3] * m[6];
    inverse[10] = m[0] * m[5] * m[15] - m[0] * m[7] * m[13] - m[4] * m[1] * m[15]
        + m[4] * m[3] * m[13]
        + m[12] * m[1] * m[7]
        - m[12] * m[3] * m[5];
    inverse[14] = -m[0] * m[5] * m[14] + m[0] * m[6] * m[13] + m[4] * m[1] * m[14]
        - m[4] * m[2] * m[13]
        - m[12] * m[1] * m[6]
        + m[12] * m[2] * m[5];
    inverse[3] = -m[1] * m[6] * m[11] + m[1] * m[7] * m[10] + m[5] * m[2] * m[11]
        - m[5] * m[3] * m[10]
        - m[9] * m[2] * m[7]
        + m[9] * m[3] * m[6];
    inverse[7] = m[0] * m[6] * m[11] - m[0] * m[7] * m[10] - m[4] * m[2] * m[11]
        + m[4] * m[3] * m[10]
        + m[8] * m[2] * m[7]
        - m[8] * m[3] * m[6];
    inverse[11] = -m[0] * m[5] * m[11] + m[0] * m[7] * m[9] + m[4] * m[1] * m[11]
        - m[4] * m[3] * m[9]
        - m[8] * m[1] * m[7]
        + m[8] * m[3] * m[5];
    inverse[15] = m[0] * m[5] * m[10] - m[0] * m[6] * m[9] - m[4] * m[1] * m[10]
        + m[4] * m[2] * m[9]
        + m[8] * m[1] * m[6]
        - m[8] * m[2] * m[5];
    let determinant =
        m[0] * inverse[0] + m[1] * inverse[4] + m[2] * inverse[8] + m[3] * inverse[12];
    if determinant == 0.0 || !determinant.is_finite() {
        return None;
    }
    Some(inverse.map(|value| value / determinant))
}

fn translation(x: f64, y: f64, z: f64) -> Values {
    let mut values = IDENTITY;
    values[12] = x;
    values[13] = y;
    values[14] = z;
    values
}

fn scaling(x: f64, y: f64, z: f64) -> Values {
    let mut values = IDENTITY;
    values[0] = x;
    values[5] = y;
    values[10] = z;
    values
}

// a turn of the angle in degrees about the axis, as CSS rotate3d() makes it
fn rotation(x: f64, y: f64, z: f64, degrees: f64) -> Values {
    let length = (x * x + y * y + z * z).sqrt();
    if length == 0.0 {
        return IDENTITY;
    }
    let (x, y, z) = (x / length, y / length, z / length);
    let angle = degrees.to_radians();
    // a turn about one axis takes its sine and cosine straight, as browsers do
    let (sin, cos) = angle.sin_cos();
    let mut values = IDENTITY;
    match (x, y, z) {
        (0.0, 0.0, axis) => {
            let sin = sin * axis;
            values[0] = cos;
            values[1] = sin;
            values[4] = -sin;
            values[5] = cos;
            return values;
        }
        (0.0, axis, 0.0) => {
            let sin = sin * axis;
            values[0] = cos;
            values[2] = -sin;
            values[8] = sin;
            values[10] = cos;
            return values;
        }
        (axis, 0.0, 0.0) => {
            let sin = sin * axis;
            values[5] = cos;
            values[6] = sin;
            values[9] = -sin;
            values[10] = cos;
            return values;
        }
        _ => {}
    }
    let (sin, cos) = (angle / 2.0).sin_cos();
    let sc = sin * cos;
    let sq = sin * sin;
    values[0] = 1.0 - 2.0 * (y * y + z * z) * sq;
    values[1] = 2.0 * (x * y * sq + z * sc);
    values[2] = 2.0 * (x * z * sq - y * sc);
    values[4] = 2.0 * (x * y * sq - z * sc);
    values[5] = 1.0 - 2.0 * (x * x + z * z) * sq;
    values[6] = 2.0 * (y * z * sq + x * sc);
    values[8] = 2.0 * (x * z * sq + y * sc);
    values[9] = 2.0 * (y * z * sq - x * sc);
    values[10] = 1.0 - 2.0 * (x * x + y * y) * sq;
    values
}

fn skew(x_degrees: f64, y_degrees: f64) -> Values {
    let mut values = IDENTITY;
    values[4] = x_degrees.to_radians().tan();
    values[1] = y_degrees.to_radians().tan();
    values
}

// a number as script prints it, as DOMMatrix.toString writes its values
fn print(value: f64) -> String {
    super::operations::number_to_string(value)
}

pub(super) fn create(
    build: &Builder,
    object_prototype: &ObjectHandle,
    point_read_only: &ObjectHandle,
    globals: &mut Vec<(&'static str, Value)>,
) -> MatrixInterfaces {
    use MatrixMember as M;
    let accessor = |target: &ObjectHandle, name: &str, get: M, set: Option<M>| {
        let get = build.function(Native::Matrix(get), &format!("get {name}"), 0);
        let set = set.map(|set| {
            Value::Object(build.function(Native::Matrix(set), &format!("set {name}"), 1))
        });
        target.borrow_mut().define(
            name.to_string(),
            Property {
                slot: Slot::Accessor {
                    get: Some(Value::Object(get)),
                    set,
                },
                enumerable: true,
                writable: false,
                configurable: true,
            },
        );
    };
    let method = |target: &ObjectHandle, name: &str, member: M, length: u32| {
        let function = build.function(Native::Matrix(member), name, length);
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
    let read_only = new_object(JsObject::with_prototype(
        Some(object_prototype.clone()),
        ObjectClass::Ordinary,
    ));
    let matrix = new_object(JsObject::with_prototype(
        Some(read_only.clone()),
        ObjectClass::Ordinary,
    ));
    for (name, index) in SHORT
        .iter()
        .copied()
        .chain(NAMES.iter().copied().zip(0..16))
    {
        accessor(&read_only, name, M::Get(index), None);
        accessor(&matrix, name, M::Get(index), Some(M::Set(index)));
    }
    accessor(&read_only, "is2D", M::Is2D, None);
    accessor(&read_only, "isIdentity", M::IsIdentity, None);
    for (name, member, length) in [
        ("translate", M::Translate(false), 0),
        ("scale", M::Scale(false), 0),
        ("scaleNonUniform", M::Scale(false), 0),
        ("scale3d", M::Scale3d(false), 0),
        ("rotate", M::Rotate(false), 0),
        ("rotateFromVector", M::RotateFromVector(false), 0),
        ("rotateAxisAngle", M::RotateAxisAngle(false), 0),
        ("skewX", M::SkewX(false), 0),
        ("skewY", M::SkewY(false), 0),
        ("multiply", M::Multiply(false), 0),
        ("flipX", M::FlipX, 0),
        ("flipY", M::FlipY, 0),
        ("inverse", M::Inverse(false), 0),
        ("transformPoint", M::TransformPoint, 0),
        ("toFloat32Array", M::ToFloat32Array, 0),
        ("toFloat64Array", M::ToFloat64Array, 0),
        ("toJSON", M::ToJson, 0),
        ("toString", M::ToString, 0),
    ] {
        method(&read_only, name, member, length);
    }
    for (name, member, length) in [
        ("translateSelf", M::Translate(true), 0),
        ("scaleSelf", M::Scale(true), 0),
        ("scale3dSelf", M::Scale3d(true), 0),
        ("rotateSelf", M::Rotate(true), 0),
        ("rotateFromVectorSelf", M::RotateFromVector(true), 0),
        ("rotateAxisAngleSelf", M::RotateAxisAngle(true), 0),
        ("skewXSelf", M::SkewX(true), 0),
        ("skewYSelf", M::SkewY(true), 0),
        ("multiplySelf", M::Multiply(true), 0),
        ("preMultiplySelf", M::PreMultiplySelf, 0),
        ("invertSelf", M::Inverse(true), 0),
        ("setMatrixValue", M::SetMatrixValue, 1),
    ] {
        method(&matrix, name, member, length);
    }
    tag(&read_only, "DOMMatrixReadOnly");
    tag(&matrix, "DOMMatrix");
    method(point_read_only, "matrixTransform", M::PointTransform, 0);
    for (name, prototype, read_only_kind) in [
        ("DOMMatrixReadOnly", &read_only, true),
        ("DOMMatrix", &matrix, false),
    ] {
        let constructor = build.constructor(
            Native::MatrixConstructor(read_only_kind),
            name,
            0,
            prototype,
        );
        method(&constructor, "fromMatrix", M::FromMatrix(read_only_kind), 0);
        method(
            &constructor,
            "fromFloat32Array",
            M::FromFloat32Array(read_only_kind),
            1,
        );
        method(
            &constructor,
            "fromFloat64Array",
            M::FromFloat64Array(read_only_kind),
            1,
        );
        globals.push((name, Value::Object(constructor)));
    }
    MatrixInterfaces { read_only, matrix }
}

impl ScriptRuntime {
    pub(super) fn matrix_value(&self, values: Values, is_2d: bool, read_only: bool) -> Value {
        let prototype = if read_only {
            self.intrinsics.matrices.read_only.clone()
        } else {
            self.intrinsics.matrices.matrix.clone()
        };
        Value::Object(new_object(JsObject::with_prototype(
            Some(prototype),
            ObjectClass::Matrix(Rc::new(RefCell::new(MatrixState { values, is_2d }))),
        )))
    }

    // the six numbers of a 2D transform, as the canvas holds them, as a DOMMatrix
    pub(super) fn matrix_2d(&self, [a, b, c, d, e, f]: [f64; 6]) -> Value {
        let mut values = IDENTITY;
        values[0] = a;
        values[1] = b;
        values[4] = c;
        values[5] = d;
        values[12] = e;
        values[13] = f;
        self.matrix_value(values, true, false)
    }

    fn matrix_of(value: &Value) -> Option<Rc<RefCell<MatrixState>>> {
        match value {
            Value::Object(object) => match &object.borrow().class {
                ObjectClass::Matrix(state) => Some(state.clone()),
                _ => None,
            },
            _ => None,
        }
    }

    // a sequence of six or sixteen numbers, or a CSS transform list
    fn matrix_init(&mut self, init: Value) -> Result<(Values, bool), ScriptError> {
        if matches!(init, Value::Undefined) {
            return Ok((IDENTITY, true));
        }
        if let Value::String(text) = &init {
            let text = text.trim();
            if text.is_empty() || text == "none" {
                return Ok((IDENTITY, true));
            }
            let Some(ops) = crate::transform::parse_transform_list(text) else {
                return Err(self.throw_value(dom_exception(
                    "SyntaxError",
                    &format!("Failed to construct 'DOMMatrix': Failed to parse '{text}'."),
                )));
            };
            let matrix = ops
                .iter()
                .fold(crate::transform::Matrix::IDENTITY, |matrix, op| {
                    matrix.multiply(&op.matrix(0.0, 0.0, 16.0))
                });
            let values = matrix.0.map(f64::from);
            let is_2d = THREE_D
                .iter()
                .all(|&index| values[index] == IDENTITY[index]);
            return Ok((values, is_2d));
        }
        let numbers = self.iterate_to_vec(init)?;
        let mut values = Vec::with_capacity(numbers.len());
        for number in numbers {
            values.push(self.coerce_number(number)?);
        }
        self.matrix_from_list(&values)
    }

    fn matrix_from_list(&mut self, list: &[f64]) -> Result<(Values, bool), ScriptError> {
        match *list {
            [a, b, c, d, e, f] => {
                let mut values = IDENTITY;
                values[0] = a;
                values[1] = b;
                values[4] = c;
                values[5] = d;
                values[12] = e;
                values[13] = f;
                Ok((values, true))
            }
            _ if list.len() == 16 => {
                let mut values = [0.0; 16];
                values.copy_from_slice(list);
                Ok((values, false))
            }
            _ => Err(self.type_error(format!(
                "Failed to construct 'DOMMatrix': The sequence must contain 6 elements for a 2D matrix or 16 elements for a 3D matrix, not {}.",
                list.len()
            ))),
        }
    }

    // a DOMMatrixInit dictionary, its a to f and m11 to m44 agreeing where both are given
    fn matrix_from_dictionary(&mut self, init: &Value) -> Result<(Values, bool), ScriptError> {
        if let Some(state) = Self::matrix_of(init) {
            let state = state.borrow();
            return Ok((state.values, state.is_2d));
        }
        if matches!(init, Value::Undefined | Value::Null) {
            return Ok((IDENTITY, true));
        }
        if is_primitive(init) {
            return Err(self.type_error(
                "Failed to execute 'fromMatrix': parameter 1 is not of type 'DOMMatrixInit'.",
            ));
        }
        let mut values = IDENTITY;
        for (index, name) in NAMES.iter().enumerate() {
            let value = self.get_property(init, name)?;
            if !matches!(value, Value::Undefined) {
                values[index] = self.coerce_number(value)?;
            }
        }
        for (name, index) in SHORT {
            let value = self.get_property(init, name)?;
            if matches!(value, Value::Undefined) {
                continue;
            }
            let number = self.coerce_number(value)?;
            let named = self.get_property(init, NAMES[index])?;
            if !matches!(named, Value::Undefined)
                && values[index] != number
                && !(number.is_nan() && values[index].is_nan())
            {
                return Err(self.type_error(format!(
                    "Failed to execute 'fromMatrix': The '{name}' property should equal the '{}' property.",
                    NAMES[index]
                )));
            }
            values[index] = number;
        }
        let flat = THREE_D
            .iter()
            .all(|&index| values[index] == IDENTITY[index]);
        let is_2d = match self.get_property(init, "is2D")? {
            Value::Undefined => flat,
            value => {
                let wanted = value.truthy();
                if wanted && !flat {
                    return Err(self.type_error("Failed to execute 'fromMatrix': The is2D member is set to true but the input matrix is a 3D matrix."));
                }
                wanted
            }
        };
        Ok((values, is_2d))
    }

    pub(super) fn construct_matrix(
        &mut self,
        read_only: bool,
        arguments: Vec<Value>,
        new_target: Value,
    ) -> Result<Value, ScriptError> {
        let (values, is_2d) =
            self.matrix_init(arguments.first().cloned().unwrap_or(Value::Undefined))?;
        let made = self.matrix_value(values, is_2d, read_only);
        if let (Value::Object(object), Value::Object(_)) = (&made, &new_target) {
            let prototype = self.prototype_from_constructor(&new_target)?;
            object.borrow_mut().set_prototype(Some(prototype));
        }
        Ok(made)
    }

    fn matrix_numbers(
        &mut self,
        arguments: &[Value],
        defaults: &[f64],
    ) -> Result<Vec<f64>, ScriptError> {
        let mut numbers = Vec::with_capacity(defaults.len());
        for (index, default) in defaults.iter().enumerate() {
            numbers.push(match arguments.get(index) {
                None | Some(Value::Undefined) => *default,
                Some(value) => self.coerce_number(value.clone())?,
            });
        }
        Ok(numbers)
    }

    pub(super) fn call_matrix(
        &mut self,
        member: MatrixMember,
        this: Value,
        arguments: Vec<Value>,
    ) -> Result<Value, ScriptError> {
        use MatrixMember as M;
        let first = arguments.first().cloned().unwrap_or(Value::Undefined);
        match member {
            M::FromMatrix(read_only) => {
                let (values, is_2d) = self.matrix_from_dictionary(&first)?;
                return Ok(self.matrix_value(values, is_2d, read_only));
            }
            M::FromFloat32Array(read_only) | M::FromFloat64Array(read_only) => {
                if !matches!(first, Value::TypedArray(_)) {
                    return Err(
                        self.type_error("Failed to execute: parameter 1 is not a typed array.")
                    );
                }
                let numbers = self.iterate_to_vec(first)?;
                let mut values = Vec::with_capacity(numbers.len());
                for number in numbers {
                    values.push(self.coerce_number(number)?);
                }
                let (values, is_2d) = self.matrix_from_list(&values)?;
                return Ok(self.matrix_value(values, is_2d, read_only));
            }
            M::PointTransform => {
                let (values, _) = self.matrix_from_dictionary(&first)?;
                let point = self.point_numbers(&this)?;
                return Ok(self.point_value(self.transform_point(&values, point)));
            }
            _ => {}
        }
        let Some(state) = Self::matrix_of(&this) else {
            return Err(self.type_error("Illegal invocation"));
        };
        let (values, is_2d) = {
            let state = state.borrow();
            (state.values, state.is_2d)
        };
        // the transforms that make a new matrix, or change this one when they end in Self
        let changed: Option<(Values, bool, bool)> = match member {
            M::Translate(own) => {
                let n = self.matrix_numbers(&arguments, &[0.0, 0.0, 0.0])?;
                Some((
                    multiply(&values, &translation(n[0], n[1], n[2])),
                    is_2d && n[2] == 0.0,
                    own,
                ))
            }
            M::Scale(own) => {
                let n = self.matrix_numbers(&arguments, &[1.0, f64::NAN, 1.0, 0.0, 0.0, 0.0])?;
                let y = if n[1].is_nan() { n[0] } else { n[1] };
                let about = translation(n[3], n[4], n[5]);
                let back = translation(-n[3], -n[4], -n[5]);
                let scaled = multiply(&multiply(&about, &scaling(n[0], y, n[2])), &back);
                Some((
                    multiply(&values, &scaled),
                    is_2d && n[2] == 1.0 && n[5] == 0.0,
                    own,
                ))
            }
            M::Scale3d(own) => {
                let n = self.matrix_numbers(&arguments, &[1.0, 0.0, 0.0, 0.0])?;
                let about = translation(n[1], n[2], n[3]);
                let back = translation(-n[1], -n[2], -n[3]);
                let scaled = multiply(&multiply(&about, &scaling(n[0], n[0], n[0])), &back);
                Some((multiply(&values, &scaled), is_2d && n[0] == 1.0, own))
            }
            M::Rotate(own) => {
                let n = self.matrix_numbers(&arguments, &[0.0, f64::NAN, f64::NAN])?;
                let (x, y, z) = if n[1].is_nan() && n[2].is_nan() {
                    (0.0, 0.0, n[0])
                } else {
                    (
                        n[0],
                        if n[1].is_nan() { 0.0 } else { n[1] },
                        if n[2].is_nan() { 0.0 } else { n[2] },
                    )
                };
                let turned = multiply(
                    &multiply(&rotation(0.0, 0.0, 1.0, z), &rotation(0.0, 1.0, 0.0, y)),
                    &rotation(1.0, 0.0, 0.0, x),
                );
                Some((
                    multiply(&values, &turned),
                    is_2d && x == 0.0 && y == 0.0,
                    own,
                ))
            }
            M::RotateFromVector(own) => {
                let n = self.matrix_numbers(&arguments, &[0.0, 0.0])?;
                let degrees = if n[0] == 0.0 && n[1] == 0.0 {
                    0.0
                } else {
                    n[1].atan2(n[0]).to_degrees()
                };
                Some((
                    multiply(&values, &rotation(0.0, 0.0, 1.0, degrees)),
                    is_2d,
                    own,
                ))
            }
            M::RotateAxisAngle(own) => {
                let n = self.matrix_numbers(&arguments, &[0.0, 0.0, 0.0, 0.0])?;
                let flat = n[0] == 0.0 && n[1] == 0.0;
                Some((
                    multiply(&values, &rotation(n[0], n[1], n[2], n[3])),
                    is_2d && flat,
                    own,
                ))
            }
            M::SkewX(own) => {
                let n = self.matrix_numbers(&arguments, &[0.0])?;
                Some((multiply(&values, &skew(n[0], 0.0)), is_2d, own))
            }
            M::SkewY(own) => {
                let n = self.matrix_numbers(&arguments, &[0.0])?;
                Some((multiply(&values, &skew(0.0, n[0])), is_2d, own))
            }
            M::Multiply(own) => {
                let (other, other_2d) = self.matrix_from_dictionary(&first)?;
                Some((multiply(&values, &other), is_2d && other_2d, own))
            }
            M::PreMultiplySelf => {
                let (other, other_2d) = self.matrix_from_dictionary(&first)?;
                Some((multiply(&other, &values), is_2d && other_2d, true))
            }
            M::FlipX => Some((multiply(&values, &scaling(-1.0, 1.0, 1.0)), is_2d, false)),
            M::FlipY => Some((multiply(&values, &scaling(1.0, -1.0, 1.0)), is_2d, false)),
            M::Inverse(own) => Some(match invert(&values) {
                Some(inverse) => (inverse, is_2d, own),
                None => ([f64::NAN; 16], false, own),
            }),
            M::SetMatrixValue => {
                let (values, is_2d) = self.matrix_init(first.clone())?;
                Some((values, is_2d, true))
            }
            _ => None,
        };
        if let Some((values, is_2d, own)) = changed {
            if own {
                let mut state = state.borrow_mut();
                state.values = values;
                state.is_2d = is_2d;
                return Ok(this);
            }
            return Ok(self.matrix_value(values, is_2d, false));
        }
        Ok(match member {
            M::Get(index) => Value::Number(values[index]),
            M::Set(index) => {
                let number = self.coerce_number(first)?;
                let mut state = state.borrow_mut();
                state.values[index] = number;
                if THREE_D.contains(&index) && number != IDENTITY[index] {
                    state.is_2d = false;
                }
                Value::Undefined
            }
            M::Is2D => Value::Bool(is_2d),
            M::IsIdentity => Value::Bool(values == IDENTITY),
            M::TransformPoint => {
                let point = if is_primitive(&first) {
                    [0.0, 0.0, 0.0, 1.0]
                } else {
                    self.point_numbers(&first)?
                };
                self.point_value(self.transform_point(&values, point))
            }
            M::ToFloat32Array | M::ToFloat64Array => {
                let kind = if member == M::ToFloat32Array {
                    typed::ElementKind::Float32
                } else {
                    typed::ElementKind::Float64
                };
                let bytes = values
                    .iter()
                    .flat_map(|&value| kind.encode(value))
                    .collect::<Vec<u8>>();
                let length = bytes.len();
                typed::new_typed(kind, Rc::new(RefCell::new(bytes.into())), 0, length)
            }
            M::ToJson => {
                let mut entries: Vec<(String, Value)> = SHORT
                    .iter()
                    .map(|&(name, index)| (name.to_string(), Value::Number(values[index])))
                    .collect();
                entries.extend(
                    NAMES
                        .iter()
                        .zip(values)
                        .map(|(name, value)| (name.to_string(), Value::Number(value))),
                );
                entries.push(("is2D".to_string(), Value::Bool(is_2d)));
                entries.push(("isIdentity".to_string(), Value::Bool(values == IDENTITY)));
                plain_object(entries)
            }
            M::ToString => {
                if values.iter().any(|value| !value.is_finite()) {
                    return Err(self.throw_value(dom_exception(
                        "InvalidStateError",
                        "Failed to execute 'toString' on 'DOMMatrixReadOnly': Cannot be serialized with NaN or Infinity values.",
                    )));
                }
                let text = if is_2d {
                    let listed = SHORT
                        .iter()
                        .map(|&(_, index)| print(values[index]))
                        .collect::<Vec<_>>();
                    format!("matrix({})", listed.join(", "))
                } else {
                    format!(
                        "matrix3d({})",
                        values
                            .iter()
                            .map(|&value| print(value))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                };
                Value::String(text.into())
            }
            _ => Value::Undefined,
        })
    }

    fn point_numbers(&mut self, point: &Value) -> Result<[f64; 4], ScriptError> {
        let mut values = [0.0, 0.0, 0.0, 1.0];
        for (index, name) in ["x", "y", "z", "w"].into_iter().enumerate() {
            let value = self.get_property(point, name)?;
            if !matches!(value, Value::Undefined) {
                values[index] = self.coerce_number(value)?;
            }
        }
        Ok(values)
    }

    fn transform_point(&self, values: &Values, [x, y, z, w]: [f64; 4]) -> [f64; 4] {
        let row = |index: usize| {
            values[index] * x
                + values[4 + index] * y
                + values[8 + index] * z
                + values[12 + index] * w
        };
        [row(0), row(1), row(2), row(3)]
    }

    fn point_value(&mut self, [x, y, z, w]: [f64; 4]) -> Value {
        let constructor = self.lookup_global("DOMPoint").unwrap_or(Value::Undefined);
        self.construct(
            constructor,
            vec![
                Value::Number(x),
                Value::Number(y),
                Value::Number(z),
                Value::Number(w),
            ],
            None,
        )
        .unwrap_or(Value::Undefined)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matrices_compose_invert_and_print_as_browsers_do() {
        let document = crate::dom::Document::parse(
            r#"<html><body><canvas id="c" width="10" height="10"></canvas><p id="out"></p></body></html>"#,
        )
        .expect("document should parse");
        let document = Rc::new(RefCell::new(document));
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(
                r#"
                const seen = [];
                seen.push(String(new DOMMatrix()));
                seen.push(new DOMMatrix([1, 2, 3, 4, 5, 6]).multiply(new DOMMatrix([1, 0, 0, 1, 10, 20])).toString());
                seen.push(new DOMMatrix().translate(10, 20).scale(2).toString());
                seen.push(new DOMMatrix().rotate(90).toString());
                seen.push(new DOMMatrix("translate(10px, 20px) scale(2)").toString());
                seen.push(new DOMMatrix([2, 0, 0, 2, 10, 10]).inverse().toString());
                const turned = new DOMMatrix().rotate(0, 30, 0);
                seen.push(turned.is2D, new DOMMatrix().isIdentity, turned.m13.toFixed(3));
                const point = new DOMMatrix().translate(10, 20).transformPoint({ x: 1, y: 1 });
                seen.push(point.x, point.y, point instanceof DOMPoint);
                const own = new DOMMatrix();
                seen.push(own.translateSelf(3, 4) === own, own.e, own.f);
                own.a = 5;
                seen.push(own.m11, DOMMatrix.fromMatrix({ a: 2, d: 2 }).toString());
                const fixed = new DOMMatrixReadOnly([1, 0, 0, 1, 0, 0]);
                seen.push(typeof fixed.translateSelf, fixed instanceof DOMMatrixReadOnly, new DOMMatrix() instanceof DOMMatrixReadOnly);
                const c = document.getElementById("c").getContext("2d");
                c.translate(5, 6);
                const transform = c.getTransform();
                seen.push(transform instanceof DOMMatrix, transform.e, transform.f);
                c.setTransform(new DOMMatrix().scale(3));
                seen.push(c.getTransform().a, new DOMPoint(1, 2).matrixTransform(new DOMMatrix().translate(1, 1)).x);
                seen.push(new DOMMatrix().toFloat64Array().length, JSON.stringify(new DOMMatrix().toJSON()).includes('"is2D":true'));
                try { new DOMMatrix([1, 2, 3]); } catch (error) { seen.push(error.name); }
                try { new DOMMatrix("nonsense(1)"); } catch (error) { seen.push(error.name); }
                document.getElementById("out").textContent = seen.join("|");
                "#,
            )
            .expect("script should run");
        let out = document.borrow().find_element_by_id("out").unwrap();
        assert_eq!(
            document.borrow().element_text_content(out).as_deref(),
            Some(
                "matrix(1, 0, 0, 1, 0, 0)|matrix(1, 2, 3, 4, 75, 106)|matrix(2, 0, 0, 2, 10, 20)|matrix(6.123233995736766e-17, 1, -1, 6.123233995736766e-17, 0, 0)|matrix(2, 0, 0, 2, 10, 20)|matrix(0.5, 0, 0, 0.5, -5, -5)|false|true|-0.500|11|21|true|true|3|4|5|matrix(2, 0, 0, 2, 0, 0)|undefined|true|true|true|5|6|3|2|16|true|TypeError|SyntaxError"
            )
        );
    }
}
