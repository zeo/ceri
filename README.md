# Ceri

Ceri is a Rust-owned desktop runtime for web applications. The engine ships inside the app, so the rendering behaviour is chosen and updated with the release instead of arriving from an operating-system webview.

Two engines sit behind one runtime contract:

- `ceri-engine`, a Rust-native browser engine in [engine/](engine/README.md), written against the standard library with no Chromium, WebKit, Servo or third-party JavaScript engine underneath it
- CEF 150.0.14 with Chromium 150, the compatibility engine for apps that need the full web platform today

Both engines load the same packaged app from the same owned HTTPS origin, `https://app.ceri.invalid`, with no loopback HTTP server, no `file://` navigation, no Edge WebView2 and no Node.js requirement. Rust owns process startup, window creation, navigation policy, asset routing and recovery in both modes.

## Measured difference

Same app, same machine, release builds, Windows x64. The runtime bundle is the executable plus the packaged `web/` and `sdk/` assets.

| | `ceri-engine` | CEF |
| --- | --- | --- |
| bundle size | 4.0 MB | 411 MB |
| files in bundle | 9 | 252 |
| processes at rest | 1 | 6 |
| resident memory, app open | 26.9 MB | 319.7 MB |
| start to first painted frame | 160 ms median, headless render of the packaged app | not instrumented |
| frame time, app animating | 3.3 ms at 1120x720, 5.7 ms at twice the device pixel ratio | not instrumented |

The Rust-native engine renders at the display device pixel ratio, resolves `font-family` stacks against installed system faces, reaches `4 / 4` on the packaged continuity page, and applies every rule in that page's stylesheet with nothing dropped.

## Build

Install Rust and the native toolchain for the target platform.

The Rust-native engine needs nothing else:

```powershell
cargo test --no-default-features --features custom-engine --all-targets
cargo run --no-default-features --features custom-engine --bin ceri_bundle -- --release --output dist\windows-x64-custom
```

The generated bundle holds `ceri.exe` and an `assets` directory. Launch the executable; it resolves its asset root next to itself.

Render one frame of the packaged app without opening a window, which is what CI checks on every push:

```powershell
cargo run --no-default-features --features custom-engine --bin ceri -- "--ceri-assets=." "--ceri-entry=web/index.html" "--ceri-screenshot=target\app.png"
```

The CEF build additionally needs CMake, Ninja and the matching CEF distribution for the target. The CEF crate can download its archive during the first build, and a shared extracted directory is faster:

```powershell
$env:CEF_PATH = (Resolve-Path -LiteralPath '.\target\cef-cache\cef_windows_x86_64').Path
$env:Path = 'C:\Program Files (x86)\Microsoft Visual Studio\18\BuildTools\Common7\IDE\CommonExtensions\Microsoft\CMake\Ninja;' + $env:Path
cargo test --all-targets
cargo run --bin ceri_bundle -- --release --output dist\windows-x64-cef
```

The default build is the CEF engine with the Windows sandbox enabled, because CEF still covers more of the web platform than the Rust-native engine does. Launch the sandboxed executable from its generated bundle: CEF starts the browser process through the bundled bootstrap DLL, so copying the executable alone does not work. For development without the sandbox, add `--no-default-features --features cef-engine`.

Each CEF release must be built with the CEF archive for its target. The Rust-native engine has no per-target archive.

## Authoring

The web surface is ordinary HTML, CSS and JavaScript. TypeScript projects can consume the declarations in `sdk/index.d.ts` and emit browser-compatible modules into the packaged asset directory.

```js
import { runtimeInfo } from "./sdk/index.js";

const info = await runtimeInfo();
console.log(info.engine, info.chromium, info.recovery);
```

`runtimeInfo()` validates the response shape before returning it, and reads the promise-based `window.ceriQuery` bridge on the Rust-native engine or `window.cefQuery` on the CEF engine. Requests are limited to the runtime method Ceri implements. A new host capability should arrive as an explicit typed method with an explicit capability check, not as a general command evaluator.

## Failure behavior

Ceri verifies the entry document before the engine starts. Every packaged app document is served from the owned HTTPS origin: through the CEF scheme handler in the CEF build, and through the same asset-origin policy behind a `ResourceProvider` in the Rust-native build. Missing assets, unsafe paths, navigation errors and repeated renderer termination produce a Ceri recovery document with a retry link. The browser's generic network failure page is not part of the application path.

The resolver rejects encoded separators, traversal, malformed UTF-8, Windows device names, invalid path characters, symlink escapes and ambiguous empty path segments. Top-level extensionless routes may use the packaged `index.html`; missing subresources never fall back to the app shell.

Page stylesheets follow CSS error handling. An unsupported rule is dropped and reported to the page console, and the rest of the sheet still applies, so one unknown at-rule cannot blank an app.

See [docs/architecture.md](docs/architecture.md) for the process model and security boundaries.

## The Rust-native engine

[engine/](engine/README.md) holds the engine: HTML parsing and a live DOM, a CSS cascade with the selector, length and media-query surface listed there, block, flex and basic grid layout, positioning, scrolling and clipping, a software paint pass with an sfnt parser and antialiased glyph rasterizer, a bounded JavaScript runtime with modules, promises, workers, fetch, storage, IndexedDB, structured clone and Web Crypto digests, an accessibility tree, and a platform-neutral frame and input boundary that the shared `winit` and `softbuffer` window host drives.

Host-owned seams keep platform work outside the renderer: `ResourceProvider` for assets and networking, `FilePicker` for file input, `ClipboardProvider` for clipboard text, `FontSource` for installed faces, and typed `HostRuntimeInfo` for the runtime identity query. The renderer never receives a filesystem path, a native clipboard handle or a socket. On Windows the runtime backs the clipboard and file input with the platform clipboard and file dialog.

A page can fetch outside its own origin only through origins the operator lists, and only over HTTPS. Windows uses the platform HTTP stack, so TLS trust and updates stay with the operating system and the bundle carries no TLS library:

```powershell
ceri.exe --ceri-allow-origin=https://api.example.com
```

Every other origin is refused with a typed rejection the page can catch, and navigation stays on the app origin regardless.

The visual surface covers antialiased outline text with system font fallback, PNG, JPEG and GIF images decoded by the engine itself with natural sizing, `object-fit` and background images, rounded corners, gradients, box shadows, group opacity composited through offscreen layers, generated content through `::before` and `::after`, CSS animations driven from the page clock with linear, ease and stepped timing, and transitions on colors, opacity, lengths and radii that start when a state change alters a declared value.

Text fields behave like native fields: a pointer press places the caret, a drag extends the selection, a double click takes the word and a triple click the whole value, Ctrl with an arrow or Backspace moves or deletes by word (Alt on macOS), and Ctrl or Cmd with A, C, X or V selects, copies, cuts and pastes through the host clipboard.

Page text selects the same way outside form fields. A drag can cross paragraphs and inline elements, a double click takes a word, a triple click takes the block, and a drag after a double click grows by whole words. Shift with an arrow, Home or End extends the selection by character, word or line, and Ctrl or Cmd with A selects every rendered line. Copy writes the text as it is drawn, with a line break between blocks, so `<script>` and `<style>` contents never reach the clipboard.

Scrollbars behave like native scrollbars: the wheel scrolls from anywhere over the gutter, and pressing the thumb drags it with the pointer while it stays grabbed at its press offset.

The window takes its caption from `document.title`, its icon from the packaged favicon, and remembers its position and size between launches under the per-user profile. If the engine thread stops, the window restarts it once and then shows the engine's own recovery document, which is what the CEF path does with a dead renderer.

What CEF still covers and this engine does not: media playback, WebGL and other GPU APIs, CSS transforms beyond translate, full WebAssembly, extensions, printing, platform accessibility adapters, multi-process renderer isolation, and any browser-compatibility suite result. Those are the gates in [docs/engine-research.md](docs/engine-research.md), which also carries the candidate comparison and the replacement criteria.
