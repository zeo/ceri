use std::path::{Path, PathBuf};

const RT_ICON: u16 = 3;
const RT_GROUP_ICON: u16 = 14;
const RT_VERSION: u16 = 16;
const RT_MANIFEST: u16 = 24;
const EN_US: u16 = 0x0409;

// the manifest tauri-build embeds by default, so dialogs draw with common controls v6, and an
// as-invoker level so windows never guesses from a name like setup.exe that it needs elevation
const MANIFEST: &str = r#"<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <dependency>
    <dependentAssembly>
      <assemblyIdentity type="win32" name="Microsoft.Windows.Common-Controls" version="6.0.0.0" processorArchitecture="*" publicKeyToken="6595b64144ccf1df" language="*" />
    </dependentAssembly>
  </dependency>
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="asInvoker" uiAccess="false" />
      </requestedPrivileges>
    </security>
  </trustInfo>
</assembly>
"#;

// what a Windows executable carries about itself: the icon Explorer shows for the file and
// the details its properties list, with the product named after the package by default
#[derive(Clone, Copy, Debug, Default)]
pub struct Resources<'a> {
    pub icon: Option<&'a Path>,
    pub product_name: Option<&'a str>,
    pub description: Option<&'a str>,
    pub company: Option<&'a str>,
    pub copyright: Option<&'a str>,
}

// from an app's build script: links the resources into its binaries on MSVC targets and does
// nothing elsewhere
pub fn resources(resources: Resources) {
    let variable = |key| std::env::var(key).unwrap_or_default();
    if variable("CARGO_CFG_TARGET_OS") != "windows" || variable("CARGO_CFG_TARGET_ENV") != "msvc" {
        return;
    }
    println!("cargo::rerun-if-changed=Cargo.toml");
    let icon = resources.icon.map(|path| {
        println!("cargo::rerun-if-changed={}", path.display());
        std::fs::read(path)
            .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()))
    });
    let file = PathBuf::from(variable("OUT_DIR")).join("ceri-resources.res");
    let name = variable("CARGO_PKG_NAME");
    let compiled = res(
        &resources,
        icon.as_deref(),
        &name,
        &variable("CARGO_PKG_VERSION"),
    );
    std::fs::write(&file, compiled).expect("the build script can write to OUT_DIR");
    // link.exe takes a compiled resource file as an input of its own
    println!("cargo::rustc-link-arg-bins={}", file.display());
}

fn res(resources: &Resources, icon: Option<&[u8]>, name: &str, version: &str) -> Vec<u8> {
    // every .res file opens with an empty entry that marks it as 32-bit
    let mut file = vec![0, 0, 0, 0, 32, 0, 0, 0, 0xff, 0xff, 0, 0, 0xff, 0xff, 0, 0];
    file.resize(32, 0);
    for (kind, id, data) in resource_entries(resources, icon, name, version) {
        entry(&mut file, kind, id, &data);
    }
    file
}

// the resources as (type, id, data) in US English, which is also what the bundler writes into
// a setup executable that is already linked
pub fn resource_entries(
    resources: &Resources,
    icon: Option<&[u8]>,
    name: &str,
    version: &str,
) -> Vec<(u16, u16, Vec<u8>)> {
    let mut entries = Vec::new();
    if let Some(icon) = icon {
        let images = icon_images(icon);
        let mut group = [0, 1, images.len() as u16].map(u16::to_le_bytes).concat();
        for (id, (directory, image)) in (1_u16..).zip(images) {
            entries.push((RT_ICON, id, image.to_vec()));
            group.extend_from_slice(directory);
            group.extend(id.to_le_bytes());
        }
        entries.push((RT_GROUP_ICON, 1, group));
    }
    let product = resources.product_name.unwrap_or(name);
    let strings = [
        ("CompanyName", resources.company),
        (
            "FileDescription",
            Some(resources.description.unwrap_or(product)),
        ),
        ("FileVersion", Some(version)),
        ("LegalCopyright", resources.copyright),
        ("ProductName", Some(product)),
        ("ProductVersion", Some(version)),
    ];
    let strings: Vec<_> = strings
        .into_iter()
        .filter_map(|(key, value)| Some((key, value?)))
        .collect();
    entries.push((RT_VERSION, 1, version_info(&strings, version)));
    entries.push((RT_MANIFEST, 1, MANIFEST.as_bytes().to_vec()));
    entries
}

fn entry(file: &mut Vec<u8>, kind: u16, name: u16, data: &[u8]) {
    file.extend((data.len() as u32).to_le_bytes());
    file.extend(32u32.to_le_bytes());
    file.extend([0xffff, kind, 0xffff, name].map(u16::to_le_bytes).concat());
    file.extend(0u32.to_le_bytes());
    file.extend([0x1030, EN_US].map(u16::to_le_bytes).concat());
    file.extend([0; 8]);
    file.extend_from_slice(data);
    pad(file);
}

// each image of an .ico file with the first 12 bytes of its directory entry, which the
// icon group repeats before naming the image's resource id
fn icon_images(icon: &[u8]) -> Vec<(&[u8], &[u8])> {
    let word = |at: usize| {
        icon.get(at..at + 2)
            .map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]))
    };
    assert!(
        word(0) == Some(0) && word(2) == Some(1),
        "the icon is not an .ico file"
    );
    let dword =
        |bytes: &[u8]| u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
    (0..usize::from(word(4).unwrap_or(0)))
        .map(|index| {
            let at = 6 + index * 16;
            let directory = icon
                .get(at..at + 16)
                .expect("the .ico directory is cut short");
            let (size, offset) = (dword(&directory[8..]), dword(&directory[12..]));
            let image = icon
                .get(offset..offset + size)
                .expect("an .ico image runs past the end of the file");
            (&directory[..12], image)
        })
        .collect()
}

fn version_info(strings: &[(&str, &str)], version: &str) -> Vec<u8> {
    let mut numbers = version
        .split(['.', '-', '+'])
        .map(|part| part.parse::<u32>().unwrap_or(0).min(0xffff));
    let [major, minor, patch] = [(); 3].map(|_| numbers.next().unwrap_or(0));
    let (high, low) = (major << 16 | minor, patch << 16);
    let fixed = [
        0xfeef04bd,
        0x0001_0000,
        high,
        low,
        high,
        low,
        0x3f,
        0,
        0x0004_0004,
        1,
        0,
        0,
        0,
    ]
    .map(u32::to_le_bytes)
    .concat();
    let table: Vec<_> = strings
        .iter()
        .map(|(key, value)| block(key, 1, &utf16(value), value.encode_utf16().count() + 1, &[]))
        .collect();
    let strings = block(
        "StringFileInfo",
        1,
        &[],
        0,
        &[block("040904b0", 1, &[], 0, &table)],
    );
    let translation = [EN_US, 1200].map(u16::to_le_bytes).concat();
    let languages = block(
        "VarFileInfo",
        1,
        &[],
        0,
        &[block("Translation", 0, &translation, 4, &[])],
    );
    block(
        "VS_VERSION_INFO",
        0,
        &fixed,
        fixed.len(),
        &[strings, languages],
    )
}

// a version node: its length, the length of its value, whether that value is text, a key, then
// the value and each child on a 32-bit boundary
fn block(key: &str, text: u16, value: &[u8], length: usize, children: &[Vec<u8>]) -> Vec<u8> {
    let mut node = vec![0; 6];
    node.extend(utf16(key));
    pad(&mut node);
    node.extend_from_slice(value);
    for child in children {
        pad(&mut node);
        node.extend_from_slice(child);
    }
    let header = [node.len() as u16, length as u16, text]
        .map(u16::to_le_bytes)
        .concat();
    node[..6].copy_from_slice(&header);
    node
}

fn utf16(text: &str) -> Vec<u8> {
    text.encode_utf16()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect()
}

fn pad(bytes: &mut Vec<u8>) {
    bytes.resize(bytes.len().next_multiple_of(4), 0);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(file: &[u8]) -> Vec<(u16, u16, &[u8])> {
        let mut entries = Vec::new();
        let mut at = 0;
        while at < file.len() {
            let dword =
                |at: usize| u32::from_le_bytes(file[at..at + 4].try_into().unwrap()) as usize;
            let word = |at: usize| u16::from_le_bytes(file[at..at + 2].try_into().unwrap());
            let (size, header) = (dword(at), dword(at + 4));
            entries.push((
                word(at + 10),
                word(at + 14),
                &file[at + header..at + header + size],
            ));
            at = (at + header + size).next_multiple_of(4);
        }
        entries
    }

    #[test]
    fn the_resource_file_holds_the_icon_version_and_manifest() {
        let mut icon = [0, 1, 2].map(u16::to_le_bytes).concat();
        for (side, size, offset) in [(16u8, 3u32, 38u32), (0, 2, 41)] {
            icon.extend([side, side, 0, 0, 1, 0, 32, 0]);
            icon.extend(size.to_le_bytes());
            icon.extend(offset.to_le_bytes());
        }
        icon.extend([7, 7, 7, 9, 9]);
        let resources = Resources {
            company: Some("rot"),
            ..Default::default()
        };
        let file = res(&resources, Some(&icon), "clip", "0.1.2-beta.3");
        let entries = entries(&file);
        let kinds: Vec<_> = entries
            .iter()
            .map(|(kind, name, _)| (*kind, *name))
            .collect();
        assert_eq!(kinds, [(0, 0), (3, 1), (3, 2), (14, 1), (16, 1), (24, 1)]);
        assert_eq!((entries[1].2, entries[2].2), (&[7, 7, 7][..], &[9, 9][..]));
        let group = entries[3].2;
        assert_eq!(&group[..6], [0, 0, 1, 0, 2, 0]);
        assert_eq!(&group[6..20], [&icon[6..18], &[1, 0]].concat());
        assert_eq!(&group[20..], [&icon[22..34], &[2, 0]].concat());

        let version = entries[4].2;
        assert_eq!(
            usize::from(u16::from_le_bytes([version[0], version[1]])),
            version.len()
        );
        let fixed = &version[40..92];
        assert_eq!(&fixed[..4], 0xfeef04bdu32.to_le_bytes());
        assert_eq!(&fixed[8..16], [1, 0, 0, 0, 0, 0, 2, 0]);
        let find = |text: &str| {
            version
                .windows(text.len() * 2 + 2)
                .any(|window| window == utf16(text))
        };
        assert!(find("CompanyName") && find("rot") && find("ProductName") && find("clip"));
        assert!(find("0.1.2-beta.3") && !find("LegalCopyright"));
        assert!(entries[5].2.starts_with(b"<assembly"));
    }
}
