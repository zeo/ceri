// the IANA time zone database tools/tz_data.py packs into tz_data.bin: each zone's transitions,
// and past the last of them the POSIX TZ rule the zone keeps, so offsets, the instants a wall
// clock time names and the transitions around an instant hold for any year

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

static DATA: &[u8] = include_bytes!("tz_data.bin");

const DAY: i64 = 86_400;

pub(super) struct Zone {
    types: Vec<(i32, bool)>,
    times: Vec<i64>,
    indices: Vec<u8>,
    rule: Option<Rule>,
}

// a POSIX TZ rule: the standard offset and, with daylight time, its offset and when it starts
// and ends each year
struct Rule {
    standard: i32,
    daylight: Option<(i32, RuleDate, i64, RuleDate, i64)>,
}

#[derive(Clone, Copy)]
enum RuleDate {
    // Jn: a day of the year from 1, never counting February 29
    Julian(i64),
    // n: a day of the year from 0, counting February 29
    Ordinal(i64),
    // Mm.w.d: the w-th (5 for last) weekday d of month m
    Weekday(i64, i64, i64),
}

struct Index {
    zones: Vec<&'static str>,
    // lowercased identifier to its listed spelling and the zone it names
    names: HashMap<String, (&'static str, usize)>,
    starts: Vec<usize>,
}

thread_local! {
    static INDEX: Index = build_index();
    static ZONES: RefCell<HashMap<usize, Rc<Zone>>> = RefCell::new(HashMap::new());
}

struct Reader {
    at: usize,
}

impl Reader {
    fn bytes(&mut self, count: usize) -> &'static [u8] {
        let slice = &DATA[self.at..self.at + count];
        self.at += count;
        slice
    }

    fn u8(&mut self) -> u8 {
        self.bytes(1)[0]
    }

    fn u16(&mut self) -> u16 {
        u16::from_le_bytes(self.bytes(2).try_into().expect("two bytes"))
    }

    fn u32(&mut self) -> u32 {
        u32::from_le_bytes(self.bytes(4).try_into().expect("four bytes"))
    }

    fn i32(&mut self) -> i32 {
        i32::from_le_bytes(self.bytes(4).try_into().expect("four bytes"))
    }

    fn i64(&mut self) -> i64 {
        i64::from_le_bytes(self.bytes(8).try_into().expect("eight bytes"))
    }

    fn text(&mut self) -> &'static str {
        let length = usize::from(self.u8());
        std::str::from_utf8(self.bytes(length)).expect("tz_data.bin names are UTF-8")
    }
}

fn build_index() -> Index {
    let mut reader = Reader { at: 0 };
    assert_eq!(
        reader.bytes(4),
        b"CTZ1",
        "tz_data.bin has an unknown layout"
    );
    // the tzdata version the file was made from
    reader.text();
    let zones = usize::from(reader.u16());
    let mut names = HashMap::new();
    let mut offsets = Vec::with_capacity(zones);
    let mut zone_names = Vec::with_capacity(zones);
    for index in 0..zones {
        let name = reader.text();
        offsets.push(reader.u32() as usize);
        names.insert(name.to_ascii_lowercase(), (name, index));
        zone_names.push(name);
    }
    let links = usize::from(reader.u16());
    for _ in 0..links {
        let name = reader.text();
        let target = usize::from(reader.u16());
        names.insert(name.to_ascii_lowercase(), (name, target));
    }
    let data = reader.at;
    Index {
        zones: zone_names,
        names,
        starts: offsets.into_iter().map(|offset| data + offset).collect(),
    }
}

fn load(index: usize, start: usize) -> Rc<Zone> {
    ZONES.with(|zones| {
        zones
            .borrow_mut()
            .entry(index)
            .or_insert_with(|| {
                let mut reader = Reader { at: start };
                let types = (0..reader.u8())
                    .map(|_| (reader.i32(), reader.u8() != 0))
                    .collect();
                let count = reader.u32() as usize;
                let times = (0..count).map(|_| reader.i64()).collect();
                let indices = reader.bytes(count).to_vec();
                let rule = parse_rule(reader.text());
                Rc::new(Zone {
                    types,
                    times,
                    indices,
                    rule,
                })
            })
            .clone()
    })
}

// a zone by any identifier the database lists, compared without regard to case, with the
// identifier as the database spells it
pub(super) fn lookup(identifier: &str) -> Option<(&'static str, Rc<Zone>)> {
    let (name, index, start) = INDEX.with(|index| {
        let &(name, zone) = index.names.get(&identifier.to_ascii_lowercase())?;
        Some((name, zone, index.starts[zone]))
    })?;
    Some((name, load(index, start)))
}

// the zone an identifier names, as the database names that zone rather than a link to it
pub(super) fn primary(identifier: &str) -> Option<&'static str> {
    INDEX.with(|index| {
        let &(_, zone) = index.names.get(&identifier.to_ascii_lowercase())?;
        Some(index.zones[zone])
    })
}

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month_index = (month + 9) % 12;
    let day_of_year = (153 * month_index + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn year_of(seconds: i64) -> i64 {
    let days = seconds.div_euclid(DAY) + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    year_of_era + era * 400 + i64::from(month_index >= 10)
}

fn is_leap(year: i64) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

fn month_length(year: i64, month: i64) -> i64 {
    match month {
        2 if is_leap(year) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

impl RuleDate {
    // the day, counted from the epoch, the rule names in a year
    fn day(self, year: i64) -> i64 {
        let first = days_from_civil(year, 1, 1);
        match self {
            RuleDate::Julian(day) => first + day - 1 + i64::from(is_leap(year) && day >= 60),
            RuleDate::Ordinal(day) => first + day,
            RuleDate::Weekday(month, week, weekday) => {
                let start = days_from_civil(year, month, 1);
                // 1970-01-01 was a Thursday
                let first_weekday = (start + 4).rem_euclid(7);
                let mut day = 1 + (weekday - first_weekday).rem_euclid(7) + (week - 1) * 7;
                while day > month_length(year, month) {
                    day -= 7;
                }
                start + day - 1
            }
        }
    }
}

// [+-]hh[:mm[:ss]], as seconds
fn rule_time(text: &mut &str) -> Option<i64> {
    let negative = text.starts_with('-');
    if text.starts_with(['+', '-']) {
        *text = &text[1..];
    }
    let mut total = 0;
    for (index, scale) in [3600, 60, 1].into_iter().enumerate() {
        if index > 0 {
            match text.strip_prefix(':') {
                Some(rest) => *text = rest,
                None => break,
            }
        }
        let digits = text.chars().take_while(char::is_ascii_digit).count();
        if digits == 0 {
            return None;
        }
        total += text[..digits].parse::<i64>().ok()? * scale;
        *text = &text[digits..];
    }
    Some(if negative { -total } else { total })
}

fn rule_name(text: &mut &str) -> Option<()> {
    if let Some(rest) = text.strip_prefix('<') {
        let end = rest.find('>')?;
        *text = &rest[end + 1..];
    } else {
        let letters = text.chars().take_while(char::is_ascii_alphabetic).count();
        if letters < 3 {
            return None;
        }
        *text = &text[letters..];
    }
    Some(())
}

fn rule_date(text: &mut &str) -> Option<(RuleDate, i64)> {
    let number = |text: &mut &str| -> Option<i64> {
        let digits = text.chars().take_while(char::is_ascii_digit).count();
        let value = text[..digits].parse().ok()?;
        *text = &text[digits..];
        Some(value)
    };
    let date = if let Some(rest) = text.strip_prefix('M') {
        *text = rest;
        let month = number(text)?;
        *text = text.strip_prefix('.')?;
        let week = number(text)?;
        *text = text.strip_prefix('.')?;
        RuleDate::Weekday(month, week, number(text)?)
    } else if let Some(rest) = text.strip_prefix('J') {
        *text = rest;
        RuleDate::Julian(number(text)?)
    } else {
        RuleDate::Ordinal(number(text)?)
    };
    let time = match text.strip_prefix('/') {
        Some(rest) => {
            *text = rest;
            rule_time(text)?
        }
        None => 7200,
    };
    Some((date, time))
}

fn parse_rule(footer: &str) -> Option<Rule> {
    let mut text = footer;
    rule_name(&mut text)?;
    // POSIX offsets count hours west of Greenwich
    let standard = -rule_time(&mut text)? as i32;
    if text.is_empty() {
        return Some(Rule {
            standard,
            daylight: None,
        });
    }
    rule_name(&mut text)?;
    let daylight = if text.starts_with([',', ';']) || text.is_empty() {
        standard + 3600
    } else {
        -rule_time(&mut text)? as i32
    };
    let (start, start_time, end, end_time) = match text.strip_prefix(',') {
        Some(rest) => {
            text = rest;
            let (start, start_time) = rule_date(&mut text)?;
            text = text.strip_prefix(',')?;
            let (end, end_time) = rule_date(&mut text)?;
            (start, start_time, end, end_time)
        }
        None => (
            RuleDate::Weekday(3, 2, 0),
            7200,
            RuleDate::Weekday(11, 1, 0),
            7200,
        ),
    };
    Some(Rule {
        standard,
        daylight: Some((daylight, start, start_time, end, end_time)),
    })
}

impl Rule {
    // the instants daylight time starts and ends in a year
    fn changes(&self, year: i64) -> Option<(i64, i64)> {
        let (daylight, start, start_time, end, end_time) = self.daylight?;
        Some((
            start.day(year) * DAY + start_time - i64::from(self.standard),
            end.day(year) * DAY + end_time - i64::from(daylight),
        ))
    }

    fn offset(&self, seconds: i64) -> i32 {
        let Some((daylight, ..)) = self.daylight else {
            return self.standard;
        };
        let (start, end) = self
            .changes(year_of(seconds + i64::from(self.standard)))
            .expect("a rule with daylight time");
        let in_daylight = if start < end {
            start <= seconds && seconds < end
        } else {
            seconds >= start || seconds < end
        };
        if in_daylight { daylight } else { self.standard }
    }

    fn transitions_near(&self, seconds: i64) -> Vec<i64> {
        let year = year_of(seconds);
        (year - 1..=year + 1)
            .filter_map(|year| self.changes(year))
            .flat_map(|(start, end)| [start, end])
            .collect()
    }
}

impl Zone {
    // the UTC offset in seconds at an instant, counted in seconds since the epoch
    pub(super) fn offset(&self, seconds: i64) -> i32 {
        let after = self.times.partition_point(|&time| time <= seconds);
        if after == 0 {
            return self.types.first().map_or(0, |&(offset, _)| offset);
        }
        if after == self.times.len()
            && let Some(rule) = &self.rule
        {
            return rule.offset(seconds);
        }
        self.types[usize::from(self.indices[after - 1])].0
    }

    // the instants whose wall clock reads a local time: one, two in a fold, none in a gap
    pub(super) fn possible(&self, local: i64) -> Vec<i64> {
        let mut found: Vec<i64> = [self.offset(local - DAY), self.offset(local + DAY)]
            .into_iter()
            .map(|offset| local - i64::from(offset))
            .filter(|&instant| i64::from(self.offset(instant)) == local - instant)
            .collect();
        found.sort_unstable();
        found.dedup();
        found
    }

    // the nearest instant after, or with next false before, where the offset changes
    pub(super) fn transition(&self, seconds: i64, next: bool) -> Option<i64> {
        let changes = |time: i64| self.offset(time - 1) != self.offset(time);
        if next {
            let start = self.times.partition_point(|&time| time <= seconds);
            if let Some(&time) = self.times[start..].iter().find(|&&time| changes(time)) {
                return Some(time);
            }
            let rule = self.rule.as_ref()?;
            let floor = self.times.last().copied().unwrap_or(i64::MIN).max(seconds);
            let mut year_from = floor;
            // a zone's rule repeats every year, so a few years ahead always finds one
            for _ in 0..4 {
                let mut candidates: Vec<i64> = rule
                    .transitions_near(year_from)
                    .into_iter()
                    .filter(|&time| time > floor && changes(time))
                    .collect();
                candidates.sort_unstable();
                if let Some(&time) = candidates.first() {
                    return Some(time);
                }
                year_from += 365 * DAY;
            }
            None
        } else {
            let last_table = self.times.last().copied();
            if let Some(rule) = &self.rule
                && last_table.is_none_or(|last| seconds > last)
            {
                let mut candidates: Vec<i64> = rule
                    .transitions_near(seconds)
                    .into_iter()
                    .filter(|&time| time < seconds && last_table.is_none_or(|last| time > last))
                    .filter(|&time| changes(time))
                    .collect();
                candidates.sort_unstable();
                if let Some(&time) = candidates.last() {
                    return Some(time);
                }
            }
            let end = self.times.partition_point(|&time| time < seconds);
            self.times[..end]
                .iter()
                .rev()
                .copied()
                .find(|&time| changes(time))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seconds(year: i64, month: i64, day: i64, hour: i64) -> i64 {
        days_from_civil(year, month, day) * DAY + hour * 3600
    }

    #[test]
    fn zones_answer_offsets_folds_gaps_and_transitions() {
        let (name, york) = lookup("america/new_york").expect("New York");
        assert_eq!(name, "America/New_York");
        assert_eq!(york.offset(seconds(2024, 1, 15, 12)), -5 * 3600);
        assert_eq!(york.offset(seconds(2024, 7, 15, 12)), -4 * 3600);
        // the rule past the table: summer of 2300 is still daylight time
        assert_eq!(york.offset(seconds(2300, 7, 1, 0)), -4 * 3600);
        assert_eq!(york.offset(seconds(2300, 12, 1, 0)), -5 * 3600);
        // 2024-03-10 02:30 does not exist and 2024-11-03 01:30 happens twice
        assert!(york.possible(seconds(2024, 3, 10, 2) + 1800).is_empty());
        assert_eq!(york.possible(seconds(2024, 11, 3, 1) + 1800).len(), 2);
        assert_eq!(
            york.transition(seconds(2024, 1, 1, 0), true),
            Some(seconds(2024, 3, 10, 7))
        );
        assert_eq!(
            york.transition(seconds(2024, 3, 10, 7), false),
            Some(seconds(2023, 11, 5, 6))
        );
        assert_eq!(
            york.transition(seconds(2400, 1, 1, 0), true),
            Some(seconds(2400, 3, 12, 7))
        );
        let (_, sydney) = lookup("Australia/Sydney").expect("Sydney");
        assert_eq!(sydney.offset(seconds(2300, 1, 1, 0)), 11 * 3600);
        assert_eq!(sydney.offset(seconds(2300, 7, 1, 0)), 10 * 3600);
        let (link, _) = lookup("US/Eastern").expect("a link");
        assert_eq!(link, "US/Eastern");
        assert_eq!(primary("us/eastern"), Some("America/New_York"));
        let (_, utc) = lookup("UTC").expect("UTC");
        assert_eq!(utc.offset(0), 0);
        assert_eq!(utc.transition(0, true), None);
        assert!(lookup("Mars/Olympus").is_none());
    }
}
