# Ceri engine laboratory

This crate is a deliberately small browser-engine experiment written with the Rust standard library. It has no CEF, WebKit, Servo, third-party JavaScript engine, or GUI dependency.

The current slice implements:

- HTML tokenization and a small DOM tree
- HTML entity decoding for common named and numeric entities
- CSS rules with tag, class, and id selectors
- a deterministic cascade with selector specificity
- block layout, row and column flex layout, inline text wrapping, margins, padding, borders, colors, and basic lengths
- a software paint step with a small built-in bitmap font
- a small JavaScript-like runtime with DOM mutation, console logging, and click events
- stable detached and attached element handles with `document.createElement` and `appendChild`
- a deterministic task clock with `setTimeout`, `clearTimeout`, `requestAnimationFrame`, and cancellation
- a strict `https://app.ceri.invalid` URL model with an in-memory packaged resource bundle
- same-origin relative URL resolution for external stylesheets and scripts
- browser navigation, extensionless app-shell fallback, history, and app-owned recovery pages
- PPM or BMP output and a textual layout dump

It is a headless laboratory, not a web-compatible browser. The script runtime covers the demo's DOM and event path, while networking, media, accessibility, GPU APIs, WebAssembly, and the operating-system window remain outside this slice.

Run the deterministic demo from the repository root:

```powershell
cargo run --manifest-path engine-lab/Cargo.toml -- --input engine-lab/demo.html --asset-root engine-lab --url https://app.ceri.invalid/ --output target/ceri-engine-lab.bmp --width 800 --height 520 --click run-check
```

The command loads the asset root into the engine's packaged resource bundle, navigates the app origin without opening a listener, resolves the demo's external CSS and JavaScript, and can dispatch a deterministic click with `--click ID`. A missing extensionless route falls back to the packaged shell, while missing top-level or required subresources receive an engine-owned recovery document. It prints the navigation result, layout tree, event result, console output, and number of non-background pixels. The output can be a plain PPM or BMP image, so the paint path does not depend on an image library.

The flex sample exercises row growth, gaps, column composition, and alignment:

```powershell
cargo run --manifest-path engine-lab/Cargo.toml -- --input engine-lab/flex-demo.html --asset-root engine-lab --url https://app.ceri.invalid/ --output target/ceri-flex-demo.bmp --width 800 --height 420
```

The task-clock sample advances the page without sleeping or relying on wall time:

```powershell
cargo run --manifest-path engine-lab/Cargo.toml -- --input engine-lab/timer-demo.html --asset-root engine-lab --url https://app.ceri.invalid/ --output target/ceri-timer-demo.bmp --width 800 --height 300 --tick 16 --tick 84
```

The first tick runs the animation frame at 16ms. The second reaches 100ms and runs the timeout. Repeating the command produces the same callbacks and pixels.

The dynamic-DOM sample creates a detached element, mutates it, appends it into the document, and paints the resulting tree:

```powershell
cargo run --manifest-path engine-lab/Cargo.toml -- --input engine-lab/dynamic-demo.html --asset-root engine-lab --url https://app.ceri.invalid/ --output target/ceri-dynamic-demo.bmp --width 800 --height 360
```
