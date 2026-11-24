// the GL state machine behind a WebGL context: objects by name, binding points, capabilities
// and the error flags, and the operations that move pixels; everything here takes plain Rust
// values, so the script bindings only convert arguments and wrap objects

use super::constants::*;
use super::glsl::{self, Base, LANES, Lod, Module, SamplerKind, Stage, Textures, Ty};
use super::raster::{
    Blend, Draw, Fetch, Link, Outcome, Pipeline, Source, Stage as DrawStage, StencilFace, Target,
    View,
};
use super::texture::{
    Bound, Format, Kind, Layout, Level, Params, Texture, channels, pack_pixel, pixel_size, unpack,
};
use crate::hashing::HashMap;
use std::collections::BTreeMap;
use std::rc::Rc;

pub(crate) const MAX_TEXTURE_SIZE_VALUE: u32 = 8192;
pub(crate) const MAX_CUBE_SIZE: u32 = 8192;
pub(crate) const MAX_3D_SIZE: u32 = 2048;
pub(crate) const MAX_LAYERS: u32 = 2048;
pub(crate) const MAX_RENDERBUFFER: u32 = 8192;
pub(crate) const MAX_VIEWPORT: u32 = 8192;
pub(crate) const VERTEX_ATTRIBS: usize = 16;
pub(crate) const TEXTURE_UNITS: usize = 32;
pub(crate) const COLOR_ATTACHMENTS: usize = 8;
pub(crate) const UNIFORM_BUFFER_BINDINGS: usize = 24;
pub(crate) const FEEDBACK_BUFFERS: usize = 4;

pub(crate) struct Buffer {
    pub(crate) data: Vec<u8>,
    pub(crate) usage: u32,
    // whether it has held indices, which WebGL keeps apart from every other use
    pub(crate) element: Option<bool>,
}

pub(crate) struct Renderbuffer {
    pub(crate) level: Option<Level>,
    pub(crate) internal: u32,
    pub(crate) samples: u32,
    pub(crate) bound: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Attachment {
    Texture {
        id: u32,
        face: usize,
        level: u32,
        layer: u32,
    },
    Renderbuffer(u32),
}

#[derive(Default)]
pub(crate) struct Framebuffer {
    pub(crate) attachments: BTreeMap<u32, Attachment>,
    pub(crate) draw_buffers: Vec<u32>,
    pub(crate) read_buffer: u32,
    pub(crate) bound: bool,
}

pub(crate) struct Shader {
    pub(crate) kind: u32,
    pub(crate) source: String,
    pub(crate) module: Option<Rc<Module>>,
    pub(crate) compiled: bool,
    pub(crate) log: String,
    pub(crate) delete_pending: bool,
    pub(crate) attached: u32,
}

#[derive(Clone)]
pub(crate) struct Uniform {
    // as getActiveUniform reports it, with [0] after an array's name
    pub(crate) name: String,
    pub(crate) base_name: String,
    // the type of one element
    pub(crate) ty: Ty,
    pub(crate) size: usize,
    // the slot of element 0 in the vertex and fragment modules
    pub(crate) slots: [Option<usize>; 2],
    pub(crate) array: bool,
    pub(crate) block: Option<usize>,
    pub(crate) offset: usize,
    pub(crate) array_stride: usize,
    pub(crate) matrix_stride: usize,
}

pub(crate) struct Block {
    pub(crate) name: String,
    pub(crate) binding: u32,
    pub(crate) size: usize,
    pub(crate) members: Vec<usize>,
    pub(crate) stages: [bool; 2],
}

pub(crate) struct Attribute {
    pub(crate) name: String,
    pub(crate) ty: Ty,
    pub(crate) location: u32,
    pub(crate) slot: usize,
}

pub(crate) struct Linked {
    pub(crate) vertex: Rc<Module>,
    pub(crate) fragment: Rc<Module>,
    pub(crate) memory: [Vec<u32>; 2],
    pub(crate) attributes: Vec<Attribute>,
    pub(crate) uniforms: Vec<Uniform>,
    // a uniform location is an index here: the uniform and the element
    pub(crate) locations: Vec<(usize, usize)>,
    pub(crate) blocks: Vec<Block>,
    pub(crate) links: Vec<Link>,
    pub(crate) outputs: Vec<(usize, usize)>,
    // the type each draw buffer's output has, for the attachment type checks
    pub(crate) output_bases: Vec<(usize, Base)>,
    pub(crate) frag_depth: Option<usize>,
    pub(crate) fragment_locations: HashMap<String, i32>,
    pub(crate) feedback: Vec<(String, Ty, usize, usize)>,
    pub(crate) feedback_mode: u32,
}

#[derive(Default)]
pub(crate) struct Program {
    pub(crate) shaders: Vec<u32>,
    pub(crate) bindings: HashMap<String, u32>,
    pub(crate) linked: Option<Linked>,
    pub(crate) link_status: bool,
    pub(crate) validate_status: bool,
    pub(crate) log: String,
    pub(crate) delete_pending: bool,
    // bumped by each link, so locations from an earlier link stop working
    pub(crate) generation: u32,
    pub(crate) feedback_varyings: Vec<String>,
    pub(crate) feedback_mode: u32,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct AttribArray {
    pub(crate) enabled: bool,
    pub(crate) size: i32,
    pub(crate) kind: u32,
    pub(crate) normalized: bool,
    pub(crate) integer: bool,
    pub(crate) stride: i32,
    pub(crate) offset: u32,
    pub(crate) buffer: u32,
    pub(crate) divisor: u32,
}

impl Default for AttribArray {
    fn default() -> Self {
        AttribArray {
            enabled: false,
            size: 4,
            kind: FLOAT,
            normalized: false,
            integer: false,
            stride: 0,
            offset: 0,
            buffer: 0,
            divisor: 0,
        }
    }
}

impl AttribArray {
    pub(crate) fn element_size(&self) -> usize {
        let component = match self.kind {
            BYTE | UNSIGNED_BYTE => 1,
            SHORT | UNSIGNED_SHORT | HALF_FLOAT | HALF_FLOAT_OES => 2,
            INT_2_10_10_10_REV | UNSIGNED_INT_2_10_10_10_REV => return 4,
            _ => 4,
        };
        component * self.size as usize
    }

    pub(crate) fn effective_stride(&self) -> usize {
        if self.stride == 0 {
            self.element_size()
        } else {
            self.stride as usize
        }
    }
}

#[derive(Clone)]
pub(crate) struct VertexArray {
    pub(crate) attribs: Vec<AttribArray>,
    pub(crate) element: u32,
    pub(crate) bound: bool,
}

impl Default for VertexArray {
    fn default() -> Self {
        VertexArray {
            attribs: vec![AttribArray::default(); VERTEX_ATTRIBS],
            element: 0,
            bound: false,
        }
    }
}

#[derive(Clone, Copy, Default)]
pub(crate) struct Range {
    pub(crate) buffer: u32,
    pub(crate) offset: u64,
    // zero means the whole buffer
    pub(crate) size: u64,
}

pub(crate) struct Feedback {
    pub(crate) buffers: Vec<Range>,
    pub(crate) active: bool,
    pub(crate) paused: bool,
    pub(crate) program: u32,
    pub(crate) mode: u32,
    // where the next captured vertex goes in each buffer
    pub(crate) written: Vec<u64>,
    pub(crate) bound: bool,
}

impl Default for Feedback {
    fn default() -> Self {
        Feedback {
            buffers: vec![Range::default(); FEEDBACK_BUFFERS],
            active: false,
            paused: false,
            program: 0,
            mode: 0,
            written: vec![0; FEEDBACK_BUFFERS],
            bound: false,
        }
    }
}

pub(crate) struct Query {
    pub(crate) target: u32,
    pub(crate) result: u64,
    pub(crate) active: bool,
}

pub(crate) struct SamplerObject {
    pub(crate) params: Params,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Capability {
    Blend,
    CullFace,
    DepthTest,
    Dither,
    PolygonOffsetFill,
    SampleAlphaToCoverage,
    SampleCoverage,
    ScissorTest,
    StencilTest,
    RasterizerDiscard,
}

pub(crate) fn capability(name: u32, webgl2: bool) -> Option<Capability> {
    Some(match name {
        BLEND => Capability::Blend,
        CULL_FACE => Capability::CullFace,
        DEPTH_TEST => Capability::DepthTest,
        DITHER => Capability::Dither,
        POLYGON_OFFSET_FILL => Capability::PolygonOffsetFill,
        SAMPLE_ALPHA_TO_COVERAGE => Capability::SampleAlphaToCoverage,
        SAMPLE_COVERAGE => Capability::SampleCoverage,
        SCISSOR_TEST => Capability::ScissorTest,
        STENCIL_TEST => Capability::StencilTest,
        RASTERIZER_DISCARD if webgl2 => Capability::RasterizerDiscard,
        _ => return None,
    })
}

pub(crate) struct Context {
    pub(crate) webgl2: bool,
    pub(crate) alpha: bool,
    pub(crate) premultiplied: bool,
    pub(crate) preserve: bool,
    pub(crate) errors: Vec<u32>,
    pub(crate) lost: bool,
    next: u32,
    pub(crate) extensions: Vec<&'static str>,

    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) color: Level,
    pub(crate) depth_stencil: Option<Level>,
    // the drawing buffer changed since paint last took it
    pub(crate) dirty: bool,
    // the drawing buffer is to be cleared before its next use, as after compositing
    pub(crate) clear_pending: bool,
    pub(crate) default_draw_buffer: u32,
    pub(crate) default_read_buffer: u32,

    pub(crate) buffers: HashMap<u32, Buffer>,
    pub(crate) textures: HashMap<u32, Texture>,
    pub(crate) renderbuffers: HashMap<u32, Renderbuffer>,
    pub(crate) framebuffers: HashMap<u32, Framebuffer>,
    pub(crate) shaders: HashMap<u32, Shader>,
    pub(crate) programs: HashMap<u32, Program>,
    pub(crate) vertex_arrays: HashMap<u32, VertexArray>,
    pub(crate) queries: HashMap<u32, Query>,
    pub(crate) samplers: HashMap<u32, SamplerObject>,
    pub(crate) syncs: HashMap<u32, ()>,
    pub(crate) feedbacks: HashMap<u32, Feedback>,

    pub(crate) array_buffer: u32,
    pub(crate) copy_read: u32,
    pub(crate) copy_write: u32,
    pub(crate) pixel_pack: u32,
    pub(crate) pixel_unpack: u32,
    pub(crate) uniform_buffer: u32,
    pub(crate) uniform_ranges: Vec<Range>,
    pub(crate) feedback_buffer: u32,
    pub(crate) feedback: u32,
    pub(crate) vertex_array: u32,
    pub(crate) generic: Vec<([u32; 4], Base)>,
    pub(crate) active_texture: u32,
    // per unit: 2D, cube map, 3D and 2D array bindings
    pub(crate) units: Vec<[u32; 4]>,
    pub(crate) unit_samplers: Vec<u32>,
    pub(crate) draw_framebuffer: u32,
    pub(crate) read_framebuffer: u32,
    pub(crate) renderbuffer: u32,
    pub(crate) program: u32,
    pub(crate) active_queries: HashMap<u32, u32>,

    pub(crate) enabled: Vec<Capability>,
    pub(crate) blend: Blend,
    pub(crate) color_mask: [bool; 4],
    pub(crate) depth_func: u32,
    pub(crate) depth_mask: bool,
    pub(crate) depth_range: [f32; 2],
    pub(crate) clear_color: [f32; 4],
    pub(crate) clear_depth: f32,
    pub(crate) clear_stencil: i32,
    pub(crate) stencil: [StencilFace; 2],
    pub(crate) viewport: [i32; 4],
    pub(crate) scissor: [i32; 4],
    pub(crate) cull_face: u32,
    pub(crate) front_face: u32,
    pub(crate) line_width: f32,
    pub(crate) polygon_offset: [f32; 2],
    pub(crate) sample_coverage: (f32, bool),
    pub(crate) hints: [u32; 2],
    pub(crate) pack: Layout,
    pub(crate) unpack: Layout,
    pub(crate) unpack_flip: bool,
    pub(crate) unpack_premultiply: bool,
    pub(crate) unpack_colorspace: u32,
}

// what clear and the draws aim at: the attachments of the bound draw framebuffer, taken out
// of their owners for the length of the operation
struct Taken {
    colors: Vec<Option<(Level, u32, Attachment)>>,
    depth_stencil: Option<(Level, Option<Attachment>)>,
}

fn kind_index(kind: SamplerKind) -> usize {
    match kind {
        SamplerKind::Two | SamplerKind::TwoShadow => 0,
        SamplerKind::Cube | SamplerKind::CubeShadow => 1,
        SamplerKind::Three => 2,
        SamplerKind::TwoArray | SamplerKind::TwoArrayShadow => 3,
    }
}

pub(crate) fn target_index(target: u32) -> Option<usize> {
    Some(match target {
        TEXTURE_2D => 0,
        TEXTURE_CUBE_MAP => 1,
        TEXTURE_3D => 2,
        TEXTURE_2D_ARRAY => 3,
        _ => return None,
    })
}

// the texture units a draw samples, one set of four targets each
struct Units<'a> {
    units: Vec<[Option<Bound<'a>>; 4]>,
}

impl Textures for Units<'_> {
    fn sample(
        &self,
        unit: u32,
        kind: SamplerKind,
        coords: &[[f32; 4]; LANES],
        lod: Lod,
        out: &mut [[f32; 4]; LANES],
    ) {
        match self
            .units
            .get(unit as usize)
            .and_then(|unit| unit[kind_index(kind)].as_ref())
        {
            Some(bound) => bound.sample(kind, coords, lod, out),
            None => *out = [[0.0, 0.0, 0.0, 1.0]; LANES],
        }
    }

    fn fetch(&self, unit: u32, kind: SamplerKind, coords: [i32; 4], level: i32) -> [f32; 4] {
        match self
            .units
            .get(unit as usize)
            .and_then(|unit| unit[kind_index(kind)].as_ref())
        {
            Some(bound) => bound.fetch(coords, level),
            None => [0.0; 4],
        }
    }

    fn size(&self, unit: u32, kind: SamplerKind, level: i32) -> [i32; 3] {
        match self
            .units
            .get(unit as usize)
            .and_then(|unit| unit[kind_index(kind)].as_ref())
        {
            Some(bound) => bound.size(level),
            None => [0; 3],
        }
    }
}

// the GL enum for a uniform or attribute type
pub(crate) fn type_enum(ty: &Ty) -> u32 {
    match ty {
        Ty::Vec(Base::Float, size) => {
            [FLOAT, FLOAT_VEC2, FLOAT_VEC3, FLOAT_VEC4][usize::from(*size) - 1]
        }
        Ty::Vec(Base::Int, size) => [INT, INT_VEC2, INT_VEC3, INT_VEC4][usize::from(*size) - 1],
        Ty::Vec(Base::Uint, size) => [
            UNSIGNED_INT,
            UNSIGNED_INT_VEC2,
            UNSIGNED_INT_VEC3,
            UNSIGNED_INT_VEC4,
        ][usize::from(*size) - 1],
        Ty::Vec(Base::Bool, size) => {
            [BOOL, BOOL_VEC2, BOOL_VEC3, BOOL_VEC4][usize::from(*size) - 1]
        }
        Ty::Mat(2, 2) => FLOAT_MAT2,
        Ty::Mat(3, 3) => FLOAT_MAT3,
        Ty::Mat(4, 4) => FLOAT_MAT4,
        Ty::Mat(2, 3) => FLOAT_MAT2x3,
        Ty::Mat(2, 4) => FLOAT_MAT2x4,
        Ty::Mat(3, 2) => FLOAT_MAT3x2,
        Ty::Mat(3, 4) => FLOAT_MAT3x4,
        Ty::Mat(4, 2) => FLOAT_MAT4x2,
        Ty::Mat(4, 3) => FLOAT_MAT4x3,
        Ty::Sampler(kind, base) => match (kind, base) {
            (SamplerKind::Two, Base::Float) => SAMPLER_2D,
            (SamplerKind::Cube, Base::Float) => SAMPLER_CUBE,
            (SamplerKind::Three, Base::Float) => SAMPLER_3D,
            (SamplerKind::TwoArray, Base::Float) => SAMPLER_2D_ARRAY,
            (SamplerKind::TwoShadow, _) => SAMPLER_2D_SHADOW,
            (SamplerKind::CubeShadow, _) => SAMPLER_CUBE_SHADOW,
            (SamplerKind::TwoArrayShadow, _) => SAMPLER_2D_ARRAY_SHADOW,
            (SamplerKind::Two, Base::Int) => INT_SAMPLER_2D,
            (SamplerKind::Three, Base::Int) => INT_SAMPLER_3D,
            (SamplerKind::Cube, Base::Int) => INT_SAMPLER_CUBE,
            (SamplerKind::TwoArray, Base::Int) => INT_SAMPLER_2D_ARRAY,
            (SamplerKind::Two, _) => UNSIGNED_INT_SAMPLER_2D,
            (SamplerKind::Three, _) => UNSIGNED_INT_SAMPLER_3D,
            (SamplerKind::Cube, _) => UNSIGNED_INT_SAMPLER_CUBE,
            (SamplerKind::TwoArray, _) => UNSIGNED_INT_SAMPLER_2D_ARRAY,
        },
        _ => 0,
    }
}

// a uniform's leaves: structs open into their fields and arrays of structs into elements,
// arrays of basic types stay whole
fn flatten(name: &str, ty: &Ty, slot: usize, out: &mut Vec<(String, Ty, usize)>) {
    match ty {
        Ty::Struct(def) => {
            let mut at = slot;
            for (field, field_ty) in &def.fields {
                flatten(&format!("{name}.{field}"), field_ty, at, out);
                at += field_ty.slots();
            }
        }
        Ty::Array(element, length) if matches!(**element, Ty::Struct(_) | Ty::Array(..)) => {
            for index in 0..*length {
                flatten(
                    &format!("{name}[{index}]"),
                    element,
                    slot + index * element.slots(),
                    out,
                );
            }
        }
        _ => out.push((name.to_string(), ty.clone(), slot)),
    }
}

// std140 alignment and size of a type, in bytes
fn std140(ty: &Ty) -> (usize, usize) {
    match ty {
        Ty::Vec(_, 1) => (4, 4),
        Ty::Vec(_, 2) => (8, 8),
        Ty::Vec(_, size) => (16, 4 * usize::from(*size)),
        Ty::Mat(columns, _) => (16, 16 * usize::from(*columns)),
        Ty::Array(element, length) => {
            let (align, size) = std140(element);
            let stride = size.div_ceil(16_usize.max(align)) * 16_usize.max(align);
            (16.max(align), stride * length)
        }
        Ty::Struct(def) => {
            let mut offset: usize = 0;
            let mut align: usize = 16;
            for (_, field) in &def.fields {
                let (field_align, size) = std140(field);
                align = align.max(field_align);
                offset = offset.div_ceil(field_align) * field_align + size;
            }
            (align, offset.div_ceil(16) * 16)
        }
        _ => (4, 4),
    }
}

impl Context {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        webgl2: bool,
        width: u32,
        height: u32,
        alpha: bool,
        depth: bool,
        stencil: bool,
        premultiplied: bool,
        preserve: bool,
    ) -> Context {
        let (color, depth_stencil) = Self::drawing_buffer(width, height, alpha, depth, stencil);
        let face = StencilFace {
            func: ALWAYS,
            reference: 0,
            value_mask: 0xffff_ffff,
            write_mask: 0xffff_ffff,
            fail: KEEP,
            depth_fail: KEEP,
            pass: KEEP,
        };
        let mut vertex_arrays = HashMap::default();
        vertex_arrays.insert(0, VertexArray::default());
        let mut feedbacks = HashMap::default();
        feedbacks.insert(0, Feedback::default());
        Context {
            webgl2,
            alpha,
            premultiplied,
            preserve,
            errors: Vec::new(),
            lost: false,
            next: 1,
            extensions: Vec::new(),
            width,
            height,
            color,
            depth_stencil,
            dirty: true,
            clear_pending: false,
            default_draw_buffer: BACK,
            default_read_buffer: BACK,
            buffers: HashMap::default(),
            textures: HashMap::default(),
            renderbuffers: HashMap::default(),
            framebuffers: HashMap::default(),
            shaders: HashMap::default(),
            programs: HashMap::default(),
            vertex_arrays,
            queries: HashMap::default(),
            samplers: HashMap::default(),
            syncs: HashMap::default(),
            feedbacks,
            array_buffer: 0,
            copy_read: 0,
            copy_write: 0,
            pixel_pack: 0,
            pixel_unpack: 0,
            uniform_buffer: 0,
            uniform_ranges: vec![Range::default(); UNIFORM_BUFFER_BINDINGS],
            feedback_buffer: 0,
            feedback: 0,
            vertex_array: 0,
            generic: vec![([0, 0, 0, 1.0_f32.to_bits()], Base::Float); VERTEX_ATTRIBS],
            active_texture: 0,
            units: vec![[0; 4]; TEXTURE_UNITS],
            unit_samplers: vec![0; TEXTURE_UNITS],
            draw_framebuffer: 0,
            read_framebuffer: 0,
            renderbuffer: 0,
            program: 0,
            active_queries: HashMap::default(),
            enabled: vec![Capability::Dither],
            blend: Blend {
                equation: [FUNC_ADD; 2],
                source: [ONE; 2],
                destination: [ZERO; 2],
                color: [0.0; 4],
            },
            color_mask: [true; 4],
            depth_func: LESS,
            depth_mask: true,
            depth_range: [0.0, 1.0],
            clear_color: [0.0; 4],
            clear_depth: 1.0,
            clear_stencil: 0,
            stencil: [face; 2],
            viewport: [0, 0, width as i32, height as i32],
            scissor: [0, 0, width as i32, height as i32],
            cull_face: BACK,
            front_face: CCW,
            line_width: 1.0,
            polygon_offset: [0.0; 2],
            sample_coverage: (1.0, false),
            hints: [DONT_CARE; 2],
            pack: Layout {
                alignment: 4,
                row_length: 0,
                image_height: 0,
                skip_pixels: 0,
                skip_rows: 0,
                skip_images: 0,
            },
            unpack: Layout {
                alignment: 4,
                row_length: 0,
                image_height: 0,
                skip_pixels: 0,
                skip_rows: 0,
                skip_images: 0,
            },
            unpack_flip: false,
            unpack_premultiply: false,
            unpack_colorspace: BROWSER_DEFAULT_WEBGL,
        }
    }

    fn drawing_buffer(
        width: u32,
        height: u32,
        alpha: bool,
        depth: bool,
        stencil: bool,
    ) -> (Level, Option<Level>) {
        let color = Level::new(
            Format::sized(if alpha { RGBA8 } else { RGB8 }).expect("a color format"),
            width,
            height,
            1,
        );
        let depth_format = match (depth, stencil) {
            (true, true) => Format::sized(DEPTH24_STENCIL8),
            (true, false) => Format::sized(DEPTH_COMPONENT24),
            (false, true) => Format::sized(STENCIL_INDEX8),
            (false, false) => None,
        };
        let depth_stencil = depth_format.map(|format| {
            let mut level = Level::new(format, width, height, 1);
            level.fill([1.0, 0.0, 0.0, 1.0]);
            level
        });
        (color, depth_stencil)
    }

    // the canvas changed size: a new drawing buffer, and a viewport that follows it
    pub(crate) fn resize(&mut self, width: u32, height: u32) {
        let depth = self
            .depth_stencil
            .as_ref()
            .is_some_and(|level| level.format.has_depth());
        let stencil = self
            .depth_stencil
            .as_ref()
            .is_some_and(|level| level.format.has_stencil());
        (self.color, self.depth_stencil) =
            Self::drawing_buffer(width, height, self.alpha, depth, stencil);
        self.width = width;
        self.height = height;
        self.dirty = true;
        self.clear_pending = false;
    }

    pub(crate) fn error(&mut self, code: u32) {
        if !self.errors.contains(&code) {
            self.errors.push(code);
        }
    }

    pub(crate) fn take_error(&mut self) -> u32 {
        if self.errors.is_empty() {
            NO_ERROR
        } else {
            self.errors.remove(0)
        }
    }

    pub(crate) fn create(&mut self) -> u32 {
        let id = self.next;
        self.next += 1;
        id
    }

    pub(crate) fn enabled(&self, capability: Capability) -> bool {
        self.enabled.contains(&capability)
    }

    pub(crate) fn set_enabled(&mut self, capability: Capability, on: bool) {
        self.enabled.retain(|found| *found != capability);
        if on {
            self.enabled.push(capability);
        }
    }

    // the drawing buffer as paint shows it, after which it reads as cleared unless the
    // context preserves it
    pub(crate) fn present(&mut self) -> crate::image::Image {
        self.flush_clear();
        let image = self.image();
        self.dirty = false;
        if !self.preserve {
            self.clear_pending = true;
        }
        image
    }

    // the drawing buffer's pixels, rows top to bottom and colors unpremultiplied
    pub(crate) fn image(&self) -> crate::image::Image {
        let (width, height) = (self.width, self.height);
        let mut pixels = Vec::with_capacity(width as usize * height as usize);
        for y in (0..height).rev() {
            for x in 0..width {
                let [r, g, b, a] = self.color.texel(x, y, 0);
                let a = if self.alpha { a } else { 1.0 };
                let (r, g, b) = if self.premultiplied && a > 0.0 && self.alpha {
                    (r / a, g / a, b / a)
                } else if self.premultiplied && self.alpha {
                    (0.0, 0.0, 0.0)
                } else {
                    (r, g, b)
                };
                let byte = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
                pixels.push(crate::css::Color {
                    red: byte(r),
                    green: byte(g),
                    blue: byte(b),
                    alpha: byte(a),
                });
            }
        }
        crate::image::Image {
            levels: Default::default(),
            width,
            height,
            pixels,
        }
    }

    // the drawing buffer after compositing reads as cleared to zero
    pub(crate) fn flush_clear(&mut self) {
        if !self.clear_pending {
            return;
        }
        self.clear_pending = false;
        let blank = [0.0, 0.0, 0.0, if self.alpha { 0.0 } else { 1.0 }];
        self.color.fill(blank);
        if let Some(level) = &mut self.depth_stencil {
            level.fill([1.0, 0.0, 0.0, 1.0]);
        }
    }

    pub(crate) fn texture_binding(&self, target: u32) -> Option<u32> {
        let index = target_index(target)?;
        Some(self.units[self.active_texture as usize][index])
    }

    // the texture a texImage call targets: its id and the cube face
    pub(crate) fn image_target(&mut self, target: u32, three: bool) -> Option<(u32, usize)> {
        let (binding, face) = match target {
            TEXTURE_2D if !three => (TEXTURE_2D, 0),
            TEXTURE_CUBE_MAP_POSITIVE_X..=TEXTURE_CUBE_MAP_NEGATIVE_Z if !three => (
                TEXTURE_CUBE_MAP,
                (target - TEXTURE_CUBE_MAP_POSITIVE_X) as usize,
            ),
            TEXTURE_3D | TEXTURE_2D_ARRAY if three && self.webgl2 => (target, 0),
            _ => {
                self.error(INVALID_ENUM);
                return None;
            }
        };
        let id = self.texture_binding(binding).unwrap_or(0);
        if id == 0 {
            self.error(INVALID_OPERATION);
            return None;
        }
        Some((id, face))
    }

    // checks the size of a level against its target's limits
    fn check_level_size(
        &mut self,
        target: u32,
        level: i32,
        width: i32,
        height: i32,
        depth: i32,
    ) -> bool {
        let max = match target {
            TEXTURE_3D => MAX_3D_SIZE,
            TEXTURE_2D_ARRAY | TEXTURE_2D => MAX_TEXTURE_SIZE_VALUE,
            _ => MAX_CUBE_SIZE,
        };
        let levels = 32 - max.leading_zeros() as i32;
        if level < 0 || level >= levels || width < 0 || height < 0 || depth < 0 {
            self.error(INVALID_VALUE);
            return false;
        }
        let limit = (max >> level).max(1) as i32;
        let depth_limit = if target == TEXTURE_2D_ARRAY {
            MAX_LAYERS as i32
        } else {
            limit
        };
        if width > limit
            || height > limit
            || (target == TEXTURE_3D || target == TEXTURE_2D_ARRAY) && depth > depth_limit
        {
            self.error(INVALID_VALUE);
            return false;
        }
        if (TEXTURE_CUBE_MAP_POSITIVE_X..=TEXTURE_CUBE_MAP_NEGATIVE_Z).contains(&target)
            && width != height
        {
            self.error(INVALID_VALUE);
            return false;
        }
        if !self.webgl2
            && level > 0
            && (!(width as u32).is_power_of_two() || !(height as u32).is_power_of_two())
        {
            self.error(INVALID_VALUE);
            return false;
        }
        true
    }

    // texImage2D and texImage3D with the texels already decoded
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn define_level(
        &mut self,
        target: u32,
        level: i32,
        internal: u32,
        width: i32,
        height: i32,
        depth: i32,
        border: i32,
        format: u32,
        kind: u32,
        three: bool,
    ) -> Option<(u32, usize, Format)> {
        let (id, face) = self.image_target(target, three)?;
        if border != 0 {
            self.error(INVALID_VALUE);
            return None;
        }
        if !self.check_level_size(target, level, width, height, depth) {
            return None;
        }
        let format = match Format::upload(internal, format, kind, self.webgl2) {
            Ok(format) => format,
            Err(code) => {
                self.error(code);
                return None;
            }
        };
        if !self.type_enabled(kind, format) {
            self.error(INVALID_ENUM);
            return None;
        }
        if format.has_depth()
            && (target != TEXTURE_2D && !(self.webgl2 && target == TEXTURE_2D_ARRAY)
                || (!self.webgl2 && level != 0))
        {
            self.error(INVALID_OPERATION);
            return None;
        }
        let texture = self.textures.get(&id).expect("a bound texture exists");
        if texture.immutable_levels.is_some() {
            self.error(INVALID_OPERATION);
            return None;
        }
        Some((id, face, format))
    }

    fn type_enabled(&self, kind: u32, format: Format) -> bool {
        let has = |name: &str| self.extensions.contains(&name);
        match kind {
            FLOAT if !self.webgl2 => has("OES_texture_float") || format.has_depth(),
            HALF_FLOAT_OES if !self.webgl2 => has("OES_texture_half_float"),
            HALF_FLOAT if !self.webgl2 => false,
            UNSIGNED_SHORT | UNSIGNED_INT | UNSIGNED_INT_24_8 if !self.webgl2 => {
                format.has_depth() && has("WEBGL_depth_texture")
            }
            HALF_FLOAT_OES => false,
            _ => true,
        }
    }

    pub(crate) fn store_level(&mut self, id: u32, face: usize, level: i32, image: Level) {
        let texture = self.textures.get_mut(&id).expect("a bound texture exists");
        texture.set_level(face, level as usize, image);
    }

    // a sub-rectangle of an existing level gets new texels
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn sub_level_target(
        &mut self,
        target: u32,
        level: i32,
        x: i32,
        y: i32,
        z: i32,
        width: i32,
        height: i32,
        depth: i32,
        format: u32,
        kind: u32,
        three: bool,
    ) -> Option<(u32, usize, Format)> {
        let (id, face) = self.image_target(target, three)?;
        if level < 0 || x < 0 || y < 0 || z < 0 || width < 0 || height < 0 || depth < 0 {
            self.error(INVALID_VALUE);
            return None;
        }
        let Some(existing) = self
            .textures
            .get(&id)
            .and_then(|texture| texture.level(face, level as usize))
        else {
            self.error(INVALID_OPERATION);
            return None;
        };
        let existing_format = existing.format;
        if (x + width) as u32 > existing.width
            || (y + height) as u32 > existing.height
            || (z + depth) as u32 > existing.depth
        {
            self.error(INVALID_VALUE);
            return None;
        }
        // the format and type must be ones an upload of this level's format could take
        let sized = existing_format.internal;
        let valid = Format::upload(sized, format, kind, self.webgl2).is_ok()
            || Format::upload(existing_format.base, format, kind, self.webgl2)
                .is_ok_and(|found| found.kind == existing_format.kind || !self.webgl2)
            || (existing_format.kind == Kind::Srgb && format == existing_format.base);
        if !valid {
            self.error(
                if is_known_format(format) && super::texture::is_type(kind) {
                    INVALID_OPERATION
                } else {
                    INVALID_ENUM
                },
            );
            return None;
        }
        Some((id, face, existing_format))
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn write_sub_level(
        &mut self,
        id: u32,
        face: usize,
        level: i32,
        x: i32,
        y: i32,
        z: i32,
        width: i32,
        height: i32,
        depth: i32,
        texels: &[[f32; 4]],
    ) {
        let texture = self.textures.get_mut(&id).expect("a bound texture exists");
        let Some(Some(image)) = texture.faces[face].get_mut(level as usize) else {
            return;
        };
        let mut source = texels.iter();
        for dz in 0..depth as u32 {
            for dy in 0..height as u32 {
                for dx in 0..width as u32 {
                    let index = image.index(x as u32 + dx, y as u32 + dy, z as u32 + dz);
                    if let Some(texel) = source.next() {
                        image.set(index, *texel);
                    }
                }
            }
        }
    }

    // decodes client bytes into texels of a format
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn decode(
        &self,
        bytes: &[u8],
        width: u32,
        height: u32,
        depth: u32,
        format: u32,
        kind: u32,
        target: Format,
    ) -> Vec<[f32; 4]> {
        unpack(
            bytes,
            &self.unpack,
            width,
            height,
            depth,
            format,
            kind,
            target,
            self.unpack_flip,
            self.unpack_premultiply,
        )
    }

    pub(crate) fn texture_params_target(&mut self, target: u32) -> Option<u32> {
        let allowed = matches!(target, TEXTURE_2D | TEXTURE_CUBE_MAP)
            || self.webgl2 && matches!(target, TEXTURE_3D | TEXTURE_2D_ARRAY);
        if !allowed {
            self.error(INVALID_ENUM);
            return None;
        }
        let id = self.texture_binding(target).unwrap_or(0);
        if id == 0 {
            self.error(INVALID_OPERATION);
            return None;
        }
        Some(id)
    }

    // texParameter and samplerParameter share their checks; the value comes as a float so
    // both the integer and float forms fit
    pub(crate) fn set_param(
        &mut self,
        params: &mut Params,
        name: u32,
        value: f32,
        sampler: bool,
    ) -> bool {
        let int = value as i64 as u32;
        let ok = match name {
            TEXTURE_MIN_FILTER => {
                let valid = matches!(
                    int,
                    NEAREST
                        | LINEAR
                        | NEAREST_MIPMAP_NEAREST
                        | LINEAR_MIPMAP_NEAREST
                        | NEAREST_MIPMAP_LINEAR
                        | LINEAR_MIPMAP_LINEAR
                );
                if valid {
                    params.min_filter = int;
                }
                valid
            }
            TEXTURE_MAG_FILTER => {
                let valid = matches!(int, NEAREST | LINEAR);
                if valid {
                    params.mag_filter = int;
                }
                valid
            }
            TEXTURE_WRAP_S | TEXTURE_WRAP_T | TEXTURE_WRAP_R => {
                if name == TEXTURE_WRAP_R && !self.webgl2 {
                    self.error(INVALID_ENUM);
                    return false;
                }
                let valid = matches!(int, REPEAT | CLAMP_TO_EDGE | MIRRORED_REPEAT);
                if valid {
                    params.wrap[(name - TEXTURE_WRAP_S) as usize % 3] = int;
                    if name == TEXTURE_WRAP_R {
                        params.wrap[2] = int;
                    }
                }
                valid
            }
            TEXTURE_MIN_LOD if self.webgl2 => {
                params.min_lod = value;
                true
            }
            TEXTURE_MAX_LOD if self.webgl2 => {
                params.max_lod = value;
                true
            }
            TEXTURE_COMPARE_MODE if self.webgl2 => {
                let valid = matches!(int, NONE | COMPARE_REF_TO_TEXTURE);
                if valid {
                    params.compare_mode = int;
                }
                valid
            }
            TEXTURE_COMPARE_FUNC if self.webgl2 => {
                let valid = (NEVER..=ALWAYS).contains(&int);
                if valid {
                    params.compare_func = int;
                }
                valid
            }
            _ => {
                let _ = sampler;
                self.error(INVALID_ENUM);
                return false;
            }
        };
        if !ok {
            self.error(INVALID_ENUM);
        }
        ok
    }

    pub(crate) fn current_vertex_array(&self) -> &VertexArray {
        &self.vertex_arrays[&self.vertex_array]
    }

    pub(crate) fn current_vertex_array_mut(&mut self) -> &mut VertexArray {
        self.vertex_arrays
            .get_mut(&self.vertex_array)
            .expect("the bound vertex array exists")
    }

    // the buffer bound to a target, or an INVALID_ENUM for a target this version lacks
    pub(crate) fn buffer_binding(&mut self, target: u32) -> Option<u32> {
        Some(match target {
            ARRAY_BUFFER => self.array_buffer,
            ELEMENT_ARRAY_BUFFER => self.current_vertex_array().element,
            COPY_READ_BUFFER if self.webgl2 => self.copy_read,
            COPY_WRITE_BUFFER if self.webgl2 => self.copy_write,
            PIXEL_PACK_BUFFER if self.webgl2 => self.pixel_pack,
            PIXEL_UNPACK_BUFFER if self.webgl2 => self.pixel_unpack,
            UNIFORM_BUFFER if self.webgl2 => self.uniform_buffer,
            TRANSFORM_FEEDBACK_BUFFER if self.webgl2 => self.feedback_buffer,
            _ => {
                self.error(INVALID_ENUM);
                return None;
            }
        })
    }

    pub(crate) fn bind_buffer(&mut self, target: u32, id: u32) {
        if self.buffer_binding(target).is_none() {
            return;
        }
        if id != 0 {
            let Some(buffer) = self.buffers.get_mut(&id) else {
                self.error(INVALID_OPERATION);
                return;
            };
            let element = target == ELEMENT_ARRAY_BUFFER;
            match buffer.element {
                Some(was) if was != element => {
                    self.error(INVALID_OPERATION);
                    return;
                }
                _ => buffer.element = Some(element),
            }
        }
        match target {
            ARRAY_BUFFER => self.array_buffer = id,
            ELEMENT_ARRAY_BUFFER => self.current_vertex_array_mut().element = id,
            COPY_READ_BUFFER => self.copy_read = id,
            COPY_WRITE_BUFFER => self.copy_write = id,
            PIXEL_PACK_BUFFER => self.pixel_pack = id,
            PIXEL_UNPACK_BUFFER => self.pixel_unpack = id,
            UNIFORM_BUFFER => self.uniform_buffer = id,
            _ => self.feedback_buffer = id,
        }
    }

    // the buffer a data call writes, checked to exist
    pub(crate) fn data_target(&mut self, target: u32) -> Option<u32> {
        let id = self.buffer_binding(target)?;
        if id == 0 {
            self.error(INVALID_OPERATION);
            return None;
        }
        Some(id)
    }

    pub(crate) fn buffer_data(&mut self, target: u32, data: Vec<u8>, usage: u32) {
        let valid_usage = matches!(usage, STREAM_DRAW | STATIC_DRAW | DYNAMIC_DRAW)
            || self.webgl2
                && matches!(
                    usage,
                    STREAM_READ
                        | STREAM_COPY
                        | STATIC_READ
                        | STATIC_COPY
                        | DYNAMIC_READ
                        | DYNAMIC_COPY
                );
        let Some(id) = self.data_target(target) else {
            return;
        };
        if !valid_usage {
            self.error(INVALID_ENUM);
            return;
        }
        let buffer = self.buffers.get_mut(&id).expect("bound buffers exist");
        buffer.data = data;
        buffer.usage = usage;
    }

    pub(crate) fn buffer_sub_data(&mut self, target: u32, offset: i64, data: &[u8]) {
        let Some(id) = self.data_target(target) else {
            return;
        };
        if offset < 0 {
            self.error(INVALID_VALUE);
            return;
        }
        let buffer = self.buffers.get_mut(&id).expect("bound buffers exist");
        let offset = offset as usize;
        if offset + data.len() > buffer.data.len() {
            self.error(INVALID_VALUE);
            return;
        }
        buffer.data[offset..offset + data.len()].copy_from_slice(data);
    }

    pub(crate) fn delete_buffer(&mut self, id: u32) {
        if self.buffers.remove(&id).is_none() {
            return;
        }
        let clear = |binding: &mut u32| {
            if *binding == id {
                *binding = 0;
            }
        };
        clear(&mut self.array_buffer);
        clear(&mut self.copy_read);
        clear(&mut self.copy_write);
        clear(&mut self.pixel_pack);
        clear(&mut self.pixel_unpack);
        clear(&mut self.uniform_buffer);
        clear(&mut self.feedback_buffer);
        for range in &mut self.uniform_ranges {
            if range.buffer == id {
                *range = Range::default();
            }
        }
        let current = self.vertex_array;
        if let Some(array) = self.vertex_arrays.get_mut(&current) {
            if array.element == id {
                array.element = 0;
            }
            for attrib in &mut array.attribs {
                if attrib.buffer == id {
                    attrib.buffer = 0;
                }
            }
        }
    }

    pub(crate) fn delete_texture(&mut self, id: u32) {
        if self.textures.remove(&id).is_none() {
            return;
        }
        for unit in &mut self.units {
            for binding in unit.iter_mut() {
                if *binding == id {
                    *binding = 0;
                }
            }
        }
        self.detach_everywhere(|attachment| matches!(attachment, Attachment::Texture { id: found, .. } if *found == id));
    }

    pub(crate) fn delete_renderbuffer(&mut self, id: u32) {
        if self.renderbuffers.remove(&id).is_none() {
            return;
        }
        if self.renderbuffer == id {
            self.renderbuffer = 0;
        }
        self.detach_everywhere(|attachment| *attachment == Attachment::Renderbuffer(id));
    }

    // a deleted image leaves the bound framebuffers it was attached to
    fn detach_everywhere(&mut self, matches: impl Fn(&Attachment) -> bool) {
        for id in [self.draw_framebuffer, self.read_framebuffer] {
            if let Some(framebuffer) = self.framebuffers.get_mut(&id) {
                framebuffer
                    .attachments
                    .retain(|_, attachment| !matches(attachment));
            }
        }
    }

    pub(crate) fn delete_framebuffer(&mut self, id: u32) {
        if self.framebuffers.remove(&id).is_none() {
            return;
        }
        if self.draw_framebuffer == id {
            self.draw_framebuffer = 0;
        }
        if self.read_framebuffer == id {
            self.read_framebuffer = 0;
        }
    }

    pub(crate) fn bind_texture(&mut self, target: u32, id: u32) {
        let Some(index) = target_index(target).filter(|&index| index < 2 || self.webgl2) else {
            self.error(INVALID_ENUM);
            return;
        };
        if id != 0 {
            let Some(texture) = self.textures.get_mut(&id) else {
                self.error(INVALID_OPERATION);
                return;
            };
            if texture.target != 0 && texture.target != target {
                self.error(INVALID_OPERATION);
                return;
            }
            texture.target = target;
        }
        self.units[self.active_texture as usize][index] = id;
    }

    // the framebuffer bound for drawing or reading, by target
    pub(crate) fn framebuffer_binding(&mut self, target: u32) -> Option<u32> {
        match target {
            FRAMEBUFFER | DRAW_FRAMEBUFFER if target == FRAMEBUFFER || self.webgl2 => {
                Some(self.draw_framebuffer)
            }
            READ_FRAMEBUFFER if self.webgl2 => Some(self.read_framebuffer),
            _ => {
                self.error(INVALID_ENUM);
                None
            }
        }
    }

    pub(crate) fn bind_framebuffer(&mut self, target: u32, id: u32) {
        if self.framebuffer_binding(target).is_none() {
            return;
        }
        if id != 0 {
            let Some(framebuffer) = self.framebuffers.get_mut(&id) else {
                self.error(INVALID_OPERATION);
                return;
            };
            if !framebuffer.bound {
                framebuffer.bound = true;
                framebuffer.draw_buffers = vec![COLOR_ATTACHMENT0];
                framebuffer.read_buffer = COLOR_ATTACHMENT0;
            }
        }
        match target {
            FRAMEBUFFER => {
                self.draw_framebuffer = id;
                self.read_framebuffer = id;
            }
            DRAW_FRAMEBUFFER => self.draw_framebuffer = id,
            _ => self.read_framebuffer = id,
        }
    }

    pub(crate) fn valid_attachment(&self, attachment: u32) -> bool {
        let colors = if self.webgl2 || self.extensions.contains(&"WEBGL_draw_buffers") {
            COLOR_ATTACHMENTS as u32
        } else {
            1
        };
        (COLOR_ATTACHMENT0..COLOR_ATTACHMENT0 + colors).contains(&attachment)
            || matches!(
                attachment,
                DEPTH_ATTACHMENT | STENCIL_ATTACHMENT | DEPTH_STENCIL_ATTACHMENT
            )
    }

    // attaches an image, or detaches with None; DEPTH_STENCIL sets both points
    pub(crate) fn attach(&mut self, target: u32, point: u32, attachment: Option<Attachment>) {
        let Some(id) = self.framebuffer_binding(target) else {
            return;
        };
        if !self.valid_attachment(point) {
            self.error(INVALID_ENUM);
            return;
        }
        if id == 0 {
            self.error(INVALID_OPERATION);
            return;
        }
        let webgl2 = self.webgl2;
        let framebuffer = self
            .framebuffers
            .get_mut(&id)
            .expect("bound framebuffers exist");
        let points: &[u32] = if point == DEPTH_STENCIL_ATTACHMENT && webgl2 {
            &[DEPTH_ATTACHMENT, STENCIL_ATTACHMENT]
        } else {
            std::slice::from_ref(&point)
        };
        for &point in points {
            match attachment {
                Some(attachment) => framebuffer.attachments.insert(point, attachment),
                None => framebuffer.attachments.remove(&point),
            };
        }
        if point == DEPTH_STENCIL_ATTACHMENT && !webgl2 {
            // WebGL 1 keeps DEPTH_STENCIL as its own point that excludes the other two
            framebuffer.attachments.remove(&DEPTH_ATTACHMENT);
            framebuffer.attachments.remove(&STENCIL_ATTACHMENT);
        } else if matches!(point, DEPTH_ATTACHMENT | STENCIL_ATTACHMENT) && !webgl2 {
            framebuffer.attachments.remove(&DEPTH_STENCIL_ATTACHMENT);
        }
    }

    pub(crate) fn attachment_format(
        &self,
        attachment: &Attachment,
    ) -> Option<(Format, u32, u32, u32)> {
        match *attachment {
            Attachment::Texture {
                id,
                face,
                level,
                layer,
            } => {
                let found = self.textures.get(&id)?.level(face, level as usize)?;
                (layer < found.depth).then_some((
                    found.format,
                    found.width,
                    found.height,
                    found.depth,
                ))
            }
            Attachment::Renderbuffer(id) => {
                let level = self.renderbuffers.get(&id)?.level.as_ref()?;
                Some((level.format, level.width, level.height, 1))
            }
        }
    }

    // FRAMEBUFFER_COMPLETE or the reason the framebuffer is not
    pub(crate) fn status(&self, id: u32) -> u32 {
        if id == 0 {
            return FRAMEBUFFER_COMPLETE;
        }
        let Some(framebuffer) = self.framebuffers.get(&id) else {
            return FRAMEBUFFER_UNSUPPORTED;
        };
        if framebuffer.attachments.is_empty() {
            return FRAMEBUFFER_INCOMPLETE_MISSING_ATTACHMENT;
        }
        let mut size = None;
        let mut depth_image = None;
        let mut stencil_image = None;
        for (&point, attachment) in &framebuffer.attachments {
            let Some((format, width, height, _)) = self.attachment_format(attachment) else {
                return FRAMEBUFFER_INCOMPLETE_ATTACHMENT;
            };
            if width == 0 || height == 0 {
                return FRAMEBUFFER_INCOMPLETE_ATTACHMENT;
            }
            let fits = match point {
                DEPTH_ATTACHMENT => {
                    format.has_depth() && (self.webgl2 || format.kind == Kind::Depth)
                }
                STENCIL_ATTACHMENT => {
                    format.has_stencil() && (self.webgl2 || format.kind == Kind::Stencil)
                }
                DEPTH_STENCIL_ATTACHMENT => format.kind == Kind::DepthStencil,
                _ => format.is_color() && self.color_renderable(format),
            };
            if !fits {
                return FRAMEBUFFER_INCOMPLETE_ATTACHMENT;
            }
            match size {
                None => size = Some((width, height)),
                Some(found) if found != (width, height) => {
                    return FRAMEBUFFER_INCOMPLETE_DIMENSIONS;
                }
                _ => {}
            }
            if point == DEPTH_ATTACHMENT {
                depth_image = Some(*attachment);
            }
            if point == STENCIL_ATTACHMENT {
                stencil_image = Some(*attachment);
            }
        }
        if let (Some(depth), Some(stencil)) = (depth_image, stencil_image)
            && depth != stencil
        {
            return FRAMEBUFFER_UNSUPPORTED;
        }
        let mut samples =
            framebuffer
                .attachments
                .values()
                .filter_map(|attachment| match attachment {
                    Attachment::Renderbuffer(id) => {
                        self.renderbuffers.get(id).map(|found| found.samples)
                    }
                    _ => Some(0),
                });
        if let Some(first) = samples.next()
            && samples.any(|found| found != first)
        {
            return FRAMEBUFFER_INCOMPLETE_MULTISAMPLE;
        }
        FRAMEBUFFER_COMPLETE
    }

    pub(crate) fn color_renderable(&self, format: Format) -> bool {
        let has = |name: &str| self.extensions.contains(&name);
        match format.kind {
            Kind::Float => match format.float_bits() {
                32 => {
                    (self.webgl2 && has("EXT_color_buffer_float"))
                        || (!self.webgl2
                            && (has("WEBGL_color_buffer_float") || has("OES_texture_float")))
                            && format.base == RGBA
                        || format.base == RGB && !self.webgl2
                }
                16 => {
                    (self.webgl2 && has("EXT_color_buffer_float"))
                        || has("EXT_color_buffer_half_float")
                        || (!self.webgl2 && has("OES_texture_half_float"))
                }
                _ => {
                    self.webgl2
                        && has("EXT_color_buffer_float")
                        && format.internal == R11F_G11F_B10F
                }
            },
            _ => {
                format.renderable()
                    && (self.webgl2
                        || matches!(
                            format.internal,
                            RGBA | RGB | RGBA4 | RGB5_A1 | RGB565 | SRGB8_ALPHA8 | SRGB_ALPHA_EXT
                        ))
            }
        }
    }

    // takes the draw framebuffer's images out of their owners
    fn take_draw_target(&mut self) -> Taken {
        let id = self.draw_framebuffer;
        if id == 0 {
            let color = std::mem::replace(
                &mut self.color,
                Level::new(Format::sized(RGBA8).expect("rgba8"), 0, 0, 0),
            );
            let colors = if self.default_draw_buffer == BACK {
                vec![Some((color, 0, Attachment::Renderbuffer(0)))]
            } else {
                self.color = color;
                vec![None]
            };
            return Taken {
                colors,
                depth_stencil: self.depth_stencil.take().map(|level| (level, None)),
            };
        }
        let framebuffer = &self.framebuffers[&id];
        let draw_buffers = framebuffer.draw_buffers.clone();
        let attachments = framebuffer.attachments.clone();
        let mut colors = Vec::new();
        for buffer in draw_buffers {
            let attachment = attachments.get(&buffer).copied();
            colors.push(attachment.and_then(|attachment| {
                self.take_image(attachment)
                    .map(|(level, layer)| (level, layer, attachment))
            }));
        }
        let depth = attachments
            .get(&DEPTH_ATTACHMENT)
            .or_else(|| attachments.get(&DEPTH_STENCIL_ATTACHMENT))
            .or_else(|| attachments.get(&STENCIL_ATTACHMENT))
            .copied();
        let depth_stencil = depth.and_then(|attachment| {
            self.take_image(attachment)
                .map(|(level, _)| (level, Some(attachment)))
        });
        Taken {
            colors,
            depth_stencil,
        }
    }

    fn take_image(&mut self, attachment: Attachment) -> Option<(Level, u32)> {
        match attachment {
            Attachment::Texture {
                id,
                face,
                level,
                layer,
            } => {
                let slot = self.textures.get_mut(&id)?.faces[face].get_mut(level as usize)?;
                slot.take().map(|found| (found, layer))
            }
            Attachment::Renderbuffer(id) => self
                .renderbuffers
                .get_mut(&id)?
                .level
                .take()
                .map(|found| (found, 0)),
        }
    }

    fn return_image(&mut self, attachment: Attachment, image: Level) {
        match attachment {
            Attachment::Texture {
                id, face, level, ..
            } => {
                if let Some(texture) = self.textures.get_mut(&id) {
                    texture.set_level(face, level as usize, image);
                }
            }
            Attachment::Renderbuffer(id) => {
                if let Some(renderbuffer) = self.renderbuffers.get_mut(&id) {
                    renderbuffer.level = Some(image);
                }
            }
        }
    }

    fn put_back(&mut self, taken: Taken) {
        let default = self.draw_framebuffer == 0;
        for (level, _, attachment) in taken.colors.into_iter().flatten() {
            if default {
                self.color = level;
            } else {
                self.return_image(attachment, level);
            }
        }
        if let Some((level, attachment)) = taken.depth_stencil {
            match attachment {
                Some(attachment) => self.return_image(attachment, level),
                None => self.depth_stencil = Some(level),
            }
        }
        if default {
            self.dirty = true;
        }
    }

    // the size of what the draw framebuffer draws into
    pub(crate) fn draw_size(&self) -> (u32, u32) {
        let id = self.draw_framebuffer;
        if id == 0 {
            return (self.width, self.height);
        }
        self.framebuffers[&id]
            .attachments
            .values()
            .filter_map(|attachment| self.attachment_format(attachment))
            .map(|(_, width, height, _)| (width, height))
            .next()
            .unwrap_or((0, 0))
    }

    // the draw framebuffer must be complete for clears and draws
    fn draw_ready(&mut self) -> bool {
        self.flush_clear();
        if self.status(self.draw_framebuffer) != FRAMEBUFFER_COMPLETE {
            self.error(INVALID_FRAMEBUFFER_OPERATION);
            return false;
        }
        true
    }

    fn scissor_box(&self) -> Option<[i32; 4]> {
        self.enabled(Capability::ScissorTest)
            .then_some(self.scissor)
    }

    pub(crate) fn clear(&mut self, mask: u32) {
        if mask & !(COLOR_BUFFER_BIT | DEPTH_BUFFER_BIT | STENCIL_BUFFER_BIT) != 0 {
            self.error(INVALID_VALUE);
            return;
        }
        if !self.draw_ready() {
            return;
        }
        if mask & COLOR_BUFFER_BIT != 0 && self.draw_framebuffer != 0 {
            let framebuffer = &self.framebuffers[&self.draw_framebuffer];
            let integer = framebuffer
                .draw_buffers
                .iter()
                .filter_map(|buffer| framebuffer.attachments.get(buffer))
                .any(|attachment| {
                    self.attachment_format(attachment)
                        .is_some_and(|(format, ..)| format.is_integer())
                });
            if integer {
                self.error(INVALID_OPERATION);
                return;
            }
        }
        let (color, depth, stencil) = (self.clear_color, self.clear_depth, self.clear_stencil);
        let mut taken = self.take_draw_target();
        let area = self.clear_area(&taken);
        if mask & COLOR_BUFFER_BIT != 0 {
            for (level, layer, _) in taken.colors.iter_mut().flatten() {
                fill_color(level, *layer, area, color, self.color_mask);
            }
        }
        if let Some((level, _)) = &mut taken.depth_stencil {
            let write_depth =
                mask & DEPTH_BUFFER_BIT != 0 && self.depth_mask && level.format.has_depth();
            let write_stencil = mask & STENCIL_BUFFER_BIT != 0 && level.format.has_stencil();
            let stored_depth = level.format.store([depth.clamp(0.0, 1.0), 0.0, 0.0, 0.0])[0];
            let write_mask = self.stencil[0].write_mask & 255;
            let stencil_value = (stencil as u32) & 255;
            // every channel the buffer has is written, so the rectangle takes one value
            let whole = (write_depth || !level.format.has_depth())
                && (write_stencil && write_mask == 255 || !level.format.has_stencil());
            let clipped = [
                area[0].max(0) as u32,
                area[1].max(0) as u32,
                area[2].clamp(0, level.width as i32) as u32,
                area[3].clamp(0, level.height as i32) as u32,
            ];
            if whole && clipped[0] < clipped[2] && clipped[1] < clipped[3] {
                level.fill_rect(0, clipped, [stored_depth, stencil_value as f32, 0.0, 1.0]);
            } else if write_depth || write_stencil {
                for_area(level, area, |texel| {
                    if write_depth {
                        texel[0] = stored_depth;
                    }
                    if write_stencil {
                        let old = texel[1] as u32;
                        texel[1] = ((old & !write_mask) | (stencil_value & write_mask)) as f32;
                    }
                });
            }
        }
        self.put_back(taken);
    }

    fn clear_area(&self, taken: &Taken) -> [i32; 4] {
        let (width, height) = taken
            .colors
            .iter()
            .flatten()
            .map(|(level, ..)| (level.width, level.height))
            .next()
            .or_else(|| {
                taken
                    .depth_stencil
                    .as_ref()
                    .map(|(level, _)| (level.width, level.height))
            })
            .unwrap_or((0, 0));
        let full = [0, 0, width as i32, height as i32];
        match self.scissor_box() {
            Some([x, y, w, h]) => [
                x.max(0),
                y.max(0),
                x.saturating_add(w).min(full[2]),
                y.saturating_add(h).min(full[3]),
            ],
            None => full,
        }
    }

    // clearBuffer{fv,iv,uiv,fi}: one buffer of the draw framebuffer
    pub(crate) fn clear_buffer(
        &mut self,
        buffer: u32,
        draw_buffer: i32,
        values: [f32; 4],
        kind: Base,
        depth_stencil: Option<(f32, i32)>,
    ) {
        if !self.draw_ready() {
            return;
        }
        let mut taken = self.take_draw_target();
        let area = self.clear_area(&taken);
        match buffer {
            COLOR => {
                if draw_buffer < 0 || draw_buffer as usize >= COLOR_ATTACHMENTS {
                    self.error(INVALID_VALUE);
                } else if let Some(Some((level, layer, _))) =
                    taken.colors.get_mut(draw_buffer as usize)
                {
                    let matching = match level.format.kind {
                        Kind::Int => kind == Base::Int,
                        Kind::Uint => kind == Base::Uint,
                        _ => kind == Base::Float,
                    };
                    if matching {
                        fill_color(level, *layer, area, values, self.color_mask);
                    } else {
                        self.error(INVALID_OPERATION);
                    }
                }
            }
            DEPTH | STENCIL | DEPTH_STENCIL => {
                if draw_buffer != 0 {
                    self.error(INVALID_VALUE);
                } else if let Some((level, _)) = &mut taken.depth_stencil {
                    let (depth, stencil) = depth_stencil.unwrap_or((values[0], values[0] as i32));
                    let write_depth =
                        buffer != STENCIL && self.depth_mask && level.format.has_depth();
                    let write_stencil = buffer != DEPTH && level.format.has_stencil();
                    let stored = level.format.store([depth.clamp(0.0, 1.0), 0.0, 0.0, 0.0])[0];
                    let write_mask = self.stencil[0].write_mask & 255;
                    for_area(level, area, |texel| {
                        if write_depth {
                            texel[0] = stored;
                        }
                        if write_stencil {
                            let old = texel[1] as u32;
                            texel[1] = ((old & !write_mask) | ((stencil as u32 & 255) & write_mask))
                                as f32;
                        }
                    });
                }
            }
            _ => self.error(INVALID_ENUM),
        }
        self.put_back(taken);
    }

    // the level and size readPixels and the copies read from
    pub(crate) fn read_source(&mut self) -> Option<(Level, Option<Attachment>)> {
        self.flush_clear();
        let id = self.read_framebuffer;
        if self.status(id) != FRAMEBUFFER_COMPLETE {
            self.error(INVALID_FRAMEBUFFER_OPERATION);
            return None;
        }
        if id == 0 {
            if self.default_read_buffer == NONE {
                self.error(INVALID_OPERATION);
                return None;
            }
            return Some((self.color.clone(), None));
        }
        let framebuffer = &self.framebuffers[&id];
        let Some(&attachment) = framebuffer
            .attachments
            .get(&framebuffer.read_buffer)
            .filter(|_| framebuffer.read_buffer != NONE)
        else {
            self.error(INVALID_OPERATION);
            return None;
        };
        let (level, layer) = match attachment {
            Attachment::Texture {
                id,
                face,
                level,
                layer,
            } => (
                self.textures[&id].level(face, level as usize)?.clone(),
                layer,
            ),
            Attachment::Renderbuffer(id) => (self.renderbuffers[&id].level.clone()?, 0),
        };
        if layer > 0 {
            return Some((level.layer(layer), Some(attachment)));
        }
        Some((level, Some(attachment)))
    }

    // the format and type readPixels accepts besides RGBA with UNSIGNED_BYTE
    pub(crate) fn implementation_read(&self, format: Format) -> (u32, u32) {
        match (format.kind, format.bits[0]) {
            (Kind::Int, 8) => (RGBA_INTEGER, BYTE),
            (Kind::Int, 16) => (RGBA_INTEGER, SHORT),
            (Kind::Int, _) => (RGBA_INTEGER, INT),
            (Kind::Uint, 8) => (RGBA_INTEGER, UNSIGNED_BYTE),
            (Kind::Uint, 16) => (RGBA_INTEGER, UNSIGNED_SHORT),
            (Kind::Uint, 10) => (RGBA_INTEGER, UNSIGNED_INT_2_10_10_10_REV),
            (Kind::Uint, _) => (RGBA_INTEGER, UNSIGNED_INT),
            (Kind::Float, 16) if self.webgl2 => (RGBA, HALF_FLOAT),
            (Kind::Float, _) => (RGBA, FLOAT),
            _ => (RGBA, UNSIGNED_BYTE),
        }
    }

    // readPixels into client bytes; the pixels outside the framebuffer are left alone
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn read_pixels(
        &mut self,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        format: u32,
        kind: u32,
        out: &mut [u8],
    ) {
        if width < 0 || height < 0 {
            self.error(INVALID_VALUE);
            return;
        }
        let Some((level, _)) = self.read_source() else {
            return;
        };
        let source = level.format;
        let standard = if source.is_integer() {
            (
                RGBA_INTEGER,
                if source.kind == Kind::Int {
                    INT
                } else {
                    UNSIGNED_INT
                },
            )
        } else if source.kind == Kind::Float && self.webgl2 {
            (RGBA, FLOAT)
        } else {
            (RGBA, UNSIGNED_BYTE)
        };
        if (format, kind) != standard && (format, kind) != self.implementation_read(source) {
            let known = matches!(
                format,
                RGBA | RGB
                    | ALPHA
                    | RED
                    | RG
                    | RGBA_INTEGER
                    | RGB_INTEGER
                    | RG_INTEGER
                    | RED_INTEGER
                    | LUMINANCE
                    | LUMINANCE_ALPHA
            ) && super::texture::is_type(kind);
            self.error(if known {
                INVALID_OPERATION
            } else {
                INVALID_ENUM
            });
            return;
        }
        let Some((pixel, _)) = pixel_size(format, kind) else {
            self.error(INVALID_ENUM);
            return;
        };
        let (row, _, start) = self.pack.strides(width as u32, height as u32, pixel);
        let needed = self.pack.needed(width as u32, height as u32, 1, pixel);
        if needed > out.len() {
            self.error(INVALID_OPERATION);
            return;
        }
        for dy in 0..height {
            let sy = y + dy;
            if sy < 0 || sy >= level.height as i32 {
                continue;
            }
            for dx in 0..width {
                let sx = x + dx;
                if sx < 0 || sx >= level.width as i32 {
                    continue;
                }
                let at = start + dy as usize * row + dx as usize * pixel;
                pack_pixel(
                    level.texel(sx as u32, sy as u32, 0),
                    source,
                    format,
                    kind,
                    &mut out[at..at + pixel],
                );
            }
        }
    }

    // copyTexImage2D and copyTexSubImage: a rectangle of the read buffer, with zeros where it
    // reaches outside
    pub(crate) fn copy_region(
        &mut self,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        target: Format,
    ) -> Option<Vec<[f32; 4]>> {
        let (level, _) = self.read_source()?;
        if !level.format.is_color() || level.format.is_integer() != target.is_integer() {
            self.error(INVALID_OPERATION);
            return None;
        }
        let mut texels = Vec::with_capacity((width.max(0) * height.max(0)) as usize);
        for dy in 0..height {
            for dx in 0..width {
                let (sx, sy) = (x + dx, y + dy);
                let inside =
                    sx >= 0 && sy >= 0 && (sx as u32) < level.width && (sy as u32) < level.height;
                let texel = if inside {
                    level.texel(sx as u32, sy as u32, 0)
                } else {
                    [0.0; 4]
                };
                let texel = if level.format.kind == Kind::Srgb && target.kind != Kind::Srgb {
                    texel.map(|value| value.clamp(0.0, 1.0))
                } else {
                    texel
                };
                texels.push(target.store(channels(RGBA, texel)));
            }
        }
        Some(texels)
    }

    // blitFramebuffer: color, depth and stencil from the read framebuffer to the draw one
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn blit(&mut self, source: [i32; 4], destination: [i32; 4], mask: u32, filter: u32) {
        if mask & !(COLOR_BUFFER_BIT | DEPTH_BUFFER_BIT | STENCIL_BUFFER_BIT) != 0
            || !matches!(filter, NEAREST | LINEAR)
        {
            self.error(
                if mask & !(COLOR_BUFFER_BIT | DEPTH_BUFFER_BIT | STENCIL_BUFFER_BIT) != 0 {
                    INVALID_VALUE
                } else {
                    INVALID_ENUM
                },
            );
            return;
        }
        if filter == LINEAR && mask & (DEPTH_BUFFER_BIT | STENCIL_BUFFER_BIT) != 0 {
            self.error(INVALID_OPERATION);
            return;
        }
        if !self.draw_ready() {
            return;
        }
        if self.status(self.read_framebuffer) != FRAMEBUFFER_COMPLETE {
            self.error(INVALID_FRAMEBUFFER_OPERATION);
            return;
        }
        let read_color = if mask & COLOR_BUFFER_BIT != 0 {
            self.read_source().map(|(level, _)| level)
        } else {
            None
        };
        let read_depth = if mask & (DEPTH_BUFFER_BIT | STENCIL_BUFFER_BIT) != 0 {
            if self.read_framebuffer == 0 {
                self.depth_stencil.clone()
            } else {
                let framebuffer = &self.framebuffers[&self.read_framebuffer];
                let attachment = framebuffer
                    .attachments
                    .get(&DEPTH_ATTACHMENT)
                    .or_else(|| framebuffer.attachments.get(&STENCIL_ATTACHMENT))
                    .or_else(|| framebuffer.attachments.get(&DEPTH_STENCIL_ATTACHMENT))
                    .copied();
                attachment.and_then(|attachment| match attachment {
                    Attachment::Texture {
                        id, face, level, ..
                    } => self.textures[&id].level(face, level as usize).cloned(),
                    Attachment::Renderbuffer(id) => self.renderbuffers[&id].level.clone(),
                })
            }
        } else {
            None
        };
        let scissor = self.scissor_box();
        let mut taken = self.take_draw_target();
        let [sx0, sy0, sx1, sy1] = source.map(|value| value as f32);
        let [dx0, dy0, dx1, dy1] = destination;
        let (left, right) = (dx0.min(dx1), dx0.max(dx1));
        let (bottom, top) = (dy0.min(dy1), dy0.max(dy1));
        let map = |d: i32, d0: i32, d1: i32, s0: f32, s1: f32| -> f32 {
            let t = (d as f32 + 0.5 - d0 as f32) / (d1 - d0) as f32;
            s0 + t * (s1 - s0)
        };
        let copy = |target: &mut Level,
                    layer: u32,
                    from: &Level,
                    write: &dyn Fn(&mut [f32; 4], [f32; 4])| {
            for y in bottom.max(0)..top.min(target.height as i32) {
                for x in left.max(0)..right.min(target.width as i32) {
                    if let Some([cx, cy, cw, ch]) = scissor
                        && (x < cx || y < cy || x >= cx + cw || y >= cy + ch)
                    {
                        continue;
                    }
                    let (u, v) = (map(x, dx0, dx1, sx0, sx1), map(y, dy0, dy1, sy0, sy1));
                    let sample = if filter == LINEAR {
                        let (u, v) = (u - 0.5, v - 0.5);
                        let (x0, y0) = (u.floor(), v.floor());
                        let (fx, fy) = (u - x0, v - y0);
                        let read = |px: f32, py: f32| {
                            from.texel(
                                (px as i64).clamp(0, from.width as i64 - 1) as u32,
                                (py as i64).clamp(0, from.height as i64 - 1) as u32,
                                0,
                            )
                        };
                        let corners = [
                            (read(x0, y0), (1.0 - fx) * (1.0 - fy)),
                            (read(x0 + 1.0, y0), fx * (1.0 - fy)),
                            (read(x0, y0 + 1.0), (1.0 - fx) * fy),
                            (read(x0 + 1.0, y0 + 1.0), fx * fy),
                        ];
                        let mut sum = [0.0_f32; 4];
                        for (texel, weight) in corners {
                            for channel in 0..4 {
                                sum[channel] += texel[channel] * weight;
                            }
                        }
                        Some(sum)
                    } else {
                        let (px, py) = (u.floor(), v.floor());
                        (px >= 0.0
                            && py >= 0.0
                            && (px as u32) < from.width
                            && (py as u32) < from.height)
                            .then(|| from.texel(px as u32, py as u32, 0))
                    };
                    if let Some(sample) = sample {
                        let index = target.index(x as u32, y as u32, layer);
                        let mut texel = target.get(index);
                        write(&mut texel, sample);
                        target.set(index, texel);
                    }
                }
            }
        };
        if let Some(from) = &read_color {
            for (level, layer, _) in taken.colors.iter_mut().flatten() {
                let format = level.format;
                copy(level, *layer, from, &|texel, sample| {
                    *texel = format.store(channels(RGBA, sample))
                });
            }
        }
        if let (Some(from), Some((level, _))) = (&read_depth, &mut taken.depth_stencil) {
            let (depth, stencil) = (mask & DEPTH_BUFFER_BIT != 0, mask & STENCIL_BUFFER_BIT != 0);
            copy(level, 0, from, &|texel, sample| {
                if depth {
                    texel[0] = sample[0];
                }
                if stencil {
                    texel[1] = sample[1];
                }
            });
        }
        self.put_back(taken);
    }

    pub(crate) fn pipeline(&self) -> Pipeline {
        let has = |capability| self.enabled(capability);
        Pipeline {
            viewport: self.viewport,
            depth_range: self.depth_range,
            scissor: self.scissor_box(),
            cull: has(Capability::CullFace).then_some(self.cull_face),
            front_face: self.front_face,
            polygon_offset: has(Capability::PolygonOffsetFill).then_some(self.polygon_offset),
            depth_test: has(Capability::DepthTest).then_some(self.depth_func),
            depth_mask: self.depth_mask,
            stencil: has(Capability::StencilTest).then_some(self.stencil),
            blend: has(Capability::Blend).then_some(self.blend),
            color_mask: self.color_mask,
            rasterizer_discard: has(Capability::RasterizerDiscard),
        }
    }

    // reads the indices of a drawElements call, with restart markers in WebGL 2
    fn element_ids(&mut self, count: usize, kind: u32, offset: i64) -> Option<Vec<u32>> {
        let size = match kind {
            UNSIGNED_BYTE => 1,
            UNSIGNED_SHORT => 2,
            UNSIGNED_INT if self.webgl2 || self.extensions.contains(&"OES_element_index_uint") => 4,
            _ => {
                self.error(INVALID_ENUM);
                return None;
            }
        };
        if offset < 0 {
            self.error(INVALID_VALUE);
            return None;
        }
        if !(offset as usize).is_multiple_of(size) {
            self.error(INVALID_OPERATION);
            return None;
        }
        let element = self.current_vertex_array().element;
        let Some(buffer) = self.buffers.get(&element).filter(|_| element != 0) else {
            self.error(INVALID_OPERATION);
            return None;
        };
        let start = offset as usize;
        let end = start + count * size;
        if end > buffer.data.len() {
            self.error(INVALID_OPERATION);
            return None;
        }
        let bytes = &buffer.data[start..end];
        let restart = self.webgl2;
        Some(match size {
            1 => bytes
                .iter()
                .map(|&byte| {
                    if restart && byte == 0xff {
                        u32::MAX
                    } else {
                        u32::from(byte)
                    }
                })
                .collect(),
            2 => bytes
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| u16::from_le_bytes(*pair))
                .map(|value| {
                    if restart && value == 0xffff {
                        u32::MAX
                    } else {
                        u32::from(value)
                    }
                })
                .collect(),
            _ => bytes
                .as_chunks::<4>()
                .0
                .iter()
                .map(|four| u32::from_le_bytes(*four))
                .collect(),
        })
    }

    // drawArrays, drawElements and their instanced forms
    pub(crate) fn draw(
        &mut self,
        mode: u32,
        first: i64,
        count: i64,
        elements: Option<(u32, i64)>,
        instances: i64,
    ) {
        if mode > TRIANGLE_FAN {
            self.error(INVALID_ENUM);
            return;
        }
        if first < 0 || count < 0 || instances < 0 {
            self.error(INVALID_VALUE);
            return;
        }
        let Some(program) = self
            .programs
            .get(&self.program)
            .filter(|program| program.linked.is_some())
        else {
            self.error(INVALID_OPERATION);
            return;
        };
        let _ = program;
        if self.feedback_active_mismatch(mode) {
            self.error(INVALID_OPERATION);
            return;
        }
        let ids: Vec<u32> = match elements {
            Some((kind, offset)) => match self.element_ids(count as usize, kind, offset) {
                Some(ids) => ids,
                None => return,
            },
            None => {
                if first + count > u32::MAX as i64 {
                    self.error(INVALID_OPERATION);
                    return;
                }
                (first as u32..(first + count) as u32).collect()
            }
        };
        if !self.draw_ready() {
            return;
        }
        if count == 0 || instances == 0 {
            return;
        }
        if !self.check_attributes(&ids, instances as u32) {
            self.error(INVALID_OPERATION);
            return;
        }
        if !self.check_outputs() || self.feedback_loop() {
            self.error(INVALID_OPERATION);
            return;
        }
        self.load_blocks();
        let pipeline = self.pipeline();
        let mut taken = self.take_draw_target();
        let outcome = self.run_draw(mode, &ids, instances as u32, &pipeline, &mut taken);
        self.put_back(taken);
        if let Some(outcome) = outcome {
            self.record(outcome);
        }
    }

    fn feedback_active_mismatch(&self, mode: u32) -> bool {
        let feedback = &self.feedbacks[&self.feedback];
        if !feedback.active || feedback.paused {
            return false;
        }
        let base = match mode {
            POINTS => POINTS,
            LINES | LINE_LOOP | LINE_STRIP => LINES,
            _ => TRIANGLES,
        };
        base != feedback.mode || mode != base
    }

    // every enabled array the program reads must hold the vertices and instances drawn
    fn check_attributes(&self, ids: &[u32], instances: u32) -> bool {
        let linked = self.programs[&self.program]
            .linked
            .as_ref()
            .expect("a linked program");
        let array = self.current_vertex_array();
        let max_id = ids
            .iter()
            .copied()
            .filter(|&id| id != u32::MAX)
            .max()
            .unwrap_or(0) as u64;
        for attribute in &linked.attributes {
            let columns = match attribute.ty {
                Ty::Mat(columns, _) => u32::from(columns),
                _ => 1,
            };
            for column in 0..columns {
                let attrib = &array.attribs[(attribute.location + column) as usize];
                if !attrib.enabled {
                    continue;
                }
                let Some(buffer) = self
                    .buffers
                    .get(&attrib.buffer)
                    .filter(|_| attrib.buffer != 0)
                else {
                    return false;
                };
                let last = (instances - 1)
                    .checked_div(attrib.divisor)
                    .map_or(max_id, u64::from);
                let needed = u64::from(attrib.offset)
                    + last * attrib.effective_stride() as u64
                    + attrib.element_size() as u64;
                if needed > buffer.data.len() as u64 {
                    return false;
                }
            }
        }
        true
    }

    // each draw buffer's attachment must take the type its fragment output writes
    fn check_outputs(&self) -> bool {
        let linked = self.programs[&self.program]
            .linked
            .as_ref()
            .expect("a linked program");
        if self.draw_framebuffer == 0 || !self.webgl2 {
            return true;
        }
        let framebuffer = &self.framebuffers[&self.draw_framebuffer];
        for &(buffer, base) in &linked.output_bases {
            let Some(attachment) = framebuffer
                .draw_buffers
                .get(buffer)
                .and_then(|point| framebuffer.attachments.get(point))
            else {
                continue;
            };
            let Some((format, ..)) = self.attachment_format(attachment) else {
                continue;
            };
            let expected = match format.kind {
                Kind::Int => Base::Int,
                Kind::Uint => Base::Uint,
                _ => Base::Float,
            };
            if expected != base {
                return false;
            }
        }
        true
    }

    // a texture the program samples may not also be drawn into
    fn feedback_loop(&self) -> bool {
        if self.draw_framebuffer == 0 {
            return false;
        }
        let framebuffer = &self.framebuffers[&self.draw_framebuffer];
        let attached: Vec<(u32, u32)> = framebuffer
            .attachments
            .values()
            .filter_map(|attachment| match attachment {
                Attachment::Texture { id, level, .. } => Some((*id, *level)),
                _ => None,
            })
            .collect();
        if attached.is_empty() {
            return false;
        }
        let linked = self.programs[&self.program]
            .linked
            .as_ref()
            .expect("a linked program");
        linked
            .uniforms
            .iter()
            .filter(|uniform| matches!(uniform.ty, Ty::Sampler(..)))
            .any(|uniform| {
                let Ty::Sampler(kind, _) = uniform.ty else {
                    return false;
                };
                (0..uniform.size).any(|element| {
                    let unit = uniform
                        .slots
                        .iter()
                        .flatten()
                        .next()
                        .map(|&slot| {
                            linked.memory[usize::from(uniform.slots[0].is_none())]
                                [(slot + element) * LANES]
                        })
                        .unwrap_or(0);
                    let id = self
                        .units
                        .get(unit as usize)
                        .map_or(0, |unit| unit[kind_index(kind)]);
                    let base = self
                        .textures
                        .get(&id)
                        .map_or(0, |texture| texture.base_level);
                    attached
                        .iter()
                        .any(|&(found, level)| found == id && level >= base)
                })
            })
    }

    // copies the bound uniform buffers into the program's uniform slots
    fn load_blocks(&mut self) {
        let ranges = self.uniform_ranges.clone();
        let Some(linked) = self
            .programs
            .get_mut(&self.program)
            .and_then(|program| program.linked.as_mut())
        else {
            return;
        };
        for block in &linked.blocks {
            let range = ranges[block.binding as usize];
            let Some(buffer) = self.buffers.get(&range.buffer) else {
                continue;
            };
            let data = &buffer.data;
            for &member in &block.members {
                let uniform = &linked.uniforms[member];
                let (columns, rows) = match uniform.ty {
                    Ty::Mat(columns, rows) => (usize::from(columns), usize::from(rows)),
                    _ => (1, uniform.ty.slots()),
                };
                for element in 0..uniform.size {
                    for column in 0..columns {
                        for row in 0..rows {
                            let at = range.offset as usize
                                + uniform.offset
                                + element * uniform.array_stride
                                + column * uniform.matrix_stride
                                + row * 4;
                            let Some(bytes) = data.get(at..at + 4) else {
                                continue;
                            };
                            let mut word =
                                u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
                            if matches!(uniform.ty, Ty::Vec(Base::Bool, _)) {
                                word = u32::from(word != 0);
                            }
                            let offset = element * uniform.ty.slots() + column * rows + row;
                            for (stage, slot) in uniform.slots.iter().enumerate() {
                                if let Some(slot) = slot {
                                    let index = (slot + offset) * LANES;
                                    linked.memory[stage][index..index + LANES].fill(word);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    fn run_draw(
        &self,
        mode: u32,
        ids: &[u32],
        instances: u32,
        pipeline: &Pipeline,
        taken: &mut Taken,
    ) -> Option<Outcome> {
        let program = &self.programs[&self.program];
        let linked = program.linked.as_ref()?;
        let array = self.current_vertex_array();
        let mut fetches = Vec::new();
        for attribute in &linked.attributes {
            let (columns, words, base) = match attribute.ty {
                Ty::Mat(columns, rows) => (u32::from(columns), usize::from(rows), Base::Float),
                Ty::Vec(base, size) => (1, usize::from(size), base),
                _ => continue,
            };
            for column in 0..columns {
                let location = (attribute.location + column) as usize;
                let attrib = &array.attribs[location];
                let source = if attrib.enabled {
                    let data = &self.buffers.get(&attrib.buffer)?.data;
                    Source::Buffer {
                        data,
                        size: attrib.size as usize,
                        kind: attrib.kind,
                        normalized: attrib.normalized,
                        integer: attrib.integer,
                        stride: attrib.effective_stride(),
                        offset: attrib.offset as usize,
                        divisor: attrib.divisor,
                    }
                } else {
                    let (value, kind) = self.generic[location];
                    let value = match (kind, base) {
                        (Base::Float, Base::Int | Base::Uint) => {
                            value.map(|word| f32::from_bits(word) as i32 as u32)
                        }
                        (Base::Int | Base::Uint, Base::Float) => {
                            value.map(|word| (word as i32 as f32).to_bits())
                        }
                        _ => value,
                    };
                    Source::Constant(value)
                };
                fetches.push(Fetch {
                    slot: attribute.slot + column as usize * words,
                    words,
                    base,
                    source,
                });
            }
        }
        let units = self.units_for(linked);
        let feedback = &self.feedbacks[&self.feedback];
        let capture = if feedback.active && !feedback.paused {
            linked
                .feedback
                .iter()
                .map(|(_, _, slot, words)| (*slot, *words))
                .collect()
        } else {
            Vec::new()
        };
        let draw = Draw {
            mode,
            indices: ids,
            instances,
            vertex: DrawStage {
                module: &linked.vertex,
                memory: &linked.memory[0],
            },
            fragment: DrawStage {
                module: &linked.fragment,
                memory: &linked.memory[1],
            },
            fetches,
            links: linked.links.clone(),
            outputs: linked.outputs.clone(),
            frag_depth: linked.frag_depth,
            textures: &units,
            capture,
        };
        let (width, height) = taken
            .colors
            .iter()
            .flatten()
            .map(|(level, ..)| (level.width, level.height))
            .next()
            .or_else(|| {
                taken
                    .depth_stencil
                    .as_ref()
                    .map(|(level, _)| (level.width, level.height))
            })
            .unwrap_or(self.draw_size());
        let target = Target {
            width,
            height,
            colors: taken
                .colors
                .iter_mut()
                .map(|entry| {
                    entry
                        .as_mut()
                        .map(|(level, layer, _)| View::layer(level, *layer))
                })
                .collect(),
            depth_stencil: taken
                .depth_stencil
                .as_mut()
                .map(|(level, _)| View::layer(level, 0)),
        };
        Some(draw.run(pipeline, target))
    }

    fn units_for<'a>(&'a self, linked: &Linked) -> Units<'a> {
        let used: Vec<bool> = {
            let mut used = vec![false; TEXTURE_UNITS];
            for uniform in linked
                .uniforms
                .iter()
                .filter(|uniform| matches!(uniform.ty, Ty::Sampler(..)))
            {
                for (stage, slot) in uniform.slots.iter().enumerate() {
                    if let Some(slot) = slot {
                        for element in 0..uniform.size {
                            let unit = linked.memory[stage][(slot + element) * LANES] as usize;
                            if unit < TEXTURE_UNITS {
                                used[unit] = true;
                            }
                        }
                    }
                }
            }
            used
        };
        let float_linear = self.extensions.contains(&"OES_texture_float_linear");
        let half_linear = self.extensions.contains(&"OES_texture_half_float_linear");
        let units = self
            .units
            .iter()
            .enumerate()
            .map(|(unit, bindings)| {
                std::array::from_fn(|index| {
                    if !used[unit] || bindings[index] == 0 {
                        return None;
                    }
                    let texture = self.textures.get(&bindings[index])?;
                    let params = self
                        .samplers
                        .get(&self.unit_samplers[unit])
                        .map_or(texture.params, |sampler| sampler.params);
                    Some(Bound {
                        texture,
                        params,
                        complete: texture.complete(&params, self.webgl2, float_linear, half_linear),
                        webgl2: self.webgl2,
                    })
                })
            })
            .collect();
        Units { units }
    }

    // what a draw leaves for transform feedback and the active queries
    fn record(&mut self, outcome: Outcome) {
        for (&target, &id) in &self.active_queries {
            if let Some(query) = self.queries.get_mut(&id) {
                match target {
                    ANY_SAMPLES_PASSED | ANY_SAMPLES_PASSED_CONSERVATIVE => {
                        query.result |= u64::from(outcome.samples_passed > 0)
                    }
                    TRANSFORM_FEEDBACK_PRIMITIVES_WRITTEN => query.result += outcome.primitives,
                    _ => {}
                }
            }
        }
        if outcome.captured.is_empty() {
            return;
        }
        let Some(linked) = self
            .programs
            .get(&self.program)
            .and_then(|program| program.linked.as_ref())
        else {
            return;
        };
        let layout: Vec<usize> = linked
            .feedback
            .iter()
            .map(|(_, _, _, words)| *words)
            .collect();
        let separate = linked.feedback_mode == SEPARATE_ATTRIBS;
        let stride: usize = layout.iter().sum();
        let feedback = self
            .feedbacks
            .get_mut(&self.feedback)
            .expect("the bound transform feedback exists");
        for vertex in outcome.captured.chunks_exact(stride.max(1)) {
            let mut word = 0;
            for (index, &words) in layout.iter().enumerate() {
                let buffer_index = if separate { index } else { 0 };
                let range = feedback.buffers[buffer_index];
                let Some(buffer) = self.buffers.get_mut(&range.buffer) else {
                    continue;
                };
                let at = range.offset as usize + feedback.written[buffer_index] as usize;
                let limit = if range.size == 0 {
                    buffer.data.len()
                } else {
                    (range.offset + range.size) as usize
                }
                .min(buffer.data.len());
                if at + words * 4 <= limit {
                    for (offset, value) in vertex[word..word + words].iter().enumerate() {
                        buffer.data[at + offset * 4..at + offset * 4 + 4]
                            .copy_from_slice(&value.to_le_bytes());
                    }
                }
                feedback.written[buffer_index] += words as u64 * 4;
                word += words;
            }
        }
    }

    pub(crate) fn compile_shader(&mut self, id: u32) {
        let extensions: Vec<&str> = self
            .extensions
            .iter()
            .filter_map(|name| match *name {
                "OES_standard_derivatives" => Some("GL_OES_standard_derivatives"),
                "EXT_frag_depth" => Some("GL_EXT_frag_depth"),
                "EXT_shader_texture_lod" => Some("GL_EXT_shader_texture_lod"),
                "WEBGL_draw_buffers" => Some("GL_EXT_draw_buffers"),
                _ => None,
            })
            .collect();
        let webgl2 = self.webgl2;
        let Some(shader) = self.shaders.get_mut(&id) else {
            return;
        };
        let stage = if shader.kind == VERTEX_SHADER {
            Stage::Vertex
        } else {
            Stage::Fragment
        };
        let result = glsl::compile(&shader.source, stage, &extensions).and_then(|module| {
            if module.version >= 300 && !webgl2 {
                Err("ERROR: 0:1: '300' : version number not supported\n".to_string())
            } else {
                Ok(module)
            }
        });
        match result {
            Ok(module) => {
                shader.module = Some(Rc::new(module));
                shader.compiled = true;
                shader.log.clear();
            }
            Err(log) => {
                shader.module = None;
                shader.compiled = false;
                shader.log = log;
            }
        }
    }

    pub(crate) fn link_program(&mut self, id: u32) {
        let Some(program) = self.programs.get(&id) else {
            return;
        };
        let modules: Vec<(u32, Option<Rc<Module>>)> = program
            .shaders
            .iter()
            .filter_map(|shader| self.shaders.get(shader))
            .map(|shader| (shader.kind, shader.module.clone()))
            .collect();
        let find = |kind: u32| modules.iter().find(|(found, _)| *found == kind);
        let result = match (find(VERTEX_SHADER), find(FRAGMENT_SHADER)) {
            (Some((_, Some(vertex))), Some((_, Some(fragment)))) => {
                link(vertex.clone(), fragment.clone(), program, self.webgl2)
            }
            (Some(_), Some(_)) => Err("one of the attached shaders is not compiled".to_string()),
            _ => Err("a program needs a vertex and a fragment shader attached".to_string()),
        };
        let webgl2 = self.webgl2;
        let program = self.programs.get_mut(&id).expect("the program exists");
        program.generation += 1;
        match result {
            Ok(linked) => {
                program.linked = Some(linked);
                program.link_status = true;
                program.log.clear();
            }
            Err(log) => {
                program.link_status = false;
                program.log = log;
                // a failed link keeps the program in use running its last good link
                if !webgl2 || self.program != id {
                    program.linked = None;
                }
                if self.program != id {
                    program.linked = None;
                }
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Setter {
    Float,
    Int,
    Uint,
    Matrix(u8, u8),
}

impl Context {
    // the linked program a location belongs to, when it is the program in use and the
    // location is from its latest link
    fn location_target(
        &mut self,
        program: u32,
        generation: u32,
        index: usize,
    ) -> Option<(usize, usize)> {
        let current = self.program;
        let found = self
            .programs
            .get(&program)
            .filter(|found| found.generation == generation)
            .and_then(|found| found.linked.as_ref())
            .and_then(|linked| linked.locations.get(index).copied());
        if current != program || found.is_none() {
            self.error(INVALID_OPERATION);
            return None;
        }
        found
    }

    // the uniform* calls: values are words already in the form the setter takes them
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn set_uniform(
        &mut self,
        program: u32,
        generation: u32,
        index: usize,
        setter: Setter,
        components: usize,
        values: &[u32],
        transpose: bool,
    ) {
        let Some((uniform_index, element)) = self.location_target(program, generation, index)
        else {
            return;
        };
        let webgl2 = self.webgl2;
        let linked = self
            .programs
            .get_mut(&program)
            .and_then(|found| found.linked.as_mut())
            .expect("a linked program");
        let uniform = linked.uniforms[uniform_index].clone();
        let fits = match (setter, &uniform.ty) {
            (Setter::Float, Ty::Vec(Base::Float | Base::Bool, size)) => {
                usize::from(*size) == components
            }
            (Setter::Int, Ty::Vec(Base::Int | Base::Bool, size)) => {
                usize::from(*size) == components
            }
            (Setter::Int, Ty::Sampler(..)) => components == 1,
            (Setter::Uint, Ty::Vec(Base::Uint | Base::Bool, size)) => {
                usize::from(*size) == components
            }
            (Setter::Matrix(columns, rows), Ty::Mat(found_columns, found_rows)) => {
                columns == *found_columns && rows == *found_rows
            }
            _ => false,
        };
        if !fits {
            self.error(INVALID_OPERATION);
            return;
        }
        let size = match setter {
            Setter::Matrix(columns, rows) => usize::from(columns) * usize::from(rows),
            _ => components,
        };
        if values.is_empty() || !values.len().is_multiple_of(size) {
            self.error(INVALID_VALUE);
            return;
        }
        if transpose && !webgl2 {
            self.error(INVALID_VALUE);
            return;
        }
        let count = values.len() / size;
        if count > 1 && !uniform.array {
            self.error(INVALID_OPERATION);
            return;
        }
        if matches!(uniform.ty, Ty::Sampler(..))
            && values
                .iter()
                .any(|&unit| unit as i32 as u32 >= TEXTURE_UNITS as u32)
        {
            self.error(INVALID_VALUE);
            return;
        }
        let count = count.min(uniform.size - element);
        let boolean = matches!(uniform.ty, Ty::Vec(Base::Bool, _));
        for item in 0..count {
            for position in 0..size {
                let source = match (setter, transpose) {
                    (Setter::Matrix(columns, rows), true) => {
                        let (column, row) =
                            (position / usize::from(rows), position % usize::from(rows));
                        row * usize::from(columns) + column
                    }
                    _ => position,
                };
                let mut word = values[item * size + source];
                if boolean {
                    word = u32::from(if setter == Setter::Float {
                        f32::from_bits(word) != 0.0
                    } else {
                        word != 0
                    });
                }
                let offset = (element + item) * uniform.ty.slots() + position;
                for (stage, slot) in uniform.slots.iter().enumerate() {
                    if let Some(slot) = slot {
                        let at = (slot + offset) * LANES;
                        linked.memory[stage][at..at + LANES].fill(word);
                    }
                }
            }
        }
    }

    // the words of one uniform element, for getUniform
    pub(crate) fn uniform_words(
        &self,
        program: u32,
        generation: u32,
        index: usize,
    ) -> Option<(Ty, Vec<u32>)> {
        let linked = self
            .programs
            .get(&program)
            .filter(|found| found.generation == generation)?
            .linked
            .as_ref()?;
        let &(uniform_index, element) = linked.locations.get(index)?;
        let uniform = &linked.uniforms[uniform_index];
        let stage = usize::from(uniform.slots[0].is_none());
        let slot = uniform.slots[stage]?;
        let start = slot + element * uniform.ty.slots();
        Some((
            uniform.ty.clone(),
            (0..uniform.ty.slots())
                .map(|offset| linked.memory[stage][(start + offset) * LANES])
                .collect(),
        ))
    }
}

fn is_known_format(format: u32) -> bool {
    matches!(
        format,
        RGBA | RGB
            | ALPHA
            | LUMINANCE
            | LUMINANCE_ALPHA
            | RED
            | RG
            | RED_INTEGER
            | RG_INTEGER
            | RGB_INTEGER
            | RGBA_INTEGER
            | DEPTH_COMPONENT
            | DEPTH_STENCIL
            | SRGB
            | SRGB_ALPHA_EXT
    )
}

fn for_area(level: &mut Level, [x0, y0, x1, y1]: [i32; 4], mut apply: impl FnMut(&mut [f32; 4])) {
    for y in y0.max(0)..y1.min(level.height as i32) {
        for x in x0.max(0)..x1.min(level.width as i32) {
            let index = level.index(x as u32, y as u32, 0);
            let mut texel = level.get(index);
            apply(&mut texel);
            level.set(index, texel);
        }
    }
}

fn fill_color(
    level: &mut Level,
    layer: u32,
    [x0, y0, x1, y1]: [i32; 4],
    color: [f32; 4],
    mask: [bool; 4],
) {
    let format = level.format;
    let stored = format.store(channels(format.base, color));
    let area = [
        x0.max(0) as u32,
        y0.max(0) as u32,
        x1.clamp(0, level.width as i32) as u32,
        y1.clamp(0, level.height as i32) as u32,
    ];
    if area[0] >= area[2] || area[1] >= area[3] {
        return;
    }
    if mask == [true; 4] {
        level.fill_rect(layer, area, stored);
        return;
    }
    for y in area[1]..area[3] {
        for x in area[0]..area[2] {
            let index = level.index(x, y, layer);
            let old = level.get(index);
            level.set(
                index,
                std::array::from_fn(|channel| {
                    if mask[channel] {
                        stored[channel]
                    } else {
                        old[channel]
                    }
                }),
            );
        }
    }
}

// matches the interfaces of two compiled shaders: attribute locations, varyings, uniforms
// shared by both stages, uniform blocks laid out by std140 and the fragment outputs
fn link(
    vertex: Rc<Module>,
    fragment: Rc<Module>,
    program: &Program,
    webgl2: bool,
) -> Result<Linked, String> {
    if vertex.version != fragment.version {
        return Err("Versions of linked shaders have a difference.".into());
    }
    // attributes: bound locations first, then layout locations, then the lowest free ones
    let mut taken = [false; VERTEX_ATTRIBS];
    let mut attributes = Vec::new();
    let mut pending = Vec::new();
    for input in vertex.inputs.iter().filter(|input| input.used) {
        let columns = match input.ty {
            Ty::Mat(columns, _) => usize::from(columns),
            _ => 1,
        };
        let fixed = program
            .bindings
            .get(&input.name)
            .map(|&location| i64::from(location))
            .or(input.location);
        match fixed {
            Some(location) => {
                let location = location as usize;
                if location + columns > VERTEX_ATTRIBS {
                    return Err(format!(
                        "attribute '{}' does not fit at location {location}",
                        input.name
                    ));
                }
                for column in 0..columns {
                    if taken[location + column] && input.location.is_some() {
                        return Err(format!(
                            "attribute '{}' overlaps another at location {}",
                            input.name,
                            location + column
                        ));
                    }
                    taken[location + column] = true;
                }
                attributes.push(Attribute {
                    name: input.name.clone(),
                    ty: input.ty.clone(),
                    location: location as u32,
                    slot: input.slot,
                });
            }
            None => pending.push((input, columns)),
        }
    }
    for (input, columns) in pending {
        let Some(location) = (0..=VERTEX_ATTRIBS.saturating_sub(columns))
            .find(|&start| (start..start + columns).all(|location| !taken[location]))
        else {
            return Err("too many vertex attributes".into());
        };
        for column in 0..columns {
            taken[location + column] = true;
        }
        attributes.push(Attribute {
            name: input.name.clone(),
            ty: input.ty.clone(),
            location: location as u32,
            slot: input.slot,
        });
    }

    // varyings by name, with matching types
    let mut links = Vec::new();
    for input in &fragment.inputs {
        let Some(output) = vertex
            .outputs
            .iter()
            .find(|output| output.name == input.name)
        else {
            if input.used {
                return Err(format!(
                    "Varying '{}' is not declared in the vertex shader.",
                    input.name
                ));
            }
            continue;
        };
        if output.ty != input.ty {
            return Err(format!(
                "Types of varying '{}' differ between shaders.",
                input.name
            ));
        }
        let flat =
            input.flat || output.flat || matches!(input.ty.base(), Some(Base::Int | Base::Uint));
        links.push(Link {
            from: output.slot,
            to: input.slot,
            count: input.ty.slots(),
            flat,
        });
    }

    // uniforms: every leaf, shared between the stages by name
    let mut uniforms: Vec<Uniform> = Vec::new();
    let mut blocks: Vec<Block> = Vec::new();
    for (stage, module) in [&vertex, &fragment].into_iter().enumerate() {
        for interface in &module.uniforms {
            let mut leaves = Vec::new();
            let block_name = interface.block.as_deref();
            let (prefix, ty) = match (block_name, &interface.ty) {
                // an instance-named block: its members are named after the block
                (Some(block), Ty::Struct(_)) if interface.name != block => {
                    (block.to_string(), interface.ty.clone())
                }
                _ => (interface.name.clone(), interface.ty.clone()),
            };
            if block_name.is_some() && matches!(ty, Ty::Struct(_)) && prefix != interface.name {
                let Ty::Struct(def) = &ty else { unreachable!() };
                let mut at = interface.slot;
                for (field, field_ty) in &def.fields {
                    leaves.push((format!("{prefix}.{field}"), field_ty.clone(), at));
                    at += field_ty.slots();
                }
            } else {
                flatten(&prefix, &ty, interface.slot, &mut leaves);
            }
            let block_index =
                block_name.map(
                    |name| match blocks.iter().position(|block| block.name == name) {
                        Some(index) => {
                            blocks[index].stages[stage] = true;
                            index
                        }
                        None => {
                            blocks.push(Block {
                                name: name.to_string(),
                                binding: 0,
                                size: 0,
                                members: Vec::new(),
                                stages: [stage == 0, stage == 1],
                            });
                            blocks.len() - 1
                        }
                    },
                );
            for (name, ty, slot) in leaves {
                let (element, size, array) = match &ty {
                    Ty::Array(element, length) => ((**element).clone(), *length, true),
                    _ => (ty.clone(), 1, false),
                };
                match uniforms.iter_mut().find(|found| found.base_name == name) {
                    Some(found) => {
                        if found.ty != element || found.size != size {
                            return Err(format!(
                                "Types of uniform '{name}' differ between shaders."
                            ));
                        }
                        found.slots[stage] = Some(slot);
                    }
                    None => {
                        let mut slots = [None, None];
                        slots[stage] = Some(slot);
                        uniforms.push(Uniform {
                            name: if array {
                                format!("{name}[0]")
                            } else {
                                name.clone()
                            },
                            base_name: name,
                            ty: element,
                            size,
                            slots,
                            array,
                            block: block_index,
                            offset: 0,
                            array_stride: 0,
                            matrix_stride: 0,
                        });
                    }
                }
            }
        }
    }
    if !webgl2 && !blocks.is_empty() {
        return Err("uniform blocks need WebGL 2".into());
    }
    // std140 offsets for each block's members, in declaration order
    for (index, block) in blocks.iter_mut().enumerate() {
        let mut offset: usize = 0;
        for (member, uniform) in uniforms
            .iter_mut()
            .enumerate()
            .filter(|(_, uniform)| uniform.block == Some(index))
        {
            let whole = if uniform.array {
                Ty::Array(Box::new(uniform.ty.clone()), uniform.size)
            } else {
                uniform.ty.clone()
            };
            let (align, size) = std140(&whole);
            offset = offset.div_ceil(align) * align;
            uniform.offset = offset;
            uniform.array_stride = if uniform.array {
                size / uniform.size
            } else {
                0
            };
            uniform.matrix_stride = if matches!(uniform.ty, Ty::Mat(..)) {
                16
            } else {
                0
            };
            offset += size;
            block.members.push(member);
        }
        block.size = offset;
    }
    let mut locations = Vec::new();
    for (index, uniform) in uniforms.iter().enumerate() {
        if uniform.block.is_none() {
            for element in 0..uniform.size {
                locations.push((index, element));
            }
        }
    }

    // fragment outputs
    let mut outputs = Vec::new();
    let mut output_bases = Vec::new();
    let mut fragment_locations = HashMap::default();
    let mut frag_depth = None;
    if fragment.version >= 300 {
        let count = fragment.outputs.len();
        for output in &fragment.outputs {
            let location = match output.location {
                Some(location) => location as usize,
                None if count == 1 => 0,
                None => return Err("multiple fragment outputs need layout locations".into()),
            };
            let (element, length) = match &output.ty {
                Ty::Array(element, length) => ((**element).clone(), *length),
                _ => (output.ty.clone(), 1),
            };
            fragment_locations.insert(output.name.clone(), location as i32);
            for index in 0..length {
                if location + index >= COLOR_ATTACHMENTS {
                    return Err(format!(
                        "output '{}' is past the last draw buffer",
                        output.name
                    ));
                }
                if outputs
                    .iter()
                    .any(|&(_, buffer)| buffer == location + index)
                {
                    return Err(format!("output '{}' shares a location", output.name));
                }
                outputs.push((output.slot + index * element.slots(), location + index));
                output_bases.push((location + index, element.base().unwrap_or(Base::Float)));
            }
        }
        if fragment.used_builtins.contains(&"gl_FragDepth") {
            frag_depth = fragment.builtins.get("gl_FragDepth").copied();
        }
    } else {
        let draw_buffers = fragment
            .extensions
            .iter()
            .any(|name| name == "GL_EXT_draw_buffers");
        if fragment.used_builtins.contains(&"gl_FragData") {
            let slot = fragment.builtins["gl_FragData"];
            let count = if draw_buffers { COLOR_ATTACHMENTS } else { 1 };
            for buffer in 0..count {
                outputs.push((slot + buffer * 4, buffer));
            }
        } else {
            let slot = fragment.builtins["gl_FragColor"];
            let count = if draw_buffers { COLOR_ATTACHMENTS } else { 1 };
            for buffer in 0..count {
                outputs.push((slot, buffer));
            }
        }
        if fragment.used_builtins.contains(&"gl_FragDepthEXT") {
            frag_depth = fragment.builtins.get("gl_FragDepthEXT").copied();
        }
    }

    // transform feedback varyings
    let mut feedback = Vec::new();
    for name in &program.feedback_varyings {
        let found = match name.as_str() {
            "gl_Position" => Some((Ty::Vec(Base::Float, 4), vertex.builtins["gl_Position"])),
            "gl_PointSize" => Some((Ty::Vec(Base::Float, 1), vertex.builtins["gl_PointSize"])),
            _ => vertex
                .outputs
                .iter()
                .find(|output| &output.name == name)
                .map(|output| (output.ty.clone(), output.slot)),
        };
        let Some((ty, slot)) = found else {
            return Err(format!(
                "transform feedback varying '{name}' is not a vertex output"
            ));
        };
        let words = ty.slots();
        feedback.push((name.clone(), ty, slot, words));
    }
    if program.feedback_mode == SEPARATE_ATTRIBS && feedback.len() > FEEDBACK_BUFFERS {
        return Err("too many separate transform feedback varyings".into());
    }

    let memory = [vertex.initial.clone(), fragment.initial.clone()];
    Ok(Linked {
        vertex,
        fragment,
        memory,
        attributes,
        uniforms,
        locations,
        blocks,
        links,
        outputs,
        output_bases,
        frag_depth,
        fragment_locations,
        feedback,
        feedback_mode: program.feedback_mode,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_program_draws_into_the_drawing_buffer() {
        let mut gl = Context::new(false, 4, 4, true, true, false, true, false);
        let vertex = gl.create();
        gl.shaders.insert(
            vertex,
            Shader {
                kind: VERTEX_SHADER,
                source: "attribute vec2 p; void main() { gl_Position = vec4(p, 0.0, 1.0); }".into(),
                module: None,
                compiled: false,
                log: String::new(),
                delete_pending: false,
                attached: 0,
            },
        );
        let fragment = gl.create();
        gl.shaders.insert(fragment, Shader { kind: FRAGMENT_SHADER, source: "precision mediump float; uniform vec4 color; void main() { gl_FragColor = color; }".into(), module: None, compiled: false, log: String::new(), delete_pending: false, attached: 0 });
        gl.compile_shader(vertex);
        gl.compile_shader(fragment);
        let program = gl.create();
        gl.programs.insert(
            program,
            Program {
                shaders: vec![vertex, fragment],
                ..Program::default()
            },
        );
        gl.link_program(program);
        assert!(
            gl.programs[&program].link_status,
            "{}",
            gl.programs[&program].log
        );
        gl.program = program;
        let linked = gl
            .programs
            .get_mut(&program)
            .and_then(|program| program.linked.as_mut())
            .expect("linked");
        let color = linked
            .uniforms
            .iter()
            .find(|uniform| uniform.name == "color")
            .expect("a uniform")
            .slots[1]
            .expect("in the fragment shader");
        for (index, value) in [1.0_f32, 0.0, 0.0, 1.0].iter().enumerate() {
            linked.memory[1][(color + index) * LANES..(color + index + 1) * LANES]
                .fill(value.to_bits());
        }
        let buffer = gl.create();
        gl.buffers.insert(
            buffer,
            Buffer {
                data: Vec::new(),
                usage: STATIC_DRAW,
                element: None,
            },
        );
        gl.bind_buffer(ARRAY_BUFFER, buffer);
        let triangle: Vec<u8> = [-1.0_f32, -1.0, 1.0, -1.0, -1.0, 1.0]
            .iter()
            .flat_map(|value| value.to_le_bytes())
            .collect();
        gl.buffer_data(ARRAY_BUFFER, triangle, STATIC_DRAW);
        let array = gl.current_vertex_array_mut();
        array.attribs[0] = AttribArray {
            enabled: true,
            size: 2,
            buffer,
            ..AttribArray::default()
        };
        gl.clear_color = [0.0, 0.0, 1.0, 1.0];
        gl.clear(COLOR_BUFFER_BIT);
        gl.draw(TRIANGLES, 0, 3, None, 1);
        assert_eq!(gl.take_error(), NO_ERROR);
        let mut pixels = vec![0_u8; 4 * 4 * 4];
        gl.read_pixels(0, 0, 4, 4, RGBA, UNSIGNED_BYTE, &mut pixels);
        // the lower-left half is red, the rest still blue
        assert_eq!(&pixels[0..4], &[255, 0, 0, 255]);
        assert_eq!(&pixels[60..64], &[0, 0, 255, 255]);
        gl.draw(TRIANGLES, 0, 4, None, 1);
        assert_eq!(gl.take_error(), INVALID_OPERATION);
        let image = gl.present();
        assert_eq!(image.pixels[12].red, 255);
        assert!(gl.clear_pending);
    }
}
