use ceri_engine_lab::{Browser, HostRuntimeInfo, NavigationStatus, ResourceBundle};
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

fn continuity_browser(root: &Path, host_runtime_info: Option<HostRuntimeInfo>) -> Browser {
    let browser = Browser::new(continuity_resources(root));
    match host_runtime_info {
        Some(info) => browser.with_host_runtime_info(info),
        None => browser,
    }
}

#[test]
fn loads_the_packaged_continuity_page_without_a_recovery_document() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("repository root")
        .to_path_buf();
    let mut browser = continuity_browser(
        &root,
        Some(HostRuntimeInfo::new(
            "ceri-lab",
            "none",
            "ready",
            "lab-session",
        )),
    );
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
        Some("4 / 4")
    );

    for name in ["module", "storage", "asset", "runtime"] {
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
    let runtime_detail = document
        .borrow()
        .find_element_by_selector_within(&runtime, ".check__detail")
        .expect("runtime detail");
    assert_eq!(
        document.borrow().element_text_content(&runtime_detail).as_deref(),
        Some("ready; session lab-sess")
    );
    let runtime_line = document
        .borrow()
        .find_element_by_id("runtime-line")
        .expect("runtime line");
    assert_eq!(
        document.borrow().element_text_content(&runtime_line).as_deref(),
        Some("ceri-lab / Chromium none")
    );

    let root = document
        .borrow()
        .document_element_path()
        .expect("document element");
    assert_eq!(
        document.borrow().element_attribute(&root, "data-outcome"),
        Some("ready".to_string())
    );
}

#[test]
fn keeps_the_cef_fallback_when_the_custom_host_is_absent() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("repository root")
        .to_path_buf();
    let mut browser = continuity_browser(&root, None);
    assert_eq!(
        browser
            .navigate("https://app.ceri.invalid/web/index.html")
            .expect("continuity navigation"),
        NavigationStatus::Loaded
    );

    let document = browser.document().expect("continuity document");
    let score = document
        .borrow()
        .find_element_by_id("score")
        .expect("score output");
    assert_eq!(
        document.borrow().element_text_content(&score).as_deref(),
        Some("3 / 4")
    );
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
}
