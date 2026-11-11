// opentype font variations: the design axes a face varies along (fvar, avar), how its glyph
// points move away from the default outlines (gvar) and how its advances change (HVAR)

use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Axis {
    pub(crate) tag: [u8; 4],
    pub(crate) minimum: f32,
    pub(crate) default: f32,
    pub(crate) maximum: f32,
}

#[derive(Debug)]
pub(crate) struct Variations {
    pub(crate) axes: Vec<Axis>,
    // avar's piecewise linear map of each axis's normalized coordinate, empty for none
    segments: Vec<Vec<(f32, f32)>>,
    gvar: Option<usize>,
    hvar: Option<usize>,
}

impl Variations {
    pub(crate) fn parse(data: &[u8], tables: &BTreeMap<[u8; 4], (usize, usize)>) -> Option<Self> {
        let fvar = tables.get(b"fvar")?.0;
        let axes_at = fvar + usize::from(read_u16(data, fvar + 4)?);
        let axis_count = usize::from(read_u16(data, fvar + 8)?);
        let axis_size = usize::from(read_u16(data, fvar + 10)?);
        let axes = (0..axis_count)
            .map(|index| {
                let at = axes_at + index * axis_size;
                let tag = data.get(at..at + 4)?;
                let minimum = read_fixed(data, at + 4)?;
                let default = read_fixed(data, at + 8)?;
                let maximum = read_fixed(data, at + 12)?;
                Some(Axis {
                    tag: [tag[0], tag[1], tag[2], tag[3]],
                    minimum: minimum.min(default),
                    default,
                    maximum: maximum.max(default),
                })
            })
            .collect::<Option<Vec<_>>>()?;
        if axes.is_empty() {
            return None;
        }
        let segments = tables
            .get(b"avar")
            .and_then(|avar| read_segment_maps(data, avar.0, axes.len()))
            .unwrap_or_default();
        Some(Self {
            axes,
            segments,
            gvar: tables.get(b"gvar").map(|table| table.0),
            hvar: tables.get(b"HVAR").map(|table| table.0),
        })
    }

    pub(crate) fn axis(&self, tag: &[u8; 4]) -> Option<&Axis> {
        self.axes.iter().find(|axis| &axis.tag == tag)
    }

    // user axis values, one per axis, as normalized coordinates in F2DOT14 units
    pub(crate) fn normalize(&self, values: &[f32]) -> Vec<i16> {
        self.axes
            .iter()
            .zip(values)
            .enumerate()
            .map(|(index, (axis, &value))| {
                let value = value.clamp(axis.minimum, axis.maximum);
                let normalized = if value < axis.default {
                    (value - axis.default) / (axis.default - axis.minimum)
                } else if value > axis.default {
                    (value - axis.default) / (axis.maximum - axis.default)
                } else {
                    0.0
                };
                let mapped = self
                    .segments
                    .get(index)
                    .map_or(normalized, |map| map_segment(map, normalized));
                (mapped.clamp(-1.0, 1.0) * 16384.0).round() as i16
            })
            .collect()
    }

    // how far gvar moves each of a glyph's points at normalized coordinates; points holds the
    // default positions with the four phantom points last, and contour ends are given for a
    // simple glyph, whose points a tuple leaves out are inferred from their neighbours
    pub(crate) fn glyph_deltas(
        &self,
        data: &[u8],
        glyph: u16,
        coordinates: &[f32],
        points: &[(f32, f32)],
        contour_ends: &[usize],
    ) -> Option<Vec<(f32, f32)>> {
        let gvar = self.gvar?;
        let axis_count = usize::from(read_u16(data, gvar + 4)?);
        if axis_count != self.axes.len() || coordinates.len() != axis_count {
            return None;
        }
        let shared_count = usize::from(read_u16(data, gvar + 6)?);
        let shared_at = gvar + read_u32(data, gvar + 8)? as usize;
        let glyph_count = read_u16(data, gvar + 12)?;
        let long_offsets = read_u16(data, gvar + 14)? & 1 != 0;
        let array = gvar + read_u32(data, gvar + 16)? as usize;
        let mut total = vec![(0.0, 0.0); points.len()];
        if glyph >= glyph_count {
            return Some(total);
        }
        let index = usize::from(glyph);
        let (start, end) = if long_offsets {
            (
                read_u32(data, gvar + 20 + index * 4)? as usize,
                read_u32(data, gvar + 24 + index * 4)? as usize,
            )
        } else {
            (
                usize::from(read_u16(data, gvar + 20 + index * 2)?) * 2,
                usize::from(read_u16(data, gvar + 22 + index * 2)?) * 2,
            )
        };
        if end <= start {
            return Some(total);
        }
        let at = array + start;
        let header = read_u16(data, at)?;
        let mut serialized = at + usize::from(read_u16(data, at + 2)?);
        let shared_points = if header & 0x8000 != 0 {
            let (shared, next) = read_packed_points(data, serialized)?;
            serialized = next;
            Some(shared)
        } else {
            None
        };
        let mut cursor = at + 4;
        for _ in 0..header & 0x0FFF {
            let size = usize::from(read_u16(data, cursor)?);
            let tuple = read_u16(data, cursor + 2)?;
            cursor += 4;
            let peak = if tuple & 0x8000 != 0 {
                let peak = read_tuple(data, cursor, axis_count)?;
                cursor += axis_count * 2;
                peak
            } else {
                let shared = usize::from(tuple & 0x0FFF);
                if shared >= shared_count {
                    return None;
                }
                read_tuple(data, shared_at + shared * axis_count * 2, axis_count)?
            };
            let (lower, upper) = if tuple & 0x4000 != 0 {
                let lower = read_tuple(data, cursor, axis_count)?;
                let upper = read_tuple(data, cursor + axis_count * 2, axis_count)?;
                cursor += axis_count * 4;
                (lower, upper)
            } else {
                (
                    peak.iter().map(|&peak| peak.min(0.0)).collect(),
                    peak.iter().map(|&peak| peak.max(0.0)).collect(),
                )
            };
            let tuple_data = serialized;
            serialized += size;
            let scalar = (0..axis_count)
                .map(|axis| axis_scalar(coordinates[axis], lower[axis], peak[axis], upper[axis]))
                .product::<f32>();
            if scalar == 0.0 {
                continue;
            }
            let (numbers, deltas_at) = if tuple & 0x2000 != 0 {
                read_packed_points(data, tuple_data)?
            } else {
                (shared_points.clone()?, tuple_data)
            };
            let count = numbers.as_ref().map_or(points.len(), Vec::len);
            let (x_deltas, next) = read_packed_deltas(data, deltas_at, count)?;
            let (y_deltas, _) = read_packed_deltas(data, next, count)?;
            let Some(numbers) = numbers else {
                for (sum, (x, y)) in total.iter_mut().zip(x_deltas.into_iter().zip(y_deltas)) {
                    sum.0 += scalar * x;
                    sum.1 += scalar * y;
                }
                continue;
            };
            let mut deltas = vec![(0.0, 0.0); points.len()];
            let mut touched = vec![false; points.len()];
            for (position, &number) in numbers.iter().enumerate() {
                let number = usize::from(number);
                if number < points.len() {
                    deltas[number] = (x_deltas[position], y_deltas[position]);
                    touched[number] = true;
                }
            }
            interpolate_untouched(points, contour_ends, &touched, &mut deltas);
            for (sum, delta) in total.iter_mut().zip(deltas) {
                sum.0 += scalar * delta.0;
                sum.1 += scalar * delta.1;
            }
        }
        Some(total)
    }

    // the change HVAR makes to a glyph's advance at normalized coordinates, none without HVAR
    pub(crate) fn advance_delta(
        &self,
        data: &[u8],
        glyph: u16,
        coordinates: &[f32],
    ) -> Option<f32> {
        let hvar = self.hvar?;
        let store = hvar + read_u32(data, hvar + 4)? as usize;
        let map = read_u32(data, hvar + 8)? as usize;
        let (outer, inner) = if map == 0 {
            (0, u32::from(glyph))
        } else {
            delta_set_index(data, hvar + map, u32::from(glyph))?
        };
        item_delta(data, store, outer as usize, inner as usize, coordinates)
    }
}

// how much a tuple or region applies along one axis at a normalized coordinate
fn axis_scalar(value: f32, lower: f32, peak: f32, upper: f32) -> f32 {
    if peak == 0.0 || lower > peak || peak > upper || (lower < 0.0 && upper > 0.0) || value == peak
    {
        return 1.0;
    }
    if value <= lower || value >= upper {
        0.0
    } else if value < peak {
        (value - lower) / (peak - lower)
    } else {
        (upper - value) / (upper - peak)
    }
}

fn map_segment(map: &[(f32, f32)], value: f32) -> f32 {
    let (Some(&first), Some(&last)) = (map.first(), map.last()) else {
        return value;
    };
    if value <= first.0 {
        return value + first.1 - first.0;
    }
    if value >= last.0 {
        return value + last.1 - last.0;
    }
    map.windows(2)
        .find(|pair| value <= pair[1].0)
        .map_or(value, |pair| {
            let (from, to) = (pair[0], pair[1]);
            from.1 + (value - from.0) / (to.0 - from.0) * (to.1 - from.1)
        })
}

fn read_segment_maps(data: &[u8], avar: usize, axis_count: usize) -> Option<Vec<Vec<(f32, f32)>>> {
    if usize::from(read_u16(data, avar + 6)?) != axis_count {
        return None;
    }
    let mut cursor = avar + 8;
    (0..axis_count)
        .map(|_| {
            let count = usize::from(read_u16(data, cursor)?);
            let map = (0..count)
                .map(|index| {
                    let at = cursor + 2 + index * 4;
                    Some((read_f2dot14(data, at)?, read_f2dot14(data, at + 2)?))
                })
                .collect::<Option<Vec<_>>>()?;
            cursor += 2 + count * 4;
            Some(map)
        })
        .collect()
}

// a packed list of point numbers and where the data after it starts; none means every point
fn read_packed_points(data: &[u8], at: usize) -> Option<(Option<Vec<u16>>, usize)> {
    let first = *data.get(at)?;
    let mut cursor = at + 1;
    if first == 0 {
        return Some((None, cursor));
    }
    let count = if first & 0x80 != 0 {
        cursor += 1;
        usize::from(first & 0x7F) << 8 | usize::from(*data.get(at + 1)?)
    } else {
        usize::from(first)
    };
    let mut numbers = Vec::with_capacity(count);
    let mut number = 0_u16;
    while numbers.len() < count {
        let control = *data.get(cursor)?;
        cursor += 1;
        for _ in 0..=control & 0x7F {
            let step = if control & 0x80 != 0 {
                cursor += 2;
                read_u16(data, cursor - 2)?
            } else {
                cursor += 1;
                u16::from(*data.get(cursor - 1)?)
            };
            number = number.wrapping_add(step);
            numbers.push(number);
        }
    }
    numbers.truncate(count);
    Some((Some(numbers), cursor))
}

// a run-length packed list of deltas and where the data after it starts
fn read_packed_deltas(data: &[u8], at: usize, count: usize) -> Option<(Vec<f32>, usize)> {
    let mut deltas = Vec::with_capacity(count);
    let mut cursor = at;
    while deltas.len() < count {
        let control = *data.get(cursor)?;
        cursor += 1;
        let run = usize::from(control & 0x3F) + 1;
        for _ in 0..run {
            let delta = match control & 0xC0 {
                0x80 => 0.0,
                0x40 => {
                    cursor += 2;
                    f32::from(read_u16(data, cursor - 2)? as i16)
                }
                0xC0 => {
                    cursor += 4;
                    read_u32(data, cursor - 4)? as i32 as f32
                }
                _ => {
                    cursor += 1;
                    f32::from(*data.get(cursor - 1)? as i8)
                }
            };
            deltas.push(delta);
        }
    }
    deltas.truncate(count);
    Some((deltas, cursor))
}

// a contour's points a tuple leaves out move with the touched points before and after them,
// interpolated by where they sit between the two on each axis
fn interpolate_untouched(
    points: &[(f32, f32)],
    contour_ends: &[usize],
    touched: &[bool],
    deltas: &mut [(f32, f32)],
) {
    let mut first = 0;
    for &last in contour_ends {
        if last < first || last >= points.len() {
            return;
        }
        let references = (first..=last)
            .filter(|&point| touched[point])
            .collect::<Vec<_>>();
        for (index, &from) in references.iter().enumerate() {
            let to = references[(index + 1) % references.len()];
            let mut point = from;
            loop {
                point = if point == last { first } else { point + 1 };
                if point == to {
                    break;
                }
                deltas[point] = (
                    infer(
                        points[point].0,
                        (points[from].0, deltas[from].0),
                        (points[to].0, deltas[to].0),
                    ),
                    infer(
                        points[point].1,
                        (points[from].1, deltas[from].1),
                        (points[to].1, deltas[to].1),
                    ),
                );
            }
        }
        first = last + 1;
    }
}

// one coordinate's delta from two reference points' positions and deltas
fn infer(value: f32, first: (f32, f32), second: (f32, f32)) -> f32 {
    if first.0 == second.0 {
        return if first.1 == second.1 { first.1 } else { 0.0 };
    }
    let (low, high) = if first.0 < second.0 {
        (first, second)
    } else {
        (second, first)
    };
    if value <= low.0 {
        low.1
    } else if value >= high.0 {
        high.1
    } else {
        low.1 + (value - low.0) / (high.0 - low.0) * (high.1 - low.1)
    }
}

// the outer and inner index a delta-set index map gives an item
fn delta_set_index(data: &[u8], at: usize, item: u32) -> Option<(u32, u32)> {
    let format = *data.get(at)?;
    let entry_format = *data.get(at + 1)?;
    let (count, entries) = if format == 0 {
        (u32::from(read_u16(data, at + 2)?), at + 4)
    } else {
        (read_u32(data, at + 2)?, at + 6)
    };
    if count == 0 {
        return None;
    }
    let size = usize::from(entry_format >> 4 & 0x03) + 1;
    let inner_bits = u32::from(entry_format & 0x0F) + 1;
    let start = entries + item.min(count - 1) as usize * size;
    let entry = data
        .get(start..start + size)?
        .iter()
        .fold(0_u32, |entry, &byte| entry << 8 | u32::from(byte));
    Some((entry >> inner_bits, entry & ((1 << inner_bits) - 1)))
}

// an item variation store's delta for one item, summed over the regions it varies in
fn item_delta(
    data: &[u8],
    store: usize,
    outer: usize,
    inner: usize,
    coordinates: &[f32],
) -> Option<f32> {
    let regions = store + read_u32(data, store + 2)? as usize;
    if outer >= usize::from(read_u16(data, store + 6)?) {
        return None;
    }
    let item = store + read_u32(data, store + 8 + outer * 4)? as usize;
    if inner >= usize::from(read_u16(data, item)?) {
        return None;
    }
    let word_field = read_u16(data, item + 2)?;
    let region_count = usize::from(read_u16(data, item + 4)?);
    let long = word_field & 0x8000 != 0;
    let words = usize::from(word_field & 0x7FFF).min(region_count);
    let (word_size, short_size) = if long { (4, 2) } else { (2, 1) };
    let row = item
        + 6
        + region_count * 2
        + inner * (words * word_size + (region_count - words) * short_size);
    let axis_count = usize::from(read_u16(data, regions)?);
    let region_total = usize::from(read_u16(data, regions + 2)?);
    let mut delta = 0.0;
    for column in 0..region_count {
        let region = usize::from(read_u16(data, item + 6 + column * 2)?);
        if region >= region_total {
            return None;
        }
        let at = regions + 4 + region * axis_count * 6;
        let scalar = (0..axis_count.min(coordinates.len()))
            .map(|axis| {
                let axis_at = at + axis * 6;
                Some(axis_scalar(
                    coordinates[axis],
                    read_f2dot14(data, axis_at)?,
                    read_f2dot14(data, axis_at + 2)?,
                    read_f2dot14(data, axis_at + 4)?,
                ))
            })
            .product::<Option<f32>>()?;
        if scalar == 0.0 {
            continue;
        }
        let value = if column < words {
            if long {
                read_u32(data, row + column * 4)? as i32 as f32
            } else {
                f32::from(read_u16(data, row + column * 2)? as i16)
            }
        } else {
            let at = row + words * word_size + (column - words) * short_size;
            if long {
                f32::from(read_u16(data, at)? as i16)
            } else {
                f32::from(*data.get(at)? as i8)
            }
        };
        delta += scalar * value;
    }
    Some(delta)
}

fn read_tuple(data: &[u8], at: usize, axis_count: usize) -> Option<Vec<f32>> {
    (0..axis_count)
        .map(|axis| read_f2dot14(data, at + axis * 2))
        .collect()
}

fn read_u16(data: &[u8], at: usize) -> Option<u16> {
    data.get(at..at + 2)
        .map(|bytes| u16::from_be_bytes([bytes[0], bytes[1]]))
}

fn read_u32(data: &[u8], at: usize) -> Option<u32> {
    data.get(at..at + 4)
        .map(|bytes| u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn read_f2dot14(data: &[u8], at: usize) -> Option<f32> {
    read_u16(data, at).map(|value| f32::from(value as i16) / 16384.0)
}

fn read_fixed(data: &[u8], at: usize) -> Option<f32> {
    read_u32(data, at).map(|value| value as i32 as f32 / 65536.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packed_point_numbers_accumulate_across_byte_and_word_runs() {
        assert_eq!(read_packed_points(&[0, 9], 0), Some((None, 1)));
        // two byte steps then one word step: 3, 4, then 4 + 300
        let data = [3, 0x01, 3, 1, 0x80, 0x01, 0x2C, 9];
        assert_eq!(
            read_packed_points(&data, 0),
            Some((Some(vec![3, 4, 304]), 7))
        );
        // a count of one written in two bytes
        let long = [0x80, 0x01, 0x00, 7];
        assert_eq!(read_packed_points(&long, 0), Some((Some(vec![7]), 4)));
        assert_eq!(read_packed_points(&[2, 0x01, 1], 0), None);
    }

    #[test]
    fn packed_deltas_read_zero_byte_word_and_long_runs() {
        let data = [
            0x81, 0x01, 0xFF, 0x05, 0x40, 0x01, 0x00, 0xC0, 0xFF, 0xFF, 0xFF, 0xFE, 0x77,
        ];
        assert_eq!(
            read_packed_deltas(&data, 0, 6),
            Some((vec![0.0, 0.0, -1.0, 5.0, 256.0, -2.0], 12))
        );
        assert_eq!(read_packed_deltas(&[0x02, 1], 0, 3), None);
    }

    #[test]
    fn untouched_points_follow_their_touched_neighbours_per_contour() {
        // a square from 0 to 100, then a second contour of two points
        let points = [
            (0.0, 0.0),
            (100.0, 0.0),
            (100.0, 100.0),
            (0.0, 100.0),
            (10.0, 10.0),
            (20.0, 20.0),
        ];
        let touched = [true, false, true, false, false, true];
        let mut deltas = [(0.0, 0.0); 6];
        deltas[2] = (20.0, 10.0);
        deltas[5] = (4.0, -4.0);
        interpolate_untouched(&points, &[3, 5], &touched, &mut deltas);
        // point 1 sits at the far x of point 2 and the near y of point 0; point 3 the reverse
        assert_eq!(deltas[1], (20.0, 0.0));
        assert_eq!(deltas[3], (0.0, 10.0));
        // a contour with one touched point moves as a whole
        assert_eq!(deltas[4], (4.0, -4.0));
        // between two references on one coordinate, the delta is proportional
        assert_eq!(infer(25.0, (0.0, 0.0), (100.0, 8.0)), 2.0);
        assert_eq!(infer(5.0, (5.0, 3.0), (5.0, 3.0)), 3.0);
        assert_eq!(infer(5.0, (5.0, 3.0), (5.0, 4.0)), 0.0);
    }

    #[test]
    fn avar_remaps_normalized_coordinates_and_regions_scale_tuples() {
        let variations = Variations {
            axes: vec![Axis {
                tag: *b"wght",
                minimum: 200.0,
                default: 400.0,
                maximum: 800.0,
            }],
            segments: vec![vec![(-1.0, -1.0), (0.0, 0.0), (0.5, 0.25), (1.0, 1.0)]],
            gvar: None,
            hvar: None,
        };
        // 300 is halfway below the default; 500 is a quarter up, which avar maps to an eighth
        assert_eq!(variations.normalize(&[300.0]), vec![-8192]);
        assert_eq!(variations.normalize(&[500.0]), vec![2048]);
        assert_eq!(variations.normalize(&[700.0]), vec![10240]);
        assert_eq!(variations.normalize(&[2000.0]), vec![16384]);
        assert_eq!(axis_scalar(0.5, 0.0, 1.0, 1.0), 0.5);
        assert_eq!(axis_scalar(-0.5, -1.0, 0.0, 0.0), 1.0);
        // an intermediate region rises from 0.2 to its peak at 0.4 and falls to zero at 0.8
        assert!((axis_scalar(0.3, 0.2, 0.4, 0.8) - 0.5).abs() < 1e-6);
        assert!((axis_scalar(0.6, 0.2, 0.4, 0.8) - 0.5).abs() < 1e-6);
        assert_eq!(axis_scalar(0.9, 0.2, 0.4, 0.8), 0.0);
        assert_eq!(axis_scalar(-0.1, 0.0, 0.4, 0.8), 0.0);
    }

    #[test]
    fn delta_set_index_maps_split_entries_into_outer_and_inner() {
        // format 0, two-byte entries with 4 inner bits, two entries
        let map = [0, 0x13, 0, 2, 0x00, 0x25, 0x01, 0x37];
        assert_eq!(delta_set_index(&map, 0, 0), Some((2, 5)));
        assert_eq!(delta_set_index(&map, 0, 1), Some((0x13, 7)));
        // items past the end repeat the last entry
        assert_eq!(delta_set_index(&map, 0, 9), Some((0x13, 7)));
    }
}
