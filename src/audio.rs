// the speakers web audio plays through: a thread per context feeds the default output device,
// follows it when the user switches or unplugs it, and keeps time by the wall clock while
// there is none

use ceri_engine::{AudioDevice, AudioOutput, AudioRender};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

// how long the thread runs without speakers before it looks for them again
const RETRY: Duration = Duration::from_secs(1);
const TICK: Duration = Duration::from_millis(10);

#[derive(Default)]
pub(crate) struct DesktopAudio;

struct Speakers {
    rate: u32,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl AudioDevice for DesktopAudio {
    fn open(
        &self,
        sample_rate: Option<u32>,
        render: AudioRender,
    ) -> Result<Box<dyn AudioOutput>, String> {
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let (opened, opening) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("ceri-audio".to_string())
            .spawn(move || speakers::play(sample_rate, &render, &stopped, opened))
            .map_err(|error| error.to_string())?;
        match opening.recv() {
            Ok(Ok(rate)) => Ok(Box::new(Speakers {
                rate,
                stop,
                thread: Some(thread),
            })),
            Ok(Err(error)) => Err(error),
            Err(_) => Err("the audio thread stopped before it opened the device".to_string()),
        }
    }
}

impl AudioOutput for Speakers {
    fn sample_rate(&self) -> u32 {
        self.rate
    }
}

impl Drop for Speakers {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            thread.thread().unpark();
            let _ = thread.join();
        }
    }
}

// renders by the wall clock for a while, so the context's time moves on with no device
fn pace(render: &AudioRender, rate: u32, stop: &AtomicBool, span: Duration) {
    let began = Instant::now();
    let mut done = 0u64;
    let mut scratch = Vec::new();
    while !stop.load(Ordering::Acquire) && began.elapsed() < span {
        std::thread::park_timeout(TICK);
        let due = (began.elapsed().as_secs_f64() * f64::from(rate)) as u64;
        scratch.resize(2 * (due - done) as usize, 0.0);
        render.render(&mut scratch, 2);
        done = due;
    }
}

#[cfg(target_os = "windows")]
mod speakers {
    use super::{AtomicBool, AudioRender, Instant, Ordering, RETRY, Sender, pace};
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Media::Audio::{
        AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
        AUDCLNT_STREAMFLAGS_EVENTCALLBACK, AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, IAudioClient,
        IAudioRenderClient, IMMDeviceEnumerator, MMDeviceEnumerator, WAVEFORMATEX, eConsole,
        eRender,
    };
    use windows::Win32::System::Com::{
        CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
        CoUninitialize,
    };
    use windows::Win32::System::Threading::{
        AvRevertMmThreadCharacteristics, AvSetMmThreadCharacteristicsW, CreateEventW,
        WaitForSingleObject,
    };
    use windows::core::{Result, w};

    const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;

    struct Event(HANDLE);

    impl Drop for Event {
        fn drop(&mut self) {
            // SAFETY: the handle came from CreateEventW and nothing uses it after this
            let _ = unsafe { CloseHandle(self.0) };
        }
    }

    // a shared-mode stream of float stereo the audio engine converts to the device's format
    struct Stream {
        client: IAudioClient,
        output: IAudioRenderClient,
        frames: u32,
        device: String,
    }

    fn default_device(devices: &IMMDeviceEnumerator) -> Result<String> {
        // SAFETY: GetId hands over a CoTaskMemAlloc string, freed once it is copied
        unsafe {
            let id = devices
                .GetDefaultAudioEndpoint(eRender, eConsole)?
                .GetId()?;
            let text = id.to_string().unwrap_or_default();
            CoTaskMemFree(Some(id.0 as _));
            Ok(text)
        }
    }

    impl Stream {
        fn open(
            devices: &IMMDeviceEnumerator,
            event: &Event,
            rate: Option<u32>,
            render: &AudioRender,
        ) -> Result<(Self, u32)> {
            // SAFETY: the calls follow WASAPI's documented order, initialize before the rest,
            // and the mix format is freed with the allocator that made it
            unsafe {
                let device = devices.GetDefaultAudioEndpoint(eRender, eConsole)?;
                let id = device.GetId()?;
                let name = id.to_string().unwrap_or_default();
                CoTaskMemFree(Some(id.0 as _));
                let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
                let rate = match rate {
                    Some(rate) => rate,
                    None => {
                        let mix = client.GetMixFormat()?;
                        let rate = (*mix).nSamplesPerSec;
                        CoTaskMemFree(Some(mix as _));
                        rate
                    }
                };
                let format = WAVEFORMATEX {
                    wFormatTag: WAVE_FORMAT_IEEE_FLOAT,
                    nChannels: 2,
                    nSamplesPerSec: rate,
                    nAvgBytesPerSec: rate * 8,
                    nBlockAlign: 8,
                    wBitsPerSample: 32,
                    cbSize: 0,
                };
                client.Initialize(
                    AUDCLNT_SHAREMODE_SHARED,
                    AUDCLNT_STREAMFLAGS_EVENTCALLBACK
                        | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
                        | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
                    0,
                    0,
                    &format,
                    None,
                )?;
                client.SetEventHandle(event.0)?;
                let stream = Self {
                    frames: client.GetBufferSize()?,
                    output: client.GetService()?,
                    client,
                    device: name,
                };
                stream.feed(render)?;
                stream.client.Start()?;
                Ok((stream, rate))
            }
        }

        fn feed(&self, render: &AudioRender) -> Result<()> {
            // SAFETY: GetBuffer lends room for free frames of 8 bytes each, which render fills
            // before ReleaseBuffer hands them back
            unsafe {
                let free = self.frames - self.client.GetCurrentPadding()?;
                if free == 0 {
                    return Ok(());
                }
                let data = self.output.GetBuffer(free)?;
                render.render(
                    std::slice::from_raw_parts_mut(data.cast::<f32>(), 2 * free as usize),
                    2,
                );
                self.output.ReleaseBuffer(free, 0)
            }
        }
    }

    impl Drop for Stream {
        fn drop(&mut self) {
            // SAFETY: stopping a started client has no preconditions
            let _ = unsafe { self.client.Stop() };
        }
    }

    pub(super) fn play(
        rate: Option<u32>,
        render: &AudioRender,
        stop: &AtomicBool,
        opened: Sender<std::result::Result<u32, String>>,
    ) {
        // SAFETY: the thread is new, so this is its first COM initialization
        if let Err(error) = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.ok() {
            let _ = opened.send(Err(error.message()));
            return;
        }
        run(rate, render, stop, opened);
        // SAFETY: balances the initialization above, after run released every COM object
        unsafe { CoUninitialize() };
    }

    fn run(
        rate: Option<u32>,
        render: &AudioRender,
        stop: &AtomicBool,
        opened: Sender<std::result::Result<u32, String>>,
    ) {
        // SAFETY: plain object and event creation with no borrowed arguments
        let started = unsafe {
            CoCreateInstance::<_, IMMDeviceEnumerator>(&MMDeviceEnumerator, None, CLSCTX_ALL)
                .and_then(|devices| Ok((devices, Event(CreateEventW(None, false, false, None)?))))
        };
        let (devices, event) = match started {
            Ok(started) => started,
            Err(error) => {
                let _ = opened.send(Err(error.message()));
                return;
            }
        };
        let (stream, rate) = match Stream::open(&devices, &event, rate, render) {
            Ok(opened) => opened,
            Err(error) => {
                let _ = opened.send(Err(error.message()));
                return;
            }
        };
        let _ = opened.send(Ok(rate));
        let mut stream = Some(stream);
        let mut task = 0;
        // SAFETY: registers this thread with the multimedia scheduler until the revert below
        let priority = unsafe { AvSetMmThreadCharacteristicsW(w!("Pro Audio"), &mut task) }.ok();
        let mut checked = Instant::now();
        while !stop.load(Ordering::Acquire) {
            let playing = match &stream {
                Some(current) => {
                    // SAFETY: the event outlives the wait
                    unsafe { WaitForSingleObject(event.0, 200) };
                    let moved = checked.elapsed() >= RETRY && {
                        checked = Instant::now();
                        default_device(&devices).is_ok_and(|id| id != current.device)
                    };
                    !moved && current.feed(render).is_ok()
                }
                None => {
                    pace(render, rate, stop, RETRY);
                    false
                }
            };
            if !playing && !stop.load(Ordering::Acquire) {
                drop(stream.take());
                stream = Stream::open(&devices, &event, Some(rate), render)
                    .ok()
                    .map(|(stream, _)| stream);
            }
        }
        drop(stream);
        if let Some(priority) = priority {
            // SAFETY: the handle came from AvSetMmThreadCharacteristicsW on this thread
            let _ = unsafe { AvRevertMmThreadCharacteristics(priority) };
        }
    }
}

#[cfg(not(target_os = "windows"))]
mod speakers {
    use super::{AtomicBool, AudioRender, Ordering, RETRY, Sender, pace};
    use std::ffi::{c_char, c_int, c_void};
    use std::sync::OnceLock;

    const RTLD_NOW: c_int = 2;
    const PA_STREAM_PLAYBACK: c_int = 1;
    const PA_SAMPLE_FLOAT32LE: c_int = 5;

    #[repr(C)]
    struct SampleSpec {
        format: c_int,
        rate: u32,
        channels: u8,
    }

    #[repr(C)]
    struct BufferAttr {
        maxlength: u32,
        tlength: u32,
        prebuf: u32,
        minreq: u32,
        fragsize: u32,
    }

    type New = unsafe extern "C" fn(
        *const c_char,
        *const c_char,
        c_int,
        *const c_char,
        *const c_char,
        *const SampleSpec,
        *const c_void,
        *const BufferAttr,
        *mut c_int,
    ) -> *mut c_void;
    type Write = unsafe extern "C" fn(*mut c_void, *const c_void, usize, *mut c_int) -> c_int;
    type Free = unsafe extern "C" fn(*mut c_void);

    unsafe extern "C" {
        fn dlopen(name: *const c_char, flags: c_int) -> *mut c_void;
        fn dlsym(library: *mut c_void, name: *const c_char) -> *mut c_void;
    }

    // libpulse-simple, loaded at first use so a system without it still runs, silently;
    // pipewire serves the same api
    struct Pulse {
        new: New,
        write: Write,
        free: Free,
    }

    fn pulse() -> Option<&'static Pulse> {
        static PULSE: OnceLock<Option<Pulse>> = OnceLock::new();
        PULSE
            .get_or_init(|| {
                // SAFETY: the library stays loaded for the life of the process, and each symbol
                // is cast to the signature simple.h declares for it
                unsafe {
                    let library = dlopen(c"libpulse-simple.so.0".as_ptr(), RTLD_NOW);
                    if library.is_null() {
                        return None;
                    }
                    let symbol = |name: &std::ffi::CStr| {
                        let symbol = dlsym(library, name.as_ptr());
                        (!symbol.is_null()).then_some(symbol)
                    };
                    Some(Pulse {
                        new: std::mem::transmute::<*mut c_void, New>(symbol(c"pa_simple_new")?),
                        write: std::mem::transmute::<*mut c_void, Write>(symbol(
                            c"pa_simple_write",
                        )?),
                        free: std::mem::transmute::<*mut c_void, Free>(symbol(c"pa_simple_free")?),
                    })
                }
            })
            .as_ref()
    }

    struct Stream {
        pulse: &'static Pulse,
        handle: *mut c_void,
    }

    impl Stream {
        fn open(rate: u32) -> Option<Self> {
            let pulse = pulse()?;
            let name = std::env::current_exe()
                .ok()
                .and_then(|path| {
                    path.file_stem()
                        .map(|stem| stem.to_string_lossy().into_owned())
                })
                .and_then(|stem| std::ffi::CString::new(stem).ok())
                .unwrap_or_else(|| c"ceri".into());
            let spec = SampleSpec {
                format: PA_SAMPLE_FLOAT32LE,
                rate,
                channels: 2,
            };
            let frame_bytes = |milliseconds: u32| rate / 1000 * milliseconds * 8;
            let buffer = BufferAttr {
                maxlength: u32::MAX,
                tlength: frame_bytes(40),
                prebuf: u32::MAX,
                minreq: frame_bytes(10),
                fragsize: u32::MAX,
            };
            let mut error = 0;
            // SAFETY: every pointer refers to a live value or is null where simple.h allows it
            let handle = unsafe {
                (pulse.new)(
                    std::ptr::null(),
                    name.as_ptr(),
                    PA_STREAM_PLAYBACK,
                    std::ptr::null(),
                    c"playback".as_ptr(),
                    &spec,
                    std::ptr::null(),
                    &buffer,
                    &mut error,
                )
            };
            (!handle.is_null()).then_some(Self { pulse, handle })
        }

        // blocks until the server has room, which is what paces the thread
        fn write(&self, samples: &[f32]) -> bool {
            let mut error = 0;
            // SAFETY: the handle is open and the slice is valid for its byte length
            unsafe {
                (self.pulse.write)(
                    self.handle,
                    samples.as_ptr().cast(),
                    std::mem::size_of_val(samples),
                    &mut error,
                ) >= 0
            }
        }
    }

    impl Drop for Stream {
        fn drop(&mut self) {
            // SAFETY: the handle came from pa_simple_new and is freed once
            unsafe { (self.pulse.free)(self.handle) };
        }
    }

    pub(super) fn play(
        rate: Option<u32>,
        render: &AudioRender,
        stop: &AtomicBool,
        opened: Sender<Result<u32, String>>,
    ) {
        let rate = rate.unwrap_or(48_000);
        let Some(stream) = Stream::open(rate) else {
            let _ = opened.send(Err("no PulseAudio or PipeWire server".to_string()));
            return;
        };
        let _ = opened.send(Ok(rate));
        let mut stream = Some(stream);
        let mut chunk = vec![0.0; 2 * (rate as usize / 100).max(1)];
        while !stop.load(Ordering::Acquire) {
            let playing = match &stream {
                Some(current) => {
                    render.render(&mut chunk, 2);
                    current.write(&chunk)
                }
                None => {
                    pace(render, rate, stop, RETRY);
                    false
                }
            };
            if !playing && !stop.load(Ordering::Acquire) {
                drop(stream.take());
                stream = Stream::open(rate);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::DesktopAudio;
    use ceri_engine::{Browser, ResourceBundle};
    use std::rc::Rc;
    use std::time::Duration;

    #[test]
    fn a_context_keeps_time_on_the_speakers_or_by_the_clock_without_them() {
        let mut resources = ResourceBundle::default();
        resources
            .insert(
                "/index.html",
                "text/html",
                b"<script>
                    const context = new AudioContext();
                    console.log(context.sampleRate + ' ' + context.state);
                    setTimeout(() => {
                        console.log(String(context.currentTime));
                        context.close().then(() => console.log(context.state));
                    }, 300);
                </script>"
                    .to_vec(),
            )
            .expect("the page should be accepted");
        let mut browser = Browser::new(resources).with_audio_device(Rc::new(DesktopAudio));
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("the page should load");
        std::thread::sleep(Duration::from_millis(300));
        browser.advance_time(300).expect("the timer should run");
        let logs = browser.console_logs();
        let (rate, state) = logs[0].split_once(' ').expect("rate and state");
        assert!((3000..=768_000).contains(&rate.parse::<u32>().expect("a rate")));
        assert_eq!(state, "running");
        let elapsed: f64 = logs[1].parse().expect("a time");
        assert!((0.15..2.0).contains(&elapsed), "{elapsed}");
        assert_eq!(logs[2], "closed");
    }
}
