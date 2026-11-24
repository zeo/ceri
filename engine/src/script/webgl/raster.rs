// the draw pipeline: vertices are fetched and shaded four at a time, primitives assembled,
// clipped against the near, far and w planes, mapped to the viewport and rasterized in 2x2
// quads so fragment shaders get derivatives; each covered fragment then goes through the
// stencil and depth tests, blending and the color mask on its way into the attachments

use super::constants::*;
use super::glsl::{ALL, Base, LANES, Machine, Module, Textures};
use super::texture::{Format, Kind, Level, TexelsMut, channels};

#[derive(Clone, Copy, Debug)]
pub(crate) struct StencilFace {
    pub(crate) func: u32,
    pub(crate) reference: i32,
    pub(crate) value_mask: u32,
    pub(crate) write_mask: u32,
    pub(crate) fail: u32,
    pub(crate) depth_fail: u32,
    pub(crate) pass: u32,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Blend {
    // rgb then alpha
    pub(crate) equation: [u32; 2],
    pub(crate) source: [u32; 2],
    pub(crate) destination: [u32; 2],
    pub(crate) color: [f32; 4],
}

#[derive(Clone, Debug)]
pub(crate) struct Pipeline {
    pub(crate) viewport: [i32; 4],
    pub(crate) depth_range: [f32; 2],
    pub(crate) scissor: Option<[i32; 4]>,
    pub(crate) cull: Option<u32>,
    pub(crate) front_face: u32,
    pub(crate) polygon_offset: Option<[f32; 2]>,
    pub(crate) depth_test: Option<u32>,
    pub(crate) depth_mask: bool,
    pub(crate) stencil: Option<[StencilFace; 2]>,
    pub(crate) blend: Option<Blend>,
    pub(crate) color_mask: [bool; 4],
    pub(crate) rasterizer_discard: bool,
}

pub(crate) enum Source<'a> {
    Buffer {
        data: &'a [u8],
        size: usize,
        kind: u32,
        normalized: bool,
        // read with vertexAttribIPointer, so integers stay integers
        integer: bool,
        stride: usize,
        offset: usize,
        divisor: u32,
    },
    Constant([u32; 4]),
}

// one attribute location the vertex shader reads
pub(crate) struct Fetch<'a> {
    pub(crate) slot: usize,
    pub(crate) words: usize,
    pub(crate) base: Base,
    pub(crate) source: Source<'a>,
}

// a vertex shader output feeding a fragment shader input
#[derive(Clone, Copy, Debug)]
pub(crate) struct Link {
    pub(crate) from: usize,
    pub(crate) to: usize,
    pub(crate) count: usize,
    pub(crate) flat: bool,
}

pub(crate) struct Stage<'a> {
    pub(crate) module: &'a Module,
    // the module's memory with uniforms set
    pub(crate) memory: &'a [u32],
}

pub(crate) struct Draw<'a> {
    pub(crate) mode: u32,
    // vertex ids in drawing order; u32::MAX restarts the primitive
    pub(crate) indices: &'a [u32],
    pub(crate) instances: u32,
    pub(crate) vertex: Stage<'a>,
    pub(crate) fragment: Stage<'a>,
    pub(crate) fetches: Vec<Fetch<'a>>,
    pub(crate) links: Vec<Link>,
    // fragment output slot and the draw buffer it goes to
    pub(crate) outputs: Vec<(usize, usize)>,
    pub(crate) frag_depth: Option<usize>,
    pub(crate) textures: &'a (dyn Textures + Sync),
    // vertex output slots and sizes transform feedback records, in order
    pub(crate) capture: Vec<(usize, usize)>,
}

// rows of one layer of an attachment, from row `top` down the slice
pub(crate) struct View<'a> {
    pub(crate) texels: TexelsMut<'a>,
    pub(crate) format: Format,
    pub(crate) width: u32,
    pub(crate) top: u32,
}

impl<'a> View<'a> {
    pub(crate) fn layer(level: &'a mut Level, layer: u32) -> View<'a> {
        let (format, width) = (level.format, level.width);
        View {
            format,
            width,
            top: 0,
            texels: level.plane_mut(layer),
        }
    }

    #[inline]
    fn at(&self, x: u32, y: u32) -> usize {
        ((y - self.top) * self.width + x) as usize
    }

    #[inline]
    fn get(&self, x: u32, y: u32) -> [f32; 4] {
        self.texels
            .get(self.at(x, y), self.format.kind == Kind::Srgb)
    }

    #[inline]
    fn set(&mut self, x: u32, y: u32, value: [f32; 4]) {
        let index = self.at(x, y);
        self.texels
            .set(index, value, self.format.kind == Kind::Srgb);
    }
}

pub(crate) struct Target<'a> {
    pub(crate) width: u32,
    pub(crate) height: u32,
    // per draw buffer
    pub(crate) colors: Vec<Option<View<'a>>>,
    pub(crate) depth_stencil: Option<View<'a>>,
}

impl<'a> Target<'a> {
    // horizontal bands of an even number of rows, so no 2x2 quad straddles two of them, each
    // with its rows of every attachment and the range of rows it covers
    fn split(self, bands: usize) -> Vec<([i32; 2], Target<'a>)> {
        let Target {
            width,
            height,
            colors,
            depth_stencil,
        } = self;
        let rows = (height as usize)
            .div_ceil(bands.max(1))
            .next_multiple_of(2)
            .max(2);
        let count = (height as usize).div_ceil(rows).max(1);
        let chunk = |view: View<'a>| {
            let size = rows * view.width as usize;
            (
                view.format,
                view.width,
                view.texels.chunks(size.max(1)).into_iter(),
            )
        };
        let mut colors: Vec<_> = colors.into_iter().map(|view| view.map(chunk)).collect();
        let mut depth = depth_stencil.map(chunk);
        let next = |entry: &mut Option<(Format, u32, std::vec::IntoIter<TexelsMut<'a>>)>,
                    top: u32| {
            entry.as_mut().and_then(|(format, width, chunks)| {
                chunks.next().map(|texels| View {
                    texels,
                    format: *format,
                    width: *width,
                    top,
                })
            })
        };
        (0..count)
            .map(|band| {
                let top = (band * rows) as u32;
                let bottom = ((band + 1) * rows).min(height as usize) as u32;
                let colors = colors.iter_mut().map(|entry| next(entry, top)).collect();
                let depth_stencil = next(&mut depth, top);
                (
                    [top as i32, bottom as i32],
                    Target {
                        width,
                        height,
                        colors,
                        depth_stencil,
                    },
                )
            })
            .collect()
    }
}

// fragment work worth another thread, as the summed bounding boxes of the primitives
const PIXELS_PER_WORKER: u64 = 16_384;

fn max_workers() -> usize {
    static WORKERS: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *WORKERS.get_or_init(|| {
        std::thread::available_parallelism()
            .map_or(1, |count| count.get())
            .min(8)
    })
}

#[derive(Default)]
pub(crate) struct Outcome {
    pub(crate) samples_passed: u64,
    pub(crate) captured: Vec<u32>,
    pub(crate) primitives: u64,
}

// a vertex after the vertex shader: clip position, point size and varying words
struct Shaded {
    stride: usize,
    positions: Vec<[f32; 4]>,
    sizes: Vec<f32>,
    words: Vec<u32>,
    captured: Vec<u32>,
    capture_stride: usize,
}

fn read_attribute(source: &Source, vertex: u32, instance: u32, words: usize) -> [u32; 4] {
    match source {
        Source::Constant(value) => *value,
        Source::Buffer {
            data,
            size,
            kind,
            normalized,
            integer,
            stride,
            offset,
            divisor,
        } => {
            let element = if *divisor == 0 {
                vertex
            } else {
                instance / divisor
            };
            let mut value = [
                0.0_f32.to_bits(),
                0.0_f32.to_bits(),
                0.0_f32.to_bits(),
                1.0_f32.to_bits(),
            ];
            if *integer {
                value = [0, 0, 0, 1];
            }
            let component_size = match *kind {
                BYTE | UNSIGNED_BYTE => 1,
                SHORT | UNSIGNED_SHORT | HALF_FLOAT | HALF_FLOAT_OES => 2,
                _ => 4,
            };
            let start = offset + element as usize * stride;
            if *kind == INT_2_10_10_10_REV || *kind == UNSIGNED_INT_2_10_10_10_REV {
                let Some(bytes) = data.get(start..start + 4) else {
                    return value;
                };
                let word = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
                let signed = *kind == INT_2_10_10_10_REV;
                for (index, (shift, bits)) in [(0, 10), (10, 10), (20, 10), (30, 2)]
                    .into_iter()
                    .enumerate()
                    .take(*size)
                {
                    let raw = (word >> shift) & ((1 << bits) - 1);
                    let number = if signed {
                        ((raw << (32 - bits)) as i32 >> (32 - bits)) as f32
                    } else {
                        raw as f32
                    };
                    let max = ((1 << (bits - u32::from(signed))) - 1) as f32;
                    value[index] = (if *normalized {
                        (number / max).max(-1.0)
                    } else {
                        number
                    })
                    .to_bits();
                }
                return value;
            }
            for (index, slot) in value
                .iter_mut()
                .enumerate()
                .take((*size).min(words.max(1)).min(4))
            {
                let at = start + index * component_size;
                let Some(bytes) = data.get(at..at + component_size) else {
                    break;
                };
                let (number, raw): (f64, u32) = match *kind {
                    BYTE => (f64::from(bytes[0] as i8), bytes[0] as i8 as i32 as u32),
                    UNSIGNED_BYTE => (f64::from(bytes[0]), u32::from(bytes[0])),
                    SHORT => {
                        let short = i16::from_le_bytes([bytes[0], bytes[1]]);
                        (f64::from(short), short as i32 as u32)
                    }
                    UNSIGNED_SHORT => {
                        let short = u16::from_le_bytes([bytes[0], bytes[1]]);
                        (f64::from(short), u32::from(short))
                    }
                    INT => {
                        let int = i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
                        (f64::from(int), int as u32)
                    }
                    UNSIGNED_INT => {
                        let int = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
                        (f64::from(int), int)
                    }
                    HALF_FLOAT | HALF_FLOAT_OES => (
                        super::super::typed::half_value(u16::from_le_bytes([bytes[0], bytes[1]])),
                        0,
                    ),
                    _ => (
                        f64::from(f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])),
                        0,
                    ),
                };
                *slot = if *integer {
                    raw
                } else if *normalized {
                    let scaled = match *kind {
                        BYTE => (number / 127.0).max(-1.0),
                        UNSIGNED_BYTE => number / 255.0,
                        SHORT => (number / 32767.0).max(-1.0),
                        UNSIGNED_SHORT => number / 65535.0,
                        INT => (number / 2_147_483_647.0).max(-1.0),
                        UNSIGNED_INT => number / 4_294_967_295.0,
                        _ => number,
                    };
                    (scaled as f32).to_bits()
                } else {
                    (number as f32).to_bits()
                };
            }
            value
        }
    }
}

impl Draw<'_> {
    fn machine(&self, stage: &Stage) -> Machine<'_> {
        let mut machine = stage.module.machine(self.textures);
        machine.memory.copy_from_slice(stage.memory);
        machine
    }

    // runs the vertex shader once per distinct vertex id of one instance
    fn shade_vertices(&self, ids: &[u32], instance: u32) -> Shaded {
        let module = self.vertex.module;
        let mut machine = self.machine(&self.vertex);
        let stride: usize = self.links.iter().map(|link| link.count).sum();
        let capture_stride: usize = self.capture.iter().map(|(_, count)| count).sum();
        let mut shaded = Shaded {
            stride,
            positions: Vec::with_capacity(ids.len()),
            sizes: Vec::with_capacity(ids.len()),
            words: Vec::with_capacity(ids.len() * stride),
            captured: Vec::with_capacity(ids.len() * capture_stride),
            capture_stride,
        };
        let position = module.builtins["gl_Position"];
        let point_size = module.builtins["gl_PointSize"];
        let vertex_id = module.builtins.get("gl_VertexID").copied();
        let instance_id = module.builtins.get("gl_InstanceID").copied();
        for batch in ids.chunks(LANES) {
            for (lane, &id) in batch.iter().enumerate() {
                for fetch in &self.fetches {
                    let value = read_attribute(&fetch.source, id, instance, fetch.words);
                    let value = match (&fetch.source, fetch.base) {
                        // a float source feeding an integer input, or the reverse, as GL converts
                        (Source::Constant(_), _) => value,
                        (Source::Buffer { integer: true, .. }, Base::Float) => {
                            value.map(|word| (word as i32 as f32).to_bits())
                        }
                        (Source::Buffer { integer: false, .. }, Base::Int | Base::Uint) => {
                            value.map(|word| f32::from_bits(word) as i32 as u32)
                        }
                        _ => value,
                    };
                    for (index, word) in value.iter().enumerate().take(fetch.words) {
                        machine.set(fetch.slot + index, lane, *word);
                    }
                }
                if let Some(slot) = vertex_id {
                    machine.set(slot, lane, id);
                }
                if let Some(slot) = instance_id {
                    machine.set(slot, lane, instance);
                }
                machine.set_float(point_size, lane, 1.0);
            }
            let mask = (1_u8 << batch.len()) - 1;
            module.run(&mut machine, mask);
            for lane in 0..batch.len() {
                shaded.positions.push(std::array::from_fn(|index| {
                    machine.float(position + index, lane)
                }));
                shaded.sizes.push(machine.float(point_size, lane));
                for link in &self.links {
                    for index in 0..link.count {
                        shaded.words.push(machine.get(link.from + index, lane));
                    }
                }
                for &(slot, count) in &self.capture {
                    for index in 0..count {
                        shaded.captured.push(machine.get(slot + index, lane));
                    }
                }
            }
        }
        shaded
    }

    pub(crate) fn run(&self, pipeline: &Pipeline, target: Target) -> Outcome {
        let mut outcome = Outcome::default();
        // distinct ids, so a vertex shared by many triangles is shaded once
        let mut order: Vec<u32> = Vec::new();
        let mut places: Vec<u32> = Vec::with_capacity(self.indices.len());
        let mut seen: std::collections::HashMap<u32, u32> = std::collections::HashMap::default();
        for &id in self.indices {
            if id == u32::MAX {
                places.push(u32::MAX);
                continue;
            }
            let place = *seen.entry(id).or_insert_with(|| {
                order.push(id);
                order.len() as u32 - 1
            });
            places.push(place);
        }
        let flat_words: Vec<bool> = self
            .links
            .iter()
            .flat_map(|link| std::iter::repeat_n(link.flat, link.count))
            .collect();
        let mut batches: Vec<(Shaded, Vec<Primitive>)> = Vec::new();
        for instance in 0..self.instances {
            let shaded = self.shade_vertices(&order, instance);
            let primitives = assemble(self.mode, &places);
            outcome.primitives += primitives.len() as u64;
            if !self.capture.is_empty() {
                for primitive in &primitives {
                    for place in primitive.vertices() {
                        let start = place as usize * shaded.capture_stride;
                        outcome.captured.extend_from_slice(
                            &shaded.captured[start..start + shaded.capture_stride],
                        );
                    }
                }
            }
            if !pipeline.rasterizer_discard {
                batches.push((shaded, primitives));
            }
        }
        if batches.is_empty() {
            return outcome;
        }
        let workers = (estimate_area(&batches, pipeline) / PIXELS_PER_WORKER)
            .clamp(1, max_workers() as u64) as usize;
        // every band sees every primitive in order, so blending keeps the draw order
        let rasterize = |rows: [i32; 2], band: Target| -> u64 {
            let mut fragments = Fragments::new(self, pipeline, band, &flat_words, rows);
            for (shaded, primitives) in &batches {
                for primitive in primitives {
                    match *primitive {
                        Primitive::Point(a) => fragments.point(shaded, a as usize),
                        Primitive::Line(a, b) => fragments.line(shaded, a as usize, b as usize),
                        Primitive::Triangle(a, b, c) => {
                            fragments.triangle(shaded, [a as usize, b as usize, c as usize])
                        }
                    }
                }
            }
            fragments.passed
        };
        let mut bands = target.split(workers);
        outcome.samples_passed = if bands.len() == 1 {
            let (rows, band) = bands.pop().expect("one band");
            rasterize(rows, band)
        } else {
            std::thread::scope(|scope| {
                let rasterize = &rasterize;
                let handles: Vec<_> = bands
                    .into_iter()
                    .map(|(rows, band)| scope.spawn(move || rasterize(rows, band)))
                    .collect();
                handles
                    .into_iter()
                    .map(|handle| handle.join().expect("a band of fragments was drawn"))
                    .sum()
            })
        };
        outcome
    }
}

#[derive(Clone, Copy)]
enum Primitive {
    Point(u32),
    Line(u32, u32),
    Triangle(u32, u32, u32),
}

impl Primitive {
    fn vertices(&self) -> Vec<u32> {
        match *self {
            Primitive::Point(a) => vec![a],
            Primitive::Line(a, b) => vec![a, b],
            Primitive::Triangle(a, b, c) => vec![a, b, c],
        }
    }
}

// primitives from the vertex list of a mode, restarting at each u32::MAX
fn assemble(mode: u32, places: &[u32]) -> Vec<Primitive> {
    let mut primitives = Vec::new();
    for run in places.split(|&place| place == u32::MAX) {
        match mode {
            POINTS => primitives.extend(run.iter().map(|&a| Primitive::Point(a))),
            LINES => primitives.extend(
                run.as_chunks::<2>()
                    .0
                    .iter()
                    .map(|&[a, b]| Primitive::Line(a, b)),
            ),
            LINE_STRIP | LINE_LOOP => {
                primitives.extend(run.windows(2).map(|pair| Primitive::Line(pair[0], pair[1])));
                if mode == LINE_LOOP && run.len() > 2 {
                    primitives.push(Primitive::Line(run[run.len() - 1], run[0]));
                }
            }
            TRIANGLES => primitives.extend(
                run.as_chunks::<3>()
                    .0
                    .iter()
                    .map(|&[a, b, c]| Primitive::Triangle(a, b, c)),
            ),
            TRIANGLE_STRIP => {
                primitives.extend(run.windows(3).enumerate().map(|(index, three)| {
                    if index % 2 == 0 {
                        Primitive::Triangle(three[0], three[1], three[2])
                    } else {
                        Primitive::Triangle(three[1], three[0], three[2])
                    }
                }))
            }
            TRIANGLE_FAN if run.len() >= 3 => primitives.extend(
                run[1..]
                    .windows(2)
                    .map(|pair| Primitive::Triangle(run[0], pair[0], pair[1])),
            ),
            _ => {}
        }
    }
    primitives
}

// how many pixels the primitives may touch, from their window-space bounding boxes; a
// primitive reaching behind the eye counts as the whole viewport
fn estimate_area(batches: &[(Shaded, Vec<Primitive>)], pipeline: &Pipeline) -> u64 {
    let [_, _, width, height] = pipeline.viewport;
    let (width, height) = (width.max(0) as f32, height.max(0) as f32);
    let full = (width * height) as u64;
    let window = |clip: [f32; 4]| -> Option<(f32, f32)> {
        (clip[3] > 0.0).then(|| {
            (
                (clip[0] / clip[3] + 1.0) * 0.5 * width,
                (clip[1] / clip[3] + 1.0) * 0.5 * height,
            )
        })
    };
    let mut total = 0_u64;
    for (shaded, primitives) in batches {
        for primitive in primitives {
            let corners: Vec<Option<(f32, f32)>> = primitive
                .vertices()
                .iter()
                .map(|&place| window(shaded.positions[place as usize]))
                .collect();
            let area = match (
                primitive,
                corners.iter().copied().collect::<Option<Vec<_>>>(),
            ) {
                (Primitive::Point(a), _) => {
                    shaded.sizes[*a as usize].clamp(1.0, 1024.0).powi(2) as u64
                }
                (_, None) => full,
                (_, Some(points)) => {
                    let (mut low, mut high) = ((f32::MAX, f32::MAX), (f32::MIN, f32::MIN));
                    for (x, y) in points {
                        low = (low.0.min(x), low.1.min(y));
                        high = (high.0.max(x), high.1.max(y));
                    }
                    let span =
                        |low: f32, high: f32, limit: f32| (high.min(limit) - low.max(0.0)).max(1.0);
                    (span(low.0, high.0, width) * span(low.1, high.1, height)) as u64
                }
            };
            total = total.saturating_add(area.min(full.max(1)));
        }
    }
    total
}

// a vertex in clip space with its varying words, as clipping makes new ones
#[derive(Clone)]
struct Corner {
    clip: [f32; 4],
    words: Vec<u32>,
}

// a vertex after the viewport transform: window x, y, z and 1/w
#[derive(Clone)]
struct Windowed {
    x: f32,
    y: f32,
    z: f32,
    inverse_w: f32,
    words: Vec<u32>,
}

fn lerp_corner(a: &Corner, b: &Corner, t: f32, flat: &[bool]) -> Corner {
    Corner {
        clip: std::array::from_fn(|index| a.clip[index] + (b.clip[index] - a.clip[index]) * t),
        words: a
            .words
            .iter()
            .zip(&b.words)
            .zip(flat)
            .map(|((&x, &y), &flat)| {
                if flat {
                    x
                } else {
                    let (x, y) = (f32::from_bits(x), f32::from_bits(y));
                    (x + (y - x) * t).to_bits()
                }
            })
            .collect(),
    }
}

// the near, far and w > 0 planes, as distances that are positive inside
const PLANES: [fn(&[f32; 4]) -> f32; 3] = [|p| p[2] + p[3], |p| p[3] - p[2], |p| p[3] - 1e-6];

fn clip_polygon(mut polygon: Vec<Corner>, flat: &[bool]) -> Vec<Corner> {
    for plane in PLANES {
        if polygon.iter().all(|corner| plane(&corner.clip) >= 0.0) {
            continue;
        }
        let mut next = Vec::with_capacity(polygon.len() + 2);
        for index in 0..polygon.len() {
            let (current, following) = (&polygon[index], &polygon[(index + 1) % polygon.len()]);
            let (d0, d1) = (plane(&current.clip), plane(&following.clip));
            if d0 >= 0.0 {
                next.push(current.clone());
            }
            if (d0 >= 0.0) != (d1 >= 0.0) {
                next.push(lerp_corner(current, following, d0 / (d0 - d1), flat));
            }
        }
        polygon = next;
        if polygon.len() < 3 {
            return Vec::new();
        }
    }
    polygon
}

struct Fragments<'a, 't> {
    draw: &'a Draw<'a>,
    pipeline: &'a Pipeline,
    target: Target<'t>,
    flat: &'a [bool],
    machine: Machine<'a>,
    // the pixels fragments may land on: viewport, scissor and attachment size together
    bounds: [i32; 4],
    // like bounds but without the viewport, for points and lines that may reach past it
    wide_bounds: [i32; 4],
    passed: u64,
    coord: usize,
    front_facing: usize,
    point_coord: usize,
    // the depth bits of the attachment, for polygon offset's unit
    depth_unit: f32,
}

#[inline]
fn intersect(a: [i32; 4], b: [i32; 4]) -> [i32; 4] {
    [
        a[0].max(b[0]),
        a[1].max(b[1]),
        a[2].min(b[2]),
        a[3].min(b[3]),
    ]
}

impl<'a, 't> Fragments<'a, 't> {
    fn new(
        draw: &'a Draw<'a>,
        pipeline: &'a Pipeline,
        target: Target<'t>,
        flat: &'a [bool],
        rows: [i32; 2],
    ) -> Self {
        let size = [
            0,
            rows[0],
            target.width as i32,
            rows[1].min(target.height as i32),
        ];
        let [x, y, width, height] = pipeline.viewport;
        let viewport = [x, y, x.saturating_add(width), y.saturating_add(height)];
        let wide_bounds = match pipeline.scissor {
            Some([sx, sy, sw, sh]) => {
                intersect(size, [sx, sy, sx.saturating_add(sw), sy.saturating_add(sh)])
            }
            None => size,
        };
        let bounds = intersect(wide_bounds, viewport);
        let module = draw.fragment.module;
        let depth_unit = match target
            .depth_stencil
            .as_ref()
            .map(|view| view.format.bits[0])
        {
            Some(bits @ 1..=24) => 1.0 / ((1_u32 << bits) - 1) as f32,
            _ => 1.0 / 16_777_216.0,
        };
        Fragments {
            draw,
            pipeline,
            target,
            flat,
            machine: draw.machine(&draw.fragment),
            bounds,
            wide_bounds,
            passed: 0,
            coord: module.builtins["gl_FragCoord"],
            front_facing: module.builtins["gl_FrontFacing"],
            point_coord: module.builtins["gl_PointCoord"],
            depth_unit,
        }
    }

    fn corner(shaded: &Shaded, place: usize) -> Corner {
        Corner {
            clip: shaded.positions[place],
            words: shaded.words[place * shaded.stride..(place + 1) * shaded.stride].to_vec(),
        }
    }

    fn window(&self, corner: Corner) -> Windowed {
        let [x, y, width, height] = self.pipeline.viewport;
        let [near, far] = self.pipeline.depth_range;
        let inverse_w = 1.0 / corner.clip[3];
        let (nx, ny, nz) = (
            corner.clip[0] * inverse_w,
            corner.clip[1] * inverse_w,
            corner.clip[2] * inverse_w,
        );
        Windowed {
            x: (nx + 1.0) * 0.5 * width as f32 + x as f32,
            y: (ny + 1.0) * 0.5 * height as f32 + y as f32,
            z: nz * 0.5 * (far - near) + 0.5 * (near + far),
            inverse_w,
            words: corner.words,
        }
    }

    fn triangle(&mut self, shaded: &Shaded, places: [usize; 3]) {
        let mut corners: Vec<Corner> = places
            .iter()
            .map(|&place| Self::corner(shaded, place))
            .collect();
        // flat varyings come from the last vertex, the provoking one
        for (index, &flat) in self.flat.iter().enumerate() {
            if flat {
                let value = corners[2].words[index];
                corners[0].words[index] = value;
                corners[1].words[index] = value;
            }
        }
        let polygon = clip_polygon(corners, self.flat);
        if polygon.len() < 3 {
            return;
        }
        let windowed: Vec<Windowed> = polygon
            .into_iter()
            .map(|corner| self.window(corner))
            .collect();
        for index in 1..windowed.len() - 1 {
            self.fill([&windowed[0], &windowed[index], &windowed[index + 1]]);
        }
    }

    // rasterizes one triangle in window space with 8 bits of subpixel precision and the
    // top-left rule, so triangles that share an edge cover each pixel once
    fn fill(&mut self, corners: [&Windowed; 3]) {
        const SUBPIXEL: f32 = 256.0;
        let fixed = |value: f32| (value * SUBPIXEL).round() as i64;
        let points: [(i64, i64); 3] = corners.map(|corner| (fixed(corner.x), fixed(corner.y)));
        let area = (points[1].0 - points[0].0) * (points[2].1 - points[0].1)
            - (points[2].0 - points[0].0) * (points[1].1 - points[0].1);
        if area == 0 {
            return;
        }
        let counter_clockwise = area > 0;
        let front = counter_clockwise == (self.pipeline.front_face == CCW);
        match self.pipeline.cull {
            Some(FRONT_AND_BACK) => return,
            Some(FRONT) if front => return,
            Some(BACK) if !front => return,
            _ => {}
        }
        // order the corners counter-clockwise so every edge function is positive inside
        let order = if counter_clockwise {
            [0, 1, 2]
        } else {
            [0, 2, 1]
        };
        let p = order.map(|index| points[index]);
        let c = order.map(|index| corners[index]);
        let area = area.abs();
        // edge i runs from corner i+1 to corner i+2, and is the one opposite corner i
        let edges: [(i64, i64, i64); 3] = std::array::from_fn(|index| {
            let (from, to) = (p[(index + 1) % 3], p[(index + 2) % 3]);
            let (a, b) = (from.1 - to.1, to.0 - from.0);
            // top or left edges keep the pixels exactly on them
            let top_left = (a == 0 && b < 0) || a > 0;
            (a, b, -(a * from.0 + b * from.1) - i64::from(!top_left))
        });
        let min_x = p.iter().map(|point| point.0).min().unwrap_or(0);
        let max_x = p.iter().map(|point| point.0).max().unwrap_or(0);
        let min_y = p.iter().map(|point| point.1).min().unwrap_or(0);
        let max_y = p.iter().map(|point| point.1).max().unwrap_or(0);
        let to_pixel = |value: i64| (value as f64 / f64::from(SUBPIXEL)).floor() as i32;
        let x0 = to_pixel(min_x).max(self.bounds[0]);
        let x1 = (to_pixel(max_x) + 1).min(self.bounds[2]);
        let y0 = to_pixel(min_y).max(self.bounds[1]);
        let y1 = (to_pixel(max_y) + 1).min(self.bounds[3]);
        if x0 >= x1 || y0 >= y1 {
            return;
        }
        let offset = self.pipeline.polygon_offset.map_or(0.0, |[factor, units]| {
            // the depth slope from the plane through the three corners
            let (ax, ay, az) = (c[1].x - c[0].x, c[1].y - c[0].y, c[1].z - c[0].z);
            let (bx, by, bz) = (c[2].x - c[0].x, c[2].y - c[0].y, c[2].z - c[0].z);
            let determinant = ax * by - bx * ay;
            let (dzdx, dzdy) = if determinant == 0.0 {
                (0.0, 0.0)
            } else {
                (
                    (az * by - bz * ay) / determinant,
                    (ax * bz - bx * az) / determinant,
                )
            };
            factor * dzdx.abs().max(dzdy.abs()) + units * self.depth_unit
        });
        let half = (SUBPIXEL / 2.0) as i64;
        let scale = 1.0 / area as f32;
        let evaluate = |x: i32, y: i32| -> [i64; 3] {
            let (px, py) = (
                i64::from(x) * SUBPIXEL as i64 + half,
                i64::from(y) * SUBPIXEL as i64 + half,
            );
            edges.map(|(a, b, constant)| a * px + b * py + constant)
        };
        let mut y = y0 & !1;
        while y < y1 {
            let mut x = x0 & !1;
            while x < x1 {
                let positions = [(x, y), (x + 1, y), (x, y + 1), (x + 1, y + 1)];
                let mut mask = 0_u8;
                let mut weights = [[0.0_f32; 3]; LANES];
                for (lane, &(px, py)) in positions.iter().enumerate() {
                    let values = evaluate(px, py);
                    let inside = px >= x0
                        && px < x1
                        && py >= y0
                        && py < y1
                        && values.iter().all(|&value| value >= 0);
                    if inside {
                        mask |= 1 << lane;
                    }
                    // the barycentric weights without the tie-breaking bias, for helpers too
                    let (cx, cy) = (f64::from(px) + 0.5, f64::from(py) + 0.5);
                    weights[lane] = std::array::from_fn(|index| {
                        let (from, to) = (p[(index + 1) % 3], p[(index + 2) % 3]);
                        let (a, b) = ((from.1 - to.1) as f64, (to.0 - from.0) as f64);
                        let value = a * cx * f64::from(SUBPIXEL) + b * cy * f64::from(SUBPIXEL)
                            - (a * from.0 as f64 + b * from.1 as f64);
                        value as f32 * scale
                    });
                }
                if mask != 0 {
                    self.shade_quad(&positions, mask, &weights, &c, front, offset, None);
                }
                x += 2;
            }
            y += 2;
        }
    }

    fn line(&mut self, shaded: &Shaded, a: usize, b: usize) {
        let mut corners = [Self::corner(shaded, a), Self::corner(shaded, b)];
        for (index, &flat) in self.flat.iter().enumerate() {
            if flat {
                corners[0].words[index] = corners[1].words[index];
            }
        }
        // clip the segment against the same planes as polygons
        let [mut start, mut end] = corners;
        for plane in PLANES {
            let (d0, d1) = (plane(&start.clip), plane(&end.clip));
            if d0 < 0.0 && d1 < 0.0 {
                return;
            }
            if d0 < 0.0 {
                start = lerp_corner(&start, &end, d0 / (d0 - d1), self.flat);
            } else if d1 < 0.0 {
                end = lerp_corner(&start, &end, d0 / (d0 - d1), self.flat);
            }
        }
        let (start, end) = (self.window(start), self.window(end));
        let (dx, dy) = (end.x - start.x, end.y - start.y);
        let x_major = dx.abs() >= dy.abs();
        let length = if x_major { dx } else { dy };
        if length == 0.0 {
            return;
        }
        let (from, to) = if x_major {
            (start.x, end.x)
        } else {
            (start.y, end.y)
        };
        // pixel centers in [from, to) along the major axis, the half-open diamond rule
        let (low, high) = if from < to { (from, to) } else { (to, from) };
        let first = (low - 0.5).ceil() as i32;
        let last = (high - 0.5).ceil() as i32;
        let corners = [&start, &end, &end];
        let bounds = self.wide_bounds;
        for major in first..last {
            let center = major as f32 + 0.5;
            if (from < to && center < from) || (from > to && center <= to) {
                continue;
            }
            let t = (center - from) / (to - from);
            let minor = if x_major {
                start.y + t * dy
            } else {
                start.x + t * dx
            };
            let minor = minor.floor() as i32;
            let (x, y) = if x_major {
                (major, minor)
            } else {
                (minor, major)
            };
            if x < bounds[0] || x >= bounds[2] || y < bounds[1] || y >= bounds[3] {
                continue;
            }
            let positions = [(x, y), (x + 1, y), (x, y + 1), (x + 1, y + 1)];
            let weight_at = |px: i32, py: i32| -> [f32; 3] {
                let along = if x_major {
                    (px as f32 + 0.5 - from) / (to - from)
                } else {
                    (py as f32 + 0.5 - from) / (to - from)
                };
                [1.0 - along, along, 0.0]
            };
            let weights = positions.map(|(px, py)| weight_at(px, py));
            self.shade_quad(&positions, 1, &weights, &corners, true, 0.0, None);
        }
    }

    fn point(&mut self, shaded: &Shaded, place: usize) {
        let corner = Self::corner(shaded, place);
        let clip = corner.clip;
        if clip[3] <= 0.0 || clip[2] < -clip[3] || clip[2] > clip[3] {
            return;
        }
        let size = shaded.sizes[place].clamp(1.0, 1024.0);
        let center = self.window(corner);
        let (left, bottom) = (center.x - size / 2.0, center.y - size / 2.0);
        let bounds = self.wide_bounds;
        let x0 = ((left - 0.5).ceil() as i32).max(bounds[0]);
        let x1 = ((left + size - 0.5).ceil() as i32).min(bounds[2]);
        let y0 = ((bottom - 0.5).ceil() as i32).max(bounds[1]);
        let y1 = ((bottom + size - 0.5).ceil() as i32).min(bounds[3]);
        let corners = [&center, &center, &center];
        let mut y = y0 & !1;
        while y < y1 {
            let mut x = x0 & !1;
            while x < x1 {
                let positions = [(x, y), (x + 1, y), (x, y + 1), (x + 1, y + 1)];
                let mut mask = 0;
                for (lane, &(px, py)) in positions.iter().enumerate() {
                    if px >= x0 && px < x1 && py >= y0 && py < y1 {
                        mask |= 1 << lane;
                    }
                }
                if mask != 0 {
                    let coords = positions.map(|(px, py)| {
                        [
                            (px as f32 + 0.5 - left) / size,
                            1.0 - (py as f32 + 0.5 - bottom) / size,
                        ]
                    });
                    self.shade_quad(
                        &positions,
                        mask,
                        &[[1.0, 0.0, 0.0]; LANES],
                        &corners,
                        true,
                        0.0,
                        Some(coords),
                    );
                }
                x += 2;
            }
            y += 2;
        }
    }

    // fills the fragment shader's inputs for a quad, runs it and sends the covered lanes on
    #[allow(clippy::too_many_arguments)]
    fn shade_quad(
        &mut self,
        positions: &[(i32, i32); LANES],
        mask: u8,
        weights: &[[f32; 3]; LANES],
        corners: &[&Windowed; 3],
        front: bool,
        offset: f32,
        point: Option<[[f32; 2]; LANES]>,
    ) {
        let mut depths = [0.0_f32; LANES];
        let mut mask = mask;
        for lane in 0..LANES {
            let w = weights[lane];
            let inverse_w = w[0] * corners[0].inverse_w
                + w[1] * corners[1].inverse_w
                + w[2] * corners[2].inverse_w;
            let z = w[0] * corners[0].z + w[1] * corners[1].z + w[2] * corners[2].z + offset;
            depths[lane] = z.clamp(0.0, 1.0);
            let (x, y) = positions[lane];
            self.machine.set_float(self.coord, lane, x as f32 + 0.5);
            self.machine.set_float(self.coord + 1, lane, y as f32 + 0.5);
            self.machine.set_float(self.coord + 2, lane, depths[lane]);
            self.machine.set_float(self.coord + 3, lane, inverse_w);
            self.machine.set(self.front_facing, lane, u32::from(front));
            if let Some(coords) = point {
                self.machine
                    .set_float(self.point_coord, lane, coords[lane][0]);
                self.machine
                    .set_float(self.point_coord + 1, lane, coords[lane][1]);
            }
            // perspective-correct weights
            let perspective = if inverse_w != 0.0 {
                [
                    w[0] * corners[0].inverse_w / inverse_w,
                    w[1] * corners[1].inverse_w / inverse_w,
                    w[2] * corners[2].inverse_w / inverse_w,
                ]
            } else {
                w
            };
            let mut word = 0;
            for link in &self.draw.links {
                for index in 0..link.count {
                    let value = if self.flat[word] {
                        corners[0].words[word]
                    } else {
                        let read = |corner: &Windowed| f32::from_bits(corner.words[word]);
                        (perspective[0] * read(corners[0])
                            + perspective[1] * read(corners[1])
                            + perspective[2] * read(corners[2]))
                        .to_bits()
                    };
                    self.machine.set(link.to + index, lane, value);
                    word += 1;
                }
            }
        }
        // with no shader depth and no stencil, fragments that fail the depth test need no shading
        if self.draw.frag_depth.is_none()
            && self.pipeline.stencil.is_none()
            && let (Some(func), Some(view)) =
                (self.pipeline.depth_test, self.target.depth_stencil.as_ref())
            && view.format.has_depth()
        {
            for (lane, &(x, y)) in positions.iter().enumerate() {
                if mask & (1 << lane) != 0
                    && !depth_passes(func, depths[lane], view.get(x as u32, y as u32)[0])
                {
                    mask &= !(1 << lane);
                }
            }
            if mask == 0 {
                return;
            }
        }
        let module = self.draw.fragment.module;
        let discarded = module.run(&mut self.machine, ALL);
        let alive = mask & !discarded;
        for lane in 0..LANES {
            if alive & (1 << lane) == 0 {
                continue;
            }
            let depth = match self.draw.frag_depth {
                Some(slot) => self.machine.float(slot, lane).clamp(0.0, 1.0),
                None => depths[lane],
            };
            let (x, y) = positions[lane];
            self.write(x as u32, y as u32, depth, front, lane);
        }
    }

    // the stencil and depth tests, then blending and the color mask into every draw buffer
    fn write(&mut self, x: u32, y: u32, depth: f32, front: bool, lane: usize) {
        let pipeline = self.pipeline;
        if let Some(level) = self.target.depth_stencil.as_mut() {
            let stored = level.get(x, y);
            let mut written = stored;
            let stencil_face = pipeline
                .stencil
                .filter(|_| level.format.has_stencil())
                .map(|faces| faces[usize::from(!front)]);
            if let Some(face) = stencil_face {
                let value = stored[1] as u32;
                let reference = face.reference.clamp(0, 255) as u32;
                if !compare(
                    face.func,
                    reference & face.value_mask,
                    value & face.value_mask,
                ) {
                    written[1] = stencil_op(face.fail, value, reference, face.write_mask);
                    level.set(x, y, written);
                    return;
                }
            }
            let depth_ok = match pipeline.depth_test.filter(|_| level.format.has_depth()) {
                Some(func) => depth_passes(func, depth, stored[0]),
                None => true,
            };
            if let Some(face) = stencil_face {
                let value = stored[1] as u32;
                let reference = face.reference.clamp(0, 255) as u32;
                let op = if depth_ok { face.pass } else { face.depth_fail };
                written[1] = stencil_op(op, value, reference, face.write_mask);
            }
            if depth_ok
                && pipeline.depth_test.is_some()
                && pipeline.depth_mask
                && level.format.has_depth()
            {
                written[0] = level.format.store([depth, 0.0, 0.0, 0.0])[0];
            }
            if written != stored {
                level.set(x, y, written);
            }
            if !depth_ok {
                return;
            }
        }
        self.passed += 1;
        let mask = pipeline.color_mask;
        if mask == [false; 4] {
            return;
        }
        for &(slot, buffer) in &self.draw.outputs {
            let Some(Some(level)) = self.target.colors.get_mut(buffer) else {
                continue;
            };
            let format = level.format;
            let destination = level.get(x, y);
            let source: [f32; 4] = std::array::from_fn(|channel| {
                let word = self.machine.get(slot + channel, lane);
                match format.kind {
                    Kind::Int => word as i32 as f32,
                    Kind::Uint => word as f32,
                    _ => f32::from_bits(word),
                }
            });
            let blended = match pipeline.blend.filter(|_| !format.is_integer()) {
                Some(blend) => blend_color(
                    &blend,
                    source,
                    destination,
                    matches!(format.kind, Kind::Unorm | Kind::Srgb),
                ),
                None => source,
            };
            let stored = format.store(channels(format.base, blended));
            let texel = std::array::from_fn(|channel| {
                if mask[channel] {
                    stored[channel]
                } else {
                    destination[channel]
                }
            });
            level.set(x, y, texel);
        }
    }
}

fn depth_passes(func: u32, incoming: f32, stored: f32) -> bool {
    compare(func, incoming, stored)
}

pub(crate) fn compare<T: PartialOrd>(func: u32, incoming: T, stored: T) -> bool {
    match func {
        NEVER => false,
        LESS => incoming < stored,
        EQUAL => incoming == stored,
        LEQUAL => incoming <= stored,
        GREATER => incoming > stored,
        NOTEQUAL => incoming != stored,
        GEQUAL => incoming >= stored,
        _ => true,
    }
}

fn stencil_op(op: u32, value: u32, reference: u32, write_mask: u32) -> f32 {
    let result = match op {
        ZERO => 0,
        REPLACE => reference,
        INCR => (value + 1).min(255),
        DECR => value.saturating_sub(1),
        INVERT => !value & 255,
        INCR_WRAP => (value + 1) & 255,
        DECR_WRAP => value.wrapping_sub(1) & 255,
        _ => value,
    };
    ((value & !write_mask) | (result & write_mask) & 255) as f32
}

fn blend_color(blend: &Blend, source: [f32; 4], destination: [f32; 4], clamp: bool) -> [f32; 4] {
    let fix = |color: [f32; 4]| {
        if clamp {
            color.map(|value| value.clamp(0.0, 1.0))
        } else {
            color
        }
    };
    let (source, destination, constant) = (fix(source), fix(destination), fix(blend.color));
    let factor = |name: u32, channel: usize| -> f32 {
        match name {
            ZERO => 0.0,
            ONE => 1.0,
            SRC_COLOR => source[channel],
            ONE_MINUS_SRC_COLOR => 1.0 - source[channel],
            DST_COLOR => destination[channel],
            ONE_MINUS_DST_COLOR => 1.0 - destination[channel],
            SRC_ALPHA => source[3],
            ONE_MINUS_SRC_ALPHA => 1.0 - source[3],
            DST_ALPHA => destination[3],
            ONE_MINUS_DST_ALPHA => 1.0 - destination[3],
            CONSTANT_COLOR => constant[channel],
            ONE_MINUS_CONSTANT_COLOR => 1.0 - constant[channel],
            CONSTANT_ALPHA => constant[3],
            ONE_MINUS_CONSTANT_ALPHA => 1.0 - constant[3],
            SRC_ALPHA_SATURATE => {
                if channel == 3 {
                    1.0
                } else {
                    source[3].min(1.0 - destination[3])
                }
            }
            _ => 0.0,
        }
    };
    let blended: [f32; 4] = std::array::from_fn(|channel| {
        let which = usize::from(channel == 3);
        let (s, d) = (
            source[channel] * factor(blend.source[which], channel),
            destination[channel] * factor(blend.destination[which], channel),
        );
        match blend.equation[which] {
            FUNC_SUBTRACT => s - d,
            FUNC_REVERSE_SUBTRACT => d - s,
            MIN => source[channel].min(destination[channel]),
            MAX => source[channel].max(destination[channel]),
            _ => s + d,
        }
    });
    fix(blended)
}

#[cfg(test)]
mod tests {
    use super::super::glsl::{NoTextures, Stage as ShaderStage, compile};
    use super::super::texture::Format;
    use super::*;

    fn pipeline() -> Pipeline {
        Pipeline {
            viewport: [0, 0, 8, 8],
            depth_range: [0.0, 1.0],
            scissor: None,
            cull: None,
            front_face: CCW,
            polygon_offset: None,
            depth_test: None,
            depth_mask: true,
            stencil: None,
            blend: None,
            color_mask: [true; 4],
            rasterizer_discard: false,
        }
    }

    #[test]
    fn triangles_cover_pixels_once_and_interpolate() {
        let vertex = compile("attribute vec2 p; attribute vec3 c; varying vec3 v; void main() { v = c; gl_Position = vec4(p, 0.0, 1.0); }", ShaderStage::Vertex, &[]).unwrap_or_else(|log| panic!("{log}"));
        let fragment = compile(
            "precision mediump float; varying vec3 v; void main() { gl_FragColor = vec4(v, 1.0); }",
            ShaderStage::Fragment,
            &[],
        )
        .unwrap_or_else(|log| panic!("{log}"));
        // two triangles making a square over the whole viewport, red on the left corners
        let positions: Vec<u8> = [-1.0_f32, -1.0, 1.0, -1.0, 1.0, 1.0, -1.0, 1.0]
            .iter()
            .flat_map(|value| value.to_le_bytes())
            .collect();
        let colors: Vec<u8> = [
            1.0_f32, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 1.0, 0.0, 0.0,
        ]
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect();
        let slot = |module: &Module, name: &str| {
            module
                .inputs
                .iter()
                .find(|input| input.name == name)
                .expect("an input")
                .slot
        };
        let draw = Draw {
            mode: TRIANGLES,
            indices: &[0, 1, 2, 0, 2, 3],
            instances: 1,
            vertex: Stage {
                module: &vertex,
                memory: &vertex.initial,
            },
            fragment: Stage {
                module: &fragment,
                memory: &fragment.initial,
            },
            fetches: vec![
                Fetch {
                    slot: slot(&vertex, "p"),
                    words: 2,
                    base: Base::Float,
                    source: Source::Buffer {
                        data: &positions,
                        size: 2,
                        kind: FLOAT,
                        normalized: false,
                        integer: false,
                        stride: 8,
                        offset: 0,
                        divisor: 0,
                    },
                },
                Fetch {
                    slot: slot(&vertex, "c"),
                    words: 3,
                    base: Base::Float,
                    source: Source::Buffer {
                        data: &colors,
                        size: 3,
                        kind: FLOAT,
                        normalized: false,
                        integer: false,
                        stride: 12,
                        offset: 0,
                        divisor: 0,
                    },
                },
            ],
            links: vec![Link {
                from: vertex.outputs[0].slot,
                to: fragment.inputs[0].slot,
                count: 3,
                flat: false,
            }],
            outputs: vec![(fragment.builtins["gl_FragColor"], 0)],
            frag_depth: None,
            textures: &NoTextures,
            capture: Vec::new(),
        };
        let format = Format::sized(RGBA32F).expect("a float format");
        let mut color = Level::new(format, 8, 8, 1);
        let target = Target {
            width: 8,
            height: 8,
            colors: vec![Some(View::layer(&mut color, 0))],
            depth_stencil: None,
        };
        let outcome = draw.run(&pipeline(), target);
        assert_eq!(outcome.samples_passed, 64);
        let near = |found: [f32; 4], expected: [f32; 4]| {
            found
                .iter()
                .zip(expected)
                .all(|(a, b)| (a - b).abs() < 1e-5)
        };
        assert!(
            near(color.texel(0, 0, 0), [0.9375, 0.0, 0.0625, 1.0]),
            "{:?}",
            color.texel(0, 0, 0)
        );
        assert!(
            near(color.texel(7, 7, 0), [0.0625, 0.0, 0.9375, 1.0]),
            "{:?}",
            color.texel(7, 7, 0)
        );

        // a target large enough to be drawn in bands on several threads
        let mut large = Level::new(format, 256, 256, 1);
        let target = Target {
            width: 256,
            height: 256,
            colors: vec![Some(View::layer(&mut large, 0))],
            depth_stencil: None,
        };
        let outcome = draw.run(
            &Pipeline {
                viewport: [0, 0, 256, 256],
                ..pipeline()
            },
            target,
        );
        assert_eq!(outcome.samples_passed, 256 * 256);
        for (x, y) in [(0, 0), (100, 77), (3, 200), (255, 255)] {
            let red = 1.0 - (x as f32 + 0.5) / 256.0;
            assert!(
                near(large.texel(x, y, 0), [red, 0.0, 1.0 - red, 1.0]),
                "{x},{y}: {:?}",
                large.texel(x, y, 0)
            );
        }
    }
}
