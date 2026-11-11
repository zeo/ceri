use super::{Change, MenuItem, Shared, TrayEvent, TrayHandler, lock, pixels};
use std::sync::{Arc, Mutex};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress, LoadLibraryW};
use windows::Win32::UI::HiDpi::{GetDpiForSystem, GetSystemMetricsForDpi};
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NIM_SETFOCUS,
    NIM_SETVERSION, NIN_SELECT, NOTIFY_ICON_DATA_FLAGS, NOTIFY_ICON_MESSAGE, NOTIFYICON_VERSION_4,
    NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, ChangeWindowMessageFilterEx, CreateIcon, CreatePopupMenu, CreateWindowExW,
    DefWindowProcW, DestroyIcon, DestroyMenu, DestroyWindow, GWLP_USERDATA, GetSystemMetrics,
    GetWindowLongPtrW, HICON, IDI_APPLICATION, LoadIconW, MF_CHECKED, MF_GRAYED, MF_SEPARATOR,
    MF_STRING, MSGFLT_ALLOW, PostMessageW, RegisterClassW, RegisterWindowMessageW, SM_CXSMICON,
    SM_MENUDROPALIGNMENT, SetForegroundWindow, SetWindowLongPtrW, TPM_BOTTOMALIGN, TPM_LEFTALIGN,
    TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTALIGN, TPM_RIGHTBUTTON, TrackPopupMenu, WM_APP,
    WM_CONTEXTMENU, WM_NCDESTROY, WM_NULL, WNDCLASSW, WS_EX_TOOLWINDOW, WS_OVERLAPPED,
};
use windows::core::{PCSTR, PCWSTR, w};

const CALLBACK: u32 = WM_APP + 0x51;
const ICON_ID: u32 = 1;
// NIN_SELECT with NINF_KEY, sent when the icon is chosen with Space or Enter
const NIN_KEYSELECT: u32 = NIN_SELECT | 1;

struct Window {
    state: Shared,
    handler: TrayHandler,
    taskbar_created: u32,
    // the icon handle and whether it is ours to destroy
    icon: Mutex<(isize, bool)>,
}

pub(crate) struct Backend {
    window: isize,
    shared: Arc<Window>,
}

fn handle(window: isize) -> HWND {
    HWND(window as _)
}

impl Backend {
    pub(crate) fn create(
        _identifier: &str,
        state: Shared,
        handler: TrayHandler,
    ) -> Result<Self, String> {
        allow_dark_menus();
        // SAFETY: the class and window outlive no borrowed data, and the procedure only reads
        // the Window it is handed through GWLP_USERDATA
        unsafe {
            let instance = GetModuleHandleW(None).map_err(|error| error.to_string())?;
            let class = w!("ceri-tray");
            // a second tray finds the class already registered, which is fine
            RegisterClassW(&WNDCLASSW {
                lpfnWndProc: Some(procedure),
                hInstance: instance.into(),
                lpszClassName: class,
                ..Default::default()
            });
            // a hidden top-level window, since message-only windows miss the TaskbarCreated broadcast
            let window = CreateWindowExW(
                WS_EX_TOOLWINDOW,
                class,
                w!("ceri tray"),
                WS_OVERLAPPED,
                0,
                0,
                0,
                0,
                None,
                None,
                Some(instance.into()),
                None,
            )
            .map_err(|error| format!("could not create the tray window: {error}"))?;
            let taskbar_created = RegisterWindowMessageW(w!("TaskbarCreated"));
            // an elevated app would otherwise never hear that Explorer restarted
            let _ = ChangeWindowMessageFilterEx(window, taskbar_created, MSGFLT_ALLOW, None);
            let shared = Arc::new(Window {
                state,
                handler,
                taskbar_created,
                icon: Mutex::new((0, false)),
            });
            SetWindowLongPtrW(
                window,
                GWLP_USERDATA,
                Arc::into_raw(shared.clone()) as isize,
            );
            let backend = Self {
                window: window.0 as isize,
                shared,
            };
            backend.shared.load_icon();
            // at login Explorer may not be ready yet; TaskbarCreated adds the icon once it is
            backend.shared.add(window);
            Ok(backend)
        }
    }

    pub(crate) fn refresh(&self, change: Change) {
        let window = handle(self.window);
        match change {
            Change::Icon => {
                let old = self.shared.load_icon();
                self.shared.notify(window, NIM_MODIFY, NIF_ICON);
                destroy(old);
            }
            Change::Tooltip => {
                self.shared
                    .notify(window, NIM_MODIFY, NIF_TIP | NIF_SHOWTIP);
            }
            // the menu is built from the state each time it opens
            Change::Menu => {}
        }
    }

    pub(crate) fn remove(&self) {
        let window = handle(self.window);
        self.shared
            .notify(window, NIM_DELETE, NOTIFY_ICON_DATA_FLAGS(0));
        // SAFETY: the window belongs to this thread, the one App::run created it on
        let _ = unsafe { DestroyWindow(window) };
        destroy(*self.shared.icon.lock().expect("tray icon poisoned"));
    }
}

fn destroy((icon, owned): (isize, bool)) {
    if owned {
        // SAFETY: the icon was created by CreateIcon and nothing shows it any more
        let _ = unsafe { DestroyIcon(HICON(icon as _)) };
    }
}

fn wide(text: &str, limit: usize) -> Vec<u16> {
    let mut units: Vec<u16> = text.encode_utf16().take(limit - 1).collect();
    units.push(0);
    units
}

impl Window {
    // swaps in an icon built from the state, handing back the one it replaced
    fn load_icon(&self) -> (isize, bool) {
        // SAFETY: plain metric queries
        let size = unsafe { GetSystemMetricsForDpi(SM_CXSMICON, GetDpiForSystem()) }.max(16) as u32;
        let image = lock(&self.state).icon.clone();
        let created = pixels(image.as_deref(), size).and_then(|rgba| {
            let bgra: Vec<u8> = rgba
                .as_chunks::<4>()
                .0
                .iter()
                .flat_map(|[red, green, blue, alpha]| [*blue, *green, *red, *alpha])
                .collect();
            // an all-clear AND mask, rows padded to 16 bits, so the alpha channel alone decides
            let mask = vec![0u8; (size as usize).div_ceil(16) * 2 * size as usize];
            let side = size as i32;
            // SAFETY: both buffers hold the rows CreateIcon reads for a size by size icon
            unsafe { CreateIcon(None, side, side, 1, 32, mask.as_ptr(), bgra.as_ptr()) }.ok()
        });
        let next = match created {
            Some(icon) => (icon.0 as isize, true),
            // SAFETY: a stock icon, shared and never destroyed
            None => (
                unsafe { LoadIconW(None, IDI_APPLICATION) }.map_or(0, |icon| icon.0 as isize),
                false,
            ),
        };
        std::mem::replace(&mut *self.icon.lock().expect("tray icon poisoned"), next)
    }

    fn data(&self, window: HWND, flags: NOTIFY_ICON_DATA_FLAGS) -> NOTIFYICONDATAW {
        let mut data = NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: window,
            uID: ICON_ID,
            uFlags: flags,
            uCallbackMessage: CALLBACK,
            hIcon: HICON(self.icon.lock().expect("tray icon poisoned").0 as _),
            ..Default::default()
        };
        let tip = wide(&lock(&self.state).tooltip, data.szTip.len());
        data.szTip[..tip.len()].copy_from_slice(&tip);
        data
    }

    fn notify(
        &self,
        window: HWND,
        message: NOTIFY_ICON_MESSAGE,
        flags: NOTIFY_ICON_DATA_FLAGS,
    ) -> bool {
        let data = self.data(window, flags);
        // SAFETY: the structure is fully initialised and lives across the call
        unsafe { Shell_NotifyIconW(message, &data) }.as_bool()
    }

    fn add(&self, window: HWND) {
        if self.notify(
            window,
            NIM_ADD,
            NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP,
        ) {
            let mut data = self.data(window, NOTIFY_ICON_DATA_FLAGS(0));
            data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
            // SAFETY: as in notify
            let _ = unsafe { Shell_NotifyIconW(NIM_SETVERSION, &data) };
        }
    }

    fn show_menu(&self, window: HWND, x: i32, y: i32) {
        // the lock is dropped before the menu's modal loop, so other threads can still update it
        let items = lock(&self.state).menu.clone();
        // SAFETY: the menu lives until DestroyMenu below, and the strings until each append
        unsafe {
            let Ok(menu) = CreatePopupMenu() else { return };
            for (index, item) in items.iter().enumerate() {
                let _ = match item {
                    MenuItem::Separator => AppendMenuW(menu, MF_SEPARATOR, 0, None),
                    MenuItem::Item {
                        text,
                        enabled,
                        checked,
                        ..
                    } => {
                        let mut flags = MF_STRING;
                        if *checked == Some(true) {
                            flags |= MF_CHECKED;
                        }
                        if !enabled {
                            flags |= MF_GRAYED;
                        }
                        // an ampersand would underline the next letter
                        let label = wide(&text.replace('&', "&&"), usize::MAX);
                        AppendMenuW(menu, flags, index + 1, PCWSTR(label.as_ptr()))
                    }
                };
            }
            // without the foreground the menu stays open when the user clicks elsewhere
            let _ = SetForegroundWindow(window);
            let side = match GetSystemMetrics(SM_MENUDROPALIGNMENT) {
                0 => TPM_LEFTALIGN,
                _ => TPM_RIGHTALIGN,
            };
            let flags = TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN | side;
            let chosen = TrackPopupMenu(menu, flags, x, y, None, window, None).0;
            let _ = PostMessageW(Some(window), WM_NULL, WPARAM(0), LPARAM(0));
            let _ = DestroyMenu(menu);
            if chosen <= 0 {
                // dismissed, so focus goes back to the notification area for keyboard users
                self.notify(window, NIM_SETFOCUS, NOTIFY_ICON_DATA_FLAGS(0));
                return;
            }
            let Some(MenuItem::Item { id: wanted, .. }) = items.get(chosen as usize - 1) else {
                return;
            };
            // the menu may have changed while it was open, so the item is found again by id
            let mut state = lock(&self.state);
            let index = state
                .menu
                .iter()
                .position(|item| matches!(item, MenuItem::Item { id, .. } if id == wanted));
            let id = index.and_then(|index| state.choose(index));
            drop(state);
            if let Some(id) = id {
                (self.handler)(TrayEvent::Menu(id));
            }
        }
    }
}

unsafe extern "system" fn procedure(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // SAFETY: GWLP_USERDATA holds the Arc<Window> leaked in create until WM_NCDESTROY
    unsafe {
        let shared = GetWindowLongPtrW(window, GWLP_USERDATA) as *const Window;
        if shared.is_null() {
            return DefWindowProcW(window, message, wparam, lparam);
        }
        if message == WM_NCDESTROY {
            SetWindowLongPtrW(window, GWLP_USERDATA, 0);
            drop(Arc::from_raw(shared));
            return DefWindowProcW(window, message, wparam, lparam);
        }
        let shared = &*shared;
        if message == CALLBACK {
            // version 4 puts the event in the low word and the anchor point in wparam
            match lparam.0 as u32 & 0xffff {
                NIN_SELECT | NIN_KEYSELECT => (shared.handler)(TrayEvent::Click),
                WM_CONTEXTMENU => {
                    let x = i32::from(wparam.0 as u16 as i16);
                    let y = i32::from((wparam.0 >> 16) as u16 as i16);
                    shared.show_menu(window, x, y);
                }
                _ => {}
            }
            return LRESULT(0);
        }
        if message == shared.taskbar_created && message != 0 {
            shared.add(window);
            return LRESULT(0);
        }
        DefWindowProcW(window, message, wparam, lparam)
    }
}

// popup menus follow the system's dark mode once the app allows it; uxtheme exports the switch
// by ordinal only, SetPreferredAppMode from 1903 and AllowDarkModeForApp before, both taking 1
fn allow_dark_menus() {
    type SetPreferredAppMode = unsafe extern "system" fn(i32) -> i32;
    type RefreshPolicy = unsafe extern "system" fn();
    // SAFETY: both ordinals have kept these signatures in every uxtheme since Windows 10 1809
    unsafe {
        let Ok(uxtheme) = LoadLibraryW(w!("uxtheme.dll")) else {
            return;
        };
        let ordinal = |number: usize| GetProcAddress(uxtheme, PCSTR(number as *const u8));
        if let Some(set_mode) = ordinal(135) {
            let set_mode: SetPreferredAppMode = std::mem::transmute(set_mode);
            set_mode(1);
        }
        if let Some(refresh) = ordinal(104) {
            let refresh: RefreshPolicy = std::mem::transmute(refresh);
            refresh();
        }
    }
}
