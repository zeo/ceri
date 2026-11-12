// the video decoders the desktop already has: media foundation on windows, gstreamer on linux;
// the engine runs each reader on a media element's own decode thread

use ceri_engine::{MediaDecoder, MediaInfo, MediaSource, VideoFrame, VideoReader};

#[derive(Default)]
pub(crate) struct DesktopVideo;

impl MediaDecoder for DesktopVideo {
    fn open(&self, source: &MediaSource) -> Result<Box<dyn VideoReader>, String> {
        Ok(Box::new(platform::Reader::open(source)?))
    }

    fn can_play(&self, mime_type: &str) -> &'static str {
        let essence = mime_type.split(';').next().unwrap_or_default().trim();
        if platform::TYPES
            .iter()
            .any(|known| known.eq_ignore_ascii_case(essence))
        {
            "maybe"
        } else {
            ""
        }
    }
}

#[cfg(target_os = "windows")]
mod platform {
    use super::{MediaInfo, MediaSource, VideoFrame, VideoReader, edit_shift};
    use std::os::windows::ffi::OsStrExt;
    use std::sync::OnceLock;
    use windows::Win32::Media::MediaFoundation::{
        IMF2DBuffer, IMFAttributes, IMFMediaBuffer, IMFMediaType, IMFSourceReader,
        MF_BYTESTREAM_CONTENT_TYPE, MF_MT_DEFAULT_STRIDE, MF_MT_FRAME_SIZE, MF_MT_MAJOR_TYPE,
        MF_MT_MINIMUM_DISPLAY_APERTURE, MF_MT_PIXEL_ASPECT_RATIO, MF_MT_SUBTYPE, MF_PD_DURATION,
        MF_SOURCE_READER_ALL_STREAMS, MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING,
        MF_SOURCE_READER_FIRST_VIDEO_STREAM, MF_SOURCE_READER_MEDIASOURCE,
        MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED, MF_SOURCE_READERF_ENDOFSTREAM,
        MF_SOURCE_READERF_ERROR, MF_VERSION, MFCreateAttributes, MFCreateMFByteStreamOnStream,
        MFCreateMediaType, MFCreateSourceReaderFromByteStream, MFCreateSourceReaderFromURL,
        MFMediaType_Video, MFSTARTUP_LITE, MFStartup, MFVideoArea, MFVideoFormat_RGB32,
    };
    use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
    use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
    use windows::Win32::System::Variant::{VT_I8, VT_UI8};
    use windows::Win32::UI::Shell::SHCreateMemStream;
    use windows::core::{GUID, Interface, PCWSTR};

    pub(super) const TYPES: &[&str] = &["video/mp4", "video/webm", "video/x-matroska"];

    const VIDEO: u32 = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
    // media foundation keeps time in units of 100 nanoseconds
    const TICKS: f64 = 10_000_000.0;

    pub(super) struct Reader {
        reader: IMFSourceReader,
        info: MediaInfo,
        // the decoded buffer's size and row stride, and the part of it that is picture
        coded: (u32, u32),
        stride: i32,
        picture: Area,
        // seconds the mp4 edit list moves pictures by
        shift: f64,
        // declared last so the reader is released before the apartment closes
        _apartment: Apartment,
    }

    // SAFETY: the engine opens, reads and drops a reader on one decode thread, the apartment
    // open joined
    unsafe impl Send for Reader {}

    struct Apartment;

    impl Drop for Apartment {
        fn drop(&mut self) {
            // SAFETY: balances the CoInitializeEx in Reader::open on this same thread
            unsafe { CoUninitialize() };
        }
    }

    fn text(error: windows::core::Error) -> String {
        error.message()
    }

    fn wide(text: &std::ffi::OsStr) -> Vec<u16> {
        text.encode_wide().chain([0]).collect()
    }

    fn started() -> Result<(), String> {
        static STARTED: OnceLock<Result<(), String>> = OnceLock::new();
        STARTED
            // SAFETY: media foundation stays started for the life of the process
            .get_or_init(|| unsafe { MFStartup(MF_VERSION, MFSTARTUP_LITE) }.map_err(text))
            .clone()
    }

    fn split(packed: u64) -> (u32, u32) {
        ((packed >> 32) as u32, packed as u32)
    }

    #[derive(Clone, Copy)]
    struct Area {
        left: u32,
        top: u32,
        width: u32,
        height: u32,
    }

    // the visible part of a frame, which codecs padding to whole macroblocks keep smaller
    // than the coded size, as 1080 rows of a 1088 row h.264 picture
    unsafe fn picture(kind: &IMFMediaType) -> Result<(Area, (u32, u32)), String> {
        unsafe {
            let coded = split(kind.GetUINT64(&MF_MT_FRAME_SIZE).map_err(text)?);
            let mut aperture = MFVideoArea::default();
            let described = kind
                .GetBlob(
                    &MF_MT_MINIMUM_DISPLAY_APERTURE,
                    std::slice::from_raw_parts_mut(
                        (&raw mut aperture).cast::<u8>(),
                        size_of::<MFVideoArea>(),
                    ),
                    None,
                )
                .is_ok();
            let area = Area {
                left: aperture.OffsetX.value.max(0) as u32,
                top: aperture.OffsetY.value.max(0) as u32,
                width: aperture.Area.cx.max(0) as u32,
                height: aperture.Area.cy.max(0) as u32,
            };
            let inside = area.width > 0
                && area.height > 0
                && area.left + area.width <= coded.0
                && area.top + area.height <= coded.1;
            let whole = Area {
                left: 0,
                top: 0,
                width: coded.0,
                height: coded.1,
            };
            Ok((if described && inside { area } else { whole }, coded))
        }
    }

    // chromium's natural size: anamorphic pixels widen the picture, or make it taller
    pub(super) fn natural_size(width: u32, height: u32, aspect: (u32, u32)) -> (u32, u32) {
        let (across, down) = aspect;
        if across == 0 || down == 0 || across == down {
            (width, height)
        } else if across > down {
            (
                (u64::from(width) * u64::from(across) / u64::from(down)) as u32,
                height,
            )
        } else {
            (
                width,
                (u64::from(height) * u64::from(down) / u64::from(across)) as u32,
            )
        }
    }

    impl Reader {
        pub(super) fn open(source: &MediaSource) -> Result<Self, String> {
            // SAFETY: the decode thread is new, so this is its first COM initialization
            unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }
                .ok()
                .map_err(text)?;
            let apartment = Apartment;
            started()?;
            // SAFETY: every call below follows the source reader's documented contract, on
            // interfaces this function created and still holds
            unsafe {
                let mut attributes = None;
                MFCreateAttributes(&mut attributes, 1).map_err(text)?;
                let attributes: IMFAttributes =
                    attributes.ok_or("media foundation gave no attribute store")?;
                attributes
                    // the advanced processor would also resample to the frame rate the
                    // container claims, which webm files often get wrong
                    .SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1)
                    .map_err(text)?;
                let reader = match source {
                    MediaSource::File(path) => {
                        let path = wide(path.as_os_str());
                        MFCreateSourceReaderFromURL(PCWSTR(path.as_ptr()), &attributes)
                    }
                    MediaSource::Bytes { body, mime_type } => {
                        let stream =
                            SHCreateMemStream(Some(body)).ok_or("could not hold the media")?;
                        let bytes = MFCreateMFByteStreamOnStream(&stream).map_err(text)?;
                        let mime_type = wide(mime_type.as_ref());
                        bytes
                            .cast::<IMFAttributes>()
                            .and_then(|described| {
                                described.SetString(
                                    &MF_BYTESTREAM_CONTENT_TYPE,
                                    PCWSTR(mime_type.as_ptr()),
                                )
                            })
                            .map_err(text)?;
                        MFCreateSourceReaderFromByteStream(&bytes, &attributes)
                    }
                }
                .map_err(text)?;
                reader
                    .SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false)
                    .map_err(text)?;
                reader.SetStreamSelection(VIDEO, true).map_err(text)?;
                let native = reader
                    .GetNativeMediaType(VIDEO, 0)
                    .map_err(|_| "the media has no video stream".to_string())?;
                let (shown, _) = picture(&native)?;
                let aspect = native
                    .GetUINT64(&MF_MT_PIXEL_ASPECT_RATIO)
                    .map_or((1, 1), split);
                let (width, height) = natural_size(shown.width, shown.height, aspect);
                let duration = reader
                    .GetPresentationAttribute(
                        MF_SOURCE_READER_MEDIASOURCE.0 as u32,
                        &MF_PD_DURATION,
                    )
                    .ok()
                    .filter(|value| value.Anonymous.Anonymous.vt == VT_UI8)
                    .map_or(f64::NAN, |value| {
                        value.Anonymous.Anonymous.Anonymous.uhVal as f64 / TICKS
                    });
                let output = MFCreateMediaType().map_err(text)?;
                output
                    .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
                    .map_err(text)?;
                output
                    .SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32)
                    .map_err(text)?;
                reader
                    .SetCurrentMediaType(VIDEO, None, &output)
                    .map_err(text)?;
                let mut opened = Self {
                    reader,
                    info: MediaInfo {
                        duration,
                        width,
                        height,
                    },
                    coded: (0, 0),
                    stride: 0,
                    picture: shown,
                    shift: edit_shift(source).unwrap_or(0.0),
                    _apartment: apartment,
                };
                opened.take_format()?;
                Ok(opened)
            }
        }

        fn take_format(&mut self) -> Result<(), String> {
            // SAFETY: reads attributes of the type the reader now delivers
            unsafe {
                let current = self.reader.GetCurrentMediaType(VIDEO).map_err(text)?;
                (self.picture, self.coded) = picture(&current)?;
                self.stride = current
                    .GetUINT32(&MF_MT_DEFAULT_STRIDE)
                    .map_or(self.coded.0 as i32 * 4, |stride| stride as i32);
            }
            Ok(())
        }

        fn copy(&self, buffer: &IMFMediaBuffer) -> Result<Vec<u8>, String> {
            let Area {
                left,
                top,
                width,
                height,
            } = self.picture;
            let row = width as usize * 4;
            let (skip, rows) = (left as usize * 4, height as usize);
            let coded = (self.coded.0 as usize * 4, self.coded.1 as usize);
            let mut bgra = Vec::with_capacity(row * rows);
            // SAFETY: the lock pins the buffer while the rows are read, each row read lies
            // inside the pitch and length the lock reported, and every lock is unlocked
            unsafe {
                if let Ok(planar) = buffer.cast::<IMF2DBuffer>() {
                    let mut first = std::ptr::null_mut();
                    let mut pitch = 0;
                    planar.Lock2D(&mut first, &mut pitch).map_err(text)?;
                    if pitch.unsigned_abs() as usize >= coded.0 {
                        for y in top as usize..top as usize + rows {
                            let start = first.offset(y as isize * pitch as isize).add(skip);
                            bgra.extend_from_slice(std::slice::from_raw_parts(start, row));
                        }
                    }
                    planar.Unlock2D().map_err(text)?;
                } else {
                    let mut start = std::ptr::null_mut();
                    let mut length = 0;
                    buffer
                        .Lock(&mut start, None, Some(&mut length))
                        .map_err(text)?;
                    let pitch = self.stride.unsigned_abs() as usize;
                    if pitch >= coded.0 && length as usize >= pitch * coded.1 {
                        let bytes = std::slice::from_raw_parts(start, length as usize);
                        for y in top as usize..top as usize + rows {
                            // a negative stride stores the bottom row first
                            let line = if self.stride < 0 { coded.1 - 1 - y } else { y };
                            bgra.extend_from_slice(&bytes[line * pitch + skip..][..row]);
                        }
                    }
                    buffer.Unlock().map_err(text)?;
                }
            }
            if bgra.is_empty() {
                return Err("the decoder gave a frame narrower than its picture".to_string());
            }
            Ok(bgra)
        }
    }

    impl VideoReader for Reader {
        fn info(&self) -> MediaInfo {
            self.info
        }

        fn seek(&mut self, seconds: f64) -> Result<(), String> {
            let mut position = PROPVARIANT::default();
            // SAFETY: fills the eight byte integer member that VT_I8 names
            unsafe {
                let value = &mut position.Anonymous.Anonymous;
                value.vt = VT_I8;
                value.Anonymous.hVal = ((seconds - self.shift).max(0.0) * TICKS) as i64;
                self.reader
                    .SetCurrentPosition(&GUID::zeroed(), &position)
                    .map_err(text)
            }
        }

        fn read(&mut self) -> Result<Option<VideoFrame>, String> {
            loop {
                let mut flags = 0;
                let mut time = 0;
                let mut sample = None;
                // SAFETY: a synchronous read into locals that outlive the call
                unsafe {
                    self.reader.ReadSample(
                        VIDEO,
                        0,
                        None,
                        Some(&mut flags),
                        Some(&mut time),
                        Some(&mut sample),
                    )
                }
                .map_err(text)?;
                let flags = flags as i32;
                if flags & MF_SOURCE_READERF_ERROR.0 != 0 {
                    return Err("the video stream failed".to_string());
                }
                if flags & MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED.0 != 0 {
                    self.take_format()?;
                }
                if let Some(sample) = sample {
                    // SAFETY: the sample came from the read above and is still held
                    let buffer = unsafe { sample.ConvertToContiguousBuffer() }.map_err(text)?;
                    return Ok(Some(VideoFrame {
                        time: time as f64 / TICKS + self.shift,
                        width: self.picture.width,
                        height: self.picture.height,
                        bgra: self.copy(&buffer)?,
                    }));
                }
                if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 != 0 {
                    return Ok(None);
                }
            }
        }
    }
}

// media foundation's mp4 source ignores the video track's edit list, which ffmpeg and chromium
// apply; an x264 file with b-frames would otherwise start its pictures two frames late
#[cfg(any(target_os = "windows", test))]
fn edit_shift(source: &MediaSource) -> Option<f64> {
    use std::io::{Cursor, Read, Seek, SeekFrom};

    // a moov this large is no movie header worth reading whole
    const LIMIT: u64 = 64 << 20;

    fn movie(mut file: impl Read + Seek) -> Option<Vec<u8>> {
        loop {
            let mut head = [0; 8];
            file.read_exact(&mut head).ok()?;
            let mut size = u64::from(u32::from_be_bytes(head[..4].try_into().ok()?));
            let mut header = 8;
            if size == 1 {
                let mut large = [0; 8];
                file.read_exact(&mut large).ok()?;
                size = u64::from_be_bytes(large);
                header = 16;
            }
            let body = size.checked_sub(header)?;
            if &head[4..] == b"moov" {
                if body > LIMIT {
                    return None;
                }
                let mut moov = vec![0; body as usize];
                file.read_exact(&mut moov).ok()?;
                return Some(moov);
            }
            file.seek(SeekFrom::Current(i64::try_from(body).ok()?))
                .ok()?;
        }
    }

    fn children(mut data: &[u8]) -> impl Iterator<Item = (&[u8], &[u8])> {
        std::iter::from_fn(move || {
            let size = u32::from_be_bytes(data.get(..4)?.try_into().ok()?) as usize;
            let (kind, body) = (data.get(4..8)?, data.get(8..size)?);
            data = &data[size..];
            Some((kind, body))
        })
    }

    fn child<'a>(data: &'a [u8], name: &[u8]) -> Option<&'a [u8]> {
        children(data)
            .find(|(kind, _)| *kind == name)
            .map(|(_, body)| body)
    }

    fn number(data: &[u8], at: usize, wide: bool) -> Option<i64> {
        Some(if wide {
            i64::from_be_bytes(data.get(at..at + 8)?.try_into().ok()?)
        } else {
            i64::from(i32::from_be_bytes(data.get(at..at + 4)?.try_into().ok()?))
        })
    }

    // mvhd and mdhd put the timescale after a version byte, flags and two dates
    fn timescale(header: &[u8]) -> Option<f64> {
        let at = if header.first()? == &1 { 20 } else { 12 };
        let scale = u32::from_be_bytes(header.get(at..at + 4)?.try_into().ok()?);
        (scale > 0).then_some(f64::from(scale))
    }

    let moov = match source {
        MediaSource::File(path) => movie(std::fs::File::open(path).ok()?)?,
        MediaSource::Bytes { body, .. } => movie(Cursor::new(&body[..]))?,
    };
    let movie_scale = timescale(child(&moov, b"mvhd")?)?;
    let track = children(&moov)
        .filter(|(kind, _)| *kind == b"trak")
        .find(|(_, trak)| {
            child(trak, b"mdia")
                .and_then(|mdia| child(mdia, b"hdlr"))
                .and_then(|hdlr| hdlr.get(8..12))
                == Some(b"vide")
        })?
        .1;
    let media_scale = timescale(child(child(track, b"mdia")?, b"mdhd")?)?;
    let list = child(child(track, b"edts")?, b"elst")?;
    let wide = list.first()? == &1;
    let count = u32::from_be_bytes(list.get(4..8)?.try_into().ok()?) as usize;
    let entry = if wide { 20 } else { 12 };
    let mut delay = 0.0;
    for index in 0..count {
        let at = 8 + index * entry;
        let duration = number(list, at, wide)?;
        let media_time = number(list, at + if wide { 8 } else { 4 }, wide)?;
        if media_time == -1 {
            delay += duration as f64 / movie_scale;
        } else {
            return Some(delay - media_time as f64 / media_scale);
        }
    }
    None
}

#[cfg(not(target_os = "windows"))]
mod platform {
    use super::{MediaInfo, MediaSource, VideoFrame, VideoReader};
    use std::ffi::{CStr, CString, c_char, c_int, c_uint, c_void};
    use std::path::PathBuf;
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant};

    pub(super) const TYPES: &[&str] = &["video/mp4", "video/webm", "video/ogg", "video/x-matroska"];

    const RTLD_NOW: c_int = 2;
    const STATE_NULL: c_int = 1;
    const STATE_PAUSED: c_int = 3;
    const STATE_PLAYING: c_int = 4;
    const STATE_CHANGE_FAILURE: c_int = 0;
    const FORMAT_TIME: c_int = 3;
    const SEEK_FLUSH: c_int = 1;
    const SEEK_ACCURATE: c_int = 2;
    const MAP_READ: c_int = 1;
    const MESSAGE_ERROR: c_int = 1 << 1;
    const CLOCK_TIME_NONE: u64 = u64::MAX;
    const NANOSECONDS: f64 = 1e9;
    // how long a pipeline may sit without a frame, an end or an error before it counts as stuck
    const STALL: Duration = Duration::from_secs(10);
    const POLL: u64 = 100_000_000;

    // videoscale squares anamorphic pixels, so the sink's size is the natural one
    const PIPELINE: &CStr = c"filesrc name=source ! decodebin ! videoconvert ! videoscale ! \
        video/x-raw,format=BGRx,pixel-aspect-ratio=1/1 ! \
        appsink name=sink sync=false max-buffers=3 enable-last-sample=false";

    #[repr(C)]
    struct MiniObject {
        kind: usize,
        refcount: c_int,
        lockstate: c_int,
        flags: c_uint,
        copy: *mut c_void,
        dispose: *mut c_void,
        free: *mut c_void,
        priv_uint: c_uint,
        priv_pointer: *mut c_void,
    }

    // the head of GstBuffer, which gstreamer 1.x keeps stable
    #[repr(C)]
    struct Buffer {
        object: MiniObject,
        pool: *mut c_void,
        pts: u64,
    }

    #[repr(C)]
    struct MapInfo {
        memory: *mut c_void,
        flags: c_int,
        data: *const u8,
        size: usize,
        maxsize: usize,
        user_data: [*mut c_void; 4],
        reserved: [*mut c_void; 4],
    }

    #[repr(C)]
    struct GError {
        domain: u32,
        code: c_int,
        message: *const c_char,
    }

    type Object = *mut c_void;

    // libgstreamer and libgstapp, loaded at first use so a system without them still runs,
    // and only its videos fail; each name is the C function the field calls
    struct Gst {
        gst_init_check: unsafe extern "C" fn(*mut c_int, *mut c_void, *mut *mut GError) -> c_int,
        gst_parse_launch: unsafe extern "C" fn(*const c_char, *mut *mut GError) -> Object,
        gst_bin_get_by_name: unsafe extern "C" fn(Object, *const c_char) -> Object,
        g_object_set: unsafe extern "C" fn(Object, *const c_char, ...),
        g_error_free: unsafe extern "C" fn(*mut GError),
        g_free: unsafe extern "C" fn(*mut c_void),
        gst_object_unref: unsafe extern "C" fn(Object),
        gst_mini_object_unref: unsafe extern "C" fn(Object),
        gst_element_set_state: unsafe extern "C" fn(Object, c_int) -> c_int,
        gst_element_get_state: unsafe extern "C" fn(Object, *mut c_int, *mut c_int, u64) -> c_int,
        gst_element_query_duration: unsafe extern "C" fn(Object, c_int, *mut i64) -> c_int,
        gst_element_seek_simple: unsafe extern "C" fn(Object, c_int, c_int, i64) -> c_int,
        gst_element_get_bus: unsafe extern "C" fn(Object) -> Object,
        gst_element_get_static_pad: unsafe extern "C" fn(Object, *const c_char) -> Object,
        gst_pad_get_current_caps: unsafe extern "C" fn(Object) -> Object,
        gst_bus_pop_filtered: unsafe extern "C" fn(Object, c_int) -> Object,
        gst_message_parse_error: unsafe extern "C" fn(Object, *mut *mut GError, *mut *mut c_char),
        gst_app_sink_try_pull_sample: unsafe extern "C" fn(Object, u64) -> Object,
        gst_app_sink_is_eos: unsafe extern "C" fn(Object) -> c_int,
        gst_sample_get_buffer: unsafe extern "C" fn(Object) -> *mut Buffer,
        gst_sample_get_caps: unsafe extern "C" fn(Object) -> Object,
        gst_caps_get_structure: unsafe extern "C" fn(Object, c_uint) -> Object,
        gst_structure_get_int: unsafe extern "C" fn(Object, *const c_char, *mut c_int) -> c_int,
        gst_buffer_map: unsafe extern "C" fn(*mut Buffer, *mut MapInfo, c_int) -> c_int,
        gst_buffer_unmap: unsafe extern "C" fn(*mut Buffer, *mut MapInfo),
    }

    unsafe extern "C" {
        fn dlopen(name: *const c_char, flags: c_int) -> *mut c_void;
        fn dlsym(library: *mut c_void, name: *const c_char) -> *mut c_void;
    }

    macro_rules! load {
        ($library:expr, $($name:ident),* $(,)?) => {{
            let library = $library;
            Gst {
                $($name: {
                    let symbol = dlsym(library, concat!(stringify!($name), "\0").as_ptr().cast());
                    if symbol.is_null() {
                        return None;
                    }
                    // the field the struct declares gives the signature
                    #[allow(clippy::missing_transmute_annotations)]
                    let function = std::mem::transmute::<*mut c_void, _>(symbol);
                    function
                },)*
            }
        }};
    }

    fn gst() -> Result<&'static Gst, String> {
        static GST: OnceLock<Option<Gst>> = OnceLock::new();
        GST.get_or_init(|| {
            // SAFETY: the libraries stay loaded for the life of the process, dlsym on gstapp
            // also finds what it links (gstreamer, gobject, glib), and each symbol is cast to
            // the signature its header declares
            unsafe {
                if dlopen(c"libgstreamer-1.0.so.0".as_ptr(), RTLD_NOW).is_null() {
                    return None;
                }
                let library = dlopen(c"libgstapp-1.0.so.0".as_ptr(), RTLD_NOW);
                if library.is_null() {
                    return None;
                }
                let gst = load!(
                    library,
                    gst_init_check,
                    gst_parse_launch,
                    gst_bin_get_by_name,
                    g_object_set,
                    g_error_free,
                    g_free,
                    gst_object_unref,
                    gst_mini_object_unref,
                    gst_element_set_state,
                    gst_element_get_state,
                    gst_element_query_duration,
                    gst_element_seek_simple,
                    gst_element_get_bus,
                    gst_element_get_static_pad,
                    gst_pad_get_current_caps,
                    gst_bus_pop_filtered,
                    gst_message_parse_error,
                    gst_app_sink_try_pull_sample,
                    gst_app_sink_is_eos,
                    gst_sample_get_buffer,
                    gst_sample_get_caps,
                    gst_caps_get_structure,
                    gst_structure_get_int,
                    gst_buffer_map,
                    gst_buffer_unmap,
                );
                let mut error = std::ptr::null_mut();
                if (gst.gst_init_check)(std::ptr::null_mut(), std::ptr::null_mut(), &mut error) == 0
                {
                    if !error.is_null() {
                        (gst.g_error_free)(error);
                    }
                    return None;
                }
                Some(gst)
            }
        })
        .as_ref()
        .ok_or_else(|| "gstreamer is not installed".to_string())
    }

    pub(super) struct Reader {
        gst: &'static Gst,
        pipeline: Object,
        sink: Object,
        bus: Object,
        info: MediaInfo,
        last: f64,
        // bytes handed over in memory, written out for filesrc and removed with the reader
        spilled: Option<PathBuf>,
    }

    // SAFETY: gstreamer objects are thread safe, and the engine uses a reader on one thread
    unsafe impl Send for Reader {}

    impl Reader {
        pub(super) fn open(source: &MediaSource) -> Result<Self, String> {
            let gst = gst()?;
            let (path, spilled) = match source {
                MediaSource::File(path) => (path.clone(), None),
                MediaSource::Bytes { body, .. } => {
                    static SPILLS: AtomicU64 = AtomicU64::new(0);
                    let path = std::env::temp_dir().join(format!(
                        "ceri-media-{}-{}",
                        std::process::id(),
                        SPILLS.fetch_add(1, Ordering::Relaxed)
                    ));
                    std::fs::write(&path, body).map_err(|error| error.to_string())?;
                    (path.clone(), Some(path))
                }
            };
            let location = CString::new(path.as_os_str().as_encoded_bytes())
                .map_err(|_| "the media path holds a nul byte".to_string())?;
            // SAFETY: each object is checked before use and released by Drop, which runs as
            // soon as the reader exists
            unsafe {
                let mut error = std::ptr::null_mut();
                let pipeline = (gst.gst_parse_launch)(PIPELINE.as_ptr(), &mut error);
                if !error.is_null() {
                    let message = take_error(gst, error);
                    if !pipeline.is_null() {
                        (gst.gst_object_unref)(pipeline);
                    }
                    return Err(message);
                }
                let mut reader = Self {
                    gst,
                    pipeline,
                    sink: (gst.gst_bin_get_by_name)(pipeline, c"sink".as_ptr()),
                    bus: (gst.gst_element_get_bus)(pipeline),
                    info: MediaInfo {
                        duration: f64::NAN,
                        width: 0,
                        height: 0,
                    },
                    last: 0.0,
                    spilled,
                };
                let source = (gst.gst_bin_get_by_name)(pipeline, c"source".as_ptr());
                if source.is_null() || reader.sink.is_null() || reader.bus.is_null() {
                    return Err("gstreamer could not build the pipeline".to_string());
                }
                (gst.g_object_set)(
                    source,
                    c"location".as_ptr(),
                    location.as_ptr(),
                    std::ptr::null::<c_char>(),
                );
                (gst.gst_object_unref)(source);
                if (gst.gst_element_set_state)(pipeline, STATE_PAUSED) == STATE_CHANGE_FAILURE
                    || (gst.gst_element_get_state)(
                        pipeline,
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        STALL.as_nanos() as u64,
                    ) == STATE_CHANGE_FAILURE
                {
                    return Err(reader
                        .error()
                        .unwrap_or_else(|| "gstreamer could not open the media".to_string()));
                }
                let (width, height) = reader
                    .negotiated()
                    .ok_or("the media has no video stream gstreamer can decode")?;
                let mut duration = 0;
                if (gst.gst_element_query_duration)(pipeline, FORMAT_TIME, &mut duration) != 0
                    && duration >= 0
                {
                    reader.info.duration = duration as f64 / NANOSECONDS;
                }
                reader.info.width = width;
                reader.info.height = height;
                if (gst.gst_element_set_state)(pipeline, STATE_PLAYING) == STATE_CHANGE_FAILURE {
                    return Err(reader
                        .error()
                        .unwrap_or_else(|| "gstreamer could not play the media".to_string()));
                }
                Ok(reader)
            }
        }

        // the frame size the sink agreed on once the pipeline prerolled
        unsafe fn negotiated(&self) -> Option<(u32, u32)> {
            let gst = self.gst;
            unsafe {
                let pad = (gst.gst_element_get_static_pad)(self.sink, c"sink".as_ptr());
                if pad.is_null() {
                    return None;
                }
                let caps = (gst.gst_pad_get_current_caps)(pad);
                (gst.gst_object_unref)(pad);
                if caps.is_null() {
                    return None;
                }
                let size = caps_size(gst, caps);
                (gst.gst_mini_object_unref)(caps);
                size
            }
        }

        unsafe fn error(&self) -> Option<String> {
            let gst = self.gst;
            unsafe {
                let message = (gst.gst_bus_pop_filtered)(self.bus, MESSAGE_ERROR);
                if message.is_null() {
                    return None;
                }
                let mut error = std::ptr::null_mut();
                let mut debug = std::ptr::null_mut();
                (gst.gst_message_parse_error)(message, &mut error, &mut debug);
                if !debug.is_null() {
                    (gst.g_free)(debug.cast());
                }
                (gst.gst_mini_object_unref)(message);
                Some(take_error(gst, error))
            }
        }

        unsafe fn frame(&mut self, sample: Object) -> Result<VideoFrame, String> {
            let gst = self.gst;
            unsafe {
                let (width, height) = caps_size(gst, (gst.gst_sample_get_caps)(sample))
                    .ok_or("gstreamer gave a frame without a size")?;
                let buffer = (gst.gst_sample_get_buffer)(sample);
                if buffer.is_null() {
                    return Err("gstreamer gave a frame without pixels".to_string());
                }
                if (*buffer).pts != CLOCK_TIME_NONE {
                    self.last = (*buffer).pts as f64 / NANOSECONDS;
                }
                let mut map = std::mem::zeroed::<MapInfo>();
                if (gst.gst_buffer_map)(buffer, &mut map, MAP_READ) == 0 {
                    return Err("gstreamer could not map a frame".to_string());
                }
                let row = width as usize * 4;
                let rows = height as usize;
                let pitch = map.size.checked_div(rows).unwrap_or(0);
                let mut bgra = Vec::with_capacity(row * rows);
                if pitch >= row {
                    let bytes = std::slice::from_raw_parts(map.data, map.size);
                    for line in bytes.chunks(pitch).take(rows) {
                        bgra.extend_from_slice(&line[..row]);
                    }
                }
                (gst.gst_buffer_unmap)(buffer, &mut map);
                if bgra.is_empty() {
                    return Err("gstreamer gave a frame smaller than its picture".to_string());
                }
                Ok(VideoFrame {
                    time: self.last,
                    width,
                    height,
                    bgra,
                })
            }
        }
    }

    unsafe fn caps_size(gst: &Gst, caps: Object) -> Option<(u32, u32)> {
        unsafe {
            if caps.is_null() {
                return None;
            }
            let structure = (gst.gst_caps_get_structure)(caps, 0);
            let (mut width, mut height) = (0, 0);
            if structure.is_null()
                || (gst.gst_structure_get_int)(structure, c"width".as_ptr(), &mut width) == 0
                || (gst.gst_structure_get_int)(structure, c"height".as_ptr(), &mut height) == 0
                || width <= 0
                || height <= 0
            {
                return None;
            }
            Some((width as u32, height as u32))
        }
    }

    unsafe fn take_error(gst: &Gst, error: *mut GError) -> String {
        unsafe {
            if error.is_null() {
                return "gstreamer failed without saying why".to_string();
            }
            let message = CStr::from_ptr((*error).message)
                .to_string_lossy()
                .into_owned();
            (gst.g_error_free)(error);
            message
        }
    }

    impl VideoReader for Reader {
        fn info(&self) -> MediaInfo {
            self.info
        }

        fn seek(&mut self, seconds: f64) -> Result<(), String> {
            let nanoseconds = (seconds.max(0.0) * NANOSECONDS) as i64;
            // SAFETY: the pipeline is alive until Drop
            let sought = unsafe {
                (self.gst.gst_element_seek_simple)(
                    self.pipeline,
                    FORMAT_TIME,
                    SEEK_FLUSH | SEEK_ACCURATE,
                    nanoseconds,
                )
            };
            if sought == 0 {
                return Err("gstreamer could not seek".to_string());
            }
            Ok(())
        }

        fn read(&mut self) -> Result<Option<VideoFrame>, String> {
            let started = Instant::now();
            // SAFETY: the sink and bus are alive until Drop, and each sample is released
            unsafe {
                loop {
                    let sample = (self.gst.gst_app_sink_try_pull_sample)(self.sink, POLL);
                    if !sample.is_null() {
                        let frame = self.frame(sample);
                        (self.gst.gst_mini_object_unref)(sample);
                        return frame.map(Some);
                    }
                    if (self.gst.gst_app_sink_is_eos)(self.sink) != 0 {
                        return Ok(None);
                    }
                    if let Some(error) = self.error() {
                        return Err(error);
                    }
                    if started.elapsed() > STALL {
                        return Err("gstreamer stopped giving frames".to_string());
                    }
                }
            }
        }
    }

    impl Drop for Reader {
        fn drop(&mut self) {
            // SAFETY: each object is released once, after the pipeline stops using them
            unsafe {
                if !self.pipeline.is_null() {
                    (self.gst.gst_element_set_state)(self.pipeline, STATE_NULL);
                }
                for object in [self.sink, self.bus, self.pipeline] {
                    if !object.is_null() {
                        (self.gst.gst_object_unref)(object);
                    }
                }
            }
            if let Some(path) = &self.spilled {
                let _ = std::fs::remove_file(path);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::DesktopVideo;
    use ceri_engine::{MediaDecoder, MediaSource};
    use std::path::PathBuf;

    // ten 64x32 frames at ten a second, key frames at 0 and 0.5 s, frame n grey at luma
    // 16 + 10n, so its rgb level is about 11.6n
    fn reel() -> PathBuf {
        let name = if cfg!(target_os = "windows") {
            "reel.mp4"
        } else {
            "reel.webm"
        };
        [env!("CARGO_MANIFEST_DIR"), "tests", "fixtures", name]
            .iter()
            .collect()
    }

    fn level(frame: &ceri_engine::VideoFrame) -> u8 {
        frame.bgra[(16 * 64 + 32) * 4 + 2]
    }

    #[test]
    fn a_file_decodes_to_timed_frames_of_its_natural_size() {
        let mut reader = DesktopVideo
            .open(&MediaSource::File(reel()))
            .expect("the fixture should open");
        let info = reader.info();
        assert_eq!((info.width, info.height), (64, 32));
        assert!((info.duration - 1.0).abs() < 0.05, "{}", info.duration);
        let mut frames = Vec::new();
        while let Some(frame) = reader.read().expect("frames should decode") {
            assert_eq!(
                (frame.width, frame.height, frame.bgra.len()),
                (64, 32, 64 * 32 * 4)
            );
            frames.push((frame.time, level(&frame)));
        }
        assert_eq!(frames.len(), 10, "{frames:?}");
        for (index, (time, level)) in frames.iter().enumerate() {
            assert!((time - index as f64 / 10.0).abs() < 0.01, "{frames:?}");
            let expected = index as f64 * 11.6;
            assert!((f64::from(*level) - expected).abs() < 8.0, "{frames:?}");
        }
    }

    #[test]
    fn a_seek_decodes_on_from_at_or_before_its_target() {
        let body = std::fs::read(reel()).expect("the fixture should read");
        let mut reader = DesktopVideo
            .open(&MediaSource::Bytes {
                body: body.into(),
                mime_type: "video/mp4".to_string(),
            })
            .expect("the fixture should open from memory");
        reader.seek(0.7).expect("the seek should land");
        let mut times = Vec::new();
        while let Some(frame) = reader.read().expect("frames should decode") {
            times.push(frame.time);
        }
        let near = |target: f64| times.iter().any(|time| (time - target).abs() < 0.01);
        assert!(times[0] <= 0.71 && near(0.7) && near(0.9), "{times:?}");
        assert!((times[times.len() - 1] - 0.9).abs() < 0.01, "{times:?}");
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn anamorphic_pixels_stretch_the_natural_size() {
        use super::platform::natural_size;

        assert_eq!(natural_size(720, 480, (32, 27)), (853, 480));
        assert_eq!(natural_size(720, 480, (8, 9)), (720, 540));
        assert_eq!(natural_size(64, 32, (0, 0)), (64, 32));
    }

    #[test]
    fn an_edit_list_moves_pictures_back_to_zero() {
        let source = |body: &[u8]| MediaSource::Bytes {
            body: body.into(),
            mime_type: "video/mp4".to_string(),
        };
        let reel = std::fs::read(reel()).expect("the fixture should read");
        let edited = std::fs::read(
            [
                env!("CARGO_MANIFEST_DIR"),
                "tests",
                "fixtures",
                "edited.mp4",
            ]
            .iter()
            .collect::<PathBuf>(),
        )
        .expect("the fixture should read");
        let shift = super::edit_shift(&source(&edited)).expect("the file has an edit list");
        assert!((shift + 2.0 / 30.0).abs() < 1e-6, "{shift}");
        if cfg!(target_os = "windows") {
            assert_eq!(super::edit_shift(&source(&reel)), Some(0.0));
            let mut reader = DesktopVideo
                .open(&source(&edited))
                .expect("the fixture should open");
            let first = reader
                .read()
                .expect("frames should decode")
                .expect("a frame");
            assert!(first.time.abs() < 1e-6, "{}", first.time);
        }
        assert_eq!(
            super::edit_shift(&source(b"\x1a\x45\xdf\xa3 not a movie")),
            None
        );
    }

    #[test]
    fn can_play_type_answers_maybe_for_containers_it_reads() {
        assert_eq!(
            DesktopVideo.can_play("video/mp4; codecs=\"avc1.42E01E\""),
            "maybe"
        );
        assert_eq!(DesktopVideo.can_play("VIDEO/WEBM"), "maybe");
        assert_eq!(DesktopVideo.can_play("video/x-unknown"), "");
    }
}
