use ceri_engine::{FontFace, FontSource, FontStyle};
use std::cell::RefCell;
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::rc::Rc;

const MAX_INDEXED_FONTS: usize = 2_000;
const MAX_FONT_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
struct IndexedFace {
    family: String,
    path: PathBuf,
    weight: u16,
    italic: bool,
}

// installed faces indexed by family name, read from each file's name table
pub struct SystemFonts {
    faces: Vec<IndexedFace>,
    // families the platform font configuration prefers for a name, in priority order
    preferred: HashMap<String, Vec<String>>,
    loaded: RefCell<HashMap<PathBuf, Option<Rc<FontFace>>>>,
    // what each character fell back to, so the search runs once per character
    fallbacks: RefCell<HashMap<char, Option<PathBuf>>>,
}

impl SystemFonts {
    pub fn discover() -> Self {
        let (directories, preferred) = font_locations();
        let mut faces = Vec::new();
        for directory in directories {
            index_directory(&directory, &mut faces, 0);
        }
        faces.sort_by(|left, right| {
            (&left.family, left.weight, left.italic).cmp(&(
                &right.family,
                right.weight,
                right.italic,
            ))
        });
        Self {
            faces,
            preferred,
            loaded: RefCell::new(HashMap::new()),
            fallbacks: RefCell::new(HashMap::new()),
        }
    }

    pub fn families(&self) -> usize {
        self.faces.len()
    }

    // an installed family first, then what the font configuration prefers for the name or its
    // css generic, then the platform's default for the generic
    fn lookup(&self, family: &str, style: FontStyle) -> Option<&IndexedFace> {
        if let Some(face) = self.installed(family, style) {
            return Some(face);
        }
        let preferred = [Some(family), css_generic(family)]
            .into_iter()
            .flatten()
            .filter_map(|name| self.preferred.get(name))
            .flatten()
            .find_map(|name| self.installed(name, style));
        if preferred.is_some() {
            return preferred;
        }
        if let Some(default) = generic_family(family) {
            return self.installed(default, style);
        }
        // a width variant that is not installed, such as arial narrow, takes its base family,
        // as edge and chrome do on windows
        let base = WIDTH_WORDS
            .iter()
            .find_map(|word| family.strip_suffix(word)?.strip_suffix(' '))?;
        self.lookup(base, style)
    }

    // the css font matching order: slant first, then the weight nearest in the direction
    // the requested weight leans
    fn installed(&self, family: &str, style: FontStyle) -> Option<&IndexedFace> {
        let matching = self
            .faces
            .iter()
            .filter(|face| face.family == family)
            .collect::<Vec<_>>();
        let slant = if matching.iter().any(|face| face.italic == style.italic) {
            style.italic
        } else {
            !style.italic
        };
        let wanted = style.weight;
        matching
            .into_iter()
            .filter(|face| face.italic == slant)
            .min_by_key(|face| weight_distance(wanted, face.weight))
    }

    fn load(&self, path: &Path) -> Option<Rc<FontFace>> {
        if let Some(cached) = self.loaded.borrow().get(path) {
            return cached.clone();
        }
        let face = std::fs::read(path)
            .ok()
            .filter(|bytes| bytes.len() as u64 <= MAX_FONT_BYTES)
            .and_then(|bytes| FontFace::parse(bytes).ok())
            .map(Rc::new);
        self.loaded
            .borrow_mut()
            .insert(path.to_path_buf(), face.clone());
        face
    }
}

impl FontSource for SystemFonts {
    fn face(&self, family: &str, style: FontStyle) -> Option<Rc<FontFace>> {
        let indexed = self.lookup(family, style)?;
        self.load(&indexed.path.clone())
    }

    // the platform's broad-coverage families first, then any installed face that has it
    fn fallback(&self, character: char, style: FontStyle) -> Option<Rc<FontFace>> {
        if let Some(path) = self.fallbacks.borrow().get(&character).cloned() {
            return path.and_then(|path| self.load(&path));
        }
        let covers = |face: &IndexedFace| {
            self.load(&face.path)
                .is_some_and(|loaded| loaded.has_glyph(character))
        };
        let found = FALLBACK_FAMILIES
            .iter()
            .filter_map(|family| self.lookup(family, style))
            .find(|face| covers(face))
            .or_else(|| {
                self.faces
                    .iter()
                    .filter(|face| !face.italic)
                    .take(MAX_FALLBACK_PROBES)
                    .find(|face| covers(face))
            })
            .map(|face| face.path.clone());
        self.fallbacks.borrow_mut().insert(character, found.clone());
        found.and_then(|path| self.load(&path))
    }

    fn emoji(&self, style: FontStyle) -> Option<Rc<FontFace>> {
        let path = self.lookup(EMOJI_FAMILY, style)?.path.clone();
        self.load(&path)
    }
}

#[cfg(target_os = "windows")]
const EMOJI_FAMILY: &str = "segoe ui emoji";
#[cfg(target_os = "macos")]
const EMOJI_FAMILY: &str = "apple color emoji";
#[cfg(all(unix, not(target_os = "macos")))]
const EMOJI_FAMILY: &str = "noto color emoji";

// how many installed faces a missing character may load before it is given up on
const MAX_FALLBACK_PROBES: usize = 64;

#[cfg(target_os = "windows")]
const FALLBACK_FAMILIES: &[&str] = &[
    "segoe ui",
    "segoe ui symbol",
    "segoe ui emoji",
    "segoe ui historic",
    "microsoft yahei",
    "yu gothic",
    "malgun gothic",
    "nirmala ui",
    "ebrima",
    "gadugi",
    "leelawadee ui",
    "javanese text",
    "mongolian baiti",
    "microsoft himalaya",
    "sylfaen",
    "arial",
    "tahoma",
];
#[cfg(target_os = "macos")]
const FALLBACK_FAMILIES: &[&str] = &[
    "helvetica neue",
    "apple color emoji",
    "pingfang sc",
    "hiragino sans",
    "apple sd gothic neo",
    "kohinoor devanagari",
    "geeza pro",
    "thonburi",
    "arial unicode ms",
];
#[cfg(all(unix, not(target_os = "macos")))]
const FALLBACK_FAMILIES: &[&str] = &[
    "dejavu sans",
    "noto sans",
    "noto sans cjk sc",
    "noto color emoji",
    "noto sans arabic",
    "noto sans devanagari",
    "noto sans thai",
    "noto sans hebrew",
];

// how far a face's weight is from the one asked for, by the css matching rules: up to 500
// look lighter first, then heavier; past 500 the reverse
fn weight_distance(wanted: u16, weight: u16) -> (u8, u16) {
    let wanted = i32::from(wanted);
    let weight = i32::from(weight);
    let heavier = weight >= wanted;
    let prefers_heavier = if (400..=500).contains(&wanted) {
        // 400 and 500 first try up to 500, then lighter, then heavier
        if weight <= 500 && heavier {
            return (0, (weight - wanted) as u16);
        }
        false
    } else {
        wanted > 500
    };
    let preferred = heavier == prefers_heavier;
    (
        if preferred { 1 } else { 2 },
        (weight - wanted).unsigned_abs() as u16,
    )
}

// generic css families map to the faces a desktop is expected to have
const WIDTH_WORDS: &[&str] = &[
    "ultra condensed",
    "extra condensed",
    "semi condensed",
    "condensed",
    "narrow",
    "compressed",
    "semi expanded",
    "extra expanded",
    "ultra expanded",
    "expanded",
    "extended",
    "wide",
];

fn css_generic(family: &str) -> Option<&'static str> {
    match family {
        "sans-serif" | "system-ui" | "ui-sans-serif" | "-apple-system" => Some("sans-serif"),
        "serif" | "ui-serif" => Some("serif"),
        "monospace" | "ui-monospace" => Some("monospace"),
        _ => None,
    }
}

fn generic_family(family: &str) -> Option<&'static str> {
    Some(match css_generic(family)? {
        "sans-serif" if cfg!(target_os = "windows") => "segoe ui",
        "sans-serif" if cfg!(target_os = "macos") => "helvetica",
        "sans-serif" => "dejavu sans",
        "serif" if cfg!(target_os = "windows") => "times new roman",
        "serif" if cfg!(target_os = "macos") => "times",
        "serif" => "dejavu serif",
        _ if cfg!(target_os = "windows") => "consolas",
        _ if cfg!(target_os = "macos") => "menlo",
        _ => "dejavu sans mono",
    })
}

// the directories to index and the preferred families for a name
fn font_locations() -> (Vec<PathBuf>, HashMap<String, Vec<String>>) {
    let mut directories = Vec::new();
    #[cfg(target_os = "windows")]
    {
        if let Some(windows) = std::env::var_os("WINDIR") {
            directories.push(PathBuf::from(windows).join("Fonts"));
        }
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            directories.push(
                PathBuf::from(local)
                    .join("Microsoft")
                    .join("Windows")
                    .join("Fonts"),
            );
        }
    }
    #[cfg(target_os = "macos")]
    {
        directories.push(PathBuf::from("/System/Library/Fonts"));
        directories.push(PathBuf::from("/System/Library/Fonts/Supplemental"));
        directories.push(PathBuf::from("/Library/Fonts"));
        if let Some(home) = std::env::var_os("HOME") {
            directories.push(PathBuf::from(home).join("Library").join("Fonts"));
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        directories.push(PathBuf::from("/usr/share/fonts"));
        directories.push(PathBuf::from("/usr/local/share/fonts"));
        if let Some(home) = &home {
            directories.push(home.join(".local/share/fonts"));
            directories.push(home.join(".fonts"));
        }
        let config = fontconfig::system(home);
        for directory in config.directories {
            if !directories.contains(&directory) {
                directories.push(directory);
            }
        }
        (directories, config.preferred)
    }
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    {
        (directories, HashMap::new())
    }
}

// the subset of fontconfig's xml that decides where fonts live and which families stand in for
// a name: dir, include and alias prefer lists
#[cfg(any(all(unix, not(target_os = "macos")), test))]
mod fontconfig {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};

    const MAX_INCLUDE_DEPTH: u8 = 16;

    #[derive(Debug, Default)]
    pub(super) struct FontConfig {
        pub(super) directories: Vec<PathBuf>,
        pub(super) preferred: HashMap<String, Vec<String>>,
    }

    pub(super) struct Roots {
        pub(super) home: Option<PathBuf>,
        pub(super) config_home: Option<PathBuf>,
        pub(super) data_home: Option<PathBuf>,
        // where relative includes such as conf.d resolve, /etc/fonts on a normal install
        pub(super) system: PathBuf,
    }

    #[derive(Debug, PartialEq)]
    enum Entry {
        Directory(PathBuf),
        Include(PathBuf),
        Prefer(String, Vec<String>),
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    pub(super) fn system(home: Option<PathBuf>) -> FontConfig {
        let xdg = |name: &str, fallback: &str| {
            std::env::var_os(name)
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .or_else(|| home.as_ref().map(|home| home.join(fallback)))
        };
        let roots = Roots {
            config_home: xdg("XDG_CONFIG_HOME", ".config"),
            data_home: xdg("XDG_DATA_HOME", ".local/share"),
            home: home.clone(),
            system: PathBuf::from("/etc/fonts"),
        };
        let mut config = FontConfig::default();
        read(&roots.system.join("fonts.conf"), &roots, &mut config, 0);
        config
    }

    // includes load in place, so an earlier file's prefer list ranks first as in fontconfig
    pub(super) fn read(path: &Path, roots: &Roots, config: &mut FontConfig, depth: u8) {
        if depth > MAX_INCLUDE_DEPTH {
            return;
        }
        if path.is_dir() {
            let Ok(entries) = std::fs::read_dir(path) else {
                return;
            };
            let mut files = entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|file| {
                    file.file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| {
                            name.starts_with(|first: char| first.is_ascii_digit())
                                && name.ends_with(".conf")
                        })
                })
                .collect::<Vec<_>>();
            files.sort();
            for file in files {
                read(&file, roots, config, depth + 1);
            }
            return;
        }
        let Ok(text) = std::fs::read_to_string(path) else {
            return;
        };
        let current = path.parent().unwrap_or(Path::new("/"));
        for entry in parse(&text, current, roots) {
            match entry {
                Entry::Directory(directory) => config.directories.push(directory),
                Entry::Include(file) => read(&file, roots, config, depth + 1),
                Entry::Prefer(family, preferred) => config
                    .preferred
                    .entry(family)
                    .or_default()
                    .extend(preferred),
            }
        }
    }

    fn parse(text: &str, current: &Path, roots: &Roots) -> Vec<Entry> {
        let mut entries = Vec::new();
        let mut open: Vec<(&str, &str)> = Vec::new();
        let mut alias: Option<(String, Vec<String>)> = None;
        let mut rest = text;
        while let Some(start) = rest.find('<') {
            let content = decode(rest[..start].trim());
            rest = &rest[start..];
            if !content.is_empty() {
                let names = open.iter().rev().map(|(name, _)| *name).collect::<Vec<_>>();
                let attributes = open.last().map_or("", |(_, attributes)| *attributes);
                match names.as_slice() {
                    ["dir", "fontconfig", ..] => {
                        let base = roots.data_home.as_deref();
                        if let Some(path) = resolve(&content, attributes, base, current, roots) {
                            entries.push(Entry::Directory(path));
                        }
                    }
                    ["include", "fontconfig", ..] => {
                        let base = roots.config_home.as_deref();
                        if let Some(path) = resolve(&content, attributes, base, current, roots) {
                            let path = if path.is_relative() {
                                roots.system.join(path)
                            } else {
                                path
                            };
                            entries.push(Entry::Include(path));
                        }
                    }
                    ["family", "alias", ..] => {
                        if let Some((family, _)) = &mut alias {
                            *family = content.to_lowercase();
                        }
                    }
                    ["family", "prefer", "alias", ..] => {
                        if let Some((_, preferred)) = &mut alias {
                            preferred.push(content.to_lowercase());
                        }
                    }
                    _ => {}
                }
            }
            let skip_to = if rest.starts_with("<!--") { "-->" } else { ">" };
            let Some(end) = rest.find(skip_to) else {
                break;
            };
            let tag = &rest[1..end];
            rest = &rest[end + skip_to.len()..];
            if tag.starts_with(['!', '?']) {
                continue;
            }
            if let Some(name) = tag.strip_prefix('/') {
                let name = name.trim();
                if let Some(position) = open.iter().rposition(|(open, _)| *open == name) {
                    open.truncate(position);
                }
                if name == "alias"
                    && let Some((family, preferred)) = alias.take()
                    && !family.is_empty()
                    && !preferred.is_empty()
                {
                    entries.push(Entry::Prefer(family, preferred));
                }
                continue;
            }
            if tag.ends_with('/') {
                continue;
            }
            let (name, attributes) = tag.split_once(char::is_whitespace).unwrap_or((tag, ""));
            if name == "alias" {
                alias = Some((String::new(), Vec::new()));
            }
            open.push((name, attributes));
        }
        entries
    }

    // "~" is the home directory, prefix="xdg" an xdg base directory and prefix="relative" the
    // directory of the file being read; anything else is taken as written
    fn resolve(
        value: &str,
        attributes: &str,
        xdg: Option<&Path>,
        current: &Path,
        roots: &Roots,
    ) -> Option<PathBuf> {
        if value == "~" {
            return roots.home.clone();
        }
        if let Some(rest) = value.strip_prefix("~/") {
            return Some(roots.home.as_ref()?.join(rest));
        }
        let value = Path::new(value);
        if value.is_absolute() {
            return Some(value.to_path_buf());
        }
        match attribute(attributes, "prefix") {
            Some("xdg") => Some(xdg?.join(value)),
            Some("relative") => Some(current.join(value)),
            _ => Some(value.to_path_buf()),
        }
    }

    fn attribute<'a>(mut attributes: &'a str, wanted: &str) -> Option<&'a str> {
        loop {
            let (name, rest) = attributes.split_once('=')?;
            let rest = rest.trim_start();
            let quote = rest
                .chars()
                .next()
                .filter(|quote| matches!(quote, '"' | '\''))?;
            let (value, rest) = rest[1..].split_once(quote)?;
            if name.trim() == wanted {
                return Some(value);
            }
            attributes = rest;
        }
    }

    fn decode(text: &str) -> String {
        text.replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&apos;", "'")
            .replace("&amp;", "&")
    }

    #[cfg(test)]
    mod tests {
        use super::{Entry, FontConfig, Roots, parse, read};
        use std::path::{Path, PathBuf};

        fn roots(system: PathBuf) -> Roots {
            Roots {
                home: Some(PathBuf::from("/home/user")),
                config_home: Some(PathBuf::from("/home/user/.config")),
                data_home: Some(PathBuf::from("/home/user/.local/share")),
                system,
            }
        }

        #[test]
        fn directories_includes_and_prefer_lists_are_read() {
            let text = r#"<?xml version="1.0"?>
<!DOCTYPE fontconfig SYSTEM "urn:fontconfig:fonts.dtd">
<fontconfig>
  <!-- <dir>/commented/out</dir> -->
  <dir>/usr/share/fonts</dir>
  <dir prefix="xdg">fonts</dir>
  <dir>~/.fonts</dir>
  <dir prefix='relative'>extra</dir>
  <match target="pattern">
    <test name="family"><string>mono</string></test>
    <edit name="family" mode="assign"><string>monospace</string></edit>
  </match>
  <include ignore_missing="yes">conf.d</include>
  <include ignore_missing="yes" prefix="xdg">fontconfig/fonts.conf</include>
  <alias>
    <family>Sans-Serif</family>
    <prefer>
      <family>Noto Sans</family>
      <family>Fira &amp; Friends</family> <!-- a comment between families -->
    </prefer>
    <default><family>DejaVu Sans</family></default>
  </alias>
  <alias binding="same">
    <family>Arial</family>
    <accept><family>Liberation Sans</family></accept>
  </alias>
  <selectfont><rejectfont><glob>*.dpkg-tmp</glob></rejectfont></selectfont>
</fontconfig>"#;
            let entries = parse(
                text,
                Path::new("/etc/fonts"),
                &roots(PathBuf::from("/etc/fonts")),
            );
            assert_eq!(
                entries,
                vec![
                    Entry::Directory(PathBuf::from("/usr/share/fonts")),
                    Entry::Directory(PathBuf::from("/home/user/.local/share/fonts")),
                    Entry::Directory(PathBuf::from("/home/user/.fonts")),
                    Entry::Directory(PathBuf::from("/etc/fonts/extra")),
                    Entry::Include(PathBuf::from("/etc/fonts/conf.d")),
                    Entry::Include(PathBuf::from("/home/user/.config/fontconfig/fonts.conf")),
                    Entry::Prefer(
                        "sans-serif".into(),
                        vec!["noto sans".into(), "fira & friends".into()]
                    ),
                ]
            );
        }

        #[test]
        fn included_directories_load_numbered_files_in_order() {
            let root = std::env::temp_dir().join(format!("ceri-fontconfig-{}", std::process::id()));
            let conf_d = root.join("conf.d");
            std::fs::create_dir_all(&conf_d).unwrap();
            let prefer = |family: &str| {
                format!(
                    "<fontconfig><alias><family>sans-serif</family><prefer><family>{family}</family></prefer></alias></fontconfig>"
                )
            };
            std::fs::write(
                root.join("fonts.conf"),
                "<fontconfig><dir>/opt/fonts</dir><include>conf.d</include><include>fonts.conf</include></fontconfig>",
            )
            .unwrap();
            std::fs::write(conf_d.join("60-latin.conf"), prefer("DejaVu Sans")).unwrap();
            std::fs::write(conf_d.join("50-user.conf"), prefer("Inter")).unwrap();
            std::fs::write(conf_d.join("README"), prefer("Ignored")).unwrap();
            std::fs::write(conf_d.join("local.conf"), prefer("Ignored")).unwrap();
            let mut config = FontConfig::default();
            read(
                &root.join("fonts.conf"),
                &roots(root.clone()),
                &mut config,
                0,
            );
            std::fs::remove_dir_all(&root).unwrap();
            // the file including itself stops at the depth limit, listing its dir once per level
            assert!(
                config
                    .directories
                    .iter()
                    .all(|path| path == Path::new("/opt/fonts"))
            );
            let sans = &config.preferred["sans-serif"];
            assert_eq!(&sans[..2], ["inter", "dejavu sans"]);
            assert!(!sans.iter().any(|family| family == "ignored"));
        }
    }
}

fn index_directory(directory: &Path, faces: &mut Vec<IndexedFace>, depth: u8) {
    if depth > 3 || faces.len() >= MAX_INDEXED_FONTS {
        return;
    }
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        if faces.len() >= MAX_INDEXED_FONTS {
            return;
        }
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            index_directory(&path, faces, depth + 1);
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        let extension = path
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if !matches!(extension.as_str(), "ttf" | "ttc" | "otf") {
            continue;
        }
        if let Some(face) = read_face_names(&path) {
            faces.push(face);
        }
    }
}

// only the table directory and name table are read, so indexing stays cheap
fn read_face_names(path: &Path) -> Option<IndexedFace> {
    let mut file = File::open(path).ok()?;
    let mut header = [0_u8; 12];
    file.read_exact(&mut header).ok()?;
    let version = u32::from_be_bytes([header[0], header[1], header[2], header[3]]);
    let mut base = 0_u64;
    if version == 0x7474_6366 {
        // a collection lists its first face's offset after the tag, version and face count
        let mut first = [0_u8; 4];
        file.read_exact(&mut first).ok()?;
        base = u64::from(u32::from_be_bytes(first));
        file.seek(SeekFrom::Start(base)).ok()?;
        file.read_exact(&mut header).ok()?;
    } else if !matches!(version, 0x0001_0000 | 0x7472_7565 | 0x4f54_544f) {
        return None;
    }
    let table_count = usize::from(u16::from_be_bytes([header[4], header[5]]));
    if table_count == 0 || table_count > 512 {
        return None;
    }
    let mut directory = vec![0_u8; table_count * 16];
    file.seek(SeekFrom::Start(base + 12)).ok()?;
    file.read_exact(&mut directory).ok()?;
    let mut name_table = None;
    let mut os2_table = None;
    for record in directory.chunks_exact(16) {
        let offset = u32::from_be_bytes([record[8], record[9], record[10], record[11]]);
        let length = u32::from_be_bytes([record[12], record[13], record[14], record[15]]);
        match &record[..4] {
            b"name" => name_table = Some((u64::from(offset), length as usize)),
            b"OS/2" => os2_table = Some(u64::from(offset)),
            _ => {}
        }
    }
    let mut os2 = [0_u8; 64];
    let os2 = os2_table.and_then(|offset| {
        file.seek(SeekFrom::Start(offset)).ok()?;
        file.read_exact(&mut os2).ok()?;
        Some(os2)
    });
    let (offset, length) = name_table?;
    if !(6..=1 << 20).contains(&length) {
        return None;
    }
    let mut table = vec![0_u8; length];
    file.seek(SeekFrom::Start(offset)).ok()?;
    file.read_exact(&mut table).ok()?;
    let family = read_name(&table, &[16, 1])?.to_ascii_lowercase();
    let subfamily = read_name(&table, &[17, 2])
        .unwrap_or_default()
        .to_ascii_lowercase();
    let weight = os2
        .map(|table| u16::from_be_bytes([table[4], table[5]]))
        .filter(|weight| (1..=1000).contains(weight))
        .unwrap_or(if subfamily.contains("bold") { 700 } else { 400 });
    let italic = os2.is_some_and(|table| table[63] & 1 != 0)
        || subfamily.contains("italic")
        || subfamily.contains("oblique");
    Some(IndexedFace {
        family,
        path: path.to_path_buf(),
        weight,
        italic,
    })
}

fn read_name(table: &[u8], wanted: &[u16]) -> Option<String> {
    let count = usize::from(u16::from_be_bytes([table[2], table[3]]));
    let storage = usize::from(u16::from_be_bytes([table[4], table[5]]));
    for &name_id in wanted {
        for index in 0..count {
            let record = 6 + index * 12;
            let record = table.get(record..record + 12)?;
            let platform = u16::from_be_bytes([record[0], record[1]]);
            let current = u16::from_be_bytes([record[6], record[7]]);
            if current != name_id {
                continue;
            }
            let length = usize::from(u16::from_be_bytes([record[8], record[9]]));
            let offset = storage + usize::from(u16::from_be_bytes([record[10], record[11]]));
            let bytes = table.get(offset..offset + length)?;
            let text = if platform == 1 {
                bytes.iter().map(|byte| char::from(*byte)).collect()
            } else {
                bytes
                    .chunks_exact(2)
                    .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
                    .collect::<Vec<_>>()
                    .iter()
                    .filter_map(|unit| char::from_u32(u32::from(*unit)))
                    .collect::<String>()
            };
            let text = text.trim().to_string();
            if !text.is_empty() {
                return Some(text);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{IndexedFace, SystemFonts, generic_family, weight_distance};
    use ceri_engine::{FontSource, FontStyle};
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::path::PathBuf;

    #[test]
    fn generic_families_map_to_platform_faces() {
        assert!(generic_family("sans-serif").is_some());
        assert!(generic_family("monospace").is_some());
        assert!(generic_family("serif").is_some());
        assert_eq!(generic_family("Aptos Narrow"), None);
    }

    #[test]
    fn a_missing_width_variant_takes_its_base_family() {
        let fonts = SystemFonts {
            faces: vec![IndexedFace {
                family: "arial".into(),
                path: PathBuf::from("arial.ttf"),
                weight: 400,
                italic: false,
            }],
            preferred: HashMap::new(),
            loaded: RefCell::new(HashMap::new()),
            fallbacks: RefCell::new(HashMap::new()),
        };
        let found = |family| {
            fonts
                .lookup(family, FontStyle::NORMAL)
                .map(|face| &face.path)
        };
        assert_eq!(found("arial narrow"), Some(&PathBuf::from("arial.ttf")));
        assert_eq!(
            found("arial semi condensed"),
            Some(&PathBuf::from("arial.ttf"))
        );
        assert_eq!(found("aptos narrow"), None);
        assert_eq!(found("arial bold"), None);
    }

    #[test]
    fn configured_preferences_pick_the_first_installed_family() {
        let face = |family: &str| IndexedFace {
            family: family.into(),
            path: PathBuf::from(format!("{family}.ttf")),
            weight: 400,
            italic: false,
        };
        let list = |families: &[&str]| families.iter().map(|family| family.to_string()).collect();
        let fonts = SystemFonts {
            faces: vec![
                face("cantarell"),
                face("liberation sans"),
                face("fira mono"),
            ],
            preferred: HashMap::from([
                (
                    "sans-serif".to_string(),
                    list(&["noto sans", "liberation sans"]),
                ),
                ("system-ui".to_string(), list(&["inter"])),
                ("monospace".to_string(), list(&["noto mono"])),
                ("helvetica".to_string(), list(&["cantarell"])),
            ]),
            loaded: RefCell::new(HashMap::new()),
            fallbacks: RefCell::new(HashMap::new()),
        };
        let found = |family| {
            fonts
                .lookup(family, FontStyle::NORMAL)
                .map(|face| face.family.as_str())
        };
        assert_eq!(found("sans-serif"), Some("liberation sans"));
        // system-ui has its own list, then falls back to the sans-serif one
        assert_eq!(found("system-ui"), Some("liberation sans"));
        assert_eq!(found("helvetica"), Some("cantarell"));
        assert_eq!(found("fira mono"), Some("fira mono"));
        // neither a preferred family nor the platform default is installed
        assert_eq!(found("monospace"), None);
    }

    #[test]
    fn weights_match_by_the_css_order() {
        let nearest = |wanted, weights: &[u16]| {
            *weights
                .iter()
                .min_by_key(|weight| weight_distance(wanted, **weight))
                .expect("weights")
        };
        assert_eq!(nearest(400, &[300, 500, 700]), 500);
        assert_eq!(nearest(400, &[300, 700]), 300);
        assert_eq!(nearest(700, &[400, 600, 900]), 900);
        assert_eq!(nearest(300, &[200, 400]), 200);
        assert_eq!(nearest(600, &[400, 500]), 500);
    }

    #[test]
    fn discovery_indexes_installed_faces_and_serves_a_default_family() {
        let fonts = SystemFonts::discover();
        if fonts.families() == 0 {
            // a build host without installed fonts still has to leave the runtime usable
            assert!(fonts.face("sans-serif", FontStyle::NORMAL).is_none());
            return;
        }
        let face = fonts
            .face("sans-serif", FontStyle::NORMAL)
            .expect("a desktop platform should provide a sans-serif face");
        assert!(face.units_per_em() > 0.0);
        assert!(face.advance(face.glyph_for('a'), 16.0) > 0.0);
        // the same family resolves to the same cached face
        assert!(fonts.face("sans-serif", FontStyle::NORMAL).is_some());
    }

    #[test]
    fn mixed_scripts_find_installed_faces() {
        let fonts = SystemFonts::discover();
        if fonts.families() == 0 {
            return;
        }
        // windows always ships these scripts, a linux install only has what its packages added
        let installed = |character| {
            fonts.faces.iter().any(|face| {
                fonts
                    .load(&face.path)
                    .is_some_and(|loaded| loaded.has_glyph(character))
            })
        };
        for character in ['\u{0634}', '\u{4E2D}'] {
            match fonts.fallback(character, FontStyle::NORMAL) {
                Some(face) => assert!(face.has_glyph(character)),
                None => assert!(
                    !cfg!(target_os = "windows") && !installed(character),
                    "an installed face draws {character}, so fallback should find it"
                ),
            }
        }
        if let Some(emoji) = fonts.emoji(FontStyle::NORMAL) {
            assert!(emoji.has_glyph('\u{1F600}'));
        }
    }
}
