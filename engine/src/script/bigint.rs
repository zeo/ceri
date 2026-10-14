// arbitrary-precision integers for BigInt: a sign and little-endian 32-bit limbs, with the
// arithmetic, two's complement bitwise operations, radix conversion and the exact conversions
// to and from doubles the language needs

use std::cmp::Ordering;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) struct BigInt {
    negative: bool,
    // little-endian limbs without trailing zeros; zero is the empty vector
    limbs: Vec<u32>,
}

fn trim(limbs: &mut Vec<u32>) {
    while limbs.last() == Some(&0) {
        limbs.pop();
    }
}

fn compare_magnitude(left: &[u32], right: &[u32]) -> Ordering {
    left.len().cmp(&right.len()).then_with(|| {
        for (a, b) in left.iter().rev().zip(right.iter().rev()) {
            match a.cmp(b) {
                Ordering::Equal => {}
                other => return other,
            }
        }
        Ordering::Equal
    })
}

fn add_magnitude(left: &[u32], right: &[u32]) -> Vec<u32> {
    let mut result = Vec::with_capacity(left.len().max(right.len()) + 1);
    let mut carry = 0u64;
    for index in 0..left.len().max(right.len()) {
        let sum = u64::from(*left.get(index).unwrap_or(&0))
            + u64::from(*right.get(index).unwrap_or(&0))
            + carry;
        result.push(sum as u32);
        carry = sum >> 32;
    }
    if carry > 0 {
        result.push(carry as u32);
    }
    result
}

// left - right where left >= right
fn sub_magnitude(left: &[u32], right: &[u32]) -> Vec<u32> {
    let mut result = Vec::with_capacity(left.len());
    let mut borrow = 0i64;
    for (index, limb) in left.iter().enumerate() {
        let mut difference = i64::from(*limb) - i64::from(*right.get(index).unwrap_or(&0)) - borrow;
        if difference < 0 {
            difference += 1 << 32;
            borrow = 1;
        } else {
            borrow = 0;
        }
        result.push(difference as u32);
    }
    trim(&mut result);
    result
}

fn mul_magnitude(left: &[u32], right: &[u32]) -> Vec<u32> {
    if left.is_empty() || right.is_empty() {
        return Vec::new();
    }
    let mut result = vec![0u32; left.len() + right.len()];
    for (i, a) in left.iter().enumerate() {
        let mut carry = 0u64;
        for (j, b) in right.iter().enumerate() {
            let product = u64::from(*a) * u64::from(*b) + u64::from(result[i + j]) + carry;
            result[i + j] = product as u32;
            carry = product >> 32;
        }
        let mut k = i + right.len();
        while carry > 0 {
            let sum = u64::from(result[k]) + carry;
            result[k] = sum as u32;
            carry = sum >> 32;
            k += 1;
        }
    }
    trim(&mut result);
    result
}

fn divmod_small(limbs: &[u32], divisor: u32) -> (Vec<u32>, u32) {
    let mut quotient = vec![0u32; limbs.len()];
    let mut remainder = 0u64;
    for index in (0..limbs.len()).rev() {
        let current = (remainder << 32) | u64::from(limbs[index]);
        quotient[index] = (current / u64::from(divisor)) as u32;
        remainder = current % u64::from(divisor);
    }
    trim(&mut quotient);
    (quotient, remainder as u32)
}

fn shift_left_magnitude(limbs: &[u32], bits: usize) -> Vec<u32> {
    if limbs.is_empty() {
        return Vec::new();
    }
    let (words, offset) = (bits / 32, bits % 32);
    let mut result = vec![0u32; words];
    let mut carry = 0u32;
    for limb in limbs {
        if offset == 0 {
            result.push(*limb);
        } else {
            result.push((limb << offset) | carry);
            carry = limb >> (32 - offset);
        }
    }
    if carry > 0 {
        result.push(carry);
    }
    trim(&mut result);
    result
}

fn shift_right_magnitude(limbs: &[u32], bits: usize) -> Vec<u32> {
    let (words, offset) = (bits / 32, bits % 32);
    if words >= limbs.len() {
        return Vec::new();
    }
    let mut result = Vec::with_capacity(limbs.len() - words);
    for index in words..limbs.len() {
        let low = limbs[index] >> offset;
        let high = if offset > 0 {
            limbs.get(index + 1).map_or(0, |next| next << (32 - offset))
        } else {
            0
        };
        result.push(low | high);
    }
    trim(&mut result);
    result
}

fn bit_length(limbs: &[u32]) -> usize {
    limbs.last().map_or(0, |top| {
        (limbs.len() - 1) * 32 + (32 - top.leading_zeros() as usize)
    })
}

// truncating division of magnitudes by shift and subtract, a limb at a time
fn divmod_magnitude(left: &[u32], right: &[u32]) -> (Vec<u32>, Vec<u32>) {
    if compare_magnitude(left, right) == Ordering::Less {
        return (Vec::new(), left.to_vec());
    }
    if right.len() == 1 {
        let (quotient, remainder) = divmod_small(left, right[0]);
        let remainder = if remainder == 0 {
            Vec::new()
        } else {
            vec![remainder]
        };
        return (quotient, remainder);
    }
    let shift = bit_length(left) - bit_length(right);
    let mut remainder = left.to_vec();
    let mut quotient = vec![0u32; shift / 32 + 1];
    let mut divisor = shift_left_magnitude(right, shift);
    for bit in (0..=shift).rev() {
        if compare_magnitude(&remainder, &divisor) != Ordering::Less {
            remainder = sub_magnitude(&remainder, &divisor);
            quotient[bit / 32] |= 1 << (bit % 32);
        }
        divisor = shift_right_magnitude(&divisor, 1);
    }
    trim(&mut quotient);
    (quotient, remainder)
}

impl BigInt {
    pub(super) fn zero() -> Self {
        BigInt {
            negative: false,
            limbs: Vec::new(),
        }
    }

    fn from_parts(negative: bool, mut limbs: Vec<u32>) -> Self {
        trim(&mut limbs);
        let negative = negative && !limbs.is_empty();
        BigInt { negative, limbs }
    }

    pub(super) fn from_i64(value: i64) -> Self {
        let magnitude = value.unsigned_abs();
        Self::from_parts(value < 0, vec![magnitude as u32, (magnitude >> 32) as u32])
    }

    pub(super) fn is_zero(&self) -> bool {
        self.limbs.is_empty()
    }

    pub(super) fn is_negative(&self) -> bool {
        self.negative
    }

    // an integral double; None for fractions and non-finite values
    pub(super) fn from_f64(value: f64) -> Option<Self> {
        if !value.is_finite() || value.fract() != 0.0 {
            return None;
        }
        let bits = value.abs().to_bits();
        let exponent = ((bits >> 52) & 0x7ff) as i64;
        let mantissa = if exponent == 0 {
            (bits & ((1 << 52) - 1)) << 1
        } else {
            (bits & ((1 << 52) - 1)) | (1 << 52)
        };
        let shift = exponent - 1075;
        let base = vec![mantissa as u32, (mantissa >> 32) as u32];
        let limbs = if shift >= 0 {
            shift_left_magnitude(&base, shift as usize)
        } else {
            shift_right_magnitude(&base, (-shift) as usize)
        };
        Some(Self::from_parts(value < 0.0, limbs))
    }

    // the nearest double, rounding as Number(bigint) does
    pub(super) fn to_f64(&self) -> f64 {
        let value: f64 = self.to_string_radix(10).parse().unwrap_or(f64::NAN);
        value
    }

    pub(super) fn parse(text: &str) -> Option<Self> {
        let text = text.trim_matches(super::strings::is_js_whitespace);
        if text.is_empty() {
            return Some(Self::zero());
        }
        let (radix, digits, negative) = match text.get(..2) {
            Some("0x" | "0X") => (16, &text[2..], false),
            Some("0o" | "0O") => (8, &text[2..], false),
            Some("0b" | "0B") => (2, &text[2..], false),
            _ => match text.as_bytes()[0] {
                b'-' => (10, &text[1..], true),
                b'+' => (10, &text[1..], false),
                _ => (10, text, false),
            },
        };
        let parsed = Self::parse_radix(digits, radix)?;
        Some(if negative { parsed.negate() } else { parsed })
    }

    pub(super) fn parse_radix(digits: &str, radix: u32) -> Option<Self> {
        if digits.is_empty() {
            return None;
        }
        let mut limbs: Vec<u32> = Vec::new();
        for character in digits.chars() {
            let digit = character.to_digit(radix)?;
            let mut carry = u64::from(digit);
            for limb in &mut limbs {
                let product = u64::from(*limb) * u64::from(radix) + carry;
                *limb = product as u32;
                carry = product >> 32;
            }
            if carry > 0 {
                limbs.push(carry as u32);
            }
        }
        Some(Self::from_parts(false, limbs))
    }

    pub(super) fn to_string_radix(&self, radix: u32) -> String {
        if self.limbs.is_empty() {
            return "0".to_string();
        }
        let mut digits = Vec::new();
        let mut limbs = self.limbs.clone();
        while !limbs.is_empty() {
            let (quotient, remainder) = divmod_small(&limbs, radix);
            digits.push(std::char::from_digit(remainder, radix).expect("digit in radix"));
            limbs = quotient;
        }
        if self.negative {
            digits.push('-');
        }
        digits.iter().rev().collect()
    }

    pub(super) fn negate(&self) -> Self {
        Self::from_parts(!self.negative, self.limbs.clone())
    }

    pub(super) fn add(&self, other: &Self) -> Self {
        if self.negative == other.negative {
            return Self::from_parts(self.negative, add_magnitude(&self.limbs, &other.limbs));
        }
        match compare_magnitude(&self.limbs, &other.limbs) {
            Ordering::Less => {
                Self::from_parts(other.negative, sub_magnitude(&other.limbs, &self.limbs))
            }
            _ => Self::from_parts(self.negative, sub_magnitude(&self.limbs, &other.limbs)),
        }
    }

    pub(super) fn sub(&self, other: &Self) -> Self {
        self.add(&other.negate())
    }

    pub(super) fn mul(&self, other: &Self) -> Self {
        Self::from_parts(
            self.negative != other.negative,
            mul_magnitude(&self.limbs, &other.limbs),
        )
    }

    // truncating division and the remainder with the dividend's sign; None on division by zero
    pub(super) fn divmod(&self, other: &Self) -> Option<(Self, Self)> {
        if other.is_zero() {
            return None;
        }
        let (quotient, remainder) = divmod_magnitude(&self.limbs, &other.limbs);
        Some((
            Self::from_parts(self.negative != other.negative, quotient),
            Self::from_parts(self.negative, remainder),
        ))
    }

    // None for a negative exponent or an unreasonably large result
    pub(super) fn pow(&self, exponent: &Self) -> Option<Self> {
        if exponent.negative {
            return None;
        }
        if exponent.is_zero() {
            return Some(Self::from_i64(1));
        }
        if self.is_zero() || (self.limbs == [1]) {
            let negative = self.negative && exponent.limbs.first().is_some_and(|low| low & 1 == 1);
            return Some(Self::from_parts(negative, self.limbs.clone()));
        }
        if exponent.limbs.len() > 1
            || bit_length(&self.limbs) as u64 * u64::from(exponent.limbs[0]) > 1 << 24
        {
            return None;
        }
        let mut result = Self::from_i64(1);
        let mut base = self.clone();
        let mut remaining = exponent.limbs[0];
        while remaining > 0 {
            if remaining & 1 == 1 {
                result = result.mul(&base);
            }
            remaining >>= 1;
            if remaining > 0 {
                base = base.mul(&base);
            }
        }
        Some(result)
    }

    pub(super) fn compare(&self, other: &Self) -> Ordering {
        match (self.negative, other.negative) {
            (false, true) => Ordering::Greater,
            (true, false) => Ordering::Less,
            (false, false) => compare_magnitude(&self.limbs, &other.limbs),
            (true, true) => compare_magnitude(&other.limbs, &self.limbs),
        }
    }

    // compares with a double mathematically; None when the double is NaN
    pub(super) fn compare_f64(&self, other: f64) -> Option<Ordering> {
        if other.is_nan() {
            return None;
        }
        if other.is_infinite() {
            return Some(if other > 0.0 {
                Ordering::Less
            } else {
                Ordering::Greater
            });
        }
        let integral = other.trunc();
        let whole = Self::from_f64(integral).expect("finite integral");
        Some(self.compare(&whole).then_with(|| {
            if other > integral {
                Ordering::Less
            } else if other < integral {
                Ordering::Greater
            } else {
                Ordering::Equal
            }
        }))
    }

    // the two's complement limbs over a fixed width, for bitwise operations
    fn twos(&self, width: usize) -> Vec<u32> {
        let mut limbs = self.limbs.clone();
        limbs.resize(width, 0);
        if self.negative {
            for limb in &mut limbs {
                *limb = !*limb;
            }
            let mut carry = 1u64;
            for limb in &mut limbs {
                let sum = u64::from(*limb) + carry;
                *limb = sum as u32;
                carry = sum >> 32;
            }
        }
        limbs
    }

    fn from_twos(mut limbs: Vec<u32>) -> Self {
        let negative = limbs.last().is_some_and(|top| top >> 31 == 1);
        if negative {
            for limb in &mut limbs {
                *limb = !*limb;
            }
            let mut carry = 1u64;
            for limb in &mut limbs {
                let sum = u64::from(*limb) + carry;
                *limb = sum as u32;
                carry = sum >> 32;
            }
        }
        Self::from_parts(negative, limbs)
    }

    pub(super) fn bitwise(&self, other: &Self, operation: fn(u32, u32) -> u32) -> Self {
        let width = self.limbs.len().max(other.limbs.len()) + 1;
        let left = self.twos(width);
        let right = other.twos(width);
        Self::from_twos(
            left.iter()
                .zip(&right)
                .map(|(a, b)| operation(*a, *b))
                .collect(),
        )
    }

    pub(super) fn not(&self) -> Self {
        self.negate().sub(&Self::from_i64(1))
    }

    // shifts left for positive counts and right (rounding toward negative infinity) for negative
    pub(super) fn shift(&self, count: i64) -> Option<Self> {
        if count >= 0 {
            if count > 1 << 24 {
                return None;
            }
            return Some(Self::from_parts(
                self.negative,
                shift_left_magnitude(&self.limbs, count as usize),
            ));
        }
        let bits = count.unsigned_abs() as usize;
        if !self.negative {
            return Some(Self::from_parts(
                false,
                shift_right_magnitude(&self.limbs, bits),
            ));
        }
        // floor division by a power of two for negative values
        let truncated = shift_right_magnitude(&self.limbs, bits);
        let exact = shift_left_magnitude(&truncated, bits) == self.limbs;
        let result = Self::from_parts(true, truncated);
        Some(if exact {
            result
        } else {
            result.sub(&Self::from_i64(1))
        })
    }

    // BigInt.asUintN and asIntN
    pub(super) fn as_bits(&self, bits: usize, signed: bool) -> Self {
        if bits == 0 {
            return Self::zero();
        }
        let width = bits.div_ceil(32) + 1;
        let mut limbs = self.twos(width.max(self.limbs.len() + 1));
        limbs.truncate(bits.div_ceil(32));
        if !bits.is_multiple_of(32)
            && let Some(top) = limbs.last_mut()
        {
            *top &= (1u32 << (bits % 32)) - 1;
        }
        let unsigned = Self::from_parts(false, limbs);
        if signed && bit_length(&unsigned.limbs) == bits {
            // the top bit is set: subtract 2^bits
            let modulus = Self::from_parts(false, shift_left_magnitude(&[1], bits));
            return unsigned.sub(&modulus);
        }
        unsigned
    }

    // the low 64 bits as two's complement, for BigInt64Array and DataView
    pub(super) fn to_u64(&self) -> u64 {
        let limbs = self.as_bits(64, false).limbs;
        u64::from(*limbs.first().unwrap_or(&0)) | (u64::from(*limbs.get(1).unwrap_or(&0)) << 32)
    }

    pub(super) fn from_u64(value: u64) -> Self {
        Self::from_parts(false, vec![value as u32, (value >> 32) as u32])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn big(text: &str) -> BigInt {
        BigInt::parse(text).expect("valid bigint")
    }

    #[test]
    fn arithmetic_is_exact() {
        let a = big("123456789012345678901234567890");
        let b = big("987654321098765432109876543210");
        assert_eq!(
            a.add(&b).to_string_radix(10),
            "1111111110111111111011111111100"
        );
        assert_eq!(
            a.sub(&b).to_string_radix(10),
            "-864197532086419753208641975320"
        );
        assert_eq!(
            a.mul(&b).to_string_radix(10),
            "121932631137021795226185032733622923332237463801111263526900"
        );
        let (quotient, remainder) = b.divmod(&a).unwrap();
        assert_eq!(
            (
                quotient.to_string_radix(10).as_str(),
                remainder.to_string_radix(10).as_str()
            ),
            ("8", "9000000000900000000090")
        );
        let (quotient, remainder) = big("-7").divmod(&big("2")).unwrap();
        assert_eq!(
            (
                quotient.to_string_radix(10).as_str(),
                remainder.to_string_radix(10).as_str()
            ),
            ("-3", "-1")
        );
        assert_eq!(
            big("2").pow(&big("100")).unwrap().to_string_radix(10),
            "1267650600228229401496703205376"
        );
        assert_eq!(big("255").to_string_radix(16), "ff");
        assert_eq!(big("0x1f").to_string_radix(10), "31");
    }

    #[test]
    fn bitwise_operations_use_twos_complement() {
        assert_eq!(
            big("-1")
                .bitwise(&big("255"), |a, b| a & b)
                .to_string_radix(10),
            "255"
        );
        assert_eq!(big("5").not().to_string_radix(10), "-6");
        assert_eq!(big("-5").shift(-1).unwrap().to_string_radix(10), "-3");
        assert_eq!(
            big("1").shift(70).unwrap().to_string_radix(10),
            "1180591620717411303424"
        );
        assert_eq!(big("255").as_bits(8, true).to_string_radix(10), "-1");
        assert_eq!(
            big("-1").as_bits(64, false).to_string_radix(10),
            "18446744073709551615"
        );
        assert_eq!(big("-1").to_u64(), u64::MAX);
    }

    #[test]
    fn doubles_convert_exactly() {
        assert_eq!(
            BigInt::from_f64(9007199254740993.0)
                .unwrap()
                .to_string_radix(10),
            "9007199254740992"
        );
        assert_eq!(
            BigInt::from_f64(1e21).unwrap().to_string_radix(10),
            "1000000000000000000000"
        );
        assert!(BigInt::from_f64(1.5).is_none());
        assert_eq!(big("9007199254740993").to_f64(), 9007199254740992.0);
        assert_eq!(big("10").compare_f64(10.5), Some(Ordering::Less));
        assert_eq!(big("-3").compare_f64(-3.0), Some(Ordering::Equal));
    }
}
