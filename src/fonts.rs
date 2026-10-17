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
    loaded: RefCell<HashMap<PathBuf, Option<Rc<FontFace>>>>,
    // what each character fell back to, so the search runs once per character
    fallbacks: RefCell<HashMap<char, Option<PathBuf>>>,
}

impl SystemFonts {
    pub fn discover() -> Self {
        let mut faces = Vec::new();
        for directory in font_directories() {
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
            loaded: RefCell::new(HashMap::new()),
            fallbacks: RefCell::new(HashMap::new()),
        }
    }

    pub fn families(&self) -> usize {
        self.faces.len()
    }

    // the css font matching order: slant first, then the weight nearest in the direction
    // the requested weight leans
    fn lookup(&self, family: &str, style: FontStyle) -> Option<&IndexedFace> {
        let family = generic_family(family).unwrap_or(family);
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
fn generic_family(family: &str) -> Option<&'static str> {
    match family {
        "sans-serif" | "system-ui" | "ui-sans-serif" | "-apple-system" => {
            Some(if cfg!(target_os = "windows") {
                "segoe ui"
            } else if cfg!(target_os = "macos") {
                "helvetica"
            } else {
                "dejavu sans"
            })
        }
        "serif" | "ui-serif" => Some(if cfg!(target_os = "windows") {
            "times new roman"
        } else if cfg!(target_os = "macos") {
            "times"
        } else {
            "dejavu serif"
        }),
        "monospace" | "ui-monospace" => Some(if cfg!(target_os = "windows") {
            "consolas"
        } else if cfg!(target_os = "macos") {
            "menlo"
        } else {
            "dejavu sans mono"
        }),
        _ => None,
    }
}

fn font_directories() -> Vec<PathBuf> {
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
        directories.push(PathBuf::from("/usr/share/fonts"));
        directories.push(PathBuf::from("/usr/local/share/fonts"));
        if let Some(home) = std::env::var_os("HOME") {
            directories.push(PathBuf::from(home).join(".local/share/fonts"));
        }
    }
    directories
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
    use super::{SystemFonts, generic_family, weight_distance};
    use ceri_engine::{FontSource, FontStyle};

    #[test]
    fn generic_families_map_to_platform_faces() {
        assert!(generic_family("sans-serif").is_some());
        assert!(generic_family("monospace").is_some());
        assert!(generic_family("serif").is_some());
        assert_eq!(generic_family("Aptos Narrow"), None);
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
}
