# Ceri engine laboratory

This crate is a deliberately small browser-engine experiment written with the Rust standard library. Its default build has no CEF, WebKit, Servo, third-party JavaScript engine, or GUI dependency.

The current slice implements:

- HTML tokenization and a small DOM tree
- HTML entity decoding for common named and numeric entities
- CSS rules with compound tag, class, id, and attribute selectors, descendant matching, and direct-child matching
- a deterministic cascade with selector specificity
- document and subtree `querySelector` and `querySelectorAll` lookups using the same selector matcher
- block layout, row and column flex layout, inline text wrapping, margins, padding, borders, colors, and basic lengths
- layout boxes retain DOM paths for coordinate hit testing
- a software paint step with a small built-in bitmap font
- a platform-neutral `EngineHost` frame and input boundary
- a platform-neutral accessibility tree with semantic roles, names, values, focusability, disabled state, hidden filtering, and stable DOM paths
- a small JavaScript-like runtime with DOM mutation, console logging, and click events
- bubbling DOM events with `type`, `target`, `currentTarget`, `defaultPrevented`, and `preventDefault()`
- element attributes, inline styles, and `classList` state mutation
- stable detached and attached element handles with `document.createElement` and `appendChild`
- parsed fragment replacement and serialization through `innerHTML`
- a deterministic task clock with `setTimeout`, `clearTimeout`, `requestAnimationFrame`, and cancellation
- a strict `https://app.ceri.invalid` URL model with an in-memory packaged resource bundle
- same-origin relative URL resolution for external stylesheets and scripts
- packaged-origin promise-based `fetch()` responses with status, MIME type, URL, text bodies, and JSON values
- app-origin `URL` and `URLSearchParams` constructors with URL resolution, query parsing, mutation, sorting, and linked `searchParams` updates
- app-origin `localStorage` and `sessionStorage` key/value stores shared across full-document navigations in one browser session
- focusable controls with `document.activeElement`, `value` state, focus and blur events, keyboard events, text input, and deterministic backspace editing
- named form controls with live checkbox and radio-group state, `FormData` construction, pair mutation, and deterministic `requestSubmit()` events
- canvas 2D contexts with `fillStyle`, `fillRect`, `clearRect`, and deterministic software painting
- app-owned SVG `<img>` resources with bounded rect, circle, and line rasterization
- static and dynamic ES module graphs with same-origin packaged imports, default and named exports, live named and namespace bindings, named re-exports, and namespace imports
- template literals with expression interpolation and module `import.meta.url`
- static node-list snapshots, `dataset` reads, array spread and `map`, and bounded `Map` construction, lookup, key enumeration, and mutation
- bounded `for...of` over arrays, node lists, strings, maps, and user-defined `Symbol.iterator` objects, promise-backed async functions with `await`, `try`/`catch`/`throw`, class and error values, `typeof`, `instanceof`, conditional expressions, and the narrow `Number`, `Array`, `Object`, `String`, `Set`, `Promise`, and `Symbol.iterator` surfaces used by the continuity page
- bounded object and array destructuring in declarations, `for...of` bindings, and function parameters, including default values and computed object keys, plus `Map` key iterators with `next()` records
- an engine-owned `window.ceriQuery` runtime identity promise, with the shared SDK retaining its `window.cefQuery` fallback for the production adapter
- dynamic CSS pseudo-classes for `:hover`, `:focus`, `:checked`, `:disabled`, and `:enabled`
- basic CSS Grid containers with fixed, fractional, percentage, and auto tracks, row-major child placement, and gaps
- app-origin `Location` state, nested anchor activation, bounded script redirects, extensionless app-shell fallback, history, and app-owned recovery pages
- PPM or BMP output and a textual layout dump

It is a headless laboratory, not a web-compatible browser. The script runtime covers the demo's DOM and event path, while networking, media, full platform accessibility adapters, GPU APIs, WebAssembly, and the operating-system window remain outside this slice.

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

The adapter is an integration slice for Windows, macOS, and Linux desktop surfaces. It does not claim browser compatibility, mobile support, or a complete cross-platform release until those targets have their own build and runtime checks.

The fetch demo exercises an app-owned resource without opening a listener:

```powershell
cargo run --manifest-path engine-lab/Cargo.toml --bin ceri-engine-lab -- --input engine-lab/fetch-demo.html --asset-root engine-lab --url https://app.ceri.invalid/ --output target/ceri-fetch-demo.bmp
```

This first fetch slice is deterministic and limited to packaged same-origin resources. It returns a promise with `then()` and `catch()` and a response with `status`, `ok`, `url`, `mimeType`, promise-returning `text()`, and promise-returning `json()`. The runtime supports basic JSON objects, arrays, property access, numeric indexes, `JSON.parse`, and deterministic `JSON.stringify`. Microtasks drain after each script turn and clock advance. Missing resources resolve with status 404, while rejected origins reject the promise. Streaming bodies, network transports, request methods, and broader JavaScript object semantics remain separate compatibility gates.

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
