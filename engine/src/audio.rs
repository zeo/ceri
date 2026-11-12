// the sound of a page's audio contexts: voices script started, mixed on a device thread that
// pulls frames, so a busy script thread never starves the speakers

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::bridge::Waker;

// the speakers a context plays through; dropping the output closes them
pub trait AudioDevice {
    // a rate of none takes the device's own; frames come from render as the device asks
    fn open(
        &self,
        sample_rate: Option<u32>,
        render: AudioRender,
    ) -> Result<Box<dyn AudioOutput>, String>;
}

pub trait AudioOutput {
    fn sample_rate(&self) -> u32;
}

#[derive(Clone, Default)]
pub struct AudioRender(Arc<Mutex<Mix>>);

#[derive(Default)]
struct Mix {
    // frames rendered while running, the context's clock
    frames: u64,
    running: bool,
    voices: Vec<Voice>,
    ended: Vec<u32>,
    waker: Option<Waker>,
    // a media element's sound, which fills the frames in place of the voices
    feed: Option<Feed>,
}

pub(crate) type Feed = Arc<dyn Fn(&mut [f32], usize) + Send + Sync>;

// a buffer source playing: where it reads, how fast, and how loud it reaches the speakers
pub(crate) struct Voice {
    pub(crate) id: u32,
    pub(crate) channels: Vec<Arc<[f32]>>,
    // the context frame it starts sounding at, and the one it is cut at
    pub(crate) start: u64,
    pub(crate) stop: Option<u64>,
    // in buffer frames: the next one read, the one it ends before, and a looped stretch
    pub(crate) position: f64,
    pub(crate) end: f64,
    pub(crate) looped: Option<(f64, f64)>,
    // buffer frames per context frame
    pub(crate) step: f64,
    pub(crate) gain: f32,
}

impl AudioRender {
    fn mix(&self) -> MutexGuard<'_, Mix> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    // fills interleaved frames for the device; a suspended context gives silence and stands still
    pub fn render(&self, out: &mut [f32], channels: usize) {
        out.fill(0.0);
        if channels == 0 {
            return;
        }
        let mut mix = self.mix();
        if !mix.running {
            return;
        }
        if let Some(feed) = mix.feed.clone() {
            drop(mix);
            feed(out, channels);
            return;
        }
        let base = mix.frames;
        let Mix { voices, ended, .. } = &mut *mix;
        let before = ended.len();
        voices.retain_mut(|voice| {
            let sounding = voice.play(out, channels, base);
            if !sounding {
                ended.push(voice.id);
            }
            sounding
        });
        let finished = ended.len() > before;
        mix.frames += (out.len() / channels) as u64;
        if finished && let Some(waker) = &mix.waker {
            waker();
        }
    }

    pub(crate) fn set_waker(&self, waker: Option<Waker>) {
        self.mix().waker = waker;
    }

    pub(crate) fn frames(&self) -> u64 {
        self.mix().frames
    }

    pub(crate) fn set_running(&self, running: bool) {
        self.mix().running = running;
    }

    pub(crate) fn set_feed(&self, feed: Feed) {
        self.mix().feed = Some(feed);
    }

    pub(crate) fn play(&self, voice: Voice) {
        let mut mix = self.mix();
        mix.voices.retain(|playing| playing.id != voice.id);
        mix.voices.push(voice);
    }

    pub(crate) fn update(&self, id: u32, change: impl FnOnce(&mut Voice)) {
        if let Some(voice) = self.mix().voices.iter_mut().find(|voice| voice.id == id) {
            change(voice);
        }
    }

    pub(crate) fn take_ended(&self) -> Vec<u32> {
        std::mem::take(&mut self.mix().ended)
    }

    // ends every voice at once, as closing the context does
    pub(crate) fn silence(&self) -> Vec<u32> {
        let mut mix = self.mix();
        let mut ended = std::mem::take(&mut mix.ended);
        ended.extend(mix.voices.drain(..).map(|voice| voice.id));
        ended
    }
}

impl Voice {
    // adds this voice's frames from the context frame base on; false once it has finished
    fn play(&mut self, out: &mut [f32], channels: usize, base: u64) -> bool {
        for (index, frame) in out.chunks_exact_mut(channels).enumerate() {
            let now = base + index as u64;
            if now < self.start {
                continue;
            }
            if self.stop.is_some_and(|stop| now >= stop) {
                return false;
            }
            match self.looped {
                Some((from, to)) => {
                    let span = to - from;
                    if self.position >= to {
                        self.position = from + (self.position - to) % span;
                    } else if self.position < from && self.step < 0.0 {
                        self.position = to - (from - self.position) % span;
                    }
                }
                None if self.position >= self.end || self.position < 0.0 => return false,
                None => {}
            }
            let whole = self.position.floor();
            let at = whole as usize;
            let fraction = (self.position - whole) as f32;
            let read = |data: &[f32]| {
                let here = data.get(at).copied().unwrap_or(0.0);
                let next = data.get(at + 1).copied().unwrap_or(0.0);
                here + (next - here) * fraction
            };
            let sources = self.channels.len();
            for (channel, sample) in frame.iter_mut().enumerate() {
                // mono spreads to every speaker, stereo folds into one, the rest map in order
                let value = match (sources, channels) {
                    (1, _) => read(&self.channels[0]),
                    (2, 1) => 0.5 * (read(&self.channels[0]) + read(&self.channels[1])),
                    _ if channel < sources => read(&self.channels[channel]),
                    _ => 0.0,
                };
                *sample += value * self.gain;
            }
            self.position += self.step;
        }
        true
    }
}

// keeps time for a context with no speakers to pull its frames
pub(crate) struct Clock {
    rate: u32,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

const TICK: Duration = Duration::from_millis(10);

impl Clock {
    pub(crate) fn start(rate: u32, render: AudioRender) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let thread = std::thread::Builder::new()
            .name("ceri-audio-clock".to_string())
            .spawn(move || {
                let began = Instant::now();
                let mut done = 0u64;
                let mut scratch = Vec::new();
                while !stopped.load(Ordering::Acquire) {
                    std::thread::park_timeout(TICK);
                    let due = (began.elapsed().as_secs_f64() * f64::from(rate)) as u64;
                    scratch.resize(2 * (due - done) as usize, 0.0);
                    render.render(&mut scratch, 2);
                    done = due;
                }
            })
            .ok();
        Self { rate, stop, thread }
    }
}

impl AudioOutput for Clock {
    fn sample_rate(&self) -> u32 {
        self.rate
    }
}

impl Drop for Clock {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            thread.thread().unpark();
            let _ = thread.join();
        }
    }
}

// a WAVE fmt chunk's layout: frames interleave channels samples of width bytes each
#[derive(Clone, Copy)]
pub(crate) struct WaveFormat {
    pub(crate) channels: usize,
    pub(crate) rate: u32,
    pub(crate) width: usize,
    pub(crate) sample: fn(&[u8]) -> f32,
}

// integer or float PCM, the formats a WAVE file plays without a codec
pub(crate) fn wave_format(format: &[u8]) -> Option<WaveFormat> {
    let field = |at: usize| Some(u16::from_le_bytes(*format.get(at..)?.first_chunk::<2>()?));
    let mut tag = field(0)?;
    let channels = usize::from(field(2)?);
    let rate = u32::from_le_bytes(*format.get(4..)?.first_chunk::<4>()?);
    let bits = field(14)?;
    // an extensible header carries the real format at the start of its subformat guid
    if tag == 0xfffe {
        tag = field(24)?;
    }
    let width = usize::from(bits.div_ceil(8));
    if channels == 0 || rate == 0 || width == 0 {
        return None;
    }
    let sample: fn(&[u8]) -> f32 = match (tag, bits) {
        (1, 8) => |b| (f32::from(b[0]) - 128.0) / 128.0,
        (1, 16) => |b| f32::from(i16::from_le_bytes([b[0], b[1]])) / 32768.0,
        (1, 24) => |b| (i32::from_le_bytes([0, b[0], b[1], b[2]]) >> 8) as f32 / 8_388_608.0,
        (1, 32) => |b| i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f32 / 2_147_483_648.0,
        (3, 32) => |b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]),
        (3, 64) => |b| f64::from_le_bytes(b[..8].try_into().unwrap_or_default()) as f32,
        _ => return None,
    };
    Some(WaveFormat {
        channels,
        rate,
        width,
        sample,
    })
}

// reads a RIFF WAVE file of integer or float PCM into one sample list per channel at rate
pub(crate) fn decode(bytes: &[u8], rate: u32) -> Option<Vec<Vec<f32>>> {
    let body = bytes
        .strip_prefix(b"RIFF")?
        .get(4..)?
        .strip_prefix(b"WAVE")?;
    let mut chunks = body;
    let mut format = None;
    let mut data = None;
    while let Some((id, rest)) = chunks.split_first_chunk::<4>() {
        let (size, rest) = rest.split_first_chunk::<4>()?;
        let size = u32::from_le_bytes(*size) as usize;
        // a writer that never came back to fill in the size leaves the data running to the end
        let content = rest.get(..size).unwrap_or(rest);
        match id {
            b"fmt " => format = Some(content),
            b"data" => data = Some(content),
            _ => {}
        }
        chunks = rest.get(size + (size & 1)..).unwrap_or_default();
    }
    let WaveFormat {
        channels,
        rate: source_rate,
        width,
        sample,
    } = wave_format(format?)?;
    let mut decoded = vec![Vec::new(); channels];
    for frame in data?.chunks_exact(width * channels) {
        for (channel, bytes) in decoded.iter_mut().zip(frame.chunks_exact(width)) {
            channel.push(sample(bytes));
        }
    }
    if source_rate != rate {
        for channel in &mut decoded {
            *channel = resample(channel, source_rate, rate);
        }
    }
    Some(decoded)
}

// band-limited resampling with a Blackman-windowed sinc
fn resample(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    const ZEROS: f64 = 16.0;
    let ratio = f64::from(to) / f64::from(from);
    let cutoff = ratio.min(1.0);
    let reach = ZEROS / cutoff;
    let length = (input.len() as f64 * ratio).round() as usize;
    (0..length)
        .map(|index| {
            let center = index as f64 / ratio;
            let first = (center - reach).ceil().max(0.0) as usize;
            let last = ((center + reach).floor() as usize).min(input.len().saturating_sub(1));
            let mut sum = 0.0;
            for (offset, &sample) in input
                .get(first..=last)
                .unwrap_or_default()
                .iter()
                .enumerate()
            {
                let x = (first + offset) as f64 - center;
                let phase = std::f64::consts::PI * x * cutoff;
                let sinc = if phase == 0.0 {
                    1.0
                } else {
                    phase.sin() / phase
                };
                let w = std::f64::consts::PI * (x / reach + 1.0);
                let window = 0.42 - 0.5 * w.cos() + 0.08 * (2.0 * w).cos();
                sum += f64::from(sample) * sinc * window * cutoff;
            }
            sum as f32
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wav(tag: u16, bits: u16, channels: u16, rate: u32, data: &[u8]) -> Vec<u8> {
        let mut bytes = b"RIFF\0\0\0\0WAVEfmt \x10\0\0\0".to_vec();
        let block = channels * bits / 8;
        bytes.extend(tag.to_le_bytes());
        bytes.extend(channels.to_le_bytes());
        bytes.extend(rate.to_le_bytes());
        bytes.extend((rate * u32::from(block)).to_le_bytes());
        bytes.extend(block.to_le_bytes());
        bytes.extend(bits.to_le_bytes());
        bytes.extend(b"LIST\x03\0\0\0abc\0");
        bytes.extend(b"data");
        bytes.extend((data.len() as u32).to_le_bytes());
        bytes.extend(data);
        bytes
    }

    #[test]
    fn wave_files_decode_to_float_channels_at_the_context_rate() {
        let pcm: Vec<u8> = [0i16, -32768, 16384, 32767]
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .collect();
        let decoded = decode(&wav(1, 16, 2, 48_000, &pcm), 48_000).expect("pcm should decode");
        assert_eq!(decoded, [vec![0.0, 0.5], vec![-1.0, 32767.0 / 32768.0]]);
        let float: Vec<u8> = [0.25f32, -0.75]
            .iter()
            .flat_map(|s| s.to_le_bytes())
            .collect();
        let decoded = decode(&wav(3, 32, 1, 8_000, &float), 8_000).expect("float should decode");
        assert_eq!(decoded, [vec![0.25, -0.75]]);
        let one_second: Vec<u8> = (0..44_100)
            .flat_map(|n| {
                ((n as f32 * 440.0 * std::f32::consts::TAU / 44_100.0).sin() * 0.5).to_le_bytes()
            })
            .collect();
        let decoded =
            decode(&wav(3, 32, 1, 44_100, &one_second), 48_000).expect("resampled should decode");
        let channel = &decoded[0];
        assert_eq!(channel.len(), 48_000);
        let expected = |n: usize| (n as f32 * 440.0 * std::f32::consts::TAU / 48_000.0).sin() * 0.5;
        assert!((1000..47_000).all(|n| (channel[n] - expected(n)).abs() < 0.002));
        assert!(decode(b"RIFF\0\0\0\0WAVEdata\0\0\0\0", 48_000).is_none());
        assert!(decode(b"ID3\x04 not a wave file", 48_000).is_none());
    }

    #[test]
    fn voices_mix_from_their_start_frame_at_their_rate_and_gain_until_they_end() {
        let render = AudioRender::default();
        render.set_running(true);
        let ramp: Arc<[f32]> = (0..8).map(|n| n as f32).collect();
        render.play(Voice {
            id: 7,
            channels: vec![ramp],
            start: 2,
            stop: None,
            position: 1.0,
            end: 8.0,
            looped: None,
            step: 1.5,
            gain: 0.5,
        });
        let mut out = vec![0.0; 12];
        render.render(&mut out, 2);
        assert_eq!(
            out,
            [
                0.0, 0.0, 0.0, 0.0, 0.5, 0.5, 1.25, 1.25, 2.0, 2.0, 2.75, 2.75
            ]
        );
        assert!(render.take_ended().is_empty());
        render.render(&mut out, 1);
        assert_eq!(&out[..2], [3.5, 0.0]);
        assert_eq!(render.take_ended(), [7]);
        assert_eq!(render.frames(), 18);
        render.set_running(false);
        render.render(&mut out, 2);
        assert_eq!(render.frames(), 18);
    }
}
