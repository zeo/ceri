use ceri_engine_lab::{Browser, NavigationStatus, ResourceBundle};
use std::path::{Path, PathBuf};

fn continuity_resources(root: &Path) -> ResourceBundle {
    let mut resources = ResourceBundle::default();
    for (path, mime_type) in [
        ("/web/index.html", "text/html; charset=utf-8"),
        ("/web/styles.css", "text/css; charset=utf-8"),
        ("/web/main.js", "text/javascript; charset=utf-8"),
        ("/web/recovery.json", "application/json"),
        ("/sdk/index.js", "text/javascript; charset=utf-8"),
    ] {
        let body = std::fs::read(root.join(path.trim_start_matches('/')))
            .expect("continuity resource");
        resources
            .insert(path, mime_type, body)
            .expect("continuity resource path");
    }
    resources
}

#[test]
fn loads_the_packaged_continuity_page_without_a_recovery_document() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("repository root")
        .to_path_buf();
    let resources = continuity_resources(&root);
    let mut browser = Browser::new(resources);
    assert_eq!(
        browser
            .navigate("https://app.ceri.invalid/web/index.html")
            .expect("continuity navigation"),
        NavigationStatus::Loaded
    );

    let document = browser.document().expect("continuity document");
    assert!(document.borrow().find_element_by_id("recovery").is_none());

    let score = document
        .borrow()
        .find_element_by_id("score")
        .expect("score output");
    assert_eq!(
        document.borrow().element_text_content(&score).as_deref(),
        Some("3 / 4")
    );

    for name in ["module", "storage", "asset"] {
        let path = document
            .borrow()
            .find_element_by_selector(&format!("[data-check=\"{name}\"]"))
            .expect("continuity check");
        assert_eq!(
            document.borrow().element_attribute(&path, "data-state"),
            Some("pass".to_string())
        );
    }

    let runtime = document
        .borrow()
        .find_element_by_selector("[data-check=\"runtime\"]")
        .expect("runtime check");
    assert_eq!(
        document.borrow().element_attribute(&runtime, "data-state"),
        Some("fail".to_string())
    );
    let runtime_detail = document
        .borrow()
        .find_element_by_selector_within(&runtime, ".check__detail")
        .expect("runtime detail");
    assert_eq!(
        document.borrow().element_text_content(&runtime_detail).as_deref(),
        Some("bridge-unavailable: The Ceri host bridge is unavailable in this document")
    );

    let root = document
        .borrow()
        .document_element_path()
        .expect("document element");
    assert_eq!(
        document.borrow().element_attribute(&root, "data-outcome"),
        Some("attention".to_string())
    );
}
