# Ceri

Ceri is a Rust-owned desktop runtime for web applications. It packages the Chromium Embedded Framework (CEF) with the application, so the browser engine is selected and updated with the app instead of coming from an operating-system webview.

The current runtime slice provides:

- CEF 150.0.14 with Chromium 150
- Rust ownership of process startup, sandbox setup, windows, navigation policy, asset routing, and recovery
- TypeScript-friendly JavaScript authoring through the zero-dependency SDK in `sdk/`
- a packaged HTTPS app origin, `https://app.ceri.invalid`
- direct bundled asset serving through a CEF scheme handler
- no loopback HTTP server, `file://` navigation, Edge WebView2, or Node.js requirement
- a native `cefQuery` compatibility surface for typed runtime information
- one renderer restart followed by a Ceri-owned recovery document

The design target is desktop Windows, macOS, and Linux on x64 and ARM64. Each release must be built with the matching CEF distribution for its target. Mobile targets are outside this CEF-based runtime.

## Build

Install Rust, CMake, Ninja, and the native toolchain for the target platform. The CEF crate can download its matching archive during the first build. A shared extracted CEF directory is faster and is required in restricted build environments.

On Windows, point `CEF_PATH` at the extracted CEF directory and put Ninja on `PATH`:

```powershell
$env:CEF_PATH = (Resolve-Path -LiteralPath '.\target\cef-cache\cef_windows_x86_64').Path
$env:Path = 'C:\Program Files (x86)\Microsoft Visual Studio\18\BuildTools\Common7\IDE\CommonExtensions\Microsoft\CMake\Ninja;' + $env:Path
cargo test --all-targets
cargo check --all-targets
```

Build an application bundle with the CEF files, executable, helper, and web assets:

```powershell
cargo run --bin ceri_bundle -- --release --output dist\windows-x64-release
```

The default Windows build enables the CEF sandbox. Launch the bundled executable from the generated bundle. The sandboxed executable is not a standalone copy operation because CEF starts the browser process through the bundled bootstrap DLL.

For development without the sandbox feature:

```powershell
cargo run --no-default-features --bin ceri_bundle -- --output dist\windows-x64-nosandbox-debug
```

## Authoring

The web surface is ordinary HTML, CSS, and JavaScript. TypeScript projects can consume the declarations in `sdk/index.d.ts` and emit browser-compatible modules into the packaged asset directory.

```js
import { runtimeInfo } from "./sdk/index.js";

const info = await runtimeInfo();
console.log(info.engine, info.chromium, info.recovery);
```

`runtimeInfo()` validates the response shape before returning it. Requests are limited to the runtime method currently implemented by Ceri. A future host API should add an explicit typed method and an explicit capability check, rather than exposing a general command evaluator.

## Failure behavior

Ceri verifies the entry document before CEF initializes. Every packaged app document is served from the owned HTTPS origin through the CEF scheme handler. Missing assets, unsafe paths, navigation errors, and repeated renderer termination produce a Ceri recovery document with a retry link. The browser's generic network failure page is not part of the application path.

The resolver rejects encoded separators, traversal, malformed UTF-8, Windows device names, invalid path characters, symlink escapes, and ambiguous empty path segments. Top-level extensionless routes may use the packaged `index.html`; missing subresources never fall back to the app shell.

See [docs/architecture.md](docs/architecture.md) for the process model and security boundaries.

## Custom engine laboratory

The isolated [engine-lab](engine-lab/README.md) crate is a from-scratch Rust rendering experiment. It currently parses a small HTML and CSS subset with compound, descendant, direct-child, and attribute selectors, stateful `:hover`, `:focus`, `:checked`, `:disabled`, and `:enabled` matching, supports document and subtree `querySelector` and `querySelectorAll`, loads same-origin external CSS and JavaScript from packaged resources, executes bounded static and dynamic ES module graphs with default, named, named re-exported, and export-star forwarded exports, explicit JSON dynamic-import attributes, template literals, and `import.meta.url`, exposes packaged-origin promise-based `fetch()` and `response.json()` data with deterministic `AbortController` cancellation and single-use response bodies, supports script-created `Response` objects with status, status text, mutable headers, body cloning, `Response.json()`, `Response.error()`, and same-origin `Response.redirect()`, supports app-origin `URL` and `URLSearchParams` construction and mutation, app-origin `localStorage` and `sessionStorage` state across full-document navigations, collects named controls through `FormData` and cancellable `requestSubmit()` events with live checkbox and radio-group state, draws a small canvas 2D command set through the software paint pass, rasterizes bounded app-owned SVG images through `<img>`, basic object and array values, static node-list snapshots with `dataset`, array spread and `map`, bounded `Map` construction and access, bounded `for...of` over built-in and user-defined iterables, object and array destructuring with default values and computed object keys, `Map` iterator records, promise-backed async functions with `await`, try/catch/throw, class and error values, an engine-owned typed runtime identity query, and the small standard-library surfaces used by the continuity page, constructs DOM nodes from scripts, replaces and serializes fragments through `innerHTML`, mutates attributes, inline styles, and `classList`, tracks `activeElement` and text-control values through focus, blur, key, and input events, runs a small script subset with bubbling cancellable DOM events, app-origin `Location` state, nested anchor activation, session history with `pushState()` and `popstate`, coordinate hit testing, and a deterministic task clock, navigates an app-owned HTTPS origin, performs block, flex, and basic grid layout, and exposes a platform-neutral frame and input boundary for native adapters. It paints a deterministic PPM or BMP image without CEF or another browser engine. An optional `windowed` feature connects that host boundary to a native desktop window through a CPU surface. It is research code and is not part of the production bundle.

See [docs/engine-research.md](docs/engine-research.md) for the candidate comparison and replacement gates.

The custom host also exposes a platform-neutral accessibility tree with semantic roles, names, values, focusability, disabled state, hidden filtering, and stable DOM paths. Its resource path has an embedder-owned `ResourceProvider` seam, so a native HTTP/TLS source can feed the same app-origin policy without a loopback listener. The optional `native-http` feature supplies a Rust-native HTTPS provider with platform certificate verification, bounded redirects, connection reuse, and a bounded response read. Navigation follows same-origin redirects under an engine-owned limit, while `ResourceRequest` carries validated fetch methods, headers, bodies, and cache modes across the seam, and `ResourceResponse` carries provider status and headers to the page. Fetch supports mutable request `Headers` construction and a bounded `Request` object with inherited init values, string, `ArrayBuffer`, or `URLSearchParams` bodies, automatic content type inference, body cloning, and provider dispatch, while fetched response headers remain read-only. Script-created `Response` objects support status, status text, type, mutable headers, body cloning, single-use reads, `Response.json()`, `Response.error()`, and same-origin `Response.redirect()`. Fetch response bodies support deterministic clone and tee branches with single-use state. `AbortSignal` can stop a fetch before provider dispatch, and `NavigationFailure` gives native adapters typed recovery status and detail without parsing the recovery DOM.

The optional remote window mode routes the browser through a bounded `EngineWorker` queue. The native event thread keeps handling window input and presentation while the blocking HTTPS provider runs on the engine thread.

The module laboratory now preserves live named and namespace bindings through re-exports, with imported names kept read-only while source modules update their exported cells. Its stylesheet path also reevaluates bounded `@media` width and height conditions for each native viewport.
