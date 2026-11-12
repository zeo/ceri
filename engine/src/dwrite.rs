// glyph masks from directwrite with the rendering modes chromium's skia picks on windows
// (SkScalerContext_win_dw.cpp), so filled text gets the same cleartype coverage as edge

use crate::font::{FontFace, GlyphBitmap};
use std::ffi::c_void;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use windows::Win32::Foundation::{E_NOINTERFACE, S_OK};
use windows::Win32::Graphics::DirectWrite::*;
use windows::core::{GUID, HRESULT, IUnknown, IUnknown_Vtbl, Interface};

thread_local! {
    static FACTORY: Option<(IDWriteFactory6, IDWriteInMemoryFontFileLoader)> = unsafe {
        let factory: Option<IDWriteFactory6> = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).ok();
        factory.and_then(|factory| {
            let loader = factory.CreateInMemoryFontFileLoader().ok()?;
            factory.RegisterFontFileLoader(&loader).ok()?;
            Some((factory, loader))
        })
    };
}

// the font bytes directwrite reads in place, released when its last font file goes
#[repr(C)]
struct FontBytes {
    vtable: *const IUnknown_Vtbl,
    references: AtomicU32,
    _data: Arc<[u8]>,
}

static FONT_BYTES_VTABLE: IUnknown_Vtbl = IUnknown_Vtbl {
    QueryInterface: query_font_bytes,
    AddRef: add_font_bytes_reference,
    Release: release_font_bytes,
};

unsafe extern "system" fn query_font_bytes(
    this: *mut c_void,
    iid: *const GUID,
    interface: *mut *mut c_void,
) -> HRESULT {
    unsafe {
        if *iid == IUnknown::IID {
            add_font_bytes_reference(this);
            *interface = this;
            S_OK
        } else {
            *interface = std::ptr::null_mut();
            E_NOINTERFACE
        }
    }
}

unsafe extern "system" fn add_font_bytes_reference(this: *mut c_void) -> u32 {
    let bytes = unsafe { &*this.cast::<FontBytes>() };
    bytes.references.fetch_add(1, Ordering::Relaxed) + 1
}

unsafe extern "system" fn release_font_bytes(this: *mut c_void) -> u32 {
    let left = unsafe { &*this.cast::<FontBytes>() }
        .references
        .fetch_sub(1, Ordering::Release)
        - 1;
    if left == 0 {
        std::sync::atomic::fence(Ordering::Acquire);
        drop(unsafe { Box::from_raw(this.cast::<FontBytes>()) });
    }
    left
}

pub(crate) fn font_face(data: &Arc<[u8]>, axes: &[([u8; 4], f32)]) -> Option<IDWriteFontFace> {
    FACTORY.with(|factory| {
        let (factory, loader) = factory.as_ref()?;
        let owner = Box::into_raw(Box::new(FontBytes {
            vtable: &FONT_BYTES_VTABLE,
            references: AtomicU32::new(1),
            _data: data.clone(),
        }));
        let owner = unsafe { IUnknown::from_raw(owner.cast()) };
        let values = axes
            .iter()
            .map(|&(tag, value)| DWRITE_FONT_AXIS_VALUE {
                axisTag: DWRITE_FONT_AXIS_TAG(u32::from_le_bytes(tag)),
                value,
            })
            .collect::<Vec<_>>();
        unsafe {
            let file = loader
                .CreateInMemoryFontFileReference(
                    factory,
                    data.as_ptr().cast(),
                    u32::try_from(data.len()).ok()?,
                    &owner,
                )
                .ok()?;
            factory
                .CreateFontFaceReference(&file, 0, DWRITE_FONT_SIMULATIONS_NONE, &values)
                .and_then(|reference| reference.CreateFontFace())
                .and_then(|face| face.cast())
                .ok()
        }
    })
}

// a filled glyph's red, green and blue coverage at a quarter-pixel pen offset, with
// chromium's synthetic oblique as a quarter skew
pub(crate) fn glyph(
    face: &FontFace,
    dwrite: &IDWriteFontFace,
    glyph: u16,
    size: f32,
    subpixel: f32,
    oblique: bool,
) -> Option<GlyphBitmap> {
    let (render_size, rendering, measuring) = rendering_mode(face, size, oblique);
    let index = glyph;
    let advance = 0.0f32;
    let offset = DWRITE_GLYPH_OFFSET::default();
    let run = DWRITE_GLYPH_RUN {
        fontFace: std::mem::ManuallyDrop::new(Some(dwrite.clone())),
        fontEmSize: render_size,
        glyphCount: 1,
        glyphIndices: &index,
        glyphAdvances: &advance,
        glyphOffsets: &offset,
        isSideways: false.into(),
        bidiLevel: 0,
    };
    let transform = DWRITE_MATRIX {
        m11: 1.0,
        m12: 0.0,
        m21: if oblique { -0.25 } else { 0.0 },
        m22: 1.0,
        dx: subpixel,
        dy: 0.0,
    };
    let mask = FACTORY.with(|factory| {
        // the original factory's analysis, which takes pixels per dip as skia calls it
        let factory: &IDWriteFactory = &factory.as_ref()?.0;
        let analysis = unsafe {
            factory.CreateGlyphRunAnalysis(
                &run,
                1.0,
                Some(&transform),
                rendering,
                measuring,
                0.0,
                0.0,
            )
        }
        .ok()?;
        texture(&analysis, DWRITE_TEXTURE_CLEARTYPE_3x1).or_else(|| {
            // a glyph with no smoothed coverage, as in a bitmap strike, comes back bilevel
            let analysis = unsafe {
                factory.CreateGlyphRunAnalysis(
                    &run,
                    1.0,
                    Some(&transform),
                    DWRITE_RENDERING_MODE_ALIASED,
                    measuring,
                    0.0,
                    0.0,
                )
            }
            .ok()?;
            let mut mask = texture(&analysis, DWRITE_TEXTURE_ALIASED_1x1)?;
            mask.coverage = mask
                .coverage
                .iter()
                .flat_map(|&value| [if value & 0x80 != 0 { 255 } else { 0 }; 3])
                .collect();
            Some(mask)
        })
    });
    drop(std::mem::ManuallyDrop::into_inner(run.fontFace));
    mask
}

fn texture(analysis: &IDWriteGlyphRunAnalysis, kind: DWRITE_TEXTURE_TYPE) -> Option<GlyphBitmap> {
    let bounds = unsafe { analysis.GetAlphaTextureBounds(kind) }.ok()?;
    if bounds.left >= bounds.right || bounds.top >= bounds.bottom {
        return None;
    }
    let width = usize::try_from(bounds.right - bounds.left).ok()?;
    let height = usize::try_from(bounds.bottom - bounds.top).ok()?;
    let channels = if kind == DWRITE_TEXTURE_CLEARTYPE_3x1 {
        3
    } else {
        1
    };
    let mut coverage = vec![0; width.checked_mul(height)?.checked_mul(channels)?];
    unsafe { analysis.CreateAlphaTexture(kind, &bounds, &mut coverage) }.ok()?;
    Some(GlyphBitmap {
        left: bounds.left,
        top: bounds.top,
        width,
        height,
        coverage,
        subpixels: true,
    })
}

// skia's choice of size and modes: gdi classic for a bitmap strike, the gasp table's symmetric
// smoothing flag when it has a version 1 table, symmetric above 20px or for unhinted faces, and
// asymmetric natural rendering at the gdi-rounded size for small hinted text
fn rendering_mode(
    face: &FontFace,
    size: f32,
    oblique: bool,
) -> (f32, DWRITE_RENDERING_MODE, DWRITE_MEASURING_MODE) {
    let gdi_size = match (size * 64.0).round() / 64.0 {
        0.0 => 1.0,
        rounded => rounded,
    };
    if has_bitmap_strike(face, gdi_size.trunc() as i32) {
        return if oblique {
            (
                gdi_size,
                DWRITE_RENDERING_MODE_NATURAL_SYMMETRIC,
                DWRITE_MEASURING_MODE_GDI_CLASSIC,
            )
        } else {
            (
                gdi_size,
                DWRITE_RENDERING_MODE_GDI_CLASSIC,
                DWRITE_MEASURING_MODE_GDI_CLASSIC,
            )
        };
    }
    match gasp_range(face, gdi_size.round() as i32) {
        Some(range) if range.version >= 1 => {
            let rendering = if range.flags & GASP_SYMMETRIC_SMOOTHING != 0 {
                DWRITE_RENDERING_MODE_NATURAL_SYMMETRIC
            } else {
                DWRITE_RENDERING_MODE_NATURAL
            };
            (size, rendering, DWRITE_MEASURING_MODE_NATURAL)
        }
        _ if size > 20.0 || !is_hinted(face) => (
            size,
            DWRITE_RENDERING_MODE_NATURAL_SYMMETRIC,
            DWRITE_MEASURING_MODE_NATURAL,
        ),
        _ => (
            gdi_size,
            DWRITE_RENDERING_MODE_NATURAL,
            DWRITE_MEASURING_MODE_NATURAL,
        ),
    }
}

const GASP_GRIDFIT: u16 = 0x0001;
const GASP_SYMMETRIC_SMOOTHING: u16 = 0x0008;

struct GaspRange {
    minimum: i32,
    maximum: i32,
    version: u16,
    flags: u16,
}

fn gasp_range(face: &FontFace, size: i32) -> Option<GaspRange> {
    let data = face.data();
    let gasp = face.table(b"gasp")?;
    let version = read_u16(data, gasp)?;
    if version > 1 {
        return None;
    }
    let count = read_u16(data, gasp + 2)?;
    if count > 1024 {
        return None;
    }
    let mut minimum = -1;
    for index in 0..usize::from(count) {
        let maximum = i32::from(read_u16(data, gasp + 4 + index * 4)?);
        if minimum < size && size <= maximum {
            return Some(GaspRange {
                minimum: minimum + 1,
                maximum,
                version,
                flags: read_u16(data, gasp + 6 + index * 4)?,
            });
        }
        minimum = maximum;
    }
    None
}

// truetype outlines that carry bytecode instructions
fn is_hinted(face: &FontFace) -> bool {
    let data = face.data();
    face.table(b"maxp").is_some_and(|maxp| {
        read_u32(data, maxp) == Some(0x0001_0000)
            && read_u16(data, maxp + 26).is_some_and(|size| size != 0)
    })
}

// an EBLC strike of at least four glyphs at the size, or the gasp range around it when that
// range is grid fitted only, or an EBSC scaled strike there
fn has_bitmap_strike(face: &FontFace, ppem: i32) -> bool {
    let data = face.data();
    let (minimum, maximum) = gasp_range(face, ppem)
        .filter(|range| range.flags == GASP_GRIDFIT)
        .map_or((ppem, ppem), |range| (range.minimum, range.maximum));
    let in_range = |x: u8, y: u8| x == y && (minimum..=maximum).contains(&i32::from(x));
    // the start of each record of a version 2 table, none when the table is missing or short
    let records = |table: &[u8; 4], size: usize| {
        let at = face.table(table)?;
        let count = usize::try_from(read_u32(data, at + 4)?).ok()?;
        (read_u32(data, at) == Some(0x0002_0000)
            && count <= 1024
            && data.len() >= at + 8 + size * count)
            .then(|| (0..count).map(move |index| at + 8 + index * size))
    };
    let Some(mut strikes) = records(b"EBLC", 48) else {
        return false;
    };
    strikes.any(|strike| {
        in_range(data[strike + 44], data[strike + 45])
            && u32::from(read_u16(data, strike + 42).unwrap_or(0))
                >= u32::from(read_u16(data, strike + 40).unwrap_or(0)) + 3
    }) || records(b"EBSC", 28)
        .is_some_and(|mut scales| scales.any(|scale| in_range(data[scale + 24], data[scale + 25])))
}

fn read_u16(data: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(data.get(at..at + 2)?.try_into().ok()?))
}

fn read_u32(data: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(data.get(at..at + 4)?.try_into().ok()?))
}
