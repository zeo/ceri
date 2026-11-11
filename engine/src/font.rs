use std::cell::RefCell;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::rc::Rc;

use crate::color_glyph::ColorGlyph;

const MAX_COMPOSITE_DEPTH: u8 = 5;
const MAX_CONTOUR_POINTS: usize = 20_000;
const MAX_GLYPH_PIXELS: usize = 4_000_000;

// glyph outlines and metrics read straight from an sfnt file, no shaping engine
pub struct FontFace {
    data: Rc<[u8]>,
    units_per_em: f32,
    ascent: f32,
    descent: f32,
    line_gap: f32,
    num_glyphs: u16,
    long_loca: bool,
    loca: (usize, usize),
    glyf: (usize, usize),
    // the PostScript outlines of an OpenType CFF font, used when it has no glyf table
    cff: Option<crate::cff::CffFont>,
    hmtx: (usize, usize),
    horizontal_metrics: u16,
    characters: Rc<BTreeMap<u32, u16>>,
    // usWeightClass and the italic flag, from OS/2 and head
    weight: u16,
    // sxHeight from OS/2 version 2 on, or half an em
    x_height: f32,
    italic: bool,
    // where each table starts, for the layout tables shaping reads
    tables: BTreeMap<[u8; 4], (usize, usize)>,
    // the axes of a variable truetype face
    variations: Option<Rc<crate::variation::Variations>>,
    // the normalized coordinates an instance draws at, empty for the default instance
    coordinates: Vec<f32>,
    // an instance's outlines and advances, varied once per glyph
    outlines: RefCell<HashMap<u16, Option<Outline>>>,
    advances: RefCell<HashMap<u16, f32>>,
    // the instances of a variable face, by their coordinates in F2DOT14 units
    instances: RefCell<HashMap<Vec<i16>, Rc<FontFace>>>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Outline {
    pub contours: Vec<Vec<OutlinePoint>>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OutlinePoint {
    pub x: f32,
    pub y: f32,
    pub on_curve: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GlyphBitmap {
    pub left: i32,
    pub top: i32,
    pub width: usize,
    pub height: usize,
    pub coverage: Vec<u8>,
}

impl FontFace {
    pub fn parse(data: Vec<u8>) -> Result<Self, String> {
        let data = if crate::woff::is_woff(&data) {
            crate::woff::decode(&data)?
        } else {
            data
        };
        let tables = read_table_directory(&data)?;
        let head = *tables.get(b"head").ok_or("font has no head table")?;
        let hhea = *tables.get(b"hhea").ok_or("font has no hhea table")?;
        let maxp = *tables.get(b"maxp").ok_or("font has no maxp table")?;
        let hmtx = *tables.get(b"hmtx").ok_or("font has no hmtx table")?;
        let (loca, glyf, cff) = match (tables.get(b"loca"), tables.get(b"glyf")) {
            (Some(loca), Some(glyf)) => (*loca, *glyf, None),
            _ => {
                let table = tables
                    .get(b"CFF ")
                    .ok_or("font has neither glyf nor CFF outlines")?;
                ((0, 0), (0, 0), Some(crate::cff::parse(&data, table.0)?))
            }
        };
        let cmap = *tables.get(b"cmap").ok_or("font has no cmap table")?;

        let units_per_em = f32::from(read_u16(&data, head.0 + 18)?);
        if units_per_em <= 0.0 {
            return Err("font declares no units per em".to_string());
        }
        let long_loca = match read_u16(&data, head.0 + 50)? {
            0 => false,
            1 => true,
            other => return Err(format!("unsupported loca format {other}")),
        };
        let ascent = f32::from(read_i16(&data, hhea.0 + 4)?);
        let descent = f32::from(read_i16(&data, hhea.0 + 6)?);
        let line_gap = f32::from(read_i16(&data, hhea.0 + 8)?);
        let horizontal_metrics = read_u16(&data, hhea.0 + 34)?;
        let num_glyphs = read_u16(&data, maxp.0 + 4)?;
        let characters = read_character_map(&data, cmap.0)?;
        if characters.is_empty() {
            return Err("font has no usable unicode character map".to_string());
        }
        let os2 = tables.get(b"OS/2").map(|table| table.0);
        let weight = os2
            .and_then(|os2| read_u16(&data, os2 + 4).ok())
            .filter(|weight| (1..=1000).contains(weight))
            .unwrap_or(400);
        let x_height = os2
            .filter(|&os2| read_u16(&data, os2).is_ok_and(|version| version >= 2))
            .and_then(|os2| read_i16(&data, os2 + 86).ok())
            .filter(|&height| height > 0)
            .map_or(units_per_em / 2.0, f32::from);
        let italic = os2
            .and_then(|os2| read_u16(&data, os2 + 62).ok())
            .map(|selection| selection & 1 != 0)
            .or_else(|| {
                read_u16(&data, head.0 + 44)
                    .ok()
                    .map(|style| style & 2 != 0)
            })
            .unwrap_or(false);
        let variations = cff
            .is_none()
            .then(|| crate::variation::Variations::parse(&data, &tables))
            .flatten()
            .map(Rc::new);

        Ok(Self {
            data: data.into(),
            units_per_em,
            ascent,
            descent: -descent,
            line_gap,
            num_glyphs,
            long_loca,
            loca,
            glyf,
            cff,
            hmtx,
            horizontal_metrics,
            characters: Rc::new(characters),
            weight,
            x_height,
            italic,
            tables,
            variations,
            coordinates: Vec::new(),
            outlines: RefCell::default(),
            advances: RefCell::default(),
            instances: RefCell::default(),
        })
    }

    // the instance of a variable face that draws a style: font-weight on the wght axis within
    // the weights the face is used for, italic on ital or else slnt, opsz at the optical size,
    // and font-variation-settings over all of them
    pub(crate) fn instance(self: &Rc<Self>, style: FontStyle, weights: (u16, u16)) -> Rc<Self> {
        let Some(variations) = &self.variations else {
            return self.clone();
        };
        let settings = font_variation_values(style.variations);
        let has_ital = variations.axis(b"ital").is_some();
        let values = variations
            .axes
            .iter()
            .map(|axis| {
                let requested = match &axis.tag {
                    b"wght" => Some(f32::from(style.weight.clamp(weights.0, weights.1))),
                    b"ital" if style.italic => Some(1.0),
                    // css oblique's default angle, counter-clockwise as slnt measures it
                    b"slnt" if style.italic && !has_ital => Some(-14.0),
                    b"opsz" if style.optical_size != 0 => Some(f32::from_bits(style.optical_size)),
                    _ => None,
                };
                settings
                    .iter()
                    .rev()
                    .find(|(tag, _)| tag == &axis.tag)
                    .map(|&(_, value)| value)
                    .or(requested)
                    .unwrap_or(axis.default)
                    .clamp(axis.minimum, axis.maximum)
            })
            .collect::<Vec<_>>();
        let key = variations.normalize(&values);
        if key.iter().all(|&coordinate| coordinate == 0) {
            return self.clone();
        }
        if let Some(instance) = self.instances.borrow().get(&key) {
            return instance.clone();
        }
        let value = |tag: &[u8; 4]| {
            variations
                .axes
                .iter()
                .position(|axis| &axis.tag == tag)
                .map(|index| values[index])
        };
        let base: &Self = self;
        let instance = Rc::new(Self {
            data: base.data.clone(),
            cff: None,
            characters: base.characters.clone(),
            weight: value(b"wght").map_or(base.weight, |weight| weight.round() as u16),
            italic: base.italic
                || value(b"ital").is_some_and(|ital| ital > 0.0)
                || value(b"slnt").is_some_and(|slnt| slnt < 0.0),
            tables: base.tables.clone(),
            variations: base.variations.clone(),
            coordinates: key
                .iter()
                .map(|&coordinate| f32::from(coordinate) / 16384.0)
                .collect(),
            outlines: RefCell::default(),
            advances: RefCell::default(),
            instances: RefCell::default(),
            ..*base
        });
        self.instances.borrow_mut().insert(key, instance.clone());
        instance
    }

    // the weights a face can draw: its wght axis, or its one weight
    pub(crate) fn weight_range(&self) -> (u16, u16) {
        self.variations
            .as_ref()
            .and_then(|variations| variations.axis(b"wght"))
            .map_or((self.weight, self.weight), |axis| {
                (axis.minimum.round() as u16, axis.maximum.round() as u16)
            })
    }

    pub fn units_per_em(&self) -> f32 {
        self.units_per_em
    }

    pub fn ascent(&self, size: f32) -> f32 {
        self.ascent / self.units_per_em * size
    }

    pub fn descent(&self, size: f32) -> f32 {
        self.descent / self.units_per_em * size
    }

    pub fn x_height(&self, size: f32) -> f32 {
        self.x_height / self.units_per_em * size
    }

    pub fn line_height(&self, size: f32) -> f32 {
        (self.ascent + self.descent + self.line_gap) / self.units_per_em * size
    }

    pub(crate) fn data(&self) -> &[u8] {
        &self.data
    }

    pub(crate) fn table(&self, tag: &[u8; 4]) -> Option<usize> {
        self.tables.get(tag).map(|(start, _)| *start)
    }

    // a glyph's advance in font units, as HVAR or else the phantom points move it
    pub(crate) fn advance_units(&self, glyph: u16) -> f32 {
        let metrics = self.horizontal_metrics.max(1);
        let index = glyph.min(metrics - 1);
        let advance =
            f32::from(read_u16(&self.data, self.hmtx.0 + usize::from(index) * 4).unwrap_or(0));
        let Some(variations) = self
            .variations
            .as_ref()
            .filter(|_| !self.coordinates.is_empty())
        else {
            return advance;
        };
        if let Some(&varied) = self.advances.borrow().get(&glyph) {
            return varied;
        }
        let delta = variations
            .advance_delta(&self.data, glyph, &self.coordinates)
            .or_else(|| {
                let points = self.point_count(glyph);
                let deltas = self.point_deltas(glyph, &vec![(0.0, 0.0); points], &[])?;
                Some(deltas[points + 1].0 - deltas[points].0)
            })
            .unwrap_or(0.0);
        let varied = (advance + delta).max(0.0);
        self.advances.borrow_mut().insert(glyph, varied);
        varied
    }

    // the points gvar moves in a glyph before its four phantom points: a simple glyph's
    // outline points, or one per component of a composite
    fn point_count(&self, glyph: u16) -> usize {
        let Some((start, end)) = self.glyph_range(glyph) else {
            return 0;
        };
        match read_i16(&self.data, start) {
            Ok(contours) if contours > 0 => {
                read_u16(&self.data, start + 10 + (contours as usize - 1) * 2)
                    .map_or(0, |last| usize::from(last) + 1)
            }
            Ok(contours) if contours < 0 => self
                .components(start, end)
                .map_or(0, |components| components.len()),
            _ => 0,
        }
    }

    // how gvar moves a glyph's points, then its phantom points, at this instance's
    // coordinates; none at the default instance
    fn point_deltas(
        &self,
        glyph: u16,
        points: &[(f32, f32)],
        contour_ends: &[usize],
    ) -> Option<Vec<(f32, f32)>> {
        if self.coordinates.is_empty() {
            return None;
        }
        let mut points = points.to_vec();
        points.extend([(0.0, 0.0); 4]);
        self.variations.as_ref()?.glyph_deltas(
            &self.data,
            glyph,
            &self.coordinates,
            &points,
            contour_ends,
        )
    }

    pub fn weight(&self) -> u16 {
        self.weight
    }

    pub fn is_italic(&self) -> bool {
        self.italic
    }

    pub fn has_glyph(&self, character: char) -> bool {
        self.characters.contains_key(&u32::from(character))
    }

    pub fn glyph_for(&self, character: char) -> u16 {
        self.characters
            .get(&u32::from(character))
            .copied()
            .unwrap_or(0)
    }

    pub fn advance(&self, glyph: u16, size: f32) -> f32 {
        self.advance_units(glyph) / self.units_per_em * size
    }

    pub fn outline(&self, glyph: u16) -> Option<Outline> {
        if let Some(cff) = &self.cff {
            return cff.outline(&self.data, glyph);
        }
        if let Some(cached) = self.outlines.borrow().get(&glyph) {
            return cached.clone();
        }
        let mut outline = Outline::default();
        let outline = self
            .append_outline(glyph, (1.0, 0.0, 0.0, 1.0), (0.0, 0.0), 0, &mut outline)
            .map(|()| outline);
        if !self.coordinates.is_empty() {
            self.outlines.borrow_mut().insert(glyph, outline.clone());
        }
        outline
    }

    fn glyph_range(&self, glyph: u16) -> Option<(usize, usize)> {
        if glyph >= self.num_glyphs {
            return None;
        }
        let index = usize::from(glyph);
        let (start, end) = if self.long_loca {
            (
                read_u32(&self.data, self.loca.0 + index * 4)? as usize,
                read_u32(&self.data, self.loca.0 + index * 4 + 4)? as usize,
            )
        } else {
            (
                usize::from(read_u16(&self.data, self.loca.0 + index * 2).ok()?) * 2,
                usize::from(read_u16(&self.data, self.loca.0 + index * 2 + 2).ok()?) * 2,
            )
        };
        if end <= start || end > self.glyf.1 {
            return None;
        }
        Some((self.glyf.0 + start, self.glyf.0 + end))
    }

    fn append_outline(
        &self,
        glyph: u16,
        transform: (f32, f32, f32, f32),
        offset: (f32, f32),
        depth: u8,
        outline: &mut Outline,
    ) -> Option<()> {
        if depth > MAX_COMPOSITE_DEPTH {
            return None;
        }
        let Some((start, end)) = self.glyph_range(glyph) else {
            return Some(());
        };
        let contour_count = read_i16(&self.data, start).ok()?;
        if contour_count >= 0 {
            self.append_simple_glyph(
                glyph,
                (start, end),
                contour_count as usize,
                transform,
                offset,
                outline,
            )
        } else {
            self.append_composite_glyph(glyph, (start, end), transform, offset, depth, outline)
        }
    }

    fn append_simple_glyph(
        &self,
        glyph: u16,
        (start, end): (usize, usize),
        contour_count: usize,
        transform: (f32, f32, f32, f32),
        offset: (f32, f32),
        outline: &mut Outline,
    ) -> Option<()> {
        let mut cursor = start + 10;
        let mut contour_ends = Vec::with_capacity(contour_count);
        for _ in 0..contour_count {
            contour_ends.push(usize::from(read_u16(&self.data, cursor).ok()?));
            cursor += 2;
        }
        let point_count = contour_ends.last().map(|last| last + 1).unwrap_or(0);
        if point_count == 0 || point_count > MAX_CONTOUR_POINTS {
            return Some(());
        }
        let instruction_length = usize::from(read_u16(&self.data, cursor).ok()?);
        cursor += 2 + instruction_length;

        let mut flags = Vec::with_capacity(point_count);
        while flags.len() < point_count {
            let flag = *self.data.get(cursor)?;
            cursor += 1;
            flags.push(flag);
            if flag & 0x08 != 0 {
                let repeats = *self.data.get(cursor)?;
                cursor += 1;
                for _ in 0..repeats {
                    if flags.len() == point_count {
                        break;
                    }
                    flags.push(flag);
                }
            }
        }

        let mut x_coordinates = Vec::with_capacity(point_count);
        let mut x = 0_i32;
        for flag in &flags {
            if flag & 0x02 != 0 {
                let delta = i32::from(*self.data.get(cursor)?);
                cursor += 1;
                x += if flag & 0x10 != 0 { delta } else { -delta };
            } else if flag & 0x10 == 0 {
                x += i32::from(read_i16(&self.data, cursor).ok()?);
                cursor += 2;
            }
            x_coordinates.push(x);
        }

        let mut y_coordinates = Vec::with_capacity(point_count);
        let mut y = 0_i32;
        for flag in &flags {
            if flag & 0x04 != 0 {
                let delta = i32::from(*self.data.get(cursor)?);
                cursor += 1;
                y += if flag & 0x20 != 0 { delta } else { -delta };
            } else if flag & 0x20 == 0 {
                y += i32::from(read_i16(&self.data, cursor).ok()?);
                cursor += 2;
            }
            y_coordinates.push(y);
        }
        if cursor > end {
            return None;
        }
        let mut points = x_coordinates
            .iter()
            .zip(&y_coordinates)
            .map(|(&x, &y)| (x as f32, y as f32))
            .collect::<Vec<_>>();
        if let Some(deltas) = self.point_deltas(glyph, &points, &contour_ends) {
            for (point, delta) in points.iter_mut().zip(deltas) {
                point.0 += delta.0;
                point.1 += delta.1;
            }
        }

        let mut first = 0;
        for &last in &contour_ends {
            if last >= point_count || last < first {
                break;
            }
            let mut contour = Vec::with_capacity(last + 1 - first);
            for index in first..=last {
                let (point_x, point_y) = points[index];
                contour.push(OutlinePoint {
                    x: transform.0 * point_x + transform.2 * point_y + offset.0,
                    y: transform.1 * point_x + transform.3 * point_y + offset.1,
                    on_curve: flags[index] & 0x01 != 0,
                });
            }
            outline.contours.push(contour);
            first = last + 1;
        }
        Some(())
    }

    fn append_composite_glyph(
        &self,
        glyph: u16,
        (start, end): (usize, usize),
        transform: (f32, f32, f32, f32),
        offset: (f32, f32),
        depth: u8,
        outline: &mut Outline,
    ) -> Option<()> {
        let mut components = self.components(start, end)?;
        let offsets = components
            .iter()
            .map(|component| component.offset)
            .collect::<Vec<_>>();
        if let Some(deltas) = self.point_deltas(glyph, &offsets, &[]) {
            for (component, delta) in components.iter_mut().zip(deltas) {
                if component.offset_is_xy {
                    component.offset.0 += delta.0;
                    component.offset.1 += delta.1;
                }
            }
        }
        for component in components {
            let component_transform = component.transform;
            // only offsets in component space are supported, matching ARGS_ARE_XY_VALUES
            let (component_offset_x, component_offset_y) = if component.offset_is_xy {
                component.offset
            } else {
                (0.0, 0.0)
            };
            let combined = (
                transform.0 * component_transform.0 + transform.2 * component_transform.1,
                transform.1 * component_transform.0 + transform.3 * component_transform.1,
                transform.0 * component_transform.2 + transform.2 * component_transform.3,
                transform.1 * component_transform.2 + transform.3 * component_transform.3,
            );
            let component_origin = (
                offset.0 + transform.0 * component_offset_x + transform.2 * component_offset_y,
                offset.1 + transform.1 * component_offset_x + transform.3 * component_offset_y,
            );
            self.append_outline(
                component.glyph,
                combined,
                component_origin,
                depth + 1,
                outline,
            )?;
        }
        Some(())
    }

    // a composite glyph's component records, in order
    fn components(&self, start: usize, end: usize) -> Option<Vec<Component>> {
        let mut components = Vec::new();
        let mut cursor = start + 10;
        loop {
            if cursor + 4 > end {
                return Some(components);
            }
            let flags = read_u16(&self.data, cursor).ok()?;
            let component = read_u16(&self.data, cursor + 2).ok()?;
            cursor += 4;
            let (argument_x, argument_y) = if flags & 0x0001 != 0 {
                let x = f32::from(read_i16(&self.data, cursor).ok()?);
                let y = f32::from(read_i16(&self.data, cursor + 2).ok()?);
                cursor += 4;
                (x, y)
            } else {
                let x = f32::from(*self.data.get(cursor)? as i8);
                let y = f32::from(*self.data.get(cursor + 1)? as i8);
                cursor += 2;
                (x, y)
            };
            let mut component_transform = (1.0, 0.0, 0.0, 1.0);
            if flags & 0x0008 != 0 {
                let scale = read_f2dot14(&self.data, cursor).ok()?;
                cursor += 2;
                component_transform = (scale, 0.0, 0.0, scale);
            } else if flags & 0x0040 != 0 {
                let x_scale = read_f2dot14(&self.data, cursor).ok()?;
                let y_scale = read_f2dot14(&self.data, cursor + 2).ok()?;
                cursor += 4;
                component_transform = (x_scale, 0.0, 0.0, y_scale);
            } else if flags & 0x0080 != 0 {
                component_transform = (
                    read_f2dot14(&self.data, cursor).ok()?,
                    read_f2dot14(&self.data, cursor + 2).ok()?,
                    read_f2dot14(&self.data, cursor + 4).ok()?,
                    read_f2dot14(&self.data, cursor + 6).ok()?,
                );
                cursor += 8;
            }
            components.push(Component {
                glyph: component,
                offset: (argument_x, argument_y),
                offset_is_xy: flags & 0x0002 != 0,
                transform: component_transform,
            });
            if flags & 0x0020 == 0 {
                return Some(components);
            }
        }
    }
}

// one glyph a composite places, with its offset (or the point numbers it matches up) and
// its 2x2 transform
struct Component {
    glyph: u16,
    offset: (f32, f32),
    offset_is_xy: bool,
    transform: (f32, f32, f32, f32),
}

// signed-area accumulation, the same approach font-rs uses for antialiased coverage
pub fn rasterize(outline: &Outline, scale: f32, subpixel_x: f32) -> Option<GlyphBitmap> {
    let mut segments = Vec::new();
    for contour in &outline.contours {
        flatten_contour(contour, scale, subpixel_x, &mut segments);
    }
    coverage_bitmap(&segments)
}

// the outline stroked at a device width, as the union of a quad along every flattened edge
// and a disc at every joint
pub fn rasterize_stroke(
    outline: &Outline,
    scale: f32,
    subpixel_x: f32,
    width: f32,
) -> Option<GlyphBitmap> {
    let mut path = Vec::new();
    for contour in &outline.contours {
        flatten_contour(contour, scale, subpixel_x, &mut path);
    }
    let half = width / 2.0;
    let mut segments = Vec::new();
    for &(from, to) in &path {
        let (dx, dy) = (to.0 - from.0, to.1 - from.1);
        let length = dx.hypot(dy);
        if length > f32::EPSILON {
            let normal = (-dy / length * half, dx / length * half);
            push_polygon(
                &mut segments,
                &[
                    (from.0 + normal.0, from.1 + normal.1),
                    (to.0 + normal.0, to.1 + normal.1),
                    (to.0 - normal.0, to.1 - normal.1),
                    (from.0 - normal.0, from.1 - normal.1),
                ],
            );
        }
        let disc = (0..8)
            .map(|step| {
                let angle = step as f32 * std::f32::consts::FRAC_PI_4;
                (to.0 + half * angle.cos(), to.1 + half * angle.sin())
            })
            .collect::<Vec<_>>();
        push_polygon(&mut segments, &disc);
    }
    coverage_bitmap(&segments)
}

// winds every polygon the same way, so where they overlap their coverage adds up instead of
// cancelling
fn push_polygon(segments: &mut Vec<Segment>, points: &[(f32, f32)]) {
    let edges = || {
        points
            .iter()
            .copied()
            .zip(points.iter().copied().cycle().skip(1))
    };
    let area: f32 = edges()
        .map(|(from, to)| from.0 * to.1 - to.0 * from.1)
        .sum();
    segments.extend(edges().map(|edge| if area < 0.0 { (edge.1, edge.0) } else { edge }));
}

fn coverage_bitmap(segments: &[Segment]) -> Option<GlyphBitmap> {
    let mut minimum = (f32::MAX, f32::MAX);
    let mut maximum = (f32::MIN, f32::MIN);
    if segments.is_empty() {
        return None;
    }
    for segment in segments {
        for point in [segment.0, segment.1] {
            minimum.0 = minimum.0.min(point.0);
            minimum.1 = minimum.1.min(point.1);
            maximum.0 = maximum.0.max(point.0);
            maximum.1 = maximum.1.max(point.1);
        }
    }
    let left = minimum.0.floor();
    let top = minimum.1.floor();
    let width = (maximum.0.ceil() - left).max(1.0) as usize + 1;
    let height = (maximum.1.ceil() - top).max(1.0) as usize + 1;
    if width * height > MAX_GLYPH_PIXELS {
        return None;
    }

    let mut area = vec![0.0_f32; width * height + 4];
    for segment in segments {
        accumulate_segment(
            &mut area,
            width,
            height,
            (segment.0.0 - left, segment.0.1 - top),
            (segment.1.0 - left, segment.1.1 - top),
        );
    }

    let mut coverage = vec![0_u8; width * height];
    for row in 0..height {
        let mut accumulator = 0.0_f32;
        for column in 0..width {
            accumulator += area[row * width + column];
            let value = (accumulator.abs().min(1.0) * 255.0).round();
            coverage[row * width + column] = value as u8;
        }
    }

    Some(GlyphBitmap {
        left: left as i32,
        top: top as i32,
        width,
        height,
        coverage,
    })
}

type Segment = ((f32, f32), (f32, f32));

fn flatten_contour(
    contour: &[OutlinePoint],
    scale: f32,
    subpixel_x: f32,
    segments: &mut Vec<Segment>,
) {
    let points = resolve_implied_points(contour);
    if points.len() < 2 {
        return;
    }
    // font units grow upwards, device rows grow downwards
    let place = |point: &OutlinePoint| (point.x * scale + subpixel_x, -point.y * scale);
    let mut index = 0;
    let start = points[0];
    let mut current = place(&start);
    while index < points.len() {
        let point = points[(index + 1) % points.len()];
        if point.on_curve {
            let next = place(&point);
            segments.push((current, next));
            current = next;
            index += 1;
            continue;
        }
        let after = points[(index + 2) % points.len()];
        let control = place(&point);
        let next = place(&after);
        flatten_quadratic(current, control, next, segments);
        current = next;
        index += 2;
    }
    let first = place(&start);
    if current != first {
        segments.push((current, first));
    }
}

fn resolve_implied_points(contour: &[OutlinePoint]) -> Vec<OutlinePoint> {
    if contour.is_empty() {
        return Vec::new();
    }
    let mut points = Vec::with_capacity(contour.len() + 2);
    // truetype contours may start off curve, so rotate to an on-curve point first
    let start = contour
        .iter()
        .position(|point| point.on_curve)
        .unwrap_or_default();
    let ordered: Vec<OutlinePoint> = contour[start..]
        .iter()
        .chain(contour[..start].iter())
        .copied()
        .collect();
    if !ordered[0].on_curve {
        let last = ordered[ordered.len() - 1];
        points.push(OutlinePoint {
            x: (ordered[0].x + last.x) / 2.0,
            y: (ordered[0].y + last.y) / 2.0,
            on_curve: true,
        });
    }
    for (index, point) in ordered.iter().enumerate() {
        if index > 0 {
            let previous = ordered[index - 1];
            if !previous.on_curve && !point.on_curve {
                points.push(OutlinePoint {
                    x: (previous.x + point.x) / 2.0,
                    y: (previous.y + point.y) / 2.0,
                    on_curve: true,
                });
            }
        }
        points.push(*point);
    }
    points
}

fn flatten_quadratic(
    from: (f32, f32),
    control: (f32, f32),
    to: (f32, f32),
    segments: &mut Vec<Segment>,
) {
    let deviation =
        (control.0 - (from.0 + to.0) / 2.0).abs() + (control.1 - (from.1 + to.1) / 2.0).abs();
    let steps = ((deviation * 0.5).sqrt().ceil() as usize).clamp(1, 16);
    let mut previous = from;
    for step in 1..=steps {
        let t = step as f32 / steps as f32;
        let inverse = 1.0 - t;
        let point = (
            inverse * inverse * from.0 + 2.0 * inverse * t * control.0 + t * t * to.0,
            inverse * inverse * from.1 + 2.0 * inverse * t * control.1 + t * t * to.1,
        );
        segments.push((previous, point));
        previous = point;
    }
}

fn accumulate_segment(
    area: &mut [f32],
    width: usize,
    height: usize,
    from: (f32, f32),
    to: (f32, f32),
) {
    if (from.1 - to.1).abs() < f32::EPSILON {
        return;
    }
    let (direction, top, bottom) = if from.1 < to.1 {
        (1.0_f32, from, to)
    } else {
        (-1.0_f32, to, from)
    };
    let slope = (bottom.0 - top.0) / (bottom.1 - top.1);
    let first_row = top.1.floor().max(0.0) as usize;
    let last_row = bottom.1.ceil().min(height as f32) as usize;
    for row in first_row..last_row {
        let row_top = (row as f32).max(top.1);
        let row_bottom = ((row + 1) as f32).min(bottom.1);
        let height_in_row = row_bottom - row_top;
        if height_in_row <= 0.0 {
            continue;
        }
        let x_start = top.0 + (row_top - top.1) * slope;
        let x_end = top.0 + (row_bottom - top.1) * slope;
        let (left_x, right_x) = if x_start <= x_end {
            (x_start, x_end)
        } else {
            (x_end, x_start)
        };
        let first_column = left_x.floor().max(0.0) as usize;
        let last_column = right_x.floor().min((width - 1) as f32).max(0.0) as usize;
        if first_column >= width {
            continue;
        }
        if first_column == last_column {
            let center = (left_x + right_x) / 2.0;
            let coverage = (1.0 - (center - first_column as f32).clamp(0.0, 1.0)) * height_in_row;
            area[row * width + first_column] += direction * coverage;
            if first_column + 1 < width {
                area[row * width + first_column + 1] += direction * (height_in_row - coverage);
            }
            continue;
        }
        let span = right_x - left_x;
        for column in first_column..=last_column {
            let cell_left = (column as f32).max(left_x);
            let cell_right = ((column + 1) as f32).min(right_x);
            let covered = ((cell_right - cell_left) / span).clamp(0.0, 1.0) * height_in_row;
            let center = (cell_left + cell_right) / 2.0;
            let inside = (1.0 - (center - column as f32).clamp(0.0, 1.0)) * covered;
            area[row * width + column] += direction * inside;
            if column + 1 < width {
                area[row * width + column + 1] += direction * (covered - inside);
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct GlyphKey {
    face: usize,
    glyph: u16,
    size: u32,
    synthetic_bold: bool,
    synthetic_italic: bool,
    // the pen's fraction of a pixel, in quarters
    quarter: u8,
    // the device width of the outline's stroke, or zero for the filled glyph
    stroke: u32,
}

// shaped runs kept before the cache is emptied
const MAX_SHAPED_RUNS: usize = 16_384;

// a glyph of a shaped run: its face (none for the builtin bitmap face), its pen position
// from the run's start and its advance, in pixels
#[derive(Clone)]
pub(crate) struct RunGlyph {
    pub(crate) face: Option<ChosenFace>,
    pub(crate) glyph: u16,
    pub(crate) x: f32,
    pub(crate) advance: f32,
}

pub(crate) struct ShapedRun {
    pub(crate) glyphs: Vec<RunGlyph>,
    pub(crate) width: f32,
}

type ShapeKey = (String, FontStack, u32, FontStyle, bool);

// the weight and slant text asks for, and how it tunes a variable face's axes
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FontStyle {
    pub weight: u16,
    pub italic: bool,
    pub variations: FontVariations,
    // the opsz font-optical-sizing auto asks for, the font size in css pixels as f32 bits, or
    // zero when optical sizing is off
    pub optical_size: u32,
}

impl FontStyle {
    pub const NORMAL: Self = Self {
        weight: 400,
        italic: false,
        variations: FontVariations(0),
        optical_size: 0,
    };

    pub fn is_bold(self) -> bool {
        self.weight >= 600
    }
}

impl Default for FontStyle {
    fn default() -> Self {
        Self::NORMAL
    }
}

// a css font-family list, interned so inherited style values stay copyable
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct FontStack(u32);

const MAX_FONT_STACKS: usize = 256;

thread_local! {
    static FONT_STACKS: RefCell<FontStackTable> = RefCell::new(FontStackTable::default());
}

#[derive(Default)]
struct FontStackTable {
    families: Vec<Rc<[String]>>,
    ids: HashMap<String, FontStack>,
}

pub fn intern_font_stack(source: &str) -> FontStack {
    let families: Vec<String> = source
        .split(',')
        .map(|family| {
            family
                .trim()
                .trim_matches(['"', '\''])
                .trim()
                .to_ascii_lowercase()
        })
        .filter(|family| !family.is_empty())
        .collect();
    if families.is_empty() {
        return FontStack::default();
    }
    let key = families.join(",");
    FONT_STACKS.with(|table| {
        let mut table = table.borrow_mut();
        if let Some(stack) = table.ids.get(&key) {
            return *stack;
        }
        if table.families.len() >= MAX_FONT_STACKS {
            return FontStack::default();
        }
        let stack = FontStack(table.families.len() as u32 + 1);
        table.families.push(families.into());
        table.ids.insert(key, stack);
        stack
    })
}

pub fn font_stack_families(stack: FontStack) -> Option<Rc<[String]>> {
    if stack.0 == 0 {
        return None;
    }
    FONT_STACKS.with(|table| table.borrow().families.get(stack.0 as usize - 1).cloned())
}

// a font-variation-settings list, interned like font stacks; the default is normal
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct FontVariations(u32);

type AxisSettings = Rc<[([u8; 4], f32)]>;

thread_local! {
    static FONT_VARIATIONS: RefCell<(Vec<AxisSettings>, HashMap<String, FontVariations>)> =
        RefCell::default();
}

// font-variation-settings: normal, or a list of quoted four letter axis tags and values
pub(crate) fn parse_font_variations(value: &str) -> Option<FontVariations> {
    let value = value.trim();
    if value.eq_ignore_ascii_case("normal") {
        return Some(FontVariations::default());
    }
    let settings = value
        .split(',')
        .map(|setting| {
            let setting = setting.trim();
            let quote = setting
                .chars()
                .next()
                .filter(|&quote| quote == '"' || quote == '\'')?;
            let (tag, number) = setting[1..].split_once(quote)?;
            let tag: [u8; 4] = tag.as_bytes().try_into().ok()?;
            let number = number
                .trim()
                .parse::<f32>()
                .ok()
                .filter(|number| number.is_finite())?;
            (tag.iter().all(|byte| (0x20..=0x7E).contains(byte))).then_some((tag, number))
        })
        .collect::<Option<Vec<_>>>()?;
    let key = settings
        .iter()
        .map(|(tag, number)| format!("{}={number}", String::from_utf8_lossy(tag)))
        .collect::<Vec<_>>()
        .join(",");
    FONT_VARIATIONS.with(|table| {
        let (lists, ids) = &mut *table.borrow_mut();
        if let Some(variations) = ids.get(&key) {
            return Some(*variations);
        }
        if lists.len() >= MAX_FONT_STACKS {
            return None;
        }
        lists.push(settings.into());
        let variations = FontVariations(lists.len() as u32);
        ids.insert(key, variations);
        Some(variations)
    })
}

fn font_variation_values(variations: FontVariations) -> AxisSettings {
    if variations.0 == 0 {
        return Rc::from([]);
    }
    FONT_VARIATIONS.with(|table| {
        table
            .borrow()
            .0
            .get(variations.0 as usize - 1)
            .cloned()
            .unwrap_or_else(|| Rc::from([]))
    })
}

// the host owns font discovery, the engine only asks for a family by name and style
pub trait FontSource {
    fn face(&self, family: &str, style: FontStyle) -> Option<Rc<FontFace>>;

    // a face that can draw a character none of the requested families have
    fn fallback(&self, _character: char, _style: FontStyle) -> Option<Rc<FontFace>> {
        None
    }

    // the platform's colour emoji face
    fn emoji(&self, _style: FontStyle) -> Option<Rc<FontFace>> {
        None
    }
}

struct SingleFaceSource {
    regular: Rc<FontFace>,
    bold: Option<Rc<FontFace>>,
}

impl FontSource for SingleFaceSource {
    fn face(&self, _family: &str, style: FontStyle) -> Option<Rc<FontFace>> {
        if style.is_bold() {
            return self.bold.clone().or_else(|| Some(self.regular.clone()));
        }
        Some(self.regular.clone())
    }
}

// a face loaded from an @font-face rule, for one family, weight range and slant
struct WebFace {
    family: String,
    weights: (u16, u16),
    italic: bool,
    // code point ranges the face is used for, all of them when empty
    ranges: Rc<[(u32, u32)]>,
    face: Rc<FontFace>,
}

// a face in a stack, limited to an @font-face unicode-range when it has one, and the weights
// it is used for
#[derive(Clone)]
struct StackFace {
    face: Rc<FontFace>,
    ranges: Rc<[(u32, u32)]>,
    weights: (u16, u16),
}

impl StackFace {
    fn covers(&self, character: char) -> bool {
        let point = u32::from(character);
        (self.ranges.is_empty()
            || self
                .ranges
                .iter()
                .any(|&(first, last)| (first..=last).contains(&point)))
            && self.face.has_glyph(character)
    }
}

// a face with the synthesis it needs to stand in for the style asked for
#[derive(Clone)]
pub(crate) struct ChosenFace {
    pub(crate) face: Rc<FontFace>,
    pub(crate) synthetic_bold: bool,
    pub(crate) synthetic_italic: bool,
}

impl ChosenFace {
    // a variable face draws at the instance the style picks within the weights it is used for
    fn new(face: &Rc<FontFace>, style: FontStyle, weights: (u16, u16)) -> Self {
        let face = face.instance(style, weights);
        Self {
            synthetic_bold: style.weight >= 600 && face.weight() <= 500,
            synthetic_italic: style.italic && !face.is_italic(),
            face,
        }
    }

    // emboldening widens each glyph by a twenty-fourth of the em
    fn advance(&self, character: char, size: f32) -> f32 {
        let advance = self.face.advance(self.face.glyph_for(character), size);
        if self.synthetic_bold {
            advance + size / 24.0
        } else {
            advance
        }
    }
}

type FaceList = Rc<[StackFace]>;

// the face each missing character fell back to, by style
type FallbackCache = HashMap<(char, FontStyle), Option<Rc<FontFace>>>;
// colour glyphs by glyph and the text colour their foreground layers take
type ColorGlyphCache = HashMap<(GlyphKey, [u8; 4]), Option<Rc<ColorGlyph>>>;

// text metrics and rasterized glyphs for the whole engine, cached per face and size
pub struct FontSet {
    source: Option<Rc<dyn FontSource>>,
    web_faces: RefCell<Vec<WebFace>>,
    // the faces a stack's families supply, in order
    stacks: RefCell<HashMap<(FontStack, FontStyle), FaceList>>,
    fallbacks: RefCell<FallbackCache>,
    shaped: RefCell<HashMap<ShapeKey, Rc<ShapedRun>>>,
    glyphs: RefCell<HashMap<GlyphKey, Option<Rc<GlyphBitmap>>>>,
    color_glyphs: RefCell<ColorGlyphCache>,
}

impl Default for FontSet {
    fn default() -> Self {
        Self::builtin()
    }
}

impl FontSet {
    // the bitmap fallback keeps deterministic metrics when no font file is supplied
    pub fn builtin() -> Self {
        Self {
            source: None,
            web_faces: RefCell::default(),
            stacks: RefCell::default(),
            fallbacks: RefCell::default(),
            shaped: RefCell::default(),
            glyphs: RefCell::default(),
            color_glyphs: RefCell::default(),
        }
    }

    pub fn from_source(source: Rc<dyn FontSource>) -> Self {
        Self {
            source: Some(source),
            ..Self::builtin()
        }
    }

    pub fn new(regular: Rc<FontFace>, bold: Option<Rc<FontFace>>) -> Self {
        Self::from_source(Rc::new(SingleFaceSource { regular, bold }))
    }

    pub fn is_builtin(&self) -> bool {
        self.source.is_none() && self.web_faces.borrow().is_empty()
    }

    // registers a face from an @font-face rule; it is chosen before installed faces, and a
    // rule without font-weight covers the weights the face itself can draw
    pub(crate) fn add_web_face(
        &self,
        family: &str,
        weights: Option<(u16, u16)>,
        italic: bool,
        ranges: Vec<(u32, u32)>,
        face: Rc<FontFace>,
    ) {
        self.web_faces.borrow_mut().push(WebFace {
            family: family.to_ascii_lowercase(),
            weights: weights.unwrap_or_else(|| face.weight_range()),
            italic,
            ranges: ranges.into(),
            face,
        });
        self.stacks.borrow_mut().clear();
        self.fallbacks.borrow_mut().clear();
        self.shaped.borrow_mut().clear();
    }

    pub(crate) fn clear_web_faces(&self) {
        if !self.web_faces.borrow().is_empty() {
            self.web_faces.borrow_mut().clear();
            self.stacks.borrow_mut().clear();
            self.fallbacks.borrow_mut().clear();
            self.shaped.borrow_mut().clear();
            self.glyphs.borrow_mut().clear();
            self.color_glyphs.borrow_mut().clear();
        }
    }

    pub fn measure(&self, text: &str, stack: FontStack, size: f32, style: FontStyle) -> f32 {
        if self.is_builtin() {
            return text.chars().count() as f32 * builtin_advance(size);
        }
        self.shape(text, stack, size, style, true).width
    }

    // the glyphs a run of text draws as, with ligatures when allowed and kerning, positioned in
    // logical order from the run's start
    pub(crate) fn shape(
        &self,
        text: &str,
        stack: FontStack,
        size: f32,
        style: FontStyle,
        ligatures: bool,
    ) -> Rc<ShapedRun> {
        let key = (text.to_string(), stack, size.to_bits(), style, ligatures);
        if let Some(cached) = self.shaped.borrow().get(&key) {
            return cached.clone();
        }
        let characters = text.chars().collect::<Vec<_>>();
        let mut faces: Vec<Option<ChosenFace>> = Vec::with_capacity(characters.len());
        for (index, &character) in characters.iter().enumerate() {
            // a selector or joiner stays in its base character's run, so the face sees the sequence
            let face = if (crate::color_glyph::is_variation_selector(character)
                || character == '\u{200D}')
                && index > 0
            {
                faces[index - 1].clone()
            } else {
                let presentation = characters.get(index + 1).copied();
                let emoji = presentation != Some('\u{FE0E}')
                    && (presentation == Some('\u{FE0F}')
                        || crate::color_glyph::prefers_emoji(character));
                emoji
                    .then(|| self.emoji_face(character, style))
                    .flatten()
                    .or_else(|| self.face_for(character, stack, style))
            };
            faces.push(face);
        }
        let mut glyphs = Vec::with_capacity(characters.len());
        let mut x = 0.0;
        let mut start = 0;
        while start < characters.len() {
            let chosen = faces[start].clone();
            // a run of characters the same face draws shapes together
            let mut end = start + 1;
            while end < characters.len() {
                let next = &faces[end];
                let same = match (&chosen, next) {
                    (Some(left), Some(right)) => {
                        Rc::ptr_eq(&left.face, &right.face)
                            && left.synthetic_bold == right.synthetic_bold
                            && left.synthetic_italic == right.synthetic_italic
                    }
                    (None, None) => true,
                    _ => false,
                };
                if !same {
                    break;
                }
                end += 1;
            }
            match chosen {
                Some(chosen) => {
                    let scale = size / chosen.face.units_per_em();
                    let extra = if chosen.synthetic_bold {
                        size / 24.0
                    } else {
                        0.0
                    };
                    // variation selectors only choose the face and take no room
                    let visible = characters[start..end]
                        .iter()
                        .copied()
                        .filter(|&character| !crate::color_glyph::is_variation_selector(character))
                        .collect::<Vec<_>>();
                    for shaped in
                        crate::shaping::Shaper::new(&chosen.face).shape(&visible, ligatures)
                    {
                        let advance = shaped.advance * scale + extra;
                        glyphs.push(RunGlyph {
                            face: Some(chosen.clone()),
                            glyph: shaped.glyph,
                            x,
                            advance,
                        });
                        x += advance;
                    }
                }
                None => {
                    let visible = characters[start..end].iter().filter(|&&character| {
                        !crate::color_glyph::is_variation_selector(character)
                    });
                    for _ in visible {
                        glyphs.push(RunGlyph {
                            face: None,
                            glyph: 0,
                            x,
                            advance: builtin_advance(size),
                        });
                        x += builtin_advance(size);
                    }
                }
            }
            start = end;
        }
        let run = Rc::new(ShapedRun { glyphs, width: x });
        let mut shaped = self.shaped.borrow_mut();
        if shaped.len() >= MAX_SHAPED_RUNS {
            shaped.clear();
        }
        shaped.insert(key, run.clone());
        run
    }

    pub fn advance(&self, character: char, stack: FontStack, size: f32, style: FontStyle) -> f32 {
        match self.face_for(character, stack, style) {
            Some(chosen) => chosen.advance(character, size),
            None => builtin_advance(size),
        }
    }

    pub fn ascent(&self, stack: FontStack, size: f32) -> f32 {
        match self.primary(stack, FontStyle::NORMAL) {
            Some(face) => face.ascent(size),
            None => size,
        }
    }

    pub fn x_height(&self, stack: FontStack, size: f32) -> f32 {
        match self.primary(stack, FontStyle::NORMAL) {
            Some(face) => face.x_height(size),
            None => size / 2.0,
        }
    }

    // the height of a glyph box, ascent plus descent, which half-leading centres in a line
    pub fn content_height(&self, stack: FontStack, size: f32) -> f32 {
        match self.primary(stack, FontStyle::NORMAL) {
            Some(face) => face.ascent(size) + face.descent(size),
            None => size,
        }
    }

    pub fn line_height(&self, stack: FontStack, size: f32) -> f32 {
        match self.primary(stack, FontStyle::NORMAL) {
            Some(face) => face.line_height(size),
            None => size * 1.25,
        }
    }

    pub fn glyph(
        &self,
        character: char,
        stack: FontStack,
        size: f32,
        style: FontStyle,
    ) -> Option<Rc<GlyphBitmap>> {
        let chosen = self.face_for(character, stack, style)?;
        self.glyph_bitmap(&chosen, chosen.face.glyph_for(character), size, 0.0, 0.0)
    }

    // a glyph rasterized at a size, at a pen position's fraction of a pixel (to a quarter),
    // filled, or stroked along its outline when the stroke width is above zero
    pub(crate) fn glyph_bitmap(
        &self,
        chosen: &ChosenFace,
        glyph: u16,
        size: f32,
        subpixel: f32,
        stroke: f32,
    ) -> Option<Rc<GlyphBitmap>> {
        let quarter = (((subpixel - subpixel.floor()) * 4.0).round() as u8) % 4;
        let key = GlyphKey {
            face: Rc::as_ptr(&chosen.face) as usize,
            glyph,
            size: size.to_bits(),
            synthetic_bold: chosen.synthetic_bold,
            synthetic_italic: chosen.synthetic_italic,
            quarter,
            stroke: stroke.to_bits(),
        };
        if let Some(cached) = self.glyphs.borrow().get(&key) {
            return cached.clone();
        }
        let bitmap = chosen
            .face
            .outline(glyph)
            .map(|outline| {
                if chosen.synthetic_italic {
                    oblique(outline)
                } else {
                    outline
                }
            })
            .and_then(|outline| {
                let scale = size / chosen.face.units_per_em();
                let subpixel = f32::from(quarter) / 4.0;
                if stroke > 0.0 {
                    rasterize_stroke(&outline, scale, subpixel, stroke)
                } else {
                    rasterize(&outline, scale, subpixel)
                }
            })
            .map(|bitmap| {
                if chosen.synthetic_bold && stroke <= 0.0 {
                    embolden(bitmap, (size / 24.0).round().max(1.0) as usize)
                } else {
                    bitmap
                }
            })
            .map(Rc::new);
        self.glyphs.borrow_mut().insert(key, bitmap.clone());
        bitmap
    }

    // a glyph in its font's own colours, for faces with COLR layers or CBDT bitmaps
    pub(crate) fn color_glyph(
        &self,
        chosen: &ChosenFace,
        glyph: u16,
        size: f32,
        subpixel: f32,
        foreground: crate::css::Color,
    ) -> Option<Rc<ColorGlyph>> {
        if !crate::color_glyph::has_color(&chosen.face) {
            return None;
        }
        let quarter = (((subpixel - subpixel.floor()) * 4.0).round() as u8) % 4;
        let key = GlyphKey {
            face: Rc::as_ptr(&chosen.face) as usize,
            glyph,
            size: size.to_bits(),
            synthetic_bold: false,
            synthetic_italic: false,
            quarter,
            stroke: 0,
        };
        let color = [
            foreground.red,
            foreground.green,
            foreground.blue,
            foreground.alpha,
        ];
        if let Some(cached) = self.color_glyphs.borrow().get(&(key, color)) {
            return cached.clone();
        }
        let rendered = crate::color_glyph::render(
            &chosen.face,
            glyph,
            size,
            f32::from(quarter) / 4.0,
            foreground,
        )
        .map(Rc::new);
        self.color_glyphs
            .borrow_mut()
            .insert((key, color), rendered.clone());
        rendered
    }

    // the face that draws a character: the first family in the stack that has it, then an
    // installed face that does, then the first family anyway
    pub(crate) fn face_for(
        &self,
        character: char,
        stack: FontStack,
        style: FontStyle,
    ) -> Option<ChosenFace> {
        let faces = self.stack_faces(stack, style);
        if let Some(entry) = faces.iter().find(|entry| entry.covers(character)) {
            return Some(ChosenFace::new(&entry.face, style, entry.weights));
        }
        if !character.is_whitespace()
            && !character.is_control()
            && let Some(face) = self.fallback(character, style)
        {
            return Some(ChosenFace::new(&face, style, face.weight_range()));
        }
        faces
            .first()
            .map(|entry| ChosenFace::new(&entry.face, style, entry.weights))
    }

    fn emoji_face(&self, character: char, style: FontStyle) -> Option<ChosenFace> {
        let face = self.source.as_ref()?.emoji(style)?;
        face.has_glyph(character)
            .then(|| ChosenFace::new(&face, FontStyle::NORMAL, face.weight_range()))
    }

    fn primary(&self, stack: FontStack, style: FontStyle) -> Option<Rc<FontFace>> {
        self.stack_faces(stack, style)
            .first()
            .map(|entry| entry.face.clone())
    }

    fn fallback(&self, character: char, style: FontStyle) -> Option<Rc<FontFace>> {
        if let Some(cached) = self.fallbacks.borrow().get(&(character, style)) {
            return cached.clone();
        }
        let face = self
            .web_faces
            .borrow()
            .iter()
            .find(|web| web.face.has_glyph(character))
            .map(|web| web.face.clone())
            .or_else(|| self.source.as_ref()?.fallback(character, style));
        self.fallbacks
            .borrow_mut()
            .insert((character, style), face.clone());
        face
    }

    fn stack_faces(&self, stack: FontStack, style: FontStyle) -> FaceList {
        let style = FontStyle {
            variations: FontVariations::default(),
            optical_size: 0,
            ..style
        };
        if let Some(cached) = self.stacks.borrow().get(&(stack, style)) {
            return cached.clone();
        }
        let mut faces: Vec<StackFace> = Vec::new();
        let families = font_stack_families(stack);
        let mut push = |entry: StackFace| {
            if !faces
                .iter()
                .any(|known| Rc::ptr_eq(&known.face, &entry.face) && known.ranges == entry.ranges)
            {
                faces.push(entry);
            }
        };
        for family in families
            .as_deref()
            .unwrap_or(&[])
            .iter()
            .map(String::as_str)
            .chain(["sans-serif"])
        {
            for entry in self.web_faces_for(family, style) {
                push(entry);
            }
            if let Some(face) = self
                .source
                .as_ref()
                .and_then(|source| source.face(family, style))
            {
                push(StackFace {
                    weights: face.weight_range(),
                    face,
                    ranges: Rc::from([]),
                });
            }
        }
        let faces: FaceList = faces.into();
        self.stacks
            .borrow_mut()
            .insert((stack, style), faces.clone());
        faces
    }

    // the @font-face faces of a family, nearest in slant and weight first, each covering only
    // its unicode-range
    fn web_faces_for(&self, family: &str, style: FontStyle) -> Vec<StackFace> {
        let web_faces = self.web_faces.borrow();
        let mut matching = web_faces
            .iter()
            .filter(|web| web.family == family)
            .collect::<Vec<_>>();
        let distance = |web: &&WebFace| {
            let (low, high) = web.weights;
            let weight = if style.weight < low {
                low - style.weight
            } else {
                style.weight.saturating_sub(high)
            };
            (u16::from(web.italic != style.italic), weight)
        };
        matching.sort_by_key(distance);
        matching
            .into_iter()
            .map(|web| StackFace {
                face: web.face.clone(),
                ranges: web.ranges.clone(),
                weights: web.weights,
            })
            .collect()
    }
}

// slants an outline as a synthetic italic does, about a fifth of the height
fn oblique(mut outline: Outline) -> Outline {
    for contour in &mut outline.contours {
        for point in contour {
            point.x += point.y * 0.2;
        }
    }
    outline
}

// thickens a glyph to the right, as a synthetic bold does
fn embolden(bitmap: GlyphBitmap, strength: usize) -> GlyphBitmap {
    let width = bitmap.width + strength;
    let mut coverage = vec![0_u8; width * bitmap.height];
    for row in 0..bitmap.height {
        for column in 0..bitmap.width {
            let value = bitmap.coverage[row * bitmap.width + column];
            for shift in 0..=strength {
                let target = &mut coverage[row * width + column + shift];
                *target = (*target).max(value);
            }
        }
    }
    GlyphBitmap {
        width,
        coverage,
        ..bitmap
    }
}

pub fn builtin_advance(size: f32) -> f32 {
    (size / 9.0).round().max(1.0) * 6.0
}

thread_local! {
    static ACTIVE_FONTS: RefCell<Option<Rc<FontSet>>> = const { RefCell::new(None) };
}

pub(crate) struct ActiveFonts {
    previous: Option<Rc<FontSet>>,
}

impl Drop for ActiveFonts {
    fn drop(&mut self) {
        ACTIVE_FONTS.with(|active| *active.borrow_mut() = self.previous.take());
    }
}

// layout and paint read the font set of the pass they run in, per engine thread
pub(crate) fn use_fonts(fonts: Rc<FontSet>) -> ActiveFonts {
    let previous = ACTIVE_FONTS.with(|active| active.borrow_mut().replace(fonts));
    ActiveFonts { previous }
}

pub(crate) fn active_fonts() -> Rc<FontSet> {
    ACTIVE_FONTS.with(|active| {
        let mut active = active.borrow_mut();
        active
            .get_or_insert_with(|| Rc::new(FontSet::builtin()))
            .clone()
    })
}

fn read_table_directory(data: &[u8]) -> Result<BTreeMap<[u8; 4], (usize, usize)>, String> {
    let version = read_u32(data, 0).ok_or("font file is too short")?;
    let offset = match version {
        0x0001_0000 | 0x7472_7565 => 0,
        0x7474_6366 => read_u32(data, 12).ok_or("font collection has no first face")? as usize,
        0x4f54_544f => 0,
        other => return Err(format!("unsupported font format {other:#x}")),
    };
    let table_count = read_u16(data, offset + 4).map_err(|error| error.to_string())?;
    let mut tables = BTreeMap::new();
    for index in 0..usize::from(table_count) {
        let record = offset + 12 + index * 16;
        let tag = data
            .get(record..record + 4)
            .ok_or("font table directory is truncated")?;
        let start = read_u32(data, record + 8).ok_or("font table has no offset")? as usize;
        let length = read_u32(data, record + 12).ok_or("font table has no length")? as usize;
        let end = start.checked_add(length).ok_or("font table overflows")?;
        if end > data.len() {
            continue;
        }
        tables.insert([tag[0], tag[1], tag[2], tag[3]], (start, end));
    }
    Ok(tables)
}

fn read_character_map(data: &[u8], cmap: usize) -> Result<BTreeMap<u32, u16>, String> {
    let table_count = read_u16(data, cmap + 2).map_err(|error| error.to_string())?;
    let mut best: Option<(u8, usize)> = None;
    for index in 0..usize::from(table_count) {
        let record = cmap + 4 + index * 8;
        let platform = read_u16(data, record).map_err(|error| error.to_string())?;
        let encoding = read_u16(data, record + 2).map_err(|error| error.to_string())?;
        let offset = read_u32(data, record + 4).ok_or("cmap record has no offset")? as usize;
        let rank = match (platform, encoding) {
            (3, 10) | (0, 4) | (0, 6) => 3,
            (3, 1) | (0, 3) => 2,
            (0, 0) | (0, 1) | (0, 2) | (3, 0) => 1,
            _ => continue,
        };
        if best.is_none_or(|(current, _)| rank > current) {
            best = Some((rank, cmap + offset));
        }
    }
    let (_, subtable) = best.ok_or("cmap has no supported subtable")?;
    match read_u16(data, subtable).map_err(|error| error.to_string())? {
        4 => read_character_map_format4(data, subtable),
        12 => read_character_map_format12(data, subtable),
        6 => read_character_map_format6(data, subtable),
        other => Err(format!("unsupported cmap format {other}")),
    }
}

fn read_character_map_format4(data: &[u8], subtable: usize) -> Result<BTreeMap<u32, u16>, String> {
    let segment_bytes = read_u16(data, subtable + 6).map_err(|error| error.to_string())?;
    let segments = usize::from(segment_bytes / 2);
    let ends = subtable + 14;
    let starts = ends + segments * 2 + 2;
    let deltas = starts + segments * 2;
    let ranges = deltas + segments * 2;
    let mut characters = BTreeMap::new();
    for segment in 0..segments {
        let end = read_u16(data, ends + segment * 2).map_err(|error| error.to_string())?;
        let start = read_u16(data, starts + segment * 2).map_err(|error| error.to_string())?;
        if start > end {
            continue;
        }
        let delta = read_u16(data, deltas + segment * 2).map_err(|error| error.to_string())?;
        let range_offset_position = ranges + segment * 2;
        let range_offset =
            read_u16(data, range_offset_position).map_err(|error| error.to_string())?;
        for code in start..=end {
            if code == 0xffff {
                continue;
            }
            let glyph = if range_offset == 0 {
                code.wrapping_add(delta)
            } else {
                let index = range_offset_position
                    + usize::from(range_offset)
                    + usize::from(code - start) * 2;
                let glyph = read_u16(data, index).unwrap_or(0);
                if glyph == 0 {
                    continue;
                }
                glyph.wrapping_add(delta)
            };
            if glyph != 0 {
                characters.insert(u32::from(code), glyph);
            }
        }
    }
    Ok(characters)
}

fn read_character_map_format6(data: &[u8], subtable: usize) -> Result<BTreeMap<u32, u16>, String> {
    let first = read_u16(data, subtable + 6).map_err(|error| error.to_string())?;
    let count = read_u16(data, subtable + 8).map_err(|error| error.to_string())?;
    let mut characters = BTreeMap::new();
    for index in 0..count {
        let glyph = read_u16(data, subtable + 10 + usize::from(index) * 2)
            .map_err(|error| error.to_string())?;
        if glyph != 0 {
            characters.insert(u32::from(first + index), glyph);
        }
    }
    Ok(characters)
}

fn read_character_map_format12(data: &[u8], subtable: usize) -> Result<BTreeMap<u32, u16>, String> {
    let groups = read_u32(data, subtable + 12).ok_or("cmap format 12 has no group count")?;
    let mut characters = BTreeMap::new();
    for index in 0..groups as usize {
        let group = subtable + 16 + index * 12;
        let start = read_u32(data, group).ok_or("cmap group has no start")?;
        let end = read_u32(data, group + 4).ok_or("cmap group has no end")?;
        let glyph = read_u32(data, group + 8).ok_or("cmap group has no glyph")?;
        if end < start || end - start > 0x10_000 {
            continue;
        }
        for code in start..=end {
            let mapped = glyph + (code - start);
            if mapped <= u32::from(u16::MAX) && mapped != 0 {
                characters.insert(code, mapped as u16);
            }
        }
    }
    Ok(characters)
}

fn read_u16(data: &[u8], offset: usize) -> Result<u16, String> {
    data.get(offset..offset + 2)
        .map(|bytes| u16::from_be_bytes([bytes[0], bytes[1]]))
        .ok_or_else(|| format!("font read past end at {offset}"))
}

fn read_i16(data: &[u8], offset: usize) -> Result<i16, String> {
    read_u16(data, offset).map(|value| value as i16)
}

fn read_u32(data: &[u8], offset: usize) -> Option<u32> {
    data.get(offset..offset + 4)
        .map(|bytes| u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn read_f2dot14(data: &[u8], offset: usize) -> Result<f32, String> {
    read_i16(data, offset).map(|value| f32::from(value) / 16384.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    // a one-glyph font built in memory, so the parser is tested without a font file
    fn synthetic_font() -> Vec<u8> {
        synthetic_font_with(Vec::new())
    }

    fn synthetic_font_with(extra: Vec<(&[u8; 4], Vec<u8>)>) -> Vec<u8> {
        let head = {
            let mut table = vec![0_u8; 54];
            table[18..20].copy_from_slice(&1000_u16.to_be_bytes());
            table[50..52].copy_from_slice(&0_u16.to_be_bytes());
            table
        };
        let hhea = {
            let mut table = vec![0_u8; 36];
            table[4..6].copy_from_slice(&800_i16.to_be_bytes());
            table[6..8].copy_from_slice(&(-200_i16).to_be_bytes());
            table[8..10].copy_from_slice(&100_i16.to_be_bytes());
            table[34..36].copy_from_slice(&2_u16.to_be_bytes());
            table
        };
        let maxp = {
            let mut table = vec![0_u8; 32];
            table[0..4].copy_from_slice(&0x0001_0000_u32.to_be_bytes());
            table[4..6].copy_from_slice(&2_u16.to_be_bytes());
            table
        };
        let hmtx = {
            let mut table = Vec::new();
            table.extend_from_slice(&500_u16.to_be_bytes());
            table.extend_from_slice(&0_i16.to_be_bytes());
            table.extend_from_slice(&600_u16.to_be_bytes());
            table.extend_from_slice(&0_i16.to_be_bytes());
            table
        };
        // glyph 1 is a filled square from (100,0) to (600,700)
        let square = {
            let mut glyph = Vec::new();
            glyph.extend_from_slice(&1_i16.to_be_bytes());
            glyph.extend_from_slice(&100_i16.to_be_bytes());
            glyph.extend_from_slice(&0_i16.to_be_bytes());
            glyph.extend_from_slice(&600_i16.to_be_bytes());
            glyph.extend_from_slice(&700_i16.to_be_bytes());
            glyph.extend_from_slice(&3_u16.to_be_bytes());
            glyph.extend_from_slice(&0_u16.to_be_bytes());
            glyph.extend_from_slice(&[0x01, 0x01, 0x01, 0x01]);
            for delta in [100_i16, 500, 0, -500] {
                glyph.extend_from_slice(&delta.to_be_bytes());
            }
            for delta in [0_i16, 0, 700, 0] {
                glyph.extend_from_slice(&delta.to_be_bytes());
            }
            glyph
        };
        let glyf = square.clone();
        let loca = {
            let mut table = Vec::new();
            table.extend_from_slice(&0_u16.to_be_bytes());
            table.extend_from_slice(&0_u16.to_be_bytes());
            table.extend_from_slice(&((glyf.len() / 2) as u16).to_be_bytes());
            table
        };
        let cmap = {
            let mut table = Vec::new();
            table.extend_from_slice(&0_u16.to_be_bytes());
            table.extend_from_slice(&1_u16.to_be_bytes());
            table.extend_from_slice(&3_u16.to_be_bytes());
            table.extend_from_slice(&1_u16.to_be_bytes());
            table.extend_from_slice(&12_u32.to_be_bytes());
            let mut subtable = Vec::new();
            subtable.extend_from_slice(&6_u16.to_be_bytes());
            subtable.extend_from_slice(&12_u16.to_be_bytes());
            subtable.extend_from_slice(&0_u16.to_be_bytes());
            subtable.extend_from_slice(&u16::from(b'A').to_be_bytes());
            subtable.extend_from_slice(&1_u16.to_be_bytes());
            subtable.extend_from_slice(&1_u16.to_be_bytes());
            table.extend_from_slice(&subtable);
            table
        };

        let mut tables: Vec<(&[u8; 4], Vec<u8>)> = vec![
            (b"cmap", cmap),
            (b"glyf", glyf),
            (b"head", head),
            (b"hhea", hhea),
            (b"hmtx", hmtx),
            (b"loca", loca),
            (b"maxp", maxp),
        ];
        tables.extend(extra);
        let mut file = Vec::new();
        file.extend_from_slice(&0x0001_0000_u32.to_be_bytes());
        file.extend_from_slice(&(tables.len() as u16).to_be_bytes());
        file.extend_from_slice(&[0; 6]);
        let mut offset = 12 + tables.len() * 16;
        let mut directory = Vec::new();
        let mut bodies = Vec::new();
        for (tag, body) in &tables {
            directory.extend_from_slice(*tag);
            directory.extend_from_slice(&0_u32.to_be_bytes());
            directory.extend_from_slice(&(offset as u32).to_be_bytes());
            directory.extend_from_slice(&(body.len() as u32).to_be_bytes());
            offset += body.len();
            bodies.extend_from_slice(body);
        }
        file.extend_from_slice(&directory);
        file.extend_from_slice(&bodies);
        file
    }

    #[test]
    fn parses_metrics_character_map_and_outlines() {
        let face = FontFace::parse(synthetic_font()).expect("synthetic font should parse");
        assert_eq!(face.units_per_em(), 1000.0);
        assert_eq!(face.ascent(100.0), 80.0);
        assert_eq!(face.descent(100.0), 20.0);
        assert_eq!(face.line_height(100.0), 110.0);
        let glyph = face.glyph_for('A');
        assert_eq!(glyph, 1);
        assert_eq!(face.glyph_for('B'), 0);
        assert_eq!(face.advance(glyph, 1000.0), 600.0);
        let outline = face.outline(glyph).expect("glyph should have an outline");
        assert_eq!(outline.contours.len(), 1);
        assert_eq!(outline.contours[0].len(), 4);
        assert_eq!(
            (outline.contours[0][0].x, outline.contours[0][0].y),
            (100.0, 0.0)
        );
        assert!(outline.contours[0].iter().all(|point| point.on_curve));
    }

    #[test]
    fn rasterizes_a_square_glyph_with_antialiased_edges() {
        let face = FontFace::parse(synthetic_font()).expect("synthetic font should parse");
        let outline = face
            .outline(face.glyph_for('A'))
            .expect("glyph should have an outline");
        let bitmap = rasterize(&outline, 20.0 / 1000.0, 0.0).expect("glyph should rasterize");
        assert_eq!((bitmap.width, bitmap.height), (11, 15));
        let interior = bitmap.coverage[7 * bitmap.width + 5];
        assert!(interior > 240, "glyph interior should be solid: {interior}");
        let outside = bitmap.coverage[bitmap.width - 1];
        assert_eq!(outside, 0, "pixels past the outline should stay empty");
    }

    #[test]
    fn strokes_a_glyph_along_its_outline_and_leaves_the_inside_empty() {
        let face = FontFace::parse(synthetic_font()).expect("synthetic font should parse");
        let outline = face
            .outline(face.glyph_for('A'))
            .expect("glyph should have an outline");
        let filled = rasterize(&outline, 20.0 / 1000.0, 0.0).expect("glyph should rasterize");
        let stroked =
            rasterize_stroke(&outline, 20.0 / 1000.0, 0.0, 2.0).expect("glyph should stroke");
        // a two pixel stroke reaches one pixel past the outline on every side
        assert_eq!(
            (stroked.left, stroked.top),
            (filled.left - 1, filled.top - 1)
        );
        let at = |x: i32, y: i32| {
            stroked.coverage
                [(y - stroked.top) as usize * stroked.width + (x - stroked.left) as usize]
        };
        let middle = filled.top + filled.height as i32 / 2;
        assert_eq!(at(filled.left + 5, middle), 0, "the inside stays empty");
        assert!(at(filled.left, middle) > 240, "the left edge is solid");
    }

    // the synthetic font with a wght axis from 100 to 900, default 400, whose heaviest end
    // moves the square's lower right corner 100 units right, its upper left corner 50 units
    // up, and the advance phantom point 100 units right; with an HVAR table the advance grows
    // by 50 instead
    fn synthetic_variable_font(hvar: bool) -> Vec<u8> {
        let mut fvar = Vec::new();
        for value in [1_u16, 0, 16, 2, 1, 20, 0, 8] {
            fvar.extend_from_slice(&value.to_be_bytes());
        }
        fvar.extend_from_slice(b"wght");
        for value in [100_i32, 400, 900] {
            fvar.extend_from_slice(&(value << 16).to_be_bytes());
        }
        fvar.extend_from_slice(&[0, 0, 1, 0]);
        // points 1, 3 and phantom 5; point 3 is touched with no x delta, so IUP moves point 2
        // with point 1 and leaves point 0
        let serialized = [3, 0x02, 1, 2, 2, 0x02, 100, 0, 100, 0x02, 0, 50, 0];
        let mut glyph = Vec::new();
        for value in [1_u16, 10, serialized.len() as u16, 0xA000, 0x4000] {
            glyph.extend_from_slice(&value.to_be_bytes());
        }
        glyph.extend_from_slice(&serialized);
        glyph.push(0);
        let mut gvar = Vec::new();
        gvar.extend_from_slice(&0x0001_0000_u32.to_be_bytes());
        for value in [1_u16, 0] {
            gvar.extend_from_slice(&value.to_be_bytes());
        }
        gvar.extend_from_slice(&26_u32.to_be_bytes());
        for value in [2_u16, 0] {
            gvar.extend_from_slice(&value.to_be_bytes());
        }
        gvar.extend_from_slice(&26_u32.to_be_bytes());
        for value in [0_u16, 0, glyph.len() as u16 / 2] {
            gvar.extend_from_slice(&value.to_be_bytes());
        }
        gvar.extend_from_slice(&glyph);
        let mut extra: Vec<(&[u8; 4], Vec<u8>)> = vec![(b"fvar", fvar), (b"gvar", gvar)];
        if hvar {
            let mut table = Vec::new();
            table.extend_from_slice(&0x0001_0000_u32.to_be_bytes());
            for value in [20_u32, 0, 0, 0] {
                table.extend_from_slice(&value.to_be_bytes());
            }
            table.extend_from_slice(&1_u16.to_be_bytes());
            table.extend_from_slice(&12_u32.to_be_bytes());
            table.extend_from_slice(&1_u16.to_be_bytes());
            table.extend_from_slice(&22_u32.to_be_bytes());
            for value in [1_u16, 1, 0, 0x4000, 0x4000, 2, 0, 1, 0] {
                table.extend_from_slice(&value.to_be_bytes());
            }
            table.extend_from_slice(&[0, 50]);
            extra.push((b"HVAR", table));
        }
        synthetic_font_with(extra)
    }

    fn weight(weight: u16) -> FontStyle {
        FontStyle {
            weight,
            ..FontStyle::NORMAL
        }
    }

    #[test]
    fn variable_instances_move_points_and_advances_with_gvar() {
        let face = Rc::new(FontFace::parse(synthetic_variable_font(false)).expect("font"));
        assert_eq!(face.weight_range(), (100, 900));
        let heaviest = face.instance(weight(900), (100, 900));
        assert!(!Rc::ptr_eq(&heaviest, &face));
        assert!(Rc::ptr_eq(
            &heaviest,
            &face.instance(weight(900), (100, 900))
        ));
        assert!(Rc::ptr_eq(&face.instance(weight(400), (100, 900)), &face));
        assert_eq!(heaviest.weight(), 900);
        let corners = |face: &FontFace| {
            face.outline(1).expect("outline").contours[0]
                .iter()
                .map(|point| (point.x, point.y))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            corners(&face),
            vec![(100.0, 0.0), (600.0, 0.0), (600.0, 700.0), (100.0, 700.0)]
        );
        assert_eq!(
            corners(&heaviest),
            vec![(100.0, 0.0), (700.0, 0.0), (700.0, 750.0), (100.0, 750.0)]
        );
        assert_eq!(heaviest.advance(1, 1000.0), 700.0);
        let middle = face.instance(weight(650), (100, 900));
        assert_eq!(middle.advance(1, 1000.0), 650.0);
        assert_eq!(corners(&middle)[2], (650.0, 725.0));

        let hvar = Rc::new(FontFace::parse(synthetic_variable_font(true)).expect("font"));
        assert_eq!(
            hvar.instance(weight(900), (100, 900)).advance(1, 1000.0),
            650.0
        );
    }

    #[test]
    fn font_weight_picks_the_wght_axis_within_the_font_face_range() {
        let fonts = FontSet::new(
            Rc::new(FontFace::parse(synthetic_font()).expect("font")),
            None,
        );
        let variable = || Rc::new(FontFace::parse(synthetic_variable_font(false)).expect("font"));
        fonts.add_web_face("Ranged", Some((400, 650)), false, Vec::new(), variable());
        fonts.add_web_face("Open", None, false, Vec::new(), variable());
        let ranged = intern_font_stack("Ranged");
        let open = intern_font_stack("Open");
        // 900 clamps to 650, halfway up the axis, and draws without synthetic bold
        assert_eq!(fonts.measure("A", ranged, 1000.0, weight(900)), 650.0);
        assert_eq!(fonts.measure("A", ranged, 1000.0, weight(300)), 600.0);
        // without a font-weight descriptor the rule covers the whole axis
        assert_eq!(fonts.measure("A", open, 1000.0, weight(900)), 700.0);
        let settings = FontStyle {
            variations: parse_font_variations("\"wght\" 650").expect("settings"),
            ..FontStyle::NORMAL
        };
        assert_eq!(fonts.measure("A", open, 1000.0, settings), 650.0);

        let chosen = ChosenFace::new(&variable(), weight(900), (100, 900));
        assert!(!chosen.synthetic_bold);
        let ranged_light = ChosenFace::new(&variable(), weight(900), (100, 500));
        assert!(ranged_light.synthetic_bold);
    }

    #[test]
    fn font_variation_settings_parse_quoted_tags_and_numbers() {
        assert_eq!(
            parse_font_variations("normal"),
            Some(FontVariations::default())
        );
        let settings = parse_font_variations("'wght' 450, \"opsz\" 12.5").expect("settings");
        assert_eq!(
            parse_font_variations("\"wght\" 450,'opsz' 12.5"),
            Some(settings)
        );
        assert_eq!(
            font_variation_values(settings).to_vec(),
            vec![(*b"wght", 450.0), (*b"opsz", 12.5)]
        );
        assert_eq!(parse_font_variations("\"wg\" 1"), None);
        assert_eq!(parse_font_variations("\"wght\""), None);
        assert_eq!(parse_font_variations("wght 400"), None);
    }

    fn fixture(name: &str) -> FontFace {
        let path = format!("{}/tests/fixtures/fonts/{name}", env!("CARGO_MANIFEST_DIR"));
        FontFace::parse(std::fs::read(&path).expect("fixture font should exist"))
            .unwrap_or_else(|error| panic!("{name} should parse: {error}"))
    }

    #[test]
    fn a_pen_left_of_zero_rasterizes_at_its_own_quarter_pixel() {
        let fonts = FontSet::new(Rc::new(fixture("ceri-test.ttf")), None);
        let chosen = fonts
            .face_for('A', intern_font_stack("Ceri Test"), FontStyle::NORMAL)
            .expect("the fixture covers A");
        let glyph = chosen.face.glyph_for('A');
        let bitmap = |x| fonts.glyph_bitmap(&chosen, glyph, 20.0, x, 0.0).expect("A rasterizes");
        assert!(Rc::ptr_eq(&bitmap(2.25), &bitmap(-0.75)));
        assert!(!Rc::ptr_eq(&bitmap(2.25), &bitmap(2.5)));
    }

    #[test]
    fn truetype_cff_woff_and_woff2_files_give_the_same_glyphs() {
        let faces = [
            "ceri-test.ttf",
            "ceri-test.otf",
            "ceri-test.woff",
            "ceri-test.woff2",
        ]
        .map(fixture);
        for face in &faces {
            assert_eq!(face.units_per_em(), 1000.0);
            assert_eq!(face.weight(), 400);
            assert!(face.has_glyph('A') && !face.has_glyph('Z'));
            assert_eq!(face.advance(face.glyph_for('A'), 1000.0), 600.0);
            let bitmap = rasterize(
                &face.outline(face.glyph_for('A')).expect("A has an outline"),
                0.05,
                0.0,
            )
            .expect("A rasterizes");
            // the left stem of the A is solid
            let row = bitmap.height / 2;
            assert!(bitmap.coverage[row * bitmap.width + 1] > 200);
        }
    }

    #[test]
    fn shaping_applies_ligatures_and_kerning_from_the_font() {
        let fonts = FontSet::new(Rc::new(fixture("ceri-test.otf")), None);
        let stack = intern_font_stack("Ceri Test");
        // f and i join into one 700 unit ligature; A and B kern by -100
        assert_eq!(fonts.measure("fi", stack, 1000.0, FontStyle::NORMAL), 700.0);
        assert_eq!(
            fonts.measure("AB", stack, 1000.0, FontStyle::NORMAL),
            1100.0
        );
        let spaced = fonts.shape("fi", stack, 1000.0, FontStyle::NORMAL, false);
        assert_eq!(
            spaced.glyphs.len(),
            2,
            "without ligatures each letter keeps its glyph"
        );
        let woff2 = FontSet::new(Rc::new(fixture("ceri-test.woff2")), None);
        assert_eq!(
            woff2.measure("fiAB", stack, 1000.0, FontStyle::NORMAL),
            1800.0
        );
    }

    #[test]
    fn font_set_measures_with_the_face_and_falls_back_to_the_builtin_metrics() {
        let face = Rc::new(FontFace::parse(synthetic_font()).expect("font"));
        let fonts = FontSet::new(face, None);
        let stack = intern_font_stack("Test Face, sans-serif");
        assert!(!fonts.is_builtin());
        assert!((fonts.measure("AA", stack, 100.0, FontStyle::NORMAL) - 120.0).abs() < 0.01);
        assert!(fonts.glyph('A', stack, 20.0, FontStyle::NORMAL).is_some());

        let builtin = FontSet::builtin();
        assert!(builtin.is_builtin());
        assert_eq!(builtin.measure("AAA", stack, 12.0, FontStyle::NORMAL), 18.0);
        assert_eq!(builtin.line_height(stack, 16.0), 20.0);
        assert!(builtin.glyph('A', stack, 16.0, FontStyle::NORMAL).is_none());
    }

    #[test]
    fn font_stacks_intern_by_family_list_and_resolve_in_order() {
        let first = intern_font_stack("\"Aptos Narrow\", Tahoma, sans-serif");
        let same = intern_font_stack("aptos narrow , tahoma , sans-serif");
        let other = intern_font_stack("Consolas, monospace");
        assert_eq!(first, same);
        assert_ne!(first, other);
        assert_eq!(
            font_stack_families(first)
                .expect("stack should resolve")
                .to_vec(),
            vec![
                "aptos narrow".to_string(),
                "tahoma".to_string(),
                "sans-serif".to_string()
            ]
        );
        assert!(font_stack_families(FontStack::default()).is_none());

        struct OnlyTahoma(Rc<FontFace>);
        impl FontSource for OnlyTahoma {
            fn face(&self, family: &str, _style: FontStyle) -> Option<Rc<FontFace>> {
                (family == "tahoma").then(|| self.0.clone())
            }
        }

        let face = Rc::new(FontFace::parse(synthetic_font()).expect("font"));
        let fonts = FontSet::from_source(Rc::new(OnlyTahoma(face)));
        assert!((fonts.measure("A", first, 1000.0, FontStyle::NORMAL) - 600.0).abs() < 0.01);
        // an unavailable stack falls back to the builtin metrics rather than measuring zero
        assert_eq!(
            fonts.measure("A", other, 1000.0, FontStyle::NORMAL),
            builtin_advance(1000.0)
        );
    }
}
