// runs the pinned test262 subset under conformance/test262 and compares the outcome with the
// checked-in list of tests known to fail; set CERI_TEST262_UPDATE=1 to rewrite that list,
// or CERI_TEST262_REPORT=<file> to write every failure with its reason
//
// each test runs once: strict when it is flagged onlyStrict, non-strict otherwise; module
// tests are counted as skipped until the runner can host a module graph

use ceri_engine::{Document, ScriptRuntime};
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

// enough for the slowest passing test, so only runaway loops reach it
const STEP_LIMIT: u64 = 250_000_000;
const HOST_PRELUDE: &str = r#"
var print = function () { console.log(Array.prototype.join.call(arguments, " ")); };
var $262 = {
  global: globalThis,
  gc: function () {},
  createRealm: function () { throw new Test262Error("this host has one realm"); },
  detachArrayBuffer: function () { throw new Test262Error("this host cannot detach buffers"); },
  evalScript: function () { throw new Test262Error("this host has no evalScript"); },
  agent: {},
};
"#;

#[derive(Default)]
struct Metadata {
    includes: Vec<String>,
    flags: Vec<String>,
    negative_type: Option<String>,
}

enum Outcome {
    Pass,
    Fail(String),
    Skip,
}

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../conformance/test262")
}

fn collect(directory: &Path, tests: &mut Vec<PathBuf>) {
    let mut entries = fs::read_dir(directory)
        .expect("test directory is readable")
        .map(|entry| entry.expect("directory entry").path())
        .collect::<Vec<_>>();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            collect(&path, tests);
        } else if path.extension().is_some_and(|extension| extension == "js")
            && !path.to_string_lossy().contains("_FIXTURE")
        {
            tests.push(path);
        }
    }
}

// the YAML front matter, read only as far as the runner needs it
fn metadata(source: &str) -> Metadata {
    let mut metadata = Metadata::default();
    let Some(start) = source.find("/*---") else {
        return metadata;
    };
    let Some(length) = source[start..].find("---*/") else {
        return metadata;
    };
    let block = &source[start + 5..start + length];
    let mut section = "";
    for line in block.lines() {
        let trimmed = line.trim();
        if let Some(item) = trimmed.strip_prefix("- ") {
            if section == "includes" {
                metadata.includes.push(item.trim().to_string());
            } else if section == "flags" {
                metadata.flags.push(item.trim().to_string());
            }
            continue;
        }
        if !line.starts_with(' ') && !line.starts_with('\t') {
            section = "";
        }
        let Some((key, value)) = trimmed.split_once(':') else {
            continue;
        };
        let value = value.trim();
        let list = || {
            value
                .trim_start_matches('[')
                .trim_end_matches(']')
                .split(',')
                .map(|item| item.trim().to_string())
                .filter(|item| !item.is_empty())
                .collect::<Vec<_>>()
        };
        match key {
            "includes" if !line.starts_with(' ') => {
                section = "includes";
                metadata.includes.extend(list());
            }
            "flags" if !line.starts_with(' ') => {
                section = "flags";
                metadata.flags.extend(list());
            }
            "negative" if !line.starts_with(' ') => section = "negative",
            "type" if section == "negative" => metadata.negative_type = Some(value.to_string()),
            _ => {}
        }
    }
    metadata
}

fn run(path: &Path, harness: &Path) -> Outcome {
    // with CERI_TEST262_TRACE set, each test is named before it runs, to find one that
    // takes the process down
    if std::env::var_os("CERI_TEST262_TRACE").is_some() {
        eprintln!("test262 start {}", path.display());
    }
    let source = match fs::read_to_string(path) {
        Ok(source) => source,
        Err(error) => return Outcome::Fail(format!("unreadable: {error}")),
    };
    let metadata = metadata(&source);
    let flag = |name: &str| metadata.flags.iter().any(|flag| flag == name);
    if flag("module") {
        return Outcome::Skip;
    }
    let mut program = String::new();
    if flag("onlyStrict") {
        program.push_str("\"use strict\";\n");
    }
    if !flag("raw") {
        program.push_str(HOST_PRELUDE);
        let mut includes = vec!["assert.js".to_string(), "sta.js".to_string()];
        if flag("async") {
            includes.push("doneprintHandle.js".to_string());
        }
        includes.extend(metadata.includes.iter().cloned());
        for include in includes {
            match fs::read_to_string(harness.join(&include)) {
                Ok(text) => program.push_str(&text),
                Err(error) => return Outcome::Fail(format!("missing include {include}: {error}")),
            }
            program.push('\n');
        }
    }
    program.push_str(&source);

    let document = Document::parse("").expect("an empty document parses");
    let mut runtime = ScriptRuntime::new(Rc::new(RefCell::new(document)));
    runtime.set_step_limit(Some(STEP_LIMIT));
    let mut result = runtime.execute(&program);
    if result.is_ok() && flag("async") {
        for _ in 0..20 {
            if runtime
                .logs()
                .iter()
                .any(|log| log.starts_with("Test262:Async"))
            {
                break;
            }
            if let Err(error) = runtime.advance_time(100) {
                result = Err(error);
                break;
            }
        }
    }
    match (&metadata.negative_type, result) {
        (Some(expected), Err(error)) if &error.name == expected => Outcome::Pass,
        (Some(expected), Err(error)) => Outcome::Fail(format!(
            "expected {expected}, got {}: {}",
            error.name, error.message
        )),
        (Some(expected), Ok(())) => {
            Outcome::Fail(format!("expected {expected}, ran to completion"))
        }
        (None, Err(error)) => Outcome::Fail(format!("{}: {}", error.name, error.message)),
        (None, Ok(())) => {
            let logs = runtime.logs();
            // a rejection left unhandled fails no test, an exception left uncaught does
            let thrown = |log: &&String| {
                log.starts_with("Uncaught") && !log.starts_with("Uncaught (in promise)")
            };
            if let Some(uncaught) = logs.iter().find(thrown) {
                return Outcome::Fail(uncaught.clone());
            }
            if !flag("async") {
                return Outcome::Pass;
            }
            match logs.iter().find(|log| log.starts_with("Test262:Async")) {
                Some(log) if log == "Test262:AsyncTestComplete" => Outcome::Pass,
                Some(log) => Outcome::Fail(log.clone()),
                None => Outcome::Fail("the async test never called $DONE".to_string()),
            }
        }
    }
}

#[test]
#[ignore = "about a minute in release; run with --release -- --ignored"]
fn pinned_test262_subset_matches_its_expectations() {
    let root = root();
    let harness = root.join("harness");
    let mut tests = Vec::new();
    collect(&root.join("test"), &mut tests);
    // CERI_TEST262_FILTER keeps the tests whose path contains it, for focused runs
    if let Some(filter) = std::env::var_os("CERI_TEST262_FILTER") {
        let filter = filter.to_string_lossy().replace('\\', "/");
        tests.retain(|path| path.to_string_lossy().replace('\\', "/").contains(&filter));
    }
    let next = AtomicUsize::new(0);
    let failures = Mutex::new(Vec::new());
    let (passed, skipped) = (AtomicUsize::new(0), AtomicUsize::new(0));
    // CERI_TEST262_WORKERS caps the threads, since tests that build arrays near the length
    // limit take hundreds of megabytes each and a full machine can run out
    let workers = std::env::var("CERI_TEST262_WORKERS")
        .ok()
        .and_then(|count| count.parse::<usize>().ok())
        .filter(|count| *count > 0)
        .unwrap_or_else(|| std::thread::available_parallelism().map_or(4, |count| count.get()));
    std::thread::scope(|scope| {
        for _ in 0..workers {
            std::thread::Builder::new()
                .stack_size(64 << 20)
                .spawn_scoped(scope, || {
                    loop {
                        let index = next.fetch_add(1, Ordering::Relaxed);
                        let Some(path) = tests.get(index) else {
                            break;
                        };
                        let started = std::time::Instant::now();
                        let outcome =
                            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                run(path, &harness)
                            }))
                            .unwrap_or_else(|_| Outcome::Fail("the engine panicked".to_string()));
                        if std::env::var_os("CERI_TEST262_TRACE").is_some() {
                            eprintln!(
                                "test262 done {} {} ms",
                                path.display(),
                                started.elapsed().as_millis()
                            );
                        }
                        match outcome {
                            Outcome::Pass => {
                                passed.fetch_add(1, Ordering::Relaxed);
                            }
                            Outcome::Skip => {
                                skipped.fetch_add(1, Ordering::Relaxed);
                            }
                            Outcome::Fail(reason) => {
                                let name = path
                                    .strip_prefix(&root)
                                    .expect("tests live under the root")
                                    .to_string_lossy()
                                    .replace('\\', "/");
                                failures.lock().expect("failure list").push((name, reason));
                            }
                        }
                    }
                })
                .expect("a worker thread starts");
        }
    });
    let mut failures = failures.into_inner().expect("failure list");
    failures.sort();
    let passed = passed.into_inner();
    let skipped = skipped.into_inner();
    let total = tests.len();
    println!(
        "test262: {passed} passed, {} failed, {skipped} skipped of {total}",
        failures.len()
    );

    // a full failure list with reasons, for triage
    if let Some(report) = std::env::var_os("CERI_TEST262_REPORT") {
        let text = failures
            .iter()
            .map(|(name, reason)| {
                format!(
                    "{name}	{reason}
"
                )
            })
            .collect::<String>();
        fs::write(report, text).expect("the report is writable");
    }

    let expectations = root.join("expected-failures.txt");
    let failing = failures
        .iter()
        .map(|(name, _)| name.clone())
        .collect::<BTreeSet<_>>();
    if std::env::var_os("CERI_TEST262_UPDATE").is_some() {
        let mut text = String::new();
        for name in &failing {
            text.push_str(name);
            text.push('\n');
        }
        fs::write(&expectations, text).expect("expectations are writable");
        return;
    }
    let expected = fs::read_to_string(&expectations)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect::<BTreeSet<_>>();
    let regressions = failures
        .iter()
        .filter(|(name, _)| !expected.contains(name))
        .map(|(name, reason)| format!("{name}: {reason}"))
        .collect::<Vec<_>>();
    let fixed = expected.difference(&failing).count();
    assert!(
        regressions.is_empty(),
        "{} test262 tests regressed:\n{}",
        regressions.len(),
        regressions.join("\n")
    );
    assert_eq!(
        fixed, 0,
        "{fixed} tests listed as failing now pass; rerun with CERI_TEST262_UPDATE=1"
    );
}
