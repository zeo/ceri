use crate::css::{Color, Overflow};
use crate::dom::NodePath;
use crate::image::Image;
use crate::layout::{
    ImageLayer, LayoutBox, LayoutDocument, Rect, TextRun, paint_order, scrollbar_metrics,
    stacking_context_entries, translate_rect, visual_offset_for, visual_rect_for,
};
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
        (0.0, 0.0),
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
    rect: Rect,
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
    let Some(command_clip) = intersect_clip(clip, rect) else {
        return;
    };
    canvas.fill_rect_clipped(
        rect.x + x,
        rect.y + y,
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
    ancestor_offset: (f32, f32),
) {
    paint_context(
        canvas,
        layout_box,
        commands,
        images,
        clip,
        ancestor_offset,
    );
}

fn paint_context(
    canvas: &mut Canvas,
    layout_box: &LayoutBox,
    commands: &[CanvasCommand],
    images: &[ImageLayer],
    clip: Option<Rect>,
    ancestor_offset: (f32, f32),
) {
    let offset = visual_offset_for(layout_box, ancestor_offset);
    let rect = visual_rect_for(layout_box, ancestor_offset);
    let content_clip = paint_box_surface(canvas, layout_box, images, clip, offset, rect);
    let mut contexts = stacking_context_entries(layout_box, clip, ancestor_offset);
    contexts.sort_by_key(|entry| (entry.z_index, entry.order));
    for entry in contexts.iter().filter(|entry| entry.z_index < 0) {
        paint_context(
            canvas,
            entry.layout,
            commands,
            images,
            entry.clip,
            entry.ancestor_offset,
        );
    }
    paint_box_text(canvas, layout_box, content_clip, offset);
    paint_non_context_children(canvas, layout_box, commands, images, content_clip, offset);
    paint_canvas_commands(canvas, layout_box, commands, content_clip, rect);
    for entry in contexts.iter().filter(|entry| entry.z_index >= 0) {
        paint_context(
            canvas,
            entry.layout,
            commands,
            images,
            entry.clip,
            entry.ancestor_offset,
        );
    }
    paint_scrollbars(canvas, layout_box, offset, clip);
}

fn paint_non_context_children(
    canvas: &mut Canvas,
    layout_box: &LayoutBox,
    commands: &[CanvasCommand],
    images: &[ImageLayer],
    clip: Option<Rect>,
    ancestor_offset: (f32, f32),
) {
    for index in paint_order(&layout_box.children) {
        let child = &layout_box.children[index];
        if child.creates_stacking_context {
            continue;
        }
        paint_non_context_subtree(canvas, child, commands, images, clip, ancestor_offset);
    }
}

fn paint_non_context_subtree(
    canvas: &mut Canvas,
    layout_box: &LayoutBox,
    commands: &[CanvasCommand],
    images: &[ImageLayer],
    clip: Option<Rect>,
    ancestor_offset: (f32, f32),
) {
    let offset = visual_offset_for(layout_box, ancestor_offset);
    let rect = visual_rect_for(layout_box, ancestor_offset);
    let content_clip = paint_box_surface(canvas, layout_box, images, clip, offset, rect);
    paint_box_text(canvas, layout_box, content_clip, offset);
    paint_non_context_children(canvas, layout_box, commands, images, content_clip, offset);
    paint_canvas_commands(canvas, layout_box, commands, content_clip, rect);
    paint_scrollbars(canvas, layout_box, offset, clip);
}

fn paint_box_surface(
    canvas: &mut Canvas,
    layout_box: &LayoutBox,
    images: &[ImageLayer],
    clip: Option<Rect>,
    offset: (f32, f32),
    rect: Rect,
) -> Option<Rect> {
    fill_rect_optional(
        canvas,
        rect.x,
        rect.y,
        rect.width,
        rect.height,
        layout_box.background,
        clip,
    );
    let content_clip = if matches!(layout_box.overflow, Overflow::Hidden | Overflow::Auto)
        || layout_box.path.is_empty()
    {
        intersect_clip(
            clip,
            translate_rect(scrollbar_metrics(layout_box).scrollport, offset),
        )
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
                x: rect.x + inset,
                y: rect.y + inset,
                width: (rect.width - inset * 2.0).max(0.0),
                height: (rect.height - inset * 2.0).max(0.0),
            },
            content_clip,
        );
    }
    if layout_box.border_width > 0.0 {
        let border = layout_box.border_width;
        fill_rect_optional(
            canvas,
            rect.x,
            rect.y,
            rect.width,
            border,
            layout_box.border_color,
            clip,
        );
        fill_rect_optional(
            canvas,
            rect.x,
            rect.bottom() - border,
            rect.width,
            border,
            layout_box.border_color,
            clip,
        );
        fill_rect_optional(
            canvas,
            rect.x,
            rect.y,
            border,
            rect.height,
            layout_box.border_color,
            clip,
        );
        fill_rect_optional(
            canvas,
            rect.x + rect.width - border,
            rect.y,
            border,
            rect.height,
            layout_box.border_color,
            clip,
        );
    }
    content_clip
}

fn paint_box_text(
    canvas: &mut Canvas,
    layout_box: &LayoutBox,
    clip: Option<Rect>,
    offset: (f32, f32),
) {
    paint_control_selection(canvas, layout_box, clip, offset);
    for run in &layout_box.text {
        paint_text(canvas, run, clip, offset);
    }
    paint_control_caret(canvas, layout_box, clip, offset);
}

fn paint_control_selection(
    canvas: &mut Canvas,
    layout_box: &LayoutBox,
    clip: Option<Rect>,
    offset: (f32, f32),
) {
    let Some(value) = &layout_box.control_value else {
        return;
    };
    let Some((start, end)) = layout_box.control_selection else {
        return;
    };
    if layout_box.control_text_origin.is_none() || start == end {
        return;
    }
    for (line_index, (line_start, line_end)) in
        control_line_ranges(value, &layout_box.text).iter().enumerate()
    {
        let Some(run) = layout_box.text.get(line_index) else {
            break;
        };
        let selected_start = start.max(*line_start);
        let selected_end = end.min(*line_end);
        if selected_start >= selected_end {
            continue;
        }
        let selected_start_char = utf16_char_offset(&run.text, selected_start - line_start);
        let selected_end_char = utf16_char_offset(&run.text, selected_end - line_start);
        let advance = text_advance(run.font_size);
        fill_rect_optional(
            canvas,
            run.x + offset.0 + selected_start_char as f32 * advance,
            run.y + offset.1,
            (selected_end_char - selected_start_char) as f32 * advance,
            run.font_size.max(1.0),
            Color::opaque(170, 205, 238),
            clip,
        );
    }
}

fn paint_control_caret(
    canvas: &mut Canvas,
    layout_box: &LayoutBox,
    clip: Option<Rect>,
    offset: (f32, f32),
) {
    if !layout_box.control_focused {
        return;
    }
    let Some(value) = &layout_box.control_value else {
        return;
    };
    if layout_box.control_text_origin.is_none() {
        return;
    }
    let caret = layout_box
        .control_selection
        .map(|(_, end)| end)
        .unwrap_or_else(|| value.encode_utf16().count());
    let Some((x, y)) = control_text_position(layout_box, value, caret) else {
        return;
    };
    fill_rect_optional(
        canvas,
        x + offset.0,
        y + offset.1,
        1.0,
        layout_box.font_size.max(1.0),
        Color::BLACK,
        clip,
    );
}

fn control_line_ranges(value: &str, lines: &[TextRun]) -> Vec<(usize, usize)> {
    let units = value.encode_utf16().collect::<Vec<_>>();
    let mut start = 0;
    let mut ranges = Vec::new();
    for line in lines {
        let end = (start + line.text.encode_utf16().count()).min(units.len());
        ranges.push((start, end));
        start = end;
        if units.get(start) == Some(&(b'\n' as u16)) {
            start += 1;
        }
    }
    ranges
}

fn control_text_position(
    layout_box: &LayoutBox,
    value: &str,
    offset: usize,
) -> Option<(f32, f32)> {
    let ranges = control_line_ranges(value, &layout_box.text);
    let mut line_index = ranges.len().saturating_sub(1);
    for (index, (_, line_end)) in ranges.iter().enumerate() {
        if offset <= *line_end || index + 1 == ranges.len() {
            line_index = index;
            break;
        }
    }
    let (line_start, line_end) = ranges.get(line_index).copied()?;
    let run = layout_box.text.get(line_index)?;
    let line_offset = offset.saturating_sub(line_start).min(line_end - line_start);
    let character_offset = utf16_char_offset(&run.text, line_offset);
    Some((
        run.x + character_offset as f32 * text_advance(run.font_size),
        run.y,
    ))
}

fn text_advance(font_size: f32) -> f32 {
    (font_size / 9.0).round().max(1.0) * 6.0
}

fn utf16_char_offset(value: &str, offset: usize) -> usize {
    let mut units = 0;
    let mut characters = 0;
    for character in value.chars() {
        let length = character.len_utf16();
        if units + length > offset {
            break;
        }
        units += length;
        characters += 1;
    }
    characters
}

fn paint_canvas_commands(
    canvas: &mut Canvas,
    layout_box: &LayoutBox,
    commands: &[CanvasCommand],
    clip: Option<Rect>,
    rect: Rect,
) {
    if layout_box.tag != "canvas" {
        return;
    }
    for command in commands {
        paint_canvas_command(canvas, layout_box, command, clip, rect);
    }
}

fn paint_scrollbars(
    canvas: &mut Canvas,
    layout_box: &LayoutBox,
    offset: (f32, f32),
    clip: Option<Rect>,
) {
    let metrics = scrollbar_metrics(layout_box);
    let track_color = Color::opaque(232, 236, 240);
    let thumb_color = Color::opaque(126, 138, 150);
    if let Some(track) = metrics.vertical_track {
        let track = translate_rect(track, offset);
        fill_rect_optional(
            canvas,
            track.x,
            track.y,
            track.width,
            track.height,
            track_color,
            clip,
        );
        let thumb = scrollbar_thumb(
            track,
            metrics.scrollport.height,
            layout_box.scroll_content.height,
            layout_box.scroll_offset.1,
            (layout_box.scroll_content.height - metrics.scrollport.height).max(0.0),
            true,
        );
        fill_rect_optional(canvas, thumb.x, thumb.y, thumb.width, thumb.height, thumb_color, clip);
    }
    if let Some(track) = metrics.horizontal_track {
        let track = translate_rect(track, offset);
        fill_rect_optional(
            canvas,
            track.x,
            track.y,
            track.width,
            track.height,
            track_color,
            clip,
        );
        let thumb = scrollbar_thumb(
            track,
            metrics.scrollport.width,
            layout_box.scroll_content.width,
            layout_box.scroll_offset.0,
            (layout_box.scroll_content.width - metrics.scrollport.width).max(0.0),
            false,
        );
        fill_rect_optional(canvas, thumb.x, thumb.y, thumb.width, thumb.height, thumb_color, clip);
    }
    if let (Some(vertical), Some(horizontal)) = (metrics.vertical_track, metrics.horizontal_track) {
        let corner = translate_rect(
            Rect {
                x: vertical.x,
                y: horizontal.y,
                width: vertical.width,
                height: horizontal.height,
            },
            offset,
        );
        fill_rect_optional(
            canvas,
            corner.x,
            corner.y,
            corner.width,
            corner.height,
            track_color,
            clip,
        );
    }
}

fn scrollbar_thumb(
    track: Rect,
    viewport_extent: f32,
    content_extent: f32,
    offset: f32,
    max_offset: f32,
    vertical: bool,
) -> Rect {
    let track_extent = if vertical { track.height } else { track.width };
    let minimum = 8.0_f32.min(track_extent);
    let thumb_extent = if content_extent > 0.0 {
        (track_extent * (viewport_extent / content_extent).clamp(0.0, 1.0))
            .max(minimum)
            .min(track_extent)
    } else {
        track_extent
    };
    let travel = (track_extent - thumb_extent).max(0.0);
    let progress = if max_offset > 0.0 {
        (offset / max_offset).clamp(0.0, 1.0)
    } else {
        0.0
    };
    if vertical {
        Rect {
            x: track.x,
            y: track.y + travel * progress,
            width: track.width,
            height: thumb_extent,
        }
    } else {
        Rect {
            x: track.x + travel * progress,
            y: track.y,
            width: thumb_extent,
            height: track.height,
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

fn paint_text(canvas: &mut Canvas, run: &TextRun, clip: Option<Rect>, offset: (f32, f32)) {
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
                        x + column as f32 * scale as f32 + offset.0,
                        run.y + row as f32 * scale as f32 + offset.1,
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
                control_value: None,
                control_selection: None,
                control_focused: false,
                control_text_origin: None,
                children: Vec::new(),
                position: crate::css::Position::Static,
                visual_offset: (0.0, 0.0),
                z_index: 0,
                creates_stacking_context: true,
                sticky_inset: crate::css::Edges::auto(),
                font_size: 16.0,
                overflow: Overflow::Visible,
                scroll_content: Size {
                    width: 20.0,
                    height: 10.0,
                },
                scroll_offset: (0.0, 0.0),
                margin: Default::default(),
            },
            images: Vec::new(),
        };
        let canvas = paint(&document);
        assert!(canvas.non_background_pixels() > 0);

        let mut focused = document.clone();
        focused.root.control_value = Some("A".to_string());
        focused.root.control_selection = Some((0, 1));
        focused.root.control_focused = true;
        focused.root.control_text_origin = Some((4.0, 5.0));
        let focused_canvas = paint(&focused);
        assert_ne!(
            focused_canvas.rgba_pixels(),
            canvas.rgba_pixels(),
            "focused controls should add a software caret"
        );
        let caret_x = (4.0 + text_advance(12.0)) as u32;
        let caret_index = ((5 * focused_canvas.width() + caret_x) * 4) as usize;
        assert_eq!(&focused_canvas.rgba_pixels()[caret_index..caret_index + 3], &[0, 0, 0]);
        let selection_index = ((5 * focused_canvas.width() + 4) * 4) as usize;
        assert_eq!(
            &focused_canvas.rgba_pixels()[selection_index..selection_index + 3],
            &[170, 205, 238]
        );

        let path = std::env::temp_dir().join(format!("ceri-engine-lab-{}.bmp", std::process::id()));
        canvas.write_bmp(&path).expect("bmp should write");
        let bytes = std::fs::read(&path).expect("bmp should be readable");
        assert_eq!(&bytes[..2], b"BM");
        std::fs::remove_file(path).expect("test bmp should be removable");
    }

    #[test]
    fn painting_textarea_selection_and_caret_use_line_offsets() {
        let mut document = LayoutDocument {
            viewport: Size {
                width: 40.0,
                height: 40.0,
            },
            root: LayoutBox {
                path: Vec::new(),
                tag: "textarea".to_string(),
                rect: Rect {
                    x: 2.0,
                    y: 3.0,
                    width: 32.0,
                    height: 28.0,
                },
                background: Color::opaque(10, 20, 30),
                border_width: 0.0,
                border_color: Color::transparent(),
                text: vec![
                    TextRun {
                        text: "AB".to_string(),
                        x: 4.0,
                        y: 5.0,
                        width: 6.0,
                        font_size: 12.0,
                        color: Color::WHITE,
                        bold: false,
                    },
                    TextRun {
                        text: "CD".to_string(),
                        x: 4.0,
                        y: 20.0,
                        width: 6.0,
                        font_size: 12.0,
                        color: Color::WHITE,
                        bold: false,
                    },
                ],
                control_value: Some("ABCD".to_string()),
                control_selection: Some((2, 3)),
                control_focused: true,
                control_text_origin: Some((4.0, 5.0)),
                children: Vec::new(),
                position: crate::css::Position::Static,
                visual_offset: (0.0, 0.0),
                z_index: 0,
                creates_stacking_context: true,
                sticky_inset: crate::css::Edges::auto(),
                font_size: 12.0,
                overflow: Overflow::Visible,
                scroll_content: Size {
                    width: 32.0,
                    height: 28.0,
                },
                scroll_offset: (0.0, 0.0),
                margin: Default::default(),
            },
            images: Vec::new(),
        };
        let canvas = paint(&document);
        let selection_index = ((20 * canvas.width() + 9) * 4) as usize;
        assert_eq!(&canvas.rgba_pixels()[selection_index..selection_index + 3], &[170, 205, 238]);
        let caret_index = ((20 * canvas.width() + 10) * 4) as usize;
        assert_eq!(&canvas.rgba_pixels()[caret_index..caret_index + 3], &[0, 0, 0]);
        document.root.control_selection = Some((0, 0));
        assert_ne!(paint(&document).rgba_pixels(), canvas.rgba_pixels());
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
                control_value: None,
                control_selection: None,
                control_focused: false,
                control_text_origin: None,
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
                        control_value: None,
                        control_selection: None,
                        control_focused: false,
                        control_text_origin: None,
                        children: Vec::new(),
                        position: crate::css::Position::Static,
                        visual_offset: (0.0, 0.0),
                        z_index: 0,
                        creates_stacking_context: false,
                        sticky_inset: crate::css::Edges::auto(),
                        font_size: 16.0,
                        overflow: Overflow::Visible,
                        scroll_content: Size {
                            width: 10.0,
                            height: 10.0,
                        },
                        scroll_offset: (0.0, 0.0),
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
                        control_value: None,
                        control_selection: None,
                        control_focused: false,
                        control_text_origin: None,
                        children: Vec::new(),
                        position: crate::css::Position::Static,
                        visual_offset: (0.0, 0.0),
                        z_index: 0,
                        creates_stacking_context: false,
                        sticky_inset: crate::css::Edges::auto(),
                        font_size: 16.0,
                        overflow: Overflow::Visible,
                        scroll_content: Size {
                            width: 10.0,
                            height: 10.0,
                        },
                        scroll_offset: (0.0, 0.0),
                        margin: Default::default(),
                    },
                ],
                position: crate::css::Position::Static,
                visual_offset: (0.0, 0.0),
                z_index: 0,
                creates_stacking_context: true,
                sticky_inset: crate::css::Edges::auto(),
                font_size: 16.0,
                overflow: Overflow::Visible,
                scroll_content: Size {
                    width: 20.0,
                    height: 20.0,
                },
                scroll_offset: (0.0, 0.0),
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
