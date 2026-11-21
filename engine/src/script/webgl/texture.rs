// texture and renderbuffer storage: a level keeps RGBA floats already rounded to its internal
// format and laid out the way its base format samples, so rendering, sampling and reading
// back all see the same values; rows run bottom to top as GL numbers them

use super::constants::*;
use super::glsl::{LANES, Lod, SamplerKind};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Kind {
    Unorm,
    Snorm,
    Float,
    Int,
    Uint,
    Srgb,
    Depth,
    DepthStencil,
    Stencil,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Format {
    // the sized internal format, or the unsized one a WebGL 1 upload named
    pub(crate) internal: u32,
    // RED, RG, RGB, RGBA, ALPHA, LUMINANCE, LUMINANCE_ALPHA, DEPTH_COMPONENT, DEPTH_STENCIL
    pub(crate) base: u32,
    pub(crate) kind: Kind,
    pub(crate) bits: [u8; 4],
}

const SIZED: &[(u32, u32, Kind, [u8; 4])] = &[
    (R8, RED, Kind::Unorm, [8, 0, 0, 0]),
    (R8_SNORM, RED, Kind::Snorm, [8, 0, 0, 0]),
    (R16F, RED, Kind::Float, [16, 0, 0, 0]),
    (R32F, RED, Kind::Float, [32, 0, 0, 0]),
    (R8UI, RED, Kind::Uint, [8, 0, 0, 0]),
    (R8I, RED, Kind::Int, [8, 0, 0, 0]),
    (R16UI, RED, Kind::Uint, [16, 0, 0, 0]),
    (R16I, RED, Kind::Int, [16, 0, 0, 0]),
    (R32UI, RED, Kind::Uint, [32, 0, 0, 0]),
    (R32I, RED, Kind::Int, [32, 0, 0, 0]),
    (RG8, RG, Kind::Unorm, [8, 8, 0, 0]),
    (RG8_SNORM, RG, Kind::Snorm, [8, 8, 0, 0]),
    (RG16F, RG, Kind::Float, [16, 16, 0, 0]),
    (RG32F, RG, Kind::Float, [32, 32, 0, 0]),
    (RG8UI, RG, Kind::Uint, [8, 8, 0, 0]),
    (RG8I, RG, Kind::Int, [8, 8, 0, 0]),
    (RG16UI, RG, Kind::Uint, [16, 16, 0, 0]),
    (RG16I, RG, Kind::Int, [16, 16, 0, 0]),
    (RG32UI, RG, Kind::Uint, [32, 32, 0, 0]),
    (RG32I, RG, Kind::Int, [32, 32, 0, 0]),
    (RGB8, RGB, Kind::Unorm, [8, 8, 8, 0]),
    (SRGB8, RGB, Kind::Srgb, [8, 8, 8, 0]),
    (RGB565, RGB, Kind::Unorm, [5, 6, 5, 0]),
    (RGB8_SNORM, RGB, Kind::Snorm, [8, 8, 8, 0]),
    (R11F_G11F_B10F, RGB, Kind::Float, [11, 11, 10, 0]),
    (RGB9_E5, RGB, Kind::Float, [9, 9, 9, 0]),
    (RGB16F, RGB, Kind::Float, [16, 16, 16, 0]),
    (RGB32F, RGB, Kind::Float, [32, 32, 32, 0]),
    (RGB8UI, RGB, Kind::Uint, [8, 8, 8, 0]),
    (RGB8I, RGB, Kind::Int, [8, 8, 8, 0]),
    (RGB16UI, RGB, Kind::Uint, [16, 16, 16, 0]),
    (RGB16I, RGB, Kind::Int, [16, 16, 16, 0]),
    (RGB32UI, RGB, Kind::Uint, [32, 32, 32, 0]),
    (RGB32I, RGB, Kind::Int, [32, 32, 32, 0]),
    (RGBA8, RGBA, Kind::Unorm, [8, 8, 8, 8]),
    (SRGB8_ALPHA8, RGBA, Kind::Srgb, [8, 8, 8, 8]),
    (RGBA8_SNORM, RGBA, Kind::Snorm, [8, 8, 8, 8]),
    (RGB5_A1, RGBA, Kind::Unorm, [5, 5, 5, 1]),
    (RGBA4, RGBA, Kind::Unorm, [4, 4, 4, 4]),
    (RGB10_A2, RGBA, Kind::Unorm, [10, 10, 10, 2]),
    (RGBA16F, RGBA, Kind::Float, [16, 16, 16, 16]),
    (RGBA32F, RGBA, Kind::Float, [32, 32, 32, 32]),
    (RGBA8UI, RGBA, Kind::Uint, [8, 8, 8, 8]),
    (RGBA8I, RGBA, Kind::Int, [8, 8, 8, 8]),
    (RGB10_A2UI, RGBA, Kind::Uint, [10, 10, 10, 2]),
    (RGBA16UI, RGBA, Kind::Uint, [16, 16, 16, 16]),
    (RGBA16I, RGBA, Kind::Int, [16, 16, 16, 16]),
    (RGBA32I, RGBA, Kind::Int, [32, 32, 32, 32]),
    (RGBA32UI, RGBA, Kind::Uint, [32, 32, 32, 32]),
    (
        DEPTH_COMPONENT16,
        DEPTH_COMPONENT,
        Kind::Depth,
        [16, 0, 0, 0],
    ),
    (
        DEPTH_COMPONENT24,
        DEPTH_COMPONENT,
        Kind::Depth,
        [24, 0, 0, 0],
    ),
    (
        DEPTH_COMPONENT32F,
        DEPTH_COMPONENT,
        Kind::Depth,
        [32, 0, 0, 0],
    ),
    (
        DEPTH24_STENCIL8,
        DEPTH_STENCIL,
        Kind::DepthStencil,
        [24, 8, 0, 0],
    ),
    (
        DEPTH32F_STENCIL8,
        DEPTH_STENCIL,
        Kind::DepthStencil,
        [32, 8, 0, 0],
    ),
    (STENCIL_INDEX8, STENCIL, Kind::Stencil, [0, 8, 0, 0]),
];

// the (internal format, format, type) triples texImage takes, the sized ones of OpenGL ES 3.0
// table 3.2 and then the unsized ones
const UPLOADS: &[(u32, u32, &[u32])] = &[
    (R8, RED, &[UNSIGNED_BYTE]),
    (R8_SNORM, RED, &[BYTE]),
    (R16F, RED, &[HALF_FLOAT, FLOAT]),
    (R32F, RED, &[FLOAT]),
    (R8UI, RED_INTEGER, &[UNSIGNED_BYTE]),
    (R8I, RED_INTEGER, &[BYTE]),
    (R16UI, RED_INTEGER, &[UNSIGNED_SHORT]),
    (R16I, RED_INTEGER, &[SHORT]),
    (R32UI, RED_INTEGER, &[UNSIGNED_INT]),
    (R32I, RED_INTEGER, &[INT]),
    (RG8, RG, &[UNSIGNED_BYTE]),
    (RG8_SNORM, RG, &[BYTE]),
    (RG16F, RG, &[HALF_FLOAT, FLOAT]),
    (RG32F, RG, &[FLOAT]),
    (RG8UI, RG_INTEGER, &[UNSIGNED_BYTE]),
    (RG8I, RG_INTEGER, &[BYTE]),
    (RG16UI, RG_INTEGER, &[UNSIGNED_SHORT]),
    (RG16I, RG_INTEGER, &[SHORT]),
    (RG32UI, RG_INTEGER, &[UNSIGNED_INT]),
    (RG32I, RG_INTEGER, &[INT]),
    (RGB8, RGB, &[UNSIGNED_BYTE]),
    (SRGB8, RGB, &[UNSIGNED_BYTE]),
    (RGB565, RGB, &[UNSIGNED_BYTE, UNSIGNED_SHORT_5_6_5]),
    (RGB8_SNORM, RGB, &[BYTE]),
    (
        R11F_G11F_B10F,
        RGB,
        &[UNSIGNED_INT_10F_11F_11F_REV, HALF_FLOAT, FLOAT],
    ),
    (RGB9_E5, RGB, &[UNSIGNED_INT_5_9_9_9_REV, HALF_FLOAT, FLOAT]),
    (RGB16F, RGB, &[HALF_FLOAT, FLOAT]),
    (RGB32F, RGB, &[FLOAT]),
    (RGB8UI, RGB_INTEGER, &[UNSIGNED_BYTE]),
    (RGB8I, RGB_INTEGER, &[BYTE]),
    (RGB16UI, RGB_INTEGER, &[UNSIGNED_SHORT]),
    (RGB16I, RGB_INTEGER, &[SHORT]),
    (RGB32UI, RGB_INTEGER, &[UNSIGNED_INT]),
    (RGB32I, RGB_INTEGER, &[INT]),
    (RGBA8, RGBA, &[UNSIGNED_BYTE]),
    (SRGB8_ALPHA8, RGBA, &[UNSIGNED_BYTE]),
    (RGBA8_SNORM, RGBA, &[BYTE]),
    (
        RGB5_A1,
        RGBA,
        &[
            UNSIGNED_BYTE,
            UNSIGNED_SHORT_5_5_5_1,
            UNSIGNED_INT_2_10_10_10_REV,
        ],
    ),
    (RGBA4, RGBA, &[UNSIGNED_BYTE, UNSIGNED_SHORT_4_4_4_4]),
    (RGB10_A2, RGBA, &[UNSIGNED_INT_2_10_10_10_REV]),
    (RGBA16F, RGBA, &[HALF_FLOAT, FLOAT]),
    (RGBA32F, RGBA, &[FLOAT]),
    (RGBA8UI, RGBA_INTEGER, &[UNSIGNED_BYTE]),
    (RGBA8I, RGBA_INTEGER, &[BYTE]),
    (RGB10_A2UI, RGBA_INTEGER, &[UNSIGNED_INT_2_10_10_10_REV]),
    (RGBA16UI, RGBA_INTEGER, &[UNSIGNED_SHORT]),
    (RGBA16I, RGBA_INTEGER, &[SHORT]),
    (RGBA32I, RGBA_INTEGER, &[INT]),
    (RGBA32UI, RGBA_INTEGER, &[UNSIGNED_INT]),
    (
        DEPTH_COMPONENT16,
        DEPTH_COMPONENT,
        &[UNSIGNED_SHORT, UNSIGNED_INT],
    ),
    (DEPTH_COMPONENT24, DEPTH_COMPONENT, &[UNSIGNED_INT]),
    (DEPTH_COMPONENT32F, DEPTH_COMPONENT, &[FLOAT]),
    (DEPTH24_STENCIL8, DEPTH_STENCIL, &[UNSIGNED_INT_24_8]),
    (
        DEPTH32F_STENCIL8,
        DEPTH_STENCIL,
        &[FLOAT_32_UNSIGNED_INT_24_8_REV],
    ),
];

const UNSIZED_TYPES: &[u32] = &[UNSIGNED_BYTE, FLOAT, HALF_FLOAT, HALF_FLOAT_OES];

impl Format {
    pub(crate) fn sized(internal: u32) -> Option<Format> {
        SIZED
            .iter()
            .find(|row| row.0 == internal)
            .map(|&(internal, base, kind, bits)| Format {
                internal,
                base,
                kind,
                bits,
            })
    }

    // the format an upload makes: a sized internal format must pair with its format and
    // type, an unsized one takes its storage from the type
    pub(crate) fn upload(
        internal: u32,
        format: u32,
        kind: u32,
        webgl2: bool,
    ) -> Result<Format, u32> {
        if let Some(row) = UPLOADS.iter().find(|row| row.0 == internal) {
            if !webgl2 && !matches!(internal, DEPTH_COMPONENT16 | DEPTH24_STENCIL8) {
                return Err(INVALID_ENUM);
            }
            if row.1 != format || !row.2.contains(&kind) {
                return Err(INVALID_OPERATION);
            }
            return Format::sized(internal).ok_or(INVALID_ENUM);
        }
        if internal != format {
            return Err(
                if matches!(
                    internal,
                    RGBA | RGB
                        | ALPHA
                        | LUMINANCE
                        | LUMINANCE_ALPHA
                        | DEPTH_COMPONENT
                        | DEPTH_STENCIL
                        | SRGB
                        | SRGB_ALPHA_EXT
                ) {
                    INVALID_OPERATION
                } else {
                    INVALID_ENUM
                },
            );
        }
        let unorm = |bits: [u8; 4]| Format {
            internal,
            base: format,
            kind: Kind::Unorm,
            bits,
        };
        let float = |bits: u8| Format {
            internal,
            base: format,
            kind: Kind::Float,
            bits: [bits; 4],
        };
        Ok(match (format, kind) {
            (RGBA, UNSIGNED_SHORT_4_4_4_4) => unorm([4, 4, 4, 4]),
            (RGBA, UNSIGNED_SHORT_5_5_5_1) => unorm([5, 5, 5, 1]),
            (RGB, UNSIGNED_SHORT_5_6_5) => unorm([5, 6, 5, 0]),
            (RGBA | RGB | ALPHA | LUMINANCE | LUMINANCE_ALPHA, UNSIGNED_BYTE) => unorm([8; 4]),
            (RGBA | RGB | ALPHA | LUMINANCE | LUMINANCE_ALPHA, FLOAT) => float(32),
            (RGBA | RGB | ALPHA | LUMINANCE | LUMINANCE_ALPHA, HALF_FLOAT | HALF_FLOAT_OES) => {
                float(16)
            }
            (SRGB, UNSIGNED_BYTE) => Format {
                internal,
                base: RGB,
                kind: Kind::Srgb,
                bits: [8; 4],
            },
            (SRGB_ALPHA_EXT, UNSIGNED_BYTE) => Format {
                internal,
                base: RGBA,
                kind: Kind::Srgb,
                bits: [8; 4],
            },
            (DEPTH_COMPONENT, UNSIGNED_SHORT) => Format {
                internal,
                base: format,
                kind: Kind::Depth,
                bits: [16, 0, 0, 0],
            },
            (DEPTH_COMPONENT, UNSIGNED_INT) => Format {
                internal,
                base: format,
                kind: Kind::Depth,
                bits: [24, 0, 0, 0],
            },
            (DEPTH_STENCIL, UNSIGNED_INT_24_8) => Format {
                internal,
                base: format,
                kind: Kind::DepthStencil,
                bits: [24, 8, 0, 0],
            },
            (
                RGBA | RGB | ALPHA | LUMINANCE | LUMINANCE_ALPHA | SRGB | SRGB_ALPHA_EXT
                | DEPTH_COMPONENT | DEPTH_STENCIL,
                _,
            ) => {
                return Err(if UNSIZED_TYPES.contains(&kind) || is_type(kind) {
                    INVALID_OPERATION
                } else {
                    INVALID_ENUM
                });
            }
            _ => return Err(INVALID_ENUM),
        })
    }

    // the format a renderbuffer gets from renderbufferStorage
    pub(crate) fn renderbuffer(internal: u32, webgl2: bool) -> Option<Format> {
        let format = match internal {
            DEPTH_STENCIL => Format::sized(DEPTH24_STENCIL8),
            RGBA4 | RGB565 | RGB5_A1 | DEPTH_COMPONENT16 | STENCIL_INDEX8 => {
                Format::sized(internal)
            }
            _ if webgl2 => Format::sized(internal).filter(Format::renderable),
            _ => None,
        }?;
        Some(Format { internal, ..format })
    }

    pub(crate) fn is_integer(&self) -> bool {
        matches!(self.kind, Kind::Int | Kind::Uint)
    }

    pub(crate) fn has_depth(&self) -> bool {
        matches!(self.kind, Kind::Depth | Kind::DepthStencil)
    }

    pub(crate) fn has_stencil(&self) -> bool {
        matches!(self.kind, Kind::DepthStencil | Kind::Stencil)
    }

    pub(crate) fn is_color(&self) -> bool {
        !self.has_depth() && !self.has_stencil()
    }

    // what can be drawn into without an extension; float formats need EXT_color_buffer_float
    pub(crate) fn renderable(&self) -> bool {
        match self.kind {
            Kind::Depth | Kind::DepthStencil | Kind::Stencil | Kind::Int | Kind::Uint => {
                !matches!(self.base, RGB)
            }
            Kind::Unorm => {
                !matches!(self.base, ALPHA | LUMINANCE | LUMINANCE_ALPHA)
                    && self.internal != RGB9_E5
            }
            Kind::Srgb => self.base == RGBA,
            Kind::Snorm => false,
            Kind::Float => {
                self.internal != RGB9_E5
                    && !matches!(self.base, ALPHA | LUMINANCE | LUMINANCE_ALPHA)
            }
        }
    }

    pub(crate) fn float_bits(&self) -> u8 {
        if self.kind == Kind::Float {
            self.bits[0]
        } else {
            0
        }
    }

    // the size of a channel as getFramebufferAttachmentParameter and getRenderbufferParameter
    // report it
    pub(crate) fn channel_bits(&self, channel: usize) -> u32 {
        let present = match self.base {
            RED => channel == 0,
            RG => channel < 2,
            RGB => channel < 3,
            RGBA => channel < 4,
            _ => false,
        };
        if present {
            u32::from(self.bits[channel])
        } else {
            0
        }
    }

    // a value written in the format's own channels, rounded and spread to RGBA
    pub(crate) fn store(&self, value: [f32; 4]) -> [f32; 4] {
        let round = |value: f32, bits: u8| -> f32 {
            match self.kind {
                Kind::Unorm | Kind::Depth | Kind::DepthStencil => {
                    let value = if value.is_nan() {
                        0.0
                    } else {
                        value.clamp(0.0, 1.0)
                    };
                    if bits == 0 || bits >= 32 {
                        return value;
                    }
                    let scale = ((1_u64 << bits) - 1) as f32;
                    (value * scale).round() / scale
                }
                Kind::Snorm => {
                    let scale = ((1_u32 << (bits - 1)) - 1) as f32;
                    (value.clamp(-1.0, 1.0) * scale).round() / scale
                }
                Kind::Float => match bits {
                    32 => value,
                    16 => half(value),
                    _ => half(value).max(0.0),
                },
                Kind::Srgb => from_srgb((to_srgb(value.clamp(0.0, 1.0)) * 255.0).round() / 255.0),
                Kind::Int => {
                    let limit = 2_f64.powi(i32::from(bits) - 1);
                    (f64::from(value.trunc())).clamp(-limit, limit - 1.0) as f32
                }
                Kind::Uint => {
                    (f64::from(value.trunc())).clamp(0.0, 2_f64.powi(i32::from(bits)) - 1.0) as f32
                }
                Kind::Stencil => value,
            }
        };
        let [r, g, b, a] = value;
        match self.base {
            RED => [round(r, self.bits[0]), 0.0, 0.0, 1.0],
            RG => [round(r, self.bits[0]), round(g, self.bits[1]), 0.0, 1.0],
            RGB => [
                round(r, self.bits[0]),
                round(g, self.bits[1]),
                round(b, self.bits[2]),
                1.0,
            ],
            ALPHA => [0.0, 0.0, 0.0, round(r, self.bits[0])],
            LUMINANCE => {
                let l = round(r, self.bits[0]);
                [l, l, l, 1.0]
            }
            LUMINANCE_ALPHA => {
                let l = round(r, self.bits[0]);
                [l, l, l, round(g, self.bits[1])]
            }
            DEPTH_COMPONENT => [round(r, self.bits[0]), 0.0, 0.0, 1.0],
            DEPTH_STENCIL | STENCIL => [round(r, self.bits[0]), (g as u32 & 0xff) as f32, 0.0, 1.0],
            _ => [
                round(r, self.bits[0]),
                round(g, self.bits[1]),
                round(b, self.bits[2]),
                round(a, self.bits[3]),
            ],
        }
    }
}

pub(crate) fn is_type(kind: u32) -> bool {
    matches!(
        kind,
        UNSIGNED_BYTE
            | BYTE
            | UNSIGNED_SHORT
            | SHORT
            | UNSIGNED_INT
            | INT
            | FLOAT
            | HALF_FLOAT
            | HALF_FLOAT_OES
            | UNSIGNED_SHORT_5_6_5
            | UNSIGNED_SHORT_4_4_4_4
            | UNSIGNED_SHORT_5_5_5_1
            | UNSIGNED_INT_2_10_10_10_REV
            | UNSIGNED_INT_10F_11F_11F_REV
            | UNSIGNED_INT_5_9_9_9_REV
            | UNSIGNED_INT_24_8
            | FLOAT_32_UNSIGNED_INT_24_8_REV
    )
}

pub(crate) fn half(value: f32) -> f32 {
    super::super::typed::half_value(super::super::typed::half_bits(f64::from(value))) as f32
}

pub(crate) fn to_srgb(linear: f32) -> f32 {
    if linear <= 0.003_130_8 {
        linear * 12.92
    } else {
        1.055 * linear.powf(1.0 / 2.4) - 0.055
    }
}

pub(crate) fn from_srgb(encoded: f32) -> f32 {
    if encoded <= 0.040_45 {
        encoded / 12.92
    } else {
        ((encoded + 0.055) / 1.055).powf(2.4)
    }
}

// the stored texels of a level: four bytes each for a format of eight bits or fewer per
// channel, as a GPU keeps it, and four floats for the rest; sRGB bytes hold encoded values
#[derive(Clone)]
pub(crate) enum Texels {
    Bytes(Vec<[u8; 4]>),
    Floats(Vec<[f32; 4]>),
}

// a run of a level's texels to read and write, such as a band of an attachment
pub(crate) enum TexelsMut<'a> {
    Bytes(&'a mut [[u8; 4]]),
    Floats(&'a mut [[f32; 4]]),
}

// whether a format's values fit a byte per channel
fn compact(format: Format) -> bool {
    matches!(format.kind, Kind::Unorm | Kind::Srgb) && format.bits.iter().all(|&bits| bits <= 8)
}

fn srgb_table() -> &'static [f32; 256] {
    static TABLE: std::sync::OnceLock<[f32; 256]> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| std::array::from_fn(|byte| from_srgb(byte as f32 / 255.0)))
}

// each byte divided by 255, so a byte reads back as exactly the value store rounded to
fn unorm_table() -> &'static [f32; 256] {
    static TABLE: std::sync::OnceLock<[f32; 256]> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| std::array::from_fn(|byte| byte as f32 / 255.0))
}

#[inline]
fn from_bytes(bytes: [u8; 4], srgb: bool) -> [f32; 4] {
    let unorm = unorm_table();
    if srgb {
        let table = srgb_table();
        [
            table[usize::from(bytes[0])],
            table[usize::from(bytes[1])],
            table[usize::from(bytes[2])],
            unorm[usize::from(bytes[3])],
        ]
    } else {
        bytes.map(|byte| unorm[usize::from(byte)])
    }
}

#[inline]
fn to_bytes(value: [f32; 4], srgb: bool) -> [u8; 4] {
    // adding a half and truncating rounds half away from zero for values in 0 to 1
    let byte = |value: f32| (value.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    if srgb {
        [
            byte(to_srgb(value[0])),
            byte(to_srgb(value[1])),
            byte(to_srgb(value[2])),
            byte(value[3]),
        ]
    } else {
        value.map(byte)
    }
}

impl TexelsMut<'_> {
    #[inline]
    pub(crate) fn get(&self, index: usize, srgb: bool) -> [f32; 4] {
        match self {
            TexelsMut::Bytes(texels) => from_bytes(texels[index], srgb),
            TexelsMut::Floats(texels) => texels[index],
        }
    }

    #[inline]
    pub(crate) fn set(&mut self, index: usize, value: [f32; 4], srgb: bool) {
        match self {
            TexelsMut::Bytes(texels) => texels[index] = to_bytes(value, srgb),
            TexelsMut::Floats(texels) => texels[index] = value,
        }
    }
}

impl<'a> TexelsMut<'a> {
    // consecutive runs of `size` texels
    pub(crate) fn chunks(self, size: usize) -> Vec<TexelsMut<'a>> {
        match self {
            TexelsMut::Bytes(texels) => texels.chunks_mut(size).map(TexelsMut::Bytes).collect(),
            TexelsMut::Floats(texels) => texels.chunks_mut(size).map(TexelsMut::Floats).collect(),
        }
    }
}

#[derive(Clone)]
pub(crate) struct Level {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) depth: u32,
    pub(crate) format: Format,
    texels: Texels,
}

impl Level {
    pub(crate) fn new(format: Format, width: u32, height: u32, depth: u32) -> Level {
        let blank = format.store([0.0; 4]);
        let blank = if format.has_depth() || format.has_stencil() {
            [0.0, 0.0, 0.0, 1.0]
        } else {
            blank
        };
        let blank = if format.base == RGBA || format.base == ALPHA || format.base == LUMINANCE_ALPHA
        {
            [blank[0], blank[1], blank[2], 0.0]
        } else {
            blank
        };
        let count = width as usize * height as usize * depth as usize;
        let texels = if compact(format) {
            Texels::Bytes(vec![to_bytes(blank, format.kind == Kind::Srgb); count])
        } else {
            Texels::Floats(vec![blank; count])
        };
        Level {
            width,
            height,
            depth,
            format,
            texels,
        }
    }

    // a level holding texels already rounded to its format
    pub(crate) fn from_floats(
        format: Format,
        width: u32,
        height: u32,
        depth: u32,
        texels: Vec<[f32; 4]>,
    ) -> Level {
        let srgb = format.kind == Kind::Srgb;
        let texels = if compact(format) {
            Texels::Bytes(
                texels
                    .into_iter()
                    .map(|texel| to_bytes(texel, srgb))
                    .collect(),
            )
        } else {
            Texels::Floats(texels)
        };
        Level {
            width,
            height,
            depth,
            format,
            texels,
        }
    }

    #[inline]
    fn srgb(&self) -> bool {
        self.format.kind == Kind::Srgb
    }

    #[inline]
    pub(crate) fn index(&self, x: u32, y: u32, z: u32) -> usize {
        ((z * self.height + y) * self.width + x) as usize
    }

    #[inline]
    pub(crate) fn get(&self, index: usize) -> [f32; 4] {
        match &self.texels {
            Texels::Bytes(texels) => from_bytes(texels[index], self.srgb()),
            Texels::Floats(texels) => texels[index],
        }
    }

    #[inline]
    pub(crate) fn set(&mut self, index: usize, value: [f32; 4]) {
        let srgb = self.srgb();
        match &mut self.texels {
            Texels::Bytes(texels) => texels[index] = to_bytes(value, srgb),
            Texels::Floats(texels) => texels[index] = value,
        }
    }

    #[inline]
    pub(crate) fn texel(&self, x: u32, y: u32, z: u32) -> [f32; 4] {
        self.get(self.index(x, y, z))
    }

    pub(crate) fn fill(&mut self, value: [f32; 4]) {
        let srgb = self.srgb();
        match &mut self.texels {
            Texels::Bytes(texels) => texels.fill(to_bytes(value, srgb)),
            Texels::Floats(texels) => texels.fill(value),
        }
    }

    // one value over a rectangle of a layer, already clipped to the level, a row at a time
    pub(crate) fn fill_rect(&mut self, layer: u32, [x0, y0, x1, y1]: [u32; 4], value: [f32; 4]) {
        let (width, plane) = (
            self.width as usize,
            self.width as usize * self.height as usize,
        );
        let rows = (y0 as usize..y1 as usize).map(|y| layer as usize * plane + y * width);
        let (x0, x1) = (x0 as usize, x1 as usize);
        let srgb = self.srgb();
        match &mut self.texels {
            Texels::Bytes(texels) => {
                let bytes = to_bytes(value, srgb);
                for row in rows {
                    texels[row + x0..row + x1].fill(bytes);
                }
            }
            Texels::Floats(texels) => {
                for row in rows {
                    texels[row + x0..row + x1].fill(value);
                }
            }
        }
    }

    // the texels of one layer, to draw into
    pub(crate) fn plane_mut(&mut self, layer: u32) -> TexelsMut<'_> {
        let plane = self.width as usize * self.height as usize;
        let range = layer as usize * plane..(layer as usize + 1) * plane;
        match &mut self.texels {
            Texels::Bytes(texels) => TexelsMut::Bytes(&mut texels[range]),
            Texels::Floats(texels) => TexelsMut::Floats(&mut texels[range]),
        }
    }

    // one layer as a level of its own
    pub(crate) fn layer(&self, layer: u32) -> Level {
        let plane = self.width as usize * self.height as usize;
        let range = layer as usize * plane..(layer as usize + 1) * plane;
        let texels = match &self.texels {
            Texels::Bytes(texels) => Texels::Bytes(texels[range].to_vec()),
            Texels::Floats(texels) => Texels::Floats(texels[range].to_vec()),
        };
        Level {
            depth: 1,
            texels,
            ..*self
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct Params {
    pub(crate) min_filter: u32,
    pub(crate) mag_filter: u32,
    pub(crate) wrap: [u32; 3],
    pub(crate) min_lod: f32,
    pub(crate) max_lod: f32,
    pub(crate) compare_mode: u32,
    pub(crate) compare_func: u32,
}

impl Default for Params {
    fn default() -> Self {
        Params {
            min_filter: NEAREST_MIPMAP_LINEAR,
            mag_filter: LINEAR,
            wrap: [REPEAT; 3],
            min_lod: -1000.0,
            max_lod: 1000.0,
            compare_mode: NONE,
            compare_func: LEQUAL,
        }
    }
}

pub(crate) struct Texture {
    pub(crate) target: u32,
    // one list of levels per cube face; other targets use the first
    pub(crate) faces: [Vec<Option<Level>>; 6],
    pub(crate) params: Params,
    pub(crate) base_level: u32,
    pub(crate) max_level: u32,
    pub(crate) immutable_levels: Option<u32>,
    pub(crate) anisotropy: f32,
}

impl Texture {
    pub(crate) fn new() -> Texture {
        Texture {
            target: 0,
            faces: Default::default(),
            params: Params::default(),
            base_level: 0,
            max_level: 1000,
            immutable_levels: None,
            anisotropy: 1.0,
        }
    }

    pub(crate) fn face_count(&self) -> usize {
        if self.target == TEXTURE_CUBE_MAP {
            6
        } else {
            1
        }
    }

    pub(crate) fn level(&self, face: usize, level: usize) -> Option<&Level> {
        self.faces.get(face)?.get(level)?.as_ref()
    }

    pub(crate) fn set_level(&mut self, face: usize, level: usize, image: Level) {
        let levels = &mut self.faces[face];
        if levels.len() <= level {
            levels.resize(level + 1, None);
        }
        levels[level] = Some(image);
    }

    pub(crate) fn base(&self) -> Option<&Level> {
        self.level(0, self.base_level as usize)
    }

    // the last level sampling may reach
    fn top_level(&self) -> usize {
        let base = self.base_level as usize;
        let Some(level) = self.base() else {
            return base;
        };
        let largest = level
            .width
            .max(level.height)
            .max(if self.target == TEXTURE_3D {
                level.depth
            } else {
                1
            });
        let chain = base + (32 - largest.leading_zeros()) as usize - 1;
        let limit = self
            .immutable_levels
            .map_or(usize::MAX, |levels| levels as usize - 1);
        chain.min(self.max_level as usize).min(limit)
    }

    fn uses_mipmaps(params: &Params) -> bool {
        !matches!(params.min_filter, NEAREST | LINEAR)
    }

    // whether sampling sees this texture's levels or reads (0, 0, 0, 1): the base level is
    // defined, cube faces match, the mipmap chain is whole when the filter uses it, and the
    // format can be filtered the way the parameters ask
    pub(crate) fn complete(
        &self,
        params: &Params,
        webgl2: bool,
        float_linear: bool,
        half_float_linear: bool,
    ) -> bool {
        let Some(base) = self.base() else {
            return false;
        };
        if base.width == 0 || base.height == 0 {
            return false;
        }
        let format = base.format;
        let linear = params.mag_filter == LINEAR
            || !matches!(params.min_filter, NEAREST | NEAREST_MIPMAP_NEAREST);
        if linear {
            if format.is_integer() || format.kind == Kind::Stencil {
                return false;
            }
            if format.kind == Kind::Float && format.float_bits() == 32 && !float_linear {
                return false;
            }
            if format.kind == Kind::Float
                && format.float_bits() == 16
                && !webgl2
                && !half_float_linear
            {
                return false;
            }
            if format.has_depth()
                && params.compare_mode == NONE
                && webgl2
                && format.internal != DEPTH_COMPONENT
                && format.internal != DEPTH_STENCIL
            {
                return false;
            }
        }
        if !webgl2 {
            let power = |side: u32| side.is_power_of_two();
            if !(power(base.width) && power(base.height))
                && (Self::uses_mipmaps(params)
                    || params.wrap[0] != CLAMP_TO_EDGE
                    || params.wrap[1] != CLAMP_TO_EDGE)
            {
                return false;
            }
        }
        let faces = self.face_count();
        for face in 1..faces {
            match self.level(face, self.base_level as usize) {
                Some(level)
                    if level.width == base.width
                        && level.height == base.height
                        && level.format == base.format
                        && level.width == level.height => {}
                _ => return false,
            }
        }
        if Self::uses_mipmaps(params) {
            let (mut width, mut height, mut depth) = (base.width, base.height, base.depth);
            for level in self.base_level as usize + 1..=self.top_level() {
                width = (width / 2).max(1);
                height = (height / 2).max(1);
                if self.target == TEXTURE_3D {
                    depth = (depth / 2).max(1);
                }
                for face in 0..faces {
                    match self.level(face, level) {
                        Some(found)
                            if found.width == width
                                && found.height == height
                                && found.depth == depth
                                && found.format == base.format => {}
                        _ => return false,
                    }
                }
            }
        }
        true
    }

    // fills every level past the base with box-filtered halves of the one before
    pub(crate) fn generate_mipmap(&mut self) {
        let top = {
            let Some(base) = self.base() else { return };
            let largest = base
                .width
                .max(base.height)
                .max(if self.target == TEXTURE_3D {
                    base.depth
                } else {
                    1
                });
            (self.base_level as usize + (32 - largest.leading_zeros()) as usize - 1)
                .min(self.max_level as usize)
        };
        let three = self.target == TEXTURE_3D;
        for face in 0..self.face_count() {
            for level in self.base_level as usize + 1..=top {
                let Some(source) = self.level(face, level - 1).cloned() else {
                    break;
                };
                let width = (source.width / 2).max(1);
                let height = (source.height / 2).max(1);
                let depth = if three {
                    (source.depth / 2).max(1)
                } else {
                    source.depth
                };
                let mut next = Level::new(source.format, width, height, depth);
                for z in 0..depth {
                    for y in 0..height {
                        for x in 0..width {
                            let mut sum = [0.0_f32; 4];
                            let mut count = 0.0;
                            for dz in 0..if three && source.depth > 1 { 2 } else { 1 } {
                                for dy in 0..if source.height > 1 { 2 } else { 1 } {
                                    for dx in 0..if source.width > 1 { 2 } else { 1 } {
                                        let sz = if three { z * 2 + dz } else { z };
                                        let texel = source.texel(x * 2 + dx, y * 2 + dy, sz);
                                        let texel = if source.format.kind == Kind::Srgb {
                                            [
                                                from_srgb(texel[0]),
                                                from_srgb(texel[1]),
                                                from_srgb(texel[2]),
                                                texel[3],
                                            ]
                                        } else {
                                            texel
                                        };
                                        for channel in 0..4 {
                                            sum[channel] += texel[channel];
                                        }
                                        count += 1.0;
                                    }
                                }
                            }
                            let mean = sum.map(|value| value / count);
                            let index = next.index(x, y, z);
                            next.set(
                                index,
                                source.format.store(channels(source.format.base, mean)),
                            );
                        }
                    }
                }
                self.set_level(face, level, next);
            }
        }
    }
}

// a stored RGBA texel back in the order of its format's channels, as store takes them
pub(crate) fn channels(base: u32, texel: [f32; 4]) -> [f32; 4] {
    match base {
        ALPHA => [texel[3], 0.0, 0.0, 0.0],
        LUMINANCE_ALPHA => [texel[0], texel[3], 0.0, 0.0],
        _ => texel,
    }
}

// a texture as one draw samples it, with the parameters of the bound sampler object
pub(crate) struct Bound<'a> {
    pub(crate) texture: &'a Texture,
    pub(crate) params: Params,
    pub(crate) complete: bool,
    pub(crate) webgl2: bool,
}

fn wrap(coordinate: i64, size: u32, mode: u32) -> u32 {
    let size = i64::from(size);
    (match mode {
        REPEAT => coordinate.rem_euclid(size),
        MIRRORED_REPEAT => {
            let period = coordinate.rem_euclid(2 * size);
            if period < size {
                period
            } else {
                2 * size - 1 - period
            }
        }
        _ => coordinate.clamp(0, size - 1),
    }) as u32
}

fn compare(func: u32, reference: f32, value: f32) -> bool {
    match func {
        NEVER => false,
        LESS => reference < value,
        EQUAL => reference == value,
        LEQUAL => reference <= value,
        GREATER => reference > value,
        NOTEQUAL => reference != value,
        GEQUAL => reference >= value,
        _ => true,
    }
}

// the cube face a direction points at and the 2D coordinates on it
pub(crate) fn cube_face(direction: [f32; 3]) -> (usize, f32, f32) {
    let [x, y, z] = direction;
    let (ax, ay, az) = (x.abs(), y.abs(), z.abs());
    let (face, sc, tc, major) = if ax >= ay && ax >= az {
        if x >= 0.0 {
            (0, -z, -y, ax)
        } else {
            (1, z, -y, ax)
        }
    } else if ay >= az {
        if y >= 0.0 {
            (2, x, z, ay)
        } else {
            (3, x, -z, ay)
        }
    } else if z >= 0.0 {
        (4, x, -y, az)
    } else {
        (5, -x, -y, az)
    };
    let major = if major == 0.0 { 1.0 } else { major };
    (face, (sc / major + 1.0) * 0.5, (tc / major + 1.0) * 0.5)
}

impl Bound<'_> {
    fn shadow(kind: SamplerKind) -> bool {
        matches!(
            kind,
            SamplerKind::TwoShadow | SamplerKind::CubeShadow | SamplerKind::TwoArrayShadow
        )
    }

    // what an incomplete texture reads as
    fn unit_value(&self, kind: SamplerKind) -> [f32; 4] {
        if Self::shadow(kind) {
            [0.0; 4]
        } else {
            [0.0, 0.0, 0.0, 1.0]
        }
    }

    // a depth texel as the sampler reads it: ES 3.00 puts depth in red only, WebGL 1's
    // depth textures repeat it like luminance
    fn depth_texel(&self, texel: [f32; 4]) -> [f32; 4] {
        if self.webgl2 {
            [texel[0], 0.0, 0.0, 1.0]
        } else {
            [texel[0], texel[0], texel[0], 1.0]
        }
    }

    fn fetch_level(&self, level: &Level, x: i64, y: i64, z: u32) -> [f32; 4] {
        let x = wrap(x, level.width, self.params.wrap[0]);
        let y = wrap(y, level.height, self.params.wrap[1]);
        level.texel(x, y, z)
    }

    // one filtered read of a level at normalized coordinates; layer picks the slice of an
    // array texture, and for a 3D texture r is the third coordinate
    #[allow(clippy::too_many_arguments)]
    fn filter(
        &self,
        level: &Level,
        linear: bool,
        s: f32,
        t: f32,
        r: f32,
        three: bool,
        reference: Option<f32>,
    ) -> [f32; 4] {
        let reference_test = |texel: [f32; 4]| -> [f32; 4] {
            match reference {
                Some(reference) => {
                    let pass = compare(
                        self.params.compare_func,
                        reference.clamp(0.0, 1.0),
                        texel[0],
                    );
                    let value = if pass { 1.0 } else { 0.0 };
                    [value, value, value, 1.0]
                }
                None if level.format.has_depth() => self.depth_texel(texel),
                None => texel,
            }
        };
        let (u, v) = (s * level.width as f32, t * level.height as f32);
        if three {
            let w = r * level.depth as f32;
            if !linear {
                let z = wrap(w.floor() as i64, level.depth, self.params.wrap[2]);
                return reference_test(self.fetch_level(
                    level,
                    u.floor() as i64,
                    v.floor() as i64,
                    z,
                ));
            }
            let (u, v, w) = (u - 0.5, v - 0.5, w - 0.5);
            let (x0, y0, z0) = (u.floor(), v.floor(), w.floor());
            let (fx, fy, fz) = (u - x0, v - y0, w - z0);
            let mut sum = [0.0_f32; 4];
            for (dz, wz) in [(0, 1.0 - fz), (1, fz)] {
                let z = wrap(z0 as i64 + dz, level.depth, self.params.wrap[2]);
                for (dy, wy) in [(0, 1.0 - fy), (1, fy)] {
                    for (dx, wx) in [(0, 1.0 - fx), (1, fx)] {
                        let texel = reference_test(self.fetch_level(
                            level,
                            x0 as i64 + dx,
                            y0 as i64 + dy,
                            z,
                        ));
                        let weight = wx * wy * wz;
                        for channel in 0..4 {
                            sum[channel] += texel[channel] * weight;
                        }
                    }
                }
            }
            return sum;
        }
        let z = if level.depth > 1 {
            (r + 0.5).floor().clamp(0.0, level.depth as f32 - 1.0) as u32
        } else {
            0
        };
        if !linear {
            return reference_test(self.fetch_level(level, u.floor() as i64, v.floor() as i64, z));
        }
        let (u, v) = (u - 0.5, v - 0.5);
        let (x0, y0) = (u.floor(), v.floor());
        let (fx, fy) = (u - x0, v - y0);
        let corners = [
            (
                self.fetch_level(level, x0 as i64, y0 as i64, z),
                (1.0 - fx) * (1.0 - fy),
            ),
            (
                self.fetch_level(level, x0 as i64 + 1, y0 as i64, z),
                fx * (1.0 - fy),
            ),
            (
                self.fetch_level(level, x0 as i64, y0 as i64 + 1, z),
                (1.0 - fx) * fy,
            ),
            (
                self.fetch_level(level, x0 as i64 + 1, y0 as i64 + 1, z),
                fx * fy,
            ),
        ];
        let mut sum = [0.0_f32; 4];
        for (texel, weight) in corners {
            let texel = reference_test(texel);
            for channel in 0..4 {
                sum[channel] += texel[channel] * weight;
            }
        }
        sum
    }

    // the level of detail from the quad's coordinate differences, scaled to texels
    fn implicit_lod(&self, kind: SamplerKind, coords: &[[f32; 4]; LANES], bias: f32) -> f32 {
        let Some(base) = self.texture.base() else {
            return 0.0;
        };
        let mut scale = [
            base.width as f32,
            base.height as f32,
            if kind == SamplerKind::Three {
                base.depth as f32
            } else {
                0.0
            },
        ];
        let project = |coordinate: [f32; 4]| -> [f32; 3] {
            if matches!(kind, SamplerKind::Cube | SamplerKind::CubeShadow) {
                let (_, s, t) = cube_face([coordinate[0], coordinate[1], coordinate[2]]);
                [s, t, 0.0]
            } else {
                [coordinate[0], coordinate[1], coordinate[2]]
            }
        };
        if matches!(
            kind,
            SamplerKind::TwoArray | SamplerKind::TwoArrayShadow | SamplerKind::TwoShadow
        ) {
            scale[2] = 0.0;
        }
        let (origin, right, below) = (project(coords[0]), project(coords[1]), project(coords[2]));
        let length = |other: [f32; 3]| -> f32 {
            (0..3)
                .map(|axis| ((other[axis] - origin[axis]) * scale[axis]).powi(2))
                .sum::<f32>()
                .sqrt()
        };
        let rho = length(right).max(length(below));
        rho.log2() + bias
    }

    fn sample_lane(&self, kind: SamplerKind, coordinate: [f32; 4], lambda: f32) -> [f32; 4] {
        let texture = self.texture;
        let params = &self.params;
        let lambda = lambda.clamp(params.min_lod, params.max_lod);
        let magnify = lambda <= 0.0 || lambda.is_nan();
        let (face, s, t, r) = match kind {
            SamplerKind::Cube | SamplerKind::CubeShadow => {
                let (face, s, t) = cube_face([coordinate[0], coordinate[1], coordinate[2]]);
                (face, s, t, 0.0)
            }
            _ => (0, coordinate[0], coordinate[1], coordinate[2]),
        };
        let reference = match kind {
            SamplerKind::TwoShadow => Some(coordinate[2]),
            SamplerKind::CubeShadow | SamplerKind::TwoArrayShadow => Some(coordinate[3]),
            _ => None,
        }
        .filter(|_| params.compare_mode == COMPARE_REF_TO_TEXTURE);
        let three = kind == SamplerKind::Three;
        let base = texture.base_level as usize;
        let read = |level: usize, linear: bool| -> [f32; 4] {
            match texture.level(face, level) {
                Some(found) => self.filter(found, linear, s, t, r, three, reference),
                None => self.unit_value(kind),
            }
        };
        if magnify {
            return read(base, params.mag_filter == LINEAR);
        }
        let top = texture.top_level();
        let linear = matches!(
            params.min_filter,
            LINEAR | LINEAR_MIPMAP_NEAREST | LINEAR_MIPMAP_LINEAR
        );
        match params.min_filter {
            NEAREST | LINEAR => read(base, linear),
            NEAREST_MIPMAP_NEAREST | LINEAR_MIPMAP_NEAREST => {
                let level = if lambda <= 0.5 {
                    base
                } else {
                    (base as f32 + (lambda + 0.5).ceil() - 1.0) as usize
                };
                read(level.min(top), linear)
            }
            _ => {
                let d = (base as f32 + lambda).min(top as f32);
                let lower = d.floor() as usize;
                if lower >= top {
                    return read(top, linear);
                }
                let fraction = d - lower as f32;
                let (a, b) = (read(lower, linear), read(lower + 1, linear));
                std::array::from_fn(|channel| a[channel] * (1.0 - fraction) + b[channel] * fraction)
            }
        }
    }

    pub(crate) fn sample(
        &self,
        kind: SamplerKind,
        coords: &[[f32; 4]; LANES],
        lod: Lod,
        out: &mut [[f32; 4]; LANES],
    ) {
        if !self.complete {
            *out = [self.unit_value(kind); LANES];
            return;
        }
        let lambdas: [f32; LANES] = match lod {
            Lod::Implicit(bias) => [self.implicit_lod(kind, coords, bias); LANES],
            Lod::Explicit(levels) => levels,
            Lod::Gradient(dx, dy) => std::array::from_fn(|lane| {
                let Some(base) = self.texture.base() else {
                    return 0.0;
                };
                let scale = [base.width as f32, base.height as f32, base.depth as f32];
                let length = |vector: [f32; 3]| {
                    (0..3)
                        .map(|axis| (vector[axis] * scale[axis]).powi(2))
                        .sum::<f32>()
                        .sqrt()
                };
                length(dx[lane]).max(length(dy[lane])).log2()
            }),
        };
        for lane in 0..LANES {
            out[lane] = self.sample_lane(kind, coords[lane], lambdas[lane]);
        }
    }

    pub(crate) fn fetch(&self, coords: [i32; 4], level: i32) -> [f32; 4] {
        let level = level + self.texture.base_level as i32;
        let Some(found) = (level >= 0)
            .then(|| self.texture.level(0, level as usize))
            .flatten()
            .filter(|_| self.complete)
        else {
            return [0.0, 0.0, 0.0, 1.0];
        };
        let inside = |value: i32, size: u32| value >= 0 && (value as u32) < size;
        if !inside(coords[0], found.width)
            || !inside(coords[1], found.height)
            || !inside(coords[2], found.depth)
        {
            return [0.0; 4];
        }
        let texel = found.texel(coords[0] as u32, coords[1] as u32, coords[2] as u32);
        if found.format.has_depth() {
            self.depth_texel(texel)
        } else {
            texel
        }
    }

    pub(crate) fn size(&self, level: i32) -> [i32; 3] {
        let level = level + self.texture.base_level as i32;
        match (level >= 0)
            .then(|| self.texture.level(0, level as usize))
            .flatten()
        {
            Some(found) => [found.width as i32, found.height as i32, found.depth as i32],
            None => [0; 3],
        }
    }
}

// how pixel transfers lay out client memory
#[derive(Clone, Copy, Debug)]
pub(crate) struct Layout {
    pub(crate) alignment: u32,
    pub(crate) row_length: u32,
    pub(crate) image_height: u32,
    pub(crate) skip_pixels: u32,
    pub(crate) skip_rows: u32,
    pub(crate) skip_images: u32,
}

// the bytes one pixel takes and how many components it has, for a format and type
pub(crate) fn pixel_size(format: u32, kind: u32) -> Option<(usize, usize)> {
    let components = match format {
        RED | RED_INTEGER | ALPHA | LUMINANCE | DEPTH_COMPONENT => 1,
        RG | RG_INTEGER | LUMINANCE_ALPHA | DEPTH_STENCIL => 2,
        RGB | RGB_INTEGER | SRGB => 3,
        RGBA | RGBA_INTEGER | SRGB_ALPHA_EXT => 4,
        _ => return None,
    };
    let size = match kind {
        UNSIGNED_BYTE | BYTE => components,
        UNSIGNED_SHORT | SHORT | HALF_FLOAT | HALF_FLOAT_OES => components * 2,
        UNSIGNED_INT | INT | FLOAT => components * 4,
        UNSIGNED_SHORT_5_6_5 | UNSIGNED_SHORT_4_4_4_4 | UNSIGNED_SHORT_5_5_5_1 => 2,
        UNSIGNED_INT_2_10_10_10_REV
        | UNSIGNED_INT_10F_11F_11F_REV
        | UNSIGNED_INT_5_9_9_9_REV
        | UNSIGNED_INT_24_8 => 4,
        FLOAT_32_UNSIGNED_INT_24_8_REV => 8,
        _ => return None,
    };
    Some((size, components))
}

impl Layout {
    // the bytes between rows and between images, and where the first pixel starts
    pub(crate) fn strides(&self, width: u32, height: u32, pixel: usize) -> (usize, usize, usize) {
        let row_pixels = if self.row_length > 0 {
            self.row_length
        } else {
            width
        } as usize;
        let alignment = self.alignment.max(1) as usize;
        let row = (row_pixels * pixel).div_ceil(alignment) * alignment;
        let rows = if self.image_height > 0 {
            self.image_height
        } else {
            height
        } as usize;
        let image = row * rows;
        let start = self.skip_images as usize * image
            + self.skip_rows as usize * row
            + self.skip_pixels as usize * pixel;
        (row, image, start)
    }

    // the bytes an upload of this size reads: whole rows, but the last row only as far as
    // its last pixel
    pub(crate) fn needed(&self, width: u32, height: u32, depth: u32, pixel: usize) -> usize {
        if width == 0 || height == 0 || depth == 0 {
            return 0;
        }
        let (row, image, start) = self.strides(width, height, pixel);
        start + (depth as usize - 1) * image + (height as usize - 1) * row + width as usize * pixel
    }
}

fn read_u16(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn read_u32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

fn unpack_float11(bits: u32, mantissa_bits: u32) -> f32 {
    let exponent = (bits >> mantissa_bits) & 0x1f;
    let mantissa = (bits & ((1 << mantissa_bits) - 1)) as f32 / (1 << mantissa_bits) as f32;
    match exponent {
        0 => mantissa * 2_f32.powi(-14),
        31 => {
            if mantissa == 0.0 {
                f32::INFINITY
            } else {
                f32::NAN
            }
        }
        _ => (1.0 + mantissa) * 2_f32.powi(exponent as i32 - 15),
    }
}

// the components of one pixel in client memory, normalized unless the format is an integer one
pub(crate) fn decode_pixel(
    bytes: &[u8],
    at: usize,
    format: u32,
    kind: u32,
    components: usize,
) -> [f32; 4] {
    let integer = matches!(
        format,
        RED_INTEGER | RG_INTEGER | RGB_INTEGER | RGBA_INTEGER
    );
    let mut value = [0.0_f32; 4];
    match kind {
        UNSIGNED_SHORT_5_6_5 => {
            let word = u32::from(read_u16(bytes, at));
            return [
                ((word >> 11) & 31) as f32 / 31.0,
                ((word >> 5) & 63) as f32 / 63.0,
                (word & 31) as f32 / 31.0,
                1.0,
            ];
        }
        UNSIGNED_SHORT_4_4_4_4 => {
            let word = u32::from(read_u16(bytes, at));
            return [12, 8, 4, 0].map(|shift| ((word >> shift) & 15) as f32 / 15.0);
        }
        UNSIGNED_SHORT_5_5_5_1 => {
            let word = u32::from(read_u16(bytes, at));
            return [
                ((word >> 11) & 31) as f32 / 31.0,
                ((word >> 6) & 31) as f32 / 31.0,
                ((word >> 1) & 31) as f32 / 31.0,
                (word & 1) as f32,
            ];
        }
        UNSIGNED_INT_2_10_10_10_REV => {
            let word = read_u32(bytes, at);
            let parts = [
                word & 1023,
                (word >> 10) & 1023,
                (word >> 20) & 1023,
                word >> 30,
            ];
            return if integer {
                parts.map(|part| part as f32)
            } else {
                [
                    parts[0] as f32 / 1023.0,
                    parts[1] as f32 / 1023.0,
                    parts[2] as f32 / 1023.0,
                    parts[3] as f32 / 3.0,
                ]
            };
        }
        UNSIGNED_INT_10F_11F_11F_REV => {
            let word = read_u32(bytes, at);
            return [
                unpack_float11(word & 0x7ff, 6),
                unpack_float11((word >> 11) & 0x7ff, 6),
                unpack_float11(word >> 22, 5),
                1.0,
            ];
        }
        UNSIGNED_INT_5_9_9_9_REV => {
            let word = read_u32(bytes, at);
            let scale = 2_f32.powi((word >> 27) as i32 - 15 - 9);
            return [
                (word & 511) as f32 * scale,
                ((word >> 9) & 511) as f32 * scale,
                ((word >> 18) & 511) as f32 * scale,
                1.0,
            ];
        }
        UNSIGNED_INT_24_8 => {
            let word = read_u32(bytes, at);
            return [
                (word >> 8) as f32 / 16_777_215.0,
                (word & 255) as f32,
                0.0,
                1.0,
            ];
        }
        FLOAT_32_UNSIGNED_INT_24_8_REV => {
            return [
                f32::from_bits(read_u32(bytes, at)),
                (read_u32(bytes, at + 4) & 255) as f32,
                0.0,
                1.0,
            ];
        }
        _ => {}
    }
    for (index, slot) in value.iter_mut().enumerate().take(components) {
        *slot = match kind {
            UNSIGNED_BYTE => {
                let byte = f32::from(bytes[at + index]);
                if integer { byte } else { byte / 255.0 }
            }
            BYTE => {
                let byte = f32::from(bytes[at + index] as i8);
                if integer {
                    byte
                } else {
                    (byte / 127.0).max(-1.0)
                }
            }
            UNSIGNED_SHORT => {
                let short = f32::from(read_u16(bytes, at + index * 2));
                if integer { short } else { short / 65535.0 }
            }
            SHORT => {
                let short = f32::from(read_u16(bytes, at + index * 2) as i16);
                if integer {
                    short
                } else {
                    (short / 32767.0).max(-1.0)
                }
            }
            UNSIGNED_INT => {
                let word = read_u32(bytes, at + index * 4);
                if integer {
                    word as f32
                } else {
                    (f64::from(word) / 4_294_967_295.0) as f32
                }
            }
            INT => {
                let word = read_u32(bytes, at + index * 4) as i32;
                if integer {
                    word as f32
                } else {
                    (f64::from(word) / 2_147_483_647.0).max(-1.0) as f32
                }
            }
            FLOAT => f32::from_bits(read_u32(bytes, at + index * 4)),
            HALF_FLOAT | HALF_FLOAT_OES => {
                super::super::typed::half_value(read_u16(bytes, at + index * 2)) as f32
            }
            _ => 0.0,
        };
    }
    value
}

// client pixels to stored texels: rows read bottom to top unless flipped, alpha multiplied in
// when asked, and every value rounded to the target format
#[allow(clippy::too_many_arguments)]
pub(crate) fn unpack(
    bytes: &[u8],
    layout: &Layout,
    width: u32,
    height: u32,
    depth: u32,
    format: u32,
    kind: u32,
    target: Format,
    flip: bool,
    premultiply: bool,
) -> Vec<[f32; 4]> {
    let (pixel, components) = pixel_size(format, kind).unwrap_or((4, 4));
    let (row, image, start) = layout.strides(width, height, pixel);
    let mut texels = Vec::with_capacity(width as usize * height as usize * depth as usize);
    for z in 0..depth as usize {
        for y in 0..height as usize {
            let source_row = if flip { height as usize - 1 - y } else { y };
            for x in 0..width as usize {
                let at = start + z * image + source_row * row + x * pixel;
                let mut value = if at + pixel <= bytes.len() {
                    decode_pixel(bytes, at, format, kind, components)
                } else {
                    [0.0; 4]
                };
                if premultiply && matches!(format, RGBA | LUMINANCE_ALPHA) {
                    let alpha = if format == RGBA { value[3] } else { value[1] };
                    for channel in value.iter_mut().take(if format == RGBA { 3 } else { 1 }) {
                        *channel *= alpha;
                    }
                }
                texels.push(target.store(value));
            }
        }
    }
    texels
}

// stored texels to client bytes for readPixels in RGBA or RGBA_INTEGER, the read formats
// WebGL allows, or the implementation format of the read buffer
pub(crate) fn pack_pixel(texel: [f32; 4], source: Format, format: u32, kind: u32, out: &mut [u8]) {
    let texel = if source.kind == Kind::Srgb {
        [
            to_srgb(texel[0]),
            to_srgb(texel[1]),
            to_srgb(texel[2]),
            texel[3],
        ]
    } else {
        texel
    };
    let components: &[usize] = match format {
        RGBA | RGBA_INTEGER => &[0, 1, 2, 3],
        RGB | RGB_INTEGER => &[0, 1, 2],
        RG | RG_INTEGER => &[0, 1],
        RED | RED_INTEGER => &[0],
        ALPHA => &[3],
        _ => &[0, 1, 2, 3],
    };
    for (index, &channel) in components.iter().enumerate() {
        let value = texel[channel];
        match kind {
            UNSIGNED_BYTE => {
                out[index] = if source.is_integer() {
                    value as u8
                } else {
                    (value.clamp(0.0, 1.0) * 255.0).round() as u8
                }
            }
            BYTE => out[index] = (value as i8) as u8,
            FLOAT => out[index * 4..index * 4 + 4].copy_from_slice(&value.to_le_bytes()),
            HALF_FLOAT | HALF_FLOAT_OES => out[index * 2..index * 2 + 2]
                .copy_from_slice(&super::super::typed::half_bits(f64::from(value)).to_le_bytes()),
            INT => out[index * 4..index * 4 + 4].copy_from_slice(&(value as i32).to_le_bytes()),
            UNSIGNED_INT => {
                out[index * 4..index * 4 + 4].copy_from_slice(&(value as u32).to_le_bytes())
            }
            UNSIGNED_SHORT => {
                out[index * 2..index * 2 + 2].copy_from_slice(&(value as u16).to_le_bytes())
            }
            SHORT => out[index * 2..index * 2 + 2].copy_from_slice(&(value as i16).to_le_bytes()),
            _ => {}
        }
    }
    if kind == UNSIGNED_INT_2_10_10_10_REV {
        let scale = |value: f32, max: f32| {
            if source.is_integer() {
                value as u32
            } else {
                (value.clamp(0.0, 1.0) * max).round() as u32
            }
        };
        let word = scale(texel[0], 1023.0)
            | scale(texel[1], 1023.0) << 10
            | scale(texel[2], 1023.0) << 20
            | scale(texel[3], 3.0) << 30;
        out[..4].copy_from_slice(&word.to_le_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_round_and_sample() {
        let rgba4 = Format::sized(RGBA4).expect("a format");
        assert_eq!(
            rgba4.store([0.5, 1.0, 0.0, 0.2]),
            [0.533_333_36, 1.0, 0.0, 0.2]
        );
        let red = Format::upload(R8, RED, UNSIGNED_BYTE, true).expect("a valid upload");
        assert_eq!(
            red.store([0.25, 0.5, 0.75, 0.0]),
            [0.250_980_4, 0.0, 0.0, 1.0]
        );
        assert_eq!(Format::upload(R8, RED, FLOAT, true), Err(INVALID_OPERATION));
        assert_eq!(
            Format::upload(RGBA, RGB, UNSIGNED_BYTE, false),
            Err(INVALID_OPERATION)
        );
        let luminance = Format::upload(LUMINANCE_ALPHA, LUMINANCE_ALPHA, UNSIGNED_BYTE, false)
            .expect("an unsized format");
        assert_eq!(luminance.store([1.0, 0.0, 0.0, 0.0]), [1.0, 1.0, 1.0, 0.0]);

        let layout = Layout {
            alignment: 4,
            row_length: 0,
            image_height: 0,
            skip_pixels: 0,
            skip_rows: 0,
            skip_images: 0,
        };
        // two RGB rows of one pixel each, padded to four bytes
        let bytes = [255, 0, 0, 9, 0, 255, 0];
        assert_eq!(layout.needed(1, 2, 1, 3), 7);
        let format = Format::upload(RGB, RGB, UNSIGNED_BYTE, false).expect("rgb");
        let texels = unpack(
            &bytes,
            &layout,
            1,
            2,
            1,
            RGB,
            UNSIGNED_BYTE,
            format,
            true,
            false,
        );
        assert_eq!(texels, [[0.0, 1.0, 0.0, 1.0], [1.0, 0.0, 0.0, 1.0]]);

        let mut texture = Texture::new();
        texture.target = TEXTURE_2D;
        let level = Level::from_floats(
            format,
            2,
            2,
            1,
            vec![
                [0.0, 0.0, 0.0, 1.0],
                [1.0, 0.0, 0.0, 1.0],
                [0.0, 1.0, 0.0, 1.0],
                [0.0, 0.0, 1.0, 1.0],
            ],
        );
        texture.set_level(0, 0, level);
        let params = Params {
            min_filter: LINEAR,
            mag_filter: LINEAR,
            wrap: [CLAMP_TO_EDGE; 3],
            ..Params::default()
        };
        assert!(texture.complete(&params, false, false, false));
        assert!(!texture.complete(&Params::default(), false, false, false));
        texture.generate_mipmap();
        assert!(texture.complete(&Params::default(), false, false, false));
        let bound = Bound {
            texture: &texture,
            params,
            complete: true,
            webgl2: false,
        };
        let mut out = [[0.0; 4]; LANES];
        bound.sample(
            SamplerKind::Two,
            &[
                [0.5, 0.5, 0.0, 0.0],
                [0.25, 0.25, 0.0, 0.0],
                [0.75, 0.25, 0.0, 0.0],
                [0.0, 0.0, 0.0, 0.0],
            ],
            Lod::Explicit([0.0; LANES]),
            &mut out,
        );
        assert_eq!(out[0], [0.25, 0.25, 0.25, 1.0]);
        assert_eq!(out[1], [0.0, 0.0, 0.0, 1.0]);
        assert_eq!(out[2], [1.0, 0.0, 0.0, 1.0]);
        let mip = texture.level(0, 1).expect("a generated level");
        assert_eq!(mip.get(0), [0.250_980_4, 0.250_980_4, 0.250_980_4, 1.0]);
        assert_eq!(cube_face([0.0, 0.0, -2.0]), (5, 0.5, 0.5));
    }
}
