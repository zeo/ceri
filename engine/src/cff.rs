// glyph outlines from a CFF table, the PostScript flavor of OpenType, by running each
// glyph's Type 2 charstring; curves come back flattened into straight segments

use crate::font::{Outline, OutlinePoint};

const MAX_SUBR_DEPTH: usize = 10;
const MAX_STACK: usize = 513;
// segments each cubic curve is split into
const CURVE_STEPS: usize = 8;

// an INDEX: the start and end of each item in the table's data
#[derive(Clone, Debug, Default)]
struct Index {
    items: Vec<(usize, usize)>,
}

impl Index {
    fn get(&self, index: usize) -> Option<(usize, usize)> {
        self.items.get(index).copied()
    }

    fn len(&self) -> usize {
        self.items.len()
    }
}

fn read_offset(data: &[u8], at: usize, size: u8) -> Option<usize> {
    let bytes = data.get(at..at + usize::from(size))?;
    Some(
        bytes
            .iter()
            .fold(0_usize, |value, byte| (value << 8) | usize::from(*byte)),
    )
}

// reads an INDEX at an offset, returning it and where the data after it starts
fn read_index(data: &[u8], at: usize) -> Option<(Index, usize)> {
    let count = usize::from(u16::from_be_bytes([*data.get(at)?, *data.get(at + 1)?]));
    if count == 0 {
        return Some((Index::default(), at + 2));
    }
    let size = *data.get(at + 2)?;
    if !(1..=4).contains(&size) {
        return None;
    }
    let base = at + 3 + (count + 1) * usize::from(size) - 1;
    let mut offsets = Vec::with_capacity(count + 1);
    for index in 0..=count {
        offsets.push(base + read_offset(data, at + 3 + index * usize::from(size), size)?);
    }
    let end = *offsets.last()?;
    if end > data.len() || offsets.windows(2).any(|pair| pair[0] > pair[1]) {
        return None;
    }
    let items = offsets.windows(2).map(|pair| (pair[0], pair[1])).collect();
    Some((Index { items }, end))
}

// the operands of each operator in a DICT
fn read_dict(data: &[u8]) -> Vec<(u16, Vec<f64>)> {
    let mut entries = Vec::new();
    let mut operands = Vec::new();
    let mut at = 0;
    while at < data.len() {
        let byte = data[at];
        at += 1;
        match byte {
            0..=21 => {
                let operator = if byte == 12 {
                    let Some(next) = data.get(at) else { break };
                    at += 1;
                    1200 + u16::from(*next)
                } else {
                    u16::from(byte)
                };
                entries.push((operator, std::mem::take(&mut operands)));
            }
            28 => {
                let Some(bytes) = data.get(at..at + 2) else {
                    break;
                };
                operands.push(f64::from(i16::from_be_bytes([bytes[0], bytes[1]])));
                at += 2;
            }
            29 => {
                let Some(bytes) = data.get(at..at + 4) else {
                    break;
                };
                operands.push(f64::from(i32::from_be_bytes([
                    bytes[0], bytes[1], bytes[2], bytes[3],
                ])));
                at += 4;
            }
            30 => {
                // a real number in packed decimal nibbles
                let mut text = String::new();
                'nibbles: while let Some(&packed) = data.get(at) {
                    at += 1;
                    for nibble in [packed >> 4, packed & 0x0F] {
                        match nibble {
                            0..=9 => text.push(char::from(b'0' + nibble)),
                            0x0A => text.push('.'),
                            0x0B => text.push('E'),
                            0x0C => text.push_str("E-"),
                            0x0E => text.push('-'),
                            0x0F => break 'nibbles,
                            _ => {}
                        }
                    }
                }
                operands.push(text.parse().unwrap_or(0.0));
            }
            32..=246 => operands.push(f64::from(i32::from(byte) - 139)),
            247..=250 => {
                let Some(&next) = data.get(at) else { break };
                at += 1;
                operands.push(f64::from(
                    (i32::from(byte) - 247) * 256 + i32::from(next) + 108,
                ));
            }
            251..=254 => {
                let Some(&next) = data.get(at) else { break };
                at += 1;
                operands.push(f64::from(
                    -(i32::from(byte) - 251) * 256 - i32::from(next) - 108,
                ));
            }
            _ => {}
        }
    }
    entries
}

fn dict_value(entries: &[(u16, Vec<f64>)], operator: u16) -> Option<&[f64]> {
    entries
        .iter()
        .find(|(found, _)| *found == operator)
        .map(|(_, operands)| operands.as_slice())
}

#[derive(Debug)]
pub(crate) struct CffFont {
    // offsets are into the whole font file
    charstrings: Index,
    global_subrs: Index,
    // one local subroutine set per font dict; plain fonts have one
    local_subrs: Vec<Index>,
    // which font dict each glyph uses, for CID-keyed fonts
    fd_select: Vec<u8>,
}

fn private_subrs(data: &[u8], table: usize, operands: Option<&[f64]>) -> Index {
    let Some([size, offset]) = operands else {
        return Index::default();
    };
    let start = table + *offset as usize;
    let Some(private) = data.get(start..start + *size as usize) else {
        return Index::default();
    };
    dict_value(&read_dict(private), 19)
        .and_then(|subrs| subrs.first())
        .and_then(|subrs| read_index(data, start + *subrs as usize))
        .map(|(index, _)| index)
        .unwrap_or_default()
}

pub(crate) fn parse(data: &[u8], table: usize) -> Result<CffFont, String> {
    let header_size = usize::from(*data.get(table + 2).ok_or("the CFF table is empty")?);
    let (_names, after_names) =
        read_index(data, table + header_size).ok_or("the CFF name index is malformed")?;
    let (top_dicts, after_top) =
        read_index(data, after_names).ok_or("the CFF top dict index is malformed")?;
    let (_strings, after_strings) =
        read_index(data, after_top).ok_or("the CFF string index is malformed")?;
    let (global_subrs, _) =
        read_index(data, after_strings).ok_or("the CFF global subroutines are malformed")?;
    let (start, end) = top_dicts.get(0).ok_or("the CFF table has no font")?;
    let top = read_dict(&data[start..end]);
    let charstrings_offset = dict_value(&top, 17)
        .and_then(|value| value.first())
        .ok_or("the CFF font has no charstrings")?;
    let (charstrings, _) = read_index(data, table + *charstrings_offset as usize)
        .ok_or("the CFF charstrings are malformed")?;
    let mut local_subrs = Vec::new();
    let mut fd_select = Vec::new();
    if let Some(fd_array) = dict_value(&top, 1236).and_then(|value| value.first()) {
        // a CID-keyed font: each font dict brings its own private dict
        let (dicts, _) = read_index(data, table + *fd_array as usize)
            .ok_or("the CFF font dicts are malformed")?;
        for index in 0..dicts.len() {
            let (start, end) = dicts.get(index).ok_or("a CFF font dict is missing")?;
            local_subrs.push(private_subrs(
                data,
                table,
                dict_value(&read_dict(&data[start..end]), 18),
            ));
        }
        if let Some(select) = dict_value(&top, 1237).and_then(|value| value.first()) {
            fd_select = read_fd_select(data, table + *select as usize, charstrings.len())
                .ok_or("the CFF font dict selector is malformed")?;
        }
    } else {
        local_subrs.push(private_subrs(data, table, dict_value(&top, 18)));
    }
    Ok(CffFont {
        charstrings,
        global_subrs,
        local_subrs,
        fd_select,
    })
}

fn read_fd_select(data: &[u8], at: usize, glyphs: usize) -> Option<Vec<u8>> {
    match *data.get(at)? {
        0 => Some(data.get(at + 1..at + 1 + glyphs)?.to_vec()),
        3 => {
            let ranges = usize::from(u16::from_be_bytes([*data.get(at + 1)?, *data.get(at + 2)?]));
            let mut select = vec![0_u8; glyphs];
            for range in 0..ranges {
                let record = at + 3 + range * 3;
                let first = usize::from(u16::from_be_bytes([
                    *data.get(record)?,
                    *data.get(record + 1)?,
                ]));
                let dict = *data.get(record + 2)?;
                let next = usize::from(u16::from_be_bytes([
                    *data.get(record + 3)?,
                    *data.get(record + 4)?,
                ]));
                let (first, next) = (first.min(glyphs), next.min(glyphs));
                if first < next {
                    select[first..next].fill(dict);
                }
            }
            Some(select)
        }
        _ => None,
    }
}

fn subr_bias(count: usize) -> i32 {
    if count < 1240 {
        107
    } else if count < 33_900 {
        1131
    } else {
        32_768
    }
}

// the running state of a charstring
struct Charstring<'a> {
    data: &'a [u8],
    font: &'a CffFont,
    local: &'a Index,
    stack: Vec<f64>,
    stems: usize,
    width_taken: bool,
    point: (f64, f64),
    contours: Vec<Vec<OutlinePoint>>,
    done: bool,
}

impl Charstring<'_> {
    fn point(&mut self) {
        if let Some(contour) = self.contours.last_mut() {
            contour.push(OutlinePoint {
                x: self.point.0 as f32,
                y: self.point.1 as f32,
                on_curve: true,
            });
        }
    }

    fn move_to(&mut self, dx: f64, dy: f64) {
        self.point.0 += dx;
        self.point.1 += dy;
        self.contours.push(Vec::new());
        self.point();
    }

    fn line_to(&mut self, dx: f64, dy: f64) {
        self.point.0 += dx;
        self.point.1 += dy;
        self.point();
    }

    fn curve_to(&mut self, (dx1, dy1, dx2, dy2, dx3, dy3): (f64, f64, f64, f64, f64, f64)) {
        let start = self.point;
        let control1 = (start.0 + dx1, start.1 + dy1);
        let control2 = (control1.0 + dx2, control1.1 + dy2);
        let end = (control2.0 + dx3, control2.1 + dy3);
        for step in 1..=CURVE_STEPS {
            let t = step as f64 / CURVE_STEPS as f64;
            let rest = 1.0 - t;
            let a = rest * rest * rest;
            let b = 3.0 * rest * rest * t;
            let c = 3.0 * rest * t * t;
            let d = t * t * t;
            self.point = (
                a * start.0 + b * control1.0 + c * control2.0 + d * end.0,
                a * start.1 + b * control1.1 + c * control2.1 + d * end.1,
            );
            self.point();
        }
        self.point = end;
    }

    // an odd extra operand before the first stack-clearing operator is the advance width
    fn take_width(&mut self, expected_even: bool) {
        if !self.width_taken {
            self.width_taken = true;
            let odd = self.stack.len() % 2 == 1;
            if odd == expected_even && !self.stack.is_empty() {
                self.stack.remove(0);
            }
        }
    }

    fn run(&mut self, start: usize, end: usize, depth: usize) -> Option<()> {
        if depth > MAX_SUBR_DEPTH {
            return None;
        }
        let mut at = start;
        while at < end && !self.done {
            let byte = self.data[at];
            at += 1;
            if self.stack.len() > MAX_STACK {
                return None;
            }
            match byte {
                28 => {
                    let bytes = self.data.get(at..at + 2)?;
                    self.stack
                        .push(f64::from(i16::from_be_bytes([bytes[0], bytes[1]])));
                    at += 2;
                }
                32..=246 => self.stack.push(f64::from(i32::from(byte) - 139)),
                247..=250 => {
                    let next = *self.data.get(at)?;
                    at += 1;
                    self.stack.push(f64::from(
                        (i32::from(byte) - 247) * 256 + i32::from(next) + 108,
                    ));
                }
                251..=254 => {
                    let next = *self.data.get(at)?;
                    at += 1;
                    self.stack.push(f64::from(
                        -(i32::from(byte) - 251) * 256 - i32::from(next) - 108,
                    ));
                }
                255 => {
                    let bytes = self.data.get(at..at + 4)?;
                    self.stack.push(
                        f64::from(i32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
                            / 65536.0,
                    );
                    at += 4;
                }
                1 | 3 | 18 | 23 => {
                    self.take_width(true);
                    self.stems += self.stack.len() / 2;
                    self.stack.clear();
                }
                19 | 20 => {
                    self.take_width(true);
                    self.stems += self.stack.len() / 2;
                    self.stack.clear();
                    at += self.stems.div_ceil(8);
                }
                21 => {
                    self.take_width(true);
                    let dy = self.stack.pop()?;
                    let dx = self.stack.pop()?;
                    self.move_to(dx, dy);
                    self.stack.clear();
                }
                22 | 4 => {
                    self.take_width(false);
                    let delta = self.stack.pop()?;
                    if byte == 22 {
                        self.move_to(delta, 0.0);
                    } else {
                        self.move_to(0.0, delta);
                    }
                    self.stack.clear();
                }
                5 => {
                    for pair in std::mem::take(&mut self.stack).as_chunks::<2>().0.iter() {
                        self.line_to(pair[0], pair[1]);
                    }
                }
                6 | 7 => {
                    let mut horizontal = byte == 6;
                    for delta in std::mem::take(&mut self.stack) {
                        if horizontal {
                            self.line_to(delta, 0.0);
                        } else {
                            self.line_to(0.0, delta);
                        }
                        horizontal = !horizontal;
                    }
                }
                8 => {
                    for curve in std::mem::take(&mut self.stack).as_chunks::<6>().0.iter() {
                        self.curve_to((curve[0], curve[1], curve[2], curve[3], curve[4], curve[5]));
                    }
                }
                24 => {
                    let stack = std::mem::take(&mut self.stack);
                    let curves = stack.len().saturating_sub(2) / 6;
                    for curve in stack.as_chunks::<6>().0.iter().take(curves) {
                        self.curve_to((curve[0], curve[1], curve[2], curve[3], curve[4], curve[5]));
                    }
                    if let [dx, dy] = stack[curves * 6..] {
                        self.line_to(dx, dy);
                    }
                }
                25 => {
                    let stack = std::mem::take(&mut self.stack);
                    let lines = stack.len().saturating_sub(6) / 2;
                    for pair in stack.as_chunks::<2>().0.iter().take(lines) {
                        self.line_to(pair[0], pair[1]);
                    }
                    if let [a, b, c, d, e, f] = stack[lines * 2..] {
                        self.curve_to((a, b, c, d, e, f));
                    }
                }
                26 | 27 => {
                    let mut stack = std::mem::take(&mut self.stack);
                    let mut first = if stack.len() % 4 == 1 {
                        stack.remove(0)
                    } else {
                        0.0
                    };
                    for curve in stack.as_chunks::<4>().0.iter() {
                        if byte == 26 {
                            self.curve_to((first, curve[0], curve[1], curve[2], 0.0, curve[3]));
                        } else {
                            self.curve_to((curve[0], first, curve[1], curve[2], curve[3], 0.0));
                        }
                        first = 0.0;
                    }
                }
                30 | 31 => {
                    let stack = std::mem::take(&mut self.stack);
                    let mut horizontal = byte == 31;
                    let count = stack.len() / 4;
                    for index in 0..count {
                        let curve = &stack[index * 4..index * 4 + 4];
                        let last = if index + 1 == count && stack.len() % 4 == 1 {
                            stack[stack.len() - 1]
                        } else {
                            0.0
                        };
                        if horizontal {
                            self.curve_to((curve[0], 0.0, curve[1], curve[2], last, curve[3]));
                        } else {
                            self.curve_to((0.0, curve[0], curve[1], curve[2], curve[3], last));
                        }
                        horizontal = !horizontal;
                    }
                }
                10 | 29 => {
                    let index = self.stack.pop()? as i32;
                    let subrs = if byte == 10 {
                        self.local
                    } else {
                        &self.font.global_subrs
                    };
                    let index = usize::try_from(index + subr_bias(subrs.len())).ok()?;
                    let (start, end) = subrs.get(index)?;
                    self.run(start, end, depth + 1)?;
                }
                11 => return Some(()),
                14 => {
                    self.take_width(true);
                    self.stack.clear();
                    self.done = true;
                }
                12 => {
                    let operator = *self.data.get(at)?;
                    at += 1;
                    let stack = std::mem::take(&mut self.stack);
                    match (operator, stack.as_slice()) {
                        (35, [a, b, c, d, e, f, g, h, i, j, k, l, _]) => {
                            self.curve_to((*a, *b, *c, *d, *e, *f));
                            self.curve_to((*g, *h, *i, *j, *k, *l));
                        }
                        (34, [a, b, c, d, e, f, g]) => {
                            self.curve_to((*a, 0.0, *b, *c, *d, 0.0));
                            self.curve_to((*e, 0.0, *f, -*c, *g, 0.0));
                        }
                        (36, [a, b, c, d, e, f, g, h, i]) => {
                            self.curve_to((*a, *b, *c, *d, *e, 0.0));
                            self.curve_to((*f, 0.0, *g, *h, *i, -(*b + *d + *h)));
                        }
                        (37, [a, b, c, d, e, f, g, h, i, j, k]) => {
                            let dx = a + c + e + g + i;
                            let dy = b + d + f + h + j;
                            self.curve_to((*a, *b, *c, *d, *e, *f));
                            if dx.abs() > dy.abs() {
                                self.curve_to((*g, *h, *i, *j, *k, -dy));
                            } else {
                                self.curve_to((*g, *h, *i, *j, -dx, *k));
                            }
                        }
                        _ => {}
                    }
                }
                _ => self.stack.clear(),
            }
        }
        Some(())
    }
}

impl CffFont {
    pub(crate) fn outline(&self, data: &[u8], glyph: u16) -> Option<Outline> {
        let (start, end) = self.charstrings.get(usize::from(glyph))?;
        let dict = self.fd_select.get(usize::from(glyph)).copied().unwrap_or(0);
        let empty = Index::default();
        let local = self.local_subrs.get(usize::from(dict)).unwrap_or(&empty);
        let mut charstring = Charstring {
            data,
            font: self,
            local,
            stack: Vec::new(),
            stems: 0,
            width_taken: false,
            point: (0.0, 0.0),
            contours: Vec::new(),
            done: false,
        };
        charstring.run(start, end, 0)?;
        let contours = charstring
            .contours
            .into_iter()
            .filter(|contour| contour.len() > 1)
            .collect();
        Some(Outline { contours })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // a CFF table holding one glyph, a square drawn with rmoveto, rlineto and endchar
    fn square_cff() -> Vec<u8> {
        let mut charstring = Vec::new();
        for value in [100, 0] {
            charstring.push((value + 139) as u8);
        }
        charstring.push(21);
        for value in [100, 0, 0, 100, -100, 0] {
            charstring.push((value + 139) as u8);
        }
        charstring.push(5);
        charstring.push(14);
        let index = |items: &[&[u8]]| {
            let mut bytes = (items.len() as u16).to_be_bytes().to_vec();
            if items.is_empty() {
                return bytes;
            }
            bytes.push(1);
            let mut offset = 1_u8;
            bytes.push(offset);
            for item in items {
                offset += item.len() as u8;
                bytes.push(offset);
            }
            for item in items {
                bytes.extend_from_slice(item);
            }
            bytes
        };
        let mut table = vec![1, 0, 4, 1];
        table.extend(index(&[b"test"]));
        // the top dict points at the charstrings, whose offset is patched in below
        let top_at = table.len();
        table.extend(index(&[&[29, 0, 0, 0, 0, 17]]));
        table.extend(index(&[]));
        table.extend(index(&[]));
        let charstrings = table.len() as i32;
        table.extend(index(&[&charstring]));
        // the dict's operand lives after the index header: count, size, two offsets
        let operand = top_at + 2 + 1 + 2 + 1;
        table[operand..operand + 4].copy_from_slice(&charstrings.to_be_bytes());
        table
    }

    #[test]
    fn runs_charstrings_into_outlines() {
        let table = square_cff();
        let font = parse(&table, 0).expect("cff should parse");
        assert_eq!(font.charstrings.len(), 1);
        let outline = font.outline(&table, 0).expect("glyph should run");
        assert_eq!(outline.contours.len(), 1);
        let points = outline.contours[0]
            .iter()
            .map(|point| (point.x, point.y))
            .collect::<Vec<_>>();
        assert_eq!(
            points,
            vec![(100.0, 0.0), (200.0, 0.0), (200.0, 100.0), (100.0, 100.0)]
        );
    }
}
