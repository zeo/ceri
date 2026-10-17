# Ceri

Ceri is a Rust-owned desktop runtime for web applications. It ships its own browser engine inside the app, so rendering behaviour is chosen and updated with the release instead of arriving from an operating-system webview or a bundled Chromium.

The engine, `ceri-engine` in [engine/](engine/README.md), is written against the Rust standard library, with no Chromium, WebKit, Servo, Edge WebView2 or third-party JavaScript engine underneath it. The app loads from an owned HTTPS origin, `https://app.ceri.invalid`, with no loopback HTTP server, no `file://` navigation and no Node.js requirement. Rust owns process startup, window creation, navigation policy, asset routing and recovery.

## Size and speed

Release build on Windows x64, running the packaged continuity app at 1120x720:

- the bundle is 5.7 MB in 9 files and runs as one process, with a 28 MB working set while the app is open
- process start to a written PNG of the first frame takes about 430 ms, font discovery included
- an animating frame takes 2.7 ms at the median of 300

The CEF 150 build this repository carried before it moved to its own engine measured 411 MB in 252 files, ran six processes, and held 319.7 MB resident for the same app.

The engine renders at the display device pixel ratio, resolves `font-family` stacks against installed system faces, reaches `4 / 4` on the packaged continuity page, and applies every rule in that page's stylesheet with nothing dropped.

## Build

Install Rust and the native toolchain for the target platform. Nothing else is needed:

```powershell
cargo test --all-targets
cargo test --release -p ceri-engine --test test262 -- --ignored
cargo run --bin ceri_bundle -- --release --output dist\windows-x64
```

The generated bundle holds `ceri.exe` and an `assets` directory. Launch the executable; it resolves its asset root next to itself.

Render one frame of the packaged app without opening a window, which is what CI checks on every push:

```powershell
cargo run --bin ceri -- "--ceri-assets=." "--ceri-entry=web/index.html" "--ceri-screenshot=target\app.png"
```

## Authoring

The web surface is ordinary HTML, CSS and JavaScript. TypeScript projects can consume the declarations in `sdk/index.d.ts` and emit browser-compatible modules into the packaged asset directory.

```js
import { runtimeInfo } from "./sdk/index.js";

const info = await runtimeInfo();
console.log(info.engine, info.recovery);
```

`runtimeInfo()` reads the promise-based `window.ceriQuery` bridge and validates the response shape before returning it.

### Commands

An app registers named commands in Rust and grants pages the right to call them. Arguments and results cross the bridge as JSON text:

```rust
use ceri::{App, Capabilities, CommandError, Grant};

fn main() -> Result<(), String> {
    App::new()
        .command("greet", |call| Ok(format!("{{\"hello\":{}}}", call.payload)))
        .blocking_command("export", |call| {
            std::fs::write("export.json", &call.payload)
                .map(|()| "true".to_string())
                .map_err(|error| CommandError::failed(error.to_string()))
        })
        .capabilities(Capabilities::new(vec![Grant {
            windows: vec!["main".into()],
            origins: vec![ceri::APP_ORIGIN.into()],
            commands: vec!["greet".into(), "export".into()],
        }]))
        .run()
}
```

```js
import { invoke, listen, emit } from "./sdk/index.js";

const reply = await invoke("greet", { name: "ceri" });
const unlisten = await listen("progress", (event) => console.log(event.payload));
await emit("ready", { at: Date.now() });
```

`command` handlers answer on the engine thread and should return quickly. `blocking_command` handlers each run on a thread of their own, and their promises settle on the next engine frame. Capabilities deny by default: a grant names windows, origins and commands, where each pattern is `*`, an exact name or a prefix ending in `*`. A call no grant allows is rejected in the engine before it reaches the app, as a `CeriBridgeError` with code `denied`. An unregistered command rejects with `unknown-command`, a payload that is not JSON serializable with `invalid-payload`, and a handler's own `CommandError` arrives with its code and message. `Browser::emit_event` delivers host events to `listen` callbacks as `{ event, payload }`, and `emit` passes page events to the router's `event` hook.

## Failure behavior

Ceri verifies the entry document before the engine starts. Every packaged app document is served from the owned HTTPS origin, through the asset-origin policy behind a `ResourceProvider`. Missing assets, unsafe paths, navigation errors and repeated renderer termination produce a Ceri recovery document with a retry link. The browser's generic network failure page is not part of the application path.

The resolver rejects encoded separators, traversal, malformed UTF-8, Windows device names, invalid path characters, symlink escapes and ambiguous empty path segments. Top-level extensionless routes may use the packaged `index.html`; missing subresources never fall back to the app shell.

Page stylesheets follow CSS error handling. An unsupported rule is dropped and reported to the page console, and the rest of the sheet still applies, so one unknown at-rule cannot blank an app.

See [docs/architecture.md](docs/architecture.md) for the process model and security boundaries.

## The engine

[engine/](engine/README.md) holds the engine: HTML parsing and a live DOM, a CSS cascade with the selector, length and media-query surface listed there, block, flex and basic grid layout, positioning, scrolling and clipping, a software paint pass with an sfnt parser and antialiased glyph rasterizer, its own ES2022 JavaScript engine (parser, bytecode compiler and VM with generators, async functions and an object model with prototypes and descriptors) with modules, promises, workers, fetch, storage, IndexedDB, structured clone and Web Crypto digests, an accessibility tree, and a platform-neutral frame and input boundary that the shared `winit` and `softbuffer` window host drives.

Host-owned seams keep platform work outside the renderer: `ResourceProvider` for assets and networking, `FilePicker` for file input, `ClipboardProvider` for clipboard text, `FontSource` for installed faces, and typed `HostRuntimeInfo` for the runtime identity query. The renderer never receives a filesystem path, a native clipboard handle or a socket. On Windows the runtime backs the clipboard and file input with the platform clipboard and file dialog.

A page can fetch outside its own origin only through origins the operator lists, and only over HTTPS. Windows uses the platform HTTP stack, so TLS trust and updates stay with the operating system and the bundle carries no TLS library:

```powershell
ceri.exe --ceri-allow-origin=https://api.example.com
```

Every other origin is refused with a typed rejection the page can catch, and navigation stays on the app origin regardless.

The visual surface covers antialiased outline text with system font fallback, PNG, JPEG and GIF images decoded by the engine itself with natural sizing, `object-fit` and background images, rounded corners, gradients, box shadows, group opacity composited through offscreen layers, 2D and 3D transforms with perspective, generated content through `::before` and `::after`, CSS animations that start when they are applied, and transitions on colors, lengths, shadows, gradients, transforms and every other interpolable property the engine styles, including `transition: all`, timed with real `cubic-bezier()` curves and every `steps()` position, and reported to the page through `transition*` and `animation*` events.

Text fields behave like native fields: a pointer press places the caret, a drag extends the selection, a double click takes the word and a triple click the whole value, Ctrl with an arrow or Backspace moves or deletes by word (Alt on macOS), and Ctrl or Cmd with A, C, X or V selects, copies, cuts and pastes through the host clipboard.

Page text selects the same way outside form fields. A drag can cross paragraphs and inline elements, a double click takes a word, a triple click takes the block, and a drag after a double click grows by whole words. Shift with an arrow, Home or End extends the selection by character, word or line, and Ctrl or Cmd with A selects every rendered line. Copy writes the text as it is drawn, with a line break between blocks, so `<script>` and `<style>` contents never reach the clipboard.

Scrollbars behave like native scrollbars: the wheel scrolls from anywhere over the gutter, and pressing the thumb drags it with the pointer while it stays grabbed at its press offset.

The window takes its caption from `document.title`, its icon from the packaged favicon, and remembers its position and size between launches under the per-user profile. If the engine thread stops, the window restarts it once and then shows the engine's own recovery document.

Not covered yet: media playback, WebGL and other GPU APIs, depth-sorted `preserve-3d` scenes, full WebAssembly, printing, platform accessibility adapters, multi-process renderer isolation with a sandbox, and any browser-compatibility suite result. Those are the gates in [docs/engine-research.md](docs/engine-research.md), which also carries the candidate comparison and the replacement criteria.
