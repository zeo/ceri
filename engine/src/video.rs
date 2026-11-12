// a media element's decoding, on a thread of its own: the decoder an embedder supplies opens
// the source and reads frames a little ahead of the playback clock, so script never waits on
// a codec and a long video is never held whole

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

use crate::bridge::Waker;
use crate::css::Color;
use crate::image::Image;
use crate::sound::AudioReader;

#[derive(Clone)]
pub enum MediaSource {
    // a file the decoder reads in place
    File(PathBuf),
    Bytes { body: Arc<[u8]>, mime_type: String },
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
    // rows top to bottom, four bytes per pixel in blue, green, red, unused order
    pub bgra: Vec<u8>,
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
    frames: VecDeque<(f64, Image)>,
    // the seek the frames answer, and a newer one not reached yet
    pub(crate) reached: u64,
    wanted: Option<(u64, f64)>,
    pub(crate) ended: bool,
    closed: bool,
}

impl TrackState {
    // the newest frame due at time, or the first one after a seek when the seek just landed
    pub(crate) fn take_frame(&mut self, time: f64, first: bool) -> Option<(f64, Image)> {
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
        let wanted = {
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
            state.wanted
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
            .filter_map(|frame| Some((frame.time, to_image(frame)?)))
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

fn to_image(frame: VideoFrame) -> Option<Image> {
    let area = frame.width as usize * frame.height as usize;
    if area == 0 || frame.bgra.len() < area * 4 {
        return None;
    }
    let pixels = frame
        .bgra
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
        width: frame.width,
        height: frame.height,
        pixels,
    })
}
