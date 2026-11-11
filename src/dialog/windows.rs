use super::{FileDialog, MessageButtons, MessageDialog, MessageLevel, Pick};
use ceri_engine::NativeHandle;
use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;
use windows::Win32::Foundation::{ERROR_CANCELLED, HWND};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoCreateInstance,
    CoInitializeEx, CoTaskMemFree, CoUninitialize,
};
use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
use windows::Win32::UI::Shell::{
    FOS_ALLOWMULTISELECT, FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM, FOS_OVERWRITEPROMPT,
    FOS_PATHMUSTEXIST, FOS_PICKFOLDERS, FileOpenDialog, FileSaveDialog, IFileDialog,
    IFileOpenDialog, IFileSaveDialog, IShellItem, SHCreateItemFromParsingName, SIGDN_FILESYSPATH,
};
use windows::Win32::UI::WindowsAndMessaging::{
    IDOK, IDYES, MB_ICONERROR, MB_ICONINFORMATION, MB_ICONWARNING, MB_OK, MB_OKCANCEL, MB_YESNO,
    MessageBoxW,
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

pub(super) fn message(dialog: &MessageDialog) -> Result<bool, String> {
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
