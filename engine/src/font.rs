use std::cell::RefCell;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::rc::Rc;

const MAX_COMPOSITE_DEPTH: u8 = 5;
const MAX_CONTOUR_POINTS: usize = 20_000;
const MAX_GLYPH_PIXELS: usize = 4_000_000;

// glyph outlines and metrics read straight from an sfnt file, no shaping engine
pub struct FontFace {
    data: Vec<u8>,
    units_per_em: f32,
    ascent: f32,
    descent: f32,
    line_gap: f32,
    num_glyphs: u16,
    long_loca: bool,
    loca: (usize, usize),
    glyf: (usize, usize),
    hmtx: (usize, usize),
    horizontal_metrics: u16,
    characters: BTreeMap<u32, u16>,
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
        let tables = read_table_directory(&data)?;
        let head = *tables.get(b"head").ok_or("font has no head table")?;
        let hhea = *tables.get(b"hhea").ok_or("font has no hhea table")?;
        let maxp = *tables.get(b"maxp").ok_or("font has no maxp table")?;
        let hmtx = *tables.get(b"hmtx").ok_or("font has no hmtx table")?;
        let loca = *tables.get(b"loca").ok_or("font has no loca table")?;
        let glyf = *tables.get(b"glyf").ok_or("font has no glyf table")?;
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

        Ok(Self {
            data,
            units_per_em,
            ascent,
            descent: -descent,
            line_gap,
            num_glyphs,
            long_loca,
            loca,
            glyf,
            hmtx,
            horizontal_metrics,
            characters,
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

    pub fn line_height(&self, size: f32) -> f32 {
        (self.ascent + self.descent + self.line_gap) / self.units_per_em * size
    }

    pub fn glyph_for(&self, character: char) -> u16 {
        self.characters
            .get(&u32::from(character))
            .copied()
            .unwrap_or(0)
    }

    pub fn advance(&self, glyph: u16, size: f32) -> f32 {
        let metrics = self.horizontal_metrics.max(1);
        let index = glyph.min(metrics - 1);
        let offset = self.hmtx.0 + usize::from(index) * 4;
        let advance = read_u16(&self.data, offset).unwrap_or(0);
        f32::from(advance) / self.units_per_em * size
    }

    pub fn outline(&self, glyph: u16) -> Option<Outline> {
        let mut outline = Outline::default();
        self.append_outline(glyph, (1.0, 0.0, 0.0, 1.0), (0.0, 0.0), 0, &mut outline)?;
        Some(outline)
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
                start,
                end,
                contour_count as usize,
                transform,
                offset,
                outline,
            )
        } else {
            self.append_composite_glyph(start, end, transform, offset, depth, outline)
        }
    }

    fn append_simple_glyph(
        &self,
        start: usize,
        end: usize,
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

        let mut first = 0;
        for &last in &contour_ends {
            if last >= point_count {
                break;
            }
            let mut contour = Vec::with_capacity(last + 1 - first);
            for index in first..=last {
                let point_x = x_coordinates[index] as f32;
                let point_y = y_coordinates[index] as f32;
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
        start: usize,
        end: usize,
        transform: (f32, f32, f32, f32),
        offset: (f32, f32),
        depth: u8,
        outline: &mut Outline,
    ) -> Option<()> {
        let mut cursor = start + 10;
        loop {
            if cursor + 4 > end {
                return Some(());
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
            // only offsets in component space are supported, matching ARGS_ARE_XY_VALUES
            let (component_offset_x, component_offset_y) = if flags & 0x0002 != 0 {
                (argument_x, argument_y)
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
            self.append_outline(component, combined, component_origin, depth + 1, outline)?;
            if flags & 0x0020 == 0 {
                return Some(());
            }
        }
    }
}

// signed-area accumulation, the same approach font-rs uses for antialiased coverage
pub fn rasterize(outline: &Outline, scale: f32, subpixel_x: f32) -> Option<GlyphBitmap> {
    let mut segments = Vec::new();
    let mut minimum = (f32::MAX, f32::MAX);
    let mut maximum = (f32::MIN, f32::MIN);
    for contour in &outline.contours {
        flatten_contour(contour, scale, subpixel_x, &mut segments);
    }
    if segments.is_empty() {
        return None;
    }
    for segment in &segments {
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
    for segment in &segments {
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
    family: FontStack,
    glyph: u16,
    size: u32,
    bold: bool,
}

type FaceCache = HashMap<(FontStack, bool), Option<Rc<FontFace>>>;

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

// the host owns font discovery, the engine only asks for a family by name
pub trait FontSource {
    fn face(&self, family: &str, bold: bool) -> Option<Rc<FontFace>>;
}

struct SingleFaceSource {
    regular: Rc<FontFace>,
    bold: Option<Rc<FontFace>>,
}

impl FontSource for SingleFaceSource {
    fn face(&self, _family: &str, bold: bool) -> Option<Rc<FontFace>> {
        if bold {
            return self.bold.clone().or_else(|| Some(self.regular.clone()));
        }
        Some(self.regular.clone())
    }
}

// text metrics and rasterized glyphs for the whole engine, cached per face and size
pub struct FontSet {
    source: Option<Rc<dyn FontSource>>,
    faces: RefCell<FaceCache>,
    glyphs: RefCell<HashMap<GlyphKey, Option<Rc<GlyphBitmap>>>>,
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
            faces: RefCell::new(HashMap::new()),
            glyphs: RefCell::new(HashMap::new()),
        }
    }

    pub fn from_source(source: Rc<dyn FontSource>) -> Self {
        Self {
            source: Some(source),
            faces: RefCell::new(HashMap::new()),
            glyphs: RefCell::new(HashMap::new()),
        }
    }

    pub fn new(regular: Rc<FontFace>, bold: Option<Rc<FontFace>>) -> Self {
        Self::from_source(Rc::new(SingleFaceSource { regular, bold }))
    }

    pub fn is_builtin(&self) -> bool {
        self.source.is_none()
    }

    pub fn measure(&self, text: &str, stack: FontStack, size: f32, bold: bool) -> f32 {
        match self.face(stack, bold) {
            Some(face) => text
                .chars()
                .map(|character| face.advance(face.glyph_for(character), size))
                .sum(),
            None => text.chars().count() as f32 * builtin_advance(size),
        }
    }

    pub fn advance(&self, character: char, stack: FontStack, size: f32, bold: bool) -> f32 {
        match self.face(stack, bold) {
            Some(face) => face.advance(face.glyph_for(character), size),
            None => builtin_advance(size),
        }
    }

    pub fn ascent(&self, stack: FontStack, size: f32) -> f32 {
        match self.face(stack, false) {
            Some(face) => face.ascent(size),
            None => size,
        }
    }

    pub fn line_height(&self, stack: FontStack, size: f32) -> f32 {
        match self.face(stack, false) {
            Some(face) => face.line_height(size),
            None => size * 1.25,
        }
    }

    pub fn glyph(
        &self,
        character: char,
        stack: FontStack,
        size: f32,
        bold: bool,
    ) -> Option<Rc<GlyphBitmap>> {
        let face = self.face(stack, bold)?;
        let glyph = face.glyph_for(character);
        let key = GlyphKey {
            family: stack,
            glyph,
            size: size.to_bits(),
            bold,
        };
        if let Some(cached) = self.glyphs.borrow().get(&key) {
            return cached.clone();
        }
        let bitmap = face
            .outline(glyph)
            .and_then(|outline| rasterize(&outline, size / face.units_per_em(), 0.0))
            .map(Rc::new);
        self.glyphs.borrow_mut().insert(key, bitmap.clone());
        bitmap
    }

    // the first family the host can supply wins, matching css font fallback
    fn face(&self, stack: FontStack, bold: bool) -> Option<Rc<FontFace>> {
        let source = self.source.as_ref()?;
        if let Some(cached) = self.faces.borrow().get(&(stack, bold)) {
            return cached.clone();
        }
        let families = font_stack_families(stack);
        let mut face = None;
        if let Some(families) = families.as_deref() {
            for family in families {
                face = source.face(family, bold);
                if face.is_some() {
                    break;
                }
            }
        }
        let face = face.or_else(|| source.face("sans-serif", bold));
        self.faces.borrow_mut().insert((stack, bold), face.clone());
        face
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
        0x4f54_544f => return Err("OpenType CFF outlines are not supported".to_string()),
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

        let tables: Vec<(&[u8; 4], Vec<u8>)> = vec![
            (b"cmap", cmap),
            (b"glyf", glyf),
            (b"head", head),
            (b"hhea", hhea),
            (b"hmtx", hmtx),
            (b"loca", loca),
            (b"maxp", maxp),
        ];
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
    fn font_set_measures_with_the_face_and_falls_back_to_the_builtin_metrics() {
        let face = Rc::new(FontFace::parse(synthetic_font()).expect("font"));
        let fonts = FontSet::new(face, None);
        let stack = intern_font_stack("Test Face, sans-serif");
        assert!(!fonts.is_builtin());
        assert!((fonts.measure("AA", stack, 100.0, false) - 120.0).abs() < 0.01);
        assert!(fonts.glyph('A', stack, 20.0, false).is_some());

        let builtin = FontSet::builtin();
        assert!(builtin.is_builtin());
        assert_eq!(builtin.measure("AAA", stack, 12.0, false), 18.0);
        assert_eq!(builtin.line_height(stack, 16.0), 20.0);
        assert!(builtin.glyph('A', stack, 16.0, false).is_none());
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
            fn face(&self, family: &str, _bold: bool) -> Option<Rc<FontFace>> {
                (family == "tahoma").then(|| self.0.clone())
            }
        }

        let face = Rc::new(FontFace::parse(synthetic_font()).expect("font"));
        let fonts = FontSet::from_source(Rc::new(OnlyTahoma(face)));
        assert!((fonts.measure("A", first, 1000.0, false) - 600.0).abs() < 0.01);
        // an unavailable stack falls back to the builtin metrics rather than measuring zero
        assert_eq!(
            fonts.measure("A", other, 1000.0, false),
            builtin_advance(1000.0)
        );
    }
}
