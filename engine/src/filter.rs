// filter and backdrop-filter: the functions as parsed and resolved, and what they do to a
// premultiplied rgba surface, computed as chromium's skia raster backend does so blurs and
// shadows land on the same pixels

use crate::css::{Color, FilterList, Length, Style, parse_color, parse_length, split_words};

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Filter<L = f32> {
    // the standard deviation
    Blur(L),
    // unlike a box-shadow's blur radius, the third length is the standard deviation itself
    DropShadow {
        offset: (L, L),
        blur: L,
        color: Color,
    },
    // row-major 4x5 over unpremultiplied rgba in 0..1, the fifth column added
    Matrix([f32; 20]),
}

// a box's filter and backdrop-filter lists, resolved to css pixels
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Filters {
    pub(crate) own: Vec<Filter>,
    pub(crate) backdrop: Vec<Filter>,
}

impl Filters {
    pub(crate) fn of(style: &Style, font_size: f32) -> Self {
        let resolve = |list: Option<FilterList>| {
            list.map_or_else(Vec::new, |list| {
                let length = |length: Length| {
                    length
                        .resolve(0.0, font_size)
                        .unwrap_or(0.0)
                        .clamp(-2048.0, 2048.0)
                };
                list.filters()
                    .iter()
                    .map(|filter| match *filter {
                        Filter::Blur(sigma) => Filter::Blur(length(sigma).max(0.0)),
                        Filter::DropShadow {
                            offset,
                            blur,
                            color,
                        } => Filter::DropShadow {
                            offset: (length(offset.0), length(offset.1)),
                            blur: length(blur).max(0.0),
                            color,
                        },
                        Filter::Matrix(matrix) => Filter::Matrix(matrix),
                    })
                    .collect()
            })
        };
        Self {
            own: resolve(style.filter),
            backdrop: resolve(style.backdrop_filter),
        }
    }
}

// the whole list, or None when any function is unknown or malformed
pub(crate) fn parse(value: &str) -> Option<Vec<Filter<Length>>> {
    let mut filters = Vec::new();
    for (name, arguments) in crate::transform::functions(value)? {
        let [argument] = arguments[..] else {
            return None;
        };
        let name = name.to_ascii_lowercase();
        filters.push(match name.as_str() {
            "blur" if argument.is_empty() => Filter::Blur(Length::px(0.0)),
            "blur" => Filter::Blur(parse_length(argument)?),
            "drop-shadow" => parse_drop_shadow(argument)?,
            "hue-rotate" if argument.is_empty() => Filter::Matrix(hue_rotate(0.0)),
            "hue-rotate" => Filter::Matrix(hue_rotate(crate::transform::parse_angle(argument)?)),
            _ => {
                let amount = if argument.is_empty() {
                    1.0
                } else {
                    parse_amount(argument)?
                };
                Filter::Matrix(color_matrix(&name, amount)?)
            }
        });
    }
    (!filters.is_empty()).then_some(filters)
}

fn parse_amount(value: &str) -> Option<f32> {
    let amount = match value.strip_suffix('%') {
        Some(percent) => percent.trim().parse::<f32>().ok()? / 100.0,
        None => value.parse::<f32>().ok()?,
    };
    (amount.is_finite() && amount >= 0.0).then_some(amount)
}

// two or three lengths and an optional color on either side of them
fn parse_drop_shadow(value: &str) -> Option<Filter<Length>> {
    let mut lengths = Vec::new();
    let mut color = None;
    for word in split_words(value) {
        if let Some(length) = parse_length(word) {
            lengths.push(length);
        } else if color.is_none() {
            color = Some(parse_color(word)?);
        } else {
            return None;
        }
    }
    if !(2..=3).contains(&lengths.len()) {
        return None;
    }
    Some(Filter::DropShadow {
        offset: (lengths[0], lengths[1]),
        blur: lengths.get(2).copied().unwrap_or(Length::px(0.0)),
        color: color.or_else(|| parse_color("currentcolor"))?,
    })
}

// the matrices from the filter effects spec; amounts past one clamp where it says so
fn color_matrix(name: &str, amount: f32) -> Option<[f32; 20]> {
    let scale = |rgb: f32, alpha: f32, offset: f32| {
        [
            rgb, 0.0, 0.0, 0.0, offset, 0.0, rgb, 0.0, 0.0, offset, 0.0, 0.0, rgb, 0.0, offset,
            0.0, 0.0, 0.0, alpha, 0.0,
        ]
    };
    let mix = |coefficients: [f32; 9]| {
        let [a, b, c, d, e, f, g, h, i] = coefficients;
        [
            a, b, c, 0.0, 0.0, d, e, f, 0.0, 0.0, g, h, i, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0,
        ]
    };
    let rest = 1.0 - amount.min(1.0);
    Some(match name {
        "brightness" => scale(amount, 1.0, 0.0),
        "contrast" => scale(amount, 1.0, 0.5 - 0.5 * amount),
        "invert" => scale(1.0 - 2.0 * amount.min(1.0), 1.0, amount.min(1.0)),
        "opacity" => scale(1.0, amount.min(1.0), 0.0),
        "saturate" => mix([
            0.213 + 0.787 * amount,
            0.715 - 0.715 * amount,
            0.072 - 0.072 * amount,
            0.213 - 0.213 * amount,
            0.715 + 0.285 * amount,
            0.072 - 0.072 * amount,
            0.213 - 0.213 * amount,
            0.715 - 0.715 * amount,
            0.072 + 0.928 * amount,
        ]),
        "grayscale" => mix([
            0.2126 + 0.7874 * rest,
            0.7152 - 0.7152 * rest,
            0.0722 - 0.0722 * rest,
            0.2126 - 0.2126 * rest,
            0.7152 + 0.2848 * rest,
            0.0722 - 0.0722 * rest,
            0.2126 - 0.2126 * rest,
            0.7152 - 0.7152 * rest,
            0.0722 + 0.9278 * rest,
        ]),
        "sepia" => mix([
            0.393 + 0.607 * rest,
            0.769 - 0.769 * rest,
            0.189 - 0.189 * rest,
            0.349 - 0.349 * rest,
            0.686 + 0.314 * rest,
            0.168 - 0.168 * rest,
            0.272 - 0.272 * rest,
            0.534 - 0.534 * rest,
            0.131 + 0.869 * rest,
        ]),
        _ => return None,
    })
}

fn hue_rotate(angle: f32) -> [f32; 20] {
    let (sin, cos) = angle.sin_cos();
    let mix = [
        0.213 + cos * 0.787 - sin * 0.213,
        0.715 - cos * 0.715 - sin * 0.715,
        0.072 - cos * 0.072 + sin * 0.928,
        0.213 - cos * 0.213 + sin * 0.143,
        0.715 + cos * 0.285 + sin * 0.140,
        0.072 - cos * 0.072 - sin * 0.283,
        0.213 - cos * 0.213 - sin * 0.787,
        0.715 - cos * 0.715 + sin * 0.715,
        0.072 + cos * 0.928 + sin * 0.072,
    ];
    let mut matrix = [0.0; 20];
    for row in 0..3 {
        matrix[row * 5..row * 5 + 3].copy_from_slice(&mix[row * 3..row * 3 + 3]);
    }
    matrix[18] = 1.0;
    matrix
}

// how far past its content a list can draw, in css pixels: a blur spreads three standard
// deviations, a shadow its offset as well, and no blur spreads past where skia clamps it
pub(crate) fn reach(filters: &[Filter]) -> f32 {
    filters
        .iter()
        .map(|filter| match *filter {
            Filter::Blur(sigma) => 3.0 * sigma.min(MAX_SIGMA) + 2.0,
            Filter::DropShadow { offset, blur, .. } => {
                offset.0.abs().max(offset.1.abs()) + 3.0 * blur.min(MAX_SIGMA) + 2.0
            }
            Filter::Matrix(_) => 0.0,
        })
        .sum()
}

// runs the list over a premultiplied rgba surface whose pixels are `scale` to a css pixel; an
// element's content fades out past the surface's edges, while a backdrop reads past them as
// if mirrored there, as chromium tiles a backdrop before filtering it
pub(crate) fn apply(
    filters: &[Filter],
    pixels: &mut [u8],
    (width, height): (usize, usize),
    scale: f32,
    mirrored: bool,
) {
    let mut index = 0;
    while index < filters.len() {
        match filters[index] {
            Filter::Blur(sigma) => blur(pixels, (width, height), sigma * scale, mirrored),
            Filter::DropShadow {
                offset,
                blur: sigma,
                color,
            } => {
                let shift = (
                    (offset.0 * scale).round() as isize,
                    (offset.1 * scale).round() as isize,
                );
                drop_shadow(
                    pixels,
                    (width, height),
                    shift,
                    sigma * scale,
                    color,
                    mirrored,
                );
            }
            Filter::Matrix(_) => {
                // skia composes adjacent color filters, so no rounding happens between them
                let matrices = filters[index..]
                    .iter()
                    .map_while(|filter| match filter {
                        Filter::Matrix(matrix) => Some(matrix),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                index += matrices.len();
                apply_matrices(pixels, &matrices);
                continue;
            }
        }
        index += 1;
    }
}

fn apply_matrices(pixels: &mut [u8], matrices: &[&[f32; 20]]) {
    for pixel in pixels.as_chunks_mut::<4>().0 {
        if pixel[3] == 0 {
            continue;
        }
        let mut color = [0.0_f32; 4];
        for (channel, value) in color.iter_mut().zip(pixel.iter()) {
            *channel = f32::from(*value) * (1.0 / 255.0);
        }
        for matrix in matrices {
            let inverse = 1.0 / color[3];
            let inverse = if inverse.is_finite() { inverse } else { 0.0 };
            let source = [
                color[0] * inverse,
                color[1] * inverse,
                color[2] * inverse,
                color[3],
            ];
            let mut output = [0.0_f32; 4];
            for (row, channel) in output.iter_mut().enumerate() {
                let weights = &matrix[row * 5..row * 5 + 5];
                *channel = (weights[0] * source[0]
                    + weights[1] * source[1]
                    + weights[2] * source[2]
                    + weights[3] * source[3]
                    + weights[4])
                    .clamp(0.0, 1.0);
            }
            color = [
                output[0] * output[3],
                output[1] * output[3],
                output[2] * output[3],
                output[3],
            ];
        }
        for (value, channel) in pixel.iter_mut().zip(color) {
            *value = (channel * 255.0 + 0.5) as u8;
        }
    }
}

// skia's raster blur clamps here and rescales the image for anything larger
const MAX_SIGMA: f32 = 135.0;

fn blur(pixels: &mut [u8], (width, height): (usize, usize), sigma: f32, mirrored: bool) {
    let Some(mut pass) = ThreeBox::new(sigma.min(MAX_SIGMA)) else {
        return;
    };
    if !mirrored {
        pass.blur(pixels, (width, height));
        return;
    }
    let pad = (3.0 * sigma.min(MAX_SIGMA)).ceil() as usize;
    let size = (width + pad * 2, height + pad * 2);
    let mut padded = vec![0; size.0 * size.1 * 4];
    for (row, target) in padded.chunks_exact_mut(size.0 * 4).enumerate() {
        let source = mirror(row as isize - pad as isize, height) * width;
        for (column, target) in target.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let index = (source + mirror(column as isize - pad as isize, width)) * 4;
            target.copy_from_slice(&pixels[index..index + 4]);
        }
    }
    pass.blur(&mut padded, size);
    for (row, target) in pixels.chunks_exact_mut(width * 4).enumerate() {
        let start = ((row + pad) * size.0 + pad) * 4;
        target.copy_from_slice(&padded[start..start + width * 4]);
    }
}

// as skia's mirror tiling, which repeats the edge pixel: ... 1 0 | 0 1 ...
fn mirror(index: isize, length: usize) -> usize {
    let period = 2 * length as isize;
    let folded = index.rem_euclid(period);
    (if folded < length as isize {
        folded
    } else {
        period - 1 - folded
    }) as usize
}

// skia's drop shadow: the input blurred, tinted to the color through its alpha, moved, and
// drawn under the input
fn drop_shadow(
    pixels: &mut [u8],
    (width, height): (usize, usize),
    shift: (isize, isize),
    sigma: f32,
    color: Color,
    mirrored: bool,
) {
    let mut shadow = pixels.to_vec();
    blur(&mut shadow, (width, height), sigma, mirrored);
    let alpha = f32::from(color.alpha) / 255.0;
    let tint = [color.red, color.green, color.blue]
        .map(|channel| f32::from(channel) / 255.0 * alpha)
        .into_iter()
        .chain([alpha])
        .collect::<Vec<_>>();
    for (row, target) in pixels.chunks_exact_mut(width * 4).enumerate() {
        let source_row = row as isize - shift.1;
        if !(0..height as isize).contains(&source_row) {
            continue;
        }
        for (column, target) in target.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let source_column = column as isize - shift.0;
            if !(0..width as isize).contains(&source_column) || target[3] == 255 {
                continue;
            }
            let coverage =
                f32::from(shadow[(source_row as usize * width + source_column as usize) * 4 + 3])
                    / 255.0;
            if coverage == 0.0 {
                continue;
            }
            let inverse = 255 - u32::from(target[3]);
            for (value, tint) in target.iter_mut().zip(&tint) {
                let under = (tint * coverage * 255.0 + 0.5) as u32;
                *value = (u32::from(*value) + (under * inverse + 127) / 255).min(255) as u8;
            }
        }
    }
}

// skia's raster blur for 8-bit colors: three box passes run in one sweep stand in for the
// gaussian, with the sums kept whole between passes; chromium builds skia to take this path
// for small blurs too
struct ThreeBox {
    border: usize,
    factor: u64,
    half: u32,
    trailing: [Vec<[u32; 4]>; 3],
    line: Vec<[u8; 4]>,
    output: Vec<[u8; 4]>,
}

impl ThreeBox {
    fn new(sigma: f32) -> Option<Self> {
        let window = (sigma * 3.0 * (2.0 * std::f32::consts::PI).sqrt() / 4.0 + 0.5).floor();
        let window = window.max(1.0) as usize;
        if window <= 1 {
            return None;
        }
        let odd = window % 2 == 1;
        let divisor = (window * window * window + if odd { 0 } else { window * window }) as u32;
        Some(Self {
            border: if odd {
                3 * ((window - 1) / 2)
            } else {
                3 * (window / 2) - 1
            },
            factor: (f64::from(divisor).recip() * 4_294_967_296.0).round() as u64,
            half: divisor.div_ceil(2),
            trailing: [
                vec![[0; 4]; window - 1],
                vec![[0; 4]; window - 1],
                vec![[0; 4]; if odd { window - 1 } else { window }],
            ],
            line: Vec::new(),
            output: Vec::new(),
        })
    }

    // rows first, then columns, each read past its ends as transparent
    fn blur(&mut self, pixels: &mut [u8], (width, height): (usize, usize)) {
        for row in 0..height {
            self.pass(pixels, (row * width, 1), width);
        }
        for column in 0..width {
            self.pass(pixels, (column, width), height);
        }
    }

    fn pass(&mut self, pixels: &mut [u8], (start, stride): (usize, usize), length: usize) {
        let pixel = |index: usize| (start + index * stride) * 4;
        self.line.clear();
        self.line.extend((0..length).map(|index| {
            let at = pixel(index);
            [pixels[at], pixels[at + 1], pixels[at + 2], pixels[at + 3]]
        }));
        self.sweep();
        for (index, value) in self.output.iter().enumerate() {
            let at = pixel(index);
            pixels[at..at + 4].copy_from_slice(value);
        }
    }

    // each output pixel trails its leading edge by the border, so the line is fed that far
    // past its end
    fn sweep(&mut self) {
        let [first, second, third] = &mut self.trailing;
        for buffer in [&mut *first, &mut *second, &mut *third] {
            buffer.fill([0; 4]);
        }
        let mut cursors = [0_usize; 3];
        let mut sums = [[0_u32; 4], [0; 4], [self.half; 4]];
        self.output.clear();
        for step in 0..self.line.len() + self.border {
            let edge = self
                .line
                .get(step)
                .map_or([0; 4], |value| value.map(u32::from));
            let mut blurred = [0_u8; 4];
            for channel in 0..4 {
                sums[0][channel] += edge[channel];
                sums[1][channel] += sums[0][channel];
                sums[2][channel] += sums[1][channel];
                blurred[channel] = ((u64::from(sums[2][channel]) * self.factor) >> 32) as u8;
            }
            // each sum drops its trailing edge, which the next sum stored before dropping its own
            for (level, buffer) in [(2, &mut *third), (1, &mut *second), (0, &mut *first)] {
                let trailing = std::mem::replace(
                    &mut buffer[cursors[level]],
                    if level == 0 { edge } else { sums[level - 1] },
                );
                for channel in 0..4 {
                    sums[level][channel] -= trailing[channel];
                }
                cursors[level] = (cursors[level] + 1) % buffer.len();
            }
            if step >= self.border {
                self.output.push(blurred);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line_alpha(line: &[u8], sigma: f32, mirrored: bool) -> Vec<u8> {
        let mut pixels = line
            .iter()
            .flat_map(|&alpha| [0, 0, 0, alpha])
            .collect::<Vec<_>>();
        if mirrored {
            blur(&mut pixels, (line.len(), 1), sigma, true);
        } else {
            ThreeBox::new(sigma)
                .expect("the blur should not be an identity")
                .pass(&mut pixels, (0, 1), line.len());
        }
        pixels
            .as_chunks::<4>()
            .0
            .iter()
            .map(|pixel| pixel[3])
            .collect()
    }

    #[test]
    fn blurs_match_skia_three_box_passes_at_small_and_large_sigmas() {
        let mut step = vec![0_u8; 20];
        step.extend([255; 60]);
        step.extend([0; 20]);
        // edge's pixels at the left side of a 60px opaque box under blur(4px) and blur(1.5px)
        let wide = line_alpha(&step, 4.0, false);
        assert_eq!(
            wide[8..32],
            [
                0, 0, 2, 4, 9, 15, 25, 37, 53, 72, 93, 116, 139, 162, 183, 202, 218, 230, 240, 246,
                251, 253, 255, 255
            ]
        );
        let narrow = line_alpha(&step, 1.5, false);
        assert_eq!(
            narrow[14..26],
            [0, 0, 0, 9, 38, 94, 161, 217, 246, 255, 255, 255]
        );
        assert_eq!(wide[50], 255, "the middle of the box stays opaque");
        assert!(
            ThreeBox::new(0.2).is_none(),
            "a one pixel window is no blur"
        );
    }

    #[test]
    fn backdrops_blur_as_if_mirrored_past_their_edges() {
        // 10px black and white stripes, the backdrop starting at a black one, as edge blurs
        // them under backdrop-filter: blur(6px)
        let stripes = (0..120)
            .map(|x| if x % 20 < 10 { 0 } else { 255 })
            .collect::<Vec<_>>();
        let blurred = line_alpha(&stripes, 6.0, true);
        assert_eq!(
            blurred[..20],
            [
                17, 20, 25, 32, 42, 55, 68, 83, 98, 112, 125, 137, 147, 154, 158, 158, 155, 149,
                141, 132
            ]
        );
        assert_eq!(
            blurred[104..],
            [
                97, 97, 101, 108, 118, 130, 143, 157, 172, 187, 200, 213, 223, 230, 235, 238
            ]
        );
    }

    #[test]
    fn minified_lists_parse_function_by_function() {
        let filters = parse("blur(18px)brightness(.5)").expect("the list should parse");
        assert_eq!(filters.len(), 2);
        assert_eq!(filters[0], Filter::Blur(Length::px(18.0)));
        assert_eq!(
            filters[1],
            Filter::Matrix(color_matrix("brightness", 0.5).unwrap())
        );
        let Some(
            [
                Filter::DropShadow {
                    offset,
                    blur,
                    color,
                },
            ],
        ) = parse("drop-shadow(0 2px 8px #000000b3)")
            .as_deref()
            .map(|list| [list[0]])
        else {
            panic!("drop-shadow should parse");
        };
        assert_eq!(offset, (Length::px(0.0), Length::px(2.0)));
        assert_eq!(blur, Length::px(8.0));
        assert_eq!(color, Color::rgba(0, 0, 0, 0xb3));
        assert!(parse("brightness(-1)").is_none());
        assert!(parse("blur(2px) url(#glow)").is_none());
        assert!(parse("drop-shadow(1px)").is_none());
        assert_eq!(
            parse("grayscale()"),
            Some(vec![Filter::Matrix(
                color_matrix("grayscale", 1.0).unwrap()
            )])
        );
    }

    #[test]
    fn adjacent_color_functions_round_once() {
        // brightness(.5) twice in one run keeps the half of a half that a rounded step loses
        let mut pixels = [101, 0, 0, 255];
        let half = color_matrix("brightness", 0.5).unwrap();
        apply_matrices(&mut pixels, &[&half, &half]);
        assert_eq!(pixels, [25, 0, 0, 255]);
        let mut translucent = [100, 50, 0, 128];
        apply_matrices(&mut translucent, &[&color_matrix("invert", 1.0).unwrap()]);
        assert_eq!(translucent, [28, 78, 128, 128]);
    }
}
