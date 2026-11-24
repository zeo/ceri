// the CSS Grid values a style carries: track lists with repeat(), minmax() and line names,
// template areas, and item placement; lists and areas are interned so Style stays Copy

use crate::css::{Length, parse_length};
use crate::hashing::HashMap;
use std::cell::RefCell;
use std::rc::Rc;

// one end of a track's size range
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Breadth {
    Length(Length),
    Fr(f32),
    Auto,
    MinContent,
    MaxContent,
    // fit-content(limit), as a maximum
    FitContent(Length),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct TrackSize {
    pub(crate) min: Breadth,
    pub(crate) max: Breadth,
}

impl TrackSize {
    fn single(breadth: Breadth) -> Self {
        match breadth {
            // a flexible track has an automatic minimum
            Breadth::Fr(_) | Breadth::FitContent(_) => Self {
                min: Breadth::Auto,
                max: breadth,
            },
            _ => Self {
                min: breadth,
                max: breadth,
            },
        }
    }

    pub(crate) const AUTO: Self = Self {
        min: Breadth::Auto,
        max: Breadth::Auto,
    };
}

// repeat(auto-fill, ...) or repeat(auto-fit, ...), expanded once the grid's size is known
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct AutoRepeat {
    pub(crate) fit: bool,
    // where the repeated tracks go among the explicit ones
    pub(crate) at: usize,
    pub(crate) tracks: Vec<TrackSize>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct TrackList {
    pub(crate) tracks: Vec<TrackSize>,
    pub(crate) repeat: Option<AutoRepeat>,
    // line names, each with the index of the line it names when auto repeats run once
    pub(crate) names: Vec<(usize, String)>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Tracks(u32);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Areas(u32);

// a named area's rows and columns as zero-based, end-exclusive line ranges
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct NamedArea {
    pub(crate) name: String,
    pub(crate) rows: (usize, usize),
    pub(crate) columns: (usize, usize),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct AreaMap {
    pub(crate) areas: Vec<NamedArea>,
    pub(crate) rows: usize,
    pub(crate) columns: usize,
}

#[derive(Default)]
struct Interned {
    track_lists: Vec<Rc<TrackList>>,
    track_keys: HashMap<String, u32>,
    area_maps: Vec<Rc<AreaMap>>,
    area_keys: HashMap<String, u32>,
    names: Vec<Rc<str>>,
    name_keys: HashMap<String, u32>,
}

thread_local! {
    static INTERNED: RefCell<Interned> = RefCell::new(Interned::default());
}

impl Tracks {
    pub(crate) const NONE: Self = Self(0);

    pub(crate) fn list(self) -> Rc<TrackList> {
        if self.0 == 0 {
            return Rc::new(TrackList::default());
        }
        INTERNED.with(|interned| interned.borrow().track_lists[self.0 as usize - 1].clone())
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        let key = value.trim().to_string();
        if key.eq_ignore_ascii_case("none") {
            return Some(Self::NONE);
        }
        if let Some(id) = INTERNED.with(|interned| interned.borrow().track_keys.get(&key).copied())
        {
            return Some(Self(id));
        }
        let list = parse_track_list(&key)?;
        Some(INTERNED.with(|interned| {
            let mut interned = interned.borrow_mut();
            interned.track_lists.push(Rc::new(list));
            let id = interned.track_lists.len() as u32;
            interned.track_keys.insert(key, id);
            Self(id)
        }))
    }
}

impl Areas {
    pub(crate) const NONE: Self = Self(0);

    pub(crate) fn map(self) -> Rc<AreaMap> {
        if self.0 == 0 {
            return Rc::new(AreaMap::default());
        }
        INTERNED.with(|interned| interned.borrow().area_maps[self.0 as usize - 1].clone())
    }

    // grid-template-areas: quoted rows of names, with dots for empty cells
    pub(crate) fn parse(value: &str) -> Option<Self> {
        let key = value.trim().to_string();
        if key.eq_ignore_ascii_case("none") {
            return Some(Self::NONE);
        }
        if let Some(id) = INTERNED.with(|interned| interned.borrow().area_keys.get(&key).copied()) {
            return Some(Self(id));
        }
        let rows = key
            .split(['"', '\''])
            .skip(1)
            .step_by(2)
            .map(|row| row.split_ascii_whitespace().collect::<Vec<_>>())
            .collect::<Vec<_>>();
        let columns = rows.first()?.len();
        if columns == 0 || rows.iter().any(|row| row.len() != columns) {
            return None;
        }
        let mut areas: Vec<NamedArea> = Vec::new();
        for (row_index, row) in rows.iter().enumerate() {
            for (column_index, cell) in row.iter().enumerate() {
                if cell.chars().all(|character| character == '.') {
                    continue;
                }
                match areas.iter_mut().find(|area| area.name == *cell) {
                    Some(area) => {
                        area.rows.0 = area.rows.0.min(row_index);
                        area.rows.1 = area.rows.1.max(row_index + 1);
                        area.columns.0 = area.columns.0.min(column_index);
                        area.columns.1 = area.columns.1.max(column_index + 1);
                    }
                    None => areas.push(NamedArea {
                        name: cell.to_string(),
                        rows: (row_index, row_index + 1),
                        columns: (column_index, column_index + 1),
                    }),
                }
            }
        }
        let map = AreaMap {
            areas,
            rows: rows.len(),
            columns,
        };
        Some(INTERNED.with(|interned| {
            let mut interned = interned.borrow_mut();
            interned.area_maps.push(Rc::new(map));
            let id = interned.area_maps.len() as u32;
            interned.area_keys.insert(key, id);
            Self(id)
        }))
    }
}

pub(crate) fn intern_name(name: &str) -> u32 {
    INTERNED.with(|interned| {
        let mut interned = interned.borrow_mut();
        if let Some(id) = interned.name_keys.get(name) {
            return *id;
        }
        interned.names.push(name.into());
        let id = interned.names.len() as u32 - 1;
        interned.name_keys.insert(name.to_string(), id);
        id
    })
}

pub(crate) fn name(id: u32) -> Rc<str> {
    INTERNED.with(|interned| interned.borrow().names[id as usize].clone())
}

// one side of an item's placement
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum GridLine {
    #[default]
    Auto,
    // a one-based line number, negative counting from the end of the explicit grid
    Line(i32),
    Span(u32),
    // a named line or area, and which occurrence of it
    Named(u32, i32),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct GridPlacement {
    pub(crate) start: GridLine,
    pub(crate) end: GridLine,
}

impl GridPlacement {
    pub(crate) const AUTO: Self = Self {
        start: GridLine::Auto,
        end: GridLine::Auto,
    };
}

pub(crate) fn parse_line(value: &str) -> Option<GridLine> {
    let words = value.split_ascii_whitespace().collect::<Vec<_>>();
    let (span, words) = match words.split_first() {
        Some((first, rest)) if first.eq_ignore_ascii_case("span") => (true, rest),
        _ => match words.split_last() {
            Some((last, rest)) if last.eq_ignore_ascii_case("span") => (true, rest),
            _ => (false, words.as_slice()),
        },
    };
    let mut number = None;
    let mut named = None;
    for word in words {
        if word.eq_ignore_ascii_case("auto") {
            return Some(GridLine::Auto);
        }
        match word.parse::<i32>() {
            Ok(value) => number = Some(value),
            Err(_) => named = Some(intern_name(word)),
        }
    }
    Some(match (span, named, number) {
        (true, None, number) => GridLine::Span(number.unwrap_or(1).max(1) as u32),
        // a named span counts lines of that name; the name alone reads as one
        (true, Some(_), number) => GridLine::Span(number.unwrap_or(1).max(1) as u32),
        (false, Some(name), number) => GridLine::Named(name, number.unwrap_or(1)),
        (false, None, Some(number)) if number != 0 => GridLine::Line(number),
        _ => return None,
    })
}

// grid-column, grid-row: start, optionally followed by a slash and the end
pub(crate) fn parse_placement(value: &str) -> Option<GridPlacement> {
    let mut parts = value.splitn(2, '/');
    let start = parse_line(parts.next()?)?;
    let end = match parts.next() {
        Some(end) => parse_line(end)?,
        // a lone name places the item on both of that area's edges
        None => match start {
            GridLine::Named(..) => start,
            _ => GridLine::Auto,
        },
    };
    Some(GridPlacement { start, end })
}

fn parse_breadth(value: &str) -> Option<Breadth> {
    let value = value.trim();
    Some(match value {
        "auto" => Breadth::Auto,
        "min-content" => Breadth::MinContent,
        "max-content" => Breadth::MaxContent,
        _ => {
            if let Some(number) = value.strip_suffix("fr") {
                let number = number.trim().parse::<f32>().ok()?;
                if number < 0.0 {
                    return None;
                }
                Breadth::Fr(number)
            } else if let Some(argument) = value
                .strip_prefix("fit-content(")
                .and_then(|rest| rest.strip_suffix(')'))
            {
                Breadth::FitContent(parse_length(argument)?)
            } else {
                match parse_length(value)? {
                    Length::Auto => Breadth::Auto,
                    length => Breadth::Length(length),
                }
            }
        }
    })
}

fn parse_track_size(value: &str) -> Option<TrackSize> {
    if let Some(arguments) = value
        .strip_prefix("minmax(")
        .and_then(|rest| rest.strip_suffix(')'))
    {
        let (minimum, maximum) = split_top_level_comma(arguments)?;
        let min = parse_breadth(minimum)?;
        // a flexible minimum is not allowed
        if matches!(min, Breadth::Fr(_)) {
            return None;
        }
        return Some(TrackSize {
            min,
            max: parse_breadth(maximum)?,
        });
    }
    parse_breadth(value).map(TrackSize::single)
}

fn split_top_level_comma(text: &str) -> Option<(&str, &str)> {
    let mut depth = 0;
    for (index, character) in text.char_indices() {
        match character {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => return Some((text[..index].trim(), text[index + 1..].trim())),
            _ => {}
        }
    }
    None
}

// splits a track list into top-level tokens: sizes, functions and bracketed names
fn tokens(value: &str) -> Vec<&str> {
    let mut tokens = Vec::new();
    let mut depth = 0;
    let mut start = None;
    for (index, character) in value.char_indices() {
        match character {
            '(' | '[' => {
                if depth == 0 && start.is_none() {
                    start = Some(index);
                }
                depth += 1;
            }
            ')' | ']' => depth -= 1,
            whitespace if whitespace.is_whitespace() && depth == 0 => {
                if let Some(begin) = start.take() {
                    tokens.push(&value[begin..index]);
                }
            }
            _ => {
                if start.is_none() {
                    start = Some(index);
                }
            }
        }
    }
    if let Some(begin) = start {
        tokens.push(&value[begin..]);
    }
    tokens
}

fn parse_track_list(value: &str) -> Option<TrackList> {
    let mut list = TrackList::default();
    for token in tokens(value) {
        if let Some(names) = token
            .strip_prefix('[')
            .and_then(|rest| rest.strip_suffix(']'))
        {
            // lines past an auto repeat count its tracks once
            let line =
                list.tracks.len() + list.repeat.as_ref().map_or(0, |repeat| repeat.tracks.len());
            list.names.extend(
                names
                    .split_ascii_whitespace()
                    .map(|name| (line, name.to_string())),
            );
            continue;
        }
        // line names keep their case; sizes and functions do not
        let token = token.to_ascii_lowercase();
        let token = token.as_str();
        if let Some(arguments) = token
            .strip_prefix("repeat(")
            .and_then(|rest| rest.strip_suffix(')'))
        {
            let (count, body) = split_top_level_comma(arguments)?;
            let repeated = tokens(body)
                .into_iter()
                .filter(|token| !token.starts_with('['))
                .map(parse_track_size)
                .collect::<Option<Vec<_>>>()?;
            if repeated.is_empty() {
                return None;
            }
            match count {
                "auto-fill" | "auto-fit" => {
                    if list.repeat.is_some() {
                        return None;
                    }
                    list.repeat = Some(AutoRepeat {
                        fit: count == "auto-fit",
                        at: list.tracks.len(),
                        tracks: repeated,
                    });
                }
                count => {
                    let count = count.parse::<usize>().ok().filter(|count| *count > 0)?;
                    for _ in 0..count.min(10_000) {
                        list.tracks.extend_from_slice(&repeated);
                    }
                }
            }
            continue;
        }
        list.tracks.push(parse_track_size(token)?);
    }
    Some(list)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_track_lists_areas_and_placements() {
        let list = Tracks::parse("[full-start] 100px repeat(2, minmax(10px, 1fr)) [full-end] auto")
            .expect("track list should parse")
            .list();
        assert_eq!(list.tracks.len(), 4);
        assert_eq!(
            list.tracks[0],
            TrackSize::single(Breadth::Length(Length::px(100.0)))
        );
        assert_eq!(
            list.tracks[1],
            TrackSize {
                min: Breadth::Length(Length::px(10.0)),
                max: Breadth::Fr(1.0),
            }
        );
        assert_eq!(
            list.names,
            vec![(0, "full-start".to_string()), (3, "full-end".to_string())]
        );
        let fill = Tracks::parse("repeat(auto-fill, minmax(120px, 1fr))")
            .expect("auto-fill should parse")
            .list();
        assert!(
            fill.repeat
                .as_ref()
                .is_some_and(|repeat| !repeat.fit && repeat.at == 0)
        );
        let areas = Areas::parse("\"head head\" \"side main\"")
            .expect("areas should parse")
            .map();
        assert_eq!(areas.columns, 2);
        assert_eq!(
            areas.areas[0],
            NamedArea {
                name: "head".to_string(),
                rows: (0, 1),
                columns: (0, 2),
            }
        );
        assert_eq!(
            parse_placement("2 / span 3"),
            Some(GridPlacement {
                start: GridLine::Line(2),
                end: GridLine::Span(3),
            })
        );
        assert!(matches!(
            parse_placement("main").map(|placement| placement.start),
            Some(GridLine::Named(..))
        ));
        assert_eq!(Tracks::parse("minmax(1fr, 2fr)"), None);
    }
}
