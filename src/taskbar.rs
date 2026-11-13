// a window's progress on its taskbar button and a count over the app's icon: ITaskbarList3 on
// windows, where the count is drawn as chromium draws a web app's badge, and elsewhere the unity
// launcher entry that plasma, dash to dock, plank and budgie read

use ceri_engine::WindowControl;

pub(crate) use platform::Taskbar;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Progress {
    None,
    // busy for no telling how long
    Indeterminate,
    // the fraction done, from 0 to 1
    Normal(f64),
    Paused(f64),
    Error(f64),
}

fn fraction(done: f64) -> f64 {
    if done.is_finite() {
        done.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

// a count past two digits reads as 99+, as chromium's badges do
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn badge_text(count: u64) -> String {
    match count {
        0..=99 => count.to_string(),
        _ => "99+".to_string(),
    }
}

fn window_missing(window: &str) -> String {
    format!("the {window} window is not open")
}

#[cfg(target_os = "windows")]
mod platform {
    use super::{Progress, WindowControl, badge_text, fraction, window_missing};
    use ceri_engine::NativeHandle;
    use std::sync::OnceLock;
    use std::sync::mpsc::{self, Receiver, Sender};
    use windows::Win32::Foundation::{COLORREF, HWND};
    use windows::Win32::Graphics::Gdi::{
        ANTIALIASED_QUALITY, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CLIP_DEFAULT_PRECIS,
        CreateCompatibleDC, CreateDIBSection, CreateFontW, DEFAULT_CHARSET, DIB_RGB_COLORS,
        DeleteDC, DeleteObject, FW_NORMAL, GdiFlush, OUT_DEFAULT_PRECIS, SelectObject, SetBkMode,
        SetTextColor, TRANSPARENT, TextOutW,
    };
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
    };
    use windows::Win32::UI::Shell::{
        ITaskbarList3, TBPF_ERROR, TBPF_INDETERMINATE, TBPF_NOPROGRESS, TBPF_NORMAL, TBPF_PAUSED,
        TaskbarList,
    };
    use windows::Win32::UI::WindowsAndMessaging::{CreateIcon, DestroyIcon, HICON};
    use windows::core::{HSTRING, w};

    type Job = Box<dyn FnOnce(&ITaskbarList3) + Send>;

    const SIDE: usize = 16;

    #[derive(Default)]
    pub(crate) struct Taskbar {
        jobs: OnceLock<Result<Sender<Job>, String>>,
    }

    impl Taskbar {
        pub(crate) fn new(_identifier: &str) -> Self {
            Self::default()
        }

        pub(crate) fn set_progress(
            &self,
            control: &WindowControl,
            window: &str,
            progress: Progress,
        ) -> Result<(), String> {
            let hwnd = hwnd(control, window).ok_or_else(|| window_missing(window))?;
            let (state, done) = match progress {
                Progress::None => (TBPF_NOPROGRESS, None),
                Progress::Indeterminate => (TBPF_INDETERMINATE, None),
                Progress::Normal(done) => (TBPF_NORMAL, Some(done)),
                Progress::Paused(done) => (TBPF_PAUSED, Some(done)),
                Progress::Error(done) => (TBPF_ERROR, Some(done)),
            };
            self.run(Box::new(move |taskbar| {
                let hwnd = HWND(hwnd as _);
                // SAFETY: the taskbar ignores a window that closed since; the value goes first
                // because setting one leaves the indeterminate state
                unsafe {
                    if let Some(done) = done {
                        let _ = taskbar.SetProgressValue(
                            hwnd,
                            (fraction(done) * 10_000.0).round() as u64,
                            10_000,
                        );
                    }
                    let _ = taskbar.SetProgressState(hwnd, state);
                }
            }))
        }

        // the overlay is a window's own, so every open window carries the count
        pub(crate) fn set_badge_count(
            &self,
            control: &WindowControl,
            count: Option<u64>,
        ) -> Result<(), String> {
            let windows: Vec<isize> = control
                .labels()
                .iter()
                .filter_map(|label| hwnd(control, label))
                .collect();
            self.run(Box::new(move |taskbar| {
                let icon = count.and_then(|count| badge_icon(&badge_text(count)));
                let description =
                    HSTRING::from(count.map(|count| count.to_string()).unwrap_or_default());
                for hwnd in windows {
                    // SAFETY: the taskbar copies the icon and ignores a window that closed since
                    let _ = unsafe {
                        taskbar.SetOverlayIcon(
                            HWND(hwnd as _),
                            icon.unwrap_or_default(),
                            &description,
                        )
                    };
                }
                if let Some(icon) = icon {
                    // SAFETY: the icon was created here and the taskbar holds its own copy
                    let _ = unsafe { DestroyIcon(icon) };
                }
            }))
        }

        fn run(&self, job: Job) -> Result<(), String> {
            let jobs = self.jobs.get_or_init(|| {
                let (sender, jobs) = mpsc::channel();
                std::thread::Builder::new()
                    .name("ceri-taskbar".into())
                    .spawn(move || serve(jobs))
                    .map(|_| sender)
                    .map_err(|error| format!("could not start the taskbar thread: {error}"))
            });
            jobs.as_ref()
                .map_err(Clone::clone)?
                .send(job)
                .map_err(|_| "the taskbar is not available".to_string())
        }
    }

    fn hwnd(control: &WindowControl, window: &str) -> Option<isize> {
        match control.state(window)?.handle? {
            NativeHandle::Win32 { hwnd } => Some(hwnd),
            _ => None,
        }
    }

    // SetOverlayIcon runs a nested message loop, so the taskbar gets an apartment of its own
    // rather than the window loop's, as chromium gives it
    fn serve(jobs: Receiver<Job>) {
        // SAFETY: the apartment is this new thread's and lasts as long as it does
        let taskbar = unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            CoCreateInstance::<_, ITaskbarList3>(&TaskbarList, None, CLSCTX_INPROC_SERVER)
                .and_then(|taskbar| taskbar.HrInit().map(|()| taskbar))
        };
        match taskbar {
            Ok(taskbar) => jobs.iter().for_each(|job| job(&taskbar)),
            Err(error) => eprintln!("ceri: the taskbar is not available: {error}"),
        }
    }

    fn badge_icon(text: &str) -> Option<HICON> {
        let bgra = badge(text)?;
        // an all-clear AND mask, rows padded to 16 bits, so the alpha channel alone decides
        let mask = [0u8; SIDE.div_ceil(16) * 2 * SIDE];
        // SAFETY: both buffers hold the rows CreateIcon reads for a 16 by 16 icon
        unsafe {
            CreateIcon(
                None,
                SIDE as i32,
                SIDE as i32,
                1,
                32,
                mask.as_ptr(),
                bgra.as_ptr(),
            )
        }
        .ok()
    }

    // chromium's web app badge: white text in the default font, as large as fits 3 pixels in
    // from the edge, on a disc in the colour of the windows badge api; straight-alpha bgra
    pub(super) fn badge(text: &str) -> Option<Vec<u8>> {
        const BOUNDS: usize = SIDE - 2 * 3;
        const BACKGROUND: [f32; 3] = [0x2d as f32, 0x25 as f32, 0x26 as f32];
        let text: Vec<u16> = text.encode_utf16().collect();
        let (coverage, [left, top, right, bottom]) = (7..=24)
            .rev()
            .filter_map(|size| glyphs(&text, size))
            .find(|(_, [left, top, right, bottom])| {
                right - left <= BOUNDS && bottom - top <= BOUNDS
            })
            .or_else(|| glyphs(&text, 7))?;
        // the ink's centre lands on the disc's
        let shift_x = (SIDE as isize - (left + right) as isize) / 2;
        let shift_y = (SIDE as isize - (top + bottom) as isize) / 2;
        let mut bgra = vec![0u8; SIDE * SIDE * 4];
        for (index, pixel) in bgra.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let (x, y) = ((index % SIDE) as isize, (index / SIDE) as isize);
            let disc = disc_coverage(x as f32, y as f32);
            let (glyph_x, glyph_y) = (x - shift_x, y - shift_y);
            let ink = match (usize::try_from(glyph_x), usize::try_from(glyph_y)) {
                (Ok(glyph_x), Ok(glyph_y)) if glyph_x < CANVAS && glyph_y < CANVAS => {
                    f32::from(coverage[glyph_y * CANVAS + glyph_x]) / 255.0
                }
                _ => 0.0,
            };
            let alpha = ink + disc * (1.0 - ink);
            if alpha <= 0.0 {
                continue;
            }
            for (channel, background) in pixel[..3].iter_mut().zip(BACKGROUND) {
                *channel = ((255.0 * ink + background * disc * (1.0 - ink)) / alpha).round() as u8;
            }
            pixel[3] = (alpha * 255.0).round() as u8;
        }
        Some(bgra)
    }

    // the share of a pixel inside the badge's disc, from 4 by 4 samples
    fn disc_coverage(x: f32, y: f32) -> f32 {
        let radius = SIDE as f32 / 2.0;
        let inside = (0..16)
            .filter(|sample| {
                let dx = x + (f32::from(*sample as u8 % 4) + 0.5) / 4.0 - radius;
                let dy = y + (f32::from(*sample as u8 / 4) + 0.5) / 4.0 - radius;
                dx * dx + dy * dy <= radius * radius
            })
            .count();
        inside as f32 / 16.0
    }

    const CANVAS: usize = 64;

    // the text's grey coverage drawn by gdi at a pixel size, and its ink's bounds
    fn glyphs(text: &[u16], size: i32) -> Option<(Vec<u8>, [usize; 4])> {
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: CANVAS as i32,
                biHeight: -(CANVAS as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        // SAFETY: every object made here is selected out and deleted before returning, and the
        // bits are read only while their bitmap lives
        let coverage = unsafe {
            let dc = CreateCompatibleDC(None);
            let mut bits = std::ptr::null_mut();
            let bitmap = CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0);
            let coverage = bitmap.ok().map(|bitmap| {
                let font = CreateFontW(
                    -size,
                    0,
                    0,
                    0,
                    FW_NORMAL.0 as i32,
                    0,
                    0,
                    0,
                    DEFAULT_CHARSET,
                    OUT_DEFAULT_PRECIS,
                    CLIP_DEFAULT_PRECIS,
                    ANTIALIASED_QUALITY,
                    0,
                    w!("Segoe UI"),
                );
                let old_bitmap = SelectObject(dc, bitmap.into());
                let old_font = SelectObject(dc, font.into());
                SetBkMode(dc, TRANSPARENT);
                SetTextColor(dc, COLORREF(0x00ff_ffff));
                let _ = TextOutW(dc, 16, 16, text);
                let _ = GdiFlush();
                let pixels = std::slice::from_raw_parts(bits.cast::<u8>(), CANVAS * CANVAS * 4);
                let coverage: Vec<u8> = pixels
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|pixel| pixel[1])
                    .collect();
                SelectObject(dc, old_font);
                SelectObject(dc, old_bitmap);
                let _ = DeleteObject(font.into());
                let _ = DeleteObject(bitmap.into());
                coverage
            });
            let _ = DeleteDC(dc);
            coverage?
        };
        let inked = |index: &usize| coverage[*index] > 0;
        let rows: Vec<usize> = (0..CANVAS)
            .filter(|row| (row * CANVAS..(row + 1) * CANVAS).any(|index| inked(&index)))
            .collect();
        let columns: Vec<usize> = (0..CANVAS)
            .filter(|column| (0..CANVAS).any(|row| inked(&(row * CANVAS + column))))
            .collect();
        let bounds = [
            *columns.first()?,
            *rows.first()?,
            columns.last()? + 1,
            rows.last()? + 1,
        ];
        Some((coverage, bounds))
    }
}

#[cfg(not(target_os = "windows"))]
mod platform {
    use super::{Progress, WindowControl, fraction, window_missing};
    use crate::dbus::{Bus, Message, Value};
    use std::sync::{Arc, OnceLock};

    pub(crate) struct Taskbar {
        identifier: String,
        // held for the app's life, since docks forget what a sender set once it leaves the bus
        bus: OnceLock<Result<Arc<Bus>, String>>,
    }

    impl Taskbar {
        pub(crate) fn new(identifier: &str) -> Self {
            Self {
                identifier: identifier.to_string(),
                bus: OnceLock::new(),
            }
        }

        // the launcher shows one progress for the whole app, and has no indeterminate state
        pub(crate) fn set_progress(
            &self,
            control: &WindowControl,
            window: &str,
            progress: Progress,
        ) -> Result<(), String> {
            control
                .state(window)
                .ok_or_else(|| window_missing(window))?;
            self.update(match progress {
                Progress::None | Progress::Indeterminate => {
                    vec![("progress-visible", Value::Bool(false))]
                }
                Progress::Normal(done) | Progress::Paused(done) | Progress::Error(done) => vec![
                    ("progress", Value::Double(fraction(done))),
                    ("progress-visible", Value::Bool(true)),
                ],
            })
        }

        pub(crate) fn set_badge_count(
            &self,
            _control: &WindowControl,
            count: Option<u64>,
        ) -> Result<(), String> {
            self.update(vec![
                (
                    "count",
                    Value::Int64(count.map_or(0, |count| count.min(i64::MAX as u64) as i64)),
                ),
                ("count-visible", Value::Bool(count.is_some())),
            ])
        }

        pub(super) fn update(&self, properties: Vec<(&'static str, Value)>) -> Result<(), String> {
            let bus = self
                .bus
                .get_or_init(|| Bus::session(|_, _| {}))
                .as_ref()
                .map_err(Clone::clone)?;
            bus.send(&launcher_update(&self.identifier, properties))
                .map(drop)
        }
    }

    // the desktop entry is named for the identifier, as the windows' app id is
    pub(super) fn launcher_update(
        identifier: &str,
        properties: Vec<(&'static str, Value)>,
    ) -> Message {
        let element: String = identifier
            .chars()
            .map(|character| match character.is_ascii_alphanumeric() {
                true => character,
                false => '_',
            })
            .collect();
        Message::signal(
            &format!("/com/canonical/unity/launcherentry/{element}"),
            "com.canonical.Unity.LauncherEntry",
            "Update",
            vec![
                Value::str(format!("application://{identifier}.desktop")),
                Value::dict(properties),
            ],
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_and_fractions_keep_to_what_a_badge_and_a_bar_show() {
        assert_eq!(badge_text(7), "7");
        assert_eq!(badge_text(99), "99");
        assert_eq!(badge_text(100), "99+");
        assert_eq!(fraction(0.25), 0.25);
        assert_eq!(fraction(1.5), 1.0);
        assert_eq!(fraction(-1.0), 0.0);
        assert_eq!(fraction(f64::NAN), 0.0);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn the_badge_is_a_dark_disc_with_white_ink_in_its_middle() {
        let badge = platform::badge("7").expect("the badge should draw");
        let pixel = |x: usize, y: usize| &badge[(y * 16 + x) * 4..(y * 16 + x) * 4 + 4];
        assert_eq!(pixel(0, 0)[3], 0);
        assert_eq!(pixel(15, 15)[3], 0);
        assert_eq!(pixel(2, 8), [0x2d, 0x25, 0x26, 255]);
        let lightest = (5..11)
            .flat_map(|y| (5..11).map(move |x| (x, y)))
            .map(|(x, y)| pixel(x, y)[1])
            .max();
        assert!(lightest.is_some_and(|green| green > 200), "{lightest:?}");
        assert!(platform::badge("99+").is_some());
    }

    // needs a session bus, which the linux run starts with dbus-run-session
    #[cfg(not(target_os = "windows"))]
    #[test]
    fn a_dock_on_the_session_bus_receives_the_count() {
        use crate::dbus::{Bus, Message, Value};
        use std::sync::{Mutex, mpsc};
        use std::time::Duration;
        if crate::dbus::session_address().is_err() {
            return;
        }
        let (heard, hearing) = mpsc::channel();
        let heard = Mutex::new(heard);
        let dock = Bus::session(move |_, message: Message| {
            if message.member.as_deref() == Some("Update") {
                let _ = heard.lock().expect("heard poisoned").send(message.body);
            }
        })
        .expect("the session bus should take a connection");
        dock.call(Message::call(
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "AddMatch",
            vec![Value::str(
                "type='signal',interface='com.canonical.Unity.LauncherEntry'",
            )],
        ))
        .expect("the bus should take the match");
        let taskbar = Taskbar::new("dev.ceri.taskbar-test");
        taskbar
            .update(vec![
                ("count", Value::Int64(4)),
                ("count-visible", Value::Bool(true)),
            ])
            .expect("the update should send");
        let body = hearing
            .recv_timeout(Duration::from_secs(5))
            .expect("the dock should hear the update");
        assert_eq!(
            body[0],
            Value::str("application://dev.ceri.taskbar-test.desktop")
        );
        assert_eq!(body[1].get("count"), Some(&Value::Int64(4)));
        assert_eq!(body[1].get("count-visible"), Some(&Value::Bool(true)));
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn the_launcher_hears_of_the_app_by_its_desktop_entry() {
        use crate::dbus::Value;
        let message = platform::launcher_update(
            "lt.rot.clip-recorder",
            vec![
                ("count", Value::Int64(3)),
                ("count-visible", Value::Bool(true)),
            ],
        );
        assert_eq!(
            message.path.as_deref(),
            Some("/com/canonical/unity/launcherentry/lt_rot_clip_recorder")
        );
        assert_eq!(message.member.as_deref(), Some("Update"));
        assert_eq!(
            message.body[0],
            Value::str("application://lt.rot.clip-recorder.desktop")
        );
        assert_eq!(message.body[1].signature(), "a{sv}");
        assert_eq!(message.body[1].get("count"), Some(&Value::Int64(3)));
    }
}
