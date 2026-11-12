// production builds of Preact, Vue and Svelte, each driving a counter and a list app

use ceri_engine::{Browser, ResourceBundle};
use std::path::Path;

fn open(fixture: &str) -> Browser {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(fixture);
    let resources = ResourceBundle::from_directory(&root).expect("fixture should load");
    let mut browser = Browser::new(resources);
    browser
        .navigate("https://app.ceri.invalid/index.html")
        .expect("fixture page should load");
    settle(&mut browser);
    if let Some(failure) = browser.last_navigation_failure() {
        panic!("{fixture} failed to load: {failure:?}");
    }
    browser
}

// lets scheduled renders, effects and animation frames run
fn settle(browser: &mut Browser) {
    for _ in 0..4 {
        browser.advance_time(16).expect("timers should run");
    }
}

fn text(browser: &mut Browser, id: &str) -> String {
    let document = browser.document().expect("document should exist");
    let document = document.borrow();
    let node = document
        .find_element_by_id(id)
        .unwrap_or_else(|| panic!("#{id} should exist; logs: {:?}", browser.console_logs()));
    document.text_content(node)
}

fn click(browser: &mut Browser, id: &str) {
    assert!(
        browser.click_id(id).expect("click should dispatch"),
        "#{id} should take the click"
    );
    settle(browser);
}

fn type_into(browser: &mut Browser, id: &str, value: &str) {
    browser.click_id(id).expect("the field should focus");
    browser.insert_text(value).expect("text should insert");
    settle(browser);
}

fn exercise(fixture: &str) {
    let mut browser = open(fixture);
    assert_eq!(
        text(&mut browser, "count"),
        "count: 0",
        "{fixture} first render"
    );
    click(&mut browser, "increment");
    click(&mut browser, "increment");
    assert_eq!(text(&mut browser, "count"), "count: 2", "{fixture} counter");
    assert_eq!(
        text(&mut browser, "items"),
        "alphabeta",
        "{fixture} first list"
    );
    type_into(&mut browser, "draft", "gamma");
    click(&mut browser, "add");
    assert_eq!(
        text(&mut browser, "items"),
        "alphabetagamma",
        "{fixture} added item"
    );
    click(&mut browser, "drop");
    assert_eq!(
        text(&mut browser, "items"),
        "betagamma",
        "{fixture} dropped item"
    );
    let errors = browser
        .console_logs()
        .iter()
        .filter(|line| line.starts_with("Uncaught"))
        .cloned()
        .collect::<Vec<_>>();
    assert!(errors.is_empty(), "{fixture} reported {errors:?}");
}

#[test]
fn preact_runs_a_counter_and_a_list() {
    exercise("preact");
}

#[test]
fn vue_runs_a_counter_and_a_list() {
    exercise("vue");
}

#[test]
fn svelte_runs_a_counter_and_a_list() {
    exercise("svelte");
}
