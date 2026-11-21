// the ISO 8601 and RFC 9557 strings Temporal reads: dates in extended or basic form with
// six-digit signed years, times with fractions up to nanoseconds, UTC offsets and Z, a time
// zone annotation, u-ca and other bracketed annotations, and ISO 8601 durations

use super::iso::{Fields, NS_PER_SECOND};

#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct Parsed {
    pub(crate) year: Option<i64>,
    pub(crate) month: Option<i64>,
    pub(crate) day: Option<i64>,
    pub(crate) time: Option<[i64; 6]>,
    pub(crate) utc: bool,
    // the offset in nanoseconds, and whether it was written to the minute only
    pub(crate) offset: Option<(i128, bool)>,
    pub(crate) zone: Option<String>,
    pub(crate) calendar: Option<String>,
}

struct Cursor<'a> {
    text: &'a [u8],
    at: usize,
}

impl Cursor<'_> {
    fn peek(&self) -> Option<u8> {
        self.text.get(self.at).copied()
    }

    fn eat(&mut self, byte: u8) -> bool {
        if self.peek() == Some(byte) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    fn eat_any(&mut self, bytes: &[u8]) -> Option<u8> {
        let found = self.peek().filter(|byte| bytes.contains(byte))?;
        self.at += 1;
        Some(found)
    }

    fn done(&self) -> bool {
        self.at == self.text.len()
    }

    fn digits(&mut self, count: usize) -> Option<i64> {
        let slice = self.text.get(self.at..self.at + count)?;
        if !slice.iter().all(u8::is_ascii_digit) {
            return None;
        }
        self.at += count;
        Some(
            slice
                .iter()
                .fold(0, |total, digit| total * 10 + i64::from(digit - b'0')),
        )
    }

    fn has_digits(&self, count: usize) -> bool {
        self.text
            .get(self.at..self.at + count)
            .is_some_and(|slice| slice.iter().all(u8::is_ascii_digit))
    }

    // a sign: + or -, or the minus sign U+2212
    fn sign(&mut self) -> Option<i64> {
        if self.eat(b'+') {
            return Some(1);
        }
        if self.eat(b'-') {
            return Some(-1);
        }
        if self.text[self.at..].starts_with("\u{2212}".as_bytes()) {
            self.at += 3;
            return Some(-1);
        }
        None
    }

    fn peek_sign(&self) -> bool {
        matches!(self.peek(), Some(b'+' | b'-'))
            || self.text[self.at..].starts_with("\u{2212}".as_bytes())
    }

    // a fraction after . or , as nanoseconds, from one to nine digits
    fn fraction(&mut self) -> Option<Option<i64>> {
        if self.eat_any(b".,").is_none() {
            return Some(None);
        }
        let start = self.at;
        while self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
            self.at += 1;
        }
        let digits = &self.text[start..self.at];
        if digits.is_empty() || digits.len() > 9 {
            return None;
        }
        let mut value = 0_i64;
        for index in 0..9 {
            value = value * 10 + digits.get(index).map_or(0, |digit| i64::from(digit - b'0'));
        }
        Some(Some(value))
    }

    fn year(&mut self) -> Option<i64> {
        if self.peek_sign() {
            let sign = self.sign()?;
            let year = self.digits(6)?;
            if year == 0 && sign < 0 {
                return None;
            }
            return Some(sign * year);
        }
        self.digits(4)
    }

    // a date in extended or basic form
    fn date(&mut self, parsed: &mut Parsed) -> Option<()> {
        let year = self.year()?;
        let extended = self.eat(b'-');
        let month = self.digits(2)?;
        if extended && !self.eat(b'-') {
            return None;
        }
        let day = self.digits(2)?;
        if !(1..=12).contains(&month)
            || !(1..=super::iso::days_in_month(year, month)).contains(&day)
        {
            return None;
        }
        parsed.year = Some(year);
        parsed.month = Some(month);
        parsed.day = Some(day);
        Some(())
    }

    fn time(&mut self, parsed: &mut Parsed) -> Option<()> {
        let hour = self.digits(2)?;
        let mut fields = [hour, 0, 0, 0, 0, 0];
        let extended = self.eat(b':');
        if extended || self.has_digits(2) {
            fields[1] = self.digits(2)?;
            let more = if extended {
                self.eat(b':')
            } else {
                self.has_digits(2)
            };
            if more {
                fields[2] = self.digits(2)?;
                if let Some(nanoseconds) = self.fraction()? {
                    fields[3] = nanoseconds / 1_000_000;
                    fields[4] = nanoseconds / 1000 % 1000;
                    fields[5] = nanoseconds % 1000;
                }
            }
        }
        if hour > 23 || fields[1] > 59 || fields[2] > 60 {
            return None;
        }
        // a leap second reads as the second before it
        fields[2] = fields[2].min(59);
        parsed.time = Some(fields);
        Some(())
    }

    // ±HH[:MM[:SS[.fraction]]] or the same without colons, as nanoseconds
    fn offset(&mut self) -> Option<(i128, bool)> {
        let sign = self.sign()?;
        let hours = self.digits(2)?;
        let mut minutes = 0;
        let mut seconds = 0;
        let mut fraction = 0;
        let mut minute_precision = true;
        let extended = self.eat(b':');
        if extended || self.has_digits(2) {
            minutes = self.digits(2)?;
            let more = if extended {
                self.eat(b':')
            } else {
                self.has_digits(2)
            };
            if more {
                seconds = self.digits(2)?;
                minute_precision = false;
                fraction = self.fraction()?.unwrap_or(0);
            }
        }
        if hours > 23 || minutes > 59 || seconds > 59 {
            return None;
        }
        let total =
            ((hours * 60 + minutes) * 60 + seconds) as i128 * NS_PER_SECOND + fraction as i128;
        Some((i128::from(sign) * total, minute_precision))
    }

    // Z or an offset, then the bracketed time zone and other annotations
    fn zone_and_annotations(&mut self, parsed: &mut Parsed) -> Option<()> {
        if self.eat_any(b"Zz").is_some() {
            parsed.utc = true;
        } else if self.peek_sign() {
            parsed.offset = Some(self.offset()?);
        }
        let mut first = true;
        let mut calendars = 0;
        let mut critical_calendar = false;
        while self.eat(b'[') {
            let critical = self.eat(b'!');
            let end = self.text[self.at..].iter().position(|&byte| byte == b']')? + self.at;
            let body = std::str::from_utf8(&self.text[self.at..end]).ok()?;
            self.at = end + 1;
            match body.split_once('=') {
                None if first => {
                    if !valid_zone_annotation(body) {
                        return None;
                    }
                    parsed.zone = Some(body.to_string());
                }
                None => return None,
                Some((key, value)) => {
                    let key_ok = key.starts_with(|c: char| c.is_ascii_lowercase() || c == '_')
                        && key.chars().all(|c| {
                            c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-'
                        });
                    let value_ok = !value.is_empty()
                        && value.split('-').all(|part| {
                            (1..=8).contains(&part.len())
                                && part.chars().all(|c| c.is_ascii_alphanumeric())
                        });
                    if !key_ok || !value_ok {
                        return None;
                    }
                    if key == "u-ca" {
                        calendars += 1;
                        critical_calendar |= critical;
                        if parsed.calendar.is_none() {
                            parsed.calendar = Some(value.to_string());
                        }
                    } else if critical {
                        return None;
                    }
                }
            }
            first = false;
        }
        if calendars > 1 && critical_calendar {
            return None;
        }
        Some(())
    }
}

fn valid_zone_annotation(body: &str) -> bool {
    if body.starts_with(['+', '-']) {
        let mut cursor = Cursor {
            text: body.as_bytes(),
            at: 0,
        };
        return cursor.offset().is_some_and(|(_, minutes)| minutes) && cursor.done();
    }
    !body.is_empty()
        && body.split('/').all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && part
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '+'))
        })
}

// a date with an optional time, offset and annotations
pub(crate) fn date_time(text: &str) -> Option<Parsed> {
    let mut cursor = Cursor {
        text: text.as_bytes(),
        at: 0,
    };
    let mut parsed = Parsed::default();
    cursor.date(&mut parsed)?;
    if cursor.eat_any(b"Tt ").is_some() {
        cursor.time(&mut parsed)?;
    }
    cursor.zone_and_annotations(&mut parsed)?;
    if parsed.time.is_none() && (parsed.utc || parsed.offset.is_some()) {
        return None;
    }
    cursor.done().then_some(parsed)
}

// a time alone, which needs a T where it would also read as a year and month or a month and
// day, or any date and time string
pub(crate) fn time(text: &str) -> Option<Parsed> {
    if let Some(parsed) = date_time(text) {
        return parsed.time.is_some().then_some(parsed);
    }
    let mut cursor = Cursor {
        text: text.as_bytes(),
        at: 0,
    };
    let designated = cursor.eat_any(b"Tt").is_some();
    let mut parsed = Parsed::default();
    cursor.time(&mut parsed)?;
    cursor.zone_and_annotations(&mut parsed)?;
    if !cursor.done() {
        return None;
    }
    if !designated {
        let bare = text.split('[').next().unwrap_or(text);
        if short_year_month(bare).is_some() || short_month_day(bare).is_some() {
            return None;
        }
    }
    Some(parsed)
}

fn short_year_month(text: &str) -> Option<(i64, i64)> {
    let mut cursor = Cursor {
        text: text.as_bytes(),
        at: 0,
    };
    let year = cursor.year()?;
    cursor.eat(b'-');
    let month = cursor.digits(2)?;
    ((1..=12).contains(&month) && cursor.done()).then_some((year, month))
}

fn short_month_day(text: &str) -> Option<(i64, i64)> {
    let mut cursor = Cursor {
        text: text.as_bytes(),
        at: 0,
    };
    if cursor.eat(b'-') && !cursor.eat(b'-') {
        return None;
    }
    let month = cursor.digits(2)?;
    cursor.eat(b'-');
    let day = cursor.digits(2)?;
    let valid =
        (1..=12).contains(&month) && (1..=super::iso::days_in_month(1972, month)).contains(&day);
    (valid && cursor.done()).then_some((month, day))
}

// annotations after a short form, which may not name a time zone
fn short_annotations(text: &str, parsed: &mut Parsed) -> Option<()> {
    let mut cursor = Cursor {
        text: text.as_bytes(),
        at: 0,
    };
    cursor.zone_and_annotations(parsed)?;
    (cursor.done() && !parsed.utc && parsed.offset.is_none()).then_some(())
}

pub(crate) fn year_month(text: &str) -> Option<Parsed> {
    if let Some(parsed) = date_time(text) {
        return Some(parsed);
    }
    let split = text.find('[').unwrap_or(text.len());
    let (year, month) = short_year_month(&text[..split])?;
    let mut parsed = Parsed {
        year: Some(year),
        month: Some(month),
        ..Parsed::default()
    };
    short_annotations(&text[split..], &mut parsed)?;
    Some(parsed)
}

pub(crate) fn month_day(text: &str) -> Option<Parsed> {
    if let Some(parsed) = date_time(text) {
        return Some(parsed);
    }
    let split = text.find('[').unwrap_or(text.len());
    let (month, day) = short_month_day(&text[..split])?;
    let mut parsed = Parsed {
        month: Some(month),
        day: Some(day),
        ..Parsed::default()
    };
    short_annotations(&text[split..], &mut parsed)?;
    Some(parsed)
}

// an offset written on its own, as a property bag or time zone gives one
pub(crate) fn offset(text: &str) -> Option<(i128, bool)> {
    let mut cursor = Cursor {
        text: text.as_bytes(),
        at: 0,
    };
    let offset = cursor.offset()?;
    cursor.done().then_some(offset)
}

// an ISO 8601 duration: P, then years, months, weeks and days, then T and hours, minutes and
// seconds; only the last part present may have a fraction, and only if it is a time unit
pub(crate) fn duration(text: &str) -> Option<Fields> {
    let mut cursor = Cursor {
        text: text.as_bytes(),
        at: 0,
    };
    let sign = if cursor.peek_sign() {
        cursor.sign()?
    } else {
        1
    };
    cursor.eat_any(b"Pp")?;
    let mut fields = [0.0_f64; 10];
    let mut any = false;
    let mut in_time = false;
    let mut last_unit = 0_usize;
    let mut fraction_seen = false;
    let date_units = [(b'Y', 0), (b'M', 1), (b'W', 2), (b'D', 3)];
    let time_units = [(b'H', 4), (b'M', 5), (b'S', 6)];
    let mut fraction_value: Option<(usize, i64)> = None;
    loop {
        if !in_time && cursor.eat_any(b"Tt").is_some() {
            in_time = true;
            if cursor.done() {
                return None;
            }
            continue;
        }
        if cursor.done() {
            break;
        }
        if fraction_seen {
            return None;
        }
        let start = cursor.at;
        while cursor.peek().is_some_and(|byte| byte.is_ascii_digit()) {
            cursor.at += 1;
        }
        if cursor.at == start {
            return None;
        }
        let whole: f64 = std::str::from_utf8(&cursor.text[start..cursor.at])
            .ok()?
            .parse()
            .ok()?;
        let fraction = cursor.fraction()?;
        let unit = cursor.peek()?.to_ascii_uppercase();
        cursor.at += 1;
        let units: &[(u8, usize)] = if in_time { &time_units } else { &date_units };
        let &(_, index) = units.iter().find(|(letter, _)| *letter == unit)?;
        if any && index <= last_unit {
            return None;
        }
        if fraction.is_some() && !in_time {
            return None;
        }
        fields[index] = whole;
        if let Some(fraction) = fraction {
            fraction_seen = true;
            fraction_value = Some((index, fraction));
        }
        last_unit = index;
        any = true;
    }
    if !any {
        return None;
    }
    // a fraction of an hour or minute spills into the smaller units
    if let Some((index, nanoseconds_fraction)) = fraction_value {
        let unit_ns: i128 = match index {
            4 => 3_600_000_000_000,
            5 => 60_000_000_000,
            _ => NS_PER_SECOND,
        };
        let mut rest = unit_ns * i128::from(nanoseconds_fraction) / NS_PER_SECOND;
        let spill = [
            (5, 60_000_000_000_i128),
            (6, NS_PER_SECOND),
            (7, 1_000_000),
            (8, 1000),
            (9, 1),
        ];
        for (target, size) in spill {
            if target <= index {
                continue;
            }
            fields[target] = (rest / size) as f64;
            rest %= size;
        }
    }
    Some(fields.map(|value| {
        if value == 0.0 {
            0.0
        } else {
            value * sign as f64
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strings_parse_by_the_temporal_grammar() {
        let parsed =
            date_time("2024-07-04T12:30:15.123456789+02:00[Europe/Paris][u-ca=iso8601]").unwrap();
        assert_eq!(
            (parsed.year, parsed.month, parsed.day),
            (Some(2024), Some(7), Some(4))
        );
        assert_eq!(parsed.time, Some([12, 30, 15, 123, 456, 789]));
        assert_eq!(parsed.offset, Some((7_200_000_000_000, true)));
        assert_eq!(parsed.zone.as_deref(), Some("Europe/Paris"));
        assert_eq!(parsed.calendar.as_deref(), Some("iso8601"));
        assert_eq!(
            date_time("20240704T1230").unwrap().time,
            Some([12, 30, 0, 0, 0, 0])
        );
        assert_eq!(date_time("+275760-09-13").unwrap().year, Some(275_760));
        assert!(date_time("-000000-01-01").is_none());
        assert!(date_time("2024-02-30").is_none());
        assert!(date_time("2024-07-04T12:30[!foo=bar]").is_none());
        assert!(date_time("2024-07-04[u-ca=gregory][!u-ca=iso8601]").is_none());
        assert_eq!(time("23:59:60").unwrap().time, Some([23, 59, 59, 0, 0, 0]));
        assert!(time("1214").is_none());
        assert!(time("T1214").is_some());
        assert_eq!(year_month("2024-07").unwrap().month, Some(7));
        assert_eq!(month_day("--07-04").unwrap().day, Some(4));
        assert_eq!(month_day("0704").unwrap().month, Some(7));
        assert_eq!(offset("-05:30"), Some((-19_800_000_000_000, true)));
        assert_eq!(
            duration("P1Y2M3W4DT5H6M7.008009010S"),
            Some([1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0])
        );
        assert_eq!(
            duration("-PT1.5H"),
            Some([0.0, 0.0, 0.0, 0.0, -1.0, -30.0, 0.0, 0.0, 0.0, 0.0])
        );
        assert!(duration("P1Y1H").is_none());
        assert!(duration("PT").is_none());
        assert!(duration("P1.5D").is_none());
        assert!(duration("PT1.5H2M").is_none());
    }
}
