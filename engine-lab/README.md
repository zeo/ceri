# Ceri engine laboratory

This crate is a deliberately small browser-engine experiment written with the Rust standard library. Its default build has no CEF, WebKit, Servo, third-party JavaScript engine, or GUI dependency.

The current slice implements:

- HTML tokenization and a small DOM tree
- HTML entity decoding for common named and numeric entities
- CSS rules with compound tag, class, id, and attribute selectors, descendant matching, and direct-child matching
- a deterministic cascade with selector specificity
- document and subtree `querySelector` and `querySelectorAll` lookups using the same selector matcher
- block layout, row and column flex layout, inline text wrapping, margins, padding, borders, colors, and basic lengths
- bounded CSS positioning for flow-preserving relative visual offsets, `relative` containing blocks, `absolute` descendants, viewport-anchored `fixed` descendants, `inset` offsets, and integer `z-index` ordering within layout parents
- layout boxes retain DOM paths for coordinate hit testing
- a software paint step with a small built-in bitmap font
- a platform-neutral `EngineHost` frame and input boundary
- a bounded standard-library `EngineWorker` queue that owns browser state off the native event thread
- page-visible dedicated `Worker` objects with packaged or provider-backed same-origin classic and module entry scripts, dedicated threads, structured JSON-cloned messages, `postMessage()`, `addEventListener()`, `onmessage`, `onerror`, `terminate()`, and an explicit host `pump()` boundary
- a platform-neutral accessibility tree with semantic roles, names, values, focusability, disabled state, hidden filtering, and stable DOM paths
- a small JavaScript-like runtime with DOM mutation, console logging, and click events
- bubbling DOM events with `type`, `target`, `currentTarget`, `defaultPrevented`, and `preventDefault()`
- element attributes, inline styles, and `classList` state mutation
- stable detached and attached element handles with `document.createElement` and `appendChild`
- parsed fragment replacement and serialization through `innerHTML`
- a deterministic task clock with `setTimeout`, `clearTimeout`, `requestAnimationFrame`, and cancellation
- a strict `https://app.ceri.invalid` URL model with an in-memory packaged resource bundle
- an embedder-owned `ResourceProvider` fallback for documents and same-origin subresources
- an optional `native-http` `ResourceProvider` backed by Rust-native HTTPS and platform certificate verification
- validated `ResourceRequest` metadata for provider-backed `fetch()` methods, headers, and request bodies
- a bounded `Request` constructor with URL and method inspection, mutable request headers, cache and redirect state, string, `ArrayBuffer`, `Blob`, `URLSearchParams`, and text-only `FormData` bodies with inferred content types, body cloning, and single-use body reads
- immutable `Blob` and `File` values with byte-part construction, MIME type normalization, file names and timestamps, slicing, text and `ArrayBuffer` reads, and request or response body integration
- a bounded `Response` constructor with status, status text, type, mutable script-created headers, body cloning, single-use body reads, `Response.json()`, `Response.error()`, same-origin `Response.redirect()`, and `blob()` reads
- same-origin relative URL resolution for external stylesheets and scripts
- packaged-origin promise-based `fetch()` responses with status, MIME type, URL, normalized read-only response headers, case-insensitive membership checks, deterministic key, value, and entry iterators, `forEach()`, single-use text and JSON bodies, `arrayBuffer()`, `clone()`, a bounded `ReadableStream` reader, deterministic tee branches, and deterministic `AbortController` preflight cancellation
- a bounded `WebAssembly.instantiate()` path for fetched `ArrayBuffer` bytes, with i32 function parameters and results, exported functions, calls, locals, arithmetic, comparisons, and bitwise operations; imports, memory, tables, globals, floating-point types, SIMD, threads, streaming compilation, and structured control flow remain outside this slice
- app-origin `URL` and `URLSearchParams` constructors with URL resolution, query parsing, mutation, sorting, and linked `searchParams` updates
- `URLSearchParams` key, value, and entry iterators, `forEach()`, and `Symbol.iterator` preserve duplicate pairs and URL linkage
- app-origin `localStorage` and `sessionStorage` key/value stores shared across full-document navigations in one browser session
- focusable controls with `document.activeElement`, `value` state, focus and blur events, keyboard events, text input, and deterministic backspace editing
- named form controls with live checkbox and radio-group state, ordered `FormData` text and `File` entries, pair mutation, and deterministic `requestSubmit()` events
- `FormData` key, value, and entry iterators, `forEach()`, and `Symbol.iterator` preserve insertion order and File values
- `Worker` entry scripts, classic `importScripts()`, static module imports, and fetch responses are loaded from the packaged bundle or the embedder-owned `ResourceProvider`, run on a dedicated standard-library thread, and exchange bounded structured messages without a loopback listener; transfer lists and nested workers remain outside this slice
- canvas 2D contexts with `fillStyle`, `fillRect`, `clearRect`, and deterministic software painting
- app-owned SVG `<img>` resources with bounded rect, circle, and line rasterization
- static and dynamic ES module graphs with same-origin packaged imports, default and named exports, live named and namespace bindings, named re-exports, and namespace imports
- template literals with expression interpolation and module `import.meta.url`
- static node-list snapshots, `dataset` reads, array spread and `map`, and bounded `Map` construction, lookup, key enumeration, and mutation
- bounded `for...of` over arrays, node lists, strings, maps, and user-defined `Symbol.iterator` objects, promise-backed async functions with `await`, `try`/`catch`/`throw`, class and error values, `typeof`, `instanceof`, conditional expressions, and the narrow `Number`, `Array`, `Object`, `String`, `Set`, `Promise`, and `Symbol.iterator` surfaces used by the continuity page
- bounded object and array destructuring in declarations, `for...of` bindings, and function parameters, including default values and computed object keys, plus `Map` key iterators with `next()` records
- an engine-owned `window.ceriQuery` runtime identity promise, with the shared SDK retaining its `window.cefQuery` fallback for the production adapter
- dynamic CSS pseudo-classes for `:hover`, `:focus`, `:checked`, `:disabled`, and `:enabled`
- viewport-aware `@media` rules for `screen`, `all`, and min/max width or height conditions
- basic CSS Grid containers with fixed, fractional, percentage, and auto tracks, row-major child placement, and gaps
- `overflow: hidden` and `overflow: clip` clipping for software paint and coordinate hit testing
- bounded element and document scrolling with persistent `scrollTop`/`scrollLeft`, `window.scrollX`/`scrollY`, wheel input, and `scroll` events
- app-origin `Location` state, nested anchor activation, bounded script redirects, extensionless app-shell fallback, history, and app-owned recovery pages
- PPM or BMP output and a textual layout dump

It is a headless laboratory, not a web-compatible browser. The default build keeps networking, media, full platform accessibility adapters, GPU APIs, and the operating-system window outside this slice. Positioning is bounded to `relative` containing-block markers, `absolute` and `fixed` layout, pixel or percentage `inset` values, flow-preserving relative visual offsets, and per-parent integer `z-index` ordering. Element and document scrolling are bounded to one overflow mode, a classic-style 12px scrollbar gutter with software-painted tracks and thumbs, software paint and hit-test clipping, scroll offsets, wheel deltas, and synchronous `scroll` events. Inline-element positioned descendants, full stacking-context isolation, smooth scrolling, and scroll snapping remain separate gates. The scrollbar gutter is reserved after the bounded layout pass, so it does not claim full CSS reflow compatibility. WebAssembly is limited to the bounded i32 module path listed above. The optional `native-http` feature adds a Rust-native HTTPS `ResourceProvider` with platform certificate verification, bounded redirects, connection reuse, proxy environment support, and a 30-second request timeout. Its API is blocking, so the native window adapter uses `EngineWorker` to keep browser state and provider calls off the UI event thread. The worker queue is bounded and drops new input when saturated instead of growing without limit.

`Browser::with_resource_provider` keeps the renderer independent of transport. The engine resolves and validates the app-origin URL, checks the packaged bundle first, and asks the provider only when that bundle misses. The provider can read a signed package, a native HTTP/TLS client, or another host-owned source. It never needs to open a listener, and it cannot receive a cross-origin `AppUrl` because the URL parser rejects those references before the provider call. Extensionless document routes use the same `/index.html` shell fallback as packaged assets.

With `--features native-http`, `NativeHttpProvider::new("https://example.com/app")` maps the engine's owned `https://app.ceri.invalid` paths onto that HTTPS origin. It rejects insecure and credentialed bases, leaves redirect policy to the engine by disabling automatic redirect following, preserves response status and headers for `fetch()`, and limits response reads through ureq's bounded body reader. Navigation follows a bounded chain of same-origin relative locations and rejects cross-origin redirects. Redirected modules resolve their relative imports from the final module URL. The provider is an opt-in transport adapter, not a loopback server or a renderer dependency.

Hosts can inspect `Browser::last_navigation_failure()` or `EngineHost::last_navigation_failure()` after a recovery page is installed. The diagnostic carries the app URL, HTTP-style recovery status, and typed detail. An upstream provider status reaches the recovery page unchanged, a provider transport failure becomes status 503, missing assets remain status 404, and parser or script failures remain status 500.

`fetch(url, options)` accepts the deterministic request subset needed by a native provider: a URL string or `Request`, `method`, an object or mutable `Headers` instance for `headers`, a string, `ArrayBuffer`, `Blob`, `File`, `URLSearchParams`, or `FormData` `body`, the standard cache modes, a `redirect` mode of `follow`, `error`, or `manual`, and an `AbortSignal`. String, `Blob`, `File`, `URLSearchParams`, and `FormData` bodies infer `Content-Type` when the page did not supply one. FormData bodies use deterministic multipart encoding with ordered text and File entries, filenames, per-file content types, and a collision-safe boundary. `new Request(input, init)` exposes normalized `url`, `method`, `headers`, `cache`, `redirect`, `signal`, `bodyUsed`, `clone()`, `text()`, `blob()`, `formData()`, and `arrayBuffer()` state, and `fetch(request)` inherits its method, headers, body, cache, redirect, and signal values unless init overrides them. `new Response(body, init)` exposes `type`, `url`, `status`, `statusText`, `ok`, `redirected`, `headers`, `bodyUsed`, `clone()`, `text()`, `json()`, `blob()`, `formData()`, and `arrayBuffer()`, with mutable headers for script-created responses and immutable headers for fetched responses. `formData()` parses `application/x-www-form-urlencoded` bodies and deterministic `multipart/form-data` bodies into ordered text and File entries. `Response.json(data, init)` serializes a runtime value and supplies `application/json` when the init headers do not set a content type. `Response.error()` creates an immutable type-error response with status 0. `Response.redirect(url, status)` creates an immutable same-origin redirect response with a normalized `Location` header for status 301, 302, 303, 307, or 308. `new Headers()` accepts objects, pair sequences, or another `Headers` instance, and exposes normalized `append()`, `set()`, `delete()`, `get()`, `has()`, key, value, and entry iteration, and `forEach()`. Header names are normalized and control characters are rejected. Packaged GET resources are resolved locally; other requests cross the `ResourceProvider::provide_response` hook with their cache mode intact. The page can read the provider's status, MIME type, and normalized headers through the response. An aborted signal rejects before the provider is called, and listeners receive one deterministic `abort` event. Response bodies are single-use across `text()`, `json()`, `blob()`, `formData()`, and `arrayBuffer()`. `response.clone()` copies the unread body and metadata into an independent response, and rejects after the source body is locked, disturbed, or consumed. `response.body` returns one stable stream object for the response. Its `getReader()` exposes fixed byte chunks, `read()`, `releaseLock()`, `cancel()`, `locked`, and `done` state. `response.body.tee()` clones the unread bytes into two independently readable branches and locks the source body. Follow mode reports the final URL and `redirected = true`, manual mode returns the redirect response, and error mode rejects. Redirected POST requests use the engine's standard method rewrite rules. Full stream backpressure, BYOB readers, and TLS policy stay with future compatibility gates.

FormData bodies use deterministic multipart encoding with ordered text and File entries, filenames, per-file content types, and a collision-safe boundary. `FormData` exposes ordered `keys()`, `values()`, `entries()`, `forEach()`, and `Symbol.iterator` views over those entries.

`URLSearchParams` exposes the same ordered key, value, entry, `forEach()`, and `Symbol.iterator` views, including duplicate pairs and updates back to its owning `URL`.

`new Worker(url, { name, type })` resolves a same-origin classic or module entry script from the packaged resource bundle or the embedder-owned `ResourceProvider`. Classic Workers can call `importScripts()` sequentially, while module imports and `fetch()` calls inside either Worker type use the same packaged-first policy. Provider requests cross a bounded host-owned bridge and are serviced by `Browser::pump()` or `EngineHost` on the engine thread. The worker has `self.addEventListener("message", callback)`, `self.onmessage`, `postMessage()`, `fetch()`, and `globalThis`, with `importScripts()` available only to classic Workers. The page has `worker.addEventListener()`, `worker.postMessage()`, `worker.onmessage`, `worker.onerror`, `worker.name`, `worker.url`, and `worker.terminate()`. Messages cross the thread boundary as JSON-cloned values through a bounded queue. This slice does not support transfer-list ownership, nested workers, or browser-compatible structured-clone types beyond JSON values.

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

The collection sample runs the first continuity-page collection expression without a listener. It queries `[data-check]` nodes, spreads the static node list into an array, maps each element to a key-value pair, reads `dataset`, and constructs a `Map`:

```powershell
cargo run --manifest-path engine-lab/Cargo.toml --bin ceri-engine-lab -- --input engine-lab/collection-demo.html --asset-root engine-lab --url https://app.ceri.invalid/ --output target/ceri-collection-demo.bmp --width 800 --height 420
```

The current collection boundary stops before generators, user-defined iterator objects, and computed destructuring keys. Those remain explicit compatibility gates for the continuity page.

The destructuring sample exercises nested object and array bindings, object rest, a destructuring arrow-function parameter, and a `Map` iterator record:

```powershell
cargo run --manifest-path engine-lab/Cargo.toml --bin ceri-engine-lab -- --input engine-lab/destructure-demo.html --asset-root engine-lab --url https://app.ceri.invalid/ --output target/ceri-destructure-demo.bmp --width 800 --height 320
```

The dynamic-module sample runs a classic script that imports a same-origin packaged module, awaits its namespace object, destructures the exports, and joins an exported array:

```powershell
cargo run --manifest-path engine-lab/Cargo.toml --bin ceri-engine-lab -- --input engine-lab/dynamic-import-demo.html --asset-root engine-lab --url https://app.ceri.invalid/ --output target/ceri-dynamic-import-demo.bmp --width 800 --height 320
```

The JSON-module sample imports a same-origin JSON resource through `import()` with an explicit `{ with: { type: "json" } }` attribute and reads its default export:

```powershell
cargo run --manifest-path engine-lab/Cargo.toml --bin ceri-engine-lab -- --input engine-lab/json-import-demo.html --asset-root engine-lab --url https://app.ceri.invalid/ --output target/ceri-json-import-demo.bmp --width 800 --height 320
```

The live-binding sample updates an exported counter and observes the new value through both a named import and a namespace import:

```powershell
cargo run --manifest-path engine-lab/Cargo.toml --bin ceri-engine-lab -- --input engine-lab/live-binding-demo.html --asset-root engine-lab --url https://app.ceri.invalid/ --output target/ceri-live-binding-demo.bmp --width 800 --height 320
```

The default-binding sample evaluates fallback expressions for missing object properties, nested objects, array entries, and references to earlier bindings:

```powershell
cargo run --manifest-path engine-lab/Cargo.toml --bin ceri-engine-lab -- --input engine-lab/defaults-demo.html --asset-root engine-lab --url https://app.ceri.invalid/ --output target/ceri-defaults-demo.bmp --width 800 --height 320
```

The computed-key sample resolves object-pattern property names from string variables and binds the matching values:

```powershell
cargo run --manifest-path engine-lab/Cargo.toml --bin ceri-engine-lab -- --input engine-lab/computed-key-demo.html --asset-root engine-lab --url https://app.ceri.invalid/ --output target/ceri-computed-key-demo.bmp --width 800 --height 320
```

The iterator sample defines a `Symbol.iterator` method, returns `next()` records from a closure, and consumes the values through `for...of`:

```powershell
cargo run --manifest-path engine-lab/Cargo.toml --bin ceri-engine-lab -- --input engine-lab/iterator-demo.html --asset-root engine-lab --url https://app.ceri.invalid/ --output target/ceri-iterator-demo.bmp --width 800 --height 320
```

The packaged continuity page is also an integration target. It loads the real `/web/index.html`, its stylesheet, module graph, SDK module, and recovery asset through the custom resource bundle. The page reaches `4 / 4`: module loading, local storage, packaged fetch, and the engine-owned runtime identity query pass. The page stays loaded and does not fall back to a recovery document:

```powershell
cargo test --locked --manifest-path engine-lab/Cargo.toml --test continuity
```

The current language boundary stops before generators, broader class method semantics, and dynamic import attributes beyond same-origin JSON modules. Those remain explicit compatibility gates.

The event sample starts a click on a nested element and handles it on an ancestor through an external script:

```powershell
cargo run --manifest-path engine-lab/Cargo.toml -- --input engine-lab/event-demo.html --asset-root engine-lab --url https://app.ceri.invalid/ --output target/ceri-event-demo.bmp --width 800 --height 360 --click label
```

The same sample can dispatch a pointer location through layout hit testing instead of naming an element:

```powershell
cargo run --manifest-path engine-lab/Cargo.toml -- --input engine-lab/event-demo.html --asset-root engine-lab --url https://app.ceri.invalid/ --output target/ceri-event-demo-pointer.bmp --width 800 --height 360 --click-at 60,160
```

`EngineHost` is the native-window boundary. A platform adapter navigates it, submits `InputEvent` values, calls `render`, and uploads `Frame.canvas.rgba_pixels()` using `Frame.canvas.bytes_per_row()`. It also exposes session-history `go`, `back`, and `forward` operations so native navigation controls can use the same browser state as page scripts. The lab CLI uses this boundary for navigation, coordinate input, timers, and presentation.

The default lab build stays dependency-free. The optional `windowed` feature adds a small native desktop adapter built with pinned `winit` 0.30.13 and `softbuffer` 0.4.8. It creates a platform window, forwards resize and pointer events, advances the deterministic page clock, and presents the custom engine's RGBA canvas through a CPU surface. It does not add CEF, WebKit, Edge WebView2, a loopback server, or a JavaScript engine.

Compile the adapter from the repository root:

```powershell
cargo check --manifest-path engine-lab/Cargo.toml --features windowed --bin ceri-engine-window
cargo run --manifest-path engine-lab/Cargo.toml --features windowed --bin ceri-engine-window
```

To load a remote app through the Rust-native HTTPS provider, pass an HTTPS base URL and enable both optional features:

```powershell
cargo run --manifest-path engine-lab/Cargo.toml --features "windowed native-http" --bin ceri-engine-window -- --native-base https://example.com/app
```

The remote mode maps `https://app.ceri.invalid/` into the supplied base, follows bounded same-origin redirects, and never opens a listener. The windowed remote mode constructs the provider and browser on `EngineWorker`, then queues navigation, input, and frame work across a bounded channel. The native event thread can continue receiving window messages while a provider request is in flight.

The adapter is an integration slice for Windows, macOS, and Linux desktop surfaces. It does not claim browser compatibility, mobile support, or a complete cross-platform release until those targets have their own build and runtime checks.

The fetch demo exercises an app-owned resource without opening a listener:

```powershell
cargo run --manifest-path engine-lab/Cargo.toml --bin ceri-engine-lab -- --input engine-lab/fetch-demo.html --asset-root engine-lab --url https://app.ceri.invalid/ --output target/ceri-fetch-demo.bmp
```

This first fetch slice is deterministic and limited to same-origin resources supplied by the packaged bundle or `ResourceProvider`. It returns a promise with `then()` and `catch()` and a response with `status`, `ok`, `url`, `redirected`, `mimeType`, `type`, `statusText`, normalized immutable headers for fetched resources, promise-returning `text()`, `json()`, `blob()`, `formData()`, `arrayBuffer()`, and `clone()`. Script-created responses can set status, status text, headers, and a string, `ArrayBuffer`, `Blob`, or `File` body, and `Response.json()` creates an `application/json` response. Request code can build `Request` and mutable `Headers` values, inspect or clone request bodies, read request blobs, parse URL-encoded or multipart bodies into FormData, pass them to `fetch`, and observe normalized values at the provider boundary. FormData requests preserve ordered text and File entries in deterministic multipart parts with filenames and content types. The body lifecycle is shared, so a second consumer rejects after the first one disturbs the body. `response.body` exposes one stable bounded `ReadableStream` with fixed byte chunks, a default reader, and deterministic `tee()` branches. `AbortController` and `AbortSignal` expose abort state, structured reasons, event listeners, `throwIfAborted()`, and preflight fetch rejection. The runtime supports basic JSON objects, arrays, property access, numeric indexes, `JSON.parse`, and deterministic `JSON.stringify`. Microtasks drain after each script turn and clock advance. Missing resources resolve with status 404, while rejected origins reject the promise. A concrete network provider, full stream backpressure, BYOB readers, TLS policy, and broader JavaScript object semantics remain separate compatibility gates.

The navigation demo exercises link default actions and `Location.replace()` across packaged pages:

```powershell
cargo run --manifest-path engine-lab/Cargo.toml --bin ceri-engine-lab -- --input engine-lab/navigation-demo.html --asset-root engine-lab --url https://app.ceri.invalid/ --output target/ceri-navigation-link.bmp --click open-link
cargo run --manifest-path engine-lab/Cargo.toml --bin ceri-engine-lab -- --input engine-lab/navigation-demo.html --asset-root engine-lab --url https://app.ceri.invalid/ --output target/ceri-navigation-replace.bmp --click replace-link
cargo run --manifest-path engine-lab/Cargo.toml --bin ceri-engine-lab -- --input engine-lab/navigation-demo.html --asset-root engine-lab --url https://app.ceri.invalid/ --output target/ceri-navigation-spa.bmp --click push-link --click back-link
```

The first command activates `/navigation-about.html` through a nested link. The second uses `window.location.replace()` and preserves the query and fragment in the app-owned URL. The third adds a same-document route with `history.pushState()`, then traverses it with `history.back()` and `popstate`. Full-page history entries reload their packaged document, while same-document entries retain the DOM and runtime. External origins, loopback listeners, and network transports remain outside this laboratory slice.

The URL demo exercises the app-origin `URL` and `URLSearchParams` APIs without a listener:

```powershell
cargo run --manifest-path engine-lab/Cargo.toml --bin ceri-engine-lab -- --input engine-lab/url-demo.html --asset-root engine-lab --url https://app.ceri.invalid/ --output target/ceri-url-demo.bmp --click run-url
```

The demo resolves a URL against the current app location, mutates its query parameters, sorts the entries, and displays the synchronized URL string. The constructor rejects origins outside `https://app.ceri.invalid`.

The input demo exercises focus, active-element state, text insertion, and key editing:

```powershell
cargo run --manifest-path engine-lab/Cargo.toml --bin ceri-engine-lab -- --input engine-lab/input-demo.html --asset-root engine-lab --url https://app.ceri.invalid/ --output target/ceri-input-demo.bmp --click name --text rust --key Backspace
```

The `--text` and `--key` switches feed the same `EngineHost` input boundary used by the optional native window adapter. The window adapter forwards winit keyboard and IME commit events through that boundary.

The grid demo exercises fixed row tracks, fractional columns, repeat syntax, and gaps:

```powershell
cargo run --manifest-path engine-lab/Cargo.toml --bin ceri-engine-lab -- --input engine-lab/grid-demo.html --asset-root engine-lab --url https://app.ceri.invalid/ --output target/ceri-grid-demo.bmp
```

Grid placement is currently row-major. Explicit `grid-column`, auto-placement modes, and subgrid remain separate compatibility work.

Positioned overlays use the nearest `position: relative` ancestor as their bounded containing block. Relative boxes keep their flow geometry while their visual offset moves paint and coordinate hit testing together. Positioned siblings use bounded integer `z-index` ordering within each layout parent. `position: fixed` uses the requested viewport. `overflow: hidden`, `overflow: clip`, and `overflow: auto` constrain both software paint and coordinate hit testing, which keeps a clipped child from receiving pointer input outside its visible box. Element containers expose `scrollTop` and `scrollLeft`; the document exposes `window.scrollX`, `window.scrollY`, `pageXOffset`, `pageYOffset`, `scrollTo()`, and `scrollBy()`. Both paths accept host wheel input, reserve a classic-style scrollbar gutter for overflowing content, paint tracks and thumbs, and dispatch `scroll` when the offset changes. Full stacking-context isolation, smooth scrolling, and scroll snapping remain separate gates.

The storage demo exercises app-owned `localStorage` and `sessionStorage` values without opening a listener:

```powershell
cargo run --manifest-path engine-lab/Cargo.toml --bin ceri-engine-lab -- --input engine-lab/storage-demo.html --asset-root engine-lab --url https://app.ceri.invalid/ --output target/ceri-storage-demo.bmp --click run-storage
```

The stores live in the `Browser` instance, so values survive full-document navigation and remain isolated from other browser instances. They are currently in-memory, with no disk persistence, quota enforcement, or cross-process sharing.

The form demo exercises named controls, `FormData`, and a cancellable submit event:

```powershell
cargo run --manifest-path engine-lab/Cargo.toml --bin ceri-engine-lab -- --input engine-lab/form-demo.html --asset-root engine-lab --url https://app.ceri.invalid/ --output target/ceri-form-demo.bmp --click submit
```

The current form boundary collects string values from named `input`, `textarea`, `select`, and `button` descendants, skips disabled and unchecked checkbox or radio controls, applies cancellable checkbox click defaults, and resolves a GET action back into the packaged app origin with URL-encoded pairs. POST bodies and external network transport remain outside this slice.

The canvas demo exercises the first drawing surface without a GPU or webview:

```powershell
cargo run --manifest-path engine-lab/Cargo.toml --bin ceri-engine-lab -- --input engine-lab/canvas-demo.html --asset-root engine-lab --url https://app.ceri.invalid/ --output target/ceri-canvas-demo.bmp --click redraw
```

The current canvas context is intentionally small. It records app-owned `fillStyle`, `fillRect`, and `clearRect` commands, clips them to the laid-out canvas element, and replays them during the software paint pass. Paths, text, images, transforms, compositing, and GPU contexts remain separate compatibility gates.

The packaged SVG demo exercises a dependency-free image path:

```powershell
cargo run --manifest-path engine-lab/Cargo.toml --bin ceri-engine-lab -- --input engine-lab/image-demo.html --asset-root engine-lab --url https://app.ceri.invalid/ --output target/ceri-image-demo.bmp
```

The current image slice accepts app-origin `image/svg+xml` resources, enforces bounded dimensions, and rasterizes `rect`, `circle`, `line`, inherited paint, and simple inline style declarations. Broken, unsupported, or cross-origin image references stay absent from the frame instead of escaping the resource boundary. PNG, JPEG, animated formats, filters, text, paths, and image decoding remain separate gates.

The static module demo exercises packaged module loading without a listener:

```powershell
cargo run --manifest-path engine-lab/Cargo.toml --bin ceri-engine-lab -- --input engine-lab/module-demo.html --asset-root engine-lab --url https://app.ceri.invalid/ --output target/ceri-module-demo.bmp --width 800 --height 300
```

The current module slice accepts external and inline `type="module"` scripts, resolves relative same-origin imports, supports default and named imports, namespace imports, live named and namespace bindings, `export default`, `export const`, `export function`, local export lists, named `export { ... } from` re-exports, and `export * from` forwarding with explicit-export precedence, caches completed modules, exposes `import.meta.url`, and rejects cycles or missing and cross-origin dependencies. Classic scripts can also use same-origin dynamic `import()` and receive a promise-backed module namespace, with deterministic rejection for missing, unsupported, and cross-origin resources. A dynamic import can request a same-origin `application/json` resource with `{ with: { type: "json" } }`, which exposes a parsed value as the namespace default export. Template literals support text plus `${...}` expressions. Async module evaluation, import attributes beyond JSON, tagged templates, nested template literals, and the rest of JavaScript grammar remain separate gates.

The default-module sample loads a packaged module with a default function and a named value:

```powershell
cargo run --manifest-path engine-lab/Cargo.toml --bin ceri-engine-lab -- --input engine-lab/default-module-demo.html --asset-root engine-lab --url https://app.ceri.invalid/ --output target/ceri-default-module-demo.bmp --width 800 --height 320
```

The re-export sample loads a named value through an intermediate packaged barrel module:

```powershell
cargo run --manifest-path engine-lab/Cargo.toml --bin ceri-engine-lab -- --input engine-lab/reexport-demo.html --asset-root engine-lab --url https://app.ceri.invalid/ --output target/ceri-reexport-demo.bmp --width 800 --height 320
```

The export-star sample forwards every non-default name from a source module and keeps an explicit barrel export when the names collide:

```powershell
cargo run --manifest-path engine-lab/Cargo.toml --bin ceri-engine-lab -- --input engine-lab/export-star-demo.html --asset-root engine-lab --url https://app.ceri.invalid/ --output target/ceri-export-star-demo.bmp --width 800 --height 320
```

The dynamic selector demo exercises stateful CSS through the host input boundary:

```powershell
cargo run --manifest-path engine-lab/Cargo.toml --bin ceri-engine-lab -- --input engine-lab/state-demo.html --asset-root engine-lab --url https://app.ceri.invalid/ --output target/ceri-state-demo.bmp --move-at 80,260 --click switch
```

The `--move-at` switch dispatches a pointer move before the click, while the click updates focus and checkbox state. The layout and paint pass then recompute pseudo-class styles without a page reload.

The responsive CSS subset reevaluates `@media screen` and `@media all` rules on every layout using the requested viewport. It supports `min-width`, `max-width`, `min-height`, and `max-height` in pixels. Unsupported at-rules and media features surface as stylesheet errors instead of being silently treated as ordinary selectors.
