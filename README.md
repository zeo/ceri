# Ceri

Ceri is a Rust-owned desktop runtime for web applications. It ships its own browser engine inside the app, so rendering behaviour is chosen and updated with the release instead of arriving from an operating-system webview or a bundled Chromium.

The engine, `ceri-engine` in [engine/](engine/README.md), is written against the Rust standard library, with no Chromium, WebKit, Servo, Edge WebView2 or third-party JavaScript engine underneath it. The app loads from an owned HTTPS origin, `https://app.ceri.invalid`, with no loopback HTTP server, no `file://` navigation and no Node.js requirement. Rust owns process startup, window creation, navigation policy, asset routing and recovery.

## Size and speed

Release build on Windows x64, running the packaged continuity app at 1120x720:

- the bundle is 5.7 MB in 9 files and runs as one process, with a 28 MB working set while the app is open
- process start to a written PNG of the first frame takes about 430 ms, font discovery included
- an animating frame takes 2.7 ms at the median of 300

The CEF 150 build this repository carried before it moved to its own engine measured 411 MB in 252 files, ran six processes, and held 319.7 MB resident for the same app.

Against Tauri on WebView2, with the same app (the clip video recorder, its Tauri release against its ceri port, 2026-11-20, Windows x64 at 1.5x):

| | Tauri | ceri |
| --- | --- | --- |
| Window asked for to a drawn shell | 787 ms | 565 ms |
| CPU over 20 s idle on the home screen | 16 ms | 0 ms |
| Processes, private bytes, working set | 7, 296 MB, 376 MB | 1, 216 MB, 179 MB |
| CPU to play a 6 s clip in the editor | 4.69 s | 4.08 s |

In the same run its eight screens matched the WebView2 build at 1x, 1.5x and 2x, where matching means no more than 0.5% of pixels differ by a CIEDE2000 distance over 2.0.

The engine renders at the display device pixel ratio, resolves `font-family` stacks against installed system faces, reaches `4 / 4` on the packaged continuity page, and applies every rule in that page's stylesheet with nothing dropped.

## Build

Install Rust and the native toolchain for the target platform. Nothing else is needed:

```powershell
cargo test --all-targets
cargo test --release -p ceri-engine --test test262 -- --ignored
cargo run --release --bin ceri_bundle -- --output dist\windows-x64
```

The bundler builds whichever app it is pointed at with `--project`, the current directory by default, and reads that app's `ceri.json`:

```json
{
  "identifier": "Ceri",
  "name": "Ceri",
  "binary": "ceri",
  "publisher": "Ceri contributors",
  "icons": ["icons/app.ico", "icons/128x128.png", "web/favicon.svg"],
  "resources": { "web": "assets/web", "sdk": "assets/sdk" },
  "signCommand": ["signtool", "sign", "/fd", "SHA256", "/a", "{file}"]
}
```

Only `identifier` is required. `name` defaults to the Cargo package name, `binary` to its only binary, and `publisher` to its first author. `resources` copies files or directories into the app folder, which is where the runtime looks for its `assets`. `signCommand` runs on each executable the bundle ships, with `{file}` replaced by its path.

The output folder holds `app/`, the executable with its resources, which runs where it is. On Windows it also holds `<name>_<version>_x64-setup.exe`, a per-user installer that needs no administrator rights: it installs to `%LOCALAPPDATA%\Programs\<name>`, adds a Start menu entry and an entry under Installed apps, and takes `/S` to install silently. The uninstaller removes what the runtime registered under the identifier as well: notifications, start at login and URL schemes. On Linux the folder holds `<package>_<version>_amd64.deb`, which installs the app under `/usr/lib/<package>` with a desktop entry and the PNG and SVG icons.

Render one frame of the packaged app without opening a window, which is what CI checks on every push:

```powershell
cargo run --bin ceri -- "--ceri-assets=." "--ceri-entry=web/index.html" "--ceri-screenshot=target\app.png"
```

Add `--ceri-scale=1.5` to render at a device pixel ratio other than 1.

## Authoring

The web surface is ordinary HTML, CSS and JavaScript. TypeScript projects can consume the declarations in `sdk/index.d.ts` and emit browser-compatible modules into the packaged asset directory.

```js
import { runtimeInfo } from "./sdk/index.js";

const info = await runtimeInfo();
console.log(info.engine, info.recovery);
```

`runtimeInfo()` reads the promise-based `window.ceriQuery` bridge and validates the response shape before returning it.

An app can compile its built pages into its executable instead of shipping an `assets` folder. Its build script names the folder, with `ceri-build` as a build dependency:

```rust
fn main() {
    ceri_build::embed("web/dist");
}
```

and the app hands the files over with `App::new().assets(ceri::embedded_assets!())`. Cargo rebuilds when anything under the folder changes. A `--ceri-assets` folder on the command line still wins over the compiled-in files.

The same build script can give a Windows executable its file icon, version details and a Common Controls v6 manifest, written as a resource file the MSVC linker takes directly, so no `rc.exe` is needed:

```rust
ceri_build::resources(ceri_build::Resources {
    icon: Some(std::path::Path::new("icons/icon.ico")),
    company: Some("rot"),
    ..Default::default()
});
```

The product name defaults to the package name, the file description to the product name, and both versions come from the package version. On other targets, and on the GNU toolchain, the call does nothing.

### Commands

An app registers named commands in Rust and grants pages the right to call them. A command takes its arguments and returns its result as serde types; `json_command` and `blocking_json_command` work on the raw call and its JSON text instead:

```rust
use ceri::{App, Capabilities, CommandError, Grant};

#[derive(serde::Deserialize)]
struct Greeting {
    name: String,
}

fn main() -> Result<(), String> {
    App::new()
        .command("greet", |greeting: Greeting| Ok(format!("hello {}", greeting.name)))
        .blocking_json_command("export", |call| {
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

A payload that does not fit the argument type rejects with `invalid-payload`. `command` handlers answer on the engine thread and should return quickly. `blocking_command` handlers each run on a thread of their own, and their promises settle on the next engine frame. Capabilities deny by default: a grant names windows, origins and commands, where each pattern is `*`, an exact name or a prefix ending in `*`. The desktop's own commands are namespaced, such as `dialog:open` or `notification:show`, and only a pattern with a colon reaches them, so `*` grants every app command and none of these. Pages may always drive their window (`window:*`) and read the app version (`app:*`); dialogs, the opener, notifications, autostart, global shortcuts and deep links each need a grant. A call no grant allows is rejected in the engine before it reaches the app, as a `CeriBridgeError` with code `denied`. An unregistered command rejects with `unknown-command`, a payload that is not JSON serializable with `invalid-payload`, and a handler's own `CommandError` arrives with its code and message. An ArrayBuffer or typed array payload skips JSON: its bytes reach the handler as `call.body`, with `call.payload` set to `null`. A third argument, `invoke(name, bytes, { headers })`, fills `call.headers`, which `call.header(name)` reads without regard to case. `Browser::emit_event` delivers host events to `listen` callbacks as `{ event, payload }`, and `emit` passes page events to the router's `event` hook.

`ceri::command!` builds a `json_command` handler over named arguments, the way a Tauri command takes them. Each argument is read from the page's object under its camelCase name, and an `Option` argument may be left out. The body answers with any serializable value, or with a `Result` whose error is a `CommandError` or a message, which rejects with code `failed`. Naming `call` first also hands the body the call:

```rust
App::new()
    .json_command("star", ceri::command!(|paths: Vec<PathBuf>, on: bool| star(&paths, on)))
    .json_command("owner", ceri::command!(|call| owner(&call.window)))
```

## Failure behavior

Ceri verifies the entry document before the engine starts. Every packaged app document is served from the owned HTTPS origin, through the asset-origin policy behind a `ResourceProvider`. Missing assets, unsafe paths, navigation errors and repeated renderer termination produce a Ceri recovery document with a retry link. The browser's generic network failure page is not part of the application path.

The resolver rejects encoded separators, traversal, malformed UTF-8, Windows device names, invalid path characters, symlink escapes and ambiguous empty path segments. Top-level extensionless routes may use the packaged `index.html`; missing subresources never fall back to the app shell.

Page stylesheets follow CSS error handling. An unsupported rule is dropped and reported to the page console, and the rest of the sheet still applies, so one unknown at-rule cannot blank an app.

See [docs/architecture.md](docs/architecture.md) for the process model and security boundaries.

## The engine

[engine/](engine/README.md) holds the engine: HTML parsing and a live DOM, a CSS cascade with the selector, length and media-query surface listed there, block, flex, grid and table layout, positioning, scrolling and clipping, a software paint pass with an sfnt parser and antialiased glyph rasterizer, SVG, a 2D canvas, WebGL 1 and 2 drawn on the CPU with a GLSL ES 1.00 and 3.00 compiler of its own, audio and video through the system's decoders on Windows, its own ES2022 JavaScript engine (parser, bytecode compiler and VM with generators, async functions and an object model with prototypes and descriptors) with modules, promises, workers, fetch, storage, IndexedDB, structured clone and Web Crypto digests, an accessibility tree, and a platform-neutral frame and input boundary that the shared `winit` and `softbuffer` window host drives.

Host-owned seams keep platform work outside the renderer: `ResourceProvider` for assets and networking, `FilePicker` for file input, `ClipboardProvider` for clipboard text and pasted pictures, `FontSource` for installed faces, and typed `HostRuntimeInfo` for the runtime identity query. The renderer never receives a filesystem path, a native clipboard handle or a socket. On Windows the runtime backs the clipboard and file input with the platform clipboard and file dialog.

A page can fetch outside its own origin only through origins the operator lists, and only over HTTPS. Windows uses the platform HTTP stack, so TLS trust and updates stay with the operating system and the bundle carries no TLS library:

```powershell
ceri.exe --ceri-allow-origin=https://api.example.com
```

Every other origin is refused with a typed rejection the page can catch, and navigation stays on the app origin regardless.

The visual surface covers antialiased outline text with system font fallback, PNG, JPEG and GIF images decoded by the engine itself with natural sizing, `object-fit` and background images, rounded corners, gradients, box shadows, group opacity composited through offscreen layers, 2D and 3D transforms with perspective, generated content through `::before` and `::after`, CSS animations that start when they are applied, and transitions on colors, lengths, shadows, gradients, transforms and every other interpolable property the engine styles, including `transition: all`, timed with real `cubic-bezier()` curves and every `steps()` position, and reported to the page through `transition*` and `animation*` events.

Text fields behave like native fields: a pointer press places the caret, a drag extends the selection, a double click takes the word and a triple click the whole value, Ctrl with an arrow or Backspace moves or deletes by word (Alt on macOS), and Ctrl or Cmd with A, C, X or V selects, copies, cuts and pastes through the host clipboard. The page hears `copy`, `cut` and `paste` first, with a `clipboardData` it can read or fill, and a pasted screenshot arrives there as an image file.

Page text selects the same way outside form fields. A drag can cross paragraphs and inline elements, a double click takes a word, a triple click takes the block, and a drag after a double click grows by whole words. Shift with an arrow, Home or End extends the selection by character, word or line, and Ctrl or Cmd with A selects every rendered line. Copy writes the text as it is drawn, with a line break between blocks, so `<script>` and `<style>` contents never reach the clipboard.

A primary press that moves 4 CSS pixels (3 on macOS) on an element whose `draggable` is true, which by default an image or a link is, fires a cancelable `dragstart` `DragEvent` at that element with the press position, as Blink does. Its `dataTransfer` is still `null`, and nothing is dragged when the page leaves it uncanceled. An app that runs the drag itself, such as an OLE drag of files on Windows, calls it from `WindowControl::run_on_loop`, which runs a task on the thread that owns the native windows.

Scrollbars behave like native scrollbars: the wheel scrolls from anywhere over the gutter, and pressing the thumb drags it with the pointer while it stays grabbed at its press offset.

Beyond the page, an app gets the desktop parts most Tauri apps lean on: windows that can be frameless, transparent, shadowless, click-through, kept out of screen capture or opened without taking focus, and able to remember where they were; a tray icon with check items and submenus; the system's file, folder and message dialogs, with buttons named as the app likes; notifications, start at login, global shortcuts, deep links and a single instance; files dropped on a window; and the Windows high contrast palette, which pages see through `forced-colors`.

The window takes its caption from `document.title`, its icon from the packaged favicon (or the PNG or SVG an app passes to `App::icon`), and remembers its position and size between launches under the per-user profile. If the engine thread stops, the window restarts it once and then shows the engine's own recovery document.

JavaScript conformance is checked against a test262 subset in CI, with the tests the engine does not yet pass listed in `conformance/test262/expected-failures.txt`.

Not covered yet: WebGPU (`navigator.gpu` finds no adapter) and WebCodecs, multisampled WebGL (a context asked for `antialias` draws without it and reports `false`), video on Linux, depth-sorted `preserve-3d` scenes, full WebAssembly, printing, platform accessibility adapters, and multi-process renderer isolation with a sandbox. Those are the gates in [docs/engine-research.md](docs/engine-research.md), which also carries the candidate comparison and the replacement criteria.

## License

Licensed under either of the [Apache License, Version 2.0](LICENSE-APACHE) or the [MIT license](LICENSE-MIT), at your option.
