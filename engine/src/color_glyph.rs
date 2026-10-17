// colour glyphs: COLR version 0 layers painted from a CPAL palette, and the PNG bitmaps of
// CBDT strikes, as emoji fonts carry them

use crate::css::Color;
use crate::font::{FontFace, rasterize};
use crate::image::Image;

// a glyph drawn in its own colours, placed like a coverage bitmap
#[derive(Clone, Debug, PartialEq)]
pub struct ColorGlyph {
    pub(crate) left: i32,
    pub(crate) top: i32,
    pub(crate) width: usize,
    pub(crate) height: usize,
    pub(crate) pixels: Vec<Color>,
}

fn u16_at(data: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*data.get(at)?, *data.get(at + 1)?]))
}

fn u32_at(data: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes([
        *data.get(at)?,
        *data.get(at + 1)?,
        *data.get(at + 2)?,
        *data.get(at + 3)?,
    ]))
}

// the glyph and palette entry of each COLR version 0 layer of a glyph, bottom first
fn colr_layers(face: &FontFace, glyph: u16) -> Option<Vec<(u16, u16)>> {
    let data = face.data();
    let colr = face.table(b"COLR")?;
    let base_count = usize::from(u16_at(data, colr + 2)?);
    let bases = colr + u32_at(data, colr + 4)? as usize;
    let layers = colr + u32_at(data, colr + 8)? as usize;
    let (mut low, mut high) = (0, base_count);
    while low < high {
        let middle = (low + high) / 2;
        let record = bases + middle * 6;
        let base = u16_at(data, record)?;
        match base.cmp(&glyph) {
            std::cmp::Ordering::Equal => {
                let first = usize::from(u16_at(data, record + 2)?);
                let count = usize::from(u16_at(data, record + 4)?);
                return (first..first + count)
                    .map(|layer| {
                        let at = layers + layer * 4;
                        Some((u16_at(data, at)?, u16_at(data, at + 2)?))
                    })
                    .collect();
            }
            std::cmp::Ordering::Less => low = middle + 1,
            std::cmp::Ordering::Greater => high = middle,
        }
    }
    None
}

// an entry of the first CPAL palette; 0xFFFF is the text's own colour
fn palette_color(face: &FontFace, index: u16, foreground: Color) -> Color {
    if index == 0xFFFF {
        return foreground;
    }
    let lookup = || -> Option<Color> {
        let data = face.data();
        let cpal = face.table(b"CPAL")?;
        let entries = u16_at(data, cpal + 2)?;
        if index >= entries {
            return None;
        }
        let records = cpal + u32_at(data, cpal + 8)? as usize;
        let first = usize::from(u16_at(data, cpal + 12)?);
        let at = records + (first + usize::from(index)) * 4;
        let bytes = data.get(at..at + 4)?;
        Some(Color::rgba(bytes[2], bytes[1], bytes[0], bytes[3]))
    };
    lookup().unwrap_or(foreground)
}

pub(crate) fn has_color(face: &FontFace) -> bool {
    [b"COLR", b"CBDT", b"sbix"]
        .into_iter()
        .any(|tag| face.table(tag).is_some())
}

// characters drawn as colour emoji without a variation selector asking for it: the pictograph,
// emoticon, transport, supplemental symbol and flag blocks
pub(crate) fn prefers_emoji(character: char) -> bool {
    matches!(
        u32::from(character),
        0x1F1E6..=0x1F1FF | 0x1F300..=0x1F64F | 0x1F680..=0x1F6FF | 0x1F900..=0x1F9FF | 0x1FA70..=0x1FAFF
    )
}

pub(crate) fn is_variation_selector(character: char) -> bool {
    matches!(character, '\u{FE00}'..='\u{FE0F}')
}

pub(crate) fn render(
    face: &FontFace,
    glyph: u16,
    size: f32,
    subpixel: f32,
    foreground: Color,
) -> Option<ColorGlyph> {
    if let Some(layers) = colr_layers(face, glyph) {
        return render_layers(face, &layers, size, subpixel, foreground);
    }
    bitmap(face, glyph, size).or_else(|| sbix(face, glyph, size))
}

fn render_layers(
    face: &FontFace,
    layers: &[(u16, u16)],
    size: f32,
    subpixel: f32,
    foreground: Color,
) -> Option<ColorGlyph> {
    let scale = size / face.units_per_em();
    let rendered = layers
        .iter()
        .filter_map(|&(layer, palette)| {
            let coverage = rasterize(&face.outline(layer)?, scale, subpixel)?;
            Some((coverage, palette_color(face, palette, foreground)))
        })
        .collect::<Vec<_>>();
    let left = rendered.iter().map(|(bitmap, _)| bitmap.left).min()?;
    let top = rendered.iter().map(|(bitmap, _)| bitmap.top).min()?;
    let right = rendered
        .iter()
        .map(|(bitmap, _)| bitmap.left + bitmap.width as i32)
        .max()?;
    let bottom = rendered
        .iter()
        .map(|(bitmap, _)| bitmap.top + bitmap.height as i32)
        .max()?;
    let (width, height) = ((right - left) as usize, (bottom - top) as usize);
    let mut image = Image::new(width as u32, height as u32);
    for (bitmap, color) in rendered {
        for row in 0..bitmap.height {
            for column in 0..bitmap.width {
                let coverage = bitmap.coverage[row * bitmap.width + column];
                if coverage == 0 {
                    continue;
                }
                let alpha = (u32::from(color.alpha) * u32::from(coverage) / 255) as u8;
                image.blend_pixel(
                    bitmap.left - left + column as i32,
                    bitmap.top - top + row as i32,
                    Color { alpha, ..color },
                );
            }
        }
    }
    Some(ColorGlyph {
        left,
        top,
        width,
        height,
        pixels: image.pixels,
    })
}

// a CBDT bitmap from the strike nearest above the size, scaled to it
fn bitmap(face: &FontFace, glyph: u16, size: f32) -> Option<ColorGlyph> {
    let data = face.data();
    let cblc = face.table(b"CBLC")?;
    let cbdt = face.table(b"CBDT")?;
    let strikes = u32_at(data, cblc + 4)? as usize;
    let mut chosen: Option<(usize, u8)> = None;
    for strike in 0..strikes.min(64) {
        let record = cblc + 8 + strike * 48;
        let (first, last) = (u16_at(data, record + 40)?, u16_at(data, record + 42)?);
        if !(first..=last).contains(&glyph) {
            continue;
        }
        let ppem = *data.get(record + 44)?;
        let better =
            chosen.is_none_or(|(_, current)| closer_strike(ppem.into(), current.into(), size));
        if better {
            chosen = Some((record, ppem));
        }
    }
    let (record, ppem) = chosen?;
    let array = cblc + u32_at(data, record)? as usize;
    let tables = u32_at(data, record + 8)? as usize;
    for table in 0..tables.min(1024) {
        let entry = array + table * 8;
        let (first, last) = (u16_at(data, entry)?, u16_at(data, entry + 2)?);
        if !(first..=last).contains(&glyph) {
            continue;
        }
        let subtable = array + u32_at(data, entry + 4)? as usize;
        let index_format = u16_at(data, subtable)?;
        let image_format = u16_at(data, subtable + 2)?;
        let image_data = cbdt + u32_at(data, subtable + 4)? as usize;
        let offset = usize::from(glyph - first);
        let start = match index_format {
            1 => u32_at(data, subtable + 8 + offset * 4)? as usize,
            3 => usize::from(u16_at(data, subtable + 8 + offset * 2)?),
            _ => return None,
        };
        let at = image_data + start;
        let (bearing_x, bearing_y, png) = match image_format {
            17 => {
                let length = u32_at(data, at + 5)? as usize;
                (
                    data[at + 2] as i8,
                    data[at + 3] as i8,
                    data.get(at + 9..at + 9 + length)?,
                )
            }
            18 => {
                let length = u32_at(data, at + 8)? as usize;
                (
                    data[at + 2] as i8,
                    data[at + 3] as i8,
                    data.get(at + 12..at + 12 + length)?,
                )
            }
            _ => return None,
        };
        let image = crate::png::decode_png(png).ok()?;
        let scale = size / f32::from(ppem.max(1));
        return Some(scaled(
            &image,
            scale,
            f32::from(bearing_x) * scale,
            f32::from(bearing_y) * scale,
        ));
    }
    None
}

// an sbix PNG from the strike nearest above the size, as Apple's colour fonts carry them
fn sbix(face: &FontFace, glyph: u16, size: f32) -> Option<ColorGlyph> {
    let data = face.data();
    let sbix = face.table(b"sbix")?;
    let strikes = u32_at(data, sbix + 4)? as usize;
    let mut chosen: Option<(usize, u16)> = None;
    for strike in 0..strikes.min(64) {
        let at = sbix + u32_at(data, sbix + 8 + strike * 4)? as usize;
        let ppem = u16_at(data, at)?;
        let better = chosen.is_none_or(|(_, current)| closer_strike(ppem, current, size));
        if better {
            chosen = Some((at, ppem));
        }
    }
    let (strike, ppem) = chosen?;
    let offsets = strike + 4 + usize::from(glyph) * 4;
    let start = strike + u32_at(data, offsets)? as usize;
    let end = strike + u32_at(data, offsets + 4)? as usize;
    if end < start + 8 || data.get(start + 4..start + 8)? != b"png " {
        return None;
    }
    let origin_x = u16_at(data, start)? as i16;
    let origin_y = u16_at(data, start + 2)? as i16;
    let image = crate::png::decode_png(data.get(start + 8..end)?).ok()?;
    let scale = size / f32::from(ppem.max(1));
    // the origin offsets place the image's bottom left corner from the glyph origin
    let top = f32::from(origin_y) + image.height as f32;
    Some(scaled(
        &image,
        scale,
        f32::from(origin_x) * scale,
        top * scale,
    ))
}

// whether a strike suits a size better than the one chosen so far: the smallest at or above
// the size, else the largest below it
fn closer_strike(ppem: u16, current: u16, size: f32) -> bool {
    match (f32::from(ppem) >= size, f32::from(current) >= size) {
        (true, true) => ppem < current,
        (true, false) => true,
        (false, true) => false,
        (false, false) => ppem > current,
    }
}

// an image resampled by a factor, with its top-left corner at a bearing from the pen
fn scaled(image: &Image, scale: f32, left: f32, top: f32) -> ColorGlyph {
    let width = ((image.width as f32 * scale).round() as usize).max(1);
    let height = ((image.height as f32 * scale).round() as usize).max(1);
    let mut pixels = Vec::with_capacity(width * height);
    for row in 0..height {
        for column in 0..width {
            let source_x = ((column as f32 + 0.5) / scale) as u32;
            let source_y = ((row as f32 + 0.5) / scale) as u32;
            pixels.push(image.pixel(
                source_x.min(image.width - 1),
                source_y.min(image.height - 1),
            ));
        }
    }
    ColorGlyph {
        left: left.round() as i32,
        top: -top.round() as i32,
        width,
        height,
        pixels,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> FontFace {
        let path = format!(
            "{}/tests/fixtures/fonts/ceri-color.ttf",
            env!("CARGO_MANIFEST_DIR")
        );
        FontFace::parse(std::fs::read(path).expect("fixture font should exist"))
            .expect("fixture should parse")
    }

    #[test]
    fn colr_layers_take_palette_colours_and_the_text_colour() {
        let face = fixture();
        assert!(has_color(&face));
        let glyph = face.glyph_for('\u{263A}');
        let blue = Color::rgba(0, 0, 255, 255);
        let drawn = render(&face, glyph, 20.0, 0.0, blue).expect("the layered glyph draws");
        assert!((20..=21).contains(&drawn.width) && (20..=21).contains(&drawn.height));
        // the outer box is the palette's red, the inner box takes the text colour
        assert_eq!(
            drawn.pixels[2 * drawn.width + 2],
            Color::rgba(255, 0, 0, 255)
        );
        assert_eq!(drawn.pixels[10 * drawn.width + 10], blue);
        assert!((-21..=-20).contains(&drawn.top));
    }

    #[test]
    fn cbdt_bitmaps_scale_from_their_strike() {
        let face = fixture();
        let glyph = face.glyph_for('\u{25CF}');
        let drawn =
            render(&face, glyph, 40.0, 0.0, Color::rgba(0, 0, 0, 255)).expect("the bitmap draws");
        assert_eq!((drawn.width, drawn.height), (20, 20));
        assert_eq!(drawn.top, -20);
        assert!(
            drawn
                .pixels
                .iter()
                .all(|pixel| *pixel == Color::rgba(0, 255, 0, 255))
        );
    }

    #[test]
    fn sbix_bitmaps_sit_on_their_origin() {
        let face = fixture();
        let glyph = face.glyph_for('\u{2605}');
        let drawn =
            render(&face, glyph, 20.0, 0.0, Color::rgba(0, 0, 0, 255)).expect("the bitmap draws");
        assert_eq!((drawn.width, drawn.height, drawn.top), (10, 10, -10));
        assert!(
            drawn
                .pixels
                .iter()
                .all(|pixel| *pixel == Color::rgba(0, 0, 255, 255))
        );
    }

    #[test]
    fn emoji_presentation_and_selectors() {
        assert!(prefers_emoji('\u{1F600}'));
        assert!(!prefers_emoji('\u{2764}'));
        assert!(is_variation_selector('\u{FE0F}'));
        assert!(!is_variation_selector('a'));
    }
}
