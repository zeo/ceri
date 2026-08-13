use crate::css::{Color, Overflow};
use crate::dom::NodePath;
use crate::image::Image;
use crate::layout::{ImageLayer, LayoutBox, LayoutDocument, Rect, TextRun};
use std::fs::File;
use std::io::{self, Write};
use std::path::Path;

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum CanvasCommand {
    FillRect {
        path: NodePath,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        color: Color,
    },
    ClearRect {
        path: NodePath,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
    },
}

#[derive(Clone, Debug)]
pub struct Canvas {
    width: u32,
    height: u32,
    background: Color,
    pixels: Vec<u8>,
}

impl Canvas {
    pub fn new(width: u32, height: u32, background: Color) -> Self {
        let mut pixels = vec![0; width as usize * height as usize * 4];
        for pixel in pixels.chunks_exact_mut(4) {
            pixel.copy_from_slice(&[background.red, background.green, background.blue, 255]);
        }
        Self {
            width,
            height,
            background,
            pixels,
        }
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn rgba_pixels(&self) -> &[u8] {
        &self.pixels
    }

    pub fn bytes_per_row(&self) -> usize {
        self.width as usize * 4
    }

    pub fn non_background_pixels(&self) -> usize {
        let background = [
            self.background.red,
            self.background.green,
            self.background.blue,
        ];
        self.pixels
            .chunks_exact(4)
            .filter(|pixel| pixel[..3] != background)
            .count()
    }

    pub fn write_ppm(&self, path: &Path) -> io::Result<()> {
        let mut file = File::create(path)?;
        write!(file, "P6\n{} {}\n255\n", self.width, self.height)?;
        for pixel in self.pixels.chunks_exact(4) {
            file.write_all(&pixel[..3])?;
        }
        Ok(())
    }

    pub fn write_bmp(&self, path: &Path) -> io::Result<()> {
        let row_stride = (self.width as usize * 3 + 3) & !3;
        let pixel_bytes = row_stride * self.height as usize;
        let file_size = 14 + 40 + pixel_bytes;
        let mut file = File::create(path)?;
        file.write_all(b"BM")?;
        file.write_all(&(file_size as u32).to_le_bytes())?;
        file.write_all(&[0; 4])?;
        file.write_all(&(54_u32).to_le_bytes())?;
        file.write_all(&(40_u32).to_le_bytes())?;
        file.write_all(&(self.width as i32).to_le_bytes())?;
        file.write_all(&(-(self.height as i32)).to_le_bytes())?;
        file.write_all(&(1_u16).to_le_bytes())?;
        file.write_all(&(24_u16).to_le_bytes())?;
        file.write_all(&[0; 4])?;
        file.write_all(&(pixel_bytes as u32).to_le_bytes())?;
        file.write_all(&[0; 16])?;
        let padding = vec![0; row_stride - self.width as usize * 3];
        for row in 0..self.height {
            let start = row as usize * self.width as usize * 4;
            for pixel in self.pixels[start..start + self.width as usize * 4].chunks_exact(4) {
                file.write_all(&[pixel[2], pixel[1], pixel[0]])?;
            }
            file.write_all(&padding)?;
        }
        Ok(())
    }

    fn fill_rect(&mut self, x: f32, y: f32, width: f32, height: f32, color: Color) {
        if width <= 0.0 || height <= 0.0 || color.alpha == 0 {
            return;
        }
        let left = x.floor().max(0.0) as u32;
        let top = y.floor().max(0.0) as u32;
        let right = (x + width).ceil().max(0.0).min(self.width as f32) as u32;
        let bottom = (y + height).ceil().max(0.0).min(self.height as f32) as u32;
        for pixel_y in top.min(self.height)..bottom.min(self.height) {
            for pixel_x in left.min(self.width)..right.min(self.width) {
                self.blend_pixel(pixel_x, pixel_y, color);
            }
        }
    }

    fn fill_rect_clipped(
        &mut self,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        color: Color,
        clip: crate::layout::Rect,
    ) {
        let left = x.max(clip.x);
        let top = y.max(clip.y);
        let right = (x + width).min(clip.x + clip.width);
        let bottom = (y + height).min(clip.y + clip.height);
        self.fill_rect(left, top, right - left, bottom - top, color);
    }

    fn blend_pixel(&mut self, x: u32, y: u32, color: Color) {
        let index = ((y * self.width + x) * 4) as usize;
        let alpha = color.alpha as u32;
        let inverse = 255 - alpha;
        self.pixels[index] =
            ((color.red as u32 * alpha + self.pixels[index] as u32 * inverse) / 255) as u8;
        self.pixels[index + 1] =
            ((color.green as u32 * alpha + self.pixels[index + 1] as u32 * inverse) / 255) as u8;
        self.pixels[index + 2] =
            ((color.blue as u32 * alpha + self.pixels[index + 2] as u32 * inverse) / 255) as u8;
        self.pixels[index + 3] = 255;
    }
}

pub(crate) fn paint(document: &LayoutDocument) -> Canvas {
    paint_with_commands(document, &[])
}

pub(crate) fn paint_with_commands(
    document: &LayoutDocument,
    commands: &[CanvasCommand],
) -> Canvas {
    let mut canvas = Canvas::new(
        document.viewport.width.max(1.0) as u32,
        document.viewport.height.max(1.0) as u32,
        Color::opaque(245, 248, 250),
    );
    paint_box(
        &mut canvas,
        &document.root,
        commands,
        &document.images,
        None,
    );
    canvas
}

fn intersect_clip(parent: Option<Rect>, rect: Rect) -> Option<Rect> {
    if rect.width <= 0.0 || rect.height <= 0.0 {
        return None;
    }
    let Some(parent) = parent else {
        return Some(rect);
    };
    let left = parent.x.max(rect.x);
    let top = parent.y.max(rect.y);
    let right = (parent.x + parent.width).min(rect.x + rect.width);
    let bottom = (parent.y + parent.height).min(rect.y + rect.height);
    (right > left && bottom > top).then_some(Rect {
        x: left,
        y: top,
        width: right - left,
        height: bottom - top,
    })
}

fn fill_rect_optional(
    canvas: &mut Canvas,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    color: Color,
    clip: Option<Rect>,
) {
    if let Some(clip) = clip {
        canvas.fill_rect_clipped(x, y, width, height, color, clip);
    } else {
        canvas.fill_rect(x, y, width, height, color);
    }
}

fn paint_canvas_command(
    canvas: &mut Canvas,
    canvas_box: &LayoutBox,
    command: &CanvasCommand,
    clip: Option<Rect>,
) {
    let (path, x, y, width, height) = match command {
        CanvasCommand::FillRect {
            path,
            x,
            y,
            width,
            height,
            ..
        }
        | CanvasCommand::ClearRect {
            path,
            x,
            y,
            width,
            height,
        } => (path, *x, *y, *width, *height),
    };
    if path != &canvas_box.path {
        return;
    }
    let color = match command {
        CanvasCommand::FillRect { color, .. } => *color,
        CanvasCommand::ClearRect { .. } => {
            if canvas_box.background.alpha == 0 {
                canvas.background
            } else {
                canvas_box.background
            }
        }
    };
    let Some(command_clip) = intersect_clip(clip, canvas_box.rect) else {
        return;
    };
    canvas.fill_rect_clipped(
        canvas_box.rect.x + x,
        canvas_box.rect.y + y,
        width,
        height,
        color,
        command_clip,
    );
}

fn paint_box(
    canvas: &mut Canvas,
    layout_box: &LayoutBox,
    commands: &[CanvasCommand],
    images: &[ImageLayer],
    clip: Option<Rect>,
) {
    fill_rect_optional(
        canvas,
        layout_box.rect.x,
        layout_box.rect.y,
        layout_box.rect.width,
        layout_box.rect.height,
        layout_box.background,
        clip,
    );
    let content_clip = if layout_box.overflow == Overflow::Hidden {
        intersect_clip(clip, layout_box.rect)
    } else {
        clip
    };
    if layout_box.tag == "img"
        && let Some(layer) = images.iter().find(|layer| layer.path == layout_box.path)
    {
        let inset = layout_box.border_width.max(0.0);
        paint_image(
            canvas,
            &layer.image,
            Rect {
                x: layout_box.rect.x + inset,
                y: layout_box.rect.y + inset,
                width: (layout_box.rect.width - inset * 2.0).max(0.0),
                height: (layout_box.rect.height - inset * 2.0).max(0.0),
            },
            content_clip,
        );
    }
    if layout_box.border_width > 0.0 {
        let border = layout_box.border_width;
        fill_rect_optional(
            canvas,
            layout_box.rect.x,
            layout_box.rect.y,
            layout_box.rect.width,
            border,
            layout_box.border_color,
            clip,
        );
        fill_rect_optional(
            canvas,
            layout_box.rect.x,
            layout_box.rect.bottom() - border,
            layout_box.rect.width,
            border,
            layout_box.border_color,
            clip,
        );
        fill_rect_optional(
            canvas,
            layout_box.rect.x,
            layout_box.rect.y,
            border,
            layout_box.rect.height,
            layout_box.border_color,
            clip,
        );
        fill_rect_optional(
            canvas,
            layout_box.rect.x + layout_box.rect.width - border,
            layout_box.rect.y,
            border,
            layout_box.rect.height,
            layout_box.border_color,
            clip,
        );
    }
    for run in &layout_box.text {
        paint_text(canvas, run, content_clip);
    }
    for child in &layout_box.children {
        paint_box(canvas, child, commands, images, content_clip);
    }
    if layout_box.tag == "canvas" {
        for command in commands {
            paint_canvas_command(canvas, layout_box, command, content_clip);
        }
    }
}

fn paint_image(canvas: &mut Canvas, image: &Image, rect: Rect, clip: Option<Rect>) {
    if rect.width <= 0.0 || rect.height <= 0.0 {
        return;
    }
    let Some(draw_rect) = intersect_clip(clip, rect) else {
        return;
    };
    let left = draw_rect.x.floor().max(0.0) as u32;
    let top = draw_rect.y.floor().max(0.0) as u32;
    let right = (draw_rect.x + draw_rect.width)
        .ceil()
        .max(0.0)
        .min(canvas.width as f32) as u32;
    let bottom = (draw_rect.y + draw_rect.height)
        .ceil()
        .max(0.0)
        .min(canvas.height as f32) as u32;
    for pixel_y in top.min(canvas.height)..bottom.min(canvas.height) {
        let source_y = (((pixel_y as f32 - rect.y) / rect.height) * image.height as f32)
            .floor()
            .clamp(0.0, image.height.saturating_sub(1) as f32) as u32;
        for pixel_x in left.min(canvas.width)..right.min(canvas.width) {
            let source_x = (((pixel_x as f32 - rect.x) / rect.width) * image.width as f32)
                .floor()
                .clamp(0.0, image.width.saturating_sub(1) as f32) as u32;
            canvas.blend_pixel(pixel_x, pixel_y, image.pixel(source_x, source_y));
        }
    }
}

fn paint_text(canvas: &mut Canvas, run: &TextRun, clip: Option<Rect>) {
    let scale = (run.font_size / 9.0).round().max(1.0) as u32;
    let mut x = run.x;
    for character in run.text.chars() {
        let glyph = glyph_rows(character);
        for (row, bits) in glyph.iter().enumerate() {
            for column in 0..5 {
                if bits & (1 << (4 - column)) != 0 {
                    let width = if run.bold { scale + 1 } else { scale };
                    fill_rect_optional(
                        canvas,
                        x + column as f32 * scale as f32,
                        run.y + row as f32 * scale as f32,
                        width as f32,
                        scale as f32,
                        run.color,
                        clip,
                    );
                }
            }
        }
        x += scale as f32 * 6.0;
    }
}

fn glyph_rows(character: char) -> [u8; 7] {
    match character.to_ascii_uppercase() {
        'A' => [
            0b01110, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001,
        ],
        'B' => [
            0b11110, 0b10001, 0b10001, 0b11110, 0b10001, 0b10001, 0b11110,
        ],
        'C' => [
            0b01111, 0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b01111,
        ],
        'D' => [
            0b11110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b11110,
        ],
        'E' => [
            0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b11111,
        ],
        'F' => [
            0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b10000,
        ],
        'G' => [
            0b01111, 0b10000, 0b10000, 0b10111, 0b10001, 0b10001, 0b01111,
        ],
        'H' => [
            0b10001, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001,
        ],
        'I' => [
            0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b11111,
        ],
        'J' => [
            0b00111, 0b00010, 0b00010, 0b00010, 0b00010, 0b10010, 0b01100,
        ],
        'K' => [
            0b10001, 0b10010, 0b10100, 0b11000, 0b10100, 0b10010, 0b10001,
        ],
        'L' => [
            0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b11111,
        ],
        'M' => [
            0b10001, 0b11011, 0b10101, 0b10101, 0b10001, 0b10001, 0b10001,
        ],
        'N' => [
            0b10001, 0b11001, 0b10101, 0b10011, 0b10001, 0b10001, 0b10001,
        ],
        'O' => [
            0b01110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110,
        ],
        'P' => [
            0b11110, 0b10001, 0b10001, 0b11110, 0b10000, 0b10000, 0b10000,
        ],
        'Q' => [
            0b01110, 0b10001, 0b10001, 0b10001, 0b10101, 0b10010, 0b01101,
        ],
        'R' => [
            0b11110, 0b10001, 0b10001, 0b11110, 0b10100, 0b10010, 0b10001,
        ],
        'S' => [
            0b01111, 0b10000, 0b10000, 0b01110, 0b00001, 0b00001, 0b11110,
        ],
        'T' => [
            0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100,
        ],
        'U' => [
            0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110,
        ],
        'V' => [
            0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01010, 0b00100,
        ],
        'W' => [
            0b10001, 0b10001, 0b10001, 0b10101, 0b10101, 0b11011, 0b10001,
        ],
        'X' => [
            0b10001, 0b10001, 0b01010, 0b00100, 0b01010, 0b10001, 0b10001,
        ],
        'Y' => [
            0b10001, 0b10001, 0b01010, 0b00100, 0b00100, 0b00100, 0b00100,
        ],
        'Z' => [
            0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b10000, 0b11111,
        ],
        '0' => [
            0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110,
        ],
        '1' => [
            0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110,
        ],
        '2' => [
            0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111,
        ],
        '3' => [
            0b11110, 0b00001, 0b00001, 0b01110, 0b00001, 0b00001, 0b11110,
        ],
        '4' => [
            0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010,
        ],
        '5' => [
            0b11111, 0b10000, 0b10000, 0b11110, 0b00001, 0b00001, 0b11110,
        ],
        '6' => [
            0b01110, 0b10000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110,
        ],
        '7' => [
            0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000,
        ],
        '8' => [
            0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110,
        ],
        '9' => [
            0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00001, 0b01110,
        ],
        '-' => [0, 0, 0, 0b11111, 0, 0, 0],
        '/' => [0b00001, 0b00010, 0b00100, 0b01000, 0b10000, 0, 0],
        '.' => [0, 0, 0, 0, 0, 0b00110, 0b00110],
        ':' => [0, 0b00110, 0b00110, 0, 0b00110, 0b00110, 0],
        '!' => [0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0, 0b00100],
        '?' => [0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0, 0b00100],
        '(' => [
            0b00010, 0b00100, 0b01000, 0b01000, 0b01000, 0b00100, 0b00010,
        ],
        ')' => [
            0b01000, 0b00100, 0b00010, 0b00010, 0b00010, 0b00100, 0b01000,
        ],
        _ => [0, 0, 0, 0, 0, 0, 0],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{LayoutBox, LayoutDocument, Rect, Size};

    #[test]
    fn painting_changes_pixels_and_writes_a_ppm_header() {
        let document = LayoutDocument {
            viewport: Size {
                width: 32.0,
                height: 24.0,
            },
            root: LayoutBox {
                path: Vec::new(),
                tag: "root".to_string(),
                rect: Rect {
                    x: 2.0,
                    y: 3.0,
                    width: 20.0,
                    height: 10.0,
                },
                background: Color::opaque(10, 20, 30),
                border_width: 0.0,
                border_color: Color::transparent(),
                text: vec![TextRun {
                    text: "A".to_string(),
                    x: 4.0,
                    y: 5.0,
                    width: 8.0,
                    font_size: 12.0,
                    color: Color::WHITE,
                    bold: false,
                }],
                children: Vec::new(),
                overflow: Overflow::Visible,
                margin: Default::default(),
            },
            images: Vec::new(),
        };
        let canvas = paint(&document);
        assert!(canvas.non_background_pixels() > 0);

        let path = std::env::temp_dir().join(format!("ceri-engine-lab-{}.bmp", std::process::id()));
        canvas.write_bmp(&path).expect("bmp should write");
        let bytes = std::fs::read(&path).expect("bmp should be readable");
        assert_eq!(&bytes[..2], b"BM");
        std::fs::remove_file(path).expect("test bmp should be removable");
    }

    #[test]
    fn canvas_commands_stay_in_document_paint_order() {
        let document = LayoutDocument {
            viewport: Size {
                width: 20.0,
                height: 20.0,
            },
            root: LayoutBox {
                path: Vec::new(),
                tag: "root".to_string(),
                rect: Rect {
                    x: 0.0,
                    y: 0.0,
                    width: 20.0,
                    height: 20.0,
                },
                background: Color::opaque(245, 248, 250),
                border_width: 0.0,
                border_color: Color::transparent(),
                text: Vec::new(),
                children: vec![
                    LayoutBox {
                        path: vec![0],
                        tag: "canvas".to_string(),
                        rect: Rect {
                            x: 0.0,
                            y: 0.0,
                            width: 10.0,
                            height: 10.0,
                        },
                        background: Color::transparent(),
                        border_width: 0.0,
                        border_color: Color::transparent(),
                        text: Vec::new(),
                        children: Vec::new(),
                        overflow: Overflow::Visible,
                        margin: Default::default(),
                    },
                    LayoutBox {
                        path: vec![1],
                        tag: "div".to_string(),
                        rect: Rect {
                            x: 0.0,
                            y: 0.0,
                            width: 10.0,
                            height: 10.0,
                        },
                        background: Color::opaque(0, 0, 255),
                        border_width: 0.0,
                        border_color: Color::transparent(),
                        text: Vec::new(),
                        children: Vec::new(),
                        overflow: Overflow::Visible,
                        margin: Default::default(),
                    },
                ],
                overflow: Overflow::Visible,
                margin: Default::default(),
            },
            images: Vec::new(),
        };
        let commands = [CanvasCommand::FillRect {
            path: vec![0],
            x: 0.0,
            y: 0.0,
            width: 10.0,
            height: 10.0,
            color: Color::opaque(255, 0, 0),
        }];

        let canvas = paint_with_commands(&document, &commands);
        assert_eq!(
            &canvas.rgba_pixels()[((5 * 20 + 5) * 4) as usize..][..4],
            [0, 0, 255, 255]
        );
    }
}
