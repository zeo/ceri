// the CSS Typed OM: CSSStyleValue with its numeric, keyword, unparsed, image and transform
// kinds, arithmetic on numeric values under the CSS type rules, CSS.px() and the other unit
// factories, and the property maps computedStyleMap() and attributeStyleMap hand out

use super::object::{JsObject, ObjectClass, ObjectHandle, Property, Slot, new_object};
use super::operations::is_primitive;
use super::realm::{Builder, Native};
use super::*;
use std::cell::Cell;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum StyleMember {
    // indices into CONSTRUCTIBLE and FACTORIES, which keep native functions, and so values, small
    Construct(u8),
    Parse(bool),
    NumericParse,
    Factory(u8),
    ToString,
    Add,
    Sub,
    Mul,
    Div,
    Min,
    Max,
    Equals,
    To,
    ToSum,
    Type,
    Value,
    SetValue,
    Unit,
    Operator,
    Values,
    Lower,
    Upper,
    Length,
    Iterate,
    Variable,
    SetVariable,
    Fallback,
    Is2D,
    SetIs2D,
    ToMatrix,
    Argument(u8),
    SetArgument(u8),
    Matrix,
    Get,
    GetAll,
    Has,
    Size,
    Entries,
    Keys,
    MapValues,
    ForEach,
    Set,
    Append,
    Delete,
    Clear,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum MathKind {
    Sum,
    Product,
    Negate,
    Invert,
    Min,
    Max,
    Clamp,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum ComponentKind {
    Translate,
    Rotate,
    Scale,
    Skew,
    SkewX,
    SkewY,
    Perspective,
}

pub(super) enum StyleValue {
    Unit(Cell<f64>, String),
    Math(MathKind, Vec<Rc<StyleValue>>),
    Keyword(RefCell<String>),
    Generic(String),
    Image(String),
    Unparsed(Vec<Segment>),
    Transform(Vec<Rc<StyleValue>>),
    Component(ComponentKind, RefCell<Vec<Rc<StyleValue>>>, Cell<bool>),
    // a matrix() or matrix3d() function, kept as the DOMMatrix it is
    Matrix(RefCell<Value>, Cell<bool>),
}

#[derive(Clone)]
pub(super) enum Segment {
    Text(String),
    Reference(Rc<Reference>),
}

pub(super) struct Reference {
    variable: RefCell<String>,
    fallback: RefCell<Option<Rc<StyleValue>>>,
}

pub(super) enum StyleOm {
    Value(Rc<StyleValue>),
    Reference(Rc<Reference>),
    Array(Vec<Rc<StyleValue>>),
    Map(NodeId, bool),
}

pub(super) struct StyleOmInterfaces {
    prototypes: HashMap<&'static str, ObjectHandle>,
}

// every unit with what it measures and its size in the canonical unit of that measure;
// a factor of zero marks a relative unit that converts to nothing else
const UNITS: &[(&str, usize, f64)] = &[
    ("px", 0, 1.0),
    ("cm", 0, 96.0 / 2.54),
    ("mm", 0, 96.0 / 25.4),
    ("q", 0, 96.0 / 101.6),
    ("in", 0, 96.0),
    ("pt", 0, 96.0 / 72.0),
    ("pc", 0, 16.0),
    ("em", 0, 0.0),
    ("rem", 0, 0.0),
    ("ex", 0, 0.0),
    ("rex", 0, 0.0),
    ("ch", 0, 0.0),
    ("rch", 0, 0.0),
    ("ic", 0, 0.0),
    ("ric", 0, 0.0),
    ("lh", 0, 0.0),
    ("rlh", 0, 0.0),
    ("cap", 0, 0.0),
    ("rcap", 0, 0.0),
    ("vw", 0, 0.0),
    ("vh", 0, 0.0),
    ("vi", 0, 0.0),
    ("vb", 0, 0.0),
    ("vmin", 0, 0.0),
    ("vmax", 0, 0.0),
    ("svw", 0, 0.0),
    ("svh", 0, 0.0),
    ("svi", 0, 0.0),
    ("svb", 0, 0.0),
    ("svmin", 0, 0.0),
    ("svmax", 0, 0.0),
    ("lvw", 0, 0.0),
    ("lvh", 0, 0.0),
    ("lvi", 0, 0.0),
    ("lvb", 0, 0.0),
    ("lvmin", 0, 0.0),
    ("lvmax", 0, 0.0),
    ("dvw", 0, 0.0),
    ("dvh", 0, 0.0),
    ("dvi", 0, 0.0),
    ("dvb", 0, 0.0),
    ("dvmin", 0, 0.0),
    ("dvmax", 0, 0.0),
    ("cqw", 0, 0.0),
    ("cqh", 0, 0.0),
    ("cqi", 0, 0.0),
    ("cqb", 0, 0.0),
    ("cqmin", 0, 0.0),
    ("cqmax", 0, 0.0),
    ("deg", 1, 1.0),
    ("rad", 1, 180.0 / std::f64::consts::PI),
    ("grad", 1, 0.9),
    ("turn", 1, 360.0),
    ("s", 2, 1.0),
    ("ms", 2, 0.001),
    ("hz", 3, 1.0),
    ("khz", 3, 1000.0),
    ("dppx", 4, 1.0),
    ("x", 4, 1.0),
    ("dpi", 4, 1.0 / 96.0),
    ("dpcm", 4, 2.54 / 96.0),
    ("fr", 5, 0.0),
];

const CANONICAL: [&str; 6] = ["px", "deg", "s", "hz", "dppx", "fr"];
const TYPE_NAMES: [&str; 7] = [
    "length",
    "angle",
    "time",
    "frequency",
    "resolution",
    "flex",
    "percent",
];
const PERCENT: usize = 6;

// the names CSS.<unit>() goes by, with the unit each makes
const FACTORIES: &[(&str, &str)] = &[
    ("number", "number"),
    ("percent", "percent"),
    ("em", "em"),
    ("rem", "rem"),
    ("ex", "ex"),
    ("rex", "rex"),
    ("ch", "ch"),
    ("rch", "rch"),
    ("ic", "ic"),
    ("ric", "ric"),
    ("lh", "lh"),
    ("rlh", "rlh"),
    ("cap", "cap"),
    ("rcap", "rcap"),
    ("vw", "vw"),
    ("vh", "vh"),
    ("vi", "vi"),
    ("vb", "vb"),
    ("vmin", "vmin"),
    ("vmax", "vmax"),
    ("svw", "svw"),
    ("svh", "svh"),
    ("svi", "svi"),
    ("svb", "svb"),
    ("svmin", "svmin"),
    ("svmax", "svmax"),
    ("lvw", "lvw"),
    ("lvh", "lvh"),
    ("lvi", "lvi"),
    ("lvb", "lvb"),
    ("lvmin", "lvmin"),
    ("lvmax", "lvmax"),
    ("dvw", "dvw"),
    ("dvh", "dvh"),
    ("dvi", "dvi"),
    ("dvb", "dvb"),
    ("dvmin", "dvmin"),
    ("dvmax", "dvmax"),
    ("cqw", "cqw"),
    ("cqh", "cqh"),
    ("cqi", "cqi"),
    ("cqb", "cqb"),
    ("cqmin", "cqmin"),
    ("cqmax", "cqmax"),
    ("cm", "cm"),
    ("mm", "mm"),
    ("Q", "q"),
    ("in", "in"),
    ("pt", "pt"),
    ("pc", "pc"),
    ("px", "px"),
    ("deg", "deg"),
    ("grad", "grad"),
    ("rad", "rad"),
    ("turn", "turn"),
    ("s", "s"),
    ("ms", "ms"),
    ("Hz", "hz"),
    ("kHz", "khz"),
    ("dpi", "dpi"),
    ("dpcm", "dpcm"),
    ("dppx", "dppx"),
    ("x", "x"),
    ("fr", "fr"),
];

// properties whose value is a comma-separated list, one style value per item
const LIST_VALUED: &[&str] = &[
    "transition-duration",
    "transition-delay",
    "transition-property",
    "transition-timing-function",
    "transition-behavior",
    "animation-duration",
    "animation-delay",
    "animation-name",
    "animation-timing-function",
    "animation-iteration-count",
    "animation-direction",
    "animation-fill-mode",
    "animation-play-state",
    "background-image",
    "background-repeat",
    "background-size",
    "background-position",
    "background-origin",
    "background-clip",
    "background-attachment",
    "mask-image",
    "will-change",
];

const IMAGE_PROPERTIES: &[&str] = &[
    "background-image",
    "list-style-image",
    "border-image-source",
    "mask-image",
];

const CONSTRUCTIBLE: &[&str] = &[
    "CSSUnitValue",
    "CSSMathSum",
    "CSSMathProduct",
    "CSSMathNegate",
    "CSSMathInvert",
    "CSSMathMin",
    "CSSMathMax",
    "CSSMathClamp",
    "CSSKeywordValue",
    "CSSUnparsedValue",
    "CSSVariableReferenceValue",
    "CSSTransformValue",
    "CSSTranslate",
    "CSSRotate",
    "CSSScale",
    "CSSSkew",
    "CSSSkewX",
    "CSSSkewY",
    "CSSPerspective",
    "CSSMatrixComponent",
];

const MATH_NAMES: [(MathKind, &str); 7] = [
    (MathKind::Sum, "CSSMathSum"),
    (MathKind::Product, "CSSMathProduct"),
    (MathKind::Negate, "CSSMathNegate"),
    (MathKind::Invert, "CSSMathInvert"),
    (MathKind::Min, "CSSMathMin"),
    (MathKind::Max, "CSSMathMax"),
    (MathKind::Clamp, "CSSMathClamp"),
];

const COMPONENT_NAMES: [(ComponentKind, &str); 7] = [
    (ComponentKind::Translate, "CSSTranslate"),
    (ComponentKind::Rotate, "CSSRotate"),
    (ComponentKind::Scale, "CSSScale"),
    (ComponentKind::Skew, "CSSSkew"),
    (ComponentKind::SkewX, "CSSSkewX"),
    (ComponentKind::SkewY, "CSSSkewY"),
    (ComponentKind::Perspective, "CSSPerspective"),
];

fn unit_of(unit: &str) -> Option<(usize, f64)> {
    UNITS
        .iter()
        .find(|(name, ..)| *name == unit)
        .map(|&(_, measure, factor)| (measure, factor))
}

fn valid_unit(unit: &str) -> bool {
    unit == "number" || unit == "percent" || unit_of(unit).is_some()
}

fn unit_suffix(unit: &str) -> &str {
    match unit {
        "number" => "",
        "percent" => "%",
        other => other,
    }
}

fn number(value: f64) -> Rc<StyleValue> {
    Rc::new(StyleValue::Unit(Cell::new(value), "number".into()))
}

fn unit(value: f64, unit: &str) -> Rc<StyleValue> {
    Rc::new(StyleValue::Unit(Cell::new(value), unit.into()))
}

fn math(kind: MathKind, values: Vec<Rc<StyleValue>>) -> Rc<StyleValue> {
    Rc::new(StyleValue::Math(kind, values))
}

// the exponent of each measure, with the measure a percentage stands for when one is known
#[derive(Clone, PartialEq, Default, Debug)]
struct NumericType {
    exponents: [i32; 7],
    hint: Option<usize>,
}

impl NumericType {
    fn of_unit(unit: &str) -> Self {
        let mut exponents = [0; 7];
        match unit {
            "number" => {}
            "percent" => exponents[PERCENT] = 1,
            other => exponents[unit_of(other).map_or(0, |(measure, _)| measure)] = 1,
        }
        Self {
            exponents,
            hint: None,
        }
    }

    fn with_hint(&self, hint: Option<usize>) -> Self {
        let mut exponents = self.exponents;
        if let Some(hint) = hint {
            exponents[hint] += exponents[PERCENT];
            exponents[PERCENT] = 0;
        }
        Self { exponents, hint }
    }

    fn single(&self) -> Option<usize> {
        let mut found = None;
        for (index, &exponent) in self.exponents.iter().enumerate() {
            match exponent {
                0 => {}
                1 if found.is_none() => found = Some(index),
                _ => return None,
            }
        }
        found
    }

    fn add(&self, other: &Self) -> Option<Self> {
        if let (Some(left), Some(right)) = (self.hint, other.hint)
            && left != right
        {
            return None;
        }
        let hint = self.hint.or(other.hint);
        let (left, right) = (self.with_hint(hint), other.with_hint(hint));
        if left.exponents == right.exponents {
            return Some(left);
        }
        // a percentage alone adds to a single other measure, standing for it
        for (percent, measure) in [(&left, &right), (&right, &left)] {
            if percent.single() == Some(PERCENT)
                && let Some(base) = measure.single().filter(|&base| base != PERCENT)
            {
                return Some(Self {
                    exponents: measure.exponents,
                    hint: Some(base),
                });
            }
        }
        None
    }

    fn multiply(&self, other: &Self) -> Option<Self> {
        if let (Some(left), Some(right)) = (self.hint, other.hint)
            && left != right
        {
            return None;
        }
        let hint = self.hint.or(other.hint);
        let (left, right) = (self.with_hint(hint), other.with_hint(hint));
        let mut exponents = left.exponents;
        for (exponent, other) in exponents.iter_mut().zip(right.exponents) {
            *exponent += other;
        }
        Some(Self { exponents, hint })
    }
}

fn numeric_type(value: &StyleValue) -> Option<NumericType> {
    match value {
        StyleValue::Unit(_, unit) => Some(NumericType::of_unit(unit)),
        StyleValue::Math(kind, values) => {
            let mut types = values.iter().map(|value| numeric_type(value));
            let first = types.next()??;
            match kind {
                MathKind::Negate => Some(first),
                MathKind::Invert => Some(NumericType {
                    exponents: first.exponents.map(|exponent| -exponent),
                    hint: first.hint,
                }),
                MathKind::Product => types.try_fold(first, |sum, next| sum.multiply(&next?)),
                _ => types.try_fold(first, |sum, next| sum.add(&next?)),
            }
        }
        _ => None,
    }
}

fn is_numeric(value: &StyleValue) -> bool {
    matches!(value, StyleValue::Unit(..) | StyleValue::Math(..))
}

fn serialize(value: &StyleValue) -> String {
    serialize_nested(value, false)
}

// a math value at the top is wrapped in calc(), one nested only in parentheses
fn serialize_nested(value: &StyleValue, nested: bool) -> String {
    match value {
        StyleValue::Unit(value, unit) => {
            format!("{}{}", geometry::css_number(value.get()), unit_suffix(unit))
        }
        StyleValue::Math(kind, values) => {
            let list = |values: &[Rc<StyleValue>]| {
                values
                    .iter()
                    .map(|value| serialize_nested(value, false))
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            let body = match kind {
                MathKind::Min => return format!("min({})", list(values)),
                MathKind::Max => return format!("max({})", list(values)),
                MathKind::Clamp => return format!("clamp({})", list(values)),
                MathKind::Sum => {
                    let mut text = serialize_nested(&values[0], true);
                    for value in &values[1..] {
                        match &**value {
                            StyleValue::Math(MathKind::Negate, inner) => {
                                text.push_str(" - ");
                                text.push_str(&serialize_nested(&inner[0], true));
                            }
                            other => {
                                text.push_str(" + ");
                                text.push_str(&serialize_nested(other, true));
                            }
                        }
                    }
                    text
                }
                MathKind::Product => {
                    let mut text = serialize_nested(&values[0], true);
                    for value in &values[1..] {
                        match &**value {
                            StyleValue::Math(MathKind::Invert, inner) => {
                                text.push_str(" / ");
                                text.push_str(&serialize_nested(&inner[0], true));
                            }
                            other => {
                                text.push_str(" * ");
                                text.push_str(&serialize_nested(other, true));
                            }
                        }
                    }
                    text
                }
                MathKind::Negate => format!("-{}", serialize_nested(&values[0], true)),
                MathKind::Invert => format!("1 / {}", serialize_nested(&values[0], true)),
            };
            if nested {
                format!("({body})")
            } else {
                format!("calc({body})")
            }
        }
        StyleValue::Keyword(keyword) => keyword.borrow().clone(),
        StyleValue::Generic(text) | StyleValue::Image(text) => text.clone(),
        StyleValue::Unparsed(segments) => segments
            .iter()
            .map(|segment| match segment {
                Segment::Text(text) => text.clone(),
                Segment::Reference(reference) => serialize_reference(reference),
            })
            .collect(),
        StyleValue::Transform(components) => components
            .iter()
            .map(|component| serialize(component))
            .collect::<Vec<_>>()
            .join(" "),
        StyleValue::Component(kind, arguments, flat) => {
            let arguments = arguments.borrow();
            let text = |index: usize| serialize(&arguments[index]);
            match (kind, flat.get()) {
                (ComponentKind::Translate, true) => format!("translate({}, {})", text(0), text(1)),
                (ComponentKind::Translate, false) => {
                    format!("translate3d({}, {}, {})", text(0), text(1), text(2))
                }
                (ComponentKind::Rotate, true) => format!("rotate({})", text(3)),
                (ComponentKind::Rotate, false) => {
                    format!(
                        "rotate3d({}, {}, {}, {})",
                        text(0),
                        text(1),
                        text(2),
                        text(3)
                    )
                }
                (ComponentKind::Scale, true) => format!("scale({}, {})", text(0), text(1)),
                (ComponentKind::Scale, false) => {
                    format!("scale3d({}, {}, {})", text(0), text(1), text(2))
                }
                (ComponentKind::Skew, _) => format!("skew({}, {})", text(0), text(1)),
                (ComponentKind::SkewX, _) => format!("skewX({})", text(0)),
                (ComponentKind::SkewY, _) => format!("skewY({})", text(0)),
                (ComponentKind::Perspective, _) => format!("perspective({})", text(0)),
            }
        }
        StyleValue::Matrix(..) => String::new(),
    }
}

fn serialize_reference(reference: &Reference) -> String {
    match &*reference.fallback.borrow() {
        Some(fallback) => format!(
            "var({},{})",
            reference.variable.borrow(),
            serialize(fallback)
        ),
        None => format!("var({})", reference.variable.borrow()),
    }
}

fn equal(left: &StyleValue, right: &StyleValue) -> bool {
    match (left, right) {
        (StyleValue::Unit(a, unit_a), StyleValue::Unit(b, unit_b)) => {
            a.get() == b.get() && unit_a == unit_b
        }
        (StyleValue::Math(kind_a, a), StyleValue::Math(kind_b, b)) => {
            kind_a == kind_b && a.len() == b.len() && a.iter().zip(b).all(|(a, b)| equal(a, b))
        }
        _ => false,
    }
}

// a sum value: terms with a value and the unit powers multiplying it
type SumValue = Vec<(f64, BTreeMap<String, i32>)>;

fn sum_value(value: &StyleValue) -> Option<SumValue> {
    let combine = |terms: SumValue| {
        let mut combined: SumValue = Vec::new();
        for (value, units) in terms {
            match combined.iter_mut().find(|(_, found)| *found == units) {
                Some(found) => found.0 += value,
                None => combined.push((value, units)),
            }
        }
        combined
    };
    match value {
        StyleValue::Unit(value, unit) => {
            let mut units = BTreeMap::new();
            let mut value = value.get();
            match unit.as_str() {
                "number" => {}
                "percent" => {
                    units.insert("percent".to_string(), 1);
                }
                other => {
                    let (measure, factor) = unit_of(other)?;
                    if factor > 0.0 {
                        value *= factor;
                        units.insert(CANONICAL[measure].to_string(), 1);
                    } else {
                        units.insert(other.to_string(), 1);
                    }
                }
            }
            Some(vec![(value, units)])
        }
        StyleValue::Math(MathKind::Sum, values) => {
            let mut terms = Vec::new();
            for value in values {
                terms.extend(sum_value(value)?);
            }
            Some(combine(terms))
        }
        StyleValue::Math(MathKind::Negate, values) => Some(
            sum_value(&values[0])?
                .into_iter()
                .map(|(value, units)| (-value, units))
                .collect(),
        ),
        StyleValue::Math(MathKind::Invert, values) => {
            let terms = sum_value(&values[0])?;
            let [(value, units)] = &terms[..] else {
                return None;
            };
            Some(vec![(
                1.0 / value,
                units
                    .iter()
                    .map(|(unit, power)| (unit.clone(), -power))
                    .collect(),
            )])
        }
        StyleValue::Math(MathKind::Product, values) => {
            let mut terms: SumValue = vec![(1.0, BTreeMap::new())];
            for value in values {
                let next = sum_value(value)?;
                let mut product = Vec::new();
                for (left, left_units) in &terms {
                    for (right, right_units) in &next {
                        let mut units = left_units.clone();
                        for (unit, power) in right_units {
                            *units.entry(unit.clone()).or_insert(0) += power;
                        }
                        units.retain(|_, power| *power != 0);
                        product.push((left * right, units));
                    }
                }
                terms = combine(product);
            }
            Some(terms)
        }
        StyleValue::Math(kind, values) => {
            let mut chosen: Option<(f64, BTreeMap<String, i32>)> = None;
            for value in values {
                let terms = sum_value(value)?;
                let [(value, units)] = &terms[..] else {
                    return None;
                };
                match &chosen {
                    Some((_, found)) if found != units => return None,
                    Some((best, _))
                        if (*kind == MathKind::Max && value <= best)
                            || (*kind != MathKind::Max && value >= best) => {}
                    _ => chosen = Some((*value, units.clone())),
                }
            }
            chosen.map(|term| vec![term])
        }
        _ => None,
    }
}

// a term of a sum value as a value in a unit, when its units allow that unit
fn term_in(term: &(f64, BTreeMap<String, i32>), target: &str) -> Option<f64> {
    let (value, units) = term;
    match target {
        "number" => units.is_empty().then_some(*value),
        "percent" => (units.len() == 1 && units.get("percent") == Some(&1)).then_some(*value),
        other => {
            let (measure, factor) = unit_of(other)?;
            let canonical = if factor > 0.0 {
                CANONICAL[measure]
            } else {
                other
            };
            (units.len() == 1 && units.get(canonical) == Some(&1))
                .then(|| if factor > 0.0 { value / factor } else { *value })
        }
    }
}

fn negate(value: &Rc<StyleValue>) -> Rc<StyleValue> {
    match &**value {
        StyleValue::Math(MathKind::Negate, inner) => inner[0].clone(),
        StyleValue::Unit(amount, name) => unit(-amount.get(), name),
        _ => math(MathKind::Negate, vec![value.clone()]),
    }
}

fn invert(value: &Rc<StyleValue>) -> Option<Rc<StyleValue>> {
    Some(match &**value {
        StyleValue::Math(MathKind::Invert, inner) => inner[0].clone(),
        StyleValue::Unit(amount, name) if name == "number" => {
            if amount.get() == 0.0 {
                return None;
            }
            number(1.0 / amount.get())
        }
        _ => math(MathKind::Invert, vec![value.clone()]),
    })
}

// a CSS math expression, parsed then folded the way calc() simplifies
struct MathParser<'a> {
    tokens: Vec<&'a str>,
    at: usize,
}

fn math_tokens(text: &str) -> Option<Vec<&str>> {
    let mut tokens = Vec::new();
    let bytes = text.as_bytes();
    let mut at = 0;
    while at < text.len() {
        let byte = bytes[at];
        if byte.is_ascii_whitespace() {
            at += 1;
            continue;
        }
        let start = at;
        let previous_operand = tokens
            .last()
            .is_some_and(|last: &&str| !matches!(*last, "+" | "-" | "*" | "/" | "(" | ","));
        let numeric_start = byte.is_ascii_digit()
            || (byte == b'.' && bytes.get(at + 1).is_some_and(u8::is_ascii_digit))
            || (matches!(byte, b'+' | b'-')
                && !previous_operand
                && bytes
                    .get(at + 1)
                    .is_some_and(|&next| next.is_ascii_digit() || next == b'.'));
        if numeric_start {
            at += 1;
            while at < text.len() {
                let next = bytes[at];
                let exponent = matches!(next, b'e' | b'E')
                    && bytes.get(at + 1).is_some_and(|&after| {
                        after.is_ascii_digit()
                            || (matches!(after, b'+' | b'-')
                                && bytes.get(at + 2).is_some_and(u8::is_ascii_digit))
                    });
                if next.is_ascii_digit() || next == b'.' {
                    at += 1;
                } else if exponent {
                    at += 2;
                } else {
                    break;
                }
            }
            while at < text.len() && (bytes[at].is_ascii_alphabetic() || bytes[at] == b'%') {
                at += 1;
            }
        } else if byte.is_ascii_alphabetic() {
            while at < text.len() && (bytes[at].is_ascii_alphanumeric() || bytes[at] == b'-') {
                at += 1;
            }
            if bytes.get(at) != Some(&b'(') {
                return None;
            }
            at += 1;
        } else if matches!(byte, b'+' | b'-' | b'*' | b'/' | b'(' | b')' | b',') {
            at += 1;
        } else {
            return None;
        }
        tokens.push(&text[start..at]);
    }
    Some(tokens)
}

// a single number, percentage or dimension
fn numeric_token(token: &str) -> Option<Rc<StyleValue>> {
    let split = token
        .char_indices()
        .rfind(|(_, character)| character.is_ascii_digit() || *character == '.')
        .map(|(index, character)| index + character.len_utf8())?;
    let (digits, suffix) = token.split_at(split);
    let digits = digits.strip_prefix('+').unwrap_or(digits);
    if digits.is_empty()
        || !digits.starts_with(|c: char| c.is_ascii_digit() || matches!(c, '-' | '.'))
    {
        return None;
    }
    let value: f64 = digits.parse().ok()?;
    let suffix = suffix.to_ascii_lowercase();
    match suffix.as_str() {
        "" => Some(number(value)),
        "%" => Some(unit(value, "percent")),
        name if unit_of(name).is_some() => Some(unit(value, name)),
        _ => None,
    }
}

impl MathParser<'_> {
    fn next(&mut self) -> Option<&str> {
        let token = self.tokens.get(self.at).copied();
        self.at += 1;
        token
    }

    fn peek(&self) -> Option<&str> {
        self.tokens.get(self.at).copied()
    }

    fn sum(&mut self) -> Option<Rc<StyleValue>> {
        let mut terms = vec![self.product()?];
        while let Some(operator @ ("+" | "-")) = self.peek() {
            let subtract = operator == "-";
            self.at += 1;
            let term = self.product()?;
            terms.push(if subtract { negate(&term) } else { term });
        }
        Some(if terms.len() == 1 {
            terms.remove(0)
        } else {
            simplify_sum(terms)?
        })
    }

    fn product(&mut self) -> Option<Rc<StyleValue>> {
        let mut factors = vec![self.value()?];
        while let Some(operator @ ("*" | "/")) = self.peek() {
            let divide = operator == "/";
            self.at += 1;
            let factor = self.value()?;
            factors.push(if divide { invert(&factor)? } else { factor });
        }
        Some(if factors.len() == 1 {
            factors.remove(0)
        } else {
            simplify_product(factors)?
        })
    }

    fn value(&mut self) -> Option<Rc<StyleValue>> {
        let token = self.next()?;
        match token.to_ascii_lowercase().as_str() {
            "(" | "calc(" => {
                let inner = self.sum()?;
                (self.next()? == ")").then_some(inner)
            }
            function @ ("min(" | "max(" | "clamp(") => {
                let mut arguments = vec![self.sum()?];
                while self.peek() == Some(",") {
                    self.at += 1;
                    arguments.push(self.sum()?);
                }
                if self.next()? != ")" {
                    return None;
                }
                let kind = match function {
                    "min(" => MathKind::Min,
                    "max(" => MathKind::Max,
                    _ if arguments.len() == 3 => MathKind::Clamp,
                    _ => return None,
                };
                Some(math(kind, arguments))
            }
            _ => numeric_token(token),
        }
    }
}

// a sum with nested sums flattened and the terms in one unit added up
fn simplify_sum(terms: Vec<Rc<StyleValue>>) -> Option<Rc<StyleValue>> {
    let mut flat: Vec<Rc<StyleValue>> = Vec::new();
    for term in terms {
        match &*term {
            StyleValue::Math(MathKind::Sum, inner) => flat.extend(inner.iter().cloned()),
            _ => flat.push(term),
        }
    }
    let mut combined: Vec<Rc<StyleValue>> = Vec::new();
    for term in flat {
        if let StyleValue::Unit(value, name) = &*term
            && let Some(StyleValue::Unit(found, _)) = combined
                .iter()
                .map(|existing| &**existing)
                .find(|existing| matches!(existing, StyleValue::Unit(_, other) if other == name))
        {
            found.set(found.get() + value.get());
            continue;
        }
        combined.push(match &*term {
            StyleValue::Unit(value, name) => unit(value.get(), name),
            _ => term,
        });
    }
    let summed = math(MathKind::Sum, combined.clone());
    numeric_type(&summed)?;
    Some(if combined.len() == 1 {
        combined.remove(0)
    } else {
        summed
    })
}

// a product with its numbers folded into the one dimension beside them, when there is one
fn simplify_product(factors: Vec<Rc<StyleValue>>) -> Option<Rc<StyleValue>> {
    let mut scale = 1.0;
    let mut rest = Vec::new();
    for factor in factors {
        match &*factor {
            StyleValue::Unit(value, name) if name == "number" => scale *= value.get(),
            _ => rest.push(factor),
        }
    }
    match &rest[..] {
        [] => Some(number(scale)),
        [only] => match &**only {
            StyleValue::Unit(value, name) => Some(unit(value.get() * scale, name)),
            _ if scale == 1.0 => Some(only.clone()),
            _ => {
                let product = math(MathKind::Product, vec![number(scale), only.clone()]);
                numeric_type(&product).map(|_| product)
            }
        },
        _ => {
            let mut factors = rest.clone();
            if scale != 1.0 {
                factors.insert(0, number(scale));
            }
            let product = math(MathKind::Product, factors);
            numeric_type(&product).map(|_| product)
        }
    }
}

// CSSNumericValue.parse: a number, percentage, dimension or math function
fn parse_numeric(text: &str) -> Option<Rc<StyleValue>> {
    let text = text.trim();
    if let Some(value) = numeric_token(text) {
        return Some(value);
    }
    let tokens = math_tokens(text)?;
    let first = tokens.first()?.to_ascii_lowercase();
    if !matches!(first.as_str(), "calc(" | "min(" | "max(" | "clamp(") {
        return None;
    }
    let mut parser = MathParser { tokens, at: 0 };
    let value = parser.value()?;
    if parser.at != parser.tokens.len() {
        return None;
    }
    numeric_type(&value)?;
    // a calc() always reifies as a sum, even of one term
    Some(match (first.as_str(), &*value) {
        ("calc(", StyleValue::Math(MathKind::Sum | MathKind::Product, _)) => value,
        ("calc(", _) => math(MathKind::Sum, vec![value]),
        _ => value,
    })
}

// splits at top-level commas or whitespace, leaving functions whole
fn split_top(text: &str, comma: bool) -> Vec<String> {
    let mut parts = Vec::new();
    let mut depth = 0;
    let mut quote = None;
    let mut current = String::new();
    for character in text.chars() {
        match (quote, character) {
            (Some(open), found) if found == open => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(character),
            (None, '(') => depth += 1,
            (None, ')') => depth -= 1,
            (None, ',') if comma && depth == 0 => {
                parts.push(std::mem::take(&mut current).trim().to_string());
                continue;
            }
            (None, found) if !comma && depth == 0 && found.is_whitespace() => {
                if !current.is_empty() {
                    parts.push(std::mem::take(&mut current));
                }
                continue;
            }
            _ => {}
        }
        current.push(character);
    }
    let current = current.trim().to_string();
    if !current.is_empty() || comma {
        parts.push(current);
    }
    parts
}

fn is_ident(text: &str) -> bool {
    let body = text.strip_prefix('-').unwrap_or(text);
    body.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_' || !c.is_ascii())
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || !c.is_ascii())
}

// a property whose identifiers name things rather than being keywords of its grammar
fn names_things(property: &str) -> bool {
    property.ends_with("color")
        || matches!(
            property,
            "fill"
                | "stroke"
                | "font-family"
                | "animation-name"
                | "transition-property"
                | "counter-reset"
                | "counter-increment"
                | "grid-area"
                | "view-transition-name"
        )
}

// an unparsed value: text with the var() references in it pulled out
fn parse_unparsed(text: &str) -> Vec<Segment> {
    let mut segments = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("var(") {
        let mut depth = 0;
        let mut end = None;
        for (index, character) in rest[start + 3..].char_indices() {
            match character {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(start + 3 + index);
                        break;
                    }
                }
                _ => {}
            }
        }
        let Some(end) = end else {
            break;
        };
        if start > 0 {
            segments.push(Segment::Text(rest[..start].to_string()));
        }
        let inner = &rest[start + 4..end];
        let (variable, fallback) = match inner.split_once(',') {
            Some((variable, fallback)) => (variable, Some(fallback)),
            None => (inner, None),
        };
        segments.push(Segment::Reference(Rc::new(Reference {
            variable: RefCell::new(variable.trim().to_string()),
            fallback: RefCell::new(
                fallback.map(|fallback| Rc::new(StyleValue::Unparsed(parse_unparsed(fallback)))),
            ),
        })));
        rest = &rest[end + 1..];
    }
    if !rest.is_empty() || segments.is_empty() {
        segments.push(Segment::Text(rest.to_string()));
    }
    segments
}

// a transform list as its components, when every function in it is one Typed OM knows
fn parse_transform(text: &str) -> Option<Rc<StyleValue>> {
    let functions = crate::transform::functions(text)?;
    let mut components = Vec::new();
    for (name, arguments) in functions {
        let values: Vec<Rc<StyleValue>> = arguments
            .iter()
            .map(|argument| numeric_token(argument.trim()).or_else(|| parse_numeric(argument)))
            .collect::<Option<_>>()?;
        let zero = |name: &str| unit(0.0, name);
        let (kind, arguments, flat) = match (name.to_ascii_lowercase().as_str(), &values[..]) {
            ("translate", [x]) => (
                ComponentKind::Translate,
                vec![x.clone(), zero("px"), zero("px")],
                true,
            ),
            ("translate", [x, y]) => (
                ComponentKind::Translate,
                vec![x.clone(), y.clone(), zero("px")],
                true,
            ),
            ("translatex", [x]) => (
                ComponentKind::Translate,
                vec![x.clone(), zero("px"), zero("px")],
                true,
            ),
            ("translatey", [y]) => (
                ComponentKind::Translate,
                vec![zero("px"), y.clone(), zero("px")],
                true,
            ),
            ("translatez", [z]) => (
                ComponentKind::Translate,
                vec![zero("px"), zero("px"), z.clone()],
                false,
            ),
            ("translate3d", [x, y, z]) => (
                ComponentKind::Translate,
                vec![x.clone(), y.clone(), z.clone()],
                false,
            ),
            ("rotate" | "rotatez", [angle]) => {
                let flat = name.eq_ignore_ascii_case("rotate");
                (
                    ComponentKind::Rotate,
                    vec![number(0.0), number(0.0), number(1.0), angle.clone()],
                    flat,
                )
            }
            ("rotatex", [angle]) => (
                ComponentKind::Rotate,
                vec![number(1.0), number(0.0), number(0.0), angle.clone()],
                false,
            ),
            ("rotatey", [angle]) => (
                ComponentKind::Rotate,
                vec![number(0.0), number(1.0), number(0.0), angle.clone()],
                false,
            ),
            ("rotate3d", [x, y, z, angle]) => (
                ComponentKind::Rotate,
                vec![x.clone(), y.clone(), z.clone(), angle.clone()],
                false,
            ),
            ("scale", [x]) => (
                ComponentKind::Scale,
                vec![x.clone(), x.clone(), number(1.0)],
                true,
            ),
            ("scale", [x, y]) => (
                ComponentKind::Scale,
                vec![x.clone(), y.clone(), number(1.0)],
                true,
            ),
            ("scalex", [x]) => (
                ComponentKind::Scale,
                vec![x.clone(), number(1.0), number(1.0)],
                true,
            ),
            ("scaley", [y]) => (
                ComponentKind::Scale,
                vec![number(1.0), y.clone(), number(1.0)],
                true,
            ),
            ("scalez", [z]) => (
                ComponentKind::Scale,
                vec![number(1.0), number(1.0), z.clone()],
                false,
            ),
            ("scale3d", [x, y, z]) => (
                ComponentKind::Scale,
                vec![x.clone(), y.clone(), z.clone()],
                false,
            ),
            ("skew", [x]) => (ComponentKind::Skew, vec![x.clone(), zero("deg")], true),
            ("skew", [x, y]) => (ComponentKind::Skew, vec![x.clone(), y.clone()], true),
            ("skewx", [x]) => (ComponentKind::SkewX, vec![x.clone()], true),
            ("skewy", [y]) => (ComponentKind::SkewY, vec![y.clone()], true),
            ("perspective", [length]) => (ComponentKind::Perspective, vec![length.clone()], false),
            _ => return None,
        };
        components.push(Rc::new(StyleValue::Component(
            kind,
            RefCell::new(arguments),
            Cell::new(flat),
        )));
    }
    (!components.is_empty()).then(|| Rc::new(StyleValue::Transform(components)))
}

// the style values a property's text reifies to, one per item of a list-valued property
fn reify(property: &str, text: &str) -> Vec<Rc<StyleValue>> {
    if property.starts_with("--") {
        return vec![Rc::new(StyleValue::Unparsed(parse_unparsed(text.trim())))];
    }
    if LIST_VALUED.contains(&property) {
        return split_top(text, true)
            .iter()
            .map(|item| reify_one(property, item))
            .collect();
    }
    vec![reify_one(property, text)]
}

fn reify_one(property: &str, text: &str) -> Rc<StyleValue> {
    let text = text.trim();
    if property == "transform"
        && !text.eq_ignore_ascii_case("none")
        && let Some(transform) = parse_transform(text)
    {
        return transform;
    }
    if IMAGE_PROPERTIES.contains(&property) && text.to_ascii_lowercase().starts_with("url(") {
        return Rc::new(StyleValue::Image(text.to_string()));
    }
    if let Some(numeric) = parse_numeric(text) {
        return numeric;
    }
    if is_ident(text) && !names_things(property) {
        return Rc::new(StyleValue::Keyword(RefCell::new(text.to_ascii_lowercase())));
    }
    Rc::new(StyleValue::Generic(text.to_string()))
}

pub(super) fn create(
    build: &Builder,
    object_prototype: &ObjectHandle,
    css: &ObjectHandle,
    globals: &mut Vec<(&'static str, Value)>,
) -> StyleOmInterfaces {
    use StyleMember as M;
    let mut prototypes: HashMap<&'static str, ObjectHandle> = HashMap::new();
    let method = |target: &ObjectHandle, name: &str, member: M, length: u32| {
        let function = build.function(Native::StyleOm(member), name, length);
        target.borrow_mut().define(
            name.to_string(),
            Property {
                enumerable: true,
                ..Property::data(Value::Object(function))
            },
        );
    };
    let accessor = |target: &ObjectHandle, name: &str, get: M, set: Option<M>| {
        let get = build.function(Native::StyleOm(get), &format!("get {name}"), 0);
        let set = set.map(|set| {
            Value::Object(build.function(Native::StyleOm(set), &format!("set {name}"), 1))
        });
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
    let mut class =
        |name: &'static str, parent: Option<&ObjectHandle>, length: u32, constructible: bool| {
            let prototype = new_object(JsObject::with_prototype(
                Some(parent.unwrap_or(object_prototype).clone()),
                ObjectClass::Ordinary,
            ));
            prototype.borrow_mut().define(
                "@@toStringTag".to_string(),
                Property {
                    writable: false,
                    ..Property::hidden(Value::String(name.into()))
                },
            );
            let native = if constructible {
                Native::StyleOm(M::Construct(
                    CONSTRUCTIBLE
                        .iter()
                        .position(|&found| found == name)
                        .expect("every constructible class is listed") as u8,
                ))
            } else {
                Native::IllegalConstructor
            };
            let constructor = build.constructor(native, name, length, &prototype);
            globals.push((name, Value::Object(constructor.clone())));
            prototypes.insert(name, prototype.clone());
            (prototype, constructor)
        };
    let (style, style_constructor) = class("CSSStyleValue", None, 0, false);
    method(&style, "toString", M::ToString, 0);
    method(&style_constructor, "parse", M::Parse(false), 2);
    method(&style_constructor, "parseAll", M::Parse(true), 2);
    let (numeric, numeric_constructor) = class("CSSNumericValue", Some(&style), 0, false);
    for (name, member, length) in [
        ("add", M::Add, 0),
        ("sub", M::Sub, 0),
        ("mul", M::Mul, 0),
        ("div", M::Div, 0),
        ("min", M::Min, 0),
        ("max", M::Max, 0),
        ("equals", M::Equals, 0),
        ("to", M::To, 1),
        ("toSum", M::ToSum, 0),
        ("type", M::Type, 0),
    ] {
        method(&numeric, name, member, length);
    }
    method(&numeric_constructor, "parse", M::NumericParse, 1);
    let (unit_value, _) = class("CSSUnitValue", Some(&numeric), 2, true);
    accessor(&unit_value, "value", M::Value, Some(M::SetValue));
    accessor(&unit_value, "unit", M::Unit, None);
    let (math_value, _) = class("CSSMathValue", Some(&numeric), 0, false);
    accessor(&math_value, "operator", M::Operator, None);
    for (kind, name) in MATH_NAMES {
        let (prototype, _) = class(name, Some(&math_value), 0, true);
        match kind {
            MathKind::Negate | MathKind::Invert => accessor(&prototype, "value", M::Value, None),
            MathKind::Clamp => {
                accessor(&prototype, "lower", M::Lower, None);
                accessor(&prototype, "value", M::Value, None);
                accessor(&prototype, "upper", M::Upper, None);
            }
            _ => accessor(&prototype, "values", M::Values, None),
        }
    }
    let iterable = |prototype: &ObjectHandle| {
        accessor(prototype, "length", M::Length, None);
        let iterate = build.function(Native::StyleOm(M::Iterate), "values", 0);
        prototype.borrow_mut().define(
            "@@iterator".to_string(),
            Property::hidden(Value::Object(iterate)),
        );
    };
    let (array, _) = class("CSSNumericArray", None, 0, false);
    iterable(&array);
    let (keyword, _) = class("CSSKeywordValue", Some(&style), 1, true);
    accessor(&keyword, "value", M::Value, Some(M::SetValue));
    let (unparsed, _) = class("CSSUnparsedValue", Some(&style), 1, true);
    iterable(&unparsed);
    let (reference, _) = class("CSSVariableReferenceValue", None, 1, true);
    accessor(&reference, "variable", M::Variable, Some(M::SetVariable));
    accessor(&reference, "fallback", M::Fallback, None);
    class("CSSImageValue", Some(&style), 0, false);
    let (transform, _) = class("CSSTransformValue", Some(&style), 1, true);
    iterable(&transform);
    accessor(&transform, "is2D", M::Is2D, None);
    method(&transform, "toMatrix", M::ToMatrix, 0);
    let (component, _) = class("CSSTransformComponent", None, 0, false);
    accessor(&component, "is2D", M::Is2D, Some(M::SetIs2D));
    method(&component, "toMatrix", M::ToMatrix, 0);
    method(&component, "toString", M::ToString, 0);
    for (kind, name) in COMPONENT_NAMES {
        let length = match kind {
            ComponentKind::Rotate
            | ComponentKind::Skew
            | ComponentKind::Translate
            | ComponentKind::Scale => 2,
            _ => 1,
        };
        let (prototype, _) = class(name, Some(&component), length, true);
        let names: &[&str] = match kind {
            ComponentKind::Translate | ComponentKind::Scale => &["x", "y", "z"],
            ComponentKind::Rotate => &["x", "y", "z", "angle"],
            ComponentKind::Skew => &["ax", "ay"],
            ComponentKind::SkewX => &["ax"],
            ComponentKind::SkewY => &["ay"],
            ComponentKind::Perspective => &["length"],
        };
        for (index, name) in names.iter().enumerate() {
            accessor(
                &prototype,
                name,
                M::Argument(index as u8),
                Some(M::SetArgument(index as u8)),
            );
        }
    }
    let (matrix, _) = class("CSSMatrixComponent", Some(&component), 1, true);
    accessor(&matrix, "matrix", M::Matrix, None);
    let (readonly, _) = class("StylePropertyMapReadOnly", None, 0, false);
    for (name, member, length) in [
        ("get", M::Get, 1),
        ("getAll", M::GetAll, 1),
        ("has", M::Has, 1),
        ("entries", M::Entries, 0),
        ("keys", M::Keys, 0),
        ("values", M::MapValues, 0),
        ("forEach", M::ForEach, 1),
    ] {
        method(&readonly, name, member, length);
    }
    accessor(&readonly, "size", M::Size, None);
    let entries = build.function(Native::StyleOm(M::Entries), "entries", 0);
    readonly.borrow_mut().define(
        "@@iterator".to_string(),
        Property::hidden(Value::Object(entries)),
    );
    let (map, _) = class("StylePropertyMap", Some(&readonly), 0, false);
    for (name, member, length) in [
        ("set", M::Set, 1),
        ("append", M::Append, 1),
        ("delete", M::Delete, 1),
        ("clear", M::Clear, 0),
    ] {
        method(&map, name, member, length);
    }
    for (index, (name, _)) in FACTORIES.iter().enumerate() {
        method(css, name, M::Factory(index as u8), 1);
    }
    StyleOmInterfaces { prototypes }
}

impl ScriptRuntime {
    fn style_om_of(value: &Value) -> Option<Rc<StyleOm>> {
        match value {
            Value::Object(object) => match &object.borrow().class {
                ObjectClass::StyleOm(typed) => Some(typed.clone()),
                _ => None,
            },
            _ => None,
        }
    }

    fn style_value_of(value: &Value) -> Option<Rc<StyleValue>> {
        match Self::style_om_of(value).as_deref() {
            Some(StyleOm::Value(value)) => Some(value.clone()),
            _ => None,
        }
    }

    fn style_om_object(&self, prototype: &str, typed: StyleOm) -> ObjectHandle {
        let prototype = self.intrinsics.typed_om.prototypes[prototype].clone();
        new_object(JsObject::with_prototype(
            Some(prototype),
            ObjectClass::StyleOm(Rc::new(typed)),
        ))
    }

    fn wrap_style(&self, value: Rc<StyleValue>) -> Value {
        let prototype = match &*value {
            StyleValue::Unit(..) => "CSSUnitValue",
            StyleValue::Math(kind, _) => {
                MATH_NAMES
                    .iter()
                    .find(|(found, _)| found == kind)
                    .expect("every math kind has a class")
                    .1
            }
            StyleValue::Keyword(_) => "CSSKeywordValue",
            StyleValue::Generic(_) => "CSSStyleValue",
            StyleValue::Image(_) => "CSSImageValue",
            StyleValue::Unparsed(_) => "CSSUnparsedValue",
            StyleValue::Transform(_) => "CSSTransformValue",
            StyleValue::Component(kind, ..) => {
                COMPONENT_NAMES
                    .iter()
                    .find(|(found, _)| found == kind)
                    .expect("every component has a class")
                    .1
            }
            StyleValue::Matrix(..) => "CSSMatrixComponent",
        };
        let indexed: Vec<Value> = match &*value {
            StyleValue::Unparsed(segments) => segments
                .iter()
                .map(|segment| self.segment_value(segment))
                .collect(),
            StyleValue::Transform(components) => components
                .iter()
                .map(|component| self.wrap_style(component.clone()))
                .collect(),
            _ => Vec::new(),
        };
        let object = self.style_om_object(prototype, StyleOm::Value(value));
        for (index, item) in indexed.into_iter().enumerate() {
            object
                .borrow_mut()
                .define(index.to_string(), Property::data(item));
        }
        Value::Object(object)
    }

    fn segment_value(&self, segment: &Segment) -> Value {
        match segment {
            Segment::Text(text) => Value::String(text.as_str().into()),
            Segment::Reference(reference) => Value::Object(self.style_om_object(
                "CSSVariableReferenceValue",
                StyleOm::Reference(reference.clone()),
            )),
        }
    }

    fn numeric_array(&self, values: &[Rc<StyleValue>]) -> Value {
        let items: Vec<Value> = values
            .iter()
            .map(|value| self.wrap_style(value.clone()))
            .collect();
        let object = self.style_om_object("CSSNumericArray", StyleOm::Array(values.to_vec()));
        for (index, item) in items.into_iter().enumerate() {
            object
                .borrow_mut()
                .define(index.to_string(), Property::data(item));
        }
        Value::Object(object)
    }

    // a CSSNumberish argument: a numeric value as it is, or a JS number as a plain number
    fn numberish(&mut self, value: Value, failed: &str) -> Result<Rc<StyleValue>, ScriptError> {
        if let Some(found) = Self::style_value_of(&value).filter(|found| is_numeric(found)) {
            return Ok(found);
        }
        if let Value::Number(amount) = value {
            return Ok(number(amount));
        }
        if !is_primitive(&value) {
            return Err(self.type_error(format!(
                "{failed}: The provided value is not of type '(double or CSSNumericValue)'."
            )));
        }
        Ok(number(self.coerce_number(value)?))
    }

    pub(super) fn construct_style_om(
        &mut self,
        index: u8,
        arguments: Vec<Value>,
        new_target: Value,
    ) -> Result<Value, ScriptError> {
        let name = CONSTRUCTIBLE[usize::from(index)];
        let failed = format!("Failed to construct '{name}'");
        if matches!(new_target, Value::Undefined) {
            return Err(self.type_error(format!("{failed}: Please use the 'new' operator, this DOM object constructor cannot be called as a function.")));
        }
        let argument = |index: usize| arguments.get(index).cloned().unwrap_or(Value::Undefined);
        let value = match name {
            "CSSUnitValue" => {
                let amount = self.coerce_number(argument(0))?;
                let name = self.coerce_string(argument(1))?.to_ascii_lowercase();
                let name = match name.as_str() {
                    "%" => "percent".to_string(),
                    _ => name,
                };
                if !valid_unit(&name) {
                    return Err(self.type_error(format!("{failed}: Invalid unit: {name}")));
                }
                unit(amount, &name)
            }
            "CSSKeywordValue" => {
                let keyword = self.coerce_string(argument(0))?;
                if keyword.is_empty() {
                    return Err(self.type_error(format!(
                        "{failed}: CSSKeywordValue does not support empty strings"
                    )));
                }
                Rc::new(StyleValue::Keyword(RefCell::new(keyword)))
            }
            "CSSUnparsedValue" => {
                let items = self.iterate_to_vec(argument(0))?;
                let mut segments = Vec::new();
                for item in items {
                    segments.push(match Self::style_om_of(&item).as_deref() {
                        Some(StyleOm::Reference(reference)) => {
                            Segment::Reference(reference.clone())
                        }
                        _ => Segment::Text(self.coerce_string(item)?),
                    });
                }
                Rc::new(StyleValue::Unparsed(segments))
            }
            "CSSVariableReferenceValue" => {
                let variable = self.coerce_string(argument(0))?;
                if !variable.starts_with("--") {
                    return Err(self.type_error(format!("{failed}: Invalid custom property name")));
                }
                let fallback = match argument(1) {
                    Value::Undefined | Value::Null => None,
                    other => match Self::style_value_of(&other) {
                        Some(found) if matches!(&*found, StyleValue::Unparsed(_)) => Some(found),
                        _ => {
                            return Err(self.type_error(format!(
                                "{failed}: parameter 2 is not of type 'CSSUnparsedValue'."
                            )));
                        }
                    },
                };
                let prototype = self.prototype_from_constructor(&new_target)?;
                return Ok(Value::Object(new_object(JsObject::with_prototype(
                    Some(prototype),
                    ObjectClass::StyleOm(Rc::new(StyleOm::Reference(Rc::new(Reference {
                        variable: RefCell::new(variable),
                        fallback: RefCell::new(fallback),
                    })))),
                ))));
            }
            "CSSMathSum" | "CSSMathProduct" | "CSSMathMin" | "CSSMathMax" | "CSSMathNegate"
            | "CSSMathInvert" | "CSSMathClamp" => {
                let kind = MATH_NAMES
                    .iter()
                    .find(|(_, found)| *found == name)
                    .expect("a math class")
                    .0;
                let mut values = Vec::new();
                for item in arguments.iter().cloned() {
                    values.push(self.numberish(item, &failed)?);
                }
                let wanted = match kind {
                    MathKind::Negate | MathKind::Invert => Some(1),
                    MathKind::Clamp => Some(3),
                    _ => None,
                };
                if wanted.is_some_and(|wanted| values.len() < wanted) || values.is_empty() {
                    return Err(self.type_error(format!("{failed}: Arguments can't be empty")));
                }
                if let Some(wanted) = wanted {
                    values.truncate(wanted);
                }
                let made = math(kind, values);
                if numeric_type(&made).is_none() {
                    return Err(self.type_error(format!("{failed}: Incompatible types")));
                }
                made
            }
            "CSSTransformValue" => {
                let items = self.iterate_to_vec(argument(0))?;
                let mut components = Vec::new();
                for item in items {
                    match Self::style_value_of(&item) {
                        Some(found)
                            if matches!(
                                &*found,
                                StyleValue::Component(..) | StyleValue::Matrix(..)
                            ) =>
                        {
                            components.push(found)
                        }
                        _ => return Err(self.type_error(format!(
                            "{failed}: The provided value is not of type 'CSSTransformComponent'."
                        ))),
                    }
                }
                if components.is_empty() {
                    return Err(self.type_error(format!(
                        "{failed}: CSSTransformValue must have at least one component"
                    )));
                }
                Rc::new(StyleValue::Transform(components))
            }
            "CSSMatrixComponent" => {
                let matrix = argument(0);
                let flat = self.get_property(&matrix, "is2D")?.truthy();
                Rc::new(StyleValue::Matrix(RefCell::new(matrix), Cell::new(flat)))
            }
            _ => {
                let kind = COMPONENT_NAMES
                    .iter()
                    .find(|(_, found)| *found == name)
                    .expect("a component class")
                    .0;
                let mut values = Vec::new();
                for item in arguments.iter().cloned() {
                    values.push(self.numberish(item, &failed)?);
                }
                let (wanted, flat) = match kind {
                    ComponentKind::Translate => (2, values.len() < 3),
                    ComponentKind::Scale => (2, values.len() < 3),
                    ComponentKind::Rotate => (1, values.len() < 4),
                    ComponentKind::Skew => (2, true),
                    ComponentKind::SkewX | ComponentKind::SkewY => (1, true),
                    ComponentKind::Perspective => (1, false),
                };
                if values.len() < wanted {
                    return Err(self.type_error(format!(
                        "{failed}: {wanted} arguments required, but only {} present.",
                        values.len()
                    )));
                }
                let values = match kind {
                    ComponentKind::Translate if values.len() == 2 => {
                        vec![values[0].clone(), values[1].clone(), unit(0.0, "px")]
                    }
                    ComponentKind::Scale if values.len() == 2 => {
                        vec![values[0].clone(), values[1].clone(), number(1.0)]
                    }
                    ComponentKind::Rotate if values.len() < 4 => {
                        vec![number(0.0), number(0.0), number(1.0), values[0].clone()]
                    }
                    _ => values,
                };
                Rc::new(StyleValue::Component(
                    kind,
                    RefCell::new(values),
                    Cell::new(flat),
                ))
            }
        };
        let object = self.wrap_style(value);
        let prototype = self.prototype_from_constructor(&new_target)?;
        if let Value::Object(object) = &object {
            object.borrow_mut().set_prototype(Some(prototype));
        }
        Ok(object)
    }

    fn valid_property(&mut self, node: Option<NodeId>, property: &str) -> bool {
        property.starts_with("--")
            || crate::css::is_supported_property(property)
            || node.is_some_and(|node| self.computed_values(node, true).contains_key(property))
    }

    // the property name a map method was given, lowercased unless custom, or the error
    fn map_property(
        &mut self,
        value: Value,
        node: NodeId,
        method: &str,
        interface: &str,
    ) -> Result<String, ScriptError> {
        let name = self.coerce_string(value)?;
        let property = if name.starts_with("--") {
            name.clone()
        } else {
            name.to_ascii_lowercase()
        };
        if !self.valid_property(Some(node), &property) {
            return Err(self.type_error(format!(
                "Failed to execute '{method}' on '{interface}': Invalid propertyName: {name}"
            )));
        }
        Ok(property)
    }

    // what a map holds: the computed values, or the declarations of the style attribute
    fn map_entries(&mut self, node: NodeId, computed: bool) -> Vec<(String, String)> {
        if computed {
            let values = self.computed_values(node, true);
            let mut entries: Vec<(String, String)> = values
                .iter()
                .filter(|(property, _)| !property.starts_with("--"))
                .map(|(property, value)| (property.clone(), value.clone()))
                .collect();
            entries.extend(
                values
                    .into_iter()
                    .filter(|(property, _)| property.starts_with("--")),
            );
            return entries;
        }
        let style = self
            .document
            .borrow()
            .element(node)
            .and_then(|element| element.attr("style").map(str::to_string))
            .unwrap_or_default();
        style
            .split(';')
            .filter_map(|declaration| {
                let (name, value) = declaration.split_once(':')?;
                let name = name.trim();
                let name = if name.starts_with("--") {
                    name.to_string()
                } else {
                    name.to_ascii_lowercase()
                };
                Some((name, value.trim().to_string()))
            })
            .filter(|(name, value)| !name.is_empty() && !value.is_empty())
            .collect()
    }

    pub(super) fn style_property_map(&self, node: NodeId, computed: bool) -> Value {
        let prototype = if computed {
            "StylePropertyMapReadOnly"
        } else {
            "StylePropertyMap"
        };
        Value::Object(self.style_om_object(prototype, StyleOm::Map(node, computed)))
    }

    // the text a value set into a property map becomes, or none when the property refuses it
    fn map_text(&mut self, property: &str, value: Value) -> Result<Option<String>, ScriptError> {
        let text = match Self::style_om_of(&value).as_deref() {
            Some(StyleOm::Value(found)) => serialize(found),
            Some(_) => return Ok(None),
            None => self.coerce_string(value)?,
        };
        let accepted = property.starts_with("--")
            || crate::css::supports_declaration(property, &text)
            || !crate::css::is_supported_property(property);
        Ok(accepted.then_some(text))
    }

    pub(super) fn call_style_om(
        &mut self,
        member: StyleMember,
        this: Value,
        arguments: Vec<Value>,
    ) -> Result<Value, ScriptError> {
        use StyleMember as M;
        let argument = |index: usize| arguments.get(index).cloned().unwrap_or(Value::Undefined);
        match member {
            M::Construct(index) => {
                return self.construct_style_om(index, arguments, Value::Undefined);
            }
            M::Factory(index) => {
                let amount = self.coerce_number(argument(0))?;
                return Ok(self.wrap_style(unit(amount, FACTORIES[usize::from(index)].1)));
            }
            M::NumericParse => {
                let text = self.coerce_string(argument(0))?;
                return match parse_numeric(&text) {
                    Some(value) => Ok(self.wrap_style(value)),
                    None => Err(self.throw_value(dom_exception(
                        "SyntaxError",
                        "Failed to execute 'parse' on 'CSSNumericValue': Invalid math expression",
                    ))),
                };
            }
            M::Parse(all) => {
                let method = if all { "parseAll" } else { "parse" };
                let failed = format!("Failed to execute '{method}' on 'CSSStyleValue'");
                let name = self.coerce_string(argument(0))?;
                let text = self.coerce_string(argument(1))?;
                let property = if name.starts_with("--") {
                    name.clone()
                } else {
                    name.to_ascii_lowercase()
                };
                if !self.valid_property(None, &property) {
                    return Err(self.type_error(format!("{failed}: Invalid property name")));
                }
                if !property.starts_with("--")
                    && !crate::css::supports_declaration(&property, text.trim())
                {
                    return Err(self.type_error(format!(
                        "{failed}: The value provided ('{text}') could not be parsed as a '{name}'."
                    )));
                }
                let values = reify(&property, &text);
                return Ok(if all {
                    let items = values
                        .into_iter()
                        .map(|value| self.wrap_style(value))
                        .collect();
                    Self::new_array(items)
                } else {
                    self.wrap_style(values[0].clone())
                });
            }
            _ => {}
        }
        let Some(typed) = Self::style_om_of(&this) else {
            return Err(self.type_error("Illegal invocation"));
        };
        match &*typed {
            StyleOm::Map(node, computed) => {
                self.call_style_map(member, *node, *computed, arguments)
            }
            StyleOm::Reference(reference) => match member {
                M::Variable => Ok(Value::String(reference.variable.borrow().as_str().into())),
                M::SetVariable => {
                    let variable = self.coerce_string(argument(0))?;
                    if !variable.starts_with("--") {
                        return Err(self.type_error("Failed to set the 'variable' property on 'CSSVariableReferenceValue': Invalid custom property name"));
                    }
                    *reference.variable.borrow_mut() = variable;
                    Ok(Value::Undefined)
                }
                M::Fallback => Ok(reference
                    .fallback
                    .borrow()
                    .clone()
                    .map_or(Value::Null, |fallback| self.wrap_style(fallback))),
                _ => Err(self.type_error("Illegal invocation")),
            },
            StyleOm::Array(values) => match member {
                M::Length => Ok(Value::Number(values.len() as f64)),
                M::Iterate => {
                    let items = values
                        .iter()
                        .map(|value| self.wrap_style(value.clone()))
                        .collect();
                    self.array_iterator(Self::new_array(items))
                }
                _ => Err(self.type_error("Illegal invocation")),
            },
            StyleOm::Value(value) => self.call_style_value(member, value.clone(), arguments),
        }
    }

    fn array_iterator(&mut self, array: Value) -> Result<Value, ScriptError> {
        let values = self.get_property(&array, "values")?;
        self.call_function(values, array, Vec::new())
    }

    fn call_style_value(
        &mut self,
        member: StyleMember,
        value: Rc<StyleValue>,
        arguments: Vec<Value>,
    ) -> Result<Value, ScriptError> {
        use StyleMember as M;
        let argument = |index: usize| arguments.get(index).cloned().unwrap_or(Value::Undefined);
        let failed = |method: &str| format!("Failed to execute '{method}' on 'CSSNumericValue'");
        let numeric = is_numeric(&value);
        match member {
            M::ToString => Ok(Value::String(serialize(&value).into())),
            M::Add | M::Sub | M::Min | M::Max | M::Mul | M::Div if numeric => {
                let name = match member {
                    M::Add => "add",
                    M::Sub => "sub",
                    M::Min => "min",
                    M::Max => "max",
                    M::Mul => "mul",
                    _ => "div",
                };
                let mut others = Vec::new();
                for item in arguments {
                    let item = self.numberish(item, &failed(name))?;
                    others.push(match member {
                        M::Sub => negate(&item),
                        M::Div => match invert(&item) {
                            Some(inverted) => inverted,
                            None => {
                                return Err(self.range_error(format!(
                                    "{}: Can't divide-by-zero",
                                    failed(name)
                                )));
                            }
                        },
                        _ => item,
                    });
                }
                let (kind, flatten) = match member {
                    M::Add | M::Sub => (MathKind::Sum, MathKind::Sum),
                    M::Mul | M::Div => (MathKind::Product, MathKind::Product),
                    M::Min => (MathKind::Min, MathKind::Min),
                    _ => (MathKind::Max, MathKind::Max),
                };
                let mut values = match &*value {
                    StyleValue::Math(found, inner) if *found == flatten => inner.clone(),
                    _ => vec![value.clone()],
                };
                values.extend(others);
                let units: Option<Vec<(f64, &str)>> = values
                    .iter()
                    .map(|value| match &**value {
                        StyleValue::Unit(amount, name) => Some((amount.get(), name.as_str())),
                        _ => None,
                    })
                    .collect();
                if let Some(units) = units {
                    let same = units.iter().all(|(_, name)| *name == units[0].1);
                    match kind {
                        MathKind::Sum if same => {
                            return Ok(self.wrap_style(unit(
                                units.iter().map(|(amount, _)| amount).sum(),
                                units[0].1,
                            )));
                        }
                        MathKind::Min if same => {
                            let least = units
                                .iter()
                                .map(|(amount, _)| *amount)
                                .fold(f64::INFINITY, f64::min);
                            return Ok(self.wrap_style(unit(least, units[0].1)));
                        }
                        MathKind::Max if same => {
                            let most = units
                                .iter()
                                .map(|(amount, _)| *amount)
                                .fold(f64::NEG_INFINITY, f64::max);
                            return Ok(self.wrap_style(unit(most, units[0].1)));
                        }
                        MathKind::Product => {
                            let dimensions: Vec<&str> = units
                                .iter()
                                .map(|(_, name)| *name)
                                .filter(|name| *name != "number")
                                .collect();
                            if dimensions.len() <= 1 {
                                let product = units.iter().map(|(amount, _)| amount).product();
                                return Ok(self.wrap_style(unit(
                                    product,
                                    dimensions.first().copied().unwrap_or("number"),
                                )));
                            }
                        }
                        _ => {}
                    }
                }
                let made = math(kind, values);
                if numeric_type(&made).is_none() {
                    return Err(self.type_error(format!("{}: Incompatible types", failed(name))));
                }
                Ok(self.wrap_style(made))
            }
            M::Equals if numeric => {
                let mut all = true;
                for item in arguments {
                    let item = self.numberish(item, &failed("equals"))?;
                    all &= equal(&value, &item);
                }
                Ok(Value::Bool(all))
            }
            M::To if numeric => {
                let target = self.coerce_string(argument(0))?.to_ascii_lowercase();
                let target = if target == "%" {
                    "percent".to_string()
                } else {
                    target
                };
                if !valid_unit(&target) {
                    return Err(self.throw_value(dom_exception(
                        "SyntaxError",
                        &format!("{}: Invalid unit for conversion", failed("to")),
                    )));
                }
                let converted = sum_value(&value)
                    .filter(|terms| terms.len() == 1)
                    .and_then(|terms| term_in(&terms[0], &target));
                match converted {
                    Some(amount) => Ok(self.wrap_style(unit(amount, &target))),
                    None => {
                        Err(self
                            .type_error(format!("{}: Cannot convert to {target}", failed("to"))))
                    }
                }
            }
            M::ToSum if numeric => {
                let mut targets = Vec::new();
                for item in arguments {
                    let target = self.coerce_string(item)?.to_ascii_lowercase();
                    if !valid_unit(&target) {
                        return Err(self.throw_value(dom_exception(
                            "SyntaxError",
                            &format!("{}: Invalid unit for conversion", failed("toSum")),
                        )));
                    }
                    targets.push(target);
                }
                let Some(mut terms) = sum_value(&value) else {
                    return Err(
                        self.type_error(format!("{}: Cannot convert to a sum", failed("toSum")))
                    );
                };
                let mut values = Vec::new();
                if targets.is_empty() {
                    terms.sort_by(|a, b| a.1.keys().cmp(b.1.keys()));
                    for (amount, units) in &terms {
                        let name = match units.iter().collect::<Vec<_>>()[..] {
                            [] => "number".to_string(),
                            [(name, 1)] => name.clone(),
                            _ => {
                                return Err(self.type_error(format!(
                                    "{}: Cannot convert to a sum",
                                    failed("toSum")
                                )));
                            }
                        };
                        values.push(unit(*amount, &name));
                    }
                } else {
                    for target in &targets {
                        let mut total = 0.0;
                        terms.retain(|term| match term_in(term, target) {
                            Some(amount) => {
                                total += amount;
                                false
                            }
                            None => true,
                        });
                        values.push(unit(total, target));
                    }
                    if !terms.is_empty() {
                        return Err(self.type_error(format!(
                            "{}: There were leftover terms that were not converted",
                            failed("toSum")
                        )));
                    }
                }
                Ok(self.wrap_style(math(MathKind::Sum, values)))
            }
            M::Type if numeric => {
                let found = numeric_type(&value).unwrap_or_default();
                let mut entries = Vec::new();
                for (index, name) in TYPE_NAMES.iter().enumerate() {
                    if found.exponents[index] != 0 {
                        entries.push((
                            name.to_string(),
                            Value::Number(f64::from(found.exponents[index])),
                        ));
                    }
                }
                if let Some(hint) = found.hint {
                    entries.push((
                        "percentHint".to_string(),
                        Value::String(TYPE_NAMES[hint].into()),
                    ));
                }
                Ok(self.object_from_entries(entries))
            }
            M::Value => Ok(match &*value {
                StyleValue::Unit(amount, _) => Value::Number(amount.get()),
                StyleValue::Keyword(keyword) => Value::String(keyword.borrow().as_str().into()),
                StyleValue::Math(MathKind::Clamp, values) => self.wrap_style(values[1].clone()),
                StyleValue::Math(_, values) => self.wrap_style(values[0].clone()),
                _ => Value::Undefined,
            }),
            M::SetValue => {
                match &*value {
                    StyleValue::Unit(amount, _) => amount.set(self.coerce_number(argument(0))?),
                    StyleValue::Keyword(keyword) => {
                        let text = self.coerce_string(argument(0))?;
                        if text.is_empty() {
                            return Err(self.type_error("Failed to set the 'value' property on 'CSSKeywordValue': CSSKeywordValue does not support empty strings"));
                        }
                        *keyword.borrow_mut() = text;
                    }
                    _ => {}
                }
                Ok(Value::Undefined)
            }
            M::Unit => Ok(match &*value {
                StyleValue::Unit(_, name) => Value::String(name.as_str().into()),
                _ => Value::Undefined,
            }),
            M::Operator => Ok(match &*value {
                StyleValue::Math(kind, _) => Value::String(
                    match kind {
                        MathKind::Sum => "sum",
                        MathKind::Product => "product",
                        MathKind::Negate => "negate",
                        MathKind::Invert => "invert",
                        MathKind::Min => "min",
                        MathKind::Max => "max",
                        MathKind::Clamp => "clamp",
                    }
                    .into(),
                ),
                _ => Value::Undefined,
            }),
            M::Values => Ok(match &*value {
                StyleValue::Math(_, values) => self.numeric_array(values),
                _ => Value::Undefined,
            }),
            M::Lower | M::Upper => Ok(match &*value {
                StyleValue::Math(MathKind::Clamp, values) => {
                    self.wrap_style(values[if member == M::Lower { 0 } else { 2 }].clone())
                }
                _ => Value::Undefined,
            }),
            M::Length => Ok(Value::Number(match &*value {
                StyleValue::Unparsed(segments) => segments.len() as f64,
                StyleValue::Transform(components) => components.len() as f64,
                _ => 0.0,
            })),
            M::Iterate => {
                let items = match &*value {
                    StyleValue::Unparsed(segments) => segments
                        .iter()
                        .map(|segment| self.segment_value(segment))
                        .collect(),
                    StyleValue::Transform(components) => components
                        .iter()
                        .map(|component| self.wrap_style(component.clone()))
                        .collect(),
                    _ => Vec::new(),
                };
                self.array_iterator(Self::new_array(items))
            }
            M::Is2D => Ok(Value::Bool(match &*value {
                StyleValue::Transform(components) => {
                    components.iter().all(|component| match &**component {
                        StyleValue::Component(_, _, flat) | StyleValue::Matrix(_, flat) => {
                            flat.get()
                        }
                        _ => true,
                    })
                }
                StyleValue::Component(_, _, flat) | StyleValue::Matrix(_, flat) => flat.get(),
                _ => false,
            })),
            M::SetIs2D => {
                if let StyleValue::Component(_, _, flat) | StyleValue::Matrix(_, flat) = &*value {
                    flat.set(argument(0).truthy());
                }
                Ok(Value::Undefined)
            }
            M::Argument(index) => Ok(match &*value {
                StyleValue::Component(_, arguments, _) => arguments
                    .borrow()
                    .get(usize::from(index))
                    .cloned()
                    .map_or(Value::Undefined, |found| self.wrap_style(found)),
                _ => Value::Undefined,
            }),
            M::SetArgument(index) => {
                if let StyleValue::Component(_, arguments, _) = &*value {
                    let replacement =
                        self.numberish(argument(0), "Failed to set a transform component")?;
                    if let Some(slot) = arguments.borrow_mut().get_mut(usize::from(index)) {
                        *slot = replacement;
                    }
                }
                Ok(Value::Undefined)
            }
            M::Matrix => Ok(match &*value {
                StyleValue::Matrix(matrix, _) => matrix.borrow().clone(),
                _ => Value::Undefined,
            }),
            M::ToMatrix => {
                let text = match &*value {
                    StyleValue::Matrix(matrix, _) => {
                        let matrix = matrix.borrow().clone();
                        let text = self.get_property(&matrix, "toString")?;
                        let text = self.call_function(text, matrix, Vec::new())?;
                        self.coerce_string(text)?
                    }
                    StyleValue::Transform(components) => {
                        let mut parts = Vec::new();
                        for component in components {
                            parts.push(match &**component {
                                StyleValue::Matrix(matrix, _) => {
                                    let matrix = matrix.borrow().clone();
                                    let text = self.get_property(&matrix, "toString")?;
                                    let text = self.call_function(text, matrix, Vec::new())?;
                                    self.coerce_string(text)?
                                }
                                other => serialize(other),
                            });
                        }
                        parts.join(" ")
                    }
                    other => serialize(other),
                };
                let constructor = self.get_property(&Value::Window, "DOMMatrix")?;
                match self.construct(constructor, vec![Value::String(text.into())], None) {
                    Ok(matrix) => Ok(matrix),
                    Err(error) => {
                        self.take_thrown(error);
                        Err(self.type_error("Failed to execute 'toMatrix': The transform has relative lengths and cannot be turned into a matrix"))
                    }
                }
            }
            _ => Err(self.type_error("Illegal invocation")),
        }
    }

    fn call_style_map(
        &mut self,
        member: StyleMember,
        node: NodeId,
        computed: bool,
        arguments: Vec<Value>,
    ) -> Result<Value, ScriptError> {
        use StyleMember as M;
        let argument = |index: usize| arguments.get(index).cloned().unwrap_or(Value::Undefined);
        let interface = if computed {
            "StylePropertyMapReadOnly"
        } else {
            "StylePropertyMap"
        };
        match member {
            M::Get | M::GetAll | M::Has => {
                let method = match member {
                    M::Get => "get",
                    M::GetAll => "getAll",
                    _ => "has",
                };
                let property = self.map_property(argument(0), node, method, interface)?;
                let found = self
                    .map_entries(node, computed)
                    .into_iter()
                    .find(|(name, _)| *name == property)
                    .map(|(_, value)| reify(&property, &value));
                Ok(match (member, found) {
                    (M::Has, found) => Value::Bool(found.is_some()),
                    (M::Get, Some(values)) => self.wrap_style(values[0].clone()),
                    (M::Get, None) => Value::Undefined,
                    (_, values) => {
                        let items = values
                            .unwrap_or_default()
                            .into_iter()
                            .map(|value| self.wrap_style(value))
                            .collect();
                        Self::new_array(items)
                    }
                })
            }
            M::Size => Ok(Value::Number(self.map_entries(node, computed).len() as f64)),
            M::Entries | M::Keys | M::MapValues | M::ForEach => {
                let entries = self.map_entries(node, computed);
                let mut items = Vec::new();
                for (property, value) in entries {
                    let values = reify(&property, &value)
                        .into_iter()
                        .map(|value| self.wrap_style(value))
                        .collect();
                    let values = Self::new_array(values);
                    let key = Value::String(property.into());
                    items.push(match member {
                        M::Keys => key,
                        M::MapValues => values,
                        _ => Self::new_array(vec![key, values]),
                    });
                }
                if member == M::ForEach {
                    let callback = argument(0);
                    if !is_callable(&callback) {
                        return Err(self.type_error(format!("Failed to execute 'forEach' on '{interface}': The callback provided as parameter 1 is not a function.")));
                    }
                    let map = self.style_property_map(node, computed);
                    for item in items {
                        let key = self.get_property(&item, "0")?;
                        let values = self.get_property(&item, "1")?;
                        self.call_function(
                            callback.clone(),
                            argument(1),
                            vec![values, key, map.clone()],
                        )?;
                    }
                    return Ok(Value::Undefined);
                }
                self.array_iterator(Self::new_array(items))
            }
            M::Set | M::Append => {
                let method = if member == M::Set { "set" } else { "append" };
                let failed = format!("Failed to execute '{method}' on 'StylePropertyMap'");
                let property = self.map_property(argument(0), node, method, "StylePropertyMap")?;
                let list = LIST_VALUED.contains(&property.as_str());
                if member == M::Append && !list {
                    return Err(self.type_error(format!(
                        "{failed}: Property does not support multiple values"
                    )));
                }
                let values = arguments[1.min(arguments.len())..].to_vec();
                if values.len() > 1 && !list && !property.starts_with("--") {
                    return Err(self.type_error(format!("{failed}: Invalid type for property")));
                }
                let mut texts = Vec::new();
                for value in values {
                    match self.map_text(&property, value)? {
                        Some(text) => texts.push(text),
                        None => {
                            return Err(
                                self.type_error(format!("{failed}: Invalid type for property"))
                            );
                        }
                    }
                }
                let mut text = texts.join(", ");
                if member == M::Append
                    && let Some((_, existing)) = self
                        .map_entries(node, false)
                        .into_iter()
                        .find(|(name, _)| *name == property)
                {
                    text = format!("{existing}, {text}");
                }
                self.document
                    .borrow_mut()
                    .set_style_property(node, &property, text);
                Ok(Value::Undefined)
            }
            M::Delete => {
                let property =
                    self.map_property(argument(0), node, "delete", "StylePropertyMap")?;
                self.document
                    .borrow_mut()
                    .set_style_property(node, &property, String::new());
                Ok(Value::Undefined)
            }
            M::Clear => {
                self.document
                    .borrow_mut()
                    .set_attribute(node, "style", String::new());
                Ok(Value::Undefined)
            }
            _ => Err(self.type_error("Illegal invocation")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(source: &str) -> String {
        let document = Rc::new(RefCell::new(
            crate::dom::Document::parse(
                r#"<div id="box" style="top: 5px; --inline: 3"></div><output id="out"></output>"#,
            )
            .expect("document should parse"),
        ));
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(&format!(
                "document.getElementById('out').textContent = (() => {{ {source} }})();"
            ))
            .unwrap_or_else(|error| panic!("{source}: {error:?}"));
        let out = document.borrow().find_element_by_id("out").unwrap();
        document
            .borrow()
            .element_text_content(out)
            .unwrap_or_default()
    }

    #[test]
    fn numeric_values_serialize_combine_and_convert() {
        assert_eq!(
            run(r#"
                const attempt = (make) => { try { return String(make()); } catch (error) { return error.name; } };
                return [
                    CSS.px(1), CSS.ms(1 / 3), CSS.px(1e21), CSS.px(1234567.5), CSS.percent(50), CSS.number(0.5),
                    CSS.px(1).add(CSS.em(2)), CSS.px(1).add(CSS.px(2), CSS.px(3)), CSS.px(1).sub(CSS.percent(5)),
                    CSS.px(2).mul(3), CSS.px(2).mul(CSS.em(1)), CSS.px(6).div(2), CSS.px(6).div(CSS.px(2)),
                    CSS.px(1).min(CSS.em(2)), CSS.px(1).max(CSS.px(3)), new CSSMathClamp(CSS.px(1), CSS.vw(5), CSS.px(10)),
                    new CSSMathNegate(CSS.px(1)), new CSSMathInvert(CSS.number(2)),
                    CSS.in(1).to("px"), CSS.deg(180).to("rad"), CSS.px(1).add(CSS.in(1)).to("px"),
                    CSS.px(1).add(CSS.em(2)).add(CSS.px(3)).toSum("px", "em"),
                    JSON.stringify(CSS.px(1).mul(CSS.px(1)).type()), JSON.stringify(CSS.px(1).add(CSS.percent(1)).type()),
                    CSS.px(1).equals(CSS.px(1)), CSS.px(1).add(CSS.em(1)).equals(CSS.px(1).add(CSS.em(1))),
                    attempt(() => CSS.px(1).add(CSS.s(1))), attempt(() => CSS.px(1).to("em")), attempt(() => new CSSUnitValue(1, "foo")),
                ].join(" | ");
            "#),
            "1px | 0.333333ms | 1e+21px | 1.23457e+06px | 50% | 0.5 | calc(1px + 2em) | 6px | calc(1px + -5%) | 6px | calc(2px * 1em) | 3px | calc(6px / 2px) | min(1px, 2em) | 3px | clamp(1px, 5vw, 10px) | calc(-1px) | calc(1 / 2) | 96px | 3.14159rad | 97px | calc(4px + 2em) | {\"length\":2} | {\"length\":1,\"percentHint\":\"length\"} | true | true | TypeError | TypeError | TypeError"
        );
    }

    #[test]
    fn values_parse_and_property_maps_read_and_write_styles() {
        assert_eq!(
            run(r#"
                const kind = (value) => value === undefined ? "undefined" : value.constructor.name + "(" + value + ")";
                const attempt = (make) => { try { return kind(make()); } catch (error) { return error.name; } };
                const box = document.getElementById("box");
                const map = box.attributeStyleMap;
                const read = [map.size, kind(map.get("top")), kind(map.get("--inline")), kind(map.get("left")), attempt(() => map.get("nope"))];
                map.set("left", CSS.px(9));
                map.set("opacity", 0.25);
                map.append("transition-duration", CSS.s(1), CSS.ms(5));
                const set = [box.style.left, box.style.opacity, box.style.transitionDuration, attempt(() => map.set("width", CSS.s(1)))];
                map.delete("top");
                const sum = CSSNumericValue.parse("calc(1px + 2em)");
                return [
                    ...read, ...set, map.has("top"),
                    kind(CSSNumericValue.parse("calc(2 * 3px)")), kind(sum), sum.values.length, kind(sum.values[1]),
                    kind(CSSStyleValue.parse("display", "block")), kind(CSSStyleValue.parse("color", "red")),
                    kind(CSSStyleValue.parse("margin", "1px 2px")), attempt(() => CSSStyleValue.parse("width", "red")),
                    CSSStyleValue.parseAll("transition-duration", "1s, 2s").map(kind).join(" "),
                    kind(CSSStyleValue.parse("transform", "translate(10px, 20px) rotate(45deg)")),
                    kind(new CSSUnparsedValue(["a ", new CSSVariableReferenceValue("--b", new CSSUnparsedValue(["c"]))])),
                    CSS.supports("width", "red"), CSS.supports("width", "calc(1px + 2%)"), CSS.supports("z-index", "1.5"),
                    CSS.supports("transition-duration", "1s, 20ms"),
                ].join(" | ");
            "#),
            "2 | CSSUnitValue(5px) | CSSUnparsedValue(3) | undefined | TypeError | 9px | 0.25 | 1s, 5ms | TypeError | false | CSSMathSum(calc(6px)) | CSSMathSum(calc(1px + 2em)) | 2 | CSSUnitValue(2em) | CSSKeywordValue(block) | CSSStyleValue(red) | CSSStyleValue(1px 2px) | TypeError | CSSUnitValue(1s) CSSUnitValue(2s) | CSSTransformValue(translate(10px, 20px) rotate(45deg)) | CSSUnparsedValue(a var(--b,c)) | false | true | false | true"
        );
    }
}
