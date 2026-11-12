// a media element's sound: a thread reads the source about a second ahead, and the speakers
// pull from that queue in step with the element's clock, skipping or waiting to stay on it

use std::collections::VecDeque;
use std::fs::File;
use std::io::{BufReader, Cursor, Read, Seek, SeekFrom};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Instant;

use crate::audio::{AudioOutput, AudioRender, WaveFormat, wave_format};
use crate::bridge::Waker;
use crate::video::{MediaDecoder, MediaSource};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AudioInfo {
    // frames a second
    pub rate: u32,
    pub channels: u16,
    // seconds, NaN when the stream does not say
    pub duration: f64,
}

pub struct AudioChunk {
    // seconds at the first frame
    pub time: f64,
    // frames of the info's channels, interleaved, at its rate
    pub samples: Vec<f32>,
}

pub trait AudioReader: Send {
    fn info(&self) -> AudioInfo;
    // moves to seconds or a little before; the chunks after say where it landed
    fn seek(&mut self, seconds: f64) -> Result<(), String>;
    // none at the end of the stream
    fn read(&mut self) -> Result<Option<AudioChunk>, String>;
}

// seconds read ahead of the speakers
const AHEAD: f64 = 1.0;
// how far the sound may stray from the element's clock before it skips or waits, about
// where a lag starts to show against pictures
const SLACK: f64 = 0.04;

pub(crate) struct Sound {
    shared: Arc<Shared>,
    render: AudioRender,
    output: Option<Box<dyn AudioOutput>>,
    // the element clock the speakers follow: its position, its start and its rate
    following: Option<(f64, u64, f64)>,
}

struct Shared {
    state: Mutex<SoundState>,
    changed: Condvar,
}

#[derive(Default)]
pub(crate) struct SoundState {
    pub(crate) opened: Option<Result<AudioInfo, String>>,
    queue: VecDeque<f32>,
    // the media time of the queue's first frame, and how far past it the speakers have read
    head: f64,
    fraction: f64,
    // the seek the queue answers, and a newer one not reached yet
    reached: u64,
    wanted: Option<(u64, f64)>,
    ended: bool,
    closed: bool,
    // the media time due at an instant while the element's clock runs
    clock: Option<(f64, Instant)>,
    rate: f64,
    gain: f32,
    output_rate: u32,
    preserves_pitch: bool,
    // the queue stretched to the rate at the source's pitch, while one is wanted
    stretch: Option<Stretch>,
}

// pitch kept at another rate by overlapping windows of the source where each lines up best
// with the last, as chromium's wsola does
struct Stretch {
    window: usize,
    search: usize,
    hann: Vec<f32>,
    transition: Vec<f32>,
    // in frames past the queue's first: where the next window is looked for, and where the
    // last one carries on, none before the first
    position: f64,
    target: Option<usize>,
    // the last window's second half, waiting for the next one to overlap it
    overlap: Vec<f32>,
    output: VecDeque<f32>,
    // the rate the stretched frames waiting in output were made at
    rate: f64,
}

// chromium's: 20 ms windows looked for 15 ms either side, never within 160 frames of the
// window before, every fifth candidate tried and then the ones beside the best
const WINDOW: f64 = 0.02;
const SEARCH: f64 = 0.015;
const EXCLUDE: usize = 160;
const DECIMATION: usize = 5;

impl Stretch {
    fn new(rate: u32, sources: usize) -> Self {
        let window = ((WINDOW * f64::from(rate)) as usize / 2 * 2).max(2);
        let hann = |length: usize| -> Vec<f32> {
            (0..length)
                .map(|n| {
                    let phase = std::f64::consts::TAU * n as f64 / length as f64;
                    (0.5 * (1.0 - phase.cos())) as f32
                })
                .collect()
        };
        Self {
            window,
            search: (SEARCH * f64::from(rate)) as usize,
            hann: hann(window),
            transition: hann(2 * window),
            position: 0.0,
            target: None,
            overlap: vec![0.0; window / 2 * sources],
            output: VecDeque::new(),
            rate: 1.0,
        }
    }

    // the frames a block needs, from the first to past the last
    fn span(&self) -> (usize, usize) {
        let center = self.position.round().max(0.0) as usize;
        let target = self.target.unwrap_or(center);
        (
            center.saturating_sub(self.search).min(target),
            (center + self.search).max(target) + self.window,
        )
    }
}

impl SoundState {
    // the queue answers the element's latest seek with sound or the end
    pub(crate) fn ready(&self, seek: u64) -> bool {
        self.reached == seek && self.wanted.is_none() && (self.ended || !self.queue.is_empty())
    }

    pub(crate) fn exhausted(&self) -> bool {
        self.wanted.is_none()
            && self.ended
            && self.queue.is_empty()
            && self
                .stretch
                .as_ref()
                .is_none_or(|stretch| stretch.output.is_empty())
    }

    // the speakers' next frames, read at the element's rate from where its clock stands
    fn fill(&mut self, out: &mut [f32], channels: usize, now: Instant) {
        let (Some(Ok(info)), Some((time, at))) = (&self.opened, self.clock) else {
            return;
        };
        let info = *info;
        if self.wanted.is_some() || channels == 0 || self.output_rate == 0 || self.rate <= 0.0 {
            return;
        }
        let sources = usize::from(info.channels);
        let rate = f64::from(info.rate);
        let stretching = self.preserves_pitch && self.rate != 1.0;
        if stretching != self.stretch.is_some() {
            self.settle(sources, rate);
        }
        let due = time + now.saturating_duration_since(at).as_secs_f64() * self.rate;
        let playing = self.playing(sources, rate);
        let mut wait = 0;
        if playing < due - SLACK {
            self.settle(sources, rate);
            let behind = ((due - self.playing(sources, rate)) * rate).round() as usize;
            self.advance(behind, sources, rate);
        } else if playing > due + SLACK {
            wait = ((playing - due) * f64::from(self.output_rate) / self.rate).round() as usize;
        }
        if stretching && self.stretch.is_none() {
            self.stretch = Some(Stretch::new(info.rate, sources));
        }
        // a stretch has already taken the rate out, so what is left is the speakers' own
        let speed = if stretching { 1.0 } else { self.rate };
        let step = speed * rate / f64::from(self.output_rate);
        for frame in out.chunks_exact_mut(channels).skip(wait) {
            while self
                .stretch
                .as_ref()
                .is_some_and(|stretch| stretch.output.len() < 2 * sources)
                && self.stretch_block(sources, rate)
            {}
            let (buffer, ended) = match &self.stretch {
                Some(stretch) => (&stretch.output, self.ended && self.queue.is_empty()),
                None => (&self.queue, self.ended),
            };
            let available = buffer.len() / sources;
            if available == 0 || (available == 1 && !ended) {
                break;
            }
            let fraction = self.fraction as f32;
            let read = |channel: usize| {
                let here = buffer[channel];
                let next = match available > 1 {
                    true => buffer[sources + channel],
                    false => 0.0,
                };
                here + (next - here) * fraction
            };
            for (channel, sample) in frame.iter_mut().enumerate() {
                // mono spreads to every speaker, stereo folds into one, the rest map in order
                let value = match (sources, channels) {
                    (1, _) => read(0),
                    (2, 1) => 0.5 * (read(0) + read(1)),
                    _ if channel < sources => read(channel),
                    _ => 0.0,
                };
                *sample += value * self.gain;
            }
            self.fraction += step;
            let whole = self.fraction.floor();
            self.fraction -= whole;
            match &mut self.stretch {
                Some(stretch) => {
                    let played = (whole as usize * sources).min(stretch.output.len());
                    stretch.output.drain(..played);
                }
                None => self.advance(whole as usize, sources, rate),
            }
        }
    }

    // the media time the speakers are at
    fn playing(&self, sources: usize, rate: f64) -> f64 {
        let frames = match &self.stretch {
            Some(stretch) => {
                let pending = (stretch.output.len() / sources) as f64 - self.fraction;
                stretch.position - pending * stretch.rate
            }
            None => self.fraction,
        };
        self.head + frames / rate
    }

    // drops a stretch, leaving the queue to start at the frame it was playing
    fn settle(&mut self, sources: usize, rate: f64) {
        if self.stretch.is_some() {
            let frames = ((self.playing(sources, rate) - self.head) * rate)
                .round()
                .max(0.0);
            self.stretch = None;
            self.fraction = 0.0;
            self.advance(frames as usize, sources, rate);
        }
    }

    // stretches one hop of the queue into the output; false while it waits on the reader, or
    // once the sound is all stretched
    fn stretch_block(&mut self, sources: usize, rate: f64) -> bool {
        let Some(stretch) = &mut self.stretch else {
            return false;
        };
        let available = self.queue.len() / sources;
        let center = stretch.position.round().max(0.0) as usize;
        if self.ended && center >= available {
            self.queue.clear();
            return false;
        }
        let (first, end) = stretch.span();
        if available < end && !self.ended {
            return false;
        }
        let (window, search) = (stretch.window, stretch.search);
        let hop = window / 2;
        // past the end of the sound reads as silence
        let span: Vec<f32> = self
            .queue
            .range(first * sources..end.min(available) * sources)
            .copied()
            .chain(std::iter::repeat(0.0))
            .take((end - first) * sources)
            .collect();
        let block = |index: usize| &span[(index - first) * sources..][..window * sources];
        let (low, high) = (center.saturating_sub(search), center + search);
        let (best, carried) = match stretch.target {
            None => (center, None),
            Some(target) if (low..=high).contains(&target) => (target, None),
            Some(target) => {
                // each channel apart, with running energies, so a candidate costs one product
                let planes: Vec<Vec<f32>> = (0..sources)
                    .map(|c| span.iter().skip(c).step_by(sources).copied().collect())
                    .collect();
                let squares: Vec<Vec<f64>> = planes
                    .iter()
                    .map(|plane| {
                        std::iter::once(0.0)
                            .chain(plane.iter().scan(0.0, |sum, &s| {
                                *sum += f64::from(s * s);
                                Some(*sum)
                            }))
                            .collect()
                    })
                    .collect();
                // running sums can round below zero over silence
                let energy = |c: usize, index: usize| {
                    (squares[c][index - first + window] - squares[c][index - first]).max(0.0)
                };
                let plane = |c: usize, index: usize| &planes[c][index - first..][..window];
                let last = target as isize - hop as isize;
                let score = |index: usize| -> f64 {
                    (0..sources)
                        .map(|c| {
                            let product = f64::from(dot(plane(c, index), plane(c, target)));
                            product / (energy(c, index) * energy(c, target) + 1e-12).sqrt()
                        })
                        .sum()
                };
                let pick = |candidates: &mut dyn Iterator<Item = usize>| {
                    candidates
                        .filter(|&index| (index as isize - last).unsigned_abs() > EXCLUDE)
                        .map(|index| (score(index), index))
                        .max_by(|a, b| a.0.total_cmp(&b.0))
                        .map(|(_, index)| index)
                };
                let coarse = pick(&mut (low..=high).step_by(DECIMATION));
                let fine = coarse.and_then(|coarse| {
                    let mut near = coarse.saturating_sub(DECIMATION - 1).max(low)
                        ..=(coarse + DECIMATION - 1).min(high);
                    pick(&mut near)
                });
                (fine.unwrap_or(center), Some(block(target)))
            }
        };
        let mut windowed = block(best).to_vec();
        for (n, frame) in windowed.chunks_exact_mut(sources).enumerate() {
            let carried = carried.map(|reference| &reference[n * sources..][..sources]);
            for (c, sample) in frame.iter_mut().enumerate() {
                // a jump fades in from where the last window would have carried on
                if let Some(reference) = carried {
                    *sample = *sample * stretch.transition[n]
                        + reference[c] * stretch.transition[window + n];
                }
                *sample *= stretch.hann[n];
            }
        }
        let (start, rest) = windowed.split_at(hop * sources);
        stretch
            .output
            .extend(stretch.overlap.iter().zip(start).map(|(a, b)| a + b));
        stretch.overlap.copy_from_slice(rest);
        stretch.target = Some(best + hop);
        stretch.position += hop as f64 * self.rate;
        stretch.rate = self.rate;
        let played = stretch.span().0.min(available);
        stretch.position -= played as f64;
        stretch.target = stretch.target.map(|target| target - played);
        self.queue.drain(..played * sources);
        self.head += played as f64 / rate;
        true
    }

    fn advance(&mut self, frames: usize, sources: usize, rate: f64) {
        let frames = frames.min(self.queue.len() / sources);
        self.queue.drain(..frames * sources);
        self.head += frames as f64 / rate;
    }
}

// eight lanes at a time, which the compiler can keep in vector registers
fn dot(a: &[f32], b: &[f32]) -> f32 {
    let mut lanes = [0.0f32; 8];
    let ((whole, tail), (other, rest)) = (a.as_chunks::<8>(), b.as_chunks::<8>());
    for (a, b) in whole.iter().zip(other) {
        for ((lane, a), b) in lanes.iter_mut().zip(a).zip(b) {
            *lane += a * b;
        }
    }
    let tail: f32 = tail.iter().zip(rest).map(|(a, b)| a * b).sum();
    lanes.iter().sum::<f32>() + tail
}

impl Sound {
    pub(crate) fn open(
        decoder: Option<Arc<dyn MediaDecoder>>,
        source: MediaSource,
        waker: Option<Waker>,
    ) -> Self {
        let shared = Arc::new(Shared {
            state: Mutex::new(SoundState {
                rate: 1.0,
                gain: 1.0,
                ..SoundState::default()
            }),
            changed: Condvar::new(),
        });
        let render = AudioRender::default();
        let feeding = shared.clone();
        render.set_feed(Arc::new(move |out, channels| {
            feeding.lock().fill(out, channels, Instant::now());
            feeding.changed.notify_all();
        }));
        render.set_running(true);
        let reading = shared.clone();
        let spawned = std::thread::Builder::new()
            .name("ceri-media-sound".into())
            .spawn(move || read(decoder, &source, &reading, &waker));
        if let Err(error) = spawned {
            shared.lock().opened = Some(Err(error.to_string()));
        }
        Self {
            shared,
            render,
            output: None,
            following: None,
        }
    }

    // a change script makes may give the thread work; it wakes once this guard lets go
    pub(crate) fn lock(&self) -> MutexGuard<'_, SoundState> {
        let state = self.shared.lock();
        self.shared.changed.notify_all();
        state
    }

    pub(crate) fn seek(&self, id: u64, seconds: f64) {
        let mut state = self.lock();
        state.wanted = Some((id, seconds));
        state.queue.clear();
        state.fraction = 0.0;
        state.stretch = None;
        state.ended = false;
    }

    // speakers open while the element plays and follow its clock while that runs; the clock
    // is its position, the clock time it started from and its rate, and time where it is now
    pub(crate) fn follow(
        &mut self,
        playing: bool,
        clock: Option<(f64, u64, f64)>,
        time: f64,
        gain: f32,
        preserves_pitch: bool,
        open: impl FnOnce(u32, &AudioRender) -> Box<dyn AudioOutput>,
    ) {
        let Some(Ok(info)) = self.shared.lock().opened.clone() else {
            return;
        };
        let output_rate = match (playing, &self.output) {
            (false, _) => {
                self.output = None;
                0
            }
            (true, Some(output)) => output.sample_rate(),
            (true, None) => {
                let output = open(info.rate, &self.render);
                let rate = output.sample_rate();
                self.output = Some(output);
                rate
            }
        };
        let mut state = self.shared.lock();
        state.output_rate = output_rate;
        state.gain = gain;
        state.preserves_pitch = preserves_pitch;
        if let Some((_, _, rate)) = clock {
            state.rate = rate;
        }
        if clock != self.following {
            state.clock = clock.map(|_| (time, Instant::now()));
            self.following = clock;
        }
    }
}

impl Drop for Sound {
    fn drop(&mut self) {
        // the thread may sit in a slow read; it sees this after and lets the source go
        self.output = None;
        self.lock().closed = true;
    }
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, SoundState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn read(
    decoder: Option<Arc<dyn MediaDecoder>>,
    source: &MediaSource,
    shared: &Shared,
    waker: &Option<Waker>,
) {
    let wake = || {
        if let Some(waker) = waker {
            waker();
        }
    };
    let opened = match WaveReader::open(source) {
        Some(reader) => Ok(Box::new(reader) as Box<dyn AudioReader>),
        None => decoder
            .ok_or_else(|| "nothing here decodes the media's sound".to_string())
            .and_then(|decoder| decoder.open_audio(source)),
    };
    let opened = opened.and_then(|reader| match reader.info() {
        info if info.rate == 0 || info.channels == 0 => {
            Err("the media's sound has no frames to play".to_string())
        }
        _ => Ok(reader),
    });
    let mut reader = match opened {
        Ok(reader) => reader,
        Err(error) => {
            shared.lock().opened = Some(Err(error));
            wake();
            return;
        }
    };
    let info = reader.info();
    shared.lock().opened = Some(Ok(info));
    wake();
    let limit = (AHEAD * f64::from(info.rate)) as usize * usize::from(info.channels);
    let mut fresh = true;
    loop {
        let wanted = {
            let mut state = shared.lock();
            while !state.closed
                && state.wanted.is_none()
                && (state.ended || state.queue.len() >= limit)
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
        let (chunk, ended) = match wanted {
            Some((_, target)) => seek_to(&mut *reader, info, target, || {
                shared.lock().wanted != wanted
            }),
            // sound a container shifts before zero, as aac priming, is cut like a seek's
            None if fresh => read_from(&mut *reader, info, 0.0, || shared.lock().wanted.is_some()),
            None => match reader.read() {
                Ok(Some(chunk)) => (Some(chunk), false),
                Ok(None) | Err(_) => (None, true),
            },
        };
        let mut state = shared.lock();
        if state.wanted != wanted {
            continue;
        }
        let mut news = state.ended != ended;
        if let Some((id, target)) = wanted {
            state.wanted = None;
            state.reached = id;
            state.head = target;
            fresh = true;
            news = true;
        }
        if let Some(chunk) = chunk {
            if fresh {
                state.head = chunk.time;
                fresh = false;
                news = true;
            }
            state.queue.extend(chunk.samples);
        }
        state.ended = ended;
        drop(state);
        if news {
            wake();
        }
    }
}

// the first chunk at or after target, cut to start there
fn seek_to(
    reader: &mut dyn AudioReader,
    info: AudioInfo,
    target: f64,
    superseded: impl Fn() -> bool,
) -> (Option<AudioChunk>, bool) {
    if reader.seek(target).is_err() {
        return (None, true);
    }
    read_from(reader, info, target, superseded)
}

fn read_from(
    reader: &mut dyn AudioReader,
    info: AudioInfo,
    target: f64,
    superseded: impl Fn() -> bool,
) -> (Option<AudioChunk>, bool) {
    let rate = f64::from(info.rate);
    let sources = usize::from(info.channels);
    loop {
        if superseded() {
            return (None, false);
        }
        match reader.read() {
            Ok(Some(mut chunk)) => {
                let early = ((target - chunk.time) * rate).round().max(0.0) as usize;
                if early >= chunk.samples.len() / sources {
                    continue;
                }
                chunk.samples.drain(..early * sources);
                chunk.time += early as f64 / rate;
                return (Some(chunk), false);
            }
            Ok(None) | Err(_) => return (None, true),
        }
    }
}

trait Source: Read + Seek + Send {}

impl<T: Read + Seek + Send> Source for T {}

// a RIFF WAVE file read a chunk at a time, so a long recording is never held whole
struct WaveReader {
    input: Box<dyn Source>,
    format: WaveFormat,
    // where the first frame starts, how many there are, and the next one read
    data: u64,
    frames: u64,
    next: u64,
}

// frames read at a time
const CHUNK: u64 = 4096;

impl WaveReader {
    // none for anything but PCM a WAVE file holds, which goes to the decoder instead
    fn open(source: &MediaSource) -> Option<Self> {
        let mut input: Box<dyn Source> = match source {
            MediaSource::File(path) => Box::new(BufReader::new(File::open(path).ok()?)),
            MediaSource::Bytes { body, .. } => Box::new(Cursor::new(body.clone())),
        };
        let mut riff = [0; 12];
        input.read_exact(&mut riff).ok()?;
        if &riff[..4] != b"RIFF" || &riff[8..] != b"WAVE" {
            return None;
        }
        let length = input.seek(SeekFrom::End(0)).ok()?;
        let mut at = input.seek(SeekFrom::Start(12)).ok()?;
        let mut format = None;
        loop {
            let mut header = [0; 8];
            input.read_exact(&mut header).ok()?;
            let size = u64::from(u32::from_le_bytes(*header[4..].first_chunk()?));
            at += 8;
            match &header[..4] {
                b"fmt " => {
                    let mut content = vec![0; size.min(64) as usize];
                    input.read_exact(&mut content).ok()?;
                    format = wave_format(&content);
                }
                b"data" => {
                    let format = format?;
                    // a writer that never came back to fill in the size leaves the data
                    // running to the end
                    let end = at.saturating_add(size).min(length);
                    let block = (format.width * format.channels) as u64;
                    return Some(Self {
                        input,
                        format,
                        data: at,
                        frames: end.saturating_sub(at) / block,
                        next: 0,
                    });
                }
                _ => {}
            }
            at += size + (size & 1);
            input.seek(SeekFrom::Start(at)).ok()?;
        }
    }

    fn block(&self) -> u64 {
        (self.format.width * self.format.channels) as u64
    }
}

impl AudioReader for WaveReader {
    fn info(&self) -> AudioInfo {
        AudioInfo {
            rate: self.format.rate,
            channels: self.format.channels as u16,
            duration: self.frames as f64 / f64::from(self.format.rate),
        }
    }

    fn seek(&mut self, seconds: f64) -> Result<(), String> {
        self.next = ((seconds.max(0.0) * f64::from(self.format.rate)) as u64).min(self.frames);
        self.input
            .seek(SeekFrom::Start(self.data + self.next * self.block()))
            .map(drop)
            .map_err(|error| error.to_string())
    }

    fn read(&mut self) -> Result<Option<AudioChunk>, String> {
        let count = CHUNK.min(self.frames - self.next);
        if count == 0 {
            return Ok(None);
        }
        let mut bytes = vec![0; (count * self.block()) as usize];
        self.input
            .read_exact(&mut bytes)
            .map_err(|error| error.to_string())?;
        let time = self.next as f64 / f64::from(self.format.rate);
        self.next += count;
        Ok(Some(AudioChunk {
            time,
            samples: bytes
                .chunks_exact(self.format.width)
                .map(self.format.sample)
                .collect(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    // a stereo 16 bit file whose left channel counts frames up from zero and right one down
    fn counting(frames: u16, rate: u32) -> Vec<u8> {
        let mut data = Vec::new();
        for frame in 0..frames {
            data.extend_from_slice(&(frame as i16).to_le_bytes());
            data.extend_from_slice(&(-(frame as i16)).to_le_bytes());
        }
        let mut file = b"RIFF\0\0\0\0WAVE".to_vec();
        file.extend_from_slice(b"LIST\x03\0\0\0abc\0");
        file.extend_from_slice(b"fmt \x10\0\0\0\x01\0\x02\0");
        file.extend_from_slice(&rate.to_le_bytes());
        file.extend_from_slice(&(rate * 4).to_le_bytes());
        file.extend_from_slice(&[4, 0, 16, 0]);
        file.extend_from_slice(b"data");
        file.extend_from_slice(&(data.len() as u32).to_le_bytes());
        file.extend_from_slice(&data);
        file
    }

    fn bytes(file: Vec<u8>) -> MediaSource {
        MediaSource::Bytes {
            body: file.into(),
            mime_type: "audio/wav".into(),
        }
    }

    fn level(sample: f32) -> i32 {
        (sample * 32768.0).round() as i32
    }

    #[test]
    fn a_wave_file_streams_in_chunks_from_where_it_seeks() {
        let mut reader = WaveReader::open(&bytes(counting(5000, 8000))).expect("a wave file");
        assert_eq!(
            reader.info(),
            AudioInfo {
                rate: 8000,
                channels: 2,
                duration: 0.625
            }
        );
        let first = reader.read().expect("reads").expect("a chunk");
        assert_eq!((first.time, first.samples.len()), (0.0, 8192));
        assert_eq!(level(first.samples[2 * 4095]), 4095);
        let rest = reader.read().expect("reads").expect("a chunk");
        assert_eq!((rest.time, rest.samples.len() / 2), (0.512, 904));
        assert!(reader.read().expect("reads").is_none());
        reader.seek(0.5).expect("seeks");
        let after = reader.read().expect("reads").expect("a chunk");
        assert_eq!(
            (after.time, level(after.samples[0]), level(after.samples[1])),
            (0.5, 4000, -4000)
        );
        assert!(WaveReader::open(&bytes(b"RIFF\0\0\0\0AVI LIST".to_vec())).is_none());
    }

    fn state(frames: u16, rate: u32) -> (SoundState, Instant) {
        let reader = WaveReader::open(&bytes(counting(frames, rate))).expect("a wave file");
        let mut state = SoundState {
            opened: Some(Ok(reader.info())),
            rate: 1.0,
            gain: 1.0,
            output_rate: rate,
            ..SoundState::default()
        };
        let mut reader = reader;
        while let Some(chunk) = reader.read().expect("reads") {
            state.queue.extend(chunk.samples);
        }
        state.ended = true;
        let start = Instant::now();
        state.clock = Some((0.0, start));
        (state, start)
    }

    #[test]
    fn the_speakers_read_at_the_element_rate_and_gain() {
        let (mut state, start) = state(400, 1000);
        let mut out = vec![0.0; 8];
        state.fill(&mut out, 2, start);
        assert_eq!(
            out.iter().map(|&s| level(s)).collect::<Vec<_>>(),
            [0, 0, 1, -1, 2, -2, 3, -3]
        );
        state.rate = 2.0;
        state.gain = 0.5;
        state.clock = Some((state.head, start));
        let mut mono = vec![0.0; 3];
        state.fill(&mut mono, 1, start);
        assert_eq!(mono, [0.0; 3]);
        let mut out = vec![0.0; 6];
        state.fill(&mut out, 2, start);
        assert_eq!(
            out.iter().map(|&s| level(s)).collect::<Vec<_>>(),
            [5, -5, 6, -6, 7, -7]
        );
    }

    #[test]
    fn the_speakers_skip_ahead_or_wait_to_stay_on_the_clock() {
        let (mut state, start) = state(400, 1000);
        let mut out = vec![0.0; 4];
        // the speakers opened a tenth of a second after the clock started
        state.fill(&mut out, 2, start + Duration::from_millis(100));
        assert_eq!(level(out[0]), 100);
        // a seek landed a little after where the clock now stands
        state.clock = Some((state.head - 0.05, start));
        let mut out = vec![0.0; 120];
        state.fill(&mut out, 2, start);
        let first = out
            .iter()
            .position(|&s| s != 0.0)
            .expect("sound after the wait");
        assert_eq!((first / 2, level(out[first])), (50, 102));
        state.clock = None;
        let mut out = vec![0.0; 4];
        state.fill(&mut out, 2, start);
        assert_eq!(out, [0.0; 4]);
    }

    #[test]
    fn a_rate_keeps_the_pitch_unless_told_not_to() {
        let tone = |frames: usize| -> Vec<f32> {
            (0..frames)
                .map(|n| (std::f64::consts::TAU * 300.0 * n as f64 / 48000.0).sin() as f32)
                .collect()
        };
        for (rate, preserves_pitch, pitch) in [
            (2.0, true, 300.0),
            (1.5, true, 300.0),
            (0.5, true, 300.0),
            (0.25, true, 300.0),
            (2.0, false, 600.0),
        ] {
            let start = Instant::now();
            let mut state = SoundState {
                opened: Some(Ok(AudioInfo {
                    rate: 48000,
                    channels: 1,
                    duration: 1.0,
                })),
                queue: tone(48000).into(),
                ended: true,
                clock: Some((0.0, start)),
                rate,
                gain: 1.0,
                output_rate: 48000,
                preserves_pitch,
                ..SoundState::default()
            };
            let mut heard = Vec::new();
            for callback in 0..30 {
                let mut out = vec![0.0; 480];
                state.fill(&mut out, 1, start + Duration::from_millis(10 * callback));
                heard.extend(out);
            }
            let steady = &heard[4800..];
            let crossings = steady
                .windows(2)
                .filter(|pair| pair[0] < 0.0 && pair[1] >= 0.0)
                .count() as f64;
            let heard_pitch = crossings / (steady.len() as f64 / 48000.0);
            assert!(
                (heard_pitch - pitch).abs() < 10.0,
                "{rate}x heard at {heard_pitch} Hz"
            );
            let quietest = steady
                .as_chunks::<480>()
                .0
                .iter()
                .map(|chunk| (chunk.iter().map(|s| s * s).sum::<f32>() / 480.0).sqrt())
                .fold(f32::MAX, f32::min);
            assert!(quietest > 0.65, "{rate}x dipped to {quietest}");
            let due = 0.3 * rate;
            let playing = state.playing(1, 48000.0);
            assert!(
                (playing - due).abs() < 0.02,
                "{rate}x at {playing}, due {due}"
            );
        }
    }

    #[test]
    fn a_sound_seeks_and_reads_ahead_on_its_thread() {
        let sound = Sound::open(None, bytes(counting(20000, 8000)), None);
        let wait = |test: &dyn Fn(&SoundState) -> bool| {
            for _ in 0..500 {
                if test(&sound.lock()) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            panic!("the sound thread never got there");
        };
        wait(&|state| state.ready(0));
        assert_eq!(
            sound.lock().opened,
            Some(Ok(AudioInfo {
                rate: 8000,
                channels: 2,
                duration: 2.5
            }))
        );
        wait(&|state| state.queue.len() >= 16000);
        sound.seek(1, 2.0);
        wait(&|state| state.ready(1));
        let state = sound.lock();
        assert_eq!((state.head, level(state.queue[0])), (2.0, 16000));
        drop(state);
        wait(&|state| state.ended);
        assert_eq!(sound.lock().queue.len(), 8000);
        let missing = Sound::open(None, bytes(b"not sound".to_vec()), None);
        for _ in 0..500 {
            if missing.lock().opened.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(matches!(missing.lock().opened, Some(Err(_))));
    }
}
