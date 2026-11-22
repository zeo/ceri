# ceri

ceri runs a web app as a desktop app with its own engine inside. There is no Chromium, no WebView2, no WebKit and no Node.js underneath: the engine in [engine/](engine/README.md) is written in Rust against the standard library, JavaScript engine included, and ships with the app.

It is meant to replace Tauri and Electron for apps that want one process, a small download and the same pixels on every machine.

## Numbers

Windows x64, clip (a game clip recorder) as its Tauri release against its ceri port, 2026-11-20, at 1.5x:

| | Tauri on WebView2 | ceri |
| --- | --- | --- |
| Processes | 7 | 1 |
| Working set | 376 MB | 179 MB |
| Private bytes | 296 MB | 216 MB |
| Window asked for to a drawn shell | 787 ms | 565 ms |
| CPU over 20 s idle | 16 ms | 0 ms |
| CPU to play a 6 s clip | 4.69 s | 4.08 s |

All eight of clip's screens match the WebView2 build at 1x, 1.5x and 2x: no more than 0.5% of pixels differ by a CIEDE2000 distance over 2.0. The release `ceri.exe`, engine included, is 17 MB and needs only the Visual C++ runtime.

## Build

Install Rust and the native toolchain for your platform, then:

```powershell
cargo test --all-targets
cargo run --release --bin ceri_bundle -- --output dist\windows-x64
```

The bundler reads the app's `ceri.json` and writes the app folder plus a per-user Windows installer, or a `.deb` on Linux. [docs/guide.md](docs/guide.md) covers packaging, embedding assets and icons.

## Writing an app

Pages are plain HTML, CSS and JavaScript. Rust registers commands and grants pages the right to call them; the SDK in `sdk/` calls them the way Tauri's `invoke` does:

```rust
#[derive(serde::Deserialize)]
struct Greeting {
    name: String,
}

ceri::App::new()
    .command("greet", |greeting: Greeting| Ok(format!("hello {}", greeting.name)))
    .run()
```

```js
import { invoke, listen, appWindow } from "./sdk/index.js";

const reply = await invoke("greet", { name: "ceri" });
await appWindow.onCloseRequested((event) => {
  if (hasUnsavedWork()) event.preventDefault();
});
```

Apps also get frameless and transparent windows, a tray, native dialogs, notifications, global shortcuts, deep links, start at login and a single instance. Capabilities deny by default. The [guide](docs/guide.md) has the full command, window and capability reference, and [docs/architecture.md](docs/architecture.md) the process model and security boundaries.

## The engine

HTML and a live DOM; CSS with flex, grid, tables, transforms, animations and transitions; canvas 2D and WebGL 1 and 2; SVG; PNG, JPEG, GIF and WebP, animated ones included; audio and video through the system decoders on Windows; and an ES2022 JavaScript engine with modules, workers, fetch, IndexedDB and Web Crypto digests. [engine/README.md](engine/README.md) lists what is covered.

Not covered yet: WebGPU, WebCodecs, multisampled WebGL, video on Linux, depth-sorted `preserve-3d`, full WebAssembly, printing, platform accessibility adapters and a sandboxed renderer process.

## License

Apache 2.0 or MIT, at your option: [LICENSE-APACHE](LICENSE-APACHE), [LICENSE-MIT](LICENSE-MIT).
