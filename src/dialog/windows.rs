use super::{FileDialog, InputDialog, MessageButtons, MessageDialog, MessageLevel, Pick};
use ceri_engine::NativeHandle;
use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;
use windows::Win32::Foundation::{ERROR_CANCELLED, HWND, LPARAM, WPARAM};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoCreateInstance,
    CoInitializeEx, CoTaskMemFree, CoUninitialize,
};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress, LoadLibraryW};
use windows::Win32::UI::Controls::{
    EM_SETSEL, TASKDIALOG_BUTTON, TASKDIALOGCONFIG, TD_ERROR_ICON, TD_INFORMATION_ICON,
    TD_WARNING_ICON, TDF_ALLOW_DIALOG_CANCELLATION, TDF_POSITION_RELATIVE_TO_WINDOW,
};
use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
use windows::Win32::UI::Shell::{
    FOS_ALLOWMULTISELECT, FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM, FOS_OVERWRITEPROMPT,
    FOS_PATHMUSTEXIST, FOS_PICKFOLDERS, FileOpenDialog, FileSaveDialog, IFileDialog,
    IFileOpenDialog, IFileSaveDialog, IShellItem, SHCreateItemFromParsingName, SIGDN_FILESYSPATH,
};
use windows::Win32::UI::WindowsAndMessaging::{
    BS_DEFPUSHBUTTON, BS_PUSHBUTTON, DLGTEMPLATE, DS_CENTER, DS_MODALFRAME, DS_SETFONT,
    DialogBoxIndirectParamW, ES_AUTOHSCROLL, EndDialog, GWLP_USERDATA, GetDlgItemTextW,
    GetWindowLongPtrW, IDCANCEL, IDOK, IDYES, MB_ICONERROR, MB_ICONINFORMATION, MB_ICONWARNING,
    MB_OK, MB_OKCANCEL, MB_YESNO, MessageBoxW, SendDlgItemMessageW, SetDlgItemTextW,
    SetWindowLongPtrW, WM_COMMAND, WM_INITDIALOG, WS_BORDER, WS_CAPTION, WS_CHILD, WS_POPUP,
    WS_SYSMENU, WS_TABSTOP, WS_VISIBLE,
};
use windows::core::{HSTRING, Interface, PCWSTR};

fn owner(handle: Option<NativeHandle>) -> Option<HWND> {
    match handle {
        Some(NativeHandle::Win32 { hwnd }) => Some(HWND(hwnd as *mut _)),
        _ => None,
    }
}

// the shell's dialogs want a single-threaded apartment, which the calling thread may not be
fn in_apartment<T: Send>(
    run: impl FnOnce() -> windows::core::Result<T> + Send,
) -> Result<T, String> {
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                // SAFETY: the thread is new, so this is its first and only COM initialization
                let initialized = unsafe {
                    CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE)
                };
                if initialized.is_err() {
                    return Err(initialized.message());
                }
                let result = run().map_err(|error| error.message());
                // SAFETY: balances the initialization above, after every COM object is released
                unsafe { CoUninitialize() };
                result
            })
            .join()
            .map_err(|_| "the dialog stopped with a panic".to_string())?
    })
}

fn pattern(extension: &str) -> String {
    match extension {
        "*" => "*.*".to_string(),
        extension => format!("*.{extension}"),
    }
}

pub(super) fn pick(dialog: &FileDialog, pick: Pick) -> Result<Option<Vec<PathBuf>>, String> {
    let (folder, name) = dialog.start();
    let specs: Vec<(HSTRING, HSTRING)> = match pick {
        Pick::Folders => Vec::new(),
        _ => dialog
            .filters
            .iter()
            .map(|filter| {
                let patterns: Vec<_> = filter.extensions.iter().map(|e| pattern(e)).collect();
                (
                    HSTRING::from(filter.name.as_str()),
                    HSTRING::from(patterns.join(";")),
                )
            })
            .collect(),
    };
    let default_extension = dialog
        .filters
        .first()
        .and_then(|filter| filter.extensions.first())
        .filter(|extension| *extension != "*");
    in_apartment(|| {
        // SAFETY: each call gets live COM objects and strings that outlive it, all on this thread
        unsafe {
            let file_dialog: IFileDialog = match pick {
                Pick::Save => CoCreateInstance::<_, IFileSaveDialog>(
                    &FileSaveDialog,
                    None,
                    CLSCTX_INPROC_SERVER,
                )?
                .cast()?,
                _ => CoCreateInstance::<_, IFileOpenDialog>(
                    &FileOpenDialog,
                    None,
                    CLSCTX_INPROC_SERVER,
                )?
                .cast()?,
            };
            let mut options = file_dialog.GetOptions()? | FOS_FORCEFILESYSTEM;
            options |= match pick {
                Pick::Files => FOS_FILEMUSTEXIST,
                Pick::Folders => FOS_PICKFOLDERS | FOS_PATHMUSTEXIST,
                Pick::Save => FOS_OVERWRITEPROMPT | FOS_PATHMUSTEXIST,
            };
            if dialog.multiple && pick != Pick::Save {
                options |= FOS_ALLOWMULTISELECT;
            }
            file_dialog.SetOptions(options)?;
            if let Some(title) = &dialog.title {
                file_dialog.SetTitle(&HSTRING::from(title.as_str()))?;
            }
            if !specs.is_empty() {
                let raw: Vec<_> = specs
                    .iter()
                    .map(|(name, spec)| COMDLG_FILTERSPEC {
                        pszName: PCWSTR(name.as_ptr()),
                        pszSpec: PCWSTR(spec.as_ptr()),
                    })
                    .collect();
                file_dialog.SetFileTypes(&raw)?;
            }
            if pick == Pick::Save
                && let Some(extension) = default_extension
            {
                file_dialog.SetDefaultExtension(&HSTRING::from(extension.as_str()))?;
            }
            // a starting folder that has gone away leaves the shell's own choice
            if let Some(folder) = &folder
                && let Ok(item) = SHCreateItemFromParsingName::<_, _, IShellItem>(
                    &HSTRING::from(folder.as_path()),
                    None,
                )
            {
                file_dialog.SetFolder(&item)?;
            }
            if let Some(name) = &name {
                file_dialog.SetFileName(&HSTRING::from(name.as_str()))?;
            }
            match file_dialog.Show(owner(dialog.owner)) {
                Err(error) if error.code() == ERROR_CANCELLED.to_hresult() => return Ok(None),
                shown => shown?,
            }
            let items = match pick {
                Pick::Save => vec![file_dialog.GetResult()?],
                _ => {
                    let results = file_dialog.cast::<IFileOpenDialog>()?.GetResults()?;
                    (0..results.GetCount()?)
                        .map(|index| results.GetItemAt(index))
                        .collect::<windows::core::Result<Vec<_>>>()?
                }
            };
            let mut paths = Vec::with_capacity(items.len());
            for item in items {
                let name = item.GetDisplayName(SIGDN_FILESYSPATH)?;
                paths.push(PathBuf::from(OsString::from_wide(name.as_wide())));
                CoTaskMemFree(Some(name.0 as *const _));
            }
            Ok(Some(paths))
        }
    })
}

type TaskDialogIndirect = unsafe extern "system" fn(
    *const TASKDIALOGCONFIG,
    *mut i32,
    *mut i32,
    *mut windows::core::BOOL,
) -> windows::core::HRESULT;

// buttons with words of their own; the task dialog only exists in common controls 6, which an
// app has when its manifest asks for it, so it is found at run time and none falls back
fn labelled(dialog: &MessageDialog) -> Option<Result<bool, String>> {
    if dialog.ok_label.is_none() && dialog.cancel_label.is_none() {
        return None;
    }
    // SAFETY: comctl32 is a system library that stays loaded, and the name is terminated
    let show: TaskDialogIndirect = unsafe {
        let library = LoadLibraryW(windows::core::w!("comctl32.dll")).ok()?;
        std::mem::transmute(GetProcAddress(
            library,
            windows::core::s!("TaskDialogIndirect"),
        )?)
    };
    let (yes, no) = match dialog.buttons {
        MessageButtons::YesNo => ("Yes", "No"),
        _ => ("OK", "Cancel"),
    };
    let accept = HSTRING::from(dialog.ok_label.as_deref().unwrap_or(yes));
    let refuse = HSTRING::from(dialog.cancel_label.as_deref().unwrap_or(no));
    let mut buttons = vec![TASKDIALOG_BUTTON {
        nButtonID: IDOK.0,
        pszButtonText: PCWSTR(accept.as_ptr()),
    }];
    if dialog.buttons != MessageButtons::Ok {
        buttons.push(TASKDIALOG_BUTTON {
            nButtonID: IDCANCEL.0,
            pszButtonText: PCWSTR(refuse.as_ptr()),
        });
    }
    let (title, text) = (
        HSTRING::from(dialog.title.as_str()),
        HSTRING::from(dialog.text.as_str()),
    );
    let mut config = TASKDIALOGCONFIG {
        cbSize: size_of::<TASKDIALOGCONFIG>() as u32,
        hwndParent: owner(dialog.owner).unwrap_or_default(),
        // escape and the close box answer as the cancel button would
        dwFlags: TDF_ALLOW_DIALOG_CANCELLATION | TDF_POSITION_RELATIVE_TO_WINDOW,
        pszWindowTitle: PCWSTR(title.as_ptr()),
        pszContent: PCWSTR(text.as_ptr()),
        cButtons: buttons.len() as u32,
        pButtons: buttons.as_ptr(),
        nDefaultButton: IDOK.0,
        ..Default::default()
    };
    config.Anonymous1.pszMainIcon = match dialog.level {
        MessageLevel::Info => TD_INFORMATION_ICON,
        MessageLevel::Warning => TD_WARNING_ICON,
        MessageLevel::Error => TD_ERROR_ICON,
    };
    let mut chosen = 0;
    // SAFETY: the configuration, its strings and buttons outlive the call
    let result = unsafe {
        show(
            &config,
            &mut chosen,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    Some(
        result
            .ok()
            .map(|()| chosen == IDOK.0)
            .map_err(|error| error.message()),
    )
}

const PROMPT_EDIT: i32 = 100;

struct Prompt {
    default: HSTRING,
    answer: Option<String>,
}

// one control of a dialog template: its style, place in dialog units, id, class atom and text
fn template_item(out: &mut Vec<u16>, style: u32, place: [i16; 4], id: u16, class: u16, text: &str) {
    if out.len() % 2 == 1 {
        out.push(0);
    }
    out.extend([style as u16, (style >> 16) as u16, 0, 0]);
    out.extend(place.map(|value| value as u16));
    out.extend([id, 0xffff, class]);
    out.extend(text.encode_utf16().chain([0]));
    out.push(0);
}

// a modal dialog with the message, an edit box holding the default, and OK and Cancel; windows
// has no stock one, so it is laid out in memory the way a resource script would describe it
pub(super) fn input(dialog: &InputDialog) -> Result<Option<String>, String> {
    let lines: usize = dialog
        .text
        .lines()
        .map(|line| line.chars().count().div_ceil(52).max(1))
        .sum::<usize>()
        .clamp(1, 24);
    let text_height = (lines * 9) as i16;
    let edit_top = 7 + text_height + 4;
    let buttons_top = edit_top + 14 + 8;
    let style =
        (WS_POPUP | WS_CAPTION | WS_SYSMENU).0 | (DS_MODALFRAME | DS_CENTER | DS_SETFONT) as u32;
    let mut template: Vec<u16> = vec![style as u16, (style >> 16) as u16, 0, 0, 4];
    template.extend([0, 0, 260, (buttons_top + 14 + 7) as u16, 0, 0]);
    template.extend(dialog.title.encode_utf16().chain([0]));
    template.push(9);
    template.extend("Segoe UI".encode_utf16().chain([0]));
    let child = (WS_CHILD | WS_VISIBLE).0;
    template_item(
        &mut template,
        child,
        [7, 7, 246, text_height],
        0xffff,
        0x82,
        &dialog.text,
    );
    template_item(
        &mut template,
        child | (WS_BORDER | WS_TABSTOP).0 | ES_AUTOHSCROLL as u32,
        [7, edit_top, 246, 14],
        PROMPT_EDIT as u16,
        0x81,
        "",
    );
    template_item(
        &mut template,
        child | WS_TABSTOP.0 | BS_DEFPUSHBUTTON as u32,
        [149, buttons_top, 50, 14],
        IDOK.0 as u16,
        0x80,
        "OK",
    );
    template_item(
        &mut template,
        child | WS_TABSTOP.0 | BS_PUSHBUTTON as u32,
        [203, buttons_top, 50, 14],
        IDCANCEL.0 as u16,
        0x80,
        "Cancel",
    );
    // the template and each control in it start on a four byte boundary
    let aligned: Vec<u32> = template
        .chunks(2)
        .map(|pair| u32::from(pair[0]) | u32::from(pair.get(1).copied().unwrap_or(0)) << 16)
        .collect();
    let mut prompt = Prompt {
        default: HSTRING::from(dialog.default.as_str()),
        answer: None,
    };
    // SAFETY: the template and the prompt outlive the modal loop, which is the only reader of
    // the pointer the dialog keeps
    let result = unsafe {
        let instance = GetModuleHandleW(None).map_err(|error| error.message())?;
        DialogBoxIndirectParamW(
            Some(instance.into()),
            aligned.as_ptr().cast::<DLGTEMPLATE>(),
            None,
            Some(prompt_procedure),
            LPARAM(&raw mut prompt as isize),
        )
    };
    match result {
        -1 => Err(windows::core::Error::from_thread().message()),
        _ => Ok(prompt.answer),
    }
}

unsafe extern "system" fn prompt_procedure(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> isize {
    // SAFETY: the user data is the prompt the caller of the modal loop owns, set before any
    // command can arrive
    unsafe {
        match message {
            WM_INITDIALOG => {
                SetWindowLongPtrW(window, GWLP_USERDATA, lparam.0);
                let prompt = &*(lparam.0 as *const Prompt);
                let _ = SetDlgItemTextW(window, PROMPT_EDIT, &prompt.default);
                SendDlgItemMessageW(window, PROMPT_EDIT, EM_SETSEL, WPARAM(0), LPARAM(-1));
                1
            }
            WM_COMMAND => {
                let id = (wparam.0 & 0xffff) as i32;
                if id == IDOK.0 {
                    let prompt = &mut *(GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Prompt);
                    let mut text = vec![0_u16; 32 * 1024];
                    let length = GetDlgItemTextW(window, PROMPT_EDIT, &mut text) as usize;
                    prompt.answer = Some(String::from_utf16_lossy(&text[..length]));
                    let _ = EndDialog(window, 1);
                } else if id == IDCANCEL.0 {
                    let _ = EndDialog(window, 0);
                }
                1
            }
            _ => 0,
        }
    }
}

pub(super) fn message(dialog: &MessageDialog) -> Result<bool, String> {
    if let Some(answer) = labelled(dialog) {
        return answer;
    }
    let buttons = match dialog.buttons {
        MessageButtons::Ok => MB_OK,
        MessageButtons::OkCancel => MB_OKCANCEL,
        MessageButtons::YesNo => MB_YESNO,
    };
    let icon = match dialog.level {
        MessageLevel::Info => MB_ICONINFORMATION,
        MessageLevel::Warning => MB_ICONWARNING,
        MessageLevel::Error => MB_ICONERROR,
    };
    // SAFETY: both strings outlive the call, and the owner is a window or none
    let answer = unsafe {
        MessageBoxW(
            owner(dialog.owner),
            &HSTRING::from(dialog.text.as_str()),
            &HSTRING::from(dialog.title.as_str()),
            buttons | icon,
        )
    };
    match answer.0 {
        0 => Err(windows::core::Error::from_thread().message()),
        _ => Ok(answer == IDOK || answer == IDYES),
    }
}
