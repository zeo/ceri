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

The asset resolver, `https://app.ceri.invalid` origin, SDK response shapes, and recovery documents can remain shared by CEF and an experimental engine. CEF-specific window and process code stays in its adapter. A future Servo adapter can use the same application contract without making Servo a production dependency.

## Compatibility gates

An experimental backend must pass these gates in order:

### Boot and origin

- starts without a loopback listener
- loads the exact app origin from packaged assets
- serves modules, styles, JSON, WebAssembly, fonts, images, and workers
- returns an app-owned recovery document for missing routes and load failures

### Web application surface

- ES modules and dynamic import
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

The first implementation is the separate `engine-lab` crate in this repository. It has no CEF, WebKit, Servo, JavaScript engine, GUI, or image-library dependency. Its current slice includes an HTML tokenizer and DOM tree, common entity decoding, CSS tag/class/id selectors, selector specificity, inherited text styles, block layout, inline text wrapping, borders, colors, a software paint step, a built-in bitmap font, PPM output, and a textual layout dump.

The custom renderer is intentionally headless. JavaScript, networking, media, accessibility, GPU APIs, and the operating-system window are future gates. The existing continuity page remains the compatibility target, but it must first be rendered through the custom pipeline before any engine adapter is attempted.

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
