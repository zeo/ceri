// animated GIF and WebP images: frames are composed in turn on a canvas of the image's size
// and decoded only when their turn comes, so a long recording holds two canvases, not every
// frame; the frame on show follows the page clock from when the image was first decoded

use crate::css::Color;
use crate::gif::GifFrame;
use crate::image::Image;
use crate::webp::WebpFrame;
use std::cell::Cell;
use std::sync::Arc;

enum Frames {
    Gif(Vec<GifFrame>),
    Webp(Vec<WebpFrame>),
}

#[derive(Clone, Copy, PartialEq)]
enum Dispose {
    Keep,
    Clear,
    Restore,
}

struct Step {
    delay: u64,
    dispose: Dispose,
    blend: bool,
}

pub(crate) struct Animation {
    width: u32,
    height: u32,
    frames: Frames,
    steps: Vec<Step>,
    total: u64,
    plays: Option<u32>,
    started: u64,
    index: usize,
    // straight RGBA of the frame on show, the canvas before it for one that restores, and
    // the area it covers
    canvas: Vec<[u8; 4]>,
    saved: Option<Vec<[u8; 4]>>,
    area: [u32; 4],
    pub(crate) first: Arc<Image>,
    current: Arc<Image>,
    // whether the last frame painted showed it, so a hidden image wakes nobody
    pub(crate) shown: Cell<bool>,
}

fn pixels(image: &Image) -> Vec<[u8; 4]> {
    image
        .pixels
        .iter()
        .map(|color| [color.red, color.green, color.blue, color.alpha])
        .collect()
}

// libwebp's blend of a straight-alpha pixel over another
fn blend(source: [u8; 4], destination: [u8; 4]) -> [u8; 4] {
    let source_alpha = u32::from(source[3]);
    if source_alpha == 0 {
        return destination;
    }
    if source_alpha == 255 {
        return source;
    }
    let destination_factor = (u32::from(destination[3]) * (256 - source_alpha)) >> 8;
    let alpha = source_alpha + destination_factor;
    let scale = (1 << 24) / alpha;
    let channel = |index: usize| {
        (((u32::from(source[index]) * source_alpha
            + u32::from(destination[index]) * destination_factor)
            * scale)
            >> 24) as u8
    };
    [channel(0), channel(1), channel(2), alpha as u8]
}

impl Animation {
    // an animation for an image of more than one frame, beginning now with its first frame,
    // which `first` already shows
    pub(crate) fn new(bytes: &[u8], first: Arc<Image>, now: u64) -> Option<Animation> {
        let (width, height, frames, steps, plays) = if crate::gif::is_gif(bytes) {
            let (width, height, frames, plays) = crate::gif::gif_frames(bytes).ok()?;
            let steps = frames
                .iter()
                .map(|frame| Step {
                    delay: u64::from(frame.delay),
                    dispose: match frame.disposal {
                        2 => Dispose::Clear,
                        3 => Dispose::Restore,
                        _ => Dispose::Keep,
                    },
                    blend: true,
                })
                .collect::<Vec<_>>();
            (width, height, Frames::Gif(frames), steps, plays)
        } else {
            let (width, height, frames, plays) = crate::webp::webp_frames(bytes)?;
            let steps = frames
                .iter()
                .map(|frame| Step {
                    delay: u64::from(frame.duration),
                    dispose: if frame.dispose {
                        Dispose::Clear
                    } else {
                        Dispose::Keep
                    },
                    blend: frame.blend,
                })
                .collect::<Vec<_>>();
            (width, height, Frames::Webp(frames), steps, plays)
        };
        if steps.len() < 2 || first.width != width || first.height != height {
            return None;
        }
        let total = steps.iter().map(|step| step.delay).sum();
        let mut animation = Animation {
            width,
            height,
            frames,
            steps,
            total,
            plays,
            started: now,
            index: 0,
            canvas: pixels(&first),
            saved: None,
            area: [0; 4],
            current: first.clone(),
            first,
            shown: Cell::new(false),
        };
        animation.area = animation.frame_area(0);
        Some(animation)
    }

    fn frame_area(&self, index: usize) -> [u32; 4] {
        match &self.frames {
            Frames::Gif(frames) => {
                let frame = &frames[index];
                [frame.left, frame.top, frame.width, frame.height]
            }
            Frames::Webp(frames) => {
                let frame = &frames[index];
                let (width, height) = frame
                    .rgba()
                    .map_or((0, 0), |(width, height, _)| (width, height));
                [frame.left, frame.top, width, height]
            }
        }
    }

    // which frame shows at a clock time, and whether the animation has stopped on it
    fn frame_for(&self, now: u64) -> (usize, bool) {
        let elapsed = now.saturating_sub(self.started);
        if self.total == 0 {
            return (0, true);
        }
        if let Some(plays) = self.plays
            && elapsed >= self.total * u64::from(plays)
        {
            return (self.steps.len() - 1, true);
        }
        let mut at = elapsed % self.total;
        for (index, step) in self.steps.iter().enumerate() {
            if at < step.delay {
                return (index, false);
            }
            at -= step.delay;
        }
        (self.steps.len() - 1, false)
    }

    // milliseconds until the frame on show changes, none once the animation has stopped
    pub(crate) fn next_change(&self, now: u64) -> Option<u64> {
        let (index, stopped) = self.frame_for(now);
        if stopped || self.total == 0 {
            return None;
        }
        let elapsed = now.saturating_sub(self.started) % self.total;
        let end: u64 = self.steps[..=index].iter().map(|step| step.delay).sum();
        Some(end.saturating_sub(elapsed).max(1))
    }

    // the picture at a clock time, composing the frames between the one on show and it
    pub(crate) fn frame_at(&mut self, now: u64) -> Arc<Image> {
        let (target, _) = self.frame_for(now);
        if target == self.index {
            return self.current.clone();
        }
        if target < self.index {
            self.canvas = pixels(&self.first);
            self.saved = None;
            self.index = 0;
            self.area = self.frame_area(0);
        }
        while self.index < target {
            self.compose(self.index + 1);
        }
        self.current = Arc::new(Image {
            levels: Default::default(),
            width: self.width,
            height: self.height,
            pixels: self
                .canvas
                .iter()
                .map(|&[red, green, blue, alpha]| Color {
                    red,
                    green,
                    blue,
                    alpha,
                })
                .collect(),
        });
        self.current.clone()
    }

    fn compose(&mut self, next: usize) {
        let width = self.width as usize;
        let [x0, y0, w, h] = self.area;
        match self.steps[self.index].dispose {
            Dispose::Clear => {
                for y in y0..(y0 + h).min(self.height) {
                    for x in x0..(x0 + w).min(self.width) {
                        self.canvas[y as usize * width + x as usize] = [0; 4];
                    }
                }
            }
            Dispose::Restore => {
                if let Some(saved) = self.saved.take() {
                    self.canvas = saved;
                }
            }
            Dispose::Keep => {}
        }
        let step = &self.steps[next];
        self.saved = (step.dispose == Dispose::Restore).then(|| self.canvas.clone());
        let (left, top, frame_width, frame_height, rgba) = match &self.frames {
            Frames::Gif(frames) => {
                let frame = &frames[next];
                (
                    frame.left,
                    frame.top,
                    frame.width,
                    frame.height,
                    frame.rgba(),
                )
            }
            Frames::Webp(frames) => {
                let frame = &frames[next];
                match frame.rgba() {
                    Some((width, height, rgba)) => (frame.left, frame.top, width, height, rgba),
                    None => (frame.left, frame.top, 0, 0, Vec::new()),
                }
            }
        };
        self.area = [left, top, frame_width, frame_height];
        for y in 0..frame_height {
            for x in 0..frame_width {
                let (cx, cy) = (left + x, top + y);
                if cx >= self.width || cy >= self.height {
                    continue;
                }
                let at = ((y * frame_width + x) * 4) as usize;
                let Some(source) = rgba.get(at..at + 4) else {
                    continue;
                };
                let source = [source[0], source[1], source[2], source[3]];
                let pixel = &mut self.canvas[cy as usize * width + cx as usize];
                *pixel = if step.blend {
                    blend(source, *pixel)
                } else {
                    source
                };
            }
        }
        self.index = next;
    }
}
