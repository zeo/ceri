# Ceri engine laboratory

This crate is a deliberately small browser-engine experiment written with the Rust standard library. Its default build has no CEF, WebKit, Servo, third-party JavaScript engine, or GUI dependency.

The current slice implements:

- HTML tokenization and a small DOM tree
- HTML entity decoding for common named and numeric entities
- CSS rules with compound tag, class, and id selectors, descendant matching, and direct-child matching
- a deterministic cascade with selector specificity
- document and subtree `querySelector` lookups using the same selector matcher
- block layout, row and column flex layout, inline text wrapping, margins, padding, borders, colors, and basic lengths
- layout boxes retain DOM paths for coordinate hit testing
- a software paint step with a small built-in bitmap font
- a platform-neutral `EngineHost` frame and input boundary
- a small JavaScript-like runtime with DOM mutation, console logging, and click events
- bubbling DOM events with `type`, `target`, and `currentTarget` values
- element attributes, inline styles, and `classList` state mutation
- stable detached and attached element handles with `document.createElement` and `appendChild`
- parsed fragment replacement and serialization through `innerHTML`
- a deterministic task clock with `setTimeout`, `clearTimeout`, `requestAnimationFrame`, and cancellation
- a strict `https://app.ceri.invalid` URL model with an in-memory packaged resource bundle
- same-origin relative URL resolution for external stylesheets and scripts
- packaged-origin promise-based `fetch()` responses with status, MIME type, URL, text bodies, and JSON values
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

The dynamic-DOM sample creates a detached element, fills it with an HTML fragment through `innerHTML`, appends it into the document, and paints the resulting tree:

```powershell
cargo run --manifest-path engine-lab/Cargo.toml -- --input engine-lab/dynamic-demo.html --asset-root engine-lab --url https://app.ceri.invalid/ --output target/ceri-dynamic-demo.bmp --width 800 --height 360
```

The nested-selector sample exercises compound selectors, descendant cascade, direct-child matching, and flex layout together:

```powershell
cargo run --manifest-path engine-lab/Cargo.toml -- --input engine-lab/selector-demo.html --asset-root engine-lab --url https://app.ceri.invalid/ --output target/ceri-selector-demo.bmp --width 800 --height 360
```

The event sample starts a click on a nested element and handles it on an ancestor through an external script:

```powershell
cargo run --manifest-path engine-lab/Cargo.toml -- --input engine-lab/event-demo.html --asset-root engine-lab --url https://app.ceri.invalid/ --output target/ceri-event-demo.bmp --width 800 --height 360 --click label
```

The same sample can dispatch a pointer location through layout hit testing instead of naming an element:

```powershell
cargo run --manifest-path engine-lab/Cargo.toml -- --input engine-lab/event-demo.html --asset-root engine-lab --url https://app.ceri.invalid/ --output target/ceri-event-demo-pointer.bmp --width 800 --height 360 --click-at 60,160
```

`EngineHost` is the native-window boundary. A platform adapter navigates it, submits `InputEvent` values, calls `render`, and uploads `Frame.canvas.rgba_pixels()` using `Frame.canvas.bytes_per_row()`. The lab CLI uses this boundary for navigation, coordinate input, timers, and presentation.

The default lab build stays dependency-free. The optional `windowed` feature adds a small native desktop adapter built with pinned `winit` 0.30.13 and `softbuffer` 0.4.8. It creates a platform window, forwards resize and pointer events, advances the deterministic page clock, and presents the custom engine's RGBA canvas through a CPU surface. It does not add CEF, WebKit, Edge WebView2, a loopback server, or a JavaScript engine.

Compile the adapter from the repository root:

```powershell
cargo check --manifest-path engine-lab/Cargo.toml --features windowed --bin ceri-engine-window
cargo run --manifest-path engine-lab/Cargo.toml --features windowed --bin ceri-engine-window
```

The adapter is an integration slice for Windows, macOS, and Linux desktop surfaces. It does not claim browser compatibility, mobile support, or a complete cross-platform release until those targets have their own build and runtime checks.

The fetch demo exercises an app-owned resource without opening a listener:

```powershell
cargo run --manifest-path engine-lab/Cargo.toml --bin ceri-engine-lab -- --input engine-lab/fetch-demo.html --asset-root engine-lab --url https://app.ceri.invalid/ --output target/ceri-fetch-demo.bmp
```

This first fetch slice is deterministic and limited to packaged same-origin resources. It returns a promise with `then()` and `catch()` and a response with `status`, `ok`, `url`, `mimeType`, promise-returning `text()`, and promise-returning `json()`. The runtime supports basic JSON objects, arrays, property access, numeric indexes, `JSON.parse`, and deterministic `JSON.stringify`. Microtasks drain after each script turn and clock advance. Missing resources resolve with status 404, while rejected origins reject the promise. Streaming bodies, network transports, request methods, and broader JavaScript object semantics remain separate compatibility gates.
