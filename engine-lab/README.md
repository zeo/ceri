# Ceri engine laboratory

This crate is a deliberately small browser-engine experiment written with the Rust standard library. It has no CEF, WebKit, Servo, third-party JavaScript engine, or GUI dependency.

The current slice implements:

- HTML tokenization and a small DOM tree
- HTML entity decoding for common named and numeric entities
- CSS rules with tag, class, and id selectors
- a deterministic cascade with selector specificity
- block layout, inline text wrapping, margins, padding, borders, colors, and basic lengths
- a software paint step with a small built-in bitmap font
- a small JavaScript-like runtime with DOM mutation, console logging, and click events
- PPM or BMP output and a textual layout dump

It is a headless laboratory, not a web-compatible browser. The script runtime covers the demo's DOM and event path, while networking, media, accessibility, GPU APIs, WebAssembly, and the operating-system window remain outside this slice.

Run the deterministic demo from the repository root:

```powershell
cargo run --manifest-path engine-lab/Cargo.toml -- --input engine-lab/demo.html --output target/ceri-engine-lab.bmp --width 800 --height 520 --click run-check
```

The command executes inline scripts before layout and can dispatch a deterministic click with `--click ID`. It prints the layout tree, event result, console output, and number of non-background pixels. The output can be a plain PPM or BMP image, so the paint path does not depend on an image library.
