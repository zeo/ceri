// a debian package: the app folder under /usr/lib/<package>, its executable linked from
// /usr/bin, a desktop entry named for the identifier so the window's app id matches it, and the
// png and svg icons in the hicolor theme. dpkg's own triggers refresh the desktop and icon caches,
// so the package carries no maintainer scripts

use super::{Project, app_files};
use ceri_engine::gzip;
use std::collections::BTreeMap;
use std::error::Error;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

// winit loads these for x11 and wayland windows, whichever the session has
const DEPENDS: &str = "libgcc-s1, libxkbcommon0, libxkbcommon-x11-0, libx11-6, libx11-xcb1, \
    libxcursor1, libxi6, libxrandr2, libwayland-client0, libwayland-cursor0";
// media playback and sound load these when a page first plays something, and links open
// through xdg-open
const RECOMMENDS: &str = "libgstreamer1.0-0, libgstreamer-plugins-base1.0-0, \
    gstreamer1.0-plugins-good, gstreamer1.0-libav, libglib2.0-0t64 | libglib2.0-0, libpulse0, \
    xdg-utils";

enum Node {
    Directory,
    File { bytes: Vec<u8>, executable: bool },
    Link(String),
}

pub(super) fn write(
    project: &Project,
    app: &Path,
    output: &Path,
) -> Result<PathBuf, Box<dyn Error>> {
    let package = package_name(&project.binary)?;
    let version = debian_version(&project.version);
    let architecture = match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        "x86" => "i386",
        "arm" => "armhf",
        other => other,
    };
    let mtime = std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or_else(|| {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |since| since.as_secs())
        });

    let mut data = BTreeMap::new();
    let lib = format!("usr/lib/{package}");
    let mut glibc = None;
    for (relative, path) in app_files(app)? {
        let bytes = std::fs::read(&path)?;
        // windows mounts and some checkouts mark every file executable, so only programs keep it
        let program = bytes.starts_with(b"\x7fELF") || bytes.starts_with(b"#!");
        let executable = program && std::fs::metadata(&path)?.permissions().mode() & 0o111 != 0;
        if relative == project.binary {
            glibc = needed_glibc(&bytes);
        }
        add(
            &mut data,
            &format!("{lib}/{relative}"),
            Node::File { bytes, executable },
        );
    }
    add(
        &mut data,
        &format!("usr/bin/{}", project.binary),
        Node::Link(format!("../lib/{package}/{}", project.binary)),
    );
    add(
        &mut data,
        &format!("usr/share/applications/{}.desktop", project.identifier),
        Node::File {
            bytes: desktop_entry(project).into_bytes(),
            executable: false,
        },
    );
    for icon in &project.icons {
        let bytes = std::fs::read(icon)?;
        let size = match icon.extension().and_then(|extension| extension.to_str()) {
            Some("png") => png_size(&bytes)
                .map(|(width, height)| format!("{width}x{height}"))
                .ok_or_else(|| format!("{} is not a png", icon.display()))?,
            Some("svg") => "scalable".to_owned(),
            _ => continue,
        };
        let extension = icon.extension().unwrap().to_string_lossy();
        add(
            &mut data,
            &format!(
                "usr/share/icons/hicolor/{size}/apps/{}.{extension}",
                project.identifier
            ),
            Node::File {
                bytes,
                executable: false,
            },
        );
    }

    let installed: usize = data
        .values()
        .map(|node| match node {
            Node::File { bytes, .. } => bytes.len().div_ceil(1024),
            _ => 1,
        })
        .sum();
    let depends = match glibc {
        Some((major, minor)) => format!("libc6 (>= {major}.{minor}), {DEPENDS}"),
        None => format!("libc6, {DEPENDS}"),
    };
    let maintainer = project
        .authors
        .first()
        .or(project.publisher.as_ref())
        .unwrap_or(&project.name);
    let description = project
        .description
        .as_deref()
        .unwrap_or(&project.name)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let control = format!(
        "Package: {package}\nVersion: {version}\nArchitecture: {architecture}\n\
         Maintainer: {maintainer}\nInstalled-Size: {installed}\nDepends: {depends}\n\
         Recommends: {RECOMMENDS}\nSection: misc\nPriority: optional\n\
         Description: {description}\n"
    );
    let mut control_tree = BTreeMap::new();
    add(
        &mut control_tree,
        "control",
        Node::File {
            bytes: control.into_bytes(),
            executable: false,
        },
    );

    let members = [
        ("debian-binary", b"2.0\n".to_vec()),
        ("control.tar.gz", gzip(&tar(&control_tree, mtime)?)),
        ("data.tar.gz", gzip(&tar(&data, mtime)?)),
    ];
    let deb = output.join(format!("{package}_{version}_{architecture}.deb"));
    std::fs::write(&deb, ar(&members, mtime))?;
    Ok(deb)
}

// a node and every directory above it, keyed so that a directory sorts before what it holds
fn add(tree: &mut BTreeMap<String, Node>, path: &str, node: Node) {
    tree.insert("./".to_owned(), Node::Directory);
    let mut at = 0;
    while let Some(slash) = path[at..].find('/') {
        at += slash + 1;
        tree.insert(format!("./{}", &path[..at]), Node::Directory);
    }
    tree.insert(format!("./{path}"), node);
}

// debian package names are lowercase letters, digits and + - . and start with a letter or digit
fn package_name(binary: &str) -> Result<String, String> {
    let name = binary.to_ascii_lowercase().replace('_', "-");
    let valid = name.len() >= 2
        && name.starts_with(|first: char| first.is_ascii_alphanumeric())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    valid
        .then_some(name)
        .ok_or_else(|| format!("{binary} cannot name a debian package"))
}

// a semver pre-release sorts before its release, which in a debian version takes a tilde
fn debian_version(version: &str) -> String {
    let (release, build) = version.split_once('+').unwrap_or((version, ""));
    let mut debian = release.replacen('-', "~", 1);
    if !build.is_empty() {
        debian = format!("{debian}+{build}");
    }
    debian
}

// the newest glibc symbol version the executable asks for, read from its version names
fn needed_glibc(executable: &[u8]) -> Option<(u32, u32)> {
    let mut newest = None;
    for at in positions(executable, b"GLIBC_2.") {
        let digits = &executable[at + b"GLIBC_".len()..];
        let text: String = digits
            .iter()
            .take_while(|byte| byte.is_ascii_digit() || **byte == b'.')
            .map(|byte| *byte as char)
            .collect();
        let mut parts = text.split('.').map(|part| part.parse::<u32>());
        if let (Some(Ok(major)), Some(Ok(minor))) = (parts.next(), parts.next()) {
            newest = newest.max(Some((major, minor)));
        }
    }
    newest
}

fn positions<'a>(haystack: &'a [u8], needle: &'a [u8]) -> impl Iterator<Item = usize> + 'a {
    haystack
        .windows(needle.len())
        .enumerate()
        .filter(move |(_, window)| *window == needle)
        .map(|(at, _)| at)
}

fn png_size(png: &[u8]) -> Option<(u32, u32)> {
    if png.get(..8)? != b"\x89PNG\r\n\x1a\n" || png.get(12..16)? != b"IHDR" {
        return None;
    }
    let word = |at: usize| u32::from_be_bytes(png[at..at + 4].try_into().unwrap());
    png.get(16..24).map(|_| (word(16), word(20)))
}

fn desktop_entry(project: &Project) -> String {
    let mut entry = format!(
        "[Desktop Entry]\nType=Application\nName={}\n",
        escape(&project.name)
    );
    if let Some(description) = &project.description {
        let line = description.split_whitespace().collect::<Vec<_>>().join(" ");
        entry.push_str(&format!("Comment={}\n", escape(&line)));
    }
    entry.push_str(&format!(
        "Exec={}\nIcon={}\nTerminal=false\nStartupWMClass={}\n",
        project.binary, project.identifier, project.identifier
    ));
    entry
}

// the desktop entry specification escapes backslashes and line breaks in string values
fn escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('\t', "\\t")
}

fn tar(tree: &BTreeMap<String, Node>, mtime: u64) -> Result<Vec<u8>, String> {
    let mut archive = Vec::new();
    for (path, node) in tree {
        let (kind, mode, size, link, bytes) = match node {
            Node::Directory => (b'5', 0o755, 0, "", &[][..]),
            Node::File { bytes, executable } => (
                b'0',
                if *executable { 0o755 } else { 0o644 },
                bytes.len(),
                "",
                &bytes[..],
            ),
            Node::Link(target) => (b'2', 0o777, 0, target.as_str(), &[][..]),
        };
        let mut header = [0_u8; 512];
        let (prefix, name) = split_name(path).ok_or_else(|| format!("{path} is too long"))?;
        if link.len() > 100 || size as u64 >= 1 << 33 {
            return Err(format!("{path} does not fit a tar header"));
        }
        header[..name.len()].copy_from_slice(name.as_bytes());
        octal(&mut header[100..108], mode);
        octal(&mut header[108..116], 0);
        octal(&mut header[116..124], 0);
        octal(&mut header[124..136], size as u64);
        octal(&mut header[136..148], mtime);
        header[156] = kind;
        header[157..157 + link.len()].copy_from_slice(link.as_bytes());
        header[257..265].copy_from_slice(b"ustar\x0000");
        header[265..269].copy_from_slice(b"root");
        header[297..301].copy_from_slice(b"root");
        header[345..345 + prefix.len()].copy_from_slice(prefix.as_bytes());
        header[148..156].fill(b' ');
        let sum: u32 = header.iter().map(|&byte| u32::from(byte)).sum();
        header[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
        archive.extend_from_slice(&header);
        archive.extend_from_slice(bytes);
        archive.resize(archive.len().next_multiple_of(512), 0);
    }
    archive.resize(archive.len() + 1024, 0);
    Ok(archive)
}

// ustar keeps up to 100 bytes of a path in its name field and the leading directories, up to
// 155 bytes, in its prefix
fn split_name(path: &str) -> Option<(&str, &str)> {
    if path.len() <= 100 {
        return Some(("", path));
    }
    let trimmed = path.trim_end_matches('/');
    path.char_indices()
        .filter(|&(at, c)| c == '/' && at < trimmed.len())
        .map(|(at, _)| (&path[..at], &path[at + 1..]))
        .find(|(prefix, name)| prefix.len() <= 155 && name.len() <= 100)
}

fn octal(field: &mut [u8], value: u64) {
    let text = format!("{value:0width$o}", width = field.len() - 1);
    field[..text.len()].copy_from_slice(text.as_bytes());
}

fn ar(members: &[(&str, Vec<u8>)], mtime: u64) -> Vec<u8> {
    let mut archive = b"!<arch>\n".to_vec();
    for (name, data) in members {
        archive.extend(
            format!(
                "{name:<16}{mtime:<12}0     0     100644  {:<10}`\n",
                data.len()
            )
            .bytes(),
        );
        archive.extend_from_slice(data);
        if data.len() % 2 == 1 {
            archive.push(b'\n');
        }
    }
    archive
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_and_names_follow_debian_policy() {
        assert_eq!(debian_version("1.2.3"), "1.2.3");
        assert_eq!(debian_version("1.2.3-beta.1"), "1.2.3~beta.1");
        assert_eq!(debian_version("1.2.3-rc-1+git.5"), "1.2.3~rc-1+git.5");
        assert_eq!(package_name("Clip_Recorder").unwrap(), "clip-recorder");
        assert!(package_name("x").is_err());
        assert!(package_name("_x").is_err());
    }

    #[test]
    fn the_newest_glibc_version_wins() {
        let names = b"\0GLIBC_2.2.5\0GLIBC_2.34\0GLIBC_PRIVATE\0GLIBC_2.17\0";
        assert_eq!(needed_glibc(names), Some((2, 34)));
        assert_eq!(needed_glibc(b"none"), None);
    }

    #[test]
    fn a_tree_lists_each_directory_before_what_it_holds() {
        let mut tree = BTreeMap::new();
        add(&mut tree, "usr/lib/app/assets/a.css", Node::Directory);
        add(&mut tree, "usr/lib/app-data", Node::Directory);
        add(
            &mut tree,
            "usr/bin/app",
            Node::Link("../lib/app/app".into()),
        );
        let paths: Vec<_> = tree.keys().map(String::as_str).collect();
        assert_eq!(paths[0], "./");
        for (index, path) in paths.iter().enumerate().skip(1) {
            let parent = &path[..path.trim_end_matches('/').rfind('/').unwrap() + 1];
            assert!(paths[..index].contains(&parent), "{path}");
        }
    }

    #[test]
    fn tar_headers_carry_long_paths_and_valid_checksums() {
        let long = format!("./usr/lib/app/{}/file.txt", "d".repeat(120));
        let mut tree = BTreeMap::new();
        tree.insert(
            long.clone(),
            Node::File {
                bytes: b"hello".to_vec(),
                executable: false,
            },
        );
        let archive = tar(&tree, 0).unwrap();
        assert_eq!(archive.len(), 512 * 2 + 1024);
        let header = &archive[..512];
        let field = |range: std::ops::Range<usize>| {
            String::from_utf8_lossy(&header[range])
                .trim_end_matches('\0')
                .to_owned()
        };
        assert_eq!(format!("{}/{}", field(345..500), field(0..100)), long);
        assert_eq!(field(124..136), "00000000005");
        let mut blank = header.to_vec();
        blank[148..156].fill(b' ');
        let sum: u32 = blank.iter().map(|&byte| u32::from(byte)).sum();
        assert_eq!(
            u32::from_str_radix(field(148..154).as_str(), 8).unwrap(),
            sum
        );
        assert_eq!(&archive[512..517], b"hello");
    }

    #[test]
    fn ar_members_are_padded_to_even_offsets() {
        let archive = ar(&[("debian-binary", b"2.0\n".to_vec()), ("x", vec![1])], 7);
        assert!(archive.starts_with(
            b"!<arch>\ndebian-binary   7           0     0     100644  4         `\n2.0\n"
        ));
        assert_eq!(archive.len(), 8 + 60 + 4 + 60 + 2);
    }
}
