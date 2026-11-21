// the GLSL ES built-in functions: componentwise math on floats and integers, geometry, matrix
// functions, vector relations, derivatives taken across a quad's lanes, and texture lookups

use super::super::exec::{LANES, Lod, Machine, Op, SamplerKind};
use super::{
    BOOL, Base, Compiler, FLOAT, INT, Result, Stage, Ty, UINT, Value, as_f32, of_f32, run,
};

// the argument codes and slots a built-in reads
fn parts(values: Vec<Value>) -> (Vec<Option<Op>>, Vec<(usize, Ty)>, bool) {
    let constant = values.iter().all(|value| value.constant);
    let mut codes = Vec::new();
    let mut slots = Vec::new();
    for value in values {
        slots.push((value.slot, value.ty));
        codes.push(value.code);
    }
    (codes, slots, constant)
}

fn is_gen_float(ty: &Ty) -> bool {
    matches!(ty, Ty::Vec(Base::Float, _))
}

fn round_even(value: f32) -> f32 {
    let rounded = value.round();
    if (value - value.trunc()).abs() == 0.5 && rounded % 2.0 != 0.0 {
        rounded - value.signum()
    } else {
        rounded
    }
}

fn to_half(value: f32) -> u32 {
    let bits = value.to_bits();
    let sign = (bits >> 16) & 0x8000;
    let exponent = ((bits >> 23) & 0xff) as i32;
    let mantissa = bits & 0x7f_ffff;
    if exponent == 0xff {
        return sign | 0x7c00 | if mantissa != 0 { 0x200 } else { 0 };
    }
    let half_exponent = exponent - 127 + 15;
    if half_exponent >= 0x1f {
        return sign | 0x7c00;
    }
    if half_exponent <= 0 {
        if half_exponent < -10 {
            return sign;
        }
        let mantissa = mantissa | 0x80_0000;
        return sign | (mantissa >> (14 - half_exponent));
    }
    sign | ((half_exponent as u32) << 10) | (mantissa >> 13)
}

fn from_half(half: u32) -> f32 {
    let sign = if half & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exponent = ((half >> 10) & 0x1f) as i32;
    let mantissa = (half & 0x3ff) as f32;
    sign * match exponent {
        0 => mantissa * 2f32.powi(-24),
        0x1f if mantissa == 0.0 => f32::INFINITY,
        0x1f => f32::NAN,
        _ => (1.0 + mantissa / 1024.0) * 2f32.powi(exponent - 15),
    }
}

impl Compiler {
    // a function of floats applied per component, where a scalar argument spreads over the
    // vector size of the result
    fn float_map(
        &mut self,
        values: Vec<Value>,
        f: impl Fn(&[f32]) -> f32 + Send + Sync + 'static,
    ) -> Value {
        let size = values
            .iter()
            .map(|value| value.ty.size())
            .max()
            .unwrap_or(1);
        let (codes, slots, constant) = parts(values);
        let out = self.allocate(usize::from(size));
        let sources: Vec<(usize, bool)> = slots
            .iter()
            .map(|(slot, ty)| (*slot, ty.size() == 1))
            .collect();
        Value {
            ty: Ty::Vec(Base::Float, size),
            slot: out,
            constant,
            code: Some(Box::new(move |m: &mut Machine| {
                for code in &codes {
                    run(code, m);
                }
                let mut arguments = [0.0_f32; 3];
                for component in 0..usize::from(size) {
                    for lane in 0..LANES {
                        for (index, (slot, spread)) in sources.iter().enumerate() {
                            arguments[index] =
                                m.float(slot + if *spread { 0 } else { component }, lane);
                        }
                        let value = f(&arguments[..sources.len()]);
                        m.set_float(out + component, lane, value);
                    }
                }
            })),
        }
    }

    // a function of words per component, for integers, with the result's base given
    fn word_map(
        &mut self,
        values: Vec<Value>,
        result: Base,
        f: impl Fn(&[u32]) -> u32 + Send + Sync + 'static,
    ) -> Value {
        let size = values
            .iter()
            .map(|value| value.ty.size())
            .max()
            .unwrap_or(1);
        let (codes, slots, constant) = parts(values);
        let out = self.allocate(usize::from(size));
        let sources: Vec<(usize, bool)> = slots
            .iter()
            .map(|(slot, ty)| (*slot, ty.size() == 1))
            .collect();
        Value {
            ty: Ty::Vec(result, size),
            slot: out,
            constant,
            code: Some(Box::new(move |m: &mut Machine| {
                for code in &codes {
                    run(code, m);
                }
                let mut arguments = [0_u32; 3];
                for component in 0..usize::from(size) {
                    for lane in 0..LANES {
                        for (index, (slot, spread)) in sources.iter().enumerate() {
                            arguments[index] =
                                m.get(slot + if *spread { 0 } else { component }, lane);
                        }
                        let value = f(&arguments[..sources.len()]);
                        m.set(out + component, lane, value);
                    }
                }
            })),
        }
    }

    // a whole-vector function of floats producing a fixed number of floats
    fn vector_function(
        &mut self,
        values: Vec<Value>,
        result: Ty,
        f: impl Fn(&[&[f32]], &mut [f32]) + Send + Sync + 'static,
    ) -> Value {
        let count = result.slots();
        let (codes, slots, constant) = parts(values);
        let out = self.allocate(count);
        let sizes: Vec<(usize, usize)> =
            slots.iter().map(|(slot, ty)| (*slot, ty.slots())).collect();
        Value {
            ty: result,
            slot: out,
            constant,
            code: Some(Box::new(move |m: &mut Machine| {
                for code in &codes {
                    run(code, m);
                }
                // a matrix has at most 16 components, and no function takes more than 3 arguments
                let mut output = [0.0_f32; 16];
                let mut storage = [[0.0_f32; 16]; 3];
                for lane in 0..LANES {
                    for (argument, (slot, size)) in storage.iter_mut().zip(&sizes) {
                        for (index, value) in argument.iter_mut().enumerate().take(*size) {
                            *value = m.float(slot + index, lane);
                        }
                    }
                    let arguments: [&[f32]; 3] = std::array::from_fn(|index| {
                        sizes
                            .get(index)
                            .map_or(&[][..], |(_, size)| &storage[index][..*size])
                    });
                    f(&arguments[..sizes.len()], &mut output[..count]);
                    for (index, value) in output[..count].iter().enumerate() {
                        m.set_float(out + index, lane, *value);
                    }
                }
            })),
        }
    }

    fn check(&self, valid: bool, name: &str) -> Result<()> {
        if valid {
            Ok(())
        } else {
            self.fail(format!("'{name}' : no matching overloaded function found"))
        }
    }

    pub(crate) fn builtin(&mut self, name: &str, values: Vec<Value>) -> Result<Value> {
        let types: Vec<Ty> = values.iter().map(|value| value.ty.clone()).collect();
        let count = values.len();
        let es3 = self.version >= 300;
        let same_or_scalar = |index: usize| types[index] == types[0] || types[index] == FLOAT;
        let unary: Option<fn(f32) -> f32> = match name {
            "radians" => Some(|x| x.to_radians()),
            "degrees" => Some(|x| x.to_degrees()),
            "sin" => Some(f32::sin),
            "cos" => Some(f32::cos),
            "tan" => Some(f32::tan),
            "asin" => Some(f32::asin),
            "acos" => Some(f32::acos),
            "exp" => Some(f32::exp),
            "log" => Some(f32::ln),
            "exp2" => Some(f32::exp2),
            "log2" => Some(f32::log2),
            "sqrt" => Some(f32::sqrt),
            "inversesqrt" => Some(|x| 1.0 / x.sqrt()),
            "floor" => Some(f32::floor),
            "ceil" => Some(f32::ceil),
            "fract" => Some(|x| x - x.floor()),
            "sinh" if es3 => Some(f32::sinh),
            "cosh" if es3 => Some(f32::cosh),
            "tanh" if es3 => Some(f32::tanh),
            "asinh" if es3 => Some(f32::asinh),
            "acosh" if es3 => Some(f32::acosh),
            "atanh" if es3 => Some(f32::atanh),
            "trunc" if es3 => Some(f32::trunc),
            "round" if es3 => Some(f32::round),
            "roundEven" if es3 => Some(round_even),
            _ => None,
        };
        if let Some(f) = unary {
            self.check(count == 1 && is_gen_float(&types[0]), name)?;
            return Ok(self.float_map(values, move |x| f(x[0])));
        }
        match name {
            "abs" | "sign" => {
                self.check(count == 1, name)?;
                match &types[0] {
                    Ty::Vec(Base::Float, _) => {
                        let abs = name == "abs";
                        Ok(self.float_map(values, move |x| {
                            if abs {
                                x[0].abs()
                            } else if x[0] > 0.0 {
                                1.0
                            } else if x[0] < 0.0 {
                                -1.0
                            } else {
                                0.0
                            }
                        }))
                    }
                    Ty::Vec(Base::Int, _) if es3 => {
                        let abs = name == "abs";
                        Ok(self.word_map(values, Base::Int, move |x| {
                            let value = x[0] as i32;
                            (if abs {
                                value.wrapping_abs()
                            } else {
                                value.signum()
                            }) as u32
                        }))
                    }
                    _ => self.check(false, name).map(|_| unreachable!()),
                }
            }
            "atan" => {
                self.check(
                    is_gen_float(&types[0]) && (count == 1 || (count == 2 && types[1] == types[0])),
                    name,
                )?;
                Ok(if count == 1 {
                    self.float_map(values, |x| x[0].atan())
                } else {
                    self.float_map(values, |x| x[0].atan2(x[1]))
                })
            }
            "pow" => {
                self.check(
                    count == 2 && is_gen_float(&types[0]) && types[1] == types[0],
                    name,
                )?;
                Ok(self.float_map(values, |x| x[0].powf(x[1])))
            }
            "mod" => {
                self.check(
                    count == 2 && is_gen_float(&types[0]) && same_or_scalar(1),
                    name,
                )?;
                Ok(self.float_map(values, |x| x[0] - x[1] * (x[0] / x[1]).floor()))
            }
            "min" | "max" => {
                self.check(count == 2, name)?;
                let minimum = name == "min";
                match &types[0] {
                    Ty::Vec(Base::Float, _) => {
                        self.check(same_or_scalar(1), name)?;
                        Ok(self.float_map(values, move |x| {
                            if minimum {
                                x[0].min(x[1])
                            } else {
                                x[0].max(x[1])
                            }
                        }))
                    }
                    Ty::Vec(base @ (Base::Int | Base::Uint), _) if es3 => {
                        let base = *base;
                        self.check(types[1] == types[0] || types[1] == Ty::Vec(base, 1), name)?;
                        Ok(self.word_map(values, base, move |x| {
                            let order = if base == Base::Int {
                                (x[0] as i32).cmp(&(x[1] as i32))
                            } else {
                                x[0].cmp(&x[1])
                            };
                            if order.is_lt() == minimum { x[0] } else { x[1] }
                        }))
                    }
                    _ => self.check(false, name).map(|_| unreachable!()),
                }
            }
            "clamp" => {
                self.check(count == 3, name)?;
                match &types[0] {
                    Ty::Vec(Base::Float, _) => {
                        self.check(same_or_scalar(1) && types[2] == types[1], name)?;
                        Ok(self.float_map(values, |x| x[0].max(x[1]).min(x[2])))
                    }
                    Ty::Vec(base @ (Base::Int | Base::Uint), _) if es3 => {
                        let base = *base;
                        self.check(
                            (types[1] == types[0] || types[1] == Ty::Vec(base, 1))
                                && types[2] == types[1],
                            name,
                        )?;
                        Ok(self.word_map(values, base, move |x| {
                            if base == Base::Int {
                                (x[0] as i32).max(x[1] as i32).min(x[2] as i32) as u32
                            } else {
                                x[0].max(x[1]).min(x[2])
                            }
                        }))
                    }
                    _ => self.check(false, name).map(|_| unreachable!()),
                }
            }
            "mix" => {
                self.check(
                    count == 3 && is_gen_float(&types[0]) && types[1] == types[0],
                    name,
                )?;
                if matches!(types[2], Ty::Vec(Base::Bool, _))
                    && es3
                    && types[2].size() == types[0].size()
                {
                    let mut values = values;
                    let pick = values.pop().expect("three arguments");
                    let converted = self.word_map(vec![pick], Base::Float, |x| {
                        of_f32(if x[0] != 0 { 1.0 } else { 0.0 })
                    });
                    values.push(converted);
                    return Ok(self.float_map(values, |x| if x[2] != 0.0 { x[1] } else { x[0] }));
                }
                self.check(same_or_scalar(2), name)?;
                Ok(self.float_map(values, |x| x[0] * (1.0 - x[2]) + x[1] * x[2]))
            }
            "step" => {
                self.check(
                    count == 2
                        && is_gen_float(&types[1])
                        && (types[0] == types[1] || types[0] == FLOAT),
                    name,
                )?;
                Ok(self.float_map(values, |x| if x[1] < x[0] { 0.0 } else { 1.0 }))
            }
            "smoothstep" => {
                self.check(
                    count == 3
                        && is_gen_float(&types[2])
                        && types[1] == types[0]
                        && (types[0] == types[2] || types[0] == FLOAT),
                    name,
                )?;
                Ok(self.float_map(values, |x| {
                    let t = ((x[2] - x[0]) / (x[1] - x[0])).clamp(0.0, 1.0);
                    t * t * (3.0 - 2.0 * t)
                }))
            }
            "length" | "normalize" | "dot" | "distance" | "faceforward" | "reflect" | "refract"
            | "cross" => self.geometric(name, values),
            "matrixCompMult" => {
                self.check(
                    count == 2 && matches!(types[0], Ty::Mat(..)) && types[1] == types[0],
                    name,
                )?;
                let ty = types[0].clone();
                Ok(self.vector_function(values, ty, |x, out| {
                    for (index, slot) in out.iter_mut().enumerate() {
                        *slot = x[0][index] * x[1][index];
                    }
                }))
            }
            "transpose" if es3 => {
                let Some(Ty::Mat(columns, rows)) = types.first().cloned() else {
                    return self.check(false, name).map(|_| unreachable!());
                };
                let (columns, rows) = (usize::from(columns), usize::from(rows));
                Ok(self.vector_function(
                    values,
                    Ty::Mat(rows as u8, columns as u8),
                    move |x, out| {
                        for column in 0..columns {
                            for row in 0..rows {
                                out[row * columns + column] = x[0][column * rows + row];
                            }
                        }
                    },
                ))
            }
            "outerProduct" if es3 => {
                self.check(
                    count == 2 && is_gen_float(&types[0]) && is_gen_float(&types[1]),
                    name,
                )?;
                let (rows, columns) = (usize::from(types[0].size()), usize::from(types[1].size()));
                Ok(self.vector_function(
                    values,
                    Ty::Mat(columns as u8, rows as u8),
                    move |x, out| {
                        for column in 0..columns {
                            for row in 0..rows {
                                out[column * rows + row] = x[0][row] * x[1][column];
                            }
                        }
                    },
                ))
            }
            "determinant" | "inverse" if es3 => {
                let Some(Ty::Mat(size, rows)) = types.first().cloned().filter(|_| count == 1)
                else {
                    return self.check(false, name).map(|_| unreachable!());
                };
                self.check(size == rows, name)?;
                let size = usize::from(size);
                let determinant = name == "determinant";
                let result = if determinant { FLOAT } else { types[0].clone() };
                Ok(self.vector_function(values, result, move |x, out| {
                    let (det, inverse) = invert(x[0], size);
                    if determinant {
                        out[0] = det;
                    } else {
                        out.copy_from_slice(&inverse);
                    }
                }))
            }
            "lessThan" | "lessThanEqual" | "greaterThan" | "greaterThanEqual" | "equal"
            | "notEqual" => {
                self.check(
                    count == 2 && types[0] == types[1] && types[0].size() >= 2,
                    name,
                )?;
                let Some(base) = types[0].base() else {
                    return self.check(false, name).map(|_| unreachable!());
                };
                let ordered = !matches!(name, "equal" | "notEqual");
                self.check(
                    !(ordered && base == Base::Bool) && (es3 || base != Base::Uint),
                    name,
                )?;
                use std::cmp::Ordering;
                let test: fn(Option<Ordering>) -> bool = match name {
                    "lessThan" => |order| order.is_some_and(Ordering::is_lt),
                    "lessThanEqual" => |order| order.is_some_and(Ordering::is_le),
                    "greaterThan" => |order| order.is_some_and(Ordering::is_gt),
                    "greaterThanEqual" => |order| order.is_some_and(Ordering::is_ge),
                    "equal" => |order| order.is_some_and(Ordering::is_eq),
                    _ => |order| order.is_none_or(Ordering::is_ne),
                };
                Ok(self.word_map(values, Base::Bool, move |x| {
                    let order = match base {
                        Base::Float => as_f32(x[0]).partial_cmp(&as_f32(x[1])),
                        Base::Int => Some((x[0] as i32).cmp(&(x[1] as i32))),
                        _ => Some(x[0].cmp(&x[1])),
                    };
                    u32::from(test(order))
                }))
            }
            "any" | "all" => {
                self.check(
                    count == 1 && matches!(types[0], Ty::Vec(Base::Bool, 2..=4)),
                    name,
                )?;
                let any = name == "any";
                let size = usize::from(types[0].size());
                let (codes, slots, constant) = parts(values);
                let source = slots[0].0;
                let out = self.allocate(1);
                Ok(Value {
                    ty: BOOL,
                    slot: out,
                    constant,
                    code: Some(Box::new(move |m: &mut Machine| {
                        for code in &codes {
                            run(code, m);
                        }
                        for lane in 0..LANES {
                            let mut values =
                                (0..size).map(|index| m.get(source + index, lane) != 0);
                            let result = if any {
                                values.any(|value| value)
                            } else {
                                values.all(|value| value)
                            };
                            m.set(out, lane, u32::from(result));
                        }
                    })),
                })
            }
            "not" => {
                self.check(
                    count == 1 && matches!(types[0], Ty::Vec(Base::Bool, 2..=4)),
                    name,
                )?;
                Ok(self.word_map(values, Base::Bool, |x| u32::from(x[0] == 0)))
            }
            "dFdx" | "dFdy" | "fwidth" | "dFdxEXT" | "dFdyEXT" | "fwidthEXT" => {
                self.check(count == 1 && is_gen_float(&types[0]), name)?;
                if self.stage != Stage::Fragment {
                    return self.fail(format!("'{name}' : only available in fragment shaders"));
                }
                if !es3
                    && !self
                        .extensions
                        .iter()
                        .any(|extension| extension == "GL_OES_standard_derivatives")
                {
                    return self.fail(format!(
                        "'{name}' : requires extension GL_OES_standard_derivatives"
                    ));
                }
                let pick: fn(f32, f32) -> f32 = match name.trim_end_matches("EXT") {
                    "dFdx" => |dx, _| dx,
                    "dFdy" => |_, dy| dy,
                    _ => |dx, dy| dx.abs() + dy.abs(),
                };
                let size = usize::from(types[0].size());
                let (codes, slots, _) = parts(values);
                let source = slots[0].0;
                let out = self.allocate(size);
                Ok(Value {
                    ty: types[0].clone(),
                    slot: out,
                    constant: false,
                    code: Some(Box::new(move |m: &mut Machine| {
                        for code in &codes {
                            run(code, m);
                        }
                        for component in 0..size {
                            let lanes: [f32; LANES] =
                                std::array::from_fn(|lane| m.float(source + component, lane));
                            // lanes 0 and 1 are the top row of the quad, 2 and 3 the bottom
                            let dx = lanes[1] - lanes[0];
                            let dy = lanes[2] - lanes[0];
                            let value = pick(dx, dy);
                            for lane in 0..LANES {
                                m.set_float(out + component, lane, value);
                            }
                        }
                    })),
                })
            }
            "floatBitsToInt" | "floatBitsToUint" if es3 => {
                self.check(count == 1 && is_gen_float(&types[0]), name)?;
                Ok(self.word_map(
                    values,
                    if name == "floatBitsToInt" {
                        Base::Int
                    } else {
                        Base::Uint
                    },
                    |x| x[0],
                ))
            }
            "intBitsToFloat" | "uintBitsToFloat" if es3 => {
                let wanted = if name == "intBitsToFloat" {
                    Base::Int
                } else {
                    Base::Uint
                };
                self.check(
                    count == 1
                        && types[0].base() == Some(wanted)
                        && matches!(types[0], Ty::Vec(..)),
                    name,
                )?;
                Ok(self.word_map(values, Base::Float, |x| x[0]))
            }
            "isnan" | "isinf" if es3 => {
                self.check(count == 1 && is_gen_float(&types[0]), name)?;
                let nan = name == "isnan";
                Ok(self.word_map(values, Base::Bool, move |x| {
                    u32::from(if nan {
                        as_f32(x[0]).is_nan()
                    } else {
                        as_f32(x[0]).is_infinite()
                    })
                }))
            }
            "packHalf2x16" | "packSnorm2x16" | "packUnorm2x16" if es3 => {
                self.check(count == 1 && types[0] == Ty::Vec(Base::Float, 2), name)?;
                let pack: fn(f32) -> u32 = match name {
                    "packHalf2x16" => to_half,
                    "packSnorm2x16" => {
                        |value| ((value.clamp(-1.0, 1.0) * 32767.0).round() as i32 as u32) & 0xffff
                    }
                    _ => |value| (value.clamp(0.0, 1.0) * 65535.0).round() as u32,
                };
                let (codes, slots, constant) = parts(values);
                let source = slots[0].0;
                let out = self.allocate(1);
                Ok(Value {
                    ty: UINT,
                    slot: out,
                    constant,
                    code: Some(Box::new(move |m: &mut Machine| {
                        for code in &codes {
                            run(code, m);
                        }
                        for lane in 0..LANES {
                            let word = pack(m.float(source, lane))
                                | (pack(m.float(source + 1, lane)) << 16);
                            m.set(out, lane, word);
                        }
                    })),
                })
            }
            "unpackHalf2x16" | "unpackSnorm2x16" | "unpackUnorm2x16" if es3 => {
                self.check(count == 1 && types[0] == UINT, name)?;
                let unpack: fn(u32) -> f32 = match name {
                    "unpackHalf2x16" => from_half,
                    "unpackSnorm2x16" => {
                        |half| (f32::from(half as u16 as i16) / 32767.0).clamp(-1.0, 1.0)
                    }
                    _ => |half| half as f32 / 65535.0,
                };
                let (codes, slots, constant) = parts(values);
                let source = slots[0].0;
                let out = self.allocate(2);
                Ok(Value {
                    ty: Ty::Vec(Base::Float, 2),
                    slot: out,
                    constant,
                    code: Some(Box::new(move |m: &mut Machine| {
                        for code in &codes {
                            run(code, m);
                        }
                        for lane in 0..LANES {
                            let word = m.get(source, lane);
                            for (index, half) in [word & 0xffff, word >> 16].into_iter().enumerate()
                            {
                                m.set_float(out + index, lane, unpack(half));
                            }
                        }
                    })),
                })
            }
            _ => self.texture_function(name, values),
        }
    }

    fn geometric(&mut self, name: &str, values: Vec<Value>) -> Result<Value> {
        let types: Vec<Ty> = values.iter().map(|value| value.ty.clone()).collect();
        let all_same = types.iter().all(|ty| *ty == types[0]) && is_gen_float(&types[0]);
        let size = usize::from(types.first().map_or(0, Ty::size));
        let ty = types.first().cloned().unwrap_or(FLOAT);
        match (name, values.len()) {
            ("length", 1) if all_same => Ok(self.vector_function(values, FLOAT, |x, out| {
                out[0] = x[0].iter().map(|v| v * v).sum::<f32>().sqrt()
            })),
            ("normalize", 1) if all_same => Ok(self.vector_function(values, ty, |x, out| {
                let length = x[0].iter().map(|v| v * v).sum::<f32>().sqrt();
                for (slot, value) in out.iter_mut().zip(x[0].iter()) {
                    *slot = value / length;
                }
            })),
            ("dot", 2) if all_same => Ok(self.vector_function(values, FLOAT, |x, out| {
                out[0] = x[0].iter().zip(x[1].iter()).map(|(a, b)| a * b).sum()
            })),
            ("distance", 2) if all_same => Ok(self.vector_function(values, FLOAT, |x, out| {
                out[0] = x[0]
                    .iter()
                    .zip(x[1].iter())
                    .map(|(a, b)| (a - b) * (a - b))
                    .sum::<f32>()
                    .sqrt()
            })),
            ("cross", 2) if all_same && size == 3 => {
                Ok(self.vector_function(values, ty, |x, out| {
                    let (a, b) = (&x[0], &x[1]);
                    out[0] = a[1] * b[2] - b[1] * a[2];
                    out[1] = a[2] * b[0] - b[2] * a[0];
                    out[2] = a[0] * b[1] - b[0] * a[1];
                }))
            }
            ("faceforward", 3) if all_same => Ok(self.vector_function(values, ty, |x, out| {
                let d: f32 = x[2].iter().zip(x[1].iter()).map(|(a, b)| a * b).sum();
                for (slot, value) in out.iter_mut().zip(x[0].iter()) {
                    *slot = if d < 0.0 { *value } else { -value };
                }
            })),
            ("reflect", 2) if all_same => Ok(self.vector_function(values, ty, |x, out| {
                let d: f32 = x[1].iter().zip(x[0].iter()).map(|(a, b)| a * b).sum();
                for (index, slot) in out.iter_mut().enumerate() {
                    *slot = x[0][index] - 2.0 * d * x[1][index];
                }
            })),
            ("refract", 3)
                if types[0] == types[1] && is_gen_float(&types[0]) && types[2] == FLOAT =>
            {
                Ok(self.vector_function(values, ty, |x, out| {
                    let eta = x[2][0];
                    let d: f32 = x[1].iter().zip(x[0].iter()).map(|(a, b)| a * b).sum();
                    let k = 1.0 - eta * eta * (1.0 - d * d);
                    for (index, slot) in out.iter_mut().enumerate() {
                        *slot = if k < 0.0 {
                            0.0
                        } else {
                            eta * x[0][index] - (eta * d + k.sqrt()) * x[1][index]
                        };
                    }
                }))
            }
            _ => self.fail(format!("'{name}' : no matching overloaded function found")),
        }
    }

    // texture lookups, the GLSL ES 1.00 names and the 3.00 overloads alike
    fn texture_function(&mut self, name: &str, values: Vec<Value>) -> Result<Value> {
        let types: Vec<Ty> = values.iter().map(|value| value.ty.clone()).collect();
        let Some(Ty::Sampler(kind, base)) = types.first().cloned() else {
            return self.fail(format!("'{name}' : no matching overloaded function found"));
        };
        let es3 = self.version >= 300;
        let lod_extension = self
            .extensions
            .iter()
            .any(|extension| extension == "GL_EXT_shader_texture_lod");
        let name_base = name.trim_end_matches("EXT");
        let coordinates = match kind {
            SamplerKind::Two => 2,
            SamplerKind::Cube
            | SamplerKind::Three
            | SamplerKind::TwoArray
            | SamplerKind::TwoShadow => 3,
            SamplerKind::CubeShadow | SamplerKind::TwoArrayShadow => 4,
        };
        let result = match kind {
            SamplerKind::TwoShadow | SamplerKind::CubeShadow | SamplerKind::TwoArrayShadow => FLOAT,
            _ => Ty::Vec(base, 4),
        };
        let fail = |compiler: &Compiler| {
            compiler.fail::<Value>(format!("'{name}' : no matching overloaded function found"))
        };
        let float_vec = |ty: &Ty, size: u8| *ty == Ty::Vec(Base::Float, size);
        // (projective divisor component, lod mode)
        enum Mode {
            Implicit,
            Bias,
            Lod,
            Grad,
        }
        let (projective, mode): (Option<usize>, Mode) = match name_base {
            "texture2D" | "textureCube" if !es3 => {
                let expected = if name_base == "texture2D" { 2 } else { 3 };
                if coordinates != expected || !float_vec(&types[1], expected as u8) {
                    return fail(self);
                }
                (
                    None,
                    if values.len() == 3 {
                        Mode::Bias
                    } else {
                        Mode::Implicit
                    },
                )
            }
            "texture2DProj" if !es3 => {
                let size = types.get(1).map_or(0, Ty::size);
                if kind != SamplerKind::Two || !(size == 3 || size == 4) {
                    return fail(self);
                }
                (
                    Some(usize::from(size) - 1),
                    if values.len() == 3 {
                        Mode::Bias
                    } else {
                        Mode::Implicit
                    },
                )
            }
            "texture2DLod" | "textureCubeLod" | "texture2DProjLod" if !es3 => {
                if self.stage == Stage::Fragment && !lod_extension {
                    return self.fail(format!("'{name}' : only available in vertex shaders"));
                }
                let projective = name_base == "texture2DProjLod";
                (
                    if projective {
                        Some(usize::from(types[1].size()) - 1)
                    } else {
                        None
                    },
                    Mode::Lod,
                )
            }
            "texture2DGrad" | "textureCubeGrad" if !es3 && lod_extension => (None, Mode::Grad),
            "texture" if es3 => {
                if !float_vec(&types[1], coordinates as u8) {
                    return fail(self);
                }
                (
                    None,
                    if values.len() == 3 {
                        Mode::Bias
                    } else {
                        Mode::Implicit
                    },
                )
            }
            "textureProj" if es3 => (
                Some(usize::from(types[1].size()) - 1),
                if values.len() == 3 {
                    Mode::Bias
                } else {
                    Mode::Implicit
                },
            ),
            "textureLod" if es3 => (None, Mode::Lod),
            "textureProjLod" if es3 => (Some(usize::from(types[1].size()) - 1), Mode::Lod),
            "textureGrad" if es3 => (None, Mode::Grad),
            "texelFetch" if es3 => return self.texel_fetch(kind, base, values),
            "textureSize" if es3 => return self.texture_size(kind, values),
            _ => return fail(self),
        };
        let (codes, slots, _) = parts(values);
        let sampler = slots[0].0;
        let (coords, coord_count) = (slots[1].0, slots[1].1.slots());
        let extra: Vec<usize> = slots[2..].iter().map(|(slot, _)| *slot).collect();
        let extra_sizes: Vec<usize> = slots[2..].iter().map(|(_, ty)| ty.slots()).collect();
        let stage_is_vertex = self.stage == Stage::Vertex;
        let out = self.allocate(result.slots());
        let scalar = result.slots() == 1;
        Ok(Value {
            ty: result,
            slot: out,
            constant: false,
            code: Some(Box::new(move |m: &mut Machine| {
                for code in &codes {
                    run(code, m);
                }
                let unit = m.get(sampler, 0);
                let mut lanes = [[0.0_f32; 4]; LANES];
                for (lane, coordinate) in lanes.iter_mut().enumerate() {
                    for (index, value) in coordinate.iter_mut().enumerate().take(coord_count.min(4))
                    {
                        *value = m.float(coords + index, lane);
                    }
                    if let Some(divisor) = projective {
                        let w = coordinate[divisor];
                        for value in coordinate.iter_mut().take(divisor) {
                            *value /= w;
                        }
                        if divisor == 3 {
                            coordinate[2] = 0.0;
                        }
                    }
                }
                let lod = match mode {
                    Mode::Implicit if stage_is_vertex => Lod::Explicit([0.0; LANES]),
                    Mode::Implicit => Lod::Implicit(0.0),
                    Mode::Bias if stage_is_vertex => Lod::Explicit([0.0; LANES]),
                    Mode::Bias => Lod::Implicit(m.float(extra[0], 0)),
                    Mode::Lod => Lod::Explicit(std::array::from_fn(|lane| m.float(extra[0], lane))),
                    Mode::Grad => {
                        let size = extra_sizes[0].min(3);
                        let read = |slot: usize, lane: usize, m: &Machine| -> [f32; 3] {
                            let mut value = [0.0; 3];
                            for (index, item) in value.iter_mut().enumerate().take(size) {
                                *item = m.float(slot + index, lane);
                            }
                            value
                        };
                        Lod::Gradient(
                            std::array::from_fn(|lane| read(extra[0], lane, m)),
                            std::array::from_fn(|lane| read(extra[1], lane, m)),
                        )
                    }
                };
                let mut texels = [[0.0_f32; 4]; LANES];
                m.textures.sample(unit, kind, &lanes, lod, &mut texels);
                for (lane, texel) in texels.iter().enumerate() {
                    if scalar {
                        m.set_float(out, lane, texel[0]);
                        continue;
                    }
                    for (index, value) in texel.iter().enumerate() {
                        let word = match base {
                            Base::Float => of_f32(*value),
                            Base::Int => (*value as i32) as u32,
                            _ => *value as u32,
                        };
                        m.set(out + index, lane, word);
                    }
                }
            })),
        })
    }

    fn texel_fetch(&mut self, kind: SamplerKind, base: Base, values: Vec<Value>) -> Result<Value> {
        let coordinates = if kind == SamplerKind::Two { 2 } else { 3 };
        if values.len() != 3
            || values[1].ty != Ty::Vec(Base::Int, coordinates)
            || values[2].ty != INT
        {
            return self.fail("'texelFetch' : no matching overloaded function found");
        }
        let (codes, slots, _) = parts(values);
        let (sampler, coords, level) = (slots[0].0, slots[1].0, slots[2].0);
        let out = self.allocate(4);
        let count = usize::from(coordinates);
        Ok(Value {
            ty: Ty::Vec(base, 4),
            slot: out,
            constant: false,
            code: Some(Box::new(move |m: &mut Machine| {
                for code in &codes {
                    run(code, m);
                }
                let unit = m.get(sampler, 0);
                for lane in 0..LANES {
                    let mut at = [0_i32; 4];
                    for (index, item) in at.iter_mut().enumerate().take(count) {
                        *item = m.get(coords + index, lane) as i32;
                    }
                    let texel = m.textures.fetch(unit, kind, at, m.get(level, lane) as i32);
                    for (index, value) in texel.iter().enumerate() {
                        let word = match base {
                            Base::Float => of_f32(*value),
                            Base::Int => (*value as i32) as u32,
                            _ => *value as u32,
                        };
                        m.set(out + index, lane, word);
                    }
                }
            })),
        })
    }

    fn texture_size(&mut self, kind: SamplerKind, values: Vec<Value>) -> Result<Value> {
        let size: u8 = match kind {
            SamplerKind::Two
            | SamplerKind::TwoShadow
            | SamplerKind::Cube
            | SamplerKind::CubeShadow => 2,
            _ => 3,
        };
        if values.len() != 2 || values[1].ty != INT {
            return self.fail("'textureSize' : no matching overloaded function found");
        }
        let (codes, slots, _) = parts(values);
        let (sampler, level) = (slots[0].0, slots[1].0);
        let out = self.allocate(usize::from(size));
        Ok(Value {
            ty: Ty::Vec(Base::Int, size),
            slot: out,
            constant: false,
            code: Some(Box::new(move |m: &mut Machine| {
                for code in &codes {
                    run(code, m);
                }
                let unit = m.get(sampler, 0);
                for lane in 0..LANES {
                    let dimensions = m.textures.size(unit, kind, m.get(level, lane) as i32);
                    for (index, value) in dimensions.iter().enumerate().take(usize::from(size)) {
                        m.set(out + index, lane, *value as u32);
                    }
                }
            })),
        })
    }
}

// the determinant and inverse of a square column-major matrix
fn invert(matrix: &[f32], size: usize) -> (f32, Vec<f32>) {
    let at = |column: usize, row: usize| matrix[column * size + row];
    let mut augmented: Vec<Vec<f64>> = (0..size)
        .map(|row| {
            let mut line: Vec<f64> = (0..size).map(|column| f64::from(at(column, row))).collect();
            line.extend((0..size).map(|column| if column == row { 1.0 } else { 0.0 }));
            line
        })
        .collect();
    let mut determinant = 1.0_f64;
    for column in 0..size {
        let pivot = (column..size)
            .max_by(|a, b| {
                augmented[*a][column]
                    .abs()
                    .total_cmp(&augmented[*b][column].abs())
            })
            .unwrap_or(column);
        if augmented[pivot][column] == 0.0 {
            return (0.0, vec![f32::NAN; size * size]);
        }
        if pivot != column {
            augmented.swap(pivot, column);
            determinant = -determinant;
        }
        let value = augmented[column][column];
        determinant *= value;
        for item in augmented[column].iter_mut() {
            *item /= value;
        }
        for row in 0..size {
            if row != column {
                let factor = augmented[row][column];
                if factor != 0.0 {
                    let pivot = augmented[column].clone();
                    for (target, source) in augmented[row].iter_mut().zip(&pivot) {
                        *target -= factor * source;
                    }
                }
            }
        }
    }
    let mut inverse = vec![0.0; size * size];
    for column in 0..size {
        for row in 0..size {
            inverse[column * size + row] = augmented[row][size + column] as f32;
        }
    }
    (determinant as f32, inverse)
}
