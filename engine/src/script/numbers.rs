// Math, the Number.prototype formatting methods and the global number parsers; decimal
// rounding works on the exact expansion of each double so ties round as the spec says

use super::strings::is_js_whitespace;
use super::*;

#[derive(Clone, Copy, Debug)]
pub(super) enum MathFunction {
    Abs,
    Acos,
    Acosh,
    Asin,
    Asinh,
    Atan,
    Atanh,
    Atan2,
    Cbrt,
    Ceil,
    Clz32,
    Cos,
    Cosh,
    Exp,
    Expm1,
    Floor,
    Fround,
    Hypot,
    Imul,
    Log,
    Log1p,
    Log10,
    Log2,
    Max,
    Min,
    Pow,
    Random,
    Round,
    Sign,
    Sin,
    Sinh,
    Sqrt,
    Tan,
    Tanh,
    Trunc,
    F16round,
    SumPrecise,
}

// the binary16 value nearest a number, ties to even, as Math.f16round and Float16Array keep it
pub(super) fn f16_round(value: f64) -> f64 {
    if value.is_nan() || value.is_infinite() || value == 0.0 {
        return value;
    }
    let magnitude = value.abs();
    // the largest finite half is 65504, and halfway to the next power is where it overflows
    if magnitude >= 65520.0 {
        return f64::INFINITY.copysign(value);
    }
    let rounded = if magnitude < 2f64.powi(-14) {
        (magnitude * 2f64.powi(24)).round_ties_even() * 2f64.powi(-24)
    } else {
        let exponent = magnitude.log2().floor() as i32;
        let exponent = if 2f64.powi(exponent) > magnitude {
            exponent - 1
        } else {
            exponent
        };
        let unit = 2f64.powi(exponent - 10);
        (magnitude / unit).round_ties_even() * unit
    };
    rounded.copysign(value)
}

fn two_sum(x: f64, y: f64) -> (f64, f64) {
    let high = x + y;
    let back = high - x;
    let low = (x - (high - back)) + (y - back);
    (high, low)
}

// the exactly rounded sum of the numbers: a list of non-overlapping partial sums kept exact,
// with sums past the largest double carried in units of 2^1024, as the proposal's reference
// implementation does
pub(super) fn sum_precise(values: &[f64]) -> f64 {
    const HALF_RANGE: f64 = 8.98846567431158e307; // 2^1023
    const MAX_ULP: f64 = 1.99584030953472e292; // 2^971, the spacing of doubles at the top
    let mut partials: Vec<f64> = Vec::new();
    let mut overflow = 0_i64;
    let mut everything_negative_zero = true;
    let (mut positive_infinity, mut negative_infinity, mut nan) = (false, false, false);
    for &value in values {
        if value.is_nan() {
            nan = true;
            continue;
        }
        if value == f64::INFINITY {
            positive_infinity = true;
            continue;
        }
        if value == f64::NEG_INFINITY {
            negative_infinity = true;
            continue;
        }
        if !(value == 0.0 && value.is_sign_negative()) {
            everything_negative_zero = false;
        }
        if value == 0.0 {
            continue;
        }
        let mut x = value;
        let mut kept = 0;
        for index in 0..partials.len() {
            let mut y = partials[index];
            if x.abs() < y.abs() {
                std::mem::swap(&mut x, &mut y);
            }
            let (mut high, mut low) = two_sum(x, y);
            if high.is_infinite() {
                let sign = if high > 0.0 { 1.0 } else { -1.0 };
                overflow += sign as i64;
                x = (x - sign * HALF_RANGE) - sign * HALF_RANGE;
                if x.abs() < y.abs() {
                    std::mem::swap(&mut x, &mut y);
                }
                (high, low) = two_sum(x, y);
            }
            if low != 0.0 {
                partials[kept] = low;
                kept += 1;
            }
            x = high;
        }
        partials.truncate(kept);
        if x != 0.0 {
            partials.push(x);
        }
    }
    if nan || (positive_infinity && negative_infinity) {
        return f64::NAN;
    }
    if positive_infinity {
        return f64::INFINITY;
    }
    if negative_infinity {
        return f64::NEG_INFINITY;
    }
    if everything_negative_zero {
        return -0.0;
    }
    let mut next_index = partials.len() as isize - 1;
    let (mut high, mut low) = (0.0, 0.0);
    if overflow != 0 {
        let next = if next_index >= 0 {
            partials[next_index as usize]
        } else {
            0.0
        };
        next_index -= 1;
        if overflow.abs() > 1 || (overflow > 0 && next > 0.0) || (overflow < 0 && next < 0.0) {
            return if overflow > 0 {
                f64::INFINITY
            } else {
                f64::NEG_INFINITY
            };
        }
        (high, low) = two_sum(overflow as f64 * HALF_RANGE, next / 2.0);
        low *= 2.0;
        if (2.0 * high).is_infinite() {
            // a sum exactly half a unit below 2^1024 rounds to the largest double only when what
            // remains pulls it down
            let below = next_index >= 0 && partials[next_index as usize] < 0.0;
            let above = next_index >= 0 && partials[next_index as usize] > 0.0;
            return if high > 0.0 {
                if high == HALF_RANGE && low == -(MAX_ULP / 2.0) && below {
                    f64::MAX
                } else {
                    f64::INFINITY
                }
            } else if high == -HALF_RANGE && low == MAX_ULP / 2.0 && above {
                -f64::MAX
            } else {
                f64::NEG_INFINITY
            };
        }
        if low != 0.0 {
            next_index += 1;
            if next_index as usize >= partials.len() {
                partials.push(low);
            } else {
                partials[next_index as usize] = low;
            }
            low = 0.0;
        }
        high *= 2.0;
    }
    while next_index >= 0 {
        let x = high;
        let y = partials[next_index as usize];
        next_index -= 1;
        high = x + y;
        low = y - (high - x);
        if low != 0.0 {
            break;
        }
    }
    // a remainder of exactly half a unit takes its direction from the next partial
    if next_index >= 0
        && ((low < 0.0 && partials[next_index as usize] < 0.0)
            || (low > 0.0 && partials[next_index as usize] > 0.0))
    {
        let y = low * 2.0;
        let x = high + y;
        if x - high == y {
            high = x;
        }
    }
    high
}

pub(super) const MATH_FUNCTIONS: &[(&str, MathFunction, u32)] = &[
    ("abs", MathFunction::Abs, 1),
    ("acos", MathFunction::Acos, 1),
    ("acosh", MathFunction::Acosh, 1),
    ("asin", MathFunction::Asin, 1),
    ("asinh", MathFunction::Asinh, 1),
    ("atan", MathFunction::Atan, 1),
    ("atanh", MathFunction::Atanh, 1),
    ("atan2", MathFunction::Atan2, 2),
    ("cbrt", MathFunction::Cbrt, 1),
    ("ceil", MathFunction::Ceil, 1),
    ("clz32", MathFunction::Clz32, 1),
    ("cos", MathFunction::Cos, 1),
    ("cosh", MathFunction::Cosh, 1),
    ("exp", MathFunction::Exp, 1),
    ("expm1", MathFunction::Expm1, 1),
    ("floor", MathFunction::Floor, 1),
    ("fround", MathFunction::Fround, 1),
    ("hypot", MathFunction::Hypot, 2),
    ("f16round", MathFunction::F16round, 1),
    ("sumPrecise", MathFunction::SumPrecise, 1),
    ("imul", MathFunction::Imul, 2),
    ("log", MathFunction::Log, 1),
    ("log1p", MathFunction::Log1p, 1),
    ("log10", MathFunction::Log10, 1),
    ("log2", MathFunction::Log2, 1),
    ("max", MathFunction::Max, 2),
    ("min", MathFunction::Min, 2),
    ("pow", MathFunction::Pow, 2),
    ("random", MathFunction::Random, 0),
    ("round", MathFunction::Round, 1),
    ("sign", MathFunction::Sign, 1),
    ("sin", MathFunction::Sin, 1),
    ("sinh", MathFunction::Sinh, 1),
    ("sqrt", MathFunction::Sqrt, 1),
    ("tan", MathFunction::Tan, 1),
    ("tanh", MathFunction::Tanh, 1),
    ("trunc", MathFunction::Trunc, 1),
];

pub(super) const MATH_CONSTANTS: &[(&str, f64)] = &[
    ("E", std::f64::consts::E),
    ("LN10", std::f64::consts::LN_10),
    ("LN2", std::f64::consts::LN_2),
    ("LOG10E", std::f64::consts::LOG10_E),
    ("LOG2E", std::f64::consts::LOG2_E),
    ("PI", std::f64::consts::PI),
    ("SQRT1_2", std::f64::consts::FRAC_1_SQRT_2),
    ("SQRT2", std::f64::consts::SQRT_2),
];

#[derive(Clone, Copy, Debug)]
pub(super) enum NumberFormat {
    Fixed,
    Precision,
    Exponential,
    Locale,
}

// Math.round: halves round toward +Infinity and the sign of zero is kept
fn round(value: f64) -> f64 {
    if !value.is_finite() || value == 0.0 {
        return value;
    }
    let floor = value.floor();
    let rounded = if value - floor >= 0.5 {
        floor + 1.0
    } else {
        floor
    };
    if rounded == 0.0 && value < 0.0 {
        -0.0
    } else {
        rounded
    }
}

fn sign(value: f64) -> f64 {
    if value.is_nan() || value == 0.0 {
        value
    } else if value > 0.0 {
        1.0
    } else {
        -1.0
    }
}

// the exact decimal digits and exponent of a positive finite double, d.ddd * 10^exponent
fn exact_digits(value: f64) -> (Vec<u8>, i32) {
    let text = format!("{value:.1100e}");
    let (mantissa, exponent) = text.split_once('e').expect("exponential form");
    let mut digits: Vec<u8> = mantissa
        .bytes()
        .filter(u8::is_ascii_digit)
        .map(|byte| byte - b'0')
        .collect();
    while digits.len() > 1 && digits.last() == Some(&0) {
        digits.pop();
    }
    (digits, exponent.parse().expect("exponent digits"))
}

// rounds digits to keep the first count, halves rounding up; returns digits and exponent
fn round_digits(digits: &[u8], exponent: i32, count: usize) -> (Vec<u8>, i32) {
    if count == 0 {
        // everything rounds away; a leading digit of five or more carries into a new digit
        return if digits.first().is_some_and(|digit| *digit >= 5) {
            (vec![1], exponent + 1)
        } else {
            (vec![0], exponent)
        };
    }
    let mut kept: Vec<u8> = digits.iter().copied().take(count).collect();
    kept.resize(count, 0);
    let round_up = digits.get(count).is_some_and(|digit| *digit >= 5);
    let mut exponent = exponent;
    if round_up {
        let mut index = count;
        loop {
            if index == 0 {
                kept.insert(0, 1);
                kept.pop();
                exponent += 1;
                break;
            }
            index -= 1;
            if kept[index] == 9 {
                kept[index] = 0;
            } else {
                kept[index] += 1;
                break;
            }
        }
    }
    (kept, exponent)
}

fn digits_text(digits: &[u8]) -> String {
    digits
        .iter()
        .map(|digit| char::from(b'0' + digit))
        .collect()
}

// Number.prototype.toFixed for |value| < 1e21: n = round(value * 10^fraction) with halves
// rounding up, written with fraction digits after the point
pub(super) fn to_fixed(value: f64, fraction: usize) -> String {
    let negative = value < 0.0;
    let magnitude = value.abs();
    let integer: Vec<u8> = if magnitude == 0.0 {
        vec![0]
    } else {
        let (digits, exponent) = exact_digits(magnitude);
        let count = exponent + 1 + fraction as i32;
        if count < 0 {
            vec![0]
        } else if count == 0 {
            vec![u8::from(digits[0] >= 5)]
        } else {
            let count = count as usize;
            let mut kept: Vec<u8> = digits.iter().copied().take(count).collect();
            kept.resize(count, 0);
            if digits.get(count).is_some_and(|digit| *digit >= 5) {
                let mut index = count;
                loop {
                    if index == 0 {
                        kept.insert(0, 1);
                        break;
                    }
                    index -= 1;
                    if kept[index] == 9 {
                        kept[index] = 0;
                    } else {
                        kept[index] += 1;
                        break;
                    }
                }
            }
            kept
        }
    };
    let mut text = digits_text(&integer);
    let trimmed = text.trim_start_matches('0');
    text = if trimmed.is_empty() {
        "0".to_string()
    } else {
        trimmed.to_string()
    };
    if text.len() < fraction + 1 {
        text = format!("{}{text}", "0".repeat(fraction + 1 - text.len()));
    }
    if fraction > 0 {
        text.insert(text.len() - fraction, '.');
    }
    if negative { format!("-{text}") } else { text }
}

// Number.prototype.toExponential; None means as many digits as the value needs
pub(super) fn to_exponential(value: f64, fraction: Option<usize>) -> String {
    let negative = value < 0.0;
    let magnitude = value.abs();
    let (digits, exponent) = if magnitude == 0.0 {
        (vec![0; fraction.unwrap_or(0) + 1], 0)
    } else {
        match fraction {
            Some(fraction) => {
                let (digits, exponent) = exact_digits(magnitude);
                round_digits(&digits, exponent, fraction + 1)
            }
            None => {
                let shortest = format!("{magnitude:e}");
                let (mantissa, exponent) = shortest.split_once('e').expect("exponential form");
                (
                    mantissa
                        .bytes()
                        .filter(u8::is_ascii_digit)
                        .map(|byte| byte - b'0')
                        .collect(),
                    exponent.parse().expect("exponent digits"),
                )
            }
        }
    };
    let text = digits_text(&digits);
    let mantissa = if text.len() > 1 {
        format!("{}.{}", &text[..1], &text[1..])
    } else {
        text
    };
    let sign = if exponent < 0 { '-' } else { '+' };
    let body = format!("{mantissa}e{sign}{}", exponent.abs());
    if negative { format!("-{body}") } else { body }
}

// Number.prototype.toPrecision
pub(super) fn to_precision(value: f64, precision: usize) -> String {
    let negative = value < 0.0;
    let magnitude = value.abs();
    let (digits, exponent) = if magnitude == 0.0 {
        (vec![0; precision], 0)
    } else {
        let (digits, exponent) = exact_digits(magnitude);
        round_digits(&digits, exponent, precision)
    };
    let text = digits_text(&digits);
    let body = if exponent < -6 || exponent >= precision as i32 {
        let mantissa = if text.len() > 1 {
            format!("{}.{}", &text[..1], &text[1..])
        } else {
            text
        };
        let sign = if exponent < 0 { '-' } else { '+' };
        format!("{mantissa}e{sign}{}", exponent.abs())
    } else if exponent >= 0 {
        let split = exponent as usize + 1;
        if split >= text.len() {
            text
        } else {
            format!("{}.{}", &text[..split], &text[split..])
        }
    } else {
        format!("0.{}{text}", "0".repeat((-exponent - 1) as usize))
    };
    if negative { format!("-{body}") } else { body }
}

// the en-US default number format: grouped integer digits and up to three fraction digits
pub(super) fn to_locale(value: f64) -> String {
    if value.is_nan() {
        return "NaN".to_string();
    }
    if value.is_infinite() {
        return if value > 0.0 { "∞" } else { "-∞" }.to_string();
    }
    let fixed = to_fixed(value.abs(), 3);
    let (integer, fraction) = fixed.split_once('.').unwrap_or((&fixed, ""));
    let fraction = fraction.trim_end_matches('0');
    let mut grouped = String::new();
    for (index, character) in integer.chars().enumerate() {
        if index > 0 && (integer.len() - index) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(character);
    }
    let body = if fraction.is_empty() {
        grouped
    } else {
        format!("{grouped}.{fraction}")
    };
    if value < 0.0 {
        format!("-{body}")
    } else {
        body
    }
}

// parseInt: optional sign and radix prefix, then the longest run of digits in the radix
pub(super) fn parse_int(text: &str, radix: i32) -> f64 {
    let text = text.trim_start_matches(is_js_whitespace);
    let (negative, mut text) = match text.as_bytes().first() {
        Some(b'-') => (true, &text[1..]),
        Some(b'+') => (false, &text[1..]),
        _ => (false, text),
    };
    let mut radix = radix;
    let mut strip_prefix = true;
    if radix != 0 {
        if !(2..=36).contains(&radix) {
            return f64::NAN;
        }
        strip_prefix = radix == 16;
    } else {
        radix = 10;
    }
    if strip_prefix && (text.starts_with("0x") || text.starts_with("0X")) {
        text = &text[2..];
        radix = 16;
    }
    let digits: Vec<u32> = text
        .chars()
        .map_while(|character| character.to_digit(radix as u32))
        .collect();
    if digits.is_empty() {
        return f64::NAN;
    }
    // decimal digits go through the float parser so large values round correctly
    let value = if radix == 10 {
        let text: String = digits
            .iter()
            .map(|digit| char::from_digit(*digit, 10).expect("digit"))
            .collect();
        text.parse::<f64>().unwrap_or(f64::NAN)
    } else {
        digits.iter().fold(0.0, |value, digit| {
            value * f64::from(radix) + f64::from(*digit)
        })
    };
    if negative { -value } else { value }
}

// parseFloat: the longest prefix that is a decimal literal or Infinity
pub(super) fn parse_float(text: &str) -> f64 {
    let text = text.trim_start_matches(is_js_whitespace);
    let bytes = text.as_bytes();
    let mut end = 0;
    if matches!(bytes.first(), Some(b'+' | b'-')) {
        end = 1;
    }
    if text[end..].starts_with("Infinity") {
        return if bytes.first() == Some(&b'-') {
            f64::NEG_INFINITY
        } else {
            f64::INFINITY
        };
    }
    let digits_start = end;
    while end < bytes.len() && bytes[end].is_ascii_digit() {
        end += 1;
    }
    let mut integer_digits = end - digits_start;
    if end < bytes.len() && bytes[end] == b'.' {
        let dot = end;
        end += 1;
        while end < bytes.len() && bytes[end].is_ascii_digit() {
            end += 1;
        }
        integer_digits += end - dot - 1;
        if end - dot == 1 && integer_digits == 0 {
            end = dot;
        }
    }
    if integer_digits == 0 {
        return f64::NAN;
    }
    if end < bytes.len() && matches!(bytes[end], b'e' | b'E') {
        let mut exponent_end = end + 1;
        if exponent_end < bytes.len() && matches!(bytes[exponent_end], b'+' | b'-') {
            exponent_end += 1;
        }
        let exponent_digits = exponent_end;
        while exponent_end < bytes.len() && bytes[exponent_end].is_ascii_digit() {
            exponent_end += 1;
        }
        if exponent_end > exponent_digits {
            end = exponent_end;
        }
    }
    text[..end].parse::<f64>().unwrap_or(f64::NAN)
}

// a small xorshift generator for Math.random, seeded from the clock at startup
pub(super) fn next_random(state: &mut u64) -> f64 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    *state = x;
    (x >> 11) as f64 / (1u64 << 53) as f64
}

pub(super) fn random_seed() -> u64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos() as u64)
        .unwrap_or(0x9e37_79b9_7f4a_7c15);
    nanos | 1
}

impl ScriptRuntime {
    pub(super) fn call_math(
        &mut self,
        function: MathFunction,
        arguments: Vec<Value>,
    ) -> Result<Value, ScriptError> {
        use MathFunction as F;
        if let F::SumPrecise = function {
            let iterable = arguments.first().cloned().unwrap_or(Value::Undefined);
            let iterator = self.get_iterator(iterable)?;
            let mut values = Vec::new();
            while let Some(item) = self.iterator_step(&iterator)? {
                let Value::Number(number) = item else {
                    let error = self.type_error("Math.sumPrecise requires an iterable of numbers");
                    let thrown = self.take_thrown(error);
                    if let Err(error) = self.iterator_close(&iterator) {
                        self.take_thrown(error);
                    }
                    return Err(self.throw_value(thrown));
                };
                values.push(number);
            }
            return Ok(Value::Number(sum_precise(&values)));
        }
        let mut numbers = Vec::with_capacity(arguments.len());
        let takes = match function {
            F::Max | F::Min | F::Hypot => arguments.len(),
            F::Random => 0,
            F::Atan2 | F::Imul | F::Pow => 2,
            _ => 1,
        };
        for index in 0..takes {
            let value = arguments.get(index).cloned().unwrap_or(Value::Undefined);
            numbers.push(self.coerce_number(value)?);
        }
        let x = numbers.first().copied().unwrap_or(f64::NAN);
        let y = numbers.get(1).copied().unwrap_or(f64::NAN);
        Ok(Value::Number(match function {
            F::Abs => x.abs(),
            F::Acos => x.acos(),
            F::Acosh => x.acosh(),
            F::Asin => x.asin(),
            F::Asinh => x.asinh(),
            F::Atan => x.atan(),
            F::Atanh => x.atanh(),
            F::Atan2 => x.atan2(y),
            F::Cbrt => x.cbrt(),
            F::Ceil => x.ceil(),
            F::Clz32 => f64::from(operations::to_uint32_number(x).leading_zeros()),
            F::Cos => x.cos(),
            F::Cosh => x.cosh(),
            F::Exp => x.exp(),
            F::Expm1 => x.exp_m1(),
            F::Floor => x.floor(),
            F::Fround => f64::from(x as f32),
            F::F16round => f16_round(x),
            F::SumPrecise => unreachable!("summed above"),
            F::Hypot => {
                if numbers.iter().any(|value| value.is_infinite()) {
                    f64::INFINITY
                } else {
                    numbers
                        .iter()
                        .map(|value| value * value)
                        .sum::<f64>()
                        .sqrt()
                }
            }
            F::Imul => f64::from(
                operations::to_int32_number(x).wrapping_mul(operations::to_int32_number(y)),
            ),
            F::Log => x.ln(),
            F::Log1p => x.ln_1p(),
            F::Log10 => x.log10(),
            F::Log2 => x.log2(),
            F::Max => numbers.iter().fold(f64::NEG_INFINITY, |best, value| {
                if best.is_nan() || value.is_nan() {
                    f64::NAN
                } else if *value > best || (*value == 0.0 && best == 0.0 && best.is_sign_negative())
                {
                    *value
                } else {
                    best
                }
            }),
            F::Min => numbers.iter().fold(f64::INFINITY, |best, value| {
                if best.is_nan() || value.is_nan() {
                    f64::NAN
                } else if *value < best
                    || (*value == 0.0 && best == 0.0 && value.is_sign_negative())
                {
                    *value
                } else {
                    best
                }
            }),
            F::Pow => {
                if y.is_nan() || (x.abs() == 1.0 && y.is_infinite()) {
                    f64::NAN
                } else {
                    x.powf(y)
                }
            }
            F::Random => next_random(&mut self.random_state),
            F::Round => round(x),
            F::Sign => sign(x),
            F::Sin => x.sin(),
            F::Sinh => x.sinh(),
            F::Sqrt => x.sqrt(),
            F::Tan => x.tan(),
            F::Tanh => x.tanh(),
            F::Trunc => x.trunc(),
        }))
    }

    pub(super) fn format_number(
        &mut self,
        format: NumberFormat,
        number: f64,
        argument: Value,
    ) -> Result<Value, ScriptError> {
        let text = match format {
            NumberFormat::Locale => to_locale(number),
            _ if !number.is_finite() => number_to_string(number),
            NumberFormat::Fixed => {
                let digits = self.coerce_number(argument)?;
                let digits = if digits.is_nan() { 0.0 } else { digits.trunc() };
                if !(0.0..=100.0).contains(&digits) {
                    return Err(
                        self.range_error("toFixed() digits argument must be between 0 and 100")
                    );
                }
                if number.abs() >= 1e21 {
                    number_to_string(number)
                } else {
                    to_fixed(number, digits as usize)
                }
            }
            NumberFormat::Exponential => {
                let digits = match argument {
                    Value::Undefined => None,
                    value => {
                        let digits = self.coerce_number(value)?;
                        let digits = if digits.is_nan() { 0.0 } else { digits.trunc() };
                        if !(0.0..=100.0).contains(&digits) {
                            return Err(self.range_error(
                                "toExponential() argument must be between 0 and 100",
                            ));
                        }
                        Some(digits as usize)
                    }
                };
                to_exponential(number, digits)
            }
            NumberFormat::Precision => match argument {
                Value::Undefined => number_to_string(number),
                value => {
                    let precision = self.coerce_number(value)?;
                    let precision = if precision.is_nan() {
                        0.0
                    } else {
                        precision.trunc()
                    };
                    if !(1.0..=100.0).contains(&precision) {
                        return Err(
                            self.range_error("toPrecision() argument must be between 1 and 100")
                        );
                    }
                    to_precision(number, precision as usize)
                }
            },
        };
        Ok(Value::String(text.into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn precise_sums_and_half_rounding_are_exact() {
        assert_eq!(sum_precise(&[1e20, 0.1, -1e20]), 0.1);
        assert_eq!(sum_precise(&[0.1, 0.2, 0.3]), 0.6);
        assert_eq!(sum_precise(&[f64::MAX, f64::MAX, -f64::MAX]), f64::MAX);
        assert_eq!(sum_precise(&[f64::MAX, f64::MAX]), f64::INFINITY);
        assert!(sum_precise(&[]).is_sign_negative());
        assert!(sum_precise(&[f64::INFINITY, f64::NEG_INFINITY]).is_nan());
        assert_eq!(sum_precise(&[-0.0, 0.0]), 0.0);
        assert!(!sum_precise(&[-0.0, 0.0]).is_sign_negative());
        assert_eq!(f16_round(1.337), 1.3369140625);
        assert_eq!(f16_round(65519.0), 65504.0);
        assert_eq!(f16_round(65520.0), f64::INFINITY);
        assert_eq!(f16_round(5.960464477539063e-8), 5.960464477539063e-8);
        assert_eq!(f16_round(2.9e-8), 0.0);
        assert_eq!(f16_round(1.00048828125), 1.0);
    }

    #[test]
    fn fixed_formatting_rounds_halves_up_on_exact_values() {
        assert_eq!(to_fixed(2.5, 0), "3");
        assert_eq!(to_fixed(1.005, 2), "1.00");
        assert_eq!(to_fixed(1.45, 1), "1.4");
        assert_eq!(to_fixed(0.000001, 2), "0.00");
        assert_eq!(to_fixed(123.456, 1), "123.5");
        assert_eq!(to_fixed(-1.5, 0), "-2");
        assert_eq!(to_fixed(-0.0001, 2), "-0.00");
        assert_eq!(to_fixed(-0.0, 2), "0.00");
        assert_eq!(to_fixed(999.99, 1), "1000.0");
        assert_eq!(to_fixed(0.5, 0), "1");
        assert_eq!(to_fixed(1e20, 2), "100000000000000000000.00");
    }

    #[test]
    fn precision_and_exponential_forms_match_engines() {
        assert_eq!(to_precision(123.456, 4), "123.5");
        assert_eq!(to_precision(0.00001234, 2), "0.000012");
        assert_eq!(to_precision(1234567.0, 2), "1.2e+6");
        assert_eq!(to_precision(0.0000001, 1), "1e-7");
        assert_eq!(to_exponential(123456.0, Some(2)), "1.23e+5");
        assert_eq!(to_exponential(0.00015, None), "1.5e-4");
        assert_eq!(to_exponential(0.0, Some(1)), "0.0e+0");
        assert_eq!(to_locale(1234567.891), "1,234,567.891");
        assert_eq!(to_locale(-0.0004), "-0");
    }

    #[test]
    fn parsers_read_the_longest_valid_prefix() {
        assert_eq!(parse_int("  42px", 0), 42.0);
        assert_eq!(parse_int("0x1F", 0), 31.0);
        assert_eq!(parse_int("-ff", 16), -255.0);
        assert!(parse_int("z", 10).is_nan());
        assert_eq!(parse_int("101", 2), 5.0);
        assert_eq!(parse_float("2.75abc"), 2.75);
        assert_eq!(parse_float(".5e1x"), 5.0);
        assert_eq!(parse_float("-Infinityx"), f64::NEG_INFINITY);
        assert!(parse_float("e5").is_nan());
        assert_eq!(parse_float("1e"), 1.0);
        assert_eq!(round(-2.5), -2.0);
        assert_eq!(round(2.5), 3.0);
        assert_eq!(round(0.49999999999999994), 0.0);
        assert!(round(-0.4).is_sign_negative());
    }
}
