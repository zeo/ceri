// freetype's latin auto-hinter in light mode, as skia runs it for slight hinting on linux: only
// the vertical axis is fitted, so stems keep their widths while the baseline, the x-height and
// the cap height land on whole pixels. the integer steps follow aflatin.c and afhints.c from
// freetype 2.13 so the fitted outlines match its own

use crate::font::{FontFace, Outline, OutlinePoint};

const DIR_NONE: i8 = 4;
const DIR_RIGHT: i8 = 1;
const DIR_LEFT: i8 = -1;
const DIR_UP: i8 = 2;
const DIR_DOWN: i8 = -2;

const FLAG_CONTROL: u8 = 1;
const FLAG_TOUCH: u8 = 2;
const FLAG_WEAK: u8 = 4;

const EDGE_ROUND: u8 = 1;
const EDGE_DONE: u8 = 2;

const MAX_WIDTHS: usize = 16;

// the latin blue strings from afblue.dat, with whether each zone is a top zone and whether it
// is the x-height the vertical scale is fitted to
const BLUES: [(&str, bool, bool); 6] = [
    ("THEZOCQS", true, false),
    ("HEZLOCUS", false, false),
    ("fijkdbh", true, false),
    ("uvxzoesc", true, true),
    ("nrxzoesc", false, false),
    ("pqgjy", false, false),
];

// the characters freetype hints as latin, from afranges.c
const LATIN: [(u32, u32); 31] = [
    (0x20, 0x7F),
    (0xA0, 0xA9),
    (0xAB, 0xB1),
    (0xB4, 0xB8),
    (0xBB, 0xFF),
    (0x100, 0x24F),
    (0x250, 0x2AF),
    (0x2B9, 0x2DF),
    (0x2E5, 0x2FF),
    (0x300, 0x36F),
    (0x1AB0, 0x1ABE),
    (0x1D00, 0x1D2B),
    (0x1D6B, 0x1D77),
    (0x1D79, 0x1D7F),
    (0x1D80, 0x1D9A),
    (0x1DC0, 0x1DFF),
    (0x1E00, 0x1EFF),
    (0x2000, 0x206F),
    (0x20A0, 0x20B8),
    (0x20BA, 0x20CF),
    (0x2150, 0x218F),
    (0x2C60, 0x2C7B),
    (0x2C7E, 0x2C7F),
    (0x2E00, 0x2E7F),
    (0xA720, 0xA76F),
    (0xA771, 0xA7F7),
    (0xA7FA, 0xA7FF),
    (0xAB30, 0xAB5B),
    (0xAB60, 0xAB6F),
    (0xFB00, 0xFB06),
    (0x1D400, 0x1D7FF),
];

// the greek and cyrillic characters, whose styles come before latin's and claim any glyph
// they share with it
const EARLIER: [(u32, u32); 7] = [
    (0x370, 0x3FF),
    (0x400, 0x4FF),
    (0x500, 0x52F),
    (0x1C80, 0x1C8F),
    (0x1F00, 0x1FFF),
    (0x2DE0, 0x2DFF),
    (0xA640, 0xA69F),
];

// latin characters freetype marks as non-base, which skip the blue zones
const NONBASE: [(u32, u32); 16] = [
    (0x5E, 0x60),
    (0x7E, 0x7E),
    (0xA8, 0xA9),
    (0xAE, 0xB0),
    (0xB4, 0xB4),
    (0xB8, 0xB8),
    (0xBC, 0xBE),
    (0x2B9, 0x2DF),
    (0x2E5, 0x2FF),
    (0x300, 0x36F),
    (0x1AB0, 0x1ABE),
    (0x1DC0, 0x1DFF),
    (0x2017, 0x2017),
    (0x203E, 0x203E),
    (0xA788, 0xA788),
    (0xA7F8, 0xA7FA),
];

fn mul_fix(a: i64, b: i64) -> i64 {
    let product = a * b;
    (product + 0x8000 - i64::from(product < 0)) >> 16
}

fn mul_div(a: i64, b: i64, c: i64) -> i64 {
    let negative = (a < 0) ^ (b < 0) ^ (c < 0);
    let (a, b, c) = (a.unsigned_abs(), b.unsigned_abs(), c.unsigned_abs());
    let d = (a * b + (c >> 1))
        .checked_div(c)
        .map_or(0x7FFF_FFFF, |d| d as i64);
    if negative { -d } else { d }
}

fn div_fix(a: i64, b: i64) -> i64 {
    let negative = (a < 0) ^ (b < 0);
    let (a, b) = (a.unsigned_abs(), b.unsigned_abs());
    let q = ((a << 16) + (b >> 1))
        .checked_div(b)
        .map_or(0x7FFF_FFFF, |q| q as i64);
    if negative { -q } else { q }
}

fn pix_round(x: i64) -> i64 {
    (x + 32) & !63
}

fn hypot(x: i64, y: i64) -> i64 {
    let (x, y) = (x.abs(), y.abs());
    if x > y {
        x + ((3 * y) >> 3)
    } else {
        y + ((3 * x) >> 3)
    }
}

fn corner_is_flat(in_x: i64, in_y: i64, out_x: i64, out_y: i64) -> bool {
    let d_in = hypot(in_x, in_y);
    let d_out = hypot(out_x, out_y);
    let d_hypot = hypot(in_x + out_x, in_y + out_y);
    d_in + d_out - d_hypot < d_hypot >> 4
}

fn direction(dx: i64, dy: i64) -> i8 {
    let (dir, long, short) = if dy >= dx {
        if dy >= -dx {
            (DIR_UP, dy, dx)
        } else {
            (DIR_LEFT, -dx, dy)
        }
    } else if dy >= -dx {
        (DIR_RIGHT, dx, dy)
    } else {
        (DIR_DOWN, -dy, dx)
    };
    if long <= 14 * short.abs() {
        DIR_NONE
    } else {
        dir
    }
}

#[derive(Clone, Copy, Default)]
struct Point {
    fx: i64,
    fy: i64,
    oy: i64,
    y: i64,
    flags: u8,
    in_dir: i8,
    out_dir: i8,
    prev: usize,
    next: usize,
}

struct Glyph {
    points: Vec<Point>,
    contours: Vec<(usize, usize)>,
    // the major direction of horizontal segments, left for truetype winding
    major_dir: i8,
}

// the points of an outline in font units, linked into contours and marked weak or strong for
// the vertical axis, as af_glyph_hints_reload leaves them
fn load_glyph(outline: &Outline, units_per_em: i64, y_scale: i64) -> Glyph {
    let mut points = Vec::new();
    let mut contours = Vec::new();
    for contour in &outline.contours {
        if contour.is_empty() {
            continue;
        }
        let start = points.len();
        let end = start + contour.len() - 1;
        for (index, point) in contour.iter().enumerate() {
            let fy = point.y.round() as i64;
            points.push(Point {
                fx: point.x.round() as i64,
                fy,
                oy: mul_fix(fy, y_scale),
                y: mul_fix(fy, y_scale),
                flags: if point.on_curve { 0 } else { FLAG_CONTROL },
                in_dir: DIR_NONE,
                out_dir: DIR_NONE,
                prev: if index == 0 { end } else { start + index - 1 },
                next: if start + index == end {
                    start
                } else {
                    start + index + 1
                },
            });
        }
        contours.push((start, end));
    }
    let major_dir = if is_postscript(&points, &contours) {
        DIR_RIGHT
    } else {
        DIR_LEFT
    };
    let near_limit = 20 * units_per_em / 2048;
    let near_limit2 = 2 * near_limit - 1;
    // u and v link each point to the next and previous points that are far enough away
    let mut forward = (0..points.len()).collect::<Vec<_>>();
    let mut backward = forward.clone();
    for &(start, _) in &contours {
        let mut point = start;
        let mut prev = points[start].prev;
        while prev != start {
            let out_x = points[point].fx - points[prev].fx;
            let out_y = points[point].fy - points[prev].fy;
            if out_x.abs() + out_y.abs() >= near_limit2 {
                break;
            }
            point = prev;
            prev = points[prev].prev;
        }
        let first = point;
        let mut curr = first;
        forward[curr] = first;
        backward[first] = curr;
        let (mut out_x, mut out_y) = (0, 0);
        let mut next = first;
        loop {
            let point = next;
            next = points[point].next;
            out_x += points[next].fx - points[point].fx;
            out_y += points[next].fy - points[point].fy;
            if out_x.abs() + out_y.abs() < near_limit {
                points[next].flags |= FLAG_WEAK;
            } else {
                forward[curr] = next;
                backward[next] = curr;
                let out_dir = direction(out_x, out_y);
                points[curr].out_dir = out_dir;
                curr = points[curr].next;
                while curr != next {
                    points[curr].in_dir = out_dir;
                    points[curr].out_dir = out_dir;
                    curr = points[curr].next;
                }
                points[next].in_dir = out_dir;
                forward[curr] = first;
                backward[first] = curr;
                out_x = 0;
                out_y = 0;
            }
            if next == first {
                break;
            }
        }
    }
    for index in 0..points.len() {
        let point = points[index];
        if point.flags & FLAG_WEAK != 0 || point.in_dir != DIR_NONE || point.out_dir != DIR_NONE {
            continue;
        }
        let (next, prev) = (forward[index], backward[index]);
        let in_x = point.fx - points[prev].fx;
        let in_y = point.fy - points[prev].fy;
        let out_x = points[next].fx - point.fx;
        let out_y = points[next].fy - point.fy;
        if (in_x ^ out_x) >= 0 && (in_y ^ out_y) >= 0 {
            points[index].flags |= FLAG_WEAK;
            forward[prev] = next;
            backward[next] = prev;
        }
    }
    for index in 0..points.len() {
        let point = points[index];
        if point.flags & FLAG_WEAK != 0 {
            continue;
        }
        let weak = if point.flags & FLAG_CONTROL != 0 {
            true
        } else if point.out_dir == point.in_dir {
            if point.out_dir != DIR_NONE {
                true
            } else {
                let (next, prev) = (forward[index], backward[index]);
                let flat = corner_is_flat(
                    point.fx - points[prev].fx,
                    point.fy - points[prev].fy,
                    points[next].fx - point.fx,
                    points[next].fy - point.fy,
                );
                if flat {
                    forward[prev] = next;
                    backward[next] = prev;
                }
                flat
            }
        } else {
            point.in_dir == -point.out_dir
        };
        if weak {
            points[index].flags |= FLAG_WEAK;
        }
    }
    Glyph {
        points,
        contours,
        major_dir,
    }
}

// FT_Outline_Get_Orientation, the winding of the control polygon
fn is_postscript(points: &[Point], contours: &[(usize, usize)]) -> bool {
    let Some(first) = points.first() else {
        return false;
    };
    let (mut x_min, mut x_max, mut y_min, mut y_max) = (first.fx, first.fx, first.fy, first.fy);
    for point in points {
        x_min = x_min.min(point.fx);
        x_max = x_max.max(point.fx);
        y_min = y_min.min(point.fy);
        y_max = y_max.max(point.fy);
    }
    if x_min == x_max || y_min == y_max {
        return false;
    }
    let msb = |value: i64| 63 - (value as u64).leading_zeros() as i64;
    let x_shift = (msb(x_max.abs() | x_min.abs()) - 14).max(0);
    let y_shift = (msb(y_max - y_min) - 14).max(0);
    let mut area = 0;
    for &(start, end) in contours {
        let mut prev = (points[end].fx >> x_shift, points[end].fy >> y_shift);
        for point in &points[start..=end] {
            let cur = (point.fx >> x_shift, point.fy >> y_shift);
            area += (cur.1 - prev.1) * (cur.0 + prev.0);
            prev = cur;
        }
    }
    area > 0
}

#[derive(Clone, Copy)]
struct Segment {
    dir: i8,
    round: bool,
    pos: i64,
    delta: i64,
    min_coord: i64,
    max_coord: i64,
    height: i64,
    score: i64,
    link: Option<usize>,
    serif: Option<usize>,
    edge: Option<usize>,
    edge_next: usize,
    first: usize,
    last: usize,
}

// runs of points heading left or right, af_latin_hints_compute_segments for the vertical axis
fn compute_segments(glyph: &Glyph, flat_threshold: i64) -> Vec<Segment> {
    let points = &glyph.points;
    let major_dir = glyph.major_dir.abs();
    let control = |index: usize| points[index].flags & FLAG_CONTROL != 0;
    let mut segments: Vec<Segment> = Vec::new();
    for &(start, _) in &glyph.contours {
        let mut point = start;
        let mut last = points[point].prev;
        let mut on_edge = false;
        let mut segment_dir = major_dir;
        let mut current = (32000, -32000, 32000, -32000, false, false, 32000, -32000);
        let mut previous = current;
        let mut prev_segment: Option<usize> = None;
        if points[last].out_dir.abs() == major_dir && points[point].out_dir.abs() == major_dir {
            last = point;
            loop {
                point = points[point].prev;
                if points[point].out_dir.abs() != major_dir {
                    point = points[point].next;
                    break;
                }
                if point == last {
                    break;
                }
            }
        }
        last = point;
        let mut passed = false;
        loop {
            if on_edge {
                // min and max pos, min and max coord, whether each extreme is a control
                // point, and the extremes of the on-curve points
                let (u, v) = (points[point].fy, points[point].fx);
                let state = &mut current;
                state.0 = state.0.min(u);
                state.1 = state.1.max(u);
                if v < state.2 {
                    state.2 = v;
                    state.4 = control(point);
                }
                if v > state.3 {
                    state.3 = v;
                    state.5 = control(point);
                }
                if !control(point) {
                    state.6 = state.6.min(v);
                    state.7 = state.7.max(v);
                }
                if points[point].out_dir != segment_dir || point == last {
                    let index = segments.len() - 1;
                    let finish = |segment: &mut Segment, state: &(i64, i64, i64, i64, bool, bool, i64, i64)| {
                        segment.last = point;
                        segment.pos = (state.0 + state.1) >> 1;
                        segment.delta = (state.1 - state.0) >> 1;
                        if (state.4 || state.5) && state.7 - state.6 < flat_threshold {
                            segment.round = true;
                        }
                        segment.min_coord = state.2;
                        segment.max_coord = state.3;
                        segment.height = state.3 - state.2;
                    };
                    match prev_segment {
                        Some(prev) if segments[index].first == segments[prev].last => {
                            if points[segments[prev].last].in_dir == points[point].in_dir {
                                let state = &mut current;
                                state.0 = state.0.min(previous.0);
                                state.1 = state.1.max(previous.1);
                                if previous.2 < state.2 {
                                    state.2 = previous.2;
                                    state.4 = previous.4;
                                }
                                if previous.3 > state.3 {
                                    state.3 = previous.3;
                                    state.5 = previous.5;
                                }
                                state.6 = state.6.min(previous.6);
                                state.7 = state.7.max(previous.7);
                                let segment = &mut segments[prev];
                                segment.round = false;
                                finish(segment, &current);
                            } else if (previous.3 - previous.2).abs()
                                > (current.3 - current.2).abs()
                            {
                                previous.0 = previous.0.min(current.0);
                                previous.1 = previous.1.max(current.1);
                                let segment = &mut segments[prev];
                                segment.last = point;
                                segment.pos = (previous.0 + previous.1) >> 1;
                                segment.delta = (previous.1 - previous.0) >> 1;
                            } else {
                                current.0 = current.0.min(previous.0);
                                current.1 = current.1.max(previous.1);
                                finish(&mut segments[index], &current);
                                segments[prev] = segments[index];
                                previous = current;
                            }
                            segments.pop();
                        }
                        _ => {
                            finish(&mut segments[index], &current);
                            prev_segment = Some(index);
                            previous = current;
                        }
                    }
                    on_edge = false;
                }
            }
            if point == last {
                if passed {
                    break;
                }
                passed = true;
            }
            if !on_edge && (points[point].out_dir.abs() == major_dir || point == points[point].prev)
            {
                if segments.len() > 1000 {
                    return Vec::new();
                }
                segment_dir = points[point].out_dir;
                segments.push(Segment {
                    dir: segment_dir,
                    round: false,
                    pos: 0,
                    delta: 0,
                    min_coord: 0,
                    max_coord: 0,
                    height: 0,
                    score: 32000,
                    link: None,
                    serif: None,
                    edge: None,
                    edge_next: 0,
                    first: point,
                    last: point,
                });
                let index = segments.len() - 1;
                if prev_segment.is_some() {
                    prev_segment = Some(index - 1);
                }
                let (u, v) = (points[point].fy, points[point].fx);
                let (on_min, on_max) = if control(point) {
                    (32000, -32000)
                } else {
                    (v, v)
                };
                current = (u, u, v, v, control(point), control(point), on_min, on_max);
                on_edge = true;
                if point == points[point].prev {
                    let segment = &mut segments[index];
                    segment.pos = u;
                    segment.round = control(point);
                    segment.min_coord = v;
                    segment.max_coord = v;
                    on_edge = false;
                }
            }
            point = points[point].next;
        }
    }
    for segment in &mut segments {
        let (first, last) = (points[segment.first], points[segment.last]);
        let (before, after) = (points[first.prev].fx, points[last.next].fx);
        if first.fx < last.fx {
            if before < first.fx {
                segment.height += (first.fx - before) >> 1;
            }
            if after > last.fx {
                segment.height += (after - last.fx) >> 1;
            }
        } else {
            if before > first.fx {
                segment.height += (before - first.fx) >> 1;
            }
            if after < last.fx {
                segment.height += (last.fx - after) >> 1;
            }
        }
    }
    segments
}

// pairs opposite segments into stems, af_latin_hints_link_segments
fn link_segments(segments: &mut [Segment], widths: &[i64], major_dir: i8, units_per_em: i64) {
    let max_width = widths.last().copied().unwrap_or(0);
    let len_threshold = (8 * units_per_em / 2048).max(1);
    let len_score = 6000 * units_per_em / 2048;
    for one in 0..segments.len() {
        if segments[one].dir != major_dir {
            continue;
        }
        for two in 0..segments.len() {
            let (pos1, pos2) = (segments[one].pos, segments[two].pos);
            if i32::from(segments[one].dir) + i32::from(segments[two].dir) != 0 || pos2 <= pos1 {
                continue;
            }
            let min = segments[one].min_coord.max(segments[two].min_coord);
            let max = segments[one].max_coord.min(segments[two].max_coord);
            let len = max - min;
            if len < len_threshold {
                continue;
            }
            let dist = pos2 - pos1;
            let demerit = if max_width != 0 {
                let delta = (dist << 10) / max_width - (1 << 10);
                if delta > 10000 {
                    32000
                } else if delta > 0 {
                    delta * delta / 3000
                } else {
                    0
                }
            } else {
                dist
            };
            let score = demerit + len_score / len;
            if score < segments[one].score {
                segments[one].score = score;
                segments[one].link = Some(two);
            }
            if score < segments[two].score {
                segments[two].score = score;
                segments[two].link = Some(one);
            }
        }
    }
    for one in 0..segments.len() {
        if let Some(two) = segments[one].link
            && segments[two].link != Some(one)
        {
            segments[one].link = None;
            segments[one].serif = segments[two].link;
        }
    }
}

#[derive(Clone, Copy)]
struct Edge {
    fpos: i64,
    opos: i64,
    pos: i64,
    flags: u8,
    dir: i8,
    scale: i64,
    blue: Option<i64>,
    link: Option<usize>,
    serif: Option<usize>,
    first: usize,
    last: usize,
}

// segments at nearly the same height grouped into edges, af_latin_hints_compute_edges
fn compute_edges(
    segments: &mut [Segment],
    major_dir: i8,
    scale: i64,
    edge_distance_threshold: i64,
) -> Vec<Edge> {
    let mut edges: Vec<Edge> = Vec::new();
    let width_threshold = div_fix(32, scale);
    let distance_threshold = div_fix(mul_fix(edge_distance_threshold, scale).min(16), scale);
    for index in 0..segments.len() {
        let segment = segments[index];
        if segment.height < 0 || segment.delta > width_threshold || segment.dir == DIR_NONE {
            continue;
        }
        if segment.serif.is_some() && 2 * segment.height < 0 {
            continue;
        }
        let found = edges.iter().position(|edge| {
            (segment.pos - edge.fpos).abs() < distance_threshold && edge.dir == segment.dir
        });
        match found {
            Some(found) => join_edge(segments, &mut edges[found], index),
            None => {
                let mut at = edges.len();
                while at > 0 {
                    if edges[at - 1].fpos < segment.pos
                        || (edges[at - 1].fpos == segment.pos && segment.dir == major_dir)
                    {
                        break;
                    }
                    at -= 1;
                }
                edges.insert(
                    at,
                    Edge {
                        fpos: segment.pos,
                        opos: mul_fix(segment.pos, scale),
                        pos: mul_fix(segment.pos, scale),
                        flags: 0,
                        dir: segment.dir,
                        scale: 0,
                        blue: None,
                        link: None,
                        serif: None,
                        first: index,
                        last: index,
                    },
                );
                segments[index].edge_next = index;
            }
        }
    }
    for index in 0..segments.len() {
        let segment = segments[index];
        if segment.dir != DIR_NONE {
            continue;
        }
        if let Some(found) = edges
            .iter()
            .position(|edge| (segment.pos - edge.fpos).abs() < distance_threshold)
        {
            join_edge(segments, &mut edges[found], index);
        }
    }
    for (index, edge) in edges.iter().enumerate() {
        let mut segment = edge.first;
        loop {
            segments[segment].edge = Some(index);
            segment = segments[segment].edge_next;
            if segment == edge.first {
                break;
            }
        }
    }
    for index in 0..edges.len() {
        let (mut round, mut straight) = (0, 0);
        let mut segment = edges[index].first;
        loop {
            let seg = segments[segment];
            if seg.round {
                round += 1;
            } else {
                straight += 1;
            }
            let serif = seg
                .serif
                .filter(|&serif| segments[serif].edge.is_some_and(|edge| edge != index));
            let linked = seg.link.filter(|&link| segments[link].edge.is_some());
            if let Some(other) = serif.or(linked) {
                let mut edge2 = if serif.is_some() {
                    edges[index].serif
                } else {
                    edges[index].link
                };
                match edge2 {
                    Some(known) => {
                        let edge_delta = (edges[index].fpos - edges[known].fpos).abs();
                        let seg_delta = (seg.pos - segments[other].pos).abs();
                        if seg_delta < edge_delta {
                            edge2 = segments[other].edge;
                        }
                    }
                    None => edge2 = segments[other].edge,
                }
                if serif.is_some() {
                    edges[index].serif = edge2;
                } else {
                    edges[index].link = edge2;
                }
            }
            segment = seg.edge_next;
            if segment == edges[index].first {
                break;
            }
        }
        edges[index].flags = if round > 0 && round >= straight {
            EDGE_ROUND
        } else {
            0
        };
        if edges[index].link.is_some() {
            edges[index].serif = None;
        }
    }
    edges
}

fn join_edge(segments: &mut [Segment], edge: &mut Edge, segment: usize) {
    segments[segment].edge_next = edge.first;
    segments[edge.last].edge_next = segment;
    edge.last = segment;
}

// af_latin_hint_edges for the vertical axis in light mode, where stems keep their widths
fn hint_edges(edges: &mut [Edge]) {
    let mut anchor: Option<usize> = None;
    let mut has_serifs = false;
    let align_linked = |edges: &mut [Edge], base: usize, stem: usize| {
        edges[stem].pos = edges[base].pos + (edges[stem].opos - edges[base].opos);
    };
    // a stem under a pixel and a half centres on a pixel centre or edge, whichever is nearer
    let centre = |center: i64, len: i64| {
        let (up, down) = if len <= 64 { (32, 32) } else { (38, 26) };
        let pos = pix_round(center);
        if (center - (pos - up)).abs() < (center - (pos + down)).abs() {
            pos - up
        } else {
            pos + down
        }
    };
    for index in 0..edges.len() {
        if edges[index].flags & EDGE_DONE != 0 {
            continue;
        }
        let link = edges[index].link;
        let (edge1, edge2) = match (edges[index].blue, link) {
            (Some(_), _) => (index, link),
            (None, Some(link)) if edges[link].blue.is_some() => (link, Some(index)),
            _ => continue,
        };
        edges[edge1].pos = edges[edge1].blue.unwrap_or_default();
        edges[edge1].flags |= EDGE_DONE;
        if let Some(edge2) = edge2
            && edges[edge2].blue.is_none()
        {
            align_linked(edges, edge1, edge2);
            edges[edge2].flags |= EDGE_DONE;
        }
        anchor.get_or_insert(index);
    }
    for index in 0..edges.len() {
        if edges[index].flags & EDGE_DONE != 0 {
            continue;
        }
        let Some(edge2) = edges[index].link else {
            has_serifs = true;
            continue;
        };
        if edges[edge2].blue.is_some() {
            align_linked(edges, edge2, index);
            edges[index].flags |= EDGE_DONE;
            continue;
        }
        let len = edges[edge2].opos - edges[index].opos;
        let Some(anchor_index) = anchor else {
            edges[index].pos = if len < 96 {
                centre(edges[index].opos + (len >> 1), len) - len / 2
            } else {
                pix_round(edges[index].opos)
            };
            anchor = Some(index);
            edges[index].flags |= EDGE_DONE;
            align_linked(edges, index, edge2);
            continue;
        };
        let org_pos = edges[anchor_index].pos + (edges[index].opos - edges[anchor_index].opos);
        let org_center = org_pos + (len >> 1);
        if edges[edge2].flags & EDGE_DONE != 0 {
            edges[index].pos = edges[edge2].pos - len;
        } else if len < 96 {
            let center = centre(org_center, len);
            edges[index].pos = center - len / 2;
            edges[edge2].pos = center + len / 2;
        } else {
            let pos1 = pix_round(org_pos);
            let delta1 = (pos1 + (len >> 1) - org_center).abs();
            let pos2 = pix_round(org_pos + len) - len;
            let delta2 = (pos2 + (len >> 1) - org_center).abs();
            edges[index].pos = if delta1 < delta2 { pos1 } else { pos2 };
            edges[edge2].pos = edges[index].pos + len;
        }
        edges[index].flags |= EDGE_DONE;
        edges[edge2].flags |= EDGE_DONE;
        // a stem fitted below the edge before it moves up to it, unless that would flatten it
        if index > 0
            && edges[index].pos < edges[index - 1].pos
            && (edges[edge2].pos - edges[index - 1].pos).abs() > 16
        {
            edges[index].pos = edges[index - 1].pos;
        }
    }
    if !has_serifs && anchor.is_some() {
        return;
    }
    // what is left has no link, so freetype's ordering fix after this pass never applies
    for index in 0..edges.len() {
        if edges[index].flags & EDGE_DONE != 0 {
            continue;
        }
        let edge = edges[index];
        let serif = edge
            .serif
            .filter(|&serif| (edges[serif].opos - edge.opos).abs() < 64 + 16);
        if let Some(serif) = serif {
            edges[index].pos = edges[serif].pos + (edge.opos - edges[serif].opos);
        } else if let Some(anchor_index) = anchor {
            let done = |edge: &Edge| edge.flags & EDGE_DONE != 0;
            let before = edges[..index].iter().rposition(done);
            let after = edges[index + 1..].iter().position(done);
            edges[index].pos = match (before, after) {
                (Some(before), Some(after)) => {
                    let (before, after) = (edges[before], edges[index + 1 + after]);
                    if after.opos == before.opos {
                        before.pos
                    } else {
                        before.pos
                            + mul_div(
                                edge.opos - before.opos,
                                after.pos - before.pos,
                                after.opos - before.opos,
                            )
                    }
                }
                _ => {
                    let anchor = edges[anchor_index];
                    anchor.pos + ((edge.opos - anchor.opos + 16) & !31)
                }
            };
        } else {
            edges[index].pos = pix_round(edge.opos);
            anchor = Some(index);
        }
        edges[index].flags |= EDGE_DONE;
    }
}

// strong points off the edges move with the edges around them,
// af_glyph_hints_align_strong_points
fn align_strong_points(points: &mut [Point], edges: &mut [Edge]) {
    let (Some(&first), Some(&last)) = (edges.first(), edges.last()) else {
        return;
    };
    for point in points.iter_mut() {
        if point.flags & (FLAG_TOUCH | FLAG_WEAK) != 0 {
            continue;
        }
        let (u, ou) = (point.fy, point.oy);
        point.flags |= FLAG_TOUCH;
        if first.fpos >= u {
            point.y = first.pos - (first.opos - ou);
            continue;
        }
        if u >= last.fpos {
            point.y = last.pos + (ou - last.opos);
            continue;
        }
        let (mut min, mut max) = (0, edges.len());
        if max <= 8 {
            min = edges.iter().position(|edge| edge.fpos >= u).unwrap_or(max);
            if edges[min].fpos == u {
                point.y = edges[min].pos;
                continue;
            }
        } else {
            while min < max {
                let mid = (min + max) >> 1;
                match u.cmp(&edges[mid].fpos) {
                    std::cmp::Ordering::Less => max = mid,
                    std::cmp::Ordering::Greater => min = mid + 1,
                    std::cmp::Ordering::Equal => {
                        min = mid;
                        break;
                    }
                }
            }
            if edges[min].fpos == u {
                point.y = edges[min].pos;
                continue;
            }
        }
        let after = edges[min];
        let before = &mut edges[min - 1];
        if before.scale == 0 {
            before.scale = div_fix(after.pos - before.pos, after.fpos - before.fpos);
        }
        point.y = before.pos + mul_fix(u - before.fpos, before.scale);
    }
}

// the remaining points interpolated between the touched points of their contour, as the
// truetype iup instruction does, af_glyph_hints_align_weak_points
fn align_weak_points(points: &mut [Point], contours: &[(usize, usize)]) {
    let interpolate = |points: &mut [Point], from: usize, to: usize, one: usize, two: usize| {
        if from > to {
            return;
        }
        let (mut ref1, mut ref2) = (points[one], points[two]);
        if ref1.oy > ref2.oy {
            std::mem::swap(&mut ref1, &mut ref2);
        }
        let (v1, v2, u1, u2) = (ref1.oy, ref2.oy, ref1.y, ref2.y);
        let scale = (u1 != u2 && v1 != v2).then(|| div_fix(u2 - u1, v2 - v1));
        for point in &mut points[from..=to] {
            let v = point.oy;
            point.y = if v <= v1 {
                v + u1 - v1
            } else if v >= v2 {
                v + u2 - v2
            } else {
                scale.map_or(u1, |scale| u1 + mul_fix(v - v1, scale))
            };
        }
    };
    let touched = |point: &Point| point.flags & FLAG_TOUCH != 0;
    for &(start, end) in contours {
        let Some(first_touched) = points[start..=end].iter().position(touched) else {
            continue;
        };
        let first_touched = start + first_touched;
        let mut point = first_touched;
        let last_touched = loop {
            while point < end && touched(&points[point + 1]) {
                point += 1;
            }
            let last_touched = point;
            let Some(next) = points[point + 1..=end].iter().position(touched) else {
                break last_touched;
            };
            point += 1 + next;
            interpolate(points, last_touched + 1, point - 1, last_touched, point);
        };
        if last_touched == first_touched {
            let delta = points[first_touched].y - points[first_touched].oy;
            for (index, point) in (start..).zip(&mut points[start..=end]) {
                if index != first_touched {
                    point.y = point.oy + delta;
                }
            }
        } else {
            if last_touched < end {
                interpolate(points, last_touched + 1, end, last_touched, first_touched);
            }
            if first_touched > start {
                interpolate(
                    points,
                    start,
                    first_touched - 1,
                    last_touched,
                    first_touched,
                );
            }
        }
    }
}

#[derive(Clone, Copy)]
struct Blue {
    reference: i64,
    overshoot: i64,
    top: bool,
    x_height: bool,
    ascender: i64,
    descender: i64,
}

// a face's horizontal stem widths and blue zones in font units, af_latin_metrics_init
pub(crate) struct Metrics {
    units_per_em: i64,
    widths: Vec<i64>,
    edge_distance_threshold: i64,
    blues: Vec<Blue>,
    // the glyphs hinted as latin, and the marks among them that skip the blue zones
    latin: Vec<u16>,
    nonbase: Vec<u16>,
}

impl Metrics {
    pub(crate) fn new(face: &FontFace) -> Option<Self> {
        let units_per_em = face.units_per_em() as i64;
        let glyphs = |ranges: &[(u32, u32)]| {
            let mut glyphs = ranges
                .iter()
                .flat_map(|&(first, last)| face.characters().range(first..=last))
                .map(|(_, &glyph)| glyph)
                .filter(|&glyph| glyph != 0)
                .collect::<Vec<_>>();
            glyphs.sort_unstable();
            glyphs.dedup();
            glyphs
        };
        let earlier = glyphs(&EARLIER);
        let mut latin = glyphs(&LATIN);
        latin.retain(|glyph| earlier.binary_search(glyph).is_err());
        let mut nonbase = glyphs(&NONBASE);
        nonbase.retain(|glyph| latin.binary_search(glyph).is_ok());
        let widths = standard_widths(face, units_per_em);
        let standard_width = widths.first().copied().unwrap_or(50 * units_per_em / 2048);
        let blues = blue_zones(face, units_per_em);
        if blues.is_empty() {
            return None;
        }
        Some(Self {
            units_per_em,
            widths,
            edge_distance_threshold: standard_width / 5,
            blues,
            latin,
            nonbase,
        })
    }

    // the outline at a pixel size with its vertical coordinates fitted, in device pixels with y
    // growing upwards; horizontal coordinates are only scaled, and glyphs freetype hints with
    // another script's metrics are left alone
    pub(crate) fn hint(&self, glyph: u16, outline: &Outline, size: f32) -> Option<Outline> {
        self.latin.binary_search(&glyph).ok()?;
        let x_scale = div_fix((size * 64.0) as i64, self.units_per_em);
        let y_scale = self.fitted_scale(x_scale);
        let mut glyph_points = load_glyph(outline, self.units_per_em, y_scale);
        let major_dir = glyph_points.major_dir;
        let mut segments = compute_segments(&glyph_points, self.units_per_em / 14);
        link_segments(&mut segments, &self.widths, major_dir, self.units_per_em);
        let mut edges = compute_edges(
            &mut segments,
            major_dir,
            y_scale,
            self.edge_distance_threshold,
        );
        if self.nonbase.binary_search(&glyph).is_err() {
            self.find_blue_edges(&mut edges, major_dir, y_scale);
        }
        hint_edges(&mut edges);
        let points = &mut glyph_points.points;
        for segment in &segments {
            let Some(edge) = segment.edge else {
                continue;
            };
            let mut point = segment.first;
            loop {
                points[point].y = edges[edge].pos;
                points[point].flags |= FLAG_TOUCH;
                if point == segment.last {
                    break;
                }
                point = points[point].next;
            }
        }
        align_strong_points(points, &mut edges);
        align_weak_points(points, &glyph_points.contours);
        let mut hinted = glyph_points.points.iter();
        let contours = outline
            .contours
            .iter()
            .filter(|contour| !contour.is_empty())
            .map(|contour| {
                contour
                    .iter()
                    .zip(&mut hinted)
                    .map(|(point, hinted)| OutlinePoint {
                        x: mul_fix(point.x.round() as i64, x_scale) as f32 / 64.0,
                        y: hinted.y as f32 / 64.0,
                        on_curve: point.on_curve,
                    })
                    .collect()
            })
            .collect();
        Some(Outline { contours })
    }

    // the vertical scale nudged so the x-height lands on a whole pixel, unless that moves the
    // tallest point by two pixels or more, af_latin_metrics_scale_dim
    fn fitted_scale(&self, scale: i64) -> i64 {
        let Some(blue) = self.blues.iter().find(|blue| blue.x_height) else {
            return scale;
        };
        let scaled = mul_fix(blue.overshoot, scale);
        let fitted = (scaled + 40) & !63;
        if scaled == fitted {
            return scale;
        }
        let fitted_scale = mul_div(scale, fitted, scaled);
        let max_height = self.blues.iter().fold(self.units_per_em, |height, blue| {
            height.max(blue.ascender).max(-blue.descender)
        });
        let dist = mul_fix(max_height, fitted_scale - scale);
        if -128 < dist && dist < 128 {
            fitted_scale
        } else {
            scale
        }
    }

    // each edge snaps to the nearest zone under a pixel tall that it faces, within a fortieth of
    // an em or half a pixel, af_latin_hints_compute_blue_edges
    fn find_blue_edges(&self, edges: &mut [Edge], major_dir: i8, scale: i64) {
        let fitted = self
            .blues
            .iter()
            .map(|blue| {
                let dist = mul_fix(blue.reference - blue.overshoot, scale);
                (dist.abs() <= 48).then(|| {
                    let delta = match dist.abs() {
                        0..32 => 0,
                        32..48 => 32,
                        _ => 64,
                    } * dist.signum();
                    let reference = pix_round(mul_fix(blue.reference, scale));
                    (reference, reference - delta)
                })
            })
            .collect::<Vec<_>>();
        let limit = mul_fix(self.units_per_em / 40, scale).min(32);
        for edge in edges {
            let mut best = limit;
            for (blue, fitted) in self.blues.iter().zip(&fitted) {
                let Some((reference, overshoot)) = *fitted else {
                    continue;
                };
                if !(blue.top ^ (edge.dir == major_dir)) {
                    continue;
                }
                let dist = mul_fix((edge.fpos - blue.reference).abs(), scale);
                if dist < best {
                    best = dist;
                    edge.blue = Some(reference);
                }
                if edge.flags & EDGE_ROUND != 0
                    && dist != 0
                    && blue.top ^ (edge.fpos < blue.reference)
                {
                    let dist = mul_fix((edge.fpos - blue.overshoot).abs(), scale);
                    if dist < best {
                        best = dist;
                        edge.blue = Some(overshoot);
                    }
                }
            }
        }
    }
}

// the widths of the horizontal stems of the face's o, af_latin_metrics_init_widths; empty when
// there is no o to measure, so the fallback width applies
fn standard_widths(face: &FontFace, units_per_em: i64) -> Vec<i64> {
    let Some(glyph) = ['o', 'O', '0']
        .into_iter()
        .map(|character| face.glyph_for(character))
        .find(|&glyph| glyph != 0)
    else {
        return Vec::new();
    };
    let Some(outline) = face
        .outline(glyph)
        .filter(|outline| outline.contours.iter().any(|contour| !contour.is_empty()))
    else {
        return Vec::new();
    };
    let glyph = load_glyph(&outline, units_per_em, 0x10000);
    let mut segments = compute_segments(&glyph, units_per_em / 14);
    link_segments(&mut segments, &[], glyph.major_dir, units_per_em);
    let widths = segments
        .iter()
        .enumerate()
        .filter_map(|(index, segment)| {
            let link = segment.link?;
            (link > index && segments[link].link == Some(index))
                .then(|| (segment.pos - segments[link].pos).abs())
        })
        .take(MAX_WIDTHS)
        .collect();
    quantize_widths(widths, units_per_em / 100)
}

// af_sort_and_quantize_widths with its quirks: no widths leave one zero width, a cluster's sum
// is divided by its end index, and the width that closes a cluster stays apart
fn quantize_widths(mut widths: Vec<i64>, threshold: i64) -> Vec<i64> {
    match widths.len() {
        0 => return vec![0],
        1 => return widths,
        _ => {}
    }
    widths.sort_unstable();
    let count = widths.len();
    let mut cluster = 0;
    let mut value = widths[0];
    let mut index = 1;
    while index < count {
        if widths[index] - value > threshold || index == count - 1 {
            if widths[index] - value <= threshold && index == count - 1 {
                index += 1;
            }
            let sum = widths[cluster..index].iter().sum::<i64>();
            widths[cluster..index].fill(0);
            widths[cluster] = sum / index as i64;
            if index < count - 1 {
                cluster = index + 1;
                value = widths[cluster];
            }
        }
        index += 1;
    }
    let mut kept = vec![widths[0]];
    kept.extend(widths[1..].iter().filter(|&&width| width != 0));
    kept
}

// the reference and overshoot heights of each blue string's letters in their original order,
// af_latin_metrics_init_blues
fn blue_zones(face: &FontFace, units_per_em: i64) -> Vec<Blue> {
    let flat_threshold = units_per_em / 14;
    let mut blues = Vec::new();
    for (string, top, x_height) in BLUES {
        let (mut flats, mut rounds) = (Vec::new(), Vec::new());
        let (mut ascender, mut descender) = (0, 0);
        for character in string.chars() {
            let glyph = face.glyph_for(character);
            let Some(outline) = (glyph != 0).then(|| face.outline(glyph)).flatten() else {
                continue;
            };
            let mut points = Vec::new();
            let mut contours = Vec::new();
            for contour in &outline.contours {
                let first = points.len();
                points.extend(contour.iter().map(|point| {
                    (
                        point.x.round() as i64,
                        point.y.round() as i64,
                        point.on_curve,
                    )
                }));
                contours.push((first, points.len()));
            }
            if points.len() <= 2 {
                continue;
            }
            let (y, round) = extremum(
                &points,
                &contours,
                top,
                flat_threshold,
                (&mut ascender, &mut descender),
            );
            if round {
                rounds.push(y);
            } else {
                flats.push(y);
            }
        }
        flats.sort_unstable();
        rounds.sort_unstable();
        let (mut reference, mut overshoot) =
            match (flats.get(flats.len() / 2), rounds.get(rounds.len() / 2)) {
                (Some(&flat), Some(&round)) => (flat, round),
                (Some(&only), None) | (None, Some(&only)) => (only, only),
                (None, None) => continue,
            };
        if overshoot != reference && top ^ (overshoot > reference) {
            reference = (overshoot + reference) / 2;
            overshoot = reference;
        }
        blues.push(Blue {
            reference,
            overshoot,
            top,
            x_height,
            ascender,
            descender,
        });
    }
    // sorted from the bottom up, no zone may reach past the one above it
    let mut sorted = (0..blues.len()).collect::<Vec<_>>();
    sorted.sort_by_key(|&index| {
        let blue = blues[index];
        if blue.top {
            blue.reference
        } else {
            blue.overshoot
        }
    });
    for pair in sorted.windows(2) {
        let next = blues[pair[1]];
        let limit = if next.top {
            next.overshoot
        } else {
            next.reference
        };
        let blue = &mut blues[pair[0]];
        let value = if blue.top {
            &mut blue.overshoot
        } else {
            &mut blue.reference
        };
        *value = (*value).min(limit);
    }
    blues
}

// the highest or lowest point of a glyph and whether the stretch of outline around it is round,
// widening the extent the zone's glyphs reach both ways
fn extremum(
    points: &[(i64, i64, bool)],
    contours: &[(usize, usize)],
    top: bool,
    flat_threshold: i64,
    (ascender, descender): (&mut i64, &mut i64),
) -> (i64, bool) {
    let mut best: Option<usize> = None;
    let mut best_y = 0;
    let (mut contour_first, mut contour_last) = (0, 0);
    // each contour as its first and one past its last point, skipping those under two points
    for &(first, end) in contours.iter().filter(|&&(first, end)| end > first + 1) {
        for (index, &(_, y, _)) in points.iter().enumerate().take(end).skip(first) {
            if best.is_none() || (if top { y > best_y } else { y < best_y }) {
                best = Some(index);
                best_y = y;
                if top {
                    *ascender = (*ascender).max(y);
                } else {
                    *descender = (*descender).min(y);
                }
            } else if top {
                *descender = (*descender).min(y);
            } else {
                *ascender = (*ascender).max(y);
            }
        }
        if best.is_some_and(|best| best >= first) {
            contour_first = first;
            contour_last = end - 1;
        }
    }
    let Some(best) = best else {
        return (best_y, false);
    };
    let (best_x, _, best_on) = points[best];
    let mut segment = (best, best);
    let mut on = if best_on {
        (Some(best), Some(best))
    } else {
        (None, None)
    };
    let leaves = |index: usize| {
        let dist = (points[index].1 - best_y).abs();
        dist > 5 && (points[index].0 - best_x).abs() <= 20 * dist
    };
    let mut prev = best;
    loop {
        prev = if prev > contour_first {
            prev - 1
        } else {
            contour_last
        };
        if leaves(prev) {
            break;
        }
        segment.0 = prev;
        if points[prev].2 {
            on.0 = Some(prev);
            on.1.get_or_insert(prev);
        }
        if prev == best {
            break;
        }
    }
    let mut next = best;
    loop {
        next = if next < contour_last {
            next + 1
        } else {
            contour_first
        };
        if leaves(next) {
            break;
        }
        segment.1 = next;
        if points[next].2 {
            on.1 = Some(next);
            on.0.get_or_insert(next);
        }
        if next == best {
            break;
        }
    }
    let flat = matches!(on, (Some(first), Some(last))
        if (points[last].0 - points[first].0).abs() > flat_threshold);
    (
        best_y,
        !flat && (!points[segment.0].2 || !points[segment.1].2),
    )
}
