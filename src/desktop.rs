use ceri_engine::{
    ClipboardProvider, FilePicker, FilePickerRequest, FileSelection, NavigationError, TimeZone,
    mime_type_for,
};
use std::path::Path;

const MAX_PICKED_BYTES: u64 = 64 * 1024 * 1024;

pub struct DesktopClipboard;

impl ClipboardProvider for DesktopClipboard {
    fn read_text(&self) -> Result<String, NavigationError> {
        platform::read_clipboard_text().map_err(NavigationError::Clipboard)
    }

    fn write_text(&self, text: &str) -> Result<(), NavigationError> {
        platform::write_clipboard_text(text).map_err(NavigationError::Clipboard)
    }
}

// the operating system's time zone rules, so script Date objects show local time
pub struct DesktopTimeZone;

impl TimeZone for DesktopTimeZone {
    fn offset_minutes(&self, utc_milliseconds: f64) -> i32 {
        platform::local_offset_minutes(utc_milliseconds)
    }

    fn name(&self, utc_milliseconds: f64) -> Option<String> {
        platform::zone_name(utc_milliseconds)
    }
}

pub struct DesktopFilePicker;

impl FilePicker for DesktopFilePicker {
    fn pick_files(
        &self,
        request: &FilePickerRequest,
    ) -> Result<Option<Vec<FileSelection>>, NavigationError> {
        let chosen = platform::pick_files(request.multiple(), request.accept())
            .map_err(NavigationError::FilePicker)?;
        let Some(chosen) = chosen else {
            return Ok(None);
        };
        let mut selections = Vec::with_capacity(chosen.len());
        for path in chosen {
            selections.push(read_selection(&path)?);
        }
        Ok(Some(selections))
    }
}

// the renderer receives bytes and metadata, never the host path
fn read_selection(path: &Path) -> Result<FileSelection, NavigationError> {
    let metadata = std::fs::metadata(path).map_err(|error| {
        NavigationError::FilePicker(format!("could not read {}: {error}", path.display()))
    })?;
    if metadata.len() > MAX_PICKED_BYTES {
        return Err(NavigationError::FilePicker(format!(
            "{} is larger than {} MiB",
            path.display(),
            MAX_PICKED_BYTES / (1024 * 1024)
        )));
    }
    let bytes = std::fs::read(path).map_err(|error| {
        NavigationError::FilePicker(format!("could not read {}: {error}", path.display()))
    })?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("file")
        .to_string();
    let modified = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|elapsed| elapsed.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or_default();
    FileSelection::new(name, mime_type_for(path), bytes, modified)
}

#[cfg(target_os = "windows")]
mod platform {
    use std::ffi::c_void;
    use std::path::PathBuf;

    type Handle = *mut c_void;

    const CF_UNICODETEXT: u32 = 13;
    const GMEM_MOVEABLE: u32 = 0x0002;
    const OFN_EXPLORER: u32 = 0x0008_0000;
    const OFN_FILEMUSTEXIST: u32 = 0x0000_1000;
    const OFN_ALLOWMULTISELECT: u32 = 0x0000_0200;
    const PATH_BUFFER: usize = 32 * 1024;

    #[repr(C)]
    struct OpenFileName {
        struct_size: u32,
        owner: Handle,
        instance: Handle,
        filter: *const u16,
        custom_filter: *mut u16,
        max_custom_filter: u32,
        filter_index: u32,
        file: *mut u16,
        max_file: u32,
        file_title: *mut u16,
        max_file_title: u32,
        initial_dir: *const u16,
        title: *const u16,
        flags: u32,
        file_offset: u16,
        file_extension: u16,
        def_ext: *const u16,
        cust_data: isize,
        hook: *const c_void,
        template_name: *const u16,
        reserved: *mut c_void,
        reserved2: u32,
        flags_ex: u32,
    }

    #[link(name = "user32")]
    unsafe extern "system" {
        fn OpenClipboard(owner: Handle) -> i32;
        fn CloseClipboard() -> i32;
        fn EmptyClipboard() -> i32;
        fn GetClipboardData(format: u32) -> Handle;
        fn SetClipboardData(format: u32, data: Handle) -> Handle;
        fn IsClipboardFormatAvailable(format: u32) -> i32;
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct SystemTime {
        year: u16,
        month: u16,
        day_of_week: u16,
        day: u16,
        hour: u16,
        minute: u16,
        second: u16,
        milliseconds: u16,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct FileTime {
        low: u32,
        high: u32,
    }

    #[repr(C)]
    struct TimeZoneInformation {
        bias: i32,
        standard_name: [u16; 32],
        standard_date: SystemTime,
        standard_bias: i32,
        daylight_name: [u16; 32],
        daylight_date: SystemTime,
        daylight_bias: i32,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn FileTimeToSystemTime(file_time: *const FileTime, system_time: *mut SystemTime) -> i32;
        fn SystemTimeToFileTime(system_time: *const SystemTime, file_time: *mut FileTime) -> i32;
        fn SystemTimeToTzSpecificLocalTime(
            zone: *const TimeZoneInformation,
            universal: *const SystemTime,
            local: *mut SystemTime,
        ) -> i32;
        fn GetTimeZoneInformation(zone: *mut TimeZoneInformation) -> u32;
    }

    // 100-nanosecond intervals between 1601-01-01 and the Unix epoch
    const EPOCH_IN_FILE_TIME: i64 = 116_444_736_000_000_000;

    fn zone_information() -> TimeZoneInformation {
        let mut zone = TimeZoneInformation {
            bias: 0,
            standard_name: [0; 32],
            standard_date: SystemTime::default(),
            standard_bias: 0,
            daylight_name: [0; 32],
            daylight_date: SystemTime::default(),
            daylight_bias: 0,
        };
        // SAFETY: the structure matches TIME_ZONE_INFORMATION and lives across the call
        unsafe { GetTimeZoneInformation(&mut zone) };
        zone
    }

    pub(super) fn local_offset_minutes(utc_milliseconds: f64) -> i32 {
        let fallback = || -(zone_information().bias);
        if !utc_milliseconds.is_finite() {
            return fallback();
        }
        let ticks = (utc_milliseconds as i64)
            .saturating_mul(10_000)
            .saturating_add(EPOCH_IN_FILE_TIME);
        if ticks < 0 {
            return fallback();
        }
        let universal_file = FileTime {
            low: ticks as u32,
            high: (ticks >> 32) as u32,
        };
        let mut universal = SystemTime::default();
        let mut local = SystemTime::default();
        let mut local_file = FileTime::default();
        // SAFETY: each pointer refers to a live, correctly laid out structure for the call
        let converted = unsafe {
            FileTimeToSystemTime(&universal_file, &mut universal) != 0
                && SystemTimeToTzSpecificLocalTime(std::ptr::null(), &universal, &mut local) != 0
                && SystemTimeToFileTime(&local, &mut local_file) != 0
        };
        if !converted {
            return fallback();
        }
        let local_ticks = (i64::from(local_file.high) << 32) | i64::from(local_file.low);
        // whole milliseconds were dropped from the round trip, so compare at minute precision
        ((local_ticks - ticks) as f64 / 600_000_000.0).round() as i32
    }

    pub(super) fn zone_name(utc_milliseconds: f64) -> Option<String> {
        let zone = zone_information();
        let standard = -(zone.bias + zone.standard_bias);
        let name = if local_offset_minutes(utc_milliseconds) == standard {
            &zone.standard_name
        } else {
            &zone.daylight_name
        };
        let length = name
            .iter()
            .position(|unit| *unit == 0)
            .unwrap_or(name.len());
        let text = String::from_utf16_lossy(&name[..length]);
        (!text.is_empty()).then_some(text)
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GlobalAlloc(flags: u32, bytes: usize) -> Handle;
        fn GlobalFree(memory: Handle) -> Handle;
        fn GlobalLock(memory: Handle) -> *mut c_void;
        fn GlobalUnlock(memory: Handle) -> i32;
    }

    #[link(name = "comdlg32")]
    unsafe extern "system" {
        fn GetOpenFileNameW(arguments: *mut OpenFileName) -> i32;
    }

    struct Clipboard;

    impl Clipboard {
        fn open() -> Result<Self, String> {
            // SAFETY: no clipboard handle is held across the call, and the guard closes it
            if unsafe { OpenClipboard(std::ptr::null_mut()) } == 0 {
                return Err("the system clipboard is in use by another window".to_string());
            }
            Ok(Self)
        }
    }

    impl Drop for Clipboard {
        fn drop(&mut self) {
            // SAFETY: the clipboard was opened by this thread
            unsafe { CloseClipboard() };
        }
    }

    pub(super) fn read_clipboard_text() -> Result<String, String> {
        // SAFETY: the format constant is valid and the call only reports availability
        if unsafe { IsClipboardFormatAvailable(CF_UNICODETEXT) } == 0 {
            return Ok(String::new());
        }
        let _clipboard = Clipboard::open()?;
        // SAFETY: the clipboard is open and the returned handle stays owned by the system
        let handle = unsafe { GetClipboardData(CF_UNICODETEXT) };
        if handle.is_null() {
            return Ok(String::new());
        }
        // SAFETY: the handle came from the clipboard, so it names global memory
        let pointer = unsafe { GlobalLock(handle) } as *const u16;
        if pointer.is_null() {
            return Err("the clipboard text could not be locked".to_string());
        }
        let mut units = Vec::new();
        // SAFETY: clipboard unicode text is a null terminated UTF-16 sequence
        unsafe {
            let mut cursor = pointer;
            while *cursor != 0 && units.len() < 1 << 22 {
                units.push(*cursor);
                cursor = cursor.add(1);
            }
            GlobalUnlock(handle);
        }
        Ok(String::from_utf16_lossy(&units))
    }

    pub(super) fn write_clipboard_text(text: &str) -> Result<(), String> {
        let mut units: Vec<u16> = text.encode_utf16().collect();
        units.push(0);
        let bytes = units.len() * 2;
        // SAFETY: the size is the exact byte length of the buffer copied below
        let memory = unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes) };
        if memory.is_null() {
            return Err("clipboard memory could not be allocated".to_string());
        }
        // SAFETY: memory came from GlobalAlloc with the byte length used here
        unsafe {
            let pointer = GlobalLock(memory) as *mut u16;
            if pointer.is_null() {
                GlobalFree(memory);
                return Err("clipboard memory could not be locked".to_string());
            }
            std::ptr::copy_nonoverlapping(units.as_ptr(), pointer, units.len());
            GlobalUnlock(memory);
        }
        let clipboard = match Clipboard::open() {
            Ok(clipboard) => clipboard,
            Err(error) => {
                // SAFETY: ownership stays here while the clipboard refuses to open
                unsafe { GlobalFree(memory) };
                return Err(error);
            }
        };
        // SAFETY: the clipboard is open, and it takes ownership of the handle on success
        unsafe {
            EmptyClipboard();
            if SetClipboardData(CF_UNICODETEXT, memory).is_null() {
                GlobalFree(memory);
                drop(clipboard);
                return Err("the clipboard rejected the text".to_string());
            }
        }
        Ok(())
    }

    pub(super) fn pick_files(
        multiple: bool,
        accept: Option<&str>,
    ) -> Result<Option<Vec<PathBuf>>, String> {
        let filter = file_filter(accept);
        let mut buffer = vec![0_u16; PATH_BUFFER];
        let mut arguments = OpenFileName {
            struct_size: std::mem::size_of::<OpenFileName>() as u32,
            owner: std::ptr::null_mut(),
            instance: std::ptr::null_mut(),
            filter: filter.as_ptr(),
            custom_filter: std::ptr::null_mut(),
            max_custom_filter: 0,
            filter_index: 1,
            file: buffer.as_mut_ptr(),
            max_file: buffer.len() as u32,
            file_title: std::ptr::null_mut(),
            max_file_title: 0,
            initial_dir: std::ptr::null(),
            title: std::ptr::null(),
            flags: OFN_EXPLORER
                | OFN_FILEMUSTEXIST
                | if multiple { OFN_ALLOWMULTISELECT } else { 0 },
            file_offset: 0,
            file_extension: 0,
            def_ext: std::ptr::null(),
            cust_data: 0,
            hook: std::ptr::null(),
            template_name: std::ptr::null(),
            reserved: std::ptr::null_mut(),
            reserved2: 0,
            flags_ex: 0,
        };
        // SAFETY: every pointer field references a live buffer for the duration of the call
        let chosen = unsafe { GetOpenFileNameW(&mut arguments) };
        if chosen == 0 {
            // a cancelled dialog is a normal outcome, not a failure
            return Ok(None);
        }
        Ok(Some(parse_selection(&buffer)))
    }

    // a multi selection arrives as a directory followed by null separated names
    fn parse_selection(buffer: &[u16]) -> Vec<PathBuf> {
        let mut entries = Vec::new();
        let mut current = Vec::new();
        for unit in buffer {
            if *unit == 0 {
                if current.is_empty() {
                    break;
                }
                entries.push(String::from_utf16_lossy(&current));
                current.clear();
                continue;
            }
            current.push(*unit);
        }
        match entries.split_first() {
            Some((directory, names)) if !names.is_empty() => names
                .iter()
                .map(|name| PathBuf::from(directory).join(name))
                .collect(),
            Some((single, _)) => vec![PathBuf::from(single)],
            None => Vec::new(),
        }
    }

    fn file_filter(accept: Option<&str>) -> Vec<u16> {
        let patterns = accept
            .map(|accept| {
                accept
                    .split(',')
                    .filter_map(|entry| {
                        let entry = entry.trim();
                        entry
                            .strip_prefix('.')
                            .map(|extension| format!("*.{extension}"))
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let mut filter = String::new();
        if !patterns.is_empty() {
            filter.push_str(&format!("Accepted files\0{}\0", patterns.join(";")));
        }
        filter.push_str("All files\0*.*\0");
        filter.push('\0');
        filter.encode_utf16().collect()
    }
}

#[cfg(not(target_os = "windows"))]
mod platform {
    use std::path::PathBuf;

    #[cfg(unix)]
    mod clock {
        use std::ffi::{CStr, c_char, c_int, c_long};

        // struct tm as glibc and the BSD libc on macOS lay it out
        #[repr(C)]
        struct Tm {
            second: c_int,
            minute: c_int,
            hour: c_int,
            day: c_int,
            month: c_int,
            year: c_int,
            weekday: c_int,
            year_day: c_int,
            daylight: c_int,
            offset: c_long,
            zone: *const c_char,
        }

        unsafe extern "C" {
            fn localtime_r(time: *const i64, result: *mut Tm) -> *mut Tm;
        }

        fn local(utc_milliseconds: f64) -> Option<(i32, Option<String>)> {
            if !utc_milliseconds.is_finite() {
                return None;
            }
            let seconds = (utc_milliseconds / 1000.0).floor() as i64;
            let mut tm = Tm {
                second: 0,
                minute: 0,
                hour: 0,
                day: 0,
                month: 0,
                year: 0,
                weekday: 0,
                year_day: 0,
                daylight: 0,
                offset: 0,
                zone: std::ptr::null(),
            };
            // SAFETY: both pointers refer to live values for the duration of the call
            let result = unsafe { localtime_r(&seconds, &mut tm) };
            if result.is_null() {
                return None;
            }
            let zone = (!tm.zone.is_null())
                // SAFETY: localtime_r sets tm_zone to a static NUL-terminated abbreviation
                .then(|| {
                    unsafe { CStr::from_ptr(tm.zone) }
                        .to_string_lossy()
                        .into_owned()
                });
            Some(((tm.offset / 60) as i32, zone))
        }

        pub(in super::super) fn local_offset_minutes(utc_milliseconds: f64) -> i32 {
            local(utc_milliseconds).map_or(0, |(offset, _)| offset)
        }

        pub(in super::super) fn zone_name(utc_milliseconds: f64) -> Option<String> {
            local(utc_milliseconds).and_then(|(_, zone)| zone)
        }
    }

    #[cfg(unix)]
    pub(super) use clock::{local_offset_minutes, zone_name};

    #[cfg(not(unix))]
    pub(super) fn local_offset_minutes(_utc_milliseconds: f64) -> i32 {
        0
    }

    #[cfg(not(unix))]
    pub(super) fn zone_name(_utc_milliseconds: f64) -> Option<String> {
        None
    }

    pub(super) fn read_clipboard_text() -> Result<String, String> {
        Err("the desktop clipboard adapter currently covers Windows only".to_string())
    }

    pub(super) fn write_clipboard_text(_text: &str) -> Result<(), String> {
        Err("the desktop clipboard adapter currently covers Windows only".to_string())
    }

    pub(super) fn pick_files(
        _multiple: bool,
        _accept: Option<&str>,
    ) -> Result<Option<Vec<PathBuf>>, String> {
        Err("the desktop file dialog adapter currently covers Windows only".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::{DesktopClipboard, DesktopFilePicker};
    use ceri_engine::{ClipboardProvider, FilePickerRequest};

    #[test]
    fn clipboard_round_trips_text_on_the_host() {
        let clipboard = DesktopClipboard;
        let sample = "ceri clipboard check";
        match clipboard.write_text(sample) {
            Ok(()) => {
                let read = clipboard
                    .read_text()
                    .expect("a written clipboard should read back");
                assert_eq!(read, sample);
            }
            Err(error) => {
                // a locked or unsupported clipboard has to surface as a typed error
                assert!(!error.to_string().is_empty());
            }
        }
    }

    #[test]
    fn a_file_request_carries_its_accept_list() {
        let request = FilePickerRequest::new("upload", true, Some(".txt,.md".to_string()));
        assert_eq!(request.accept(), Some(".txt,.md"));
        assert!(request.multiple());
        // the dialog itself needs a user, so only the request shape is checked here
        let _picker = DesktopFilePicker;
    }
}
