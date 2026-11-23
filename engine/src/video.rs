// a media element's decoding, on a thread of its own: the decoder an embedder supplies opens
// the source and reads frames a little ahead of the playback clock, so script never waits on
// a codec and a long video is never held whole

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError};

use crate::bridge::Waker;
use crate::css::Color;
use crate::image::Image;
use crate::sound::AudioReader;

#[derive(Clone)]
pub enum MediaSource {
    // a file the decoder reads in place
    File(PathBuf),
    Bytes { body: MediaBytes, mime_type: String },
}

// media held in memory, which every reader of it shares: a fetched body, or a file compiled into
// the app
#[derive(Clone)]
pub enum MediaBytes {
    Shared(Arc<[u8]>),
    Static(&'static [u8]),
}

impl std::ops::Deref for MediaBytes {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        match self {
            Self::Shared(body) => body,
            Self::Static(body) => body,
        }
    }
}

impl AsRef<[u8]> for MediaBytes {
    fn as_ref(&self) -> &[u8] {
        self
    }
}

impl From<Vec<u8>> for MediaBytes {
    fn from(body: Vec<u8>) -> Self {
        Self::Shared(body.into())
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MediaInfo {
    // seconds, NaN when the stream does not say
    pub duration: f64,
    // zero for a source without pictures
    pub width: u32,
    pub height: u32,
}

pub struct VideoFrame {
    // presentation time in seconds
    pub time: f64,
    pub width: u32,
    pub height: u32,
    // rows top to bottom, four bytes per pixel in blue, green, red, unused order; empty when
    // the frame comes as a surface
    pub bgra: Vec<u8>,
    pub surface: Option<Arc<dyn VideoSurface>>,
}

// a frame left in gpu memory, which a presenter shows without its pixels crossing the cpu
pub trait VideoSurface: Send + Sync {
    // an nt handle to the frame as a texture behind a keyed mutex that key 0 takes, in
    // B8G8R8A8, or in R10G10B10A2 holding bt.2100 pq when hdr10 says so
    fn shared_handle(&self) -> isize;
    // the frame's rows as in VideoFrame::bgra, for when something paints it
    fn read(&self) -> Result<Vec<u8>, String>;
    fn hdr10(&self) -> bool {
        false
    }
    // the presenter says whether the display the video shows on is in hdr, so a decoder with
    // an hdr source can hand its next frames over in hdr10
    fn show_hdr(&self, _display: bool) {}
}

impl std::fmt::Debug for dyn VideoSurface {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("VideoSurface")
    }
}

// a frame as paint and the presenter take it; a surface shown as a plane is read back only
// when something paints it, and until then its image has a size and no pixels
pub(crate) struct Picture {
    image: Arc<Image>,
    surface: Option<Arc<dyn VideoSurface>>,
    read: OnceLock<Arc<Image>>,
}

impl Picture {
    // the pixels at hand, none for a surface nothing has painted yet
    pub(crate) fn image(&self) -> Arc<Image> {
        self.read.get().unwrap_or(&self.image).clone()
    }

    pub(crate) fn pixels(&self) -> Arc<Image> {
        let Some(surface) = self
            .surface
            .as_ref()
            .filter(|_| self.image.pixels.is_empty())
        else {
            return self.image.clone();
        };
        if let Some(read) = self.read.get() {
            return read.clone();
        }
        let (width, height) = (self.image.width, self.image.height);
        // a read that fails is tried again by the next paint, where keeping its blank would
        // leave a paused video black until another frame came
        match surface
            .read()
            .ok()
            .and_then(|bgra| to_image(width, height, &bgra))
        {
            Some(image) => self.read.get_or_init(|| Arc::new(image)).clone(),
            None => Arc::new(Image::new(width, height)),
        }
    }

    pub(crate) fn surface(&self) -> Option<Arc<dyn VideoSurface>> {
        self.surface.clone()
    }
}

pub trait MediaDecoder: Send + Sync {
    fn open(&self, source: &MediaSource) -> Result<Box<dyn VideoReader>, String>;

    // the source's sound; a WAVE file of PCM never reaches here, the engine reads it itself
    fn open_audio(&self, _source: &MediaSource) -> Result<Box<dyn AudioReader>, String> {
        Err("the decoder plays no sound".to_string())
    }

    // "probably", "maybe" or "", as canPlayType answers for a MIME type
    fn can_play(&self, _mime_type: &str) -> &'static str {
        ""
    }
}

pub trait VideoReader: Send {
    fn info(&self) -> MediaInfo;
    // moves to the key frame at or before seconds
    fn seek(&mut self, seconds: f64) -> Result<(), String>;
    // none at the end of the stream
    fn read(&mut self) -> Result<Option<VideoFrame>, String>;
}

// frames decoded ahead of the one on screen
const AHEAD: usize = 3;
// a frame whose time is this close to a seek target counts as at it
const NEAR: f64 = 0.0005;

pub(crate) struct Track(Arc<Shared>);

struct Shared {
    state: Mutex<TrackState>,
    changed: Condvar,
}

#[derive(Default)]
pub(crate) struct TrackState {
    pub(crate) opened: Option<Result<MediaInfo, String>>,
    frames: VecDeque<(f64, Picture)>,
    // the seek the frames answer, and a newer one not reached yet
    pub(crate) reached: u64,
    wanted: Option<(u64, f64)>,
    pub(crate) ended: bool,
    closed: bool,
    // the last paint showed the frames as a plane, so a surface needs no reading back
    pub(crate) planed: bool,
}

impl TrackState {
    // the newest frame due at time, or the first one after a seek when the seek just landed
    pub(crate) fn take_frame(&mut self, time: f64, first: bool) -> Option<(f64, Picture)> {
        if self.wanted.is_some() {
            return None;
        }
        let mut taken = first.then(|| self.frames.pop_front()).flatten();
        while self
            .frames
            .front()
            .is_some_and(|(at, _)| *at <= time + NEAR)
        {
            taken = self.frames.pop_front();
        }
        taken
    }

    pub(crate) fn exhausted(&self) -> bool {
        self.wanted.is_none() && self.ended && self.frames.is_empty()
    }
}

impl Track {
    pub(crate) fn open(
        decoder: Arc<dyn MediaDecoder>,
        source: MediaSource,
        waker: Option<Waker>,
    ) -> Self {
        let shared = Arc::new(Shared {
            state: Mutex::default(),
            changed: Condvar::new(),
        });
        let decoding = shared.clone();
        let spawned = std::thread::Builder::new()
            .name("ceri-media".into())
            .spawn(move || decode(&*decoder, &source, &decoding, &waker));
        if let Err(error) = spawned {
            shared.lock().opened = Some(Err(error.to_string()));
        }
        Self(shared)
    }

    // whatever script changes, a frame taken or a seek asked, may give the thread work; it
    // wakes once this guard lets the state go
    pub(crate) fn lock(&self) -> MutexGuard<'_, TrackState> {
        let state = self.0.lock();
        self.0.changed.notify_all();
        state
    }

    pub(crate) fn seek(&self, id: u64, seconds: f64) {
        let mut state = self.lock();
        state.wanted = Some((id, seconds));
        state.frames.clear();
        state.ended = false;
    }
}

impl Drop for Track {
    fn drop(&mut self) {
        // the thread may sit in a slow read; it sees this after and lets the reader go
        self.lock().closed = true;
    }
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, TrackState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn decode(
    decoder: &dyn MediaDecoder,
    source: &MediaSource,
    shared: &Shared,
    waker: &Option<Waker>,
) {
    let wake = || {
        if let Some(waker) = waker {
            waker();
        }
    };
    let mut reader = match decoder.open(source) {
        Ok(reader) => reader,
        Err(error) => {
            shared.lock().opened = Some(Err(error));
            wake();
            return;
        }
    };
    shared.lock().opened = Some(Ok(reader.info()));
    wake();
    loop {
        let (wanted, planed) = {
            let mut state = shared.lock();
            while !state.closed
                && state.wanted.is_none()
                && (state.ended || state.frames.len() >= AHEAD)
            {
                state = shared
                    .changed
                    .wait(state)
                    .unwrap_or_else(PoisonError::into_inner);
            }
            if state.closed {
                return;
            }
            (state.wanted, state.planed)
        };
        let (frames, ended) = match wanted {
            Some((_, target)) => seek_to(&mut *reader, target, || shared.lock().wanted != wanted),
            None => match reader.read() {
                Ok(Some(frame)) => (vec![frame], false),
                Ok(None) | Err(_) => (Vec::new(), true),
            },
        };
        let frames = frames
            .into_iter()
            .filter_map(|frame| Some((frame.time, to_picture(frame, planed)?)))
            .collect::<Vec<_>>();
        let mut state = shared.lock();
        if state.wanted != wanted {
            continue;
        }
        if let Some((id, _)) = wanted {
            state.wanted = None;
            state.reached = id;
        }
        state.frames.extend(frames);
        state.ended = ended;
        drop(state);
        wake();
    }
}

// decodes on from the key frame before target to the frame showing at it, and the one after
fn seek_to(
    reader: &mut dyn VideoReader,
    target: f64,
    superseded: impl Fn() -> bool,
) -> (Vec<VideoFrame>, bool) {
    if reader.seek(target).is_err() {
        return (Vec::new(), true);
    }
    let mut last = None;
    loop {
        if superseded() {
            return (Vec::new(), false);
        }
        match reader.read() {
            Ok(Some(frame)) if frame.time > target + NEAR => {
                return (last.into_iter().chain([frame]).collect(), false);
            }
            Ok(Some(frame)) => last = Some(frame),
            Ok(None) | Err(_) => return (last.into_iter().collect(), true),
        }
    }
}

// a painted video's surface is read here, off the engine thread
fn to_picture(frame: VideoFrame, planed: bool) -> Option<Picture> {
    let VideoFrame {
        width,
        height,
        bgra,
        surface,
        ..
    } = frame;
    let image = match &surface {
        Some(_) if planed && width > 0 && height > 0 => Image {
            levels: Default::default(),
            width,
            height,
            pixels: Vec::new(),
        },
        Some(surface) if bgra.is_empty() => {
            // paint takes sdr, so later frames need no mapping on their way back
            surface.show_hdr(false);
            to_image(width, height, &surface.read().ok()?)?
        }
        _ => to_image(width, height, &bgra)?,
    };
    Some(Picture {
        image: Arc::new(image),
        surface,
        read: OnceLock::new(),
    })
}

fn to_image(width: u32, height: u32, bgra: &[u8]) -> Option<Image> {
    let area = width as usize * height as usize;
    if area == 0 || bgra.len() < area * 4 {
        return None;
    }
    let pixels = bgra
        .as_chunks::<4>()
        .0
        .iter()
        .map(|&[blue, green, red, _]| Color {
            red,
            green,
            blue,
            alpha: 255,
        })
        .take(area)
        .collect();
    Some(Image {
        levels: Default::default(),
        width,
        height,
        pixels,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // a surface whose first read fails
    struct Flaky(AtomicUsize);

    impl VideoSurface for Flaky {
        fn shared_handle(&self) -> isize {
            0
        }

        fn read(&self) -> Result<Vec<u8>, String> {
            match self.0.fetch_add(1, Ordering::Relaxed) {
                0 => Err("busy".to_string()),
                _ => Ok(vec![10, 20, 30, 255]),
            }
        }
    }

    #[test]
    fn a_surface_whose_read_fails_is_read_again_and_then_kept() {
        let surface = Arc::new(Flaky(AtomicUsize::new(0)));
        let picture = Picture {
            image: Arc::new(Image {
                levels: Default::default(),
                width: 1,
                height: 1,
                pixels: Vec::new(),
            }),
            surface: Some(surface.clone()),
            read: OnceLock::new(),
        };
        let blank = picture.pixels();
        assert_eq!(blank.pixels, Image::new(1, 1).pixels);
        // nothing was kept, so paint still sees a frame without pixels and asks again
        assert!(picture.image().pixels.is_empty());
        let read = picture.pixels();
        assert_ne!(read.pixels, blank.pixels);
        assert!(Arc::ptr_eq(&read, &picture.pixels()));
        assert_eq!(surface.0.load(Ordering::Relaxed), 2);
    }
}
