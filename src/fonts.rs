use ceri_engine::{FontFace, FontSource, FontStyle};
use std::cell::RefCell;
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, OnceLock};

const MAX_INDEXED_FONTS: usize = 2_000;
const MAX_FONT_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
struct IndexedFace {
    family: String,
    path: PathBuf,
    weight: u16,
    italic: bool,
}

struct FontIndex {
    faces: Vec<IndexedFace>,
    // families the platform font configuration prefers for a name, in priority order
    preferred: HashMap<String, Vec<String>>,
}

static FULL_INDEX: OnceLock<Arc<FontIndex>> = OnceLock::new();

fn shared_index() -> &'static Arc<FontIndex> {
    FULL_INDEX.get_or_init(|| Arc::new(scan_installed_fonts()))
}

fn scan_installed_fonts() -> FontIndex {
    let (directories, preferred) = font_locations();
    let mut faces = Vec::new();
    let mut scratch = Vec::new();
    for directory in directories {
        index_directory(&directory, &mut faces, &mut scratch, 0);
    }
    sort_faces(&mut faces);
    FontIndex { faces, preferred }
}

fn sort_faces(faces: &mut [IndexedFace]) {
    faces.sort_by(|left, right| {
        (&left.family, left.weight, left.italic).cmp(&(&right.family, right.weight, right.italic))
    });
}

#[cfg(test)]
struct CustomIndex {
    #[cfg(target_os = "windows")]
    allow_direct: bool,
    index: OnceLock<Arc<FontIndex>>,
    build: Box<dyn Fn() -> FontIndex>,
}

#[cfg(test)]
impl CustomIndex {
    fn inline(mut faces: Vec<IndexedFace>, preferred: HashMap<String, Vec<String>>) -> Self {
        sort_faces(&mut faces);
        let index = OnceLock::new();
        let _ = index.set(Arc::new(FontIndex { faces, preferred }));
        Self {
            #[cfg(target_os = "windows")]
            allow_direct: false,
            index,
            build: Box::new(|| FontIndex {
                faces: Vec::new(),
                preferred: HashMap::new(),
            }),
        }
    }

    fn lazy(build: impl Fn() -> FontIndex + 'static) -> Self {
        Self {
            #[cfg(target_os = "windows")]
            allow_direct: true,
            index: OnceLock::new(),
            build: Box::new(build),
        }
    }

    fn get(&self) -> &FontIndex {
        self.index.get_or_init(|| Arc::new((self.build)())).as_ref()
    }
}

// installed faces resolved on demand, backed by a single process-wide directory scan when needed
pub struct SystemFonts {
    #[cfg(test)]
    custom: Option<CustomIndex>,
    // standard windows families resolved directly from windir\fonts without scanning the directory
    #[cfg(target_os = "windows")]
    direct: RefCell<HashMap<&'static str, Vec<IndexedFace>>>,
    loaded: RefCell<HashMap<PathBuf, Option<Rc<FontFace>>>>,
    // what each character fell back to, so the search runs once per character
    fallbacks: RefCell<HashMap<char, Option<PathBuf>>>,
}

impl SystemFonts {
    // defers full directory scanning until an unknown family or fallback miss requires it
    pub fn lazy() -> Self {
        Self {
            #[cfg(test)]
            custom: None,
            #[cfg(target_os = "windows")]
            direct: RefCell::new(HashMap::new()),
            loaded: RefCell::new(HashMap::new()),
            fallbacks: RefCell::new(HashMap::new()),
        }
    }

    #[cfg(test)]
    pub fn discover() -> Self {
        let _ = shared_index();
        Self::lazy()
    }

    #[cfg(test)]
    pub fn families(&self) -> usize {
        self.index().faces.len()
    }

    fn index(&self) -> &FontIndex {
        #[cfg(test)]
        if let Some(custom) = &self.custom {
            return custom.get();
        }
        shared_index().as_ref()
    }

    #[cfg(target_os = "windows")]
    fn allow_direct(&self) -> bool {
        #[cfg(test)]
        if let Some(custom) = &self.custom {
            return custom.allow_direct;
        }
        true
    }

    #[cfg(target_os = "windows")]
    fn direct_lookup(&self, family: &str, style: FontStyle) -> Option<PathBuf> {
        if !self.allow_direct() {
            return None;
        }
        let (target, candidates) = windows_standard_faces(family).or_else(|| {
            let mapped = generic_family(family).or_else(|| substitute(family))?;
            windows_standard_faces(mapped)
        })?;
        let directory = windows_fonts_dir()?;
        let mut direct = self.direct.borrow_mut();
        let faces = direct.entry(target).or_insert_with(|| {
            candidates
                .iter()
                .filter_map(|&(file, weight, italic)| {
                    let path = directory.join(file);
                    path.is_file().then(|| IndexedFace {
                        family: target.to_string(),
                        path,
                        weight,
                        italic,
                    })
                })
                .collect()
        });
        best_matching_face(faces, style).map(|face| face.path.clone())
    }

    fn lookup_path(&self, family: &str, style: FontStyle) -> Option<PathBuf> {
        #[cfg(target_os = "windows")]
        if let Some(path) = self.direct_lookup(family, style) {
            return Some(path);
        }
        lookup_in_index(self.index(), family, style).map(|face| face.path.clone())
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

// an installed family first, then what the font configuration prefers for the name or its
// css generic, then the platform's default for the generic
fn lookup_in_index<'a>(
    index: &'a FontIndex,
    family: &str,
    style: FontStyle,
) -> Option<&'a IndexedFace> {
    if let Some(face) = installed_in(index, family, style) {
        return Some(face);
    }
    // what is configured for system-ui, then for the sans-serif it is one of
    let sans = (css_generic(family) == Some("system-ui")).then_some("sans-serif");
    let preferred = [Some(family), css_generic(family), sans]
        .into_iter()
        .flatten()
        .filter_map(|name| index.preferred.get(name))
        .flatten()
        .find_map(|name| installed_in(index, name, style));
    if preferred.is_some() {
        return preferred;
    }
    if let Some(default) = generic_family(family).or_else(|| substitute(family)) {
        return installed_in(index, default, style);
    }
    // a width variant that is not installed, such as arial narrow, takes its base family,
    // as edge and chrome do on windows
    let base = WIDTH_WORDS
        .iter()
        .find_map(|word| family.strip_suffix(word)?.strip_suffix(' '))?;
    lookup_in_index(index, base, style)
}

fn installed_in<'a>(
    index: &'a FontIndex,
    family: &str,
    style: FontStyle,
) -> Option<&'a IndexedFace> {
    let start = index
        .faces
        .partition_point(|face| face.family.as_str() < family);
    let end = index.faces[start..].partition_point(|face| face.family.as_str() == family) + start;
    best_matching_face(&index.faces[start..end], style)
}

// the css font matching order: slant first, then the weight nearest in the direction
// the requested weight leans
fn best_matching_face(matching: &[IndexedFace], style: FontStyle) -> Option<&IndexedFace> {
    if matching.is_empty() {
        return None;
    }
    let slant = if matching.iter().any(|face| face.italic == style.italic) {
        style.italic
    } else {
        !style.italic
    };
    let wanted = style.weight;
    matching
        .iter()
        .filter(|face| face.italic == slant)
        .min_by_key(|face| weight_distance(wanted, face.weight))
}

#[cfg(target_os = "windows")]
fn windows_fonts_dir() -> Option<&'static Path> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(|| {
        std::env::var_os("WINDIR").map(|windows| PathBuf::from(windows).join("Fonts"))
    })
    .as_deref()
}

#[cfg(target_os = "windows")]
type StandardFace = (&'static str, u16, bool);

#[cfg(target_os = "windows")]
fn windows_standard_faces(family: &str) -> Option<(&'static str, &'static [StandardFace])> {
    Some(match family {
        "segoe ui" => (
            "segoe ui",
            &[
                ("segoeuil.ttf", 300, false),
                ("seguili.ttf", 300, true),
                ("segoeuisl.ttf", 350, false),
                ("seguisli.ttf", 350, true),
                ("segoeui.ttf", 400, false),
                ("segoeuii.ttf", 400, true),
                ("seguisb.ttf", 600, false),
                ("seguisbi.ttf", 600, true),
                ("segoeuib.ttf", 700, false),
                ("segoeuiz.ttf", 700, true),
                ("seguibl.ttf", 900, false),
                ("seguibli.ttf", 900, true),
            ],
        ),
        "arial" => (
            "arial",
            &[
                ("arial.ttf", 400, false),
                ("ariali.ttf", 400, true),
                ("arialbd.ttf", 700, false),
                ("arialbi.ttf", 700, true),
            ],
        ),
        "consolas" => (
            "consolas",
            &[
                ("consola.ttf", 400, false),
                ("consolai.ttf", 400, true),
                ("consolab.ttf", 700, false),
                ("consolaz.ttf", 700, true),
            ],
        ),
        "tahoma" => (
            "tahoma",
            &[("tahoma.ttf", 400, false), ("tahomabd.ttf", 700, false)],
        ),
        "segoe ui symbol" => ("segoe ui symbol", &[("seguisym.ttf", 400, false)]),
        "segoe ui emoji" => ("segoe ui emoji", &[("seguiemj.ttf", 400, false)]),
        "segoe ui historic" => ("segoe ui historic", &[("seguihis.ttf", 400, false)]),
        "times new roman" => (
            "times new roman",
            &[
                ("times.ttf", 400, false),
                ("timesi.ttf", 400, true),
                ("timesbd.ttf", 700, false),
                ("timesbi.ttf", 700, true),
            ],
        ),
        "courier new" => (
            "courier new",
            &[
                ("cour.ttf", 400, false),
                ("couri.ttf", 400, true),
                ("courbd.ttf", 700, false),
                ("courbi.ttf", 700, true),
            ],
        ),
        "cambria" => (
            "cambria",
            &[
                ("cambria.ttc", 400, false),
                ("cambriai.ttf", 400, true),
                ("cambriab.ttf", 700, false),
                ("cambriaz.ttf", 700, true),
            ],
        ),
        "cambria math" => ("cambria math", &[("cambria.ttc", 400, false)]),
        "impact" => ("impact", &[("impact.ttf", 400, false)]),
        "comic sans ms" => (
            "comic sans ms",
            &[
                ("comic.ttf", 400, false),
                ("comici.ttf", 400, true),
                ("comicbd.ttf", 700, false),
                ("comicz.ttf", 700, true),
            ],
        ),
        "georgia" => (
            "georgia",
            &[
                ("georgia.ttf", 400, false),
                ("georgiai.ttf", 400, true),
                ("georgiab.ttf", 700, false),
                ("georgiaz.ttf", 700, true),
            ],
        ),
        "verdana" => (
            "verdana",
            &[
                ("verdana.ttf", 400, false),
                ("verdanai.ttf", 400, true),
                ("verdanab.ttf", 700, false),
                ("verdanaz.ttf", 700, true),
            ],
        ),
        "trebuchet ms" => (
            "trebuchet ms",
            &[
                ("trebuc.ttf", 400, false),
                ("trebucit.ttf", 400, true),
                ("trebucbd.ttf", 700, false),
                ("trebucbi.ttf", 700, true),
            ],
        ),
        "calibri" => (
            "calibri",
            &[
                ("calibril.ttf", 300, false),
                ("calibrili.ttf", 300, true),
                ("calibri.ttf", 400, false),
                ("calibrii.ttf", 400, true),
                ("calibrib.ttf", 700, false),
                ("calibriz.ttf", 700, true),
            ],
        ),
        "cascadia mono" => ("cascadia mono", &[("CascadiaMono.ttf", 400, false)]),
        "cascadia code" => ("cascadia code", &[("CascadiaCode.ttf", 400, false)]),
        _ => return None,
    })
}

impl FontSource for SystemFonts {
    fn face(&self, family: &str, style: FontStyle) -> Option<Rc<FontFace>> {
        let path = self.lookup_path(family, style)?;
        self.load(&path)
    }

    // the platform's broad-coverage families first, then any installed face that has it
    fn fallback(&self, character: char, style: FontStyle) -> Option<Rc<FontFace>> {
        if let Some(path) = self.fallbacks.borrow().get(&character).cloned() {
            return path.and_then(|path| self.load(&path));
        }
        let covers = |path: &Path| {
            self.load(path)
                .is_some_and(|loaded| loaded.has_glyph(character))
        };
        let found = FALLBACK_FAMILIES
            .iter()
            .filter_map(|family| self.lookup_path(family, style))
            .find(|path| covers(path))
            .or_else(|| {
                self.index()
                    .faces
                    .iter()
                    .filter(|face| !face.italic)
                    .take(MAX_FALLBACK_PROBES)
                    .find(|face| covers(&face.path))
                    .map(|face| face.path.clone())
            });
        self.fallbacks.borrow_mut().insert(character, found.clone());
        found.and_then(|path| self.load(&path))
    }

    fn emoji(&self, style: FontStyle) -> Option<Rc<FontFace>> {
        let path = self.lookup_path(EMOJI_FAMILY, style)?;
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

// the generic families chromium answers; a name such as -apple-system or ui-sans-serif is
// one it does not know off a mac, so a stack moves on past it
fn css_generic(family: &str) -> Option<&'static str> {
    match family {
        "sans-serif" | "serif" | "monospace" | "system-ui" | "cursive" | "fantasy" | "math" => [
            "sans-serif",
            "serif",
            "monospace",
            "system-ui",
            "cursive",
            "fantasy",
            "math",
        ]
        .into_iter()
        .find(|name| *name == family),
        "-apple-system" | "blinkmacsystemfont" if cfg!(target_os = "macos") => Some("system-ui"),
        _ => None,
    }
}

// the family each generic stands for, as chromium's defaults have them on each platform
fn generic_family(family: &str) -> Option<&'static str> {
    let windows = cfg!(target_os = "windows");
    let mac = cfg!(target_os = "macos");
    Some(match css_generic(family)? {
        "sans-serif" if windows => "arial",
        "sans-serif" if mac => "helvetica",
        "sans-serif" => "dejavu sans",
        "system-ui" if windows => "segoe ui",
        "system-ui" if mac => "helvetica neue",
        "system-ui" => "dejavu sans",
        "serif" | "math" | "cursive" | "fantasy" if !windows && !mac => "dejavu serif",
        "serif" if windows => "times new roman",
        "serif" => "times",
        "cursive" if windows => "comic sans ms",
        "cursive" => "apple chancery",
        "fantasy" if windows => "impact",
        "fantasy" => "papyrus",
        "math" if windows => "cambria math",
        "math" => "stix two math",
        _ if windows => "consolas",
        _ if mac => "menlo",
        _ => "dejavu sans mono",
    })
}

// the families windows stands in for names it does not ship
fn substitute(family: &str) -> Option<&'static str> {
    match family {
        "helvetica" if cfg!(target_os = "windows") => Some("arial"),
        "courier" if cfg!(target_os = "windows") => Some("courier new"),
        "times" if cfg!(target_os = "windows") => Some("times new roman"),
        _ => None,
    }
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

fn index_directory(
    directory: &Path,
    faces: &mut Vec<IndexedFace>,
    scratch: &mut Vec<u8>,
    depth: u8,
) {
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
            index_directory(&path, faces, scratch, depth + 1);
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
        if let Some(face) = read_face_names(&path, scratch) {
            faces.push(face);
        }
    }
}

// only the table directory and name table are read, so indexing stays cheap
fn read_face_names(path: &Path, scratch: &mut Vec<u8>) -> Option<IndexedFace> {
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
    scratch.resize(table_count * 16, 0);
    file.seek(SeekFrom::Start(base + 12)).ok()?;
    file.read_exact(scratch).ok()?;
    let mut name_table = None;
    let mut os2_table = None;
    for record in scratch.as_chunks::<16>().0 {
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
    scratch.resize(length, 0);
    file.seek(SeekFrom::Start(offset)).ok()?;
    file.read_exact(scratch).ok()?;
    let family = read_name(scratch, &[16, 1])?.to_ascii_lowercase();
    let subfamily = read_name(scratch, &[17, 2])
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
                    .as_chunks::<2>()
                    .0
                    .iter()
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
    use super::{
        CustomIndex, FontIndex, IndexedFace, SystemFonts, generic_family, shared_index,
        weight_distance,
    };
    use ceri_engine::{FontSource, FontStyle};
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn test_fonts(faces: Vec<IndexedFace>, preferred: HashMap<String, Vec<String>>) -> SystemFonts {
        SystemFonts {
            custom: Some(CustomIndex::inline(faces, preferred)),
            #[cfg(target_os = "windows")]
            direct: RefCell::new(HashMap::new()),
            loaded: RefCell::new(HashMap::new()),
            fallbacks: RefCell::new(HashMap::new()),
        }
    }

    fn lazy_with_counter(scans: Arc<AtomicUsize>, fallback: Vec<IndexedFace>) -> SystemFonts {
        SystemFonts {
            custom: Some(CustomIndex::lazy(move || {
                scans.fetch_add(1, Ordering::SeqCst);
                FontIndex {
                    faces: fallback.clone(),
                    preferred: HashMap::new(),
                }
            })),
            #[cfg(target_os = "windows")]
            direct: RefCell::new(HashMap::new()),
            loaded: RefCell::new(HashMap::new()),
            fallbacks: RefCell::new(HashMap::new()),
        }
    }

    #[test]
    fn generic_families_map_to_platform_faces() {
        assert!(generic_family("sans-serif").is_some());
        assert!(generic_family("monospace").is_some());
        assert!(generic_family("serif").is_some());
        assert_eq!(generic_family("Aptos Narrow"), None);
    }

    #[test]
    fn a_missing_width_variant_takes_its_base_family() {
        let fonts = test_fonts(
            vec![IndexedFace {
                family: "arial".into(),
                path: PathBuf::from("arial.ttf"),
                weight: 400,
                italic: false,
            }],
            HashMap::new(),
        );
        let found = |family| fonts.lookup_path(family, FontStyle::NORMAL);
        assert_eq!(found("arial narrow"), Some(PathBuf::from("arial.ttf")));
        assert_eq!(
            found("arial semi condensed"),
            Some(PathBuf::from("arial.ttf"))
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
        let fonts = test_fonts(
            vec![
                face("cantarell"),
                face("liberation sans"),
                face("fira mono"),
            ],
            HashMap::from([
                (
                    "sans-serif".to_string(),
                    list(&["noto sans", "liberation sans"]),
                ),
                ("system-ui".to_string(), list(&["inter"])),
                ("monospace".to_string(), list(&["noto mono"])),
                ("helvetica".to_string(), list(&["cantarell"])),
            ]),
        );
        let found = |family| {
            fonts
                .lookup_path(family, FontStyle::NORMAL)
                .map(|path| path.to_string_lossy().into_owned())
        };
        assert_eq!(found("sans-serif").as_deref(), Some("liberation sans.ttf"));
        // system-ui has its own list, then falls back to the sans-serif one
        assert_eq!(found("system-ui").as_deref(), Some("liberation sans.ttf"));
        assert_eq!(found("helvetica").as_deref(), Some("cantarell.ttf"));
        assert_eq!(found("fira mono").as_deref(), Some("fira mono.ttf"));
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
    fn lazy_fonts_defer_and_cache_the_full_directory_scan() {
        let scans = Arc::new(AtomicUsize::new(0));
        let fonts = lazy_with_counter(scans.clone(), Vec::new());
        assert_eq!(scans.load(Ordering::SeqCst), 0);

        // repeated unknown-family misses scan at most once
        assert!(
            fonts
                .face("ceri-nonexistent-family-a", FontStyle::NORMAL)
                .is_none()
        );
        assert_eq!(scans.load(Ordering::SeqCst), 1);
        assert!(
            fonts
                .face("ceri-nonexistent-family-b", FontStyle::NORMAL)
                .is_none()
        );
        assert_eq!(scans.load(Ordering::SeqCst), 1);

        // process-wide shared instances reuse the same cached index
        let first = SystemFonts::lazy();
        let second = SystemFonts::lazy();
        assert!(Arc::ptr_eq(shared_index(), shared_index()));
        assert_eq!(first.families(), second.families());
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_standard_families_resolve_directly_without_directory_scan() {
        let scans = Arc::new(AtomicUsize::new(0));
        let fonts = lazy_with_counter(scans.clone(), Vec::new());

        for family in [
            "segoe ui",
            "arial",
            "consolas",
            "tahoma",
            "segoe ui symbol",
            "segoe ui emoji",
            "times new roman",
            "courier new",
            "cambria math",
            "impact",
            "comic sans ms",
            "georgia",
            "verdana",
            "trebuchet ms",
            "calibri",
            "sans-serif",
            "system-ui",
            "serif",
            "monospace",
            "cursive",
            "fantasy",
            "math",
            "helvetica",
            "courier",
            "times",
        ] {
            assert!(
                fonts.face(family, FontStyle::NORMAL).is_some(),
                "standard family {family} should resolve directly from WINDIR\\Fonts"
            );
        }
        assert!(fonts.emoji(FontStyle::NORMAL).is_some());
        assert!(fonts.fallback('\u{2192}', FontStyle::NORMAL).is_some());

        // verify weight and italic matching on the direct path
        let regular = fonts
            .lookup_path("segoe ui", FontStyle::NORMAL)
            .expect("segoe ui regular");
        let bold = fonts
            .lookup_path(
                "segoe ui",
                FontStyle {
                    weight: 700,
                    ..FontStyle::NORMAL
                },
            )
            .expect("segoe ui bold");
        let semibold = fonts
            .lookup_path(
                "segoe ui",
                FontStyle {
                    weight: 600,
                    ..FontStyle::NORMAL
                },
            )
            .expect("segoe ui semibold");
        let italic = fonts
            .lookup_path(
                "segoe ui",
                FontStyle {
                    italic: true,
                    ..FontStyle::NORMAL
                },
            )
            .expect("segoe ui italic");
        assert!(regular.ends_with("segoeui.ttf"), "{regular:?}");
        assert!(bold.ends_with("segoeuib.ttf"), "{bold:?}");
        assert!(semibold.ends_with("seguisb.ttf"), "{semibold:?}");
        assert!(italic.ends_with("segoeuii.ttf"), "{italic:?}");

        assert_eq!(
            scans.load(Ordering::SeqCst),
            0,
            "standard windows families must not trigger a full directory scan"
        );

        // only an unknown family falls back to the cached full scan
        assert!(
            fonts
                .face("ceri-nonexistent-family", FontStyle::NORMAL)
                .is_none()
        );
        assert_eq!(scans.load(Ordering::SeqCst), 1);
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
            fonts.index().faces.iter().any(|face| {
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
