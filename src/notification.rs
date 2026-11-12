// desktop notifications under the app's own name: a toast on Windows, registered for the app id
// in the current user's classes, and the freedesktop notification service elsewhere

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub(crate) struct Notifier {
    identifier: String,
    name: String,
    icon_source: Option<Vec<u8>>,
    profile: PathBuf,
    // the icon file, written and the app id registered on the first notification
    icon: OnceLock<Option<PathBuf>>,
}

impl Notifier {
    pub(crate) fn new(
        identifier: &str,
        name: &str,
        icon_source: Option<Vec<u8>>,
        profile: &Path,
    ) -> Self {
        Self {
            identifier: identifier.to_string(),
            name: name.to_string(),
            icon_source,
            profile: profile.to_path_buf(),
            icon: OnceLock::new(),
        }
    }

    pub(crate) fn show(&self, title: &str, body: &str) -> Result<(), String> {
        let icon = self.icon.get_or_init(|| {
            let icon = self.write_icon();
            #[cfg(target_os = "windows")]
            if let Err(error) = platform::register(&self.identifier, &self.name, icon.as_deref()) {
                eprintln!(
                    "ceri: could not register {} for notifications: {error}",
                    self.identifier
                );
            }
            icon
        });
        platform::show(self, icon.as_deref(), title, body)
    }

    fn write_icon(&self) -> Option<PathBuf> {
        let png = ceri_engine::icon_png(self.icon_source.as_deref()?, 256)?;
        let path = self.profile.join("notification-icon.png");
        let written = fs::create_dir_all(&self.profile).and_then(|()| fs::write(&path, png));
        match written {
            Ok(()) => Some(path),
            Err(error) => {
                eprintln!(
                    "ceri: notifications go without an icon, {}: {error}",
                    path.display()
                );
                None
            }
        }
    }
}

// the five characters xml and the notification servers' markup give meaning to
fn escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            _ => escaped.push(character),
        }
    }
    escaped
}

#[cfg(target_os = "windows")]
mod platform {
    use super::{Notifier, escape};
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;
    use windows::Data::Xml::Dom::XmlDocument;
    use windows::UI::Notifications::{ToastNotification, ToastNotificationManager};
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, REG_SZ, RegSetKeyValueW};
    use windows::core::{HSTRING, PCWSTR, w};

    pub(super) fn key(identifier: &str) -> HSTRING {
        HSTRING::from(format!("Software\\Classes\\AppUserModelId\\{identifier}"))
    }

    // an unpackaged app's toasts show only for an app id the shell knows; this key is what the
    // windows app sdk writes for one, and it needs no shortcut
    pub(super) fn register(
        identifier: &str,
        name: &str,
        icon: Option<&Path>,
    ) -> Result<(), String> {
        let key = key(identifier);
        set(&key, w!("DisplayName"), name.as_ref())?;
        if let Some(icon) = icon {
            set(&key, w!("IconUri"), icon.as_os_str())?;
        }
        Ok(())
    }

    fn set(key: &HSTRING, name: PCWSTR, value: &OsStr) -> Result<(), String> {
        let wide: Vec<u16> = value.encode_wide().chain([0]).collect();
        // SAFETY: the value is a terminated wide string and the size counts its bytes
        unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                key,
                name,
                REG_SZ.0,
                Some(wide.as_ptr().cast()),
                (wide.len() * 2) as u32,
            )
        }
        .ok()
        .map_err(|error| error.message())
    }

    pub(super) fn show(
        notifier: &Notifier,
        _icon: Option<&Path>,
        title: &str,
        body: &str,
    ) -> Result<(), String> {
        let mut texts = format!("<text>{}</text>", escape(title));
        if !body.is_empty() {
            texts.push_str(&format!("<text>{}</text>", escape(body)));
        }
        let xml = format!(
            "<toast><visual><binding template=\"ToastGeneric\">{texts}</binding></visual></toast>"
        );
        let shown = || -> windows::core::Result<()> {
            let document = XmlDocument::new()?;
            document.LoadXml(&HSTRING::from(xml))?;
            let toast = ToastNotification::CreateToastNotification(&document)?;
            ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(
                notifier.identifier.as_str(),
            ))?
            .Show(&toast)
        };
        shown().map_err(|error| format!("could not show the notification: {}", error.message()))
    }
}

#[cfg(not(target_os = "windows"))]
mod platform {
    use super::{Notifier, escape};
    use crate::dbus::{Bus, Message, Value};
    use std::path::Path;

    fn call(member: &str, body: Vec<Value>) -> Message {
        Message::call(
            "org.freedesktop.Notifications",
            "/org/freedesktop/Notifications",
            "org.freedesktop.Notifications",
            member,
            body,
        )
    }

    pub(super) fn show(
        notifier: &Notifier,
        icon: Option<&Path>,
        title: &str,
        body: &str,
    ) -> Result<(), String> {
        let bus = Bus::session(|_, _| {})?;
        let shown = (|| {
            // a server that reads markup in the body would drop a bare < and what follows
            let capabilities = bus.call(call("GetCapabilities", Vec::new()))?;
            let markup = capabilities.first().is_some_and(|list| {
                list.items()
                    .iter()
                    .any(|item| item.as_str() == Some("body-markup"))
            });
            let body = match markup {
                true => escape(body),
                false => body.to_string(),
            };
            let icon = icon
                .map(|icon| icon.to_string_lossy().into_owned())
                .unwrap_or_default();
            bus.call(call(
                "Notify",
                vec![
                    Value::str(notifier.name.as_str()),
                    Value::Uint32(0),
                    Value::str(icon),
                    Value::str(title),
                    Value::str(body),
                    Value::Array("s".to_string(), Vec::new()),
                    // the desktop file named for the app id, which desktops group notices by
                    Value::dict([("desktop-entry", Value::str(notifier.identifier.as_str()))]),
                    Value::Int32(-1),
                ],
            ))
        })();
        bus.close();
        shown
            .map(drop)
            .map_err(|error| format!("could not show the notification: {error}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markup_characters_are_escaped() {
        assert_eq!(
            escape(r#"5 < 6 & "clips" > 'none'"#),
            "5 &lt; 6 &amp; &quot;clips&quot; &gt; &apos;none&apos;"
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn an_app_id_is_registered_under_its_name_and_icon() {
        use windows::Win32::System::Registry::{
            HKEY_CURRENT_USER, RRF_RT_REG_SZ, RegDeleteTreeW, RegGetValueW,
        };
        use windows::core::w;
        let identifier = format!("dev.ceri.notification-test-{}", std::process::id());
        let icon = std::env::temp_dir().join("ceri notification.png");
        platform::register(&identifier, "Ceri Test", Some(&icon)).expect("registers");
        let key = platform::key(&identifier);
        let read = |name| {
            let mut buffer = [0_u16; 260];
            let mut size = (buffer.len() * 2) as u32;
            // SAFETY: the buffer and its byte size describe the same memory
            unsafe {
                RegGetValueW(
                    HKEY_CURRENT_USER,
                    &key,
                    name,
                    RRF_RT_REG_SZ,
                    None,
                    Some(buffer.as_mut_ptr().cast()),
                    Some(&mut size),
                )
            }
            .ok()
            .expect("the value is there");
            String::from_utf16_lossy(&buffer[..size as usize / 2 - 1])
        };
        let name = read(w!("DisplayName"));
        let icon_uri = read(w!("IconUri"));
        // SAFETY: removes only the key this test wrote
        unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, &key) }
            .ok()
            .expect("the test key goes");
        assert_eq!(name, "Ceri Test");
        assert_eq!(icon_uri, icon.to_string_lossy());
    }
}
