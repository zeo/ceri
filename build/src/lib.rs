use std::fmt::Write;
use std::path::{Path, PathBuf};

// from an app's build script: compiles every file under the folder into the app, where
// ceri::embedded_assets!() hands them to App::assets
pub fn embed(folder: impl AsRef<Path>) {
    let folder = folder.as_ref();
    println!("cargo::rerun-if-changed={}", folder.display());
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("embed runs in a build script"));
    std::fs::write(out.join("ceri-assets.rs"), table(folder))
        .expect("the build script can write to OUT_DIR");
}

fn table(folder: &Path) -> String {
    let root = std::path::absolute(folder).expect("the asset folder has an absolute path");
    let mut files = Vec::new();
    collect(&root, &mut files);
    files.sort();
    let mut table = String::from("&[\n");
    for file in files {
        let name: Vec<_> = file
            .strip_prefix(&root)
            .expect("collected under the root")
            .iter()
            .map(|part| part.to_str().expect("asset names are UTF-8"))
            .collect();
        let path = file.to_str().expect("asset paths are UTF-8");
        let _ = writeln!(
            table,
            "    ({:?}, include_bytes!({path:?})),",
            name.join("/")
        );
    }
    table + "]\n"
}

fn collect(folder: &Path, files: &mut Vec<PathBuf>) {
    let entries = std::fs::read_dir(folder)
        .unwrap_or_else(|error| panic!("cannot embed {}: {error}", folder.display()));
    for entry in entries {
        let path = entry.expect("the asset folder can be listed").path();
        if path.is_dir() {
            collect(&path, files);
        } else if path.is_file() {
            files.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_table_names_files_by_their_path_under_the_folder() {
        let root = std::env::temp_dir().join(format!("ceri-build-{}", std::process::id()));
        std::fs::create_dir_all(root.join("assets")).unwrap();
        std::fs::write(root.join("index.html"), "<p>").unwrap();
        std::fs::write(root.join("assets").join("app.js"), "1").unwrap();
        let table = super::table(&root);
        std::fs::remove_dir_all(&root).unwrap();
        let lines: Vec<_> = table.lines().collect();
        assert_eq!(lines.len(), 4, "{table}");
        assert!(
            lines[1].starts_with("    (\"assets/app.js\", include_bytes!("),
            "{table}"
        );
        assert!(
            lines[2].starts_with("    (\"index.html\", include_bytes!("),
            "{table}"
        );
    }
}
