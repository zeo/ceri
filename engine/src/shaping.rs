// glyph selection and positioning from a font's OpenType tables: GSUB single, multiple,
// alternate and ligature substitutions (directly, through extensions, and from chained contexts), Arabic
// joining forms, and GPOS pair kerning with the legacy kern table as a fallback

use crate::font::FontFace;
use crate::text_tables::JOINING_TYPE;

// a glyph to draw, in font units, with the character it began at
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ShapedGlyph {
    pub(crate) glyph: u16,
    pub(crate) advance: f32,
    pub(crate) offset: f32,
    // index of the first character this glyph stands for
    pub(crate) cluster: usize,
}

// positional forms each glyph may take, from its joining with its neighbours
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Form {
    None,
    Isolated,
    Initial,
    Medial,
    Final,
}

fn u16_at(data: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*data.get(at)?, *data.get(at + 1)?]))
}

fn i16_at(data: &[u8], at: usize) -> Option<i16> {
    u16_at(data, at).map(|value| value as i16)
}

fn u32_at(data: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes([
        *data.get(at)?,
        *data.get(at + 1)?,
        *data.get(at + 2)?,
        *data.get(at + 3)?,
    ]))
}

// the index of a glyph in a coverage table
fn coverage(data: &[u8], table: usize, glyph: u16) -> Option<usize> {
    match u16_at(data, table)? {
        1 => {
            let count = usize::from(u16_at(data, table + 2)?);
            let (mut low, mut high) = (0, count);
            while low < high {
                let middle = (low + high) / 2;
                let found = u16_at(data, table + 4 + middle * 2)?;
                match found.cmp(&glyph) {
                    std::cmp::Ordering::Equal => return Some(middle),
                    std::cmp::Ordering::Less => low = middle + 1,
                    std::cmp::Ordering::Greater => high = middle,
                }
            }
            None
        }
        2 => {
            let count = usize::from(u16_at(data, table + 2)?);
            for range in 0..count {
                let record = table + 4 + range * 6;
                let (start, end, index) = (
                    u16_at(data, record)?,
                    u16_at(data, record + 2)?,
                    u16_at(data, record + 4)?,
                );
                if (start..=end).contains(&glyph) {
                    return Some(usize::from(index) + usize::from(glyph - start));
                }
                if start > glyph {
                    break;
                }
            }
            None
        }
        _ => None,
    }
}

// the class a class definition table gives a glyph
fn class_of(data: &[u8], table: usize, glyph: u16) -> u16 {
    let lookup = || -> Option<u16> {
        match u16_at(data, table)? {
            1 => {
                let start = u16_at(data, table + 2)?;
                let count = u16_at(data, table + 4)?;
                if glyph < start || glyph - start >= count {
                    return Some(0);
                }
                u16_at(data, table + 6 + usize::from(glyph - start) * 2)
            }
            2 => {
                let count = usize::from(u16_at(data, table + 2)?);
                for range in 0..count {
                    let record = table + 4 + range * 6;
                    if (u16_at(data, record)?..=u16_at(data, record + 2)?).contains(&glyph) {
                        return u16_at(data, record + 4);
                    }
                }
                Some(0)
            }
            _ => Some(0),
        }
    };
    lookup().unwrap_or(0)
}

const MAX_FEATURE_LOOKUPS: usize = 4096;

// a feature tag and the value it is set to
type Feature = ([u8; 4], u32);

// the lookups, in lookup list order, that a script's default language system turns on for the
// features asked for, each with the feature tag and value that turned it on
fn feature_lookups(
    data: &[u8],
    table: usize,
    scripts: &[&[u8; 4]],
    features: &[Feature],
) -> Vec<(u16, Feature)> {
    let find = || -> Option<Vec<(u16, Feature)>> {
        let script_list = table + usize::from(u16_at(data, table + 4)?);
        let feature_list = table + usize::from(u16_at(data, table + 6)?);
        let script_count = usize::from(u16_at(data, script_list)?);
        let mut script = None;
        'wanted: for wanted in scripts {
            for index in 0..script_count {
                let record = script_list + 2 + index * 6;
                if data.get(record..record + 4)? == wanted.as_slice() {
                    script = Some(script_list + usize::from(u16_at(data, record + 4)?));
                    break 'wanted;
                }
            }
        }
        let script = script?;
        let default = u16_at(data, script)?;
        if default == 0 {
            return None;
        }
        let language = script + usize::from(default);
        let count = usize::from(u16_at(data, language + 4)?);
        let mut lookups = Vec::new();
        for index in 0..count {
            let feature = usize::from(u16_at(data, language + 6 + index * 2)?);
            let record = feature_list + 2 + feature * 6;
            let tag = data.get(record..record + 4)?;
            let Some(wanted) = features.iter().find(|(wanted, _)| wanted.as_slice() == tag) else {
                continue;
            };
            let feature_table = feature_list + usize::from(u16_at(data, record + 4)?);
            let lookup_count = usize::from(u16_at(data, feature_table + 2)?);
            for lookup in 0..lookup_count.min(MAX_FEATURE_LOOKUPS - lookups.len()) {
                lookups.push((u16_at(data, feature_table + 4 + lookup * 2)?, *wanted));
            }
        }
        lookups.sort_unstable_by_key(|(index, _)| *index);
        lookups.dedup_by_key(|(index, _)| *index);
        Some(lookups)
    };
    find().unwrap_or_default()
}

// a lookup's flag and subtables, with extension subtables followed to what they wrap
fn lookup_subtables(
    data: &[u8],
    table: usize,
    index: u16,
    extension_type: u16,
) -> Option<(u16, u16, Vec<usize>)> {
    let lookup_list = table + usize::from(u16_at(data, table + 8)?);
    let lookup = lookup_list + usize::from(u16_at(data, lookup_list + 2 + usize::from(index) * 2)?);
    let kind = u16_at(data, lookup)?;
    let flag = u16_at(data, lookup + 2)?;
    let count = usize::from(u16_at(data, lookup + 4)?);
    let mut wrapped = kind;
    let mut subtables = Vec::with_capacity(count);
    for subtable in 0..count {
        let mut at = lookup + usize::from(u16_at(data, lookup + 6 + subtable * 2)?);
        if kind == extension_type {
            wrapped = u16_at(data, at + 2)?;
            at += u32_at(data, at + 4)? as usize;
        }
        subtables.push(at);
    }
    Some((wrapped, flag, subtables))
}

struct Glyph {
    id: u16,
    cluster: usize,
    form: Form,
}

pub(crate) struct Shaper<'a> {
    face: &'a FontFace,
    data: &'a [u8],
    // GDEF glyph classes, where 3 marks a mark
    glyph_classes: Option<usize>,
}

const MAX_SHAPED_GLYPHS: usize = 4096;

impl<'a> Shaper<'a> {
    pub(crate) fn new(face: &'a FontFace) -> Self {
        let data = face.data();
        let glyph_classes = face.table(b"GDEF").and_then(|gdef| {
            let offset = u16_at(data, gdef + 4)?;
            (offset != 0).then_some(gdef + usize::from(offset))
        });
        Self {
            face,
            data,
            glyph_classes,
        }
    }

    fn is_mark(&self, glyph: u16) -> bool {
        self.glyph_classes
            .is_some_and(|classes| class_of(self.data, classes, glyph) == 3)
    }

    fn skipped(&self, flag: u16, glyph: u16) -> bool {
        flag & 0x0008 != 0 && self.is_mark(glyph)
    }

    // shapes characters in logical order with the default features set by the overrides in
    // order, zero turning one off and higher values picking alternates; the glyphs come back in
    // logical order too
    pub(crate) fn shape(&self, characters: &[char], overrides: &[Feature]) -> Vec<ShapedGlyph> {
        let mut glyphs = characters
            .iter()
            .enumerate()
            .map(|(cluster, character)| Glyph {
                id: self.face.glyph_for(*character),
                cluster,
                form: Form::None,
            })
            .collect::<Vec<_>>();
        let arabic = characters
            .iter()
            .any(|character| joining_type(*character).is_some());
        if arabic {
            assign_forms(characters, &mut glyphs);
        }
        let mut features = [*b"ccmp", *b"locl", *b"liga", *b"clig", *b"calt", *b"kern"]
            .map(|tag| (tag, 1))
            .to_vec();
        if arabic {
            features.extend([*b"isol", *b"init", *b"medi", *b"fina", *b"rlig"].map(|tag| (tag, 1)));
        }
        for &(tag, value) in overrides {
            features.retain(|(known, _)| *known != tag);
            if value != 0 {
                features.push((tag, value));
            }
        }
        if let Some(gsub) = self.face.table(b"GSUB") {
            let scripts: &[&[u8; 4]] = if arabic {
                &[b"arab", b"DFLT", b"latn"]
            } else {
                &[b"latn", b"DFLT", b"cyrl", b"grek"]
            };
            for (lookup, feature) in feature_lookups(self.data, gsub, scripts, &features) {
                self.substitute(gsub, lookup, feature, &mut glyphs);
                if glyphs.len() > MAX_SHAPED_GLYPHS {
                    break;
                }
            }
        }
        let mut shaped = glyphs
            .iter()
            .map(|glyph| ShapedGlyph {
                glyph: glyph.id,
                advance: self.face.advance_units(glyph.id),
                offset: 0.0,
                cluster: glyph.cluster,
            })
            .collect::<Vec<_>>();
        if features.iter().any(|(tag, _)| tag == b"kern") {
            self.kern(&mut shaped, arabic);
        }
        shaped
    }

    fn substitute(
        &self,
        gsub: usize,
        lookup: u16,
        (feature, value): Feature,
        glyphs: &mut Vec<Glyph>,
    ) {
        let Some((kind, flag, subtables)) = lookup_subtables(self.data, gsub, lookup, 7) else {
            return;
        };
        let wanted_form = match &feature {
            b"isol" => Some(Form::Isolated),
            b"init" => Some(Form::Initial),
            b"medi" => Some(Form::Medial),
            b"fina" => Some(Form::Final),
            _ => None,
        };
        let mut index = 0;
        while index < glyphs.len() {
            if wanted_form.is_some_and(|form| glyphs[index].form != form)
                || self.skipped(flag, glyphs[index].id)
            {
                index += 1;
                continue;
            }
            let mut applied = false;
            for &subtable in &subtables {
                if let Some(advance) =
                    self.apply_substitution(gsub, (kind, flag, value), subtable, glyphs, index)
                {
                    index += advance;
                    applied = true;
                    break;
                }
            }
            if !applied {
                index += 1;
            }
        }
    }

    // applies one substitution subtable of a lookup's kind and flag, for a feature value, at a
    // position; returns how far to move on
    fn apply_substitution(
        &self,
        gsub: usize,
        (kind, flag, value): (u16, u16, u32),
        subtable: usize,
        glyphs: &mut Vec<Glyph>,
        index: usize,
    ) -> Option<usize> {
        let data = self.data;
        let glyph = glyphs[index].id;
        match kind {
            1 => {
                let covered = coverage(
                    data,
                    subtable + usize::from(u16_at(data, subtable + 2)?),
                    glyph,
                )?;
                glyphs[index].id = match u16_at(data, subtable)? {
                    1 => (glyph as i32 + i32::from(i16_at(data, subtable + 4)?)) as u16,
                    2 => u16_at(data, subtable + 6 + covered * 2)?,
                    _ => return None,
                };
                Some(1)
            }
            2 => {
                let covered = coverage(
                    data,
                    subtable + usize::from(u16_at(data, subtable + 2)?),
                    glyph,
                )?;
                let sequence = subtable + usize::from(u16_at(data, subtable + 6 + covered * 2)?);
                let count = usize::from(u16_at(data, sequence)?);
                let cluster = glyphs[index].cluster;
                let form = glyphs[index].form;
                let replacement = (0..count)
                    .map(|position| {
                        u16_at(data, sequence + 2 + position * 2).map(|id| Glyph {
                            id,
                            cluster,
                            form,
                        })
                    })
                    .collect::<Option<Vec<_>>>()?;
                let length = replacement.len();
                glyphs.splice(index..=index, replacement);
                Some(length.max(1))
            }
            // the value picks an alternate from one, as harfbuzz does, and out of range picks none
            3 => {
                let covered = coverage(
                    data,
                    subtable + usize::from(u16_at(data, subtable + 2)?),
                    glyph,
                )?;
                let set = subtable + usize::from(u16_at(data, subtable + 6 + covered * 2)?);
                let count = u32::from(u16_at(data, set)?);
                if value == 0 || value > count {
                    return None;
                }
                glyphs[index].id = u16_at(data, set + 2 * value as usize)?;
                Some(1)
            }
            4 => {
                let covered = coverage(
                    data,
                    subtable + usize::from(u16_at(data, subtable + 2)?),
                    glyph,
                )?;
                let set = subtable + usize::from(u16_at(data, subtable + 6 + covered * 2)?);
                let count = usize::from(u16_at(data, set)?);
                for ligature in 0..count {
                    let table = set + usize::from(u16_at(data, set + 2 + ligature * 2)?);
                    let components = usize::from(u16_at(data, table + 2)?);
                    let mut positions = vec![index];
                    let mut next = index + 1;
                    let mut matched = true;
                    for component in 1..components {
                        while next < glyphs.len() && self.skipped(flag, glyphs[next].id) {
                            next += 1;
                        }
                        if next >= glyphs.len()
                            || Some(glyphs[next].id) != u16_at(data, table + 2 + component * 2)
                        {
                            matched = false;
                            break;
                        }
                        positions.push(next);
                        next += 1;
                    }
                    if matched {
                        glyphs[index].id = u16_at(data, table)?;
                        for position in positions.into_iter().skip(1).rev() {
                            glyphs.remove(position);
                        }
                        return Some(1);
                    }
                }
                None
            }
            6 => self.chained_context(gsub, value, subtable, glyphs, index),
            _ => None,
        }
    }

    // chained context format 3: coverage tables for the backtrack, input and lookahead, then
    // nested lookups at input positions
    fn chained_context(
        &self,
        gsub: usize,
        value: u32,
        subtable: usize,
        glyphs: &mut Vec<Glyph>,
        index: usize,
    ) -> Option<usize> {
        let data = self.data;
        if u16_at(data, subtable)? != 3 {
            return None;
        }
        let mut at = subtable + 2;
        let mut read_coverages = || -> Option<Vec<usize>> {
            let count = usize::from(u16_at(data, at)?);
            let tables = (0..count)
                .map(|position| {
                    u16_at(data, at + 2 + position * 2).map(|offset| subtable + usize::from(offset))
                })
                .collect::<Option<Vec<_>>>()?;
            at += 2 + count * 2;
            Some(tables)
        };
        let backtrack = read_coverages()?;
        let input = read_coverages()?;
        let lookahead = read_coverages()?;
        if input.is_empty() || index + input.len() > glyphs.len() || index < backtrack.len() {
            return None;
        }
        let matches =
            |position: usize, table: usize| coverage(data, table, glyphs[position].id).is_some();
        if !input
            .iter()
            .enumerate()
            .all(|(offset, table)| matches(index + offset, *table))
            || !backtrack
                .iter()
                .enumerate()
                .all(|(offset, table)| matches(index - 1 - offset, *table))
            || !lookahead.iter().enumerate().all(|(offset, table)| {
                index + input.len() + offset < glyphs.len()
                    && matches(index + input.len() + offset, *table)
            })
        {
            return None;
        }
        let count = usize::from(u16_at(data, at)?);
        for record in 0..count {
            let position = usize::from(u16_at(data, at + 2 + record * 4)?);
            let lookup = u16_at(data, at + 4 + record * 4)?;
            if let Some((kind, flag, subtables)) = lookup_subtables(data, gsub, lookup, 7) {
                for nested in subtables {
                    // nested contexts are not followed further
                    if kind != 6
                        && index + position < glyphs.len()
                        && self
                            .apply_substitution(
                                gsub,
                                (kind, flag, value),
                                nested,
                                glyphs,
                                index + position,
                            )
                            .is_some()
                    {
                        break;
                    }
                }
            }
        }
        Some(input.len())
    }

    // pair kerning from GPOS, or from the kern table when the font has no GPOS kerning
    fn kern(&self, glyphs: &mut [ShapedGlyph], arabic: bool) {
        let data = self.data;
        if let Some(gpos) = self.face.table(b"GPOS") {
            let scripts: &[&[u8; 4]] = if arabic {
                &[b"arab", b"DFLT"]
            } else {
                &[b"latn", b"DFLT"]
            };
            let lookups = feature_lookups(data, gpos, scripts, &[(*b"kern", 1)]);
            if !lookups.is_empty() {
                for (lookup, _) in lookups {
                    let Some((kind, flag, subtables)) = lookup_subtables(data, gpos, lookup, 9)
                    else {
                        continue;
                    };
                    if kind != 2 {
                        continue;
                    }
                    for index in 0..glyphs.len().saturating_sub(1) {
                        if self.skipped(flag, glyphs[index].glyph) {
                            continue;
                        }
                        let mut next = index + 1;
                        while next < glyphs.len() && self.skipped(flag, glyphs[next].glyph) {
                            next += 1;
                        }
                        if next >= glyphs.len() {
                            break;
                        }
                        if let Some(adjust) = subtables.iter().find_map(|&subtable| {
                            pair_adjustment(data, subtable, glyphs[index].glyph, glyphs[next].glyph)
                        }) {
                            glyphs[index].advance += adjust;
                        }
                    }
                }
                return;
            }
        }
        if let Some(kern) = self.face.table(b"kern") {
            for index in 0..glyphs.len().saturating_sub(1) {
                glyphs[index].advance +=
                    legacy_kern(data, kern, glyphs[index].glyph, glyphs[index + 1].glyph);
            }
        }
    }
}

// the x advance adjustment a pair positioning subtable gives the first glyph of a pair
fn pair_adjustment(data: &[u8], subtable: usize, first: u16, second: u16) -> Option<f32> {
    let format = u16_at(data, subtable)?;
    let covered = coverage(
        data,
        subtable + usize::from(u16_at(data, subtable + 2)?),
        first,
    )?;
    let value_format1 = u16_at(data, subtable + 4)?;
    let value_format2 = u16_at(data, subtable + 6)?;
    let size = |format: u16| format.count_ones() as usize * 2;
    // x advance sits after x and y placement when they are present
    let advance_in = |record: usize| -> Option<f32> {
        if value_format1 & 0x0004 == 0 {
            return Some(0.0);
        }
        let skip = (value_format1 & 0x0003).count_ones() as usize * 2;
        i16_at(data, record + skip).map(f32::from)
    };
    match format {
        1 => {
            let set = subtable + usize::from(u16_at(data, subtable + 10 + covered * 2)?);
            let count = usize::from(u16_at(data, set)?);
            let record_size = 2 + size(value_format1) + size(value_format2);
            for pair in 0..count {
                let record = set + 2 + pair * record_size;
                if u16_at(data, record)? == second {
                    return advance_in(record + 2);
                }
            }
            None
        }
        2 => {
            let class1 = class_of(
                data,
                subtable + usize::from(u16_at(data, subtable + 8)?),
                first,
            );
            let class2 = class_of(
                data,
                subtable + usize::from(u16_at(data, subtable + 10)?),
                second,
            );
            let class2_count = usize::from(u16_at(data, subtable + 14)?);
            let record_size = size(value_format1) + size(value_format2);
            let record = subtable
                + 16
                + (usize::from(class1) * class2_count + usize::from(class2)) * record_size;
            advance_in(record)
        }
        _ => None,
    }
}

fn legacy_kern(data: &[u8], table: usize, first: u16, second: u16) -> f32 {
    let lookup = || -> Option<f32> {
        if u16_at(data, table)? != 0 {
            return None;
        }
        let subtable = table + 4;
        if u16_at(data, subtable + 4)? >> 8 != 0 {
            return None;
        }
        let pairs = usize::from(u16_at(data, subtable + 6)?);
        let key = (u32::from(first) << 16) | u32::from(second);
        let (mut low, mut high) = (0, pairs);
        while low < high {
            let middle = (low + high) / 2;
            let record = subtable + 14 + middle * 6;
            match u32_at(data, record)?.cmp(&key) {
                std::cmp::Ordering::Equal => return i16_at(data, record + 4).map(f32::from),
                std::cmp::Ordering::Less => low = middle + 1,
                std::cmp::Ordering::Greater => high = middle,
            }
        }
        None
    };
    lookup().unwrap_or(0.0)
}

fn joining_type(character: char) -> Option<u8> {
    let point = u32::from(character);
    JOINING_TYPE
        .binary_search_by(|&(first, last, _)| {
            if point < first {
                std::cmp::Ordering::Greater
            } else if point > last {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .ok()
        .map(|index| JOINING_TYPE[index].2)
        .or_else(|| {
            (crate::bidi::bidi_class(character) == crate::bidi::BidiClass::Nsm).then_some(b'T')
        })
}

// each letter's positional form from whether it joins the letters on either side, looking
// past transparent marks
fn assign_forms(characters: &[char], glyphs: &mut [Glyph]) {
    let kinds = characters
        .iter()
        .map(|character| joining_type(*character))
        .collect::<Vec<_>>();
    let joins_after = |kind: Option<u8>| matches!(kind, Some(b'D' | b'L' | b'C'));
    let joins_before = |kind: Option<u8>| matches!(kind, Some(b'D' | b'R' | b'C'));
    for (index, glyph) in glyphs.iter_mut().enumerate() {
        let Some(kind) = kinds.get(glyph.cluster).copied().flatten() else {
            continue;
        };
        if kind == b'T' || kind == b'U' {
            continue;
        }
        let neighbour = |range: &mut dyn Iterator<Item = usize>| {
            range
                .map(|position| kinds[position])
                .find(|kind| *kind != Some(b'T'))
                .flatten()
        };
        let previous = neighbour(&mut (0..index).rev());
        let next = neighbour(&mut (index + 1..kinds.len()));
        let from_previous = joins_after(previous) && joins_before(Some(kind));
        let to_next = joins_after(Some(kind)) && joins_before(next);
        glyph.form = match (from_previous, to_next) {
            (true, true) => Form::Medial,
            (true, false) => Form::Final,
            (false, true) => Form::Initial,
            (false, false) => Form::Isolated,
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_extension_subtable_of_a_lookup_is_followed() {
        let mut table = vec![0_u8; 24];
        table[8..10].copy_from_slice(&10_u16.to_be_bytes());
        for (at, value) in [
            (10, 1),
            (12, 4),
            (14, 9),
            (16, 0),
            (18, 2),
            (20, 10),
            (22, 18),
        ] {
            table[at..at + 2].copy_from_slice(&u16::to_be_bytes(value));
        }
        for offset in [256_u32, 512] {
            table.extend(1_u16.to_be_bytes());
            table.extend(2_u16.to_be_bytes());
            table.extend(offset.to_be_bytes());
        }
        assert_eq!(
            lookup_subtables(&table, 0, 0, 9),
            Some((2, 0, vec![24 + 256, 32 + 512]))
        );
    }
}
