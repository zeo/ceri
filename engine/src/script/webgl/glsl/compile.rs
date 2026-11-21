// type checking and code generation in one pass: each expression gets slots of its own and a
// closure that fills them for all four lanes, each statement a closure that works under the
// machine's lane mask, and the whole shader a module of main, its memory and its interface

mod builtins;

use super::exec::{ALL, LANES, Machine, Op, SamplerKind};
use super::parse::{
    Declaration, Expr, ExprKind, External, Parameter, Stmt, Storage, SwitchItem, TypeSpec,
};
use std::cell::OnceCell;
use std::collections::HashMap;
use std::rc::Rc;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Base {
    Float,
    Int,
    Uint,
    Bool,
}

#[derive(Debug)]
pub(crate) struct StructTy {
    pub(crate) name: String,
    pub(crate) fields: Vec<(String, Ty)>,
}

#[derive(Clone, Debug)]
pub(crate) enum Ty {
    Void,
    Vec(Base, u8),
    Mat(u8, u8),
    Sampler(SamplerKind, Base),
    Struct(Rc<StructTy>),
    Array(Box<Ty>, usize),
}

impl PartialEq for Ty {
    fn eq(&self, other: &Ty) -> bool {
        match (self, other) {
            (Ty::Void, Ty::Void) => true,
            (Ty::Vec(a, n), Ty::Vec(b, m)) => a == b && n == m,
            (Ty::Mat(a, b), Ty::Mat(c, d)) => a == c && b == d,
            (Ty::Sampler(a, b), Ty::Sampler(c, d)) => a == c && b == d,
            (Ty::Struct(a), Ty::Struct(b)) => Rc::ptr_eq(a, b),
            (Ty::Array(a, n), Ty::Array(b, m)) => n == m && a == b,
            _ => false,
        }
    }
}

pub(crate) const FLOAT: Ty = Ty::Vec(Base::Float, 1);
pub(crate) const INT: Ty = Ty::Vec(Base::Int, 1);
pub(crate) const UINT: Ty = Ty::Vec(Base::Uint, 1);
pub(crate) const BOOL: Ty = Ty::Vec(Base::Bool, 1);

impl Ty {
    pub(crate) fn slots(&self) -> usize {
        match self {
            Ty::Void => 0,
            Ty::Vec(_, size) => usize::from(*size),
            Ty::Mat(columns, rows) => usize::from(*columns) * usize::from(*rows),
            Ty::Sampler(..) => 1,
            Ty::Struct(def) => def.fields.iter().map(|(_, ty)| ty.slots()).sum(),
            Ty::Array(element, length) => element.slots() * length,
        }
    }

    pub(crate) fn base(&self) -> Option<Base> {
        match self {
            Ty::Vec(base, _) => Some(*base),
            Ty::Mat(..) => Some(Base::Float),
            _ => None,
        }
    }

    pub(crate) fn size(&self) -> u8 {
        match self {
            Ty::Vec(_, size) => *size,
            _ => 0,
        }
    }

    pub(crate) fn is_scalar(&self) -> bool {
        matches!(self, Ty::Vec(_, 1))
    }

    pub(crate) fn has_sampler(&self) -> bool {
        match self {
            Ty::Sampler(..) => true,
            Ty::Struct(def) => def.fields.iter().any(|(_, ty)| ty.has_sampler()),
            Ty::Array(element, _) => element.has_sampler(),
            _ => false,
        }
    }

    pub(crate) fn name(&self) -> String {
        match self {
            Ty::Void => "void".into(),
            Ty::Vec(base, 1) => match base {
                Base::Float => "float",
                Base::Int => "int",
                Base::Uint => "uint",
                Base::Bool => "bool",
            }
            .into(),
            Ty::Vec(base, size) => format!(
                "{}vec{size}",
                match base {
                    Base::Float => "",
                    Base::Int => "i",
                    Base::Uint => "u",
                    Base::Bool => "b",
                }
            ),
            Ty::Mat(columns, rows) if columns == rows => format!("mat{columns}"),
            Ty::Mat(columns, rows) => format!("mat{columns}x{rows}"),
            Ty::Sampler(kind, base) => {
                let prefix = match base {
                    Base::Int => "i",
                    Base::Uint => "u",
                    _ => "",
                };
                let name = match kind {
                    SamplerKind::Two => "sampler2D",
                    SamplerKind::Cube => "samplerCube",
                    SamplerKind::Three => "sampler3D",
                    SamplerKind::TwoArray => "sampler2DArray",
                    SamplerKind::TwoShadow => "sampler2DShadow",
                    SamplerKind::CubeShadow => "samplerCubeShadow",
                    SamplerKind::TwoArrayShadow => "sampler2DArrayShadow",
                };
                format!("{prefix}{name}")
            }
            Ty::Struct(def) => def.name.clone(),
            Ty::Array(element, length) => format!("{}[{length}]", element.name()),
        }
    }

    // a built-in type by its keyword
    pub(crate) fn named(word: &str) -> Option<Ty> {
        let vector = |base: Base, digit: &str| {
            digit
                .parse::<u8>()
                .ok()
                .filter(|size| (2..=4).contains(size))
                .map(|size| Ty::Vec(base, size))
        };
        Some(match word {
            "void" => Ty::Void,
            "float" => FLOAT,
            "int" => INT,
            "uint" => UINT,
            "bool" => BOOL,
            "sampler2D" => Ty::Sampler(SamplerKind::Two, Base::Float),
            "samplerCube" => Ty::Sampler(SamplerKind::Cube, Base::Float),
            "sampler3D" => Ty::Sampler(SamplerKind::Three, Base::Float),
            "sampler2DArray" => Ty::Sampler(SamplerKind::TwoArray, Base::Float),
            "sampler2DShadow" => Ty::Sampler(SamplerKind::TwoShadow, Base::Float),
            "samplerCubeShadow" => Ty::Sampler(SamplerKind::CubeShadow, Base::Float),
            "sampler2DArrayShadow" => Ty::Sampler(SamplerKind::TwoArrayShadow, Base::Float),
            "isampler2D" => Ty::Sampler(SamplerKind::Two, Base::Int),
            "isampler3D" => Ty::Sampler(SamplerKind::Three, Base::Int),
            "isamplerCube" => Ty::Sampler(SamplerKind::Cube, Base::Int),
            "isampler2DArray" => Ty::Sampler(SamplerKind::TwoArray, Base::Int),
            "usampler2D" => Ty::Sampler(SamplerKind::Two, Base::Uint),
            "usampler3D" => Ty::Sampler(SamplerKind::Three, Base::Uint),
            "usamplerCube" => Ty::Sampler(SamplerKind::Cube, Base::Uint),
            "usampler2DArray" => Ty::Sampler(SamplerKind::TwoArray, Base::Uint),
            _ => {
                if let Some(digit) = word.strip_prefix("vec") {
                    return vector(Base::Float, digit);
                }
                if let Some(digit) = word.strip_prefix("ivec") {
                    return vector(Base::Int, digit);
                }
                if let Some(digit) = word.strip_prefix("uvec") {
                    return vector(Base::Uint, digit);
                }
                if let Some(digit) = word.strip_prefix("bvec") {
                    return vector(Base::Bool, digit);
                }
                let shape = word.strip_prefix("mat")?;
                let (columns, rows) = match shape.split_once('x') {
                    Some((columns, rows)) => {
                        (columns.parse::<u8>().ok()?, rows.parse::<u8>().ok()?)
                    }
                    None => {
                        let size = shape.parse::<u8>().ok()?;
                        (size, size)
                    }
                };
                if !(2..=4).contains(&columns) || !(2..=4).contains(&rows) {
                    return None;
                }
                Ty::Mat(columns, rows)
            }
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Stage {
    Vertex,
    Fragment,
}

// a variable of the shader's interface
#[derive(Clone, Debug)]
pub(crate) struct Interface {
    pub(crate) name: String,
    pub(crate) ty: Ty,
    pub(crate) slot: usize,
    pub(crate) location: Option<i64>,
    pub(crate) flat: bool,
    pub(crate) used: bool,
    // the uniform block it belongs to; an instance-named block is one struct-typed uniform
    pub(crate) block: Option<Rc<str>>,
}

pub(crate) struct Module {
    pub(crate) version: u32,
    pub(crate) main: Rc<Op>,
    pub(crate) initial: Vec<u32>,
    pub(crate) inputs: Vec<Interface>,
    pub(crate) outputs: Vec<Interface>,
    pub(crate) uniforms: Vec<Interface>,
    pub(crate) builtins: HashMap<&'static str, usize>,
    // the built-in variables the shader names, which tells gl_FragColor from gl_FragData and
    // whether gl_FragDepth is written
    pub(crate) used_builtins: Vec<&'static str>,
    // the extensions its #extension directives enabled
    pub(crate) extensions: Vec<String>,
}

#[derive(Clone)]
struct Var {
    ty: Ty,
    slot: usize,
    storage: Storage,
    constant: bool,
    builtin_readonly: bool,
}

struct Function {
    parameters: Vec<(Ty, Storage, usize)>,
    returns: Ty,
    return_slot: usize,
    body: Rc<OnceCell<Op>>,
}

// a compiled expression: its type, where its value is, the code that puts it there, and
// whether it is a constant expression
pub(crate) struct Value {
    pub(crate) ty: Ty,
    pub(crate) slot: usize,
    pub(crate) code: Option<Op>,
    pub(crate) constant: bool,
}

// where an assignment writes: a base slot, per-lane offsets from dynamic indexes, and the
// components picked from the place
struct Place {
    ty: Ty,
    base: usize,
    dynamic: Vec<(Value, usize, usize)>,
    components: Option<Vec<usize>>,
    readonly: Option<String>,
}

pub(crate) struct Compiler {
    pub(crate) stage: Stage,
    pub(crate) version: u32,
    pub(crate) extensions: Vec<String>,
    pub(crate) initial: Vec<u32>,
    scopes: Vec<HashMap<String, Var>>,
    functions: HashMap<String, Vec<Rc<Function>>>,
    structs: Vec<HashMap<String, Rc<StructTy>>>,
    returns: Option<(Ty, usize)>,
    loops: usize,
    switches: usize,
    inputs: Vec<Interface>,
    outputs: Vec<Interface>,
    uniforms: Vec<Interface>,
    builtins: HashMap<&'static str, usize>,
    used_builtins: Vec<&'static str>,
    pub(crate) line: u32,
}

pub(crate) type Result<T> = std::result::Result<T, String>;

fn no_op() -> Option<Op> {
    None
}

pub(crate) fn run(code: &Option<Op>, machine: &mut Machine) {
    if let Some(code) = code {
        code(machine);
    }
}

pub(crate) fn as_f32(word: u32) -> f32 {
    f32::from_bits(word)
}

pub(crate) fn of_f32(value: f32) -> u32 {
    value.to_bits()
}

// a value converted from one base to another, as constructors do
fn convert(word: u32, from: Base, to: Base) -> u32 {
    match (from, to) {
        (a, b) if a == b => word,
        (Base::Float, Base::Int) => (as_f32(word) as i32) as u32,
        (Base::Float, Base::Uint) => {
            let value = as_f32(word);
            if value < 0.0 {
                (value as i32) as u32
            } else {
                value as u32
            }
        }
        (Base::Float, Base::Bool) => u32::from(as_f32(word) != 0.0),
        (Base::Int, Base::Float) => of_f32(word as i32 as f32),
        (Base::Uint, Base::Float) => of_f32(word as f32),
        (Base::Bool, Base::Float) => of_f32(if word != 0 { 1.0 } else { 0.0 }),
        (_, Base::Bool) => u32::from(word != 0),
        (Base::Bool, _) => u32::from(word != 0),
        _ => word,
    }
}

impl Compiler {
    pub(crate) fn fail<T>(&self, message: impl Into<String>) -> Result<T> {
        Err(format!("ERROR: 0:{}: {}", self.line, message.into()))
    }

    pub(crate) fn allocate(&mut self, count: usize) -> usize {
        let slot = self.initial.len() / LANES;
        self.initial
            .resize(self.initial.len() + count.max(1) * LANES, 0);
        slot
    }

    // a constant of a type, stored once in the initial memory
    pub(crate) fn constant(&mut self, ty: Ty, words: &[u32]) -> Value {
        let slot = self.allocate(words.len());
        for (index, word) in words.iter().enumerate() {
            for lane in 0..LANES {
                self.initial[(slot + index) * LANES + lane] = *word;
            }
        }
        Value {
            ty,
            slot,
            code: no_op(),
            constant: true,
        }
    }

    // the words of a constant expression, run on a scratch machine over the initial memory
    pub(crate) fn evaluate(&self, value: &Value) -> Option<Vec<u32>> {
        if !value.constant {
            return None;
        }
        let textures = super::exec::NoTextures;
        let mut machine = Machine {
            memory: self.initial.clone(),
            mask: ALL,
            broken: 0,
            continued: 0,
            returned: 0,
            discarded: 0,
            textures: &textures,
            budget: 1 << 20,
        };
        run(&value.code, &mut machine);
        Some(
            (0..value.ty.slots())
                .map(|index| machine.get(value.slot + index, 0))
                .collect(),
        )
    }

    fn lookup(&self, name: &str) -> Option<&Var> {
        self.scopes.iter().rev().find_map(|scope| scope.get(name))
    }

    fn lookup_struct(&self, name: &str) -> Option<Rc<StructTy>> {
        self.structs
            .iter()
            .rev()
            .find_map(|scope| scope.get(name).cloned())
    }

    fn declare(&mut self, name: &str, var: Var) -> Result<()> {
        if name.starts_with("gl_") {
            return self.fail(format!(
                "'{name}' : identifiers starting with gl_ are reserved"
            ));
        }
        let scope = self.scopes.last_mut().expect("a scope");
        if scope.contains_key(name) {
            return self.fail(format!("'{name}' : redefinition"));
        }
        scope.insert(name.to_string(), var);
        Ok(())
    }

    pub(crate) fn resolve_type(&mut self, spec: &TypeSpec) -> Result<Ty> {
        match spec {
            TypeSpec::Named(word) => {
                if let Some(def) = self.lookup_struct(word) {
                    return Ok(Ty::Struct(def));
                }
                match Ty::named(word) {
                    Some(ty) => {
                        if self.version < 300
                            && matches!(
                                &ty,
                                Ty::Vec(Base::Uint, _) | Ty::Mat(_, _) | Ty::Sampler(_, _)
                            )
                            && !matches!(&ty, Ty::Mat(a, b) if a == b && !word.contains('x'))
                            && !matches!(
                                &ty,
                                Ty::Sampler(SamplerKind::Two | SamplerKind::Cube, Base::Float)
                            )
                        {
                            return self.fail(format!("'{word}' : not supported in GLSL ES 1.00"));
                        }
                        Ok(ty)
                    }
                    None => self.fail(format!("'{word}' : unknown type")),
                }
            }
            TypeSpec::Struct(def) => {
                let mut fields = Vec::new();
                for (ty, type_array, name, array) in &def.fields {
                    let mut ty = self.resolve_type(ty)?;
                    if let Some(size) = type_array {
                        ty = Ty::Array(Box::new(ty), self.array_size(size)?);
                    }
                    if let Some(size) = array {
                        ty = Ty::Array(Box::new(ty), self.array_size(size)?);
                    }
                    if fields
                        .iter()
                        .any(|(existing, _): &(String, Ty)| existing == name)
                    {
                        return self.fail(format!("'{name}' : duplicate field name in structure"));
                    }
                    fields.push((name.clone(), ty));
                }
                let name = def
                    .name
                    .clone()
                    .unwrap_or_else(|| format!("__anonymous{}", self.initial.len()));
                let made = Rc::new(StructTy {
                    name: name.clone(),
                    fields,
                });
                if def.name.is_some() {
                    self.structs
                        .last_mut()
                        .expect("a scope")
                        .insert(name, made.clone());
                }
                Ok(Ty::Struct(made))
            }
        }
    }

    fn array_size(&mut self, size: &Expr) -> Result<usize> {
        let value = self.expression(size)?;
        if !matches!(value.ty, Ty::Vec(Base::Int | Base::Uint, 1)) {
            return self.fail("array size must be a constant integer expression");
        }
        match self.evaluate(&value) {
            Some(words) if (words[0] as i32) > 0 => Ok(words[0] as i32 as usize),
            Some(_) => self.fail("array size must be greater than zero"),
            None => self.fail("array size must be a constant integer expression"),
        }
    }

    // reads of a place into a value of its own slots
    fn load(&mut self, place: Place) -> Value {
        let size = place.ty.slots();
        if place.dynamic.is_empty() && place.components.is_none() {
            return Value {
                ty: place.ty,
                slot: place.base,
                code: no_op(),
                constant: false,
            };
        }
        let out = self.allocate(size);
        let Place {
            ty,
            base,
            dynamic,
            components,
            ..
        } = place;
        let code: Op = Box::new(move |m: &mut Machine| {
            for (index, _, _) in &dynamic {
                run(&index.code, m);
            }
            for lane in 0..LANES {
                let mut offset = base;
                for (index, stride, length) in &dynamic {
                    let at = (m.get(index.slot, lane) as i32).clamp(0, *length as i32 - 1) as usize;
                    offset += at * stride;
                }
                match &components {
                    Some(components) => {
                        for (target, component) in components.iter().enumerate() {
                            let value = m.get(offset + component, lane);
                            m.set(out + target, lane, value);
                        }
                    }
                    None => {
                        for target in 0..size {
                            let value = m.get(offset + target, lane);
                            m.set(out + target, lane, value);
                        }
                    }
                }
            }
        });
        Value {
            ty,
            slot: out,
            code: Some(code),
            constant: false,
        }
    }

    // a place split in two: one that computes its indexes, for the read that comes first, and one
    // that reuses them, for the write after
    fn split(place: Place) -> (Place, Place) {
        let reuse = place
            .dynamic
            .iter()
            .map(|(index, stride, length)| {
                (
                    Value {
                        ty: index.ty.clone(),
                        slot: index.slot,
                        code: no_op(),
                        constant: false,
                    },
                    *stride,
                    *length,
                )
            })
            .collect();
        let writing = Place {
            ty: place.ty.clone(),
            base: place.base,
            dynamic: reuse,
            components: place.components.clone(),
            readonly: place.readonly.clone(),
        };
        (place, writing)
    }

    // a store into a place, for the lanes alive in the mask
    fn store(place: Place, value_slot: usize) -> Op {
        let Place {
            ty,
            base,
            dynamic,
            components,
            ..
        } = place;
        let size = ty.slots();
        Box::new(move |m: &mut Machine| {
            for (index, _, _) in &dynamic {
                run(&index.code, m);
            }
            for lane in 0..LANES {
                if m.mask & (1 << lane) == 0 {
                    continue;
                }
                let mut offset = base;
                for (index, stride, length) in &dynamic {
                    let at = (m.get(index.slot, lane) as i32).clamp(0, *length as i32 - 1) as usize;
                    offset += at * stride;
                }
                match &components {
                    Some(components) => {
                        for (source, component) in components.iter().enumerate() {
                            let value = m.get(value_slot + source, lane);
                            m.set(offset + component, lane, value);
                        }
                    }
                    None => {
                        for index in 0..size {
                            let value = m.get(value_slot + index, lane);
                            m.set(offset + index, lane, value);
                        }
                    }
                }
            }
        })
    }

    fn place(&mut self, expr: &Expr) -> Result<Place> {
        self.line = expr.line;
        match &expr.kind {
            ExprKind::Ident(name) => {
                let Some(var) = self.lookup(name).cloned() else {
                    return self.fail(format!("'{name}' : undeclared identifier"));
                };
                self.note_builtin(var.slot);
                let readonly = match var.storage {
                    _ if var.constant || var.builtin_readonly => Some(name.clone()),
                    Storage::Uniform | Storage::Attribute => Some(name.clone()),
                    Storage::Varying | Storage::In
                        if self.stage == Stage::Fragment || var.storage == Storage::In =>
                    {
                        Some(name.clone())
                    }
                    _ => None,
                };
                Ok(Place {
                    ty: var.ty,
                    base: var.slot,
                    dynamic: Vec::new(),
                    components: None,
                    readonly,
                })
            }
            ExprKind::Index(inner, index) => {
                let mut place = self.place(inner)?;
                if place.components.is_some() {
                    return self.fail("cannot index a swizzle as an l-value");
                }
                let index_value = self.expression(index)?;
                if !matches!(index_value.ty, Ty::Vec(Base::Int | Base::Uint, 1)) {
                    return self.fail("array index must be an integer");
                }
                let (element, stride, length) = match &place.ty {
                    Ty::Array(element, length) => ((**element).clone(), element.slots(), *length),
                    Ty::Mat(columns, rows) => (
                        Ty::Vec(Base::Float, *rows),
                        usize::from(*rows),
                        usize::from(*columns),
                    ),
                    Ty::Vec(base, size) if *size > 1 => (Ty::Vec(*base, 1), 1, usize::from(*size)),
                    _ => {
                        return self
                            .fail("'[' : left of '[' is not of type array, matrix, or vector");
                    }
                };
                if let Some(words) = self.evaluate(&index_value) {
                    let at = words[0] as i32;
                    if at < 0 || at as usize >= length {
                        return self.fail(format!("'[' : index {at} out of range"));
                    }
                    place.base += at as usize * stride;
                } else {
                    place.dynamic.push((index_value, stride, length));
                }
                place.ty = element;
                Ok(place)
            }
            ExprKind::Field(inner, field) => {
                let mut place = self.place(inner)?;
                match place.ty.clone() {
                    Ty::Struct(def) => {
                        if place.components.is_some() {
                            return self.fail("cannot select a field of a swizzle");
                        }
                        let mut offset = 0;
                        for (name, ty) in &def.fields {
                            if name == field {
                                place.base += offset;
                                place.ty = ty.clone();
                                return Ok(place);
                            }
                            offset += ty.slots();
                        }
                        self.fail(format!("'{field}' : no such field in structure"))
                    }
                    Ty::Vec(base, size) => {
                        let picked = self.swizzle(field, size)?;
                        let unique = picked
                            .iter()
                            .enumerate()
                            .all(|(index, item)| !picked[..index].contains(item));
                        let mapped: Vec<usize> = match &place.components {
                            Some(existing) => picked.iter().map(|&index| existing[index]).collect(),
                            None => picked.clone(),
                        };
                        if !unique {
                            place.readonly = Some(format!("{field} (repeated components)"));
                        }
                        place.ty = Ty::Vec(base, picked.len() as u8);
                        place.components = Some(mapped);
                        Ok(place)
                    }
                    other => self.fail(format!(
                        "'.' : field selection requires structure or vector, not {}",
                        other.name()
                    )),
                }
            }
            _ => self.fail("l-value required"),
        }
    }

    fn swizzle(&self, field: &str, size: u8) -> Result<Vec<usize>> {
        let sets = ["xyzw", "rgba", "stpq"];
        let Some(set) = sets
            .iter()
            .find(|set| field.chars().all(|c| set.contains(c)))
        else {
            return self.fail(format!("'{field}' : illegal vector field selection"));
        };
        if field.is_empty() || field.len() > 4 {
            return self.fail(format!("'{field}' : illegal vector field selection"));
        }
        let picked: Vec<usize> = field
            .chars()
            .map(|c| set.find(c).expect("checked above"))
            .collect();
        if picked.iter().any(|&index| index >= usize::from(size)) {
            return self.fail(format!("'{field}' : vector field selection out of range"));
        }
        Ok(picked)
    }

    fn index_value(&mut self, value: Value, index: &Expr) -> Result<Value> {
        let index_value = self.expression(index)?;
        if !matches!(index_value.ty, Ty::Vec(Base::Int | Base::Uint, 1)) {
            return self.fail("array index must be an integer");
        }
        let (element, stride, length) = match &value.ty {
            Ty::Array(element, length) => ((**element).clone(), element.slots(), *length),
            Ty::Mat(columns, rows) => (
                Ty::Vec(Base::Float, *rows),
                usize::from(*rows),
                usize::from(*columns),
            ),
            Ty::Vec(base, size) if *size > 1 => (Ty::Vec(*base, 1), 1, usize::from(*size)),
            _ => return self.fail("'[' : left of '[' is not of type array, matrix, or vector"),
        };
        let constant = value.constant && index_value.constant;
        let place = Place {
            ty: element,
            base: value.slot,
            dynamic: vec![(index_value, stride, length)],
            components: None,
            readonly: None,
        };
        let inner = value.code;
        let mut loaded = self.load(place);
        let read = loaded.code.take();
        loaded.code = Some(Box::new(move |m: &mut Machine| {
            run(&inner, m);
            run(&read, m);
        }));
        loaded.constant = constant;
        Ok(loaded)
    }

    pub(crate) fn expression(&mut self, expr: &Expr) -> Result<Value> {
        self.line = expr.line;
        match &expr.kind {
            ExprKind::Int(value, unsigned) => {
                if *unsigned && self.version < 300 {
                    return self.fail("unsigned integers need GLSL ES 3.00");
                }
                if !*unsigned && self.version < 300 && *value > i32::MAX as u32 + 1 {
                    return self.fail("integer constant overflow");
                }
                Ok(self.constant(if *unsigned { UINT } else { INT }, &[*value]))
            }
            ExprKind::Float(value) => Ok(self.constant(FLOAT, &[of_f32(*value)])),
            ExprKind::Bool(value) => Ok(self.constant(BOOL, &[u32::from(*value)])),
            ExprKind::Ident(name) => {
                let Some(var) = self.lookup(name).cloned() else {
                    return self.fail(format!("'{name}' : undeclared identifier"));
                };
                self.note_builtin(var.slot);
                if let Some(interface) = self
                    .inputs
                    .iter_mut()
                    .chain(self.uniforms.iter_mut())
                    .chain(self.outputs.iter_mut())
                    .find(|interface| interface.slot == var.slot)
                {
                    interface.used = true;
                }
                Ok(Value {
                    ty: var.ty,
                    slot: var.slot,
                    code: no_op(),
                    constant: var.constant,
                })
            }
            ExprKind::Field(inner, field) => {
                let value = self.expression(inner)?;
                match value.ty.clone() {
                    Ty::Struct(def) => {
                        let mut offset = 0;
                        for (name, ty) in &def.fields {
                            if name == field {
                                return Ok(Value {
                                    ty: ty.clone(),
                                    slot: value.slot + offset,
                                    code: value.code,
                                    constant: value.constant,
                                });
                            }
                            offset += ty.slots();
                        }
                        self.fail(format!("'{field}' : no such field in structure"))
                    }
                    Ty::Vec(base, size) => {
                        let picked = self.swizzle(field, size)?;
                        let out = self.allocate(picked.len());
                        let source = value.slot;
                        let inner = value.code;
                        let count = picked.len();
                        Ok(Value {
                            ty: Ty::Vec(base, count as u8),
                            slot: out,
                            constant: value.constant,
                            code: Some(Box::new(move |m: &mut Machine| {
                                run(&inner, m);
                                for (target, &component) in picked.iter().enumerate() {
                                    for lane in 0..LANES {
                                        let word = m.get(source + component, lane);
                                        m.set(out + target, lane, word);
                                    }
                                }
                            })),
                        })
                    }
                    other => self.fail(format!(
                        "'.' : field selection requires structure or vector, not {}",
                        other.name()
                    )),
                }
            }
            ExprKind::Index(inner, index) => {
                let value = self.expression(inner)?;
                let index_value = self.expression(index)?;
                if let (Some(words), true) = (
                    self.evaluate(&index_value),
                    matches!(value.ty, Ty::Array(..) | Ty::Mat(..) | Ty::Vec(_, 2..=4)),
                ) {
                    let at = words[0] as i32;
                    let (element, stride, length) = match &value.ty {
                        Ty::Array(element, length) => {
                            ((**element).clone(), element.slots(), *length)
                        }
                        Ty::Mat(columns, rows) => (
                            Ty::Vec(Base::Float, *rows),
                            usize::from(*rows),
                            usize::from(*columns),
                        ),
                        Ty::Vec(base, size) => (Ty::Vec(*base, 1), 1, usize::from(*size)),
                        _ => unreachable!("checked above"),
                    };
                    if at < 0 || at as usize >= length {
                        return self.fail(format!("'[' : index {at} out of range"));
                    }
                    return Ok(Value {
                        ty: element,
                        slot: value.slot + at as usize * stride,
                        code: value.code,
                        constant: value.constant,
                    });
                }
                self.index_value(value, index)
            }
            ExprKind::Length(inner) => {
                let value = self.expression(inner)?;
                let length = match &value.ty {
                    Ty::Array(_, length) => *length,
                    Ty::Vec(_, size) if *size > 1 => usize::from(*size),
                    Ty::Mat(columns, _) => usize::from(*columns),
                    _ => return self.fail("length() needs an array, vector or matrix"),
                };
                Ok(self.constant(INT, &[length as u32]))
            }
            ExprKind::Comma(left, right) => {
                let left = self.expression(left)?;
                let right = self.expression(right)?;
                let (first, second) = (left.code, right.code);
                Ok(Value {
                    ty: right.ty,
                    slot: right.slot,
                    constant: left.constant && right.constant,
                    code: Some(Box::new(move |m: &mut Machine| {
                        run(&first, m);
                        run(&second, m);
                    })),
                })
            }
            ExprKind::Ternary(condition, then, otherwise) => {
                let condition = self.expression(condition)?;
                if condition.ty != BOOL {
                    return self.fail("'?:' : boolean expression expected");
                }
                let then = self.expression(then)?;
                let otherwise = self.expression(otherwise)?;
                if then.ty != otherwise.ty {
                    return self.fail("'?:' : wrong operand types");
                }
                let size = then.ty.slots();
                let out = self.allocate(size);
                let constant = condition.constant && then.constant && otherwise.constant;
                let (test, then_slot, else_slot) = (condition.slot, then.slot, otherwise.slot);
                let (test_code, then_code, else_code) = (condition.code, then.code, otherwise.code);
                Ok(Value {
                    ty: then.ty,
                    slot: out,
                    constant,
                    code: Some(Box::new(move |m: &mut Machine| {
                        run(&test_code, m);
                        let saved = m.mask;
                        let mut chosen = 0;
                        for lane in 0..LANES {
                            if m.get(test, lane) != 0 {
                                chosen |= 1 << lane;
                            }
                        }
                        m.mask = saved & chosen;
                        if m.mask != 0 || saved == 0 {
                            run(&then_code, m);
                        }
                        m.mask = saved & !chosen & ALL;
                        if m.mask != 0 || saved == 0 {
                            run(&else_code, m);
                        }
                        m.mask = saved;
                        for lane in 0..LANES {
                            let from = if chosen & (1 << lane) != 0 {
                                then_slot
                            } else {
                                else_slot
                            };
                            for index in 0..size {
                                let word = m.get(from + index, lane);
                                m.set(out + index, lane, word);
                            }
                        }
                    })),
                })
            }
            ExprKind::Assign(op, target, source) => self.assignment(op, target, source),
            ExprKind::Unary(op, operand) => self.unary(op, operand),
            ExprKind::Postfix(op, operand) => self.increment(op, operand, true),
            ExprKind::Binary(op, left, right) => {
                if matches!(*op, "&&" | "||") {
                    return self.logical(op, left, right);
                }
                let left = self.expression(left)?;
                let right = self.expression(right)?;
                self.binary(op, left, right)
            }
            ExprKind::Call(name, array, arguments) => self.call(name, array.as_ref(), arguments),
        }
    }

    fn logical(&mut self, op: &str, left: &Expr, right: &Expr) -> Result<Value> {
        let left = self.expression(left)?;
        let right = self.expression(right)?;
        if left.ty != BOOL || right.ty != BOOL {
            return self.fail(format!("'{op}' : wrong operand types"));
        }
        let out = self.allocate(1);
        let and = op == "&&";
        let constant = left.constant && right.constant;
        let (a, b) = (left.slot, right.slot);
        let (left_code, right_code) = (left.code, right.code);
        Ok(Value {
            ty: BOOL,
            slot: out,
            constant,
            code: Some(Box::new(move |m: &mut Machine| {
                run(&left_code, m);
                let saved = m.mask;
                let mut needs = 0;
                for lane in 0..LANES {
                    if (m.get(a, lane) != 0) == and {
                        needs |= 1 << lane;
                    }
                }
                m.mask = saved & needs;
                if m.mask != 0 || saved == 0 {
                    run(&right_code, m);
                }
                m.mask = saved;
                for lane in 0..LANES {
                    let first = m.get(a, lane) != 0;
                    let value = if and {
                        first && m.get(b, lane) != 0
                    } else {
                        first || m.get(b, lane) != 0
                    };
                    m.set(out, lane, u32::from(value));
                }
            })),
        })
    }

    fn assignment(&mut self, op: &'static str, target: &Expr, source: &Expr) -> Result<Value> {
        let place = self.place(target)?;
        if let Some(name) = &place.readonly {
            return self.fail(format!(
                "'{op}' : l-value required ({name} cannot be assigned)"
            ));
        }
        let source = self.expression(source)?;
        let value = if op == "=" {
            if place.ty != source.ty {
                return self.fail(format!(
                    "'=' : cannot convert from '{}' to '{}'",
                    source.ty.name(),
                    place.ty.name()
                ));
            }
            source
        } else {
            let (reading, writing) = Self::split(place);
            let current = self.load(reading);
            let combined = self.binary(&op[..op.len() - 1], current, source)?;
            if combined.ty != writing.ty {
                return self.fail(format!(
                    "'{op}' : cannot convert from '{}' to '{}'",
                    combined.ty.name(),
                    writing.ty.name()
                ));
            }
            let ty = writing.ty.clone();
            let out = combined.slot;
            let compute = combined.code;
            let store = Self::store(writing, out);
            return Ok(Value {
                ty,
                slot: out,
                constant: false,
                code: Some(Box::new(move |m: &mut Machine| {
                    run(&compute, m);
                    store(m);
                })),
            });
        };
        let ty = place.ty.clone();
        let out = value.slot;
        let compute = value.code;
        let store = Self::store(place, out);
        Ok(Value {
            ty,
            slot: out,
            constant: false,
            code: Some(Box::new(move |m: &mut Machine| {
                run(&compute, m);
                store(m);
            })),
        })
    }

    fn increment(&mut self, op: &'static str, operand: &Expr, postfix: bool) -> Result<Value> {
        let place = self.place(operand)?;
        if let Some(name) = &place.readonly {
            return self.fail(format!(
                "'{op}' : l-value required ({name} cannot be modified)"
            ));
        }
        let Some(base) = place.ty.base().filter(|base| *base != Base::Bool) else {
            return self.fail(format!("'{op}' : wrong operand type"));
        };
        let ty = place.ty.clone();
        let size = ty.slots();
        let (reading, writing) = Self::split(place);
        let current = self.load(reading);
        let old = self.allocate(size);
        let new = self.allocate(size);
        let read = current.code;
        let current_slot = current.slot;
        let store = Self::store(writing, new);
        let up = op == "++";
        Ok(Value {
            ty,
            slot: if postfix { old } else { new },
            constant: false,
            code: Some(Box::new(move |m: &mut Machine| {
                run(&read, m);
                m.copy_all(current_slot, old, size);
                for index in 0..size {
                    for lane in 0..LANES {
                        let word = m.get(current_slot + index, lane);
                        let next = match base {
                            Base::Float => of_f32(as_f32(word) + if up { 1.0 } else { -1.0 }),
                            _ => {
                                if up {
                                    word.wrapping_add(1)
                                } else {
                                    word.wrapping_sub(1)
                                }
                            }
                        };
                        m.set(new + index, lane, next);
                    }
                }
                store(m);
            })),
        })
    }

    fn unary(&mut self, op: &'static str, operand: &Expr) -> Result<Value> {
        if matches!(op, "++" | "--") {
            return self.increment(op, operand, false);
        }
        let value = self.expression(operand)?;
        let ty = value.ty.clone();
        let size = ty.slots();
        let Some(base) = ty.base() else {
            return self.fail(format!("'{op}' : wrong operand type"));
        };
        match (op, base) {
            ("!", Base::Bool) if ty.is_scalar() => {}
            ("~", Base::Int | Base::Uint) if self.version >= 300 => {}
            ("+" | "-", Base::Float | Base::Int | Base::Uint) => {}
            _ => return self.fail(format!("'{op}' : wrong operand type '{}'", ty.name())),
        }
        let out = self.allocate(size);
        let source = value.slot;
        let inner = value.code;
        let apply: fn(u32) -> u32 = match (op, base) {
            ("+", _) => |word| word,
            ("-", Base::Float) => |word| of_f32(-as_f32(word)),
            ("-", _) => u32::wrapping_neg,
            ("!", _) => |word| u32::from(word == 0),
            _ => |word| !word,
        };
        Ok(Value {
            ty,
            slot: out,
            constant: value.constant,
            code: Some(Box::new(move |m: &mut Machine| {
                run(&inner, m);
                for index in 0..size {
                    for lane in 0..LANES {
                        let word = m.get(source + index, lane);
                        m.set(out + index, lane, apply(word));
                    }
                }
            })),
        })
    }

    // the binary operators: componentwise on vectors, with a scalar spread over a vector or
    // matrix, and linear algebra for * between matrices and vectors
    pub(crate) fn binary(&mut self, op: &str, left: Value, right: Value) -> Result<Value> {
        let (lt, rt) = (left.ty.clone(), right.ty.clone());
        let fail = |compiler: &Compiler| {
            compiler.fail::<Value>(format!("'{op}' : wrong operand types - no operation '{op}' exists that takes a left-hand operand of type '{}' and a right operand of type '{}' (or there is no acceptable conversion)", lt.name(), rt.name()))
        };
        let constant = left.constant && right.constant;
        let (a, b) = (left.slot, right.slot);
        let (left_code, right_code) = (left.code, right.code);
        let prelude = move |m: &mut Machine| {
            run(&left_code, m);
            run(&right_code, m);
        };
        if matches!(op, "==" | "!=") {
            if lt != rt || lt.has_sampler() || matches!(lt, Ty::Void) {
                return fail(self);
            }
            let size = lt.slots();
            let out = self.allocate(1);
            let equal = op == "==";
            let floats = float_mask(&lt);
            return Ok(Value {
                ty: BOOL,
                slot: out,
                constant,
                code: Some(Box::new(move |m: &mut Machine| {
                    prelude(m);
                    for lane in 0..LANES {
                        let same = (0..size).all(|index| {
                            let (x, y) = (m.get(a + index, lane), m.get(b + index, lane));
                            if floats[index] {
                                as_f32(x) == as_f32(y)
                            } else {
                                x == y
                            }
                        });
                        m.set(out, lane, u32::from(same == equal));
                    }
                })),
            });
        }
        if op == "^^" {
            if lt != BOOL || rt != BOOL {
                return fail(self);
            }
            let out = self.allocate(1);
            return Ok(Value {
                ty: BOOL,
                slot: out,
                constant,
                code: Some(Box::new(move |m: &mut Machine| {
                    prelude(m);
                    for lane in 0..LANES {
                        let value = (m.get(a, lane) != 0) != (m.get(b, lane) != 0);
                        m.set(out, lane, u32::from(value));
                    }
                })),
            });
        }
        if matches!(op, "<" | ">" | "<=" | ">=") {
            let (Ty::Vec(base, 1), Ty::Vec(other, 1)) = (&lt, &rt) else {
                return fail(self);
            };
            if base != other || *base == Base::Bool {
                return fail(self);
            }
            let base = *base;
            let out = self.allocate(1);
            let test: fn(std::cmp::Ordering) -> bool = match op {
                "<" => std::cmp::Ordering::is_lt,
                ">" => std::cmp::Ordering::is_gt,
                "<=" => std::cmp::Ordering::is_le,
                _ => std::cmp::Ordering::is_ge,
            };
            return Ok(Value {
                ty: BOOL,
                slot: out,
                constant,
                code: Some(Box::new(move |m: &mut Machine| {
                    prelude(m);
                    for lane in 0..LANES {
                        let (x, y) = (m.get(a, lane), m.get(b, lane));
                        let ordering = match base {
                            Base::Float => as_f32(x).partial_cmp(&as_f32(y)),
                            Base::Int => Some((x as i32).cmp(&(y as i32))),
                            _ => Some(x.cmp(&y)),
                        };
                        m.set(out, lane, u32::from(ordering.is_some_and(test)));
                    }
                })),
            });
        }
        // linear algebra
        if op == "*" {
            match (&lt, &rt) {
                (Ty::Mat(columns, rows), Ty::Vec(Base::Float, size)) if *size == *columns => {
                    let (columns, rows) = (usize::from(*columns), usize::from(*rows));
                    let out = self.allocate(rows);
                    return Ok(Value {
                        ty: Ty::Vec(Base::Float, rows as u8),
                        slot: out,
                        constant,
                        code: Some(Box::new(move |m: &mut Machine| {
                            prelude(m);
                            for lane in 0..LANES {
                                for row in 0..rows {
                                    let mut sum = 0.0;
                                    for column in 0..columns {
                                        sum += m.float(a + column * rows + row, lane)
                                            * m.float(b + column, lane);
                                    }
                                    m.set_float(out + row, lane, sum);
                                }
                            }
                        })),
                    });
                }
                (Ty::Vec(Base::Float, size), Ty::Mat(columns, rows)) if *size == *rows => {
                    let (columns, rows) = (usize::from(*columns), usize::from(*rows));
                    let out = self.allocate(columns);
                    return Ok(Value {
                        ty: Ty::Vec(Base::Float, columns as u8),
                        slot: out,
                        constant,
                        code: Some(Box::new(move |m: &mut Machine| {
                            prelude(m);
                            for lane in 0..LANES {
                                for column in 0..columns {
                                    let mut sum = 0.0;
                                    for row in 0..rows {
                                        sum += m.float(a + row, lane)
                                            * m.float(b + column * rows + row, lane);
                                    }
                                    m.set_float(out + column, lane, sum);
                                }
                            }
                        })),
                    });
                }
                (Ty::Mat(left_columns, left_rows), Ty::Mat(right_columns, right_rows))
                    if left_columns == right_rows =>
                {
                    let (inner, rows, columns) = (
                        usize::from(*left_columns),
                        usize::from(*left_rows),
                        usize::from(*right_columns),
                    );
                    let out = self.allocate(rows * columns);
                    return Ok(Value {
                        ty: Ty::Mat(columns as u8, rows as u8),
                        slot: out,
                        constant,
                        code: Some(Box::new(move |m: &mut Machine| {
                            prelude(m);
                            for lane in 0..LANES {
                                for column in 0..columns {
                                    for row in 0..rows {
                                        let mut sum = 0.0;
                                        for k in 0..inner {
                                            sum += m.float(a + k * rows + row, lane)
                                                * m.float(b + column * inner + k, lane);
                                        }
                                        m.set_float(out + column * rows + row, lane, sum);
                                    }
                                }
                            }
                        })),
                    });
                }
                _ => {}
            }
        }
        // componentwise, with a scalar spread over the other side
        let (base, result) = match (&lt, &rt) {
            (Ty::Vec(x, n), Ty::Vec(y, m)) if x == y && (n == m || *n == 1 || *m == 1) => {
                (*x, Ty::Vec(*x, (*n).max(*m)))
            }
            (Ty::Mat(c, r), Ty::Mat(c2, r2)) if c == c2 && r == r2 && op != "*" => {
                (Base::Float, lt.clone())
            }
            (Ty::Mat(..), Ty::Vec(Base::Float, 1)) => (Base::Float, lt.clone()),
            (Ty::Vec(Base::Float, 1), Ty::Mat(..)) => (Base::Float, rt.clone()),
            _ => return fail(self),
        };
        let valid = match op {
            "+" | "-" | "*" | "/" => base != Base::Bool,
            "%" | "&" | "|" | "^" | "<<" | ">>" => {
                matches!(base, Base::Int | Base::Uint) && self.version >= 300
            }
            _ => false,
        };
        let shift = matches!(op, "<<" | ">>");
        let integers = matches!(lt.base(), Some(Base::Int | Base::Uint))
            && matches!(rt.base(), Some(Base::Int | Base::Uint));
        if !(valid || shift && integers && self.version >= 300) {
            return fail(self);
        }
        let size = result.slots();
        let (left_spread, right_spread) = (lt.slots() == 1, rt.slots() == 1);
        let out = self.allocate(size);
        let operation = operation(op, base);
        Ok(Value {
            ty: result,
            slot: out,
            constant,
            code: Some(Box::new(move |m: &mut Machine| {
                prelude(m);
                for index in 0..size {
                    let (x_slot, y_slot) = (
                        a + if left_spread { 0 } else { index },
                        b + if right_spread { 0 } else { index },
                    );
                    for lane in 0..LANES {
                        let (x, y) = (m.get(x_slot, lane), m.get(y_slot, lane));
                        m.set(out + index, lane, operation(x, y));
                    }
                }
            })),
        })
    }
}

fn float_mask(ty: &Ty) -> Vec<bool> {
    match ty {
        Ty::Vec(base, size) => vec![*base == Base::Float; usize::from(*size)],
        Ty::Mat(..) => vec![true; ty.slots()],
        Ty::Struct(def) => def
            .fields
            .iter()
            .flat_map(|(_, ty)| float_mask(ty))
            .collect(),
        Ty::Array(element, length) => (0..*length).flat_map(|_| float_mask(element)).collect(),
        _ => vec![false; ty.slots()],
    }
}

// a binary operator on one component, picked once while compiling
fn operation(op: &str, base: Base) -> fn(u32, u32) -> u32 {
    match (base, op) {
        (Base::Float, "+") => |x, y| of_f32(as_f32(x) + as_f32(y)),
        (Base::Float, "-") => |x, y| of_f32(as_f32(x) - as_f32(y)),
        (Base::Float, "*") => |x, y| of_f32(as_f32(x) * as_f32(y)),
        (Base::Float, _) => |x, y| of_f32(as_f32(x) / as_f32(y)),
        (Base::Int, "+") => |x, y| (x as i32).wrapping_add(y as i32) as u32,
        (Base::Int, "-") => |x, y| (x as i32).wrapping_sub(y as i32) as u32,
        (Base::Int, "*") => |x, y| (x as i32).wrapping_mul(y as i32) as u32,
        (Base::Int, "/") => |x, y| {
            if y == 0 {
                0
            } else {
                (x as i32).wrapping_div(y as i32) as u32
            }
        },
        (Base::Int, "%") => |x, y| {
            if y == 0 {
                0
            } else {
                (x as i32).wrapping_rem(y as i32) as u32
            }
        },
        (Base::Int, ">>") => |x, y| (x as i32).wrapping_shr(y) as u32,
        (_, "+") => u32::wrapping_add,
        (_, "-") => u32::wrapping_sub,
        (_, "*") => u32::wrapping_mul,
        (_, "/") => |x, y| x.checked_div(y).unwrap_or(0),
        (_, "%") => |x, y| x.checked_rem(y).unwrap_or(0),
        (_, "&") => |x, y| x & y,
        (_, "|") => |x, y| x | y,
        (_, "^") => |x, y| x ^ y,
        (_, "<<") => u32::wrapping_shl,
        _ => u32::wrapping_shr,
    }
}

impl Compiler {
    // a call: a constructor when the name is a type, else a user function or a built-in
    fn call(
        &mut self,
        name: &str,
        array: Option<&Option<Box<Expr>>>,
        arguments: &[Expr],
    ) -> Result<Value> {
        let line = self.line;
        let mut values = Vec::new();
        let mut places = Vec::new();
        let user = self.functions.get(name).cloned();
        for (index, argument) in arguments.iter().enumerate() {
            let out_parameter = user.as_ref().is_some_and(|overloads| {
                overloads.iter().any(|function| {
                    function.parameters.len() == arguments.len()
                        && matches!(function.parameters[index].1, Storage::Out | Storage::InOut)
                })
            });
            if out_parameter {
                let place = self.place(argument)?;
                if let Some(name) = &place.readonly {
                    return self.fail(format!("'{name}' : l-value required for an out parameter"));
                }
                let (reading, writing) = Self::split(place);
                places.push(Some(writing));
                let mut loaded = self.load(reading);
                loaded.constant = false;
                values.push(loaded);
            } else {
                places.push(None);
                values.push(self.expression(argument)?);
            }
        }
        self.line = line;
        let struct_type = self.lookup_struct(name);
        if Ty::named(name).is_some() || struct_type.is_some() {
            let mut ty = match struct_type {
                Some(def) => Ty::Struct(def),
                None => self.resolve_type(&TypeSpec::Named(name.to_string()))?,
            };
            if let Some(size) = array {
                let length = match size {
                    Some(size) => self.array_size(size)?,
                    None => values.len(),
                };
                ty = Ty::Array(Box::new(ty), length);
            }
            return self.construct(ty, values);
        }
        if let Some(overloads) = user {
            let Some(function) = overloads.iter().find(|function| {
                function.parameters.len() == values.len()
                    && function
                        .parameters
                        .iter()
                        .zip(&values)
                        .all(|((ty, ..), value)| *ty == value.ty)
            }) else {
                return self.fail(format!("'{name}' : no matching overloaded function found"));
            };
            let function = function.clone();
            for ((_, storage, _), place) in function.parameters.iter().zip(&places) {
                if matches!(storage, Storage::Out | Storage::InOut) && place.is_none() {
                    return self.fail(format!("'{name}' : l-value required for an out parameter"));
                }
            }
            return Ok(self.user_call(function, values, places));
        }
        self.builtin(name, values)
    }

    fn user_call(
        &mut self,
        function: Rc<Function>,
        values: Vec<Value>,
        places: Vec<Option<Place>>,
    ) -> Value {
        let size = function.returns.slots();
        let out = self.allocate(size);
        let returns = function.returns.clone();
        let mut copies_in = Vec::new();
        let mut copies_out = Vec::new();
        let mut codes = Vec::new();
        for (((ty, storage, slot), value), place) in
            function.parameters.iter().zip(values).zip(places)
        {
            codes.push(value.code);
            if *storage != Storage::Out {
                copies_in.push((value.slot, *slot, ty.slots()));
            }
            if let Some(place) = place {
                copies_out.push(Self::store(place, *slot));
            }
        }
        let body = function.body.clone();
        let return_slot = function.return_slot;
        Value {
            ty: returns,
            slot: out,
            constant: false,
            code: Some(Box::new(move |m: &mut Machine| {
                for code in &codes {
                    run(code, m);
                }
                for &(from, to, count) in &copies_in {
                    m.copy_all(from, to, count);
                }
                let (mask, returned, broken, continued) =
                    (m.mask, m.returned, m.broken, m.continued);
                m.returned = 0;
                m.broken = 0;
                m.continued = 0;
                if let Some(body) = body.get() {
                    body(m);
                }
                m.mask = mask & !m.discarded & ALL;
                m.returned = returned;
                m.broken = broken;
                m.continued = continued;
                for copy in &copies_out {
                    copy(m);
                }
                m.copy_all(return_slot, out, size);
            })),
        }
    }

    // constructors: conversions, vectors and matrices from components, structs and arrays
    fn construct(&mut self, ty: Ty, values: Vec<Value>) -> Result<Value> {
        let constant = values.iter().all(|value| value.constant);
        let size = ty.slots();
        let out = self.allocate(size);
        let mut values = values;
        let codes: Vec<Option<Op>> = values.iter_mut().map(|value| value.code.take()).collect();
        let fail = |compiler: &Compiler| {
            compiler.fail::<Value>(format!("'{}' : constructor has wrong arguments", ty.name()))
        };
        if values.is_empty() {
            return fail(self);
        }
        // each output word as (source slot, source base, or a fixed word)
        enum Source {
            Slot(usize, Base),
            Word(u32),
        }
        let mut sources: Vec<Source> = Vec::new();
        match &ty {
            Ty::Vec(base, count) => {
                let count = usize::from(*count);
                if values.len() == 1 && values[0].ty.slots() == 1 {
                    for _ in 0..count {
                        sources.push(Source::Slot(
                            values[0].slot,
                            values[0].ty.base().unwrap_or(*base),
                        ));
                    }
                } else {
                    for value in &values {
                        let Some(from) = value.ty.base() else {
                            return fail(self);
                        };
                        for index in 0..value.ty.slots() {
                            if sources.len() < count {
                                sources.push(Source::Slot(value.slot + index, from));
                            }
                        }
                    }
                    if sources.len() < count {
                        return fail(self);
                    }
                }
                let target = *base;
                let sources: Vec<(usize, Base)> = sources
                    .into_iter()
                    .map(|source| match source {
                        Source::Slot(slot, base) => (slot, base),
                        Source::Word(_) => unreachable!("vectors take slots"),
                    })
                    .collect();
                return Ok(Value {
                    ty,
                    slot: out,
                    constant,
                    code: Some(Box::new(move |m: &mut Machine| {
                        for code in &codes {
                            run(code, m);
                        }
                        for (index, (slot, from)) in sources.iter().enumerate() {
                            for lane in 0..LANES {
                                let word = convert(m.get(*slot, lane), *from, target);
                                m.set(out + index, lane, word);
                            }
                        }
                    })),
                });
            }
            Ty::Mat(columns, rows) => {
                let (columns, rows) = (usize::from(*columns), usize::from(*rows));
                if values.len() == 1 && values[0].ty.slots() == 1 {
                    for column in 0..columns {
                        for row in 0..rows {
                            sources.push(if column == row {
                                Source::Slot(
                                    values[0].slot,
                                    values[0].ty.base().unwrap_or(Base::Float),
                                )
                            } else {
                                Source::Word(0)
                            });
                        }
                    }
                } else if values.len() == 1
                    && let Ty::Mat(from_columns, from_rows) = values[0].ty
                {
                    let (from_columns, from_rows) =
                        (usize::from(from_columns), usize::from(from_rows));
                    for column in 0..columns {
                        for row in 0..rows {
                            sources.push(if column < from_columns && row < from_rows {
                                Source::Slot(values[0].slot + column * from_rows + row, Base::Float)
                            } else {
                                Source::Word(of_f32(if column == row { 1.0 } else { 0.0 }))
                            });
                        }
                    }
                } else {
                    for value in &values {
                        let Some(from) =
                            value.ty.base().filter(|_| matches!(value.ty, Ty::Vec(..)))
                        else {
                            return fail(self);
                        };
                        for index in 0..value.ty.slots() {
                            sources.push(Source::Slot(value.slot + index, from));
                        }
                    }
                    if sources.len() != columns * rows {
                        return fail(self);
                    }
                }
            }
            Ty::Struct(def) => {
                if values.len() != def.fields.len()
                    || def
                        .fields
                        .iter()
                        .zip(&values)
                        .any(|((_, ty), value)| *ty != value.ty)
                {
                    return fail(self);
                }
                for value in &values {
                    for index in 0..value.ty.slots() {
                        sources.push(Source::Slot(value.slot + index, Base::Uint));
                    }
                }
            }
            Ty::Array(element, length) => {
                if values.len() != *length || values.iter().any(|value| value.ty != **element) {
                    return fail(self);
                }
                for value in &values {
                    for index in 0..value.ty.slots() {
                        sources.push(Source::Slot(value.slot + index, Base::Uint));
                    }
                }
            }
            _ => return fail(self),
        }
        let sources: Vec<(Option<(usize, Base)>, u32)> = sources
            .into_iter()
            .map(|source| match source {
                Source::Slot(slot, base) => (Some((slot, base)), 0),
                Source::Word(word) => (None, word),
            })
            .collect();
        let target = ty.base().unwrap_or(Base::Uint);
        Ok(Value {
            ty,
            slot: out,
            constant,
            code: Some(Box::new(move |m: &mut Machine| {
                for code in &codes {
                    run(code, m);
                }
                for (index, (from, word)) in sources.iter().enumerate() {
                    for lane in 0..LANES {
                        let value = match from {
                            Some((slot, base)) if target == Base::Float => {
                                convert(m.get(*slot, lane), *base, Base::Float)
                            }
                            Some((slot, _)) => m.get(*slot, lane),
                            None => *word,
                        };
                        m.set(out + index, lane, value);
                    }
                }
            })),
        })
    }
}

impl Compiler {
    fn declare_variables(&mut self, declaration: &Declaration, global: bool) -> Result<Option<Op>> {
        let base_ty = self.resolve_type(&declaration.ty)?;
        let storage = declaration.qualifiers.storage;
        let mut codes: Vec<Op> = Vec::new();
        for variable in &declaration.variables {
            self.line = variable.line;
            let mut ty = base_ty.clone();
            if let Some(size) = &declaration.type_array {
                let length = match size {
                    Some(size) => self.array_size(size)?,
                    None => return self.fail("unsized array declaration"),
                };
                ty = Ty::Array(Box::new(ty), length);
            }
            let init = match &variable.init {
                Some(init) => Some(self.expression(init)?),
                None => None,
            };
            if let Some(size) = &variable.array {
                let length = match size {
                    Some(size) => self.array_size(size)?,
                    None => match &init {
                        Some(Value {
                            ty: Ty::Array(_, length),
                            ..
                        }) => *length,
                        _ => return self.fail("unsized array declaration"),
                    },
                };
                ty = Ty::Array(Box::new(ty), length);
            }
            if matches!(ty, Ty::Void) {
                return self.fail(format!("'{}' : illegal use of type 'void'", variable.name));
            }
            if ty.has_sampler() && !matches!(storage, Storage::Uniform) {
                return self.fail(format!("'{}' : samplers must be uniform", variable.name));
            }
            let slot = self.allocate(ty.slots());
            let constant = storage == Storage::Const;
            if let Some(init) = init {
                if init.ty != ty {
                    return self.fail(format!(
                        "'=' : cannot convert from '{}' to '{}'",
                        init.ty.name(),
                        ty.name()
                    ));
                }
                if matches!(
                    storage,
                    Storage::Uniform
                        | Storage::Attribute
                        | Storage::Varying
                        | Storage::In
                        | Storage::Out
                ) {
                    return self.fail(format!(
                        "'{}' : cannot initialize this type of qualifier",
                        variable.name
                    ));
                }
                if constant {
                    let Some(words) = self.evaluate(&init) else {
                        return self.fail(format!(
                            "'{}' : assigning non-constant to 'const'",
                            variable.name
                        ));
                    };
                    for (index, word) in words.iter().enumerate() {
                        for lane in 0..LANES {
                            self.initial[(slot + index) * LANES + lane] = *word;
                        }
                    }
                } else {
                    let size = ty.slots();
                    let from = init.slot;
                    let compute = init.code;
                    codes.push(Box::new(move |m: &mut Machine| {
                        run(&compute, m);
                        let mask = if global { ALL } else { m.mask };
                        m.copy_masked(from, slot, size, mask);
                    }));
                }
            } else if constant {
                return self.fail(format!(
                    "'{}' : variables with qualifier 'const' must be initialized",
                    variable.name
                ));
            }
            let interface = Interface {
                name: variable.name.clone(),
                ty: ty.clone(),
                slot,
                location: declaration.qualifiers.location,
                flat: declaration.qualifiers.flat,
                used: false,
                block: None,
            };
            match (storage, self.stage) {
                (Storage::Uniform, _) => self.uniforms.push(interface),
                (Storage::Attribute, Stage::Vertex) | (Storage::In, Stage::Vertex) => {
                    self.inputs.push(interface)
                }
                (Storage::Varying, Stage::Vertex) | (Storage::Out, _) => {
                    self.outputs.push(interface)
                }
                (Storage::Varying, Stage::Fragment) | (Storage::In, Stage::Fragment) => {
                    self.inputs.push(interface)
                }
                (Storage::Attribute, Stage::Fragment) => {
                    return self.fail("'attribute' : supported in vertex shaders only");
                }
                _ => {}
            }
            self.declare(
                &variable.name,
                Var {
                    ty,
                    slot,
                    storage,
                    constant,
                    builtin_readonly: false,
                },
            )?;
        }
        Ok(match codes.len() {
            0 => None,
            1 => codes.pop(),
            _ => Some(Box::new(move |m: &mut Machine| {
                for code in &codes {
                    code(m);
                }
            })),
        })
    }

    fn statements(&mut self, statements: &[Stmt]) -> Result<Option<Op>> {
        let mut codes = Vec::new();
        for statement in statements {
            if let Some(code) = self.statement(statement)? {
                codes.push(code);
            }
        }
        Ok(match codes.len() {
            0 => None,
            1 => codes.pop(),
            _ => Some(Box::new(move |m: &mut Machine| {
                for code in &codes {
                    if m.mask == 0 {
                        return;
                    }
                    code(m);
                }
            })),
        })
    }

    fn condition(&mut self, expr: &Expr) -> Result<Value> {
        let value = self.expression(expr)?;
        if value.ty != BOOL {
            return self.fail("boolean expression expected");
        }
        Ok(value)
    }

    fn statement(&mut self, statement: &Stmt) -> Result<Option<Op>> {
        match statement {
            Stmt::Empty => Ok(None),
            Stmt::Expr(expr) => Ok(self.expression(expr)?.code),
            Stmt::Declare(declaration) => {
                if declaration.variables.is_empty() {
                    if let TypeSpec::Struct(_) = &declaration.ty {
                        self.resolve_type(&declaration.ty)?;
                    }
                    return Ok(None);
                }
                self.declare_variables(declaration, false)
            }
            Stmt::Block(statements) => {
                self.scopes.push(HashMap::new());
                self.structs.push(HashMap::new());
                let code = self.statements(statements);
                self.scopes.pop();
                self.structs.pop();
                code
            }
            Stmt::If(condition, then, otherwise) => {
                let condition = self.condition(condition)?;
                self.scopes.push(HashMap::new());
                let then = self.statement(then)?;
                self.scopes.pop();
                self.scopes.push(HashMap::new());
                let otherwise = match otherwise {
                    Some(otherwise) => self.statement(otherwise)?,
                    None => None,
                };
                self.scopes.pop();
                let test = condition.slot;
                let compute = condition.code;
                Ok(Some(Box::new(move |m: &mut Machine| {
                    run(&compute, m);
                    let saved = m.mask;
                    let mut chosen = 0;
                    for lane in 0..LANES {
                        if m.get(test, lane) != 0 {
                            chosen |= 1 << lane;
                        }
                    }
                    m.mask = saved & chosen;
                    if m.mask != 0 {
                        run(&then, m);
                    }
                    m.mask = saved & !chosen & m.alive();
                    if m.mask != 0 {
                        run(&otherwise, m);
                    }
                    m.mask = saved & m.alive();
                })))
            }
            Stmt::While(condition, body) => self.looping(None, Some(condition), None, body, false),
            Stmt::DoWhile(body, condition) => self.looping(None, Some(condition), None, body, true),
            Stmt::For(init, condition, step, body) => {
                self.scopes.push(HashMap::new());
                let init = match init {
                    Some(init) => self.statement(init)?,
                    None => None,
                };
                let looped = self.looping(init, condition.as_ref(), step.as_ref(), body, false);
                self.scopes.pop();
                looped
            }
            Stmt::Break => {
                if self.loops == 0 && self.switches == 0 {
                    return self.fail(
                        "'break' : break statement only allowed in loops and switch statements",
                    );
                }
                Ok(Some(Box::new(|m: &mut Machine| {
                    m.broken |= m.mask;
                    m.mask = 0;
                })))
            }
            Stmt::Continue => {
                if self.loops == 0 {
                    return self.fail("'continue' : continue statement only allowed in loops");
                }
                Ok(Some(Box::new(|m: &mut Machine| {
                    m.continued |= m.mask;
                    m.mask = 0;
                })))
            }
            Stmt::Discard => {
                if self.stage != Stage::Fragment {
                    return self.fail("'discard' : supported in fragment shaders only");
                }
                Ok(Some(Box::new(|m: &mut Machine| {
                    m.discarded |= m.mask;
                    m.mask = 0;
                })))
            }
            Stmt::Return(value, line) => {
                self.line = *line;
                let Some((returns, slot)) = self.returns.clone() else {
                    return self.fail("'return' : outside a function");
                };
                let value = match value {
                    Some(value) => Some(self.expression(value)?),
                    None => None,
                };
                match &value {
                    None if returns != Ty::Void => {
                        return self.fail("'return' : non-void function must return a value");
                    }
                    Some(value) if value.ty != returns => {
                        return self.fail("'return' : function return is not matching type");
                    }
                    _ => {}
                }
                let size = returns.slots();
                let (from, compute) = value
                    .map(|value| (value.slot, value.code))
                    .unwrap_or((0, None));
                Ok(Some(Box::new(move |m: &mut Machine| {
                    run(&compute, m);
                    if size > 0 {
                        let mask = m.mask;
                        m.copy_masked(from, slot, size, mask);
                    }
                    m.returned |= m.mask;
                    m.mask = 0;
                })))
            }
            Stmt::Switch(selector, items) => self.switch(selector, items),
        }
    }

    fn looping(
        &mut self,
        init: Option<Op>,
        condition: Option<&Expr>,
        step: Option<&Expr>,
        body: &Stmt,
        test_after: bool,
    ) -> Result<Option<Op>> {
        let condition = match condition {
            Some(condition) => Some(self.condition(condition)?),
            None => None,
        };
        let step = match step {
            Some(step) => self.expression(step)?.code,
            None => None,
        };
        self.loops += 1;
        self.scopes.push(HashMap::new());
        let body = self.statement(body)?;
        self.scopes.pop();
        self.loops -= 1;
        let (test, test_code) = match condition {
            Some(condition) => (Some(condition.slot), condition.code),
            None => (None, None),
        };
        Ok(Some(Box::new(move |m: &mut Machine| {
            run(&init, m);
            let entry = m.mask;
            let (saved_broken, saved_continued) = (m.broken, m.continued);
            m.broken = 0;
            let mut active = entry;
            let mut first = true;
            loop {
                m.mask = active & !m.broken & !m.returned & !m.discarded & ALL;
                if !(first && test_after)
                    && let Some(test) = test
                {
                    run(&test_code, m);
                    let mut passing = 0;
                    for lane in 0..LANES {
                        if m.get(test, lane) != 0 {
                            passing |= 1 << lane;
                        }
                    }
                    m.mask &= passing;
                }
                first = false;
                if m.mask == 0 || m.budget == 0 {
                    break;
                }
                m.budget -= 1;
                m.continued = 0;
                run(&body, m);
                m.mask = (m.mask | m.continued) & !m.broken & !m.returned & !m.discarded & ALL;
                m.continued = 0;
                if m.mask != 0 {
                    run(&step, m);
                }
                active = m.mask;
            }
            m.broken = saved_broken;
            m.continued = saved_continued;
            m.mask = entry & !m.returned & !m.discarded & ALL;
        })))
    }

    fn switch(&mut self, selector: &Expr, items: &[SwitchItem]) -> Result<Option<Op>> {
        let selector = self.expression(selector)?;
        if !matches!(selector.ty, Ty::Vec(Base::Int | Base::Uint, 1)) {
            return self
                .fail("'switch' : init-expression in a switch statement must be a scalar integer");
        }
        self.switches += 1;
        self.scopes.push(HashMap::new());
        // each case label and the statements from it to the next
        let mut labels: Vec<(Option<u32>, usize)> = Vec::new();
        let mut bodies: Vec<Option<Op>> = Vec::new();
        for item in items {
            match item {
                SwitchItem::Case(value) => {
                    let value = self.expression(value)?;
                    let Some(words) = self.evaluate(&value) else {
                        return self
                            .fail("'case' : case label must be a constant integer expression");
                    };
                    labels.push((Some(words[0]), bodies.len()));
                }
                SwitchItem::Default => labels.push((None, bodies.len())),
                SwitchItem::Stmt(statement) => bodies.push(self.statement(statement)?),
            }
        }
        self.scopes.pop();
        self.switches -= 1;
        let slot = selector.slot;
        let compute = selector.code;
        Ok(Some(Box::new(move |m: &mut Machine| {
            run(&compute, m);
            let entry = m.mask;
            let saved_broken = m.broken;
            m.broken = 0;
            // lanes that have entered the switch body, by the first statement they run
            let mut entering = vec![0_u8; bodies.len() + 1];
            for lane in 0..LANES {
                if entry & (1 << lane) == 0 {
                    continue;
                }
                let word = m.get(slot, lane);
                let target = labels
                    .iter()
                    .find(|(value, _)| *value == Some(word))
                    .or_else(|| labels.iter().find(|(value, _)| value.is_none()))
                    .map(|(_, start)| *start);
                if let Some(start) = target {
                    entering[start] |= 1 << lane;
                }
            }
            let mut active = 0;
            for (index, body) in bodies.iter().enumerate() {
                active |= entering[index];
                active &= !m.broken & !m.returned & !m.discarded & !m.continued & ALL;
                if active != 0 {
                    m.mask = active;
                    run(body, m);
                    active = m.mask;
                }
            }
            m.broken = saved_broken;
            m.mask = entry & m.alive();
        })))
    }

    fn function_definition(&mut self, function: &super::parse::Function) -> Result<()> {
        self.line = function.line;
        let returns = self.resolve_type(&function.returns)?;
        let mut parameters = Vec::new();
        for Parameter {
            storage, ty, array, ..
        } in &function.parameters
        {
            let mut ty = self.resolve_type(ty)?;
            if let Some(size) = array {
                ty = Ty::Array(Box::new(ty), self.array_size(size)?);
            }
            parameters.push((ty, *storage));
        }
        if function.name == "main" && (returns != Ty::Void || !parameters.is_empty()) {
            return self.fail("'main' : function must have no parameters and return void");
        }
        let existing = self.functions.get(&function.name).and_then(|overloads| {
            overloads
                .iter()
                .find(|found| {
                    found.parameters.len() == parameters.len()
                        && found
                            .parameters
                            .iter()
                            .zip(&parameters)
                            .all(|((a, ..), (b, _))| a == b)
                })
                .cloned()
        });
        let declared = match existing {
            Some(found) => {
                if found.returns != returns {
                    return self.fail(format!(
                        "'{}' : overloaded functions must have the same return type",
                        function.name
                    ));
                }
                found
            }
            None => {
                let mut slotted = Vec::new();
                for (ty, storage) in &parameters {
                    let slot = self.allocate(ty.slots());
                    slotted.push((ty.clone(), *storage, slot));
                }
                let return_slot = self.allocate(returns.slots());
                let made = Rc::new(Function {
                    parameters: slotted,
                    returns: returns.clone(),
                    return_slot,
                    body: Rc::new(OnceCell::new()),
                });
                self.functions
                    .entry(function.name.clone())
                    .or_default()
                    .push(made.clone());
                made
            }
        };
        let Some(body) = &function.body else {
            return Ok(());
        };
        if declared.body.get().is_some() {
            return self.fail(format!("'{}' : function already has a body", function.name));
        }
        self.scopes.push(HashMap::new());
        self.structs.push(HashMap::new());
        for (parameter, (ty, storage, slot)) in function.parameters.iter().zip(&declared.parameters)
        {
            if let Some(name) = &parameter.name {
                self.declare(
                    name,
                    Var {
                        ty: ty.clone(),
                        slot: *slot,
                        storage: if *storage == Storage::In {
                            Storage::Local
                        } else {
                            *storage
                        },
                        constant: parameter.constant,
                        builtin_readonly: false,
                    },
                )?;
            }
        }
        self.returns = Some((returns, declared.return_slot));
        let code = self.statements(body)?;
        self.returns = None;
        self.scopes.pop();
        self.structs.pop();
        let code: Op = match code {
            Some(code) => code,
            None => Box::new(|_: &mut Machine| {}),
        };
        let _ = declared.body.set(code);
        Ok(())
    }

    fn builtin_variables(&mut self) {
        let add = |compiler: &mut Compiler, name: &'static str, ty: Ty, readonly: bool| {
            let slot = compiler.allocate(ty.slots());
            compiler.builtins.insert(name, slot);
            compiler.scopes[0].insert(
                name.to_string(),
                Var {
                    ty,
                    slot,
                    storage: Storage::Local,
                    constant: false,
                    builtin_readonly: readonly,
                },
            );
        };
        match self.stage {
            Stage::Vertex => {
                add(self, "gl_Position", Ty::Vec(Base::Float, 4), false);
                add(self, "gl_PointSize", FLOAT, false);
                if self.version >= 300 {
                    add(self, "gl_VertexID", INT, true);
                    add(self, "gl_InstanceID", INT, true);
                }
            }
            Stage::Fragment => {
                add(self, "gl_FragCoord", Ty::Vec(Base::Float, 4), true);
                add(self, "gl_FrontFacing", BOOL, true);
                add(self, "gl_PointCoord", Ty::Vec(Base::Float, 2), true);
                if self.version >= 300 {
                    add(self, "gl_FragDepth", FLOAT, false);
                } else {
                    add(self, "gl_FragColor", Ty::Vec(Base::Float, 4), false);
                    let draw_buffers = if self
                        .extensions
                        .iter()
                        .any(|name| name == "GL_EXT_draw_buffers")
                    {
                        8
                    } else {
                        1
                    };
                    add(
                        self,
                        "gl_FragData",
                        Ty::Array(Box::new(Ty::Vec(Base::Float, 4)), draw_buffers),
                        false,
                    );
                    if self
                        .extensions
                        .iter()
                        .any(|name| name == "GL_EXT_frag_depth")
                    {
                        add(self, "gl_FragDepthEXT", FLOAT, false);
                    }
                }
            }
        }
        let limits: [(&str, u32); 9] = [
            ("gl_MaxVertexAttribs", 16),
            ("gl_MaxVertexUniformVectors", 4096),
            ("gl_MaxVaryingVectors", 30),
            ("gl_MaxVertexTextureImageUnits", 16),
            ("gl_MaxCombinedTextureImageUnits", 32),
            ("gl_MaxTextureImageUnits", 16),
            ("gl_MaxFragmentUniformVectors", 1024),
            ("gl_MaxDrawBuffers", 8),
            ("gl_MinProgramTexelOffset", (-8_i32) as u32),
        ];
        for (name, value) in limits {
            let constant = self.constant(INT, &[value]);
            self.scopes[0].insert(
                name.to_string(),
                Var {
                    ty: INT,
                    slot: constant.slot,
                    storage: Storage::Const,
                    constant: true,
                    builtin_readonly: true,
                },
            );
        }
    }
}

// compiles a shader's source into a module, or fails with an info log in the style drivers
// write
pub(crate) fn compile(source: &str, stage: Stage, extensions_allowed: &[&str]) -> Result<Module> {
    let preprocessed = super::lex::preprocess(source, extensions_allowed)
        .map_err(|message| format!("ERROR: 0:{message}\n"))?;
    let mut parser = super::parse::Parser::new(preprocessed.tokens);
    let externals = parser
        .translation_unit()
        .map_err(|message| format!("ERROR: 0:{message}\n"))?;
    let mut compiler = Compiler {
        stage,
        version: preprocessed.version,
        extensions: preprocessed.extensions,
        initial: Vec::new(),
        scopes: vec![HashMap::new()],
        functions: HashMap::new(),
        structs: vec![HashMap::new()],
        returns: None,
        loops: 0,
        switches: 0,
        inputs: Vec::new(),
        outputs: Vec::new(),
        uniforms: Vec::new(),
        builtins: HashMap::new(),
        used_builtins: Vec::new(),
        line: 1,
    };
    compiler.builtin_variables();
    compiler.scopes.push(HashMap::new());
    let mut global_code: Vec<Op> = Vec::new();
    let result: Result<()> = (|| {
        for external in &externals {
            match external {
                External::Function(function) => compiler.function_definition(function)?,
                External::Declare(declaration) => {
                    if declaration.variables.is_empty() {
                        if let TypeSpec::Struct(_) = &declaration.ty {
                            compiler.resolve_type(&declaration.ty)?;
                        }
                        continue;
                    }
                    if matches!(declaration.qualifiers.storage, Storage::In | Storage::Out)
                        && compiler.version < 300
                    {
                        return compiler.fail("'in'/'out' qualifiers on globals need GLSL ES 3.00");
                    }
                    if matches!(
                        declaration.qualifiers.storage,
                        Storage::Attribute | Storage::Varying
                    ) && compiler.version >= 300
                    {
                        return compiler
                            .fail("'attribute'/'varying' are not allowed in GLSL ES 3.00");
                    }
                    if let Some(code) = compiler.declare_variables(declaration, true)? {
                        global_code.push(code);
                    }
                }
                External::Block(block) => compiler.uniform_block(block)?,
            }
        }
        Ok(())
    })();
    result.map_err(|message| format!("{message}\n"))?;
    let main = compiler
        .functions
        .get("main")
        .and_then(|overloads| overloads.first().cloned())
        .filter(|main| main.body.get().is_some());
    let Some(main) = main else {
        return Err("ERROR: 0:0: 'main' : function not defined\n".into());
    };
    let body = main.body.clone();
    let entry: Op = Box::new(move |m: &mut Machine| {
        for code in &global_code {
            code(m);
        }
        if let Some(body) = body.get() {
            body(m);
        }
    });
    Ok(Module {
        used_builtins: compiler.used_builtins,
        extensions: compiler.extensions,
        version: compiler.version,
        main: Rc::new(entry),
        initial: compiler.initial,
        inputs: compiler.inputs,
        outputs: compiler.outputs,
        uniforms: compiler.uniforms,
        builtins: compiler.builtins,
    })
}

impl Compiler {
    fn note_builtin(&mut self, slot: usize) {
        if let Some((&name, _)) = self.builtins.iter().find(|(_, found)| **found == slot)
            && !self.used_builtins.contains(&name)
        {
            self.used_builtins.push(name);
        }
    }

    // a uniform block: its members become uniforms, under the instance name when it has one
    fn uniform_block(&mut self, block: &super::parse::Block) -> Result<()> {
        if block.storage != Storage::Uniform {
            return self.fail("only uniform blocks are supported");
        }
        let mut fields = Vec::new();
        for (ty, name, array) in &block.members {
            let mut ty = self.resolve_type(ty)?;
            if let Some(size) = array {
                ty = Ty::Array(Box::new(ty), self.array_size(size)?);
            }
            fields.push((name.clone(), ty));
        }
        match &block.instance {
            Some((instance, _)) => {
                let def = Rc::new(StructTy {
                    name: block.name.clone(),
                    fields,
                });
                let ty = Ty::Struct(def);
                let slot = self.allocate(ty.slots());
                self.uniforms.push(Interface {
                    name: instance.clone(),
                    ty: ty.clone(),
                    slot,
                    location: None,
                    flat: false,
                    used: false,
                    block: Some(block.name.as_str().into()),
                });
                self.declare(
                    instance,
                    Var {
                        ty,
                        slot,
                        storage: Storage::Uniform,
                        constant: false,
                        builtin_readonly: false,
                    },
                )
            }
            None => {
                for (name, ty) in fields {
                    let slot = self.allocate(ty.slots());
                    self.uniforms.push(Interface {
                        name: name.clone(),
                        ty: ty.clone(),
                        slot,
                        location: None,
                        flat: false,
                        used: false,
                        block: Some(block.name.as_str().into()),
                    });
                    self.declare(
                        &name,
                        Var {
                            ty,
                            slot,
                            storage: Storage::Uniform,
                            constant: false,
                            builtin_readonly: false,
                        },
                    )?;
                }
                Ok(())
            }
        }
    }
}
