use ceri_engine::{
    ClipboardProvider, FilePicker, FilePickerRequest, FileSelection, NavigationError, TimeZone,
    mime_type_for,
};
use std::path::Path;

const MAX_PICKED_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Default)]
pub struct DesktopClipboard {
    // x11 and wayland serve pasted text from the copying process, so the connection lives as
    // long as the app and hands its text to a clipboard manager when dropped
    #[cfg(all(unix, not(target_os = "macos")))]
    connection: std::cell::RefCell<Option<arboard::Clipboard>>,
}

impl ClipboardProvider for DesktopClipboard {
    fn read_text(&self) -> Result<String, NavigationError> {
        platform::read_clipboard_text(self).map_err(NavigationError::Clipboard)
    }

    fn write_text(&self, text: &str) -> Result<(), NavigationError> {
        platform::write_clipboard_text(self, text).map_err(NavigationError::Clipboard)
    }

    #[cfg(target_os = "windows")]
    fn read_image(&self) -> Result<Option<(String, Vec<u8>)>, NavigationError> {
        platform::read_clipboard_image().map_err(NavigationError::Clipboard)
    }
}

// the size and RGBA pixels of a device independent bitmap as the clipboard packs one: a
// header, any colour masks, then rows of blue, green, red from the bottom up unless the
// height is negative
#[cfg(any(target_os = "windows", test))]
fn dib_pixels(dib: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let long = |at: usize| Some(i32::from_le_bytes(dib.get(at..at + 4)?.try_into().ok()?));
    let header = usize::try_from(long(0)?).ok().filter(|size| *size >= 40)?;
    let (width, height) = (long(4)?, long(8)?);
    let bits = u16::from_le_bytes(dib.get(14..16)?.try_into().ok()?);
    let compression = long(16)?;
    // uncompressed rows, or the same with the masks spelled out
    if width <= 0 || height == 0 || !matches!(bits, 24 | 32) || !matches!(compression, 0 | 3) {
        return None;
    }
    let (columns, rows) = (width as usize, height.unsigned_abs() as usize);
    if columns.checked_mul(rows)? > 1 << 26 {
        return None;
    }
    // a forty byte header keeps its three masks after it, and larger ones hold them
    let masks = if compression == 3 && header == 40 {
        12
    } else {
        0
    };
    let palette = usize::try_from(long(32)?).ok()?.checked_mul(4)?;
    let each = usize::from(bits / 8);
    let stride = (columns * each + 3) & !3;
    let start = header.checked_add(masks)?.checked_add(palette)?;
    let packed = dib.get(start..start.checked_add(stride.checked_mul(rows)?)?)?;
    let mut rgba = Vec::with_capacity(columns * rows * 4);
    for row in 0..rows {
        let source = if height > 0 { rows - 1 - row } else { row };
        let line = &packed[source * stride..][..columns * each];
        for pixel in line.chunks_exact(each) {
            let alpha = if each == 4 { pixel[3] } else { 255 };
            rgba.extend([pixel[2], pixel[1], pixel[0], alpha]);
        }
    }
    // thirty two bits with no alpha anywhere is a solid picture, as most screenshots are
    if rgba.as_chunks::<4>().0.iter().all(|pixel| pixel[3] == 0) {
        for pixel in rgba.as_chunks_mut::<4>().0 {
            pixel[3] = 255;
        }
    }
    Some((columns as u32, rows as u32, rgba))
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

// the user's region setting as a BCP 47 tag, so Intl formats dates and numbers the way the OS does
pub fn user_locale() -> Option<String> {
    platform::user_locale()
}

// turns a POSIX locale such as en_GB.UTF-8@euro into en-GB
#[cfg(any(not(target_os = "windows"), test))]
fn posix_locale_tag(value: &str) -> Option<String> {
    let name = value.split(['.', '@']).next()?;
    (!name.is_empty() && name != "C" && name != "POSIX").then(|| name.replace('_', "-"))
}

pub struct DesktopFilePicker;

impl FilePicker for DesktopFilePicker {
    fn pick_files(
        &self,
        request: &FilePickerRequest,
    ) -> Result<Option<Vec<FileSelection>>, NavigationError> {
        let chosen = crate::dialog::FileDialog {
            filters: crate::dialog::accept_filters(request.accept()),
            multiple: request.multiple(),
            ..Default::default()
        }
        .open_files()
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
    use windows::Win32::Foundation::{FILETIME, GlobalFree, HANDLE, HGLOBAL, SYSTEMTIME};
    use windows::Win32::Globalization::GetUserDefaultLocaleName;
    use windows::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable,
        OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
    };
    use windows::Win32::System::Memory::{
        GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
    };
    use windows::Win32::System::Ole::{CF_DIB, CF_UNICODETEXT};
    use windows::Win32::System::Time::{
        FileTimeToSystemTime, GetTimeZoneInformation, SystemTimeToFileTime,
        SystemTimeToTzSpecificLocalTime, TIME_ZONE_INFORMATION,
    };
    // 100-nanosecond intervals between 1601-01-01 and the Unix epoch
    const EPOCH_IN_FILE_TIME: i64 = 116_444_736_000_000_000;

    fn unicode_text() -> u32 {
        u32::from(CF_UNICODETEXT.0)
    }

    fn zone_information() -> TIME_ZONE_INFORMATION {
        let mut zone = TIME_ZONE_INFORMATION::default();
        // SAFETY: the structure is TIME_ZONE_INFORMATION and lives across the call
        unsafe { GetTimeZoneInformation(&mut zone) };
        zone
    }

    pub(super) fn local_offset_minutes(utc_milliseconds: f64) -> i32 {
        let fallback = || -(zone_information().Bias);
        if !utc_milliseconds.is_finite() {
            return fallback();
        }
        let ticks = (utc_milliseconds as i64)
            .saturating_mul(10_000)
            .saturating_add(EPOCH_IN_FILE_TIME);
        if ticks < 0 {
            return fallback();
        }
        let universal_file = FILETIME {
            dwLowDateTime: ticks as u32,
            dwHighDateTime: (ticks >> 32) as u32,
        };
        let mut universal = SYSTEMTIME::default();
        let mut local = SYSTEMTIME::default();
        let mut local_file = FILETIME::default();
        // SAFETY: each pointer refers to a live, correctly laid out structure for the call
        let converted = unsafe {
            FileTimeToSystemTime(&universal_file, &mut universal).is_ok()
                && SystemTimeToTzSpecificLocalTime(None, &universal, &mut local).is_ok()
                && SystemTimeToFileTime(&local, &mut local_file).is_ok()
        };
        if !converted {
            return fallback();
        }
        let local_ticks =
            (i64::from(local_file.dwHighDateTime) << 32) | i64::from(local_file.dwLowDateTime);
        // whole milliseconds were dropped from the round trip, so compare at minute precision
        ((local_ticks - ticks) as f64 / 600_000_000.0).round() as i32
    }

    pub(super) fn user_locale() -> Option<String> {
        // LOCALE_NAME_MAX_LENGTH, the terminator included
        let mut name = [0u16; 85];
        // SAFETY: the buffer outlives the call and its length bounds the write
        let length = unsafe { GetUserDefaultLocaleName(&mut name) };
        (length > 1).then(|| String::from_utf16_lossy(&name[..length as usize - 1]))
    }

    pub(super) fn zone_name(utc_milliseconds: f64) -> Option<String> {
        let zone = zone_information();
        let standard = -(zone.Bias + zone.StandardBias);
        let name = if local_offset_minutes(utc_milliseconds) == standard {
            &zone.StandardName
        } else {
            &zone.DaylightName
        };
        let length = name
            .iter()
            .position(|unit| *unit == 0)
            .unwrap_or(name.len());
        let text = String::from_utf16_lossy(&name[..length]);
        (!text.is_empty()).then_some(text)
    }

    struct Clipboard;

    impl Clipboard {
        fn open() -> Result<Self, String> {
            // SAFETY: no clipboard handle is held across the call, and the guard closes it
            unsafe { OpenClipboard(None) }
                .map_err(|_| "the system clipboard is in use by another window".to_string())?;
            Ok(Self)
        }
    }

    impl Drop for Clipboard {
        fn drop(&mut self) {
            // SAFETY: the clipboard was opened by this thread
            let _ = unsafe { CloseClipboard() };
        }
    }

    pub(super) fn read_clipboard_text(
        _clipboard: &super::DesktopClipboard,
    ) -> Result<String, String> {
        // SAFETY: the format constant is valid and the call only reports availability
        if unsafe { IsClipboardFormatAvailable(unicode_text()) }.is_err() {
            return Ok(String::new());
        }
        let _clipboard = Clipboard::open()?;
        // SAFETY: the clipboard is open and the returned handle stays owned by the system
        let Ok(handle) = (unsafe { GetClipboardData(unicode_text()) }) else {
            return Ok(String::new());
        };
        let memory = HGLOBAL(handle.0);
        // SAFETY: the handle came from the clipboard, so it names global memory
        let pointer = unsafe { GlobalLock(memory) } as *const u16;
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
            // the lock count reaching zero reports as a failure with no error set
            let _ = GlobalUnlock(memory);
        }
        Ok(String::from_utf16_lossy(&units))
    }

    // a picture as a PNG file: the one an app put there under that name, or the bitmap
    // every screenshot comes with, encoded
    pub(super) fn read_clipboard_image() -> Result<Option<(String, Vec<u8>)>, String> {
        // SAFETY: the name is a terminated wide string
        let png = unsafe { RegisterClipboardFormatW(windows::core::w!("PNG")) };
        let held = [png, u32::from(CF_DIB.0)].into_iter().find(|format| {
            // SAFETY: the call only reports availability
            *format != 0 && unsafe { IsClipboardFormatAvailable(*format) }.is_ok()
        });
        let Some(format) = held else {
            return Ok(None);
        };
        let _clipboard = Clipboard::open()?;
        // SAFETY: the clipboard is open and the returned handle stays owned by the system
        let Ok(handle) = (unsafe { GetClipboardData(format) }) else {
            return Ok(None);
        };
        let memory = HGLOBAL(handle.0);
        // SAFETY: the handle came from the clipboard, so it names global memory of the size
        // GlobalSize gives, which stays locked while it is copied
        let bytes = unsafe {
            let pointer = GlobalLock(memory) as *const u8;
            if pointer.is_null() {
                return Err("the clipboard picture could not be locked".to_string());
            }
            let bytes = std::slice::from_raw_parts(pointer, GlobalSize(memory)).to_vec();
            let _ = GlobalUnlock(memory);
            bytes
        };
        let file = if format == png {
            Some(bytes)
        } else {
            super::dib_pixels(&bytes)
                .map(|(width, height, rgba)| ceri_engine::png_from_rgba(width, height, &rgba))
        };
        Ok(file.map(|file| ("image/png".to_string(), file)))
    }

    pub(super) fn write_clipboard_text(
        _clipboard: &super::DesktopClipboard,
        text: &str,
    ) -> Result<(), String> {
        let mut units: Vec<u16> = text.encode_utf16().collect();
        units.push(0);
        // SAFETY: the size is the exact byte length of the buffer copied below
        let memory = unsafe { GlobalAlloc(GMEM_MOVEABLE, units.len() * 2) }
            .map_err(|_| "clipboard memory could not be allocated".to_string())?;
        let free = || {
            // SAFETY: the memory is still owned here
            let _ = unsafe { GlobalFree(Some(memory)) };
        };
        // SAFETY: memory came from GlobalAlloc with the byte length used here
        unsafe {
            let pointer = GlobalLock(memory) as *mut u16;
            if pointer.is_null() {
                free();
                return Err("clipboard memory could not be locked".to_string());
            }
            std::ptr::copy_nonoverlapping(units.as_ptr(), pointer, units.len());
            let _ = GlobalUnlock(memory);
        }
        let _clipboard = match Clipboard::open() {
            Ok(clipboard) => clipboard,
            Err(error) => {
                free();
                return Err(error);
            }
        };
        // SAFETY: the clipboard is open, and it takes ownership of the handle on success
        unsafe {
            let _ = EmptyClipboard();
            if SetClipboardData(unicode_text(), Some(HANDLE(memory.0))).is_err() {
                free();
                return Err("the clipboard rejected the text".to_string());
            }
        }
        Ok(())
    }
}

#[cfg(not(target_os = "windows"))]
mod platform {
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

    pub(super) fn user_locale() -> Option<String> {
        ["LC_ALL", "LC_TIME", "LANG"]
            .into_iter()
            .find_map(|name| std::env::var(name).ok().filter(|value| !value.is_empty()))
            .and_then(|value| super::posix_locale_tag(&value))
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    fn with_connection<T>(
        clipboard: &super::DesktopClipboard,
        action: impl FnOnce(&mut arboard::Clipboard) -> Result<T, arboard::Error>,
    ) -> Result<T, String> {
        let mut connection = clipboard.connection.borrow_mut();
        let connection = match &mut *connection {
            Some(connection) => connection,
            None => {
                connection.insert(arboard::Clipboard::new().map_err(|error| error.to_string())?)
            }
        };
        action(connection).map_err(|error| error.to_string())
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    pub(super) fn read_clipboard_text(
        clipboard: &super::DesktopClipboard,
    ) -> Result<String, String> {
        with_connection(clipboard, |connection| match connection.get_text() {
            // an empty or non-text clipboard reads as empty text, as on windows
            Err(arboard::Error::ContentNotAvailable) => Ok(String::new()),
            result => result,
        })
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    pub(super) fn write_clipboard_text(
        clipboard: &super::DesktopClipboard,
        text: &str,
    ) -> Result<(), String> {
        with_connection(clipboard, |connection| connection.set_text(text))
    }

    #[cfg(not(all(unix, not(target_os = "macos"))))]
    pub(super) fn read_clipboard_text(
        _clipboard: &super::DesktopClipboard,
    ) -> Result<String, String> {
        Err("the desktop clipboard adapter covers Windows and Linux only".to_string())
    }

    #[cfg(not(all(unix, not(target_os = "macos"))))]
    pub(super) fn write_clipboard_text(
        _clipboard: &super::DesktopClipboard,
        _text: &str,
    ) -> Result<(), String> {
        Err("the desktop clipboard adapter covers Windows and Linux only".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::{DesktopClipboard, DesktopFilePicker, posix_locale_tag, user_locale};
    use ceri_engine::{ClipboardProvider, FilePickerRequest};

    #[test]
    fn a_clipboard_bitmap_reads_bottom_up_as_rgba() {
        // two by two, 24 bits: rows are padded to four bytes and the last row comes first
        let mut dib = vec![0_u8; 40];
        dib[0] = 40;
        dib[4] = 2;
        dib[8] = 2;
        dib[14] = 24;
        dib.extend([1, 2, 3, 4, 5, 6, 0, 0, 7, 8, 9, 10, 11, 12, 0, 0]);
        let (width, height, rgba) = super::dib_pixels(&dib).expect("bitmap");
        assert_eq!((width, height), (2, 2));
        assert_eq!(
            rgba,
            [9, 8, 7, 255, 12, 11, 10, 255, 3, 2, 1, 255, 6, 5, 4, 255]
        );
        // 32 bits top down with masks after the header, and alpha left at nothing
        let mut dib = vec![0_u8; 40];
        dib[0] = 40;
        dib[4] = 1;
        dib[8..12].copy_from_slice(&(-1_i32).to_le_bytes());
        dib[14] = 32;
        dib[16] = 3;
        dib.extend([0; 12]);
        dib.extend([30, 20, 10, 0]);
        assert_eq!(super::dib_pixels(&dib), Some((1, 1, vec![10, 20, 30, 255])));
        assert_eq!(super::dib_pixels(&dib[..44]), None, "cut short");
        assert_eq!(super::dib_pixels(&[]), None);
    }

    #[test]
    fn locales_come_out_as_bcp_47_tags() {
        assert_eq!(posix_locale_tag("en_GB.UTF-8").as_deref(), Some("en-GB"));
        assert_eq!(posix_locale_tag("de_DE@euro").as_deref(), Some("de-DE"));
        assert_eq!(posix_locale_tag("C.UTF-8"), None);
        assert_eq!(posix_locale_tag("POSIX"), None);
        assert!(user_locale().is_none_or(|tag| !tag.is_empty() && !tag.contains('_')));
    }

    #[test]
    fn clipboard_round_trips_text_on_the_host() {
        let clipboard = DesktopClipboard::default();
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
