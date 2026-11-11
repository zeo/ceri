use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub const APP_ORIGIN: &str = "https://app.ceri.invalid";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequestTarget {
    TopLevelNavigation,
    Subresource,
}

// files compiled into the app by ceri_build::embed, named by their path under the folder
pub type EmbeddedAssets = &'static [(&'static str, &'static [u8])];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedAsset {
    pub path: PathBuf,
    pub relative_path: PathBuf,
    pub embedded: Option<&'static [u8]>,
}

impl ResolvedAsset {
    pub fn read(&self) -> io::Result<Vec<u8>> {
        match self.embedded {
            Some(bytes) => Ok(bytes.to_vec()),
            None => fs::read(&self.path),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AssetRoute {
    File(ResolvedAsset),
    AppShell(ResolvedAsset),
    NotFound,
    Rejected(RejectReason),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RejectReason {
    InvalidUrl,
    InsecureScheme,
    WrongOrigin,
    Credentials,
    Port,
    Fragment,
    InvalidPercentEncoding,
    EncodedSeparator,
    Backslash,
    NulByte,
    Colon,
    DotSegment,
    EmptySegment,
    InvalidUtf8,
    UnsafeCharacter,
    UnsafeWindowsName,
    PathEscapesRoot,
}

#[derive(Clone, Debug)]
pub struct AssetOrigin {
    root: Root,
}

#[derive(Clone, Debug)]
enum Root {
    Folder(PathBuf),
    Embedded(EmbeddedAssets),
}

impl AssetOrigin {
    pub fn new(root: impl AsRef<Path>) -> io::Result<Self> {
        let root = fs::canonicalize(root)?;
        if !fs::metadata(&root)?.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "asset root is not a directory",
            ));
        }

        Ok(Self {
            root: Root::Folder(root),
        })
    }

    pub fn embedded(files: EmbeddedAssets) -> Self {
        Self {
            root: Root::Embedded(files),
        }
    }

    // the folder the assets are read from, none when they are compiled in
    pub fn root(&self) -> Option<&Path> {
        match &self.root {
            Root::Folder(root) => Some(root),
            Root::Embedded(_) => None,
        }
    }

    pub fn route(&self, request_url: &str, target: RequestTarget) -> io::Result<AssetRoute> {
        let relative_path = match request_path(request_url) {
            Ok(path) => path,
            Err(reason) => return Ok(AssetRoute::Rejected(reason)),
        };

        if relative_path.as_os_str().is_empty() {
            return self.app_shell();
        }

        match self.locate(&relative_path)? {
            Located::File(asset) => Ok(AssetRoute::File(asset)),
            Located::EscapesRoot => Ok(AssetRoute::Rejected(RejectReason::PathEscapesRoot)),
            Located::Missing
                if target == RequestTarget::TopLevelNavigation
                    && is_extensionless(&relative_path) =>
            {
                self.app_shell()
            }
            Located::Missing => Ok(AssetRoute::NotFound),
        }
    }

    fn app_shell(&self) -> io::Result<AssetRoute> {
        match self.locate(Path::new("index.html"))? {
            Located::File(asset) => Ok(AssetRoute::AppShell(asset)),
            Located::Missing => Ok(AssetRoute::NotFound),
            Located::EscapesRoot => Ok(AssetRoute::Rejected(RejectReason::PathEscapesRoot)),
        }
    }

    fn locate(&self, relative_path: &Path) -> io::Result<Located> {
        let root = match &self.root {
            Root::Folder(root) => root,
            Root::Embedded(files) => {
                // the request path was decoded from UTF-8 and every segment checked
                let name = relative_path
                    .iter()
                    .filter_map(|segment| segment.to_str())
                    .collect::<Vec<_>>()
                    .join("/");
                return Ok(files.iter().find(|(file, _)| *file == name).map_or(
                    Located::Missing,
                    |(_, bytes)| {
                        Located::File(ResolvedAsset {
                            path: relative_path.to_path_buf(),
                            relative_path: relative_path.to_path_buf(),
                            embedded: Some(bytes),
                        })
                    },
                ));
            }
        };
        let candidate = root.join(relative_path);
        let path = match fs::canonicalize(candidate) {
            Ok(path) => path,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                ) =>
            {
                return Ok(Located::Missing);
            }
            Err(error) => return Err(error),
        };

        if !path.starts_with(root) {
            return Ok(Located::EscapesRoot);
        }

        if !fs::metadata(&path)?.is_file() {
            return Ok(Located::Missing);
        }

        Ok(Located::File(ResolvedAsset {
            path,
            relative_path: relative_path.to_path_buf(),
            embedded: None,
        }))
    }
}

enum Located {
    File(ResolvedAsset),
    Missing,
    EscapesRoot,
}

fn request_path(request_url: &str) -> Result<PathBuf, RejectReason> {
    let scheme_end = request_url.find("://").ok_or(RejectReason::InvalidUrl)?;
    let scheme = &request_url[..scheme_end];
    if !scheme.eq_ignore_ascii_case("https") {
        return Err(RejectReason::InsecureScheme);
    }

    let remainder = &request_url[scheme_end + 3..];
    let authority_end = remainder.find(['/', '?', '#']).unwrap_or(remainder.len());
    let authority = &remainder[..authority_end];

    if authority.contains('@') {
        return Err(RejectReason::Credentials);
    }
    if authority.contains(':') {
        return Err(RejectReason::Port);
    }
    if !authority.eq_ignore_ascii_case("app.ceri.invalid") {
        return Err(RejectReason::WrongOrigin);
    }

    let request_target = &remainder[authority_end..];
    if request_target.contains('#') {
        return Err(RejectReason::Fragment);
    }

    let raw_path = request_target
        .split_once('?')
        .map_or(request_target, |(path, _query)| path);
    let raw_path = if raw_path.is_empty() { "/" } else { raw_path };
    let decoded_path = decode_path(raw_path)?;

    relative_path(&decoded_path)
}

fn decode_path(raw_path: &str) -> Result<String, RejectReason> {
    if !raw_path.starts_with('/') {
        return Err(RejectReason::InvalidUrl);
    }

    let bytes = raw_path.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut cursor = 0;

    while cursor < bytes.len() {
        let byte = bytes[cursor];
        if byte == b'%' {
            let high = bytes
                .get(cursor + 1)
                .and_then(|value| hex_value(*value))
                .ok_or(RejectReason::InvalidPercentEncoding)?;
            let low = bytes
                .get(cursor + 2)
                .and_then(|value| hex_value(*value))
                .ok_or(RejectReason::InvalidPercentEncoding)?;
            let value = (high << 4) | low;
            reject_path_byte(value, true)?;
            decoded.push(value);
            cursor += 3;
            continue;
        }

        reject_path_byte(byte, false)?;
        decoded.push(byte);
        cursor += 1;
    }

    String::from_utf8(decoded).map_err(|_| RejectReason::InvalidUtf8)
}

fn reject_path_byte(byte: u8, encoded: bool) -> Result<(), RejectReason> {
    match byte {
        b'/' if encoded => Err(RejectReason::EncodedSeparator),
        b'\\' if encoded => Err(RejectReason::EncodedSeparator),
        b'\\' => Err(RejectReason::Backslash),
        0 => Err(RejectReason::NulByte),
        b':' => Err(RejectReason::Colon),
        1..=31 | 127 => Err(RejectReason::UnsafeCharacter),
        _ => Ok(()),
    }
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn relative_path(decoded_path: &str) -> Result<PathBuf, RejectReason> {
    if decoded_path == "/" {
        return Ok(PathBuf::new());
    }

    let body = decoded_path
        .strip_prefix('/')
        .ok_or(RejectReason::InvalidUrl)?;
    let body = body.strip_suffix('/').unwrap_or(body);
    if body.is_empty() {
        return Err(RejectReason::EmptySegment);
    }

    let mut path = PathBuf::new();
    for segment in body.split('/') {
        validate_segment(segment)?;
        path.push(segment);
    }

    Ok(path)
}

fn validate_segment(segment: &str) -> Result<(), RejectReason> {
    if segment.is_empty() {
        return Err(RejectReason::EmptySegment);
    }
    if matches!(segment, "." | "..") {
        return Err(RejectReason::DotSegment);
    }
    if segment.ends_with('.') || segment.ends_with(' ') {
        return Err(RejectReason::UnsafeWindowsName);
    }
    if segment
        .bytes()
        .any(|byte| matches!(byte, b'<' | b'>' | b'"' | b'|' | b'?' | b'*'))
    {
        return Err(RejectReason::UnsafeCharacter);
    }
    if is_windows_device_name(segment) {
        return Err(RejectReason::UnsafeWindowsName);
    }

    Ok(())
}

fn is_windows_device_name(segment: &str) -> bool {
    let stem = segment.split('.').next().unwrap_or(segment);
    let upper = stem.to_ascii_uppercase();

    matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || matches!(
            upper.strip_prefix("COM"),
            Some("1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
        )
        || matches!(
            upper.strip_prefix("LPT"),
            Some("1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
        )
}

fn is_extensionless(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| !name.contains('.'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TREE: AtomicU64 = AtomicU64::new(0);

    struct TestTree {
        path: PathBuf,
    }

    impl TestTree {
        fn new(label: &str) -> Self {
            let sequence = NEXT_TREE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "ceri-assets-{}-{sequence}-{label}",
                std::process::id()
            ));
            fs::create_dir_all(&path).unwrap();
            Self { path }
        }

        fn write(&self, relative_path: &str, contents: &[u8]) {
            let path = self.path.join(relative_path);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(path, contents).unwrap();
        }

        fn origin(&self) -> AssetOrigin {
            AssetOrigin::new(&self.path).unwrap()
        }
    }

    impl Drop for TestTree {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn populated_tree(label: &str) -> TestTree {
        let tree = TestTree::new(label);
        tree.write("index.html", b"app shell");
        tree.write("assets/app.js", b"application code");
        tree
    }

    fn expect_file(route: AssetRoute, relative_path: &str) -> ResolvedAsset {
        let AssetRoute::File(asset) = route else {
            panic!("expected file route, got {route:?}");
        };
        assert_eq!(asset.relative_path, PathBuf::from(relative_path));
        assert!(asset.path.is_absolute());
        asset
    }

    fn expect_shell(route: AssetRoute) -> ResolvedAsset {
        let AssetRoute::AppShell(asset) = route else {
            panic!("expected app shell route, got {route:?}");
        };
        assert_eq!(asset.relative_path, PathBuf::from("index.html"));
        assert!(asset.path.is_absolute());
        asset
    }

    #[test]
    fn embedded_assets_route_as_a_folder_does() {
        static FILES: EmbeddedAssets = &[
            ("index.html", b"app shell"),
            ("assets/app.js", b"application code"),
        ];
        let origin = AssetOrigin::embedded(FILES);
        let route = |url: &str, target| origin.route(url, target).unwrap();
        let AssetRoute::File(script) = route(
            "https://app.ceri.invalid/assets/app.js?v=2",
            RequestTarget::Subresource,
        ) else {
            panic!("the script is embedded");
        };
        assert_eq!(script.read().unwrap(), b"application code");
        let AssetRoute::AppShell(shell) = route(
            "https://app.ceri.invalid/library",
            RequestTarget::TopLevelNavigation,
        ) else {
            panic!("an extensionless route falls back to the shell");
        };
        assert_eq!(shell.read().unwrap(), b"app shell");
        assert_eq!(
            route(
                "https://app.ceri.invalid/assets/missing.js",
                RequestTarget::Subresource
            ),
            AssetRoute::NotFound
        );
        assert_eq!(
            route(
                "https://app.ceri.invalid/assets",
                RequestTarget::Subresource
            ),
            AssetRoute::NotFound
        );
        assert_eq!(
            route(
                "https://app.ceri.invalid/assets/%2e%2e/index.html",
                RequestTarget::Subresource
            ),
            AssetRoute::Rejected(RejectReason::DotSegment)
        );
        assert_eq!(origin.root(), None);
    }

    #[test]
    fn root_maps_to_app_shell() {
        let tree = populated_tree("root");
        let origin = tree.origin();

        expect_shell(
            origin
                .route(
                    "https://app.ceri.invalid/",
                    RequestTarget::TopLevelNavigation,
                )
                .unwrap(),
        );
        expect_shell(
            origin
                .route(
                    "https://app.ceri.invalid",
                    RequestTarget::TopLevelNavigation,
                )
                .unwrap(),
        );
    }

    #[test]
    fn existing_assets_map_directly() {
        let tree = populated_tree("static");
        let origin = tree.origin();

        expect_file(
            origin
                .route(
                    "https://app.ceri.invalid/assets/app.js",
                    RequestTarget::Subresource,
                )
                .unwrap(),
            "assets/app.js",
        );
    }

    #[test]
    fn existing_extensionless_files_map_directly() {
        let tree = populated_tree("extensionless-file");
        tree.write("manifest", b"manifest data");
        let origin = tree.origin();

        expect_file(
            origin
                .route(
                    "https://app.ceri.invalid/manifest",
                    RequestTarget::TopLevelNavigation,
                )
                .unwrap(),
            "manifest",
        );
    }

    #[test]
    fn missing_extensionless_navigation_maps_to_app_shell() {
        let tree = populated_tree("spa");
        let origin = tree.origin();

        expect_shell(
            origin
                .route(
                    "https://app.ceri.invalid/projects/ceri/",
                    RequestTarget::TopLevelNavigation,
                )
                .unwrap(),
        );
    }

    #[test]
    fn missing_subresource_never_maps_to_app_shell() {
        let tree = populated_tree("missing-resource");
        let origin = tree.origin();

        assert_eq!(
            origin
                .route(
                    "https://app.ceri.invalid/api/session",
                    RequestTarget::Subresource,
                )
                .unwrap(),
            AssetRoute::NotFound
        );
    }

    #[test]
    fn missing_navigation_with_an_extension_is_not_found() {
        let tree = populated_tree("missing-extension");
        let origin = tree.origin();

        assert_eq!(
            origin
                .route(
                    "https://app.ceri.invalid/missing.html",
                    RequestTarget::TopLevelNavigation,
                )
                .unwrap(),
            AssetRoute::NotFound
        );
    }

    #[test]
    fn query_is_ignored_for_path_mapping() {
        let tree = populated_tree("query");
        let origin = tree.origin();

        expect_file(
            origin
                .route(
                    "https://app.ceri.invalid/assets/app.js?v=317&mode=dark",
                    RequestTarget::Subresource,
                )
                .unwrap(),
            "assets/app.js",
        );
        expect_shell(
            origin
                .route(
                    "https://app.ceri.invalid/search?q=rust",
                    RequestTarget::TopLevelNavigation,
                )
                .unwrap(),
        );
    }

    #[test]
    fn fragment_is_rejected() {
        let tree = populated_tree("fragment");
        let origin = tree.origin();

        assert_eq!(
            origin
                .route(
                    "https://app.ceri.invalid/index.html#section",
                    RequestTarget::TopLevelNavigation,
                )
                .unwrap(),
            AssetRoute::Rejected(RejectReason::Fragment)
        );
    }

    #[test]
    fn scheme_and_origin_are_exact() {
        let tree = populated_tree("origin");
        let origin = tree.origin();

        for (url, reason) in [
            ("http://app.ceri.invalid/", RejectReason::InsecureScheme),
            ("https://evil.invalid/", RejectReason::WrongOrigin),
            (
                "https://app.ceri.invalid.evil.invalid/",
                RejectReason::WrongOrigin,
            ),
            ("https://user@app.ceri.invalid/", RejectReason::Credentials),
            ("https://app.ceri.invalid:443/", RejectReason::Port),
        ] {
            assert_eq!(
                origin
                    .route(url, RequestTarget::TopLevelNavigation)
                    .unwrap(),
                AssetRoute::Rejected(reason),
                "unexpected route for {url}"
            );
        }
    }

    #[test]
    fn scheme_and_host_follow_url_case_rules() {
        let tree = populated_tree("origin-case");
        let origin = tree.origin();

        expect_shell(
            origin
                .route(
                    "HTTPS://APP.CERI.INVALID/",
                    RequestTarget::TopLevelNavigation,
                )
                .unwrap(),
        );
    }

    #[test]
    fn raw_and_encoded_traversal_are_rejected() {
        let tree = populated_tree("traversal");
        let origin = tree.origin();

        for url in [
            "https://app.ceri.invalid/../secret.txt",
            "https://app.ceri.invalid/%2e%2e/secret.txt",
            "https://app.ceri.invalid/%2E./secret.txt",
            "https://app.ceri.invalid/safe/./secret.txt",
        ] {
            assert_eq!(
                origin.route(url, RequestTarget::Subresource).unwrap(),
                AssetRoute::Rejected(RejectReason::DotSegment),
                "unexpected route for {url}"
            );
        }
    }

    #[test]
    fn separators_cannot_be_smuggled_through_encoding() {
        let tree = populated_tree("separators");
        let origin = tree.origin();

        for url in [
            "https://app.ceri.invalid/assets%2fapp.js",
            "https://app.ceri.invalid/assets%2Fapp.js",
            "https://app.ceri.invalid/assets%5capp.js",
            "https://app.ceri.invalid/assets%5Capp.js",
        ] {
            assert_eq!(
                origin.route(url, RequestTarget::Subresource).unwrap(),
                AssetRoute::Rejected(RejectReason::EncodedSeparator),
                "unexpected route for {url}"
            );
        }

        assert_eq!(
            origin
                .route(
                    "https://app.ceri.invalid/assets\\app.js",
                    RequestTarget::Subresource,
                )
                .unwrap(),
            AssetRoute::Rejected(RejectReason::Backslash)
        );
    }

    #[test]
    fn nul_and_colon_are_rejected_after_decoding() {
        let tree = populated_tree("nul-colon");
        let origin = tree.origin();

        assert_eq!(
            origin
                .route(
                    "https://app.ceri.invalid/assets/a%00.js",
                    RequestTarget::Subresource,
                )
                .unwrap(),
            AssetRoute::Rejected(RejectReason::NulByte)
        );
        assert_eq!(
            origin
                .route(
                    "https://app.ceri.invalid/C%3a/secret.txt",
                    RequestTarget::Subresource,
                )
                .unwrap(),
            AssetRoute::Rejected(RejectReason::Colon)
        );
    }

    #[test]
    fn malformed_encoding_and_invalid_utf8_are_rejected() {
        let tree = populated_tree("encoding");
        let origin = tree.origin();

        for url in [
            "https://app.ceri.invalid/assets/%",
            "https://app.ceri.invalid/assets/%2",
            "https://app.ceri.invalid/assets/%zz",
        ] {
            assert_eq!(
                origin.route(url, RequestTarget::Subresource).unwrap(),
                AssetRoute::Rejected(RejectReason::InvalidPercentEncoding),
                "unexpected route for {url}"
            );
        }
        assert_eq!(
            origin
                .route(
                    "https://app.ceri.invalid/assets/%ff.js",
                    RequestTarget::Subresource,
                )
                .unwrap(),
            AssetRoute::Rejected(RejectReason::InvalidUtf8)
        );
    }

    #[test]
    fn empty_segments_are_rejected() {
        let tree = populated_tree("empty-segments");
        let origin = tree.origin();

        for url in [
            "https://app.ceri.invalid//assets/app.js",
            "https://app.ceri.invalid/assets//app.js",
            "https://app.ceri.invalid//",
        ] {
            assert_eq!(
                origin.route(url, RequestTarget::Subresource).unwrap(),
                AssetRoute::Rejected(RejectReason::EmptySegment),
                "unexpected route for {url}"
            );
        }
    }

    #[test]
    fn windows_aliases_and_unsafe_characters_are_rejected_everywhere() {
        let tree = populated_tree("windows-names");
        let origin = tree.origin();

        for url in [
            "https://app.ceri.invalid/con",
            "https://app.ceri.invalid/COM1.txt",
            "https://app.ceri.invalid/assets/name.",
            "https://app.ceri.invalid/assets/name%20",
        ] {
            assert_eq!(
                origin.route(url, RequestTarget::Subresource).unwrap(),
                AssetRoute::Rejected(RejectReason::UnsafeWindowsName),
                "unexpected route for {url}"
            );
        }
        assert_eq!(
            origin
                .route(
                    "https://app.ceri.invalid/assets/a%3fb.js",
                    RequestTarget::Subresource,
                )
                .unwrap(),
            AssetRoute::Rejected(RejectReason::UnsafeCharacter)
        );
    }

    #[test]
    fn percent_encoded_unicode_assets_are_supported() {
        let tree = populated_tree("unicode");
        tree.write("assets/café.txt", b"coffee");
        let origin = tree.origin();

        expect_file(
            origin
                .route(
                    "https://app.ceri.invalid/assets/caf%C3%A9.txt",
                    RequestTarget::Subresource,
                )
                .unwrap(),
            "assets/café.txt",
        );
    }

    #[test]
    fn missing_shell_is_not_found() {
        let tree = TestTree::new("no-shell");
        let origin = tree.origin();

        assert_eq!(
            origin
                .route(
                    "https://app.ceri.invalid/",
                    RequestTarget::TopLevelNavigation,
                )
                .unwrap(),
            AssetRoute::NotFound
        );
    }

    #[test]
    fn directories_are_never_returned_as_files() {
        let tree = populated_tree("directory");
        fs::create_dir_all(tree.path.join("assets/folder")).unwrap();
        let origin = tree.origin();

        assert_eq!(
            origin
                .route(
                    "https://app.ceri.invalid/assets/folder",
                    RequestTarget::Subresource,
                )
                .unwrap(),
            AssetRoute::NotFound
        );
    }

    #[test]
    fn constructor_rejects_a_file_as_the_root() {
        let tree = TestTree::new("file-root");
        tree.write("root.txt", b"not a directory");

        let error = AssetOrigin::new(tree.path.join("root.txt")).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_cannot_escape_the_asset_root() {
        use std::os::unix::fs::symlink;

        let tree = populated_tree("symlink-root");
        let outside = TestTree::new("symlink-outside");
        outside.write("secret.txt", b"secret");
        symlink(
            outside.path.join("secret.txt"),
            tree.path.join("secret.txt"),
        )
        .unwrap();
        let origin = tree.origin();

        assert_eq!(
            origin
                .route(
                    "https://app.ceri.invalid/secret.txt",
                    RequestTarget::Subresource,
                )
                .unwrap(),
            AssetRoute::Rejected(RejectReason::PathEscapesRoot)
        );
    }
}
