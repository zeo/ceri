# Frames

How the engine makes a frame, what it carries from one frame to the next, and how to see where the time goes.

## What a frame does

The engine thread runs a frame in four steps. The names in brackets are the profiler's phases.

1. The page's timers and animation frame callbacks run (`script: tick`, with `script: callback` for each one), then audio, then the resize and intersection observers (`script: observers`), which measure the page and so may lay it out. After every input the engine also looks for transitions and animations to start (`frame: motion`).
2. Layout works out every box (`frame: layout`, `layout`). Style is part of it: the cascade matches rules to an element (`style: cascade`), the computed style applies them (`style: compute`), and one pass resolves custom properties (`style: custom properties`). Flex and grid containers (`layout: flex`, `layout: grid`) ask their items for intrinsic widths (`layout: intrinsic width`) and lay them out (`layout: element`).
3. Paint compares the new layout with the one on the canvas (`paint: damage`) and repaints the areas that differ (`paint: repaint`, `paint: document`). A first frame, or one whose size changed, paints everything (`paint: whole frame`). Shapes, borders, shadows, text, images and SVG scenes have phases of their own.
4. The window uploads the boxes that changed to its frame texture and presents.

Frames come as often as the display refreshes: the window tells the engine its monitor's refresh rate, and animation frames, the wakes of anything in motion and the frame slots follow it, in whole milliseconds no longer than the display's interval (8 ms at 120 Hz, 16 ms until a window says otherwise). A hidden or minimised window gets no frames at all: `document.visibilityState` turns `hidden`, `visibilitychange` fires, animation frame callbacks wait, and timers, workers and media go on.

A frame that changes nothing costs a comparison of keys: the layout of an unchanged document under an unchanged state is the same `Rc` as before, and the canvas is handed out again.

## What is kept between frames

Most frames change little: a playhead moves, a counter ticks, the pointer crosses a button. Everything below exists so that the cost of a frame follows what changed and not the size of the page.

| Kept | Stands until |
| --- | --- |
| The cascade of each element and generated box | An element is added, removed or moved, or any attribute but `style` changes its value (the document's selector version); setting an attribute or a class to what it already is reaches observers and nothing else. Focus, the active element, checked and disabled sets, control values, the viewport, the color scheme, reduced motion, container sizes, the top layer or a forced pseudo-class changes. The element's own `style` text changes, which drops that one cascade. |
| Boxes and intrinsic widths | Anything in the node's subtree is written, or an ancestor itself is. The sheet, the fonts or their web faces, the device scale, or any part of the style state but the clock changes. A box that sampled an animation or a transition, or sized a query container, lasts one layout. |
| The custom properties of the last two passes | The document or the state they read changes. |
| The canvas | Never thrown away while the size holds: damaged areas are repainted over it. While the window still holds the last frame, the next one is painted over the frame before it, which first catches up on the areas it missed, so no frame is copied. |
| A scrolled frame | A scroll by whole device pixels moves the pixels and repaints what came into view. |
| Rasters of SVG scenes that are slow to draw | The scene, the scale or the subpixel offset changes. Six million pixels in all, oldest dropped first. |

A change of the hovered element is narrower than the rest of the state. Only the elements on one of the two ancestor chains and not the other change whether they are hovered, and a rule can follow that only through a `:hover` compound one of them fits. The cascades and boxes of those subtrees are dropped, with the chain above each for the boxes. A sheet with `:hover` inside `:not()`, `:is()`, `:where()` or `:has()` drops everything on a hover change.

Three kinds of page give some of this up:

- A sheet that uses `:empty` or an attribute selector on `style` keeps no cascades and no boxes across layouts, because text and inline style change without moving the selector version.
- A document with a shadow root keeps none either: a shadow tree is written without its host hearing of it.
- A sheet with container queries keeps no boxes, because a query container's size is something each layout has to write down.

Kept boxes hold their whole subtree, so only what recent work used stays: the boxes beside the chain that changed, and not the ones inside them. An answer goes once two layouts that worked something out have not asked for it; layouts that find nothing changed, as a frame's does after a script has measured, age nothing. The engine thread lets go of the kept layout answers and the second canvas one second after the last frame.

## Measuring

The command line runs frames without a window. `--frames` advances the clock 16 ms a frame, lays out and paints; `--profile` adds the phase table.

```powershell
cargo run --release -p ceri-engine -- --input page.html --asset-root . --url https://app.ceri.invalid/page.html --width 1920 --height 1080 --scale 2 --frames 60 --profile
```

```
60 frames: 4.45 ms median, 6.19 ms mean, 98.21 ms worst, 225 fps at the median
per frame: script 0.40 ms, layout 1.96 ms, paint 2.06 ms
phase                          self ms  total ms     calls
layout: flex                     0.668     3.400      29.6
style: compute                   0.450     0.460      94.2
...
```

Each line is a phase: milliseconds per frame of its own, milliseconds with what it called, and calls per frame. The worst frame is the first, which paints everything.

`--sweep X,Y:X,Y` moves the pointer along a line, one step a frame, to time hover changes under an animation. `--output` writes the last frame, so two builds can be compared byte for byte.

A running app answers the same questions over devtools. Build it with `.devtools(true)`, start it with `--remote-debugging-port=9336`, and send the page target:

```json
{"id": 1, "method": "Performance.enable"}
{"id": 2, "method": "Performance.getMetrics"}
```

`Performance.enable` starts collecting, and each `Performance.getMetrics` returns the totals since then: `LayoutCount`, `LayoutDuration`, `RecalcStyleCount`, `RecalcStyleDuration` and `ScriptDuration` in the units Chromium uses (counts and seconds), then three metrics for every phase, such as `paint: shadow self`, `paint: shadow total` and `paint: shadow calls`. The frames painted are `frame: paint calls`; divide by it for a per-frame figure. `Performance.disable` stops collecting and forgets the totals.

A new phase is one line where the work starts:

```rust
let _profile = crate::profile::scope("paint: shadow");
```

A scope costs one thread-local read while profiling is off.

Four phases take no time and only count, to say whether a slow frame lost what it kept: `kept: answer taken` (a box or width taken from an earlier layout), `kept: answer written since` (one that was there but whose node or an ancestor had been written), `kept: all answers dropped` and `kept: all cascades dropped` (something global changed). A page that plays or animates should show the last two at zero and the first far above `layout: element`, the boxes laid out afresh.

## Checking

Everything kept is checked against working it out again.

- The `recheck` feature recomputes every cascade, box and intrinsic width taken from an earlier layout and panics where one differs. The test gates run with it; an app does not.

  ```powershell
  cargo test -p ceri-engine --all-targets --features "windowed native-http recheck"
  ```

- `a_layout_that_keeps_answers_matches_one_from_nothing_after_any_write` in `layout.rs` makes 600 seeded writes (inline styles, classes, text, inserts, removals, hover, `hidden`) with a running animation, and after each compares the layout that reuses answers with one that takes nothing from before.
- `a_frame_painted_over_an_earlier_one_matches_a_whole_paint_after_any_change` in `navigation.rs` does the same for paint, with pointer moves and scrolls, holding the previous frame through every paint as a window does.
- `shapes_fill_by_runs_as_they_do_pixel_by_pixel` and `shadows_fill_by_rows_as_they_do_pixel_by_pixel` in `paint.rs` hold the span fills and the row-wise shadow blur to plain per-pixel versions, byte for byte.

A debug build recomputes nothing unless `recheck` is on. Layout and paint go one call deeper per level of the tree and a debug build's calls are large, so the engine's threads reserve a 64 MB stack (`ENGINE_STACK`).

## Numbers

Measured on 2026-11-20 on a Ryzen 7 7800X3D, one thread, release build, 3840 by 2160 device pixels (1920 by 1080 at scale 2). The page is a video editor screen of about 1,500 elements during playback: every frame moves the playheads, rewrites the time readout and one button label.

| | Before | After |
| --- | --- | --- |
| Frame, median | 28 ms | 4.5 ms |
| of which layout | 11.5 ms | 2.0 ms |
| of which paint | 12 ms | 2.1 ms |
| Same, reading a `getBoundingClientRect()` every frame | | 5.1 ms |
| Same, with the pointer sweeping the window | | 7.0 ms |
| First frame (script, layout, paint of everything) | about 270 ms | about 100 ms |
| Paint of everything alone | 184 ms | 85 ms |

A paint of everything is still what a window resize pays at each step. At this size it is single-threaded fill work: SVG scenes 24 ms on their first draw, opaque fills 17 ms, shadows 16 ms, gradients 8 ms, text 6 ms.
