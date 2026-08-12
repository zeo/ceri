# Browser engine research

Research snapshot: 2026-08-12

## Decision

Ceri should keep CEF as the shipping engine while two independent engine tracks are explored. The first is a dependency-free custom engine laboratory written in Rust. The second is a Servo comparison backend because Servo is written in Rust, exposes an embedding-oriented `WebView` API, and has active desktop support for Windows, macOS, and Linux. Neither track is ready to replace the CEF path without an application compatibility and recovery test suite.

This separates three different goals:

1. Ceri owns its desktop runtime and app protocol. This is the current product work.
2. Ceri can embed a browser engine through a stable adapter boundary. This is the next architecture task.
3. Ceri implements a browser engine from first principles. This is a long-term research project with no shipping commitment.

Writing a new engine from scratch would require a standards implementation, JavaScript and WebAssembly execution, layout, painting, compositing, networking, media, accessibility, security boundaries, and platform integrations. Replacing CEF before those pieces pass the same tests would trade a known dependency for an unfinished browser.

## Candidates

| Candidate | What it gives Ceri | Main gap | Decision |
| --- | --- | --- | --- |
| CEF with Chromium | Broad current web compatibility, multi-process isolation, GPU, media, DevTools, and desktop binaries | Large bundle and Chromium upgrade work | Keep as the shipping baseline |
| Servo | Rust implementation, an embedding `WebView` API, WebGL and WebGPU, and Windows, macOS, Linux, Android, and OpenHarmony targets | Embedding documentation is still sparse, media needs GStreamer, the public API is evolving, and Servo has no 1.0 release or specific security guarantees | Keep as a comparison backend |
| Ladybird | A genuinely independent engine built from scratch with no Blink, WebKit, or Gecko code | The project is targeting its first 2026 alpha on Linux and macOS, and it is primarily a browser project rather than an embeddable desktop runtime | Track for research, not a Ceri backend yet |
| WPE WebKit | A focused embedded WebKit port with hardware-accelerated rendering and media support | WPE currently runs on Linux-based systems, and WebRTC is disabled in official release tarballs by default | Useful Linux comparison, not the all-platform engine |
| New engine written by Ceri | Full ownership of architecture and policy | Full browser-engine scope, long compatibility tail, and no security history | Build the isolated laboratory now |

The Servo embedding chapter explicitly says its documentation is sparse and still in progress. It also documents that common desktop media support requires the GStreamer feature and platform libraries. Servo's LTS guidance says the library has no 1.0 release and is provided on a best-effort basis without specific security guarantees. Those are acceptable conditions for an isolated experiment and unacceptable conditions for silently replacing the production backend.

WPE's architecture is attractive for embedded Linux because it separates WebKit from the final rendering backend. Its own FAQ says the port currently works only on Linux-based operating systems, so it cannot satisfy Ceri's desktop target by itself. WebKit's port documentation also warns that there are no cross-platform WebKit releases, each port publishes its own builds and security advisories.

Ladybird is the most interesting independent-engine project to watch. Its public project page describes a from-scratch engine and sets the first alpha target for Linux and macOS in 2026. That makes it useful for studying independent browser architecture, while its current milestone keeps it outside Ceri's compatibility baseline.

## Adapter boundary

The production app contract should not expose CEF types. The browser implementation should sit behind a small native boundary with these responsibilities:

- create and destroy a top-level view
- load an application URL from an embedder-owned asset source
- deliver input, resize, focus, clipboard, and IME events
- report navigation, title, load, console, and renderer-failure events
- evaluate only explicitly registered host capabilities
- paint or present frames through a platform-owned surface
- terminate and recover a renderer without changing the app origin
- expose typed navigation and recovery diagnostics to the native host

The asset resolver, `https://app.ceri.invalid` origin, SDK response shapes, recovery documents, and host-owned `ResourceProvider` contract can remain shared by CEF and an experimental engine. CEF-specific window and process code stays in its adapter. A future Servo adapter can use the same application contract without making Servo a production dependency.

The provider seam is now exercised by host-backed integration tests covering the document, stylesheet, module graph, JSON fetched by the page, and a POST request with normalized headers, a body, a 201 response, and response header membership, lookup, and iteration. The script runtime can construct mutable `Headers` values from objects, pair sequences, or another `Headers`, construct and clone `Request` objects, inspect their URL and method state, pass them through `fetch`, construct script-owned `Response` objects with status, status text, mutable headers, JSON serialization, error responses, and same-origin redirects, and retain an immutable response-header view for fetched resources. `AbortController` and `AbortSignal` now carry structured reasons, abort events, and deterministic preflight rejection through the fetch surface. Response bodies now have shared single-use state, `arrayBuffer()`, and a bounded default `ReadableStream` reader. The optional `native-http` feature adds a Rust-native HTTPS provider while keeping transport outside the renderer. Recovery pages also expose a typed URL, status, and detail diagnostic, so a native adapter does not need to scrape the document to classify a failed load. The renderer now reevaluates a bounded viewport-aware `@media` subset during layout, which is the first responsive cross-window gate.

## Compatibility gates

The current fetch experiment includes shared single-use response bodies, `arrayBuffer()`, `blob()`, `clone()`, `follow`/`manual`/`error` redirect modes with final URL state, a bounded `Request` constructor with URL, method, headers, cache, redirect, signal, `bodyUsed`, body cloning, single-use text, blob, or byte reads, and string, `ArrayBuffer`, `Blob`, `File`, `URLSearchParams`, or `FormData` bodies with inferred content types, a bounded `Response` constructor with status, status text, type, mutable script-created headers, `Response.json()`, `Response.error()`, same-origin `Response.redirect()`, and immutable fetched headers, mutable request `Headers` with constructor, `append()`, `set()`, `delete()`, `get()`, `has()`, key, value, and entry iterators, `forEach()`, and normalized response headers. Immutable `Blob` and `File` values support byte-part construction, MIME type normalization, file names and timestamps, slicing, text and `ArrayBuffer` reads, and request or response body integration. FormData bodies use deterministic multipart encoding with ordered text and File entries, filenames, per-file content types, and a collision-safe boundary. It also has one stable response stream object, a bounded default `ReadableStream` reader with deterministic byte chunks, and `tee()` branches that lock the source body. Full stream backpressure, BYOB readers, and wiring native transport into a production navigation queue remain separate gates.

An optional `native-http` feature now supplies a Rust-native HTTPS `ResourceProvider` with platform certificate verification, connection reuse, a 30-second timeout, and automatic redirect suppression so the engine retains redirect policy. Navigation follows a bounded chain of same-origin relative locations, rejects cross-origin locations, and consumes provider responses with their HTTP status, so upstream 404 and 503 failures reach the typed recovery page without extensionless shell fallback. Redirected modules resolve relative imports from their final URL. The new bounded `EngineWorker` owns `Browser` and `EngineHost` on a dedicated standard-library thread, and the native window adapter uses it for remote mode so blocking provider calls never run on the UI event thread. This proves a real transport adapter without making the renderer depend on CEF, Edge WebView2, or a loopback listener.

An experimental backend must pass these gates in order:

### Boot and origin

- starts without a loopback listener
- loads the exact app origin from packaged assets
- serves modules, styles, JSON, WebAssembly, fonts, images, and workers
- returns an app-owned recovery document for missing routes and load failures

### Web application surface

- ES modules and same-origin dynamic import
- `fetch`, streams, `URL`, `URLSearchParams`, and `AbortController`
- localStorage, IndexedDB, WebCrypto, and cache storage
- workers, timers, `requestAnimationFrame`, and WebAssembly
- CSS grid, flexbox, container queries, forms, focus, selection, and IME
- canvas 2D, WebGL, and WebGPU where the target platform exposes them

### Product integrations

- audio and video playback
- MSE and WebRTC capability reporting
- clipboard and file-pick boundaries
- accessibility tree delivery
- popup, permission, fullscreen, download, and authentication decisions
- crash, reload, and recovery behavior

### Platform and security

- Windows, macOS, and Linux x64 first
- ARM64 after the desktop contract is stable
- sandbox or equivalent process restrictions
- no renderer access to the asset root outside the native boundary
- reproducible resource packaging and an upgrade path for engine security fixes

CEF remains the comparison baseline. The Servo experiment should report pass counts and failures against the same pages, startup time, first usable frame, steady-state memory, renderer crash recovery, and bundle size. A speed claim is not valid until the workload and engine versions are pinned.

## First experiment

The first implementation is the separate `engine-lab` crate in this repository. Its default build has no CEF, WebKit, Servo, third-party JavaScript engine, GUI, or image-library dependency. Its current slice includes an HTML tokenizer and DOM tree, common entity decoding, CSS compound tag/class/id selectors with descendant and direct-child relationships, selector specificity, stateful `:hover`, `:focus`, `:checked`, `:disabled`, and `:enabled` matching, document and subtree `querySelector`, inherited text styles, block layout, row and column flex layout, basic grid tracks and gaps, inline text wrapping, borders, colors, a software paint step, a built-in bitmap font, bounded app-owned SVG `<img>` rasterization for rect, circle, and line shapes, static and dynamic ES module graphs with same-origin packaged imports, default and named exports, named re-exports, live named and namespace bindings, explicit JSON dynamic-import attributes, template literals with interpolation, and `import.meta.url`, a small JavaScript-like runtime with stable detached and attached DOM handles, `createElement`, `appendChild`, parsed `innerHTML` fragment replacement and serialization, DOM mutation, attribute and inline-style mutation, `classList` state changes, console logging, basic object and array values, immutable `Blob` and `File` values with byte reads, slicing, names, and timestamps, JSON parsing and serialization, cancellable bubbling click events, active-element tracking with control values, focus, blur, key, and input events, named form controls with live checkbox and radio-group state, ordered `FormData` text and `File` entries, cancellable `requestSubmit()` events with packaged GET navigation, a small canvas 2D context with software `fillStyle`, `fillRect`, and `clearRect` replay, app-origin `Location` properties and navigation requests, app-origin `URL` and `URLSearchParams` construction with linked query mutation, app-origin `localStorage` and `sessionStorage` stores shared across full-document navigations, nested anchor activation, session history with `pushState()`, `replaceState()`, `back()`, `forward()`, and `popstate`, layout-to-DOM coordinate hit testing, a platform-neutral `EngineHost` frame and input boundary, a deterministic task clock with timers and animation frames, a strict `https://app.ceri.invalid` URL model, same-origin relative URL resolution, same-origin promise-based `fetch()` responses and `response.json()` from packaged assets or an embedder-owned `ResourceProvider`, deterministic `AbortController` and `AbortSignal` state, reasons, events, and preflight cancellation, an in-memory packaged resource bundle, document-ordered external CSS and JavaScript, bounded script redirects, app-shell fallback, recovery documents, PPM or BMP output, and a textual layout dump.

The default custom renderer remains headless and does not open a loopback listener. An optional `windowed` feature now connects the `EngineHost` boundary to a `winit` 0.30.13 window and a `softbuffer` 0.4.8 CPU surface. With `native-http`, the same window boots an app from an HTTPS base without a listener and sends browser work through `EngineWorker`, while the script runtime can read packaged same-origin resources through a narrow promise-based `fetch()` surface with deterministic microtask draining, response status, MIME type, URL, text bodies, JSON objects, arrays, and numeric indexes. The current continuity slice also has attribute selectors, static `querySelectorAll` node lists, `dataset` reads, array spread and `map`, bounded `Map` operations, `for...of` over user-defined `Symbol.iterator` objects, object and array destructuring with default values and computed object keys, `Map` key iterator records, promise-backed async functions with `await`, same-origin dynamic import with namespace promises and deterministic rejection, explicit JSON dynamic-import attributes, live named and namespace bindings, static default, named, and star exports, a platform-neutral accessibility tree with semantic roles and stable DOM paths, deterministic draining of fulfilled promise chains, try/catch/throw, class and error values, an engine-owned typed runtime identity query, and the standard-library checks used by the real page. The packaged `/web/index.html` now loads through the custom pipeline without a recovery document and reaches `4 / 4`, with module loading, storage, packaged fetch, and the custom host query passing. The shared SDK keeps its CEF callback fallback for the production adapter. These slices prove native window input, resize, clock, frame presentation, app-owned resource reads, typed host integration, accessibility-tree delivery, and continuity-page execution without making the renderer depend on CEF, WebKit, Edge WebView2, or a loopback server. They do not pass any browser-compatibility gate. Async module evaluation, dynamic import attributes beyond JSON modules, full stream backpressure, BYOB readers, production navigation integration for native networking, media, full platform accessibility adapters, GPU APIs, WebAssembly, and mobile targets remain future gates. The existing continuity page remains the compatibility target, and it must first be rendered through the custom pipeline before the production adapter is reconsidered.

The experiment is useful even when a gate fails. A failure at a named gate tells us whether Ceri's stable contract is missing an abstraction. A passing slice gives us a measured basis for deciding whether to grow the custom engine or use Servo for that layer.

## Sources

- [CEF documentation](https://chromiumembedded.github.io/cef/general_usage.html)
- [Servo project](https://servo.org/)
- [Servo embedding overview](https://book.servo.org/embedding/overview.html)
- [Servo WebView API](https://doc.servo.org/servo/webview/struct.WebView.html)
- [Servo LTS releases](https://book.servo.org/embedding/lts-release.html)
- [Ladybird](https://ladybird.org/)
- [WPE WebKit](https://webkit.org/wpe/)
- [WPE FAQ](https://wpewebkit.org/about/faq.html)
- [WebKit ports](https://docs.webkit.org/Ports/Introduction.html)
- [SpiderMonkey](https://spidermonkey.dev/)
- [Boa](https://boajs.dev/)
- [winit 0.30.13](https://docs.rs/winit/0.30.13/winit/)
- [softbuffer 0.4.8](https://docs.rs/softbuffer/0.4.8/softbuffer/)
- [WHATWG Fetch Standard](https://fetch.spec.whatwg.org/)
- [WHATWG URL Standard](https://url.spec.whatwg.org/)
