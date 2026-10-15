# sideDuck — specification

Read `AGENTS.md` for the operating rules before building anything from this file.

## Mission

Build `sideDuck`, a macOS painting and annotation application with no practical
layer limit, end to end, with no human in the loop. The run ends when every item
in the Completion checklist is true and `make check` passes on a fresh clone.

The problem it exists to solve: iPad painting apps tie their layer ceiling to
canvas size because every layer is a fully resident bitmap and the device has a
few gigabytes to spend. A 4096×4096 RGBA8 layer is 64 MiB resident, 128 MiB at
16-bit, so a large canvas runs out of layers quickly. `sideDuck` fixes this three
ways at once: it runs on a Mac with far more memory, it stores layers as sparse
tiles so empty space costs nothing, and it composites against two cached textures
so the number of layers barely affects the work done per frame.

The iPad becomes the drawing surface over Sidecar, which is free, built into
macOS, needs no developer account, and does deliver Apple Pencil pressure and
tilt to applications that implement tablet-event handling. The Mac display holds
every panel; the iPad holds nothing but canvas. That two-screen split is a thing
a tablet-only app structurally cannot do.

The document is an operation log with raster checkpoints, not only pixels. That
choice costs disk and complexity in v1 and unlocks a set of features in n+1 that
no shipping app in this category has.

## Product scope

In scope for v1:

- Sparse tile-based layer storage with a memory-mapped tile store, residency
  budget, LRU eviction, and compression for cold tiles.
- A compositor with cached below-stack and above-stack textures, correct handling
  of backdrop-dependent blend modes, and a tiled viewport renderer with a
  resolution pyramid for zoomed-out views.
- An operation log with periodic raster checkpoints, deterministic replay, and
  crash-safe journaling.
- A dab-stamping brush engine: shape and grain textures, spacing, scatter,
  jitter, wet mixing, and per-parameter response curves driven by pressure, tilt,
  azimuth, and velocity. Brushes are data, with an editor and an open format.
- Tablet input on macOS: pressure, tilt, azimuth, and barrel/eraser detection,
  with stroke stabilization, forward prediction, a pressure-curve editor, and
  pressure auto-calibration from sample strokes.
- A canvas-only window mode for the Sidecar display, with stylus-only input as the
  default and a modifier to suspend snapping or stabilization mid-stroke.
- Layers: groups, clipping masks, layer masks, blend modes, opacity, lock, merge,
  reorder, and unlimited count.
- Selections and transform: rectangle, ellipse, lasso, magic wand, feather,
  invert, grow and shrink; move, scale, rotate, flip, free distort; copy, cut, and
  paste into a new layer.
- Annotation: a live transparent always-on-top overlay for drawing over whatever
  is on screen, a variant pinned to a single window, and a capture mode that
  turns a screenshot into a document.
- Reference layers: dropped PDFs and images kept as sources and re-rendered at the
  current zoom rather than flattened, with annotations on raster layers above.
- Export: PNG, TIFF, JPEG, and PDF with annotations composited or kept as a
  separate layer.
- Undo across everything, backed by the tile journal and the operation log.

Out of scope for v1 — these are phase n+1, specified at the end of this document
so their v1 hooks exist:

- The guide and ruler system (perspective, parallel, concentric, curve, symmetry),
  snapping, and per-layer guide assignment.
- Adjustment layers, filters, liquify, and smudge beyond a basic implementation.
- Text and typography. Animation and timelapse. PSD import and export.
- Canvas re-render at a new resolution, retroactive brush change, document
  branching and diffing, and perspective estimation from a photograph.
- Any iPad application, any App Store or TestFlight distribution, any custom
  iPad-to-Mac streaming client. Sidecar is the only iPad path in v1.
- Cloud sync, accounts, collaboration, telemetry.

## Naming and motifs

The app is `sideDuck`, always that exact casing: lowercase s, capital D, one word,
no space, no hyphen. The name is a Sidecar pun and the duck is the mascot.

Motifs are allowed, and they are bounded. They live only in user-facing surfaces:
the app name, the icon, file extensions, the About panel, and empty states. They
never appear in crate names, type names, function names, module paths, log
messages, error text, settings labels, menu items, tool names, or the technical
vocabulary of this specification. A control's label says what the control does. If
a motif would make someone guess what a thing is, it does not belong there.

The complete approved list, and nothing beyond it without the user asking:

- **File extensions.** A document package is `.pond`; a brush package is `.quill`.
  Both are documented in `RUNBOOK.md` under their plain descriptions.
- **App icon.** A duck in profile, built from a few overlapping flat shapes that
  read as stacked layers, legible at 16 px. No gradients, no rubber-duck
  photorealism, no bath toy. Generate it as SVG and render the required icon sizes
  at build time; do not download art.
- **About panel.** The duck mark, the version, the build hash, and the measured
  benchmark numbers from the last `make bench`.
- **Empty states**, three of them, one short line each with no exclamation marks:
  no document open, a document with no layers, and an empty brush library.
- **One easter egg**, optional and strictly bounded: a small duck that paddles
  across the About panel on a ten-second loop. It must be disableable, must not
  run while a document is open, and must cost nothing when not visible.

Do not name brushes, tools, layer modes, or menu commands after ducks or water.
"Soft airbrush" stays "Soft airbrush".

## Architecture

A Rust workspace, one language throughout, buildable and testable on Linux and on
macOS. The GPU layer is `wgpu`, which targets Metal on macOS and Vulkan on Linux,
so the agent can build and test everything the user will run.

```
sideduck-tile        tile geometry, sparse layer storage, mmap store, compression,
                   residency manager, dirty tracking
sideduck-composite   blend modes, below/above caches, run flattening, viewport
                   pyramid, CPU reference compositor
sideduck-brush       dab stamping, response curves, wet mix, deterministic
                   rasterizer, CPU reference rasterizer, brush format
sideduck-doc         layers, groups, masks, selections, transform, operation log,
                   checkpoints, journal, document package format
sideduck-input       sample stream, stabilization, prediction, pressure curves,
                   auto-calibration, stylus-only routing
sideduck-platform    traits for tablet events, screen capture, window overlay,
                   display enumeration; macos/ and mock/ implementations
sideduck-ui          egui panels, tools, canvas view, canvas-only window
sideduck-app         binary: window management, document lifecycle, wiring
sideduck-bench       criterion benchmarks that double as budget assertions
```

Key crates, pinned: `wgpu`, `winit`, `egui` with `egui-wgpu`, `memmap2`, `lz4_flex`
or `zstd`, `rkyv` or `postcard` for tile and log serialization, `half` for 16-bit
float tiles, `rayon` for tile-parallel CPU work, `objc2` plus `objc2-app-kit` and
`objc2-screen-capture-kit` behind `#[cfg(target_os = "macos")]`, `criterion`,
`proptest`.

`sideduck-platform` is the only crate that knows about macOS. Everything above it
talks to traits: `TabletSource` yields input samples, `ScreenCapture` yields
images, `OverlayWindow` manages the transparent always-on-top surface,
`DisplayInfo` enumerates displays and identifies which one is a Sidecar iPad. The
mock implementations replay recorded sample streams and synthetic images, which is
what the whole test suite runs against.

Make targets: `setup`, `check` (fmt, clippy, test, release build, budget
benchmarks), `test`, `bench`, `run` (the app against a scratch document),
`soak` (the long-running memory and crash-safety harness), `dist`.

## Tile store

The foundation. Everything else depends on getting this right.

- **Tiles** are 256×256 by default, configurable at document creation. Formats:
  RGBA8 premultiplied (default) and RGBA16F (high bit depth). A 256×256 RGBA8 tile
  is 256 KiB; a 4096×4096 layer is 16×16 = 256 tiles.
- **Sparse layers.** A layer owns a tile index, not a buffer. Untouched tiles do
  not exist; a layer with 20 painted tiles costs 5 MiB rather than the 64 MiB a
  dense 4K layer would. Tiles that become fully transparent are dropped from the
  index on save.
- **Store.** One memory-mapped file per document holding all tile blobs, with an
  index mapping (layer id, tile coord, generation) to an offset, length, and
  codec. Cold tiles are compressed; hot tiles are stored raw. Writes are
  append-only with periodic compaction, which is what makes crash safety cheap.
- **Residency manager.** A byte budget, defaulting to a fraction of physical
  memory and overridable in settings, with LRU eviction and pinning for the tiles
  the compositor currently needs. Eviction writes dirty tiles to the store first.
  Prefetch is a window around the active layer and the visible tile region — this
  is where the "load layers in batches" idea lives, as an async prefetch of
  neighbouring layers' visible tiles on layer-selection change.
- **Dirty tracking** per tile and per layer, feeding the compositor's cache
  invalidation and the undo journal.

## Compositor

The reason layer count stops mattering.

- Partition the stack around the active layer. Everything below flattens into one
  **backdrop texture**; everything above flattens into an **overlay**; editing
  touches three textures regardless of whether the document has 12 layers or 800.
- **Blend-mode correctness is the subtle part.** A layer whose blend mode depends
  on its backdrop (multiply, screen, overlay, and the rest) cannot be
  pre-flattened independently of what is beneath it. So the above-stack is
  compiled into maximal runs: consecutive layers that are backdrop-independent
  (normal/alpha-over, not clipping, not masked by a group with a non-normal mode)
  flatten into one texture; every backdrop-dependent layer stays a separate pass
  replayed each frame. Document the rule and the run-compilation algorithm in
  `sideduck-composite/README.md` and test it against the reference.
- **Cache invalidation**: editing layer k invalidates the backdrop for all active
  indices above k and the overlay for all active indices below k. Switching the
  active layer rebuilds at most one of the two, incrementally where possible.
- **Viewport rendering** composites only tiles intersecting the visible region at
  the current zoom. A mip pyramid per layer, maintained incrementally on dirty
  tiles, serves zoomed-out views so a 16K canvas at fit-to-screen reads from a
  small level rather than every full-resolution tile.
- **Reference compositor**: a scalar CPU implementation of every blend mode and
  the full stack, used only by tests.

Blend modes for v1: normal, multiply, screen, overlay, darken, lighten,
colour-dodge, colour-burn, hard-light, soft-light, difference, exclusion, hue,
saturation, colour, luminosity, plus add and subtract. Colour management: working
space is linear sRGB internally with Display P3 output; document the premultiplied
alpha convention and test round-trips.

## Document model and operation log

A document is a package directory with the extension `.pond`: `manifest.json` (version, canvas size, colour
space, tile size), `tiles.store` plus `tiles.index`, `ops.log`, `checkpoints/`,
`thumbnail.png`.

- **Operation log.** Every mutating action appends a record: strokes as their full
  input sample stream (position, pressure, tilt, azimuth, timestamp) plus the
  brush parameter set and seed; layer operations, transforms, and selection edits
  as structured records. Records are content-addressed and ordered.
- **Checkpoints.** Every N operations, and always immediately after an operation
  that is expensive to replay, write the affected tiles as a checkpoint so replay
  starts from the nearest one rather than the beginning.
- **Deterministic replay.** Replaying the log from a checkpoint reproduces the
  document bit-identically on the same platform. This is a tested property, and it
  is what phase n+1's re-render, retroactive brush edit, and diff features depend
  on.
- **Crash safety.** The log and the tile store are append-only with a commit
  record; opening a document rolls forward to the last valid commit. Killing the
  process mid-stroke must never corrupt a document, and the soak harness proves it
  by doing exactly that repeatedly.
- **Undo** is the journal of pre-images for dirty tiles per operation, bounded by
  a configurable disk budget, plus the log for structural operations. Unlimited
  layers makes a memory-resident undo stack untenable, so undo lives on disk.

## Brush engine

Dab stamping. A stroke is resampled to even spacing along its path, and at each
dab the engine stamps a shape texture modulated by a grain texture.

- **Parameters**: shape and grain textures, size, spacing, scatter, rotation and
  rotation-follows-direction, flow, opacity, hardness, grain scale and motion,
  wet-mix amount and pickup length, colour jitter in HSV, and smoothing.
- **Response curves**: every parameter above can be driven by pressure, tilt,
  azimuth, velocity, or a random stream, each through an editable curve. This is
  where brush feel actually comes from.
- **Stroke-level compositing**: dabs accumulate into a stroke buffer so opacity
  applies once per stroke rather than per dab, which is what prevents the muddy
  overlap look.
- **Determinism**: jitter and scatter draw from a seeded stream derived from
  (stroke id, dab index). Same inputs, same pixels.
- **Format**: a JSON parameter file plus two PNG textures in a `.quill`
  package. Ship at least 12 tuned brushes covering pencil, ink pen, technical pen,
  soft airbrush, hard round, flat bristle, wet oil, watercolour, charcoal, marker,
  texture stamp, and a soft eraser. Quality over count; the user tunes the feel
  afterward.
- **Reference rasterizer**: a scalar CPU implementation used for parity tests.

## Input and Sidecar

- **Tablet events.** On macOS, `TabletSource` reads the tablet-point event path
  for pressure, tilt in both axes, azimuth, and the eraser/barrel indicator,
  rather than treating the stylus as a mouse. This is what "advanced stylus
  support" means and it is what makes pressure work over Sidecar.
- **Stylus-only canvas.** By default the canvas accepts tablet events only, and
  mouse or trackpad input pans and operates the UI. This is a setting, not a law.
- **Stabilization**: a configurable smoothing filter over the incoming samples,
  with a pull-string mode and an average mode.
- **Prediction**: extrapolate a short way ahead of the latest sample and render
  the predicted segment provisionally, reconciling when the real samples arrive.
  Sidecar adds latency that cannot be removed, so this is a feature, not an
  optimization.
- **Pressure curve**: a global curve plus a per-brush curve, both editable. Plus
  **auto-calibration**: the user draws a few natural strokes, the app fits the
  curve to their observed pressure distribution, and shows the before-and-after.
- **Input filter chain.** All samples pass through an ordered chain of filters
  (calibration, stabilization, prediction, and later snapping). Adding the guide
  system in n+1 must mean adding one filter, not restructuring input.
- **Latency instrumentation**: an optional readout of sample-timestamp to
  frame-presented milliseconds, with a logged histogram, exposed in settings.
- **Canvas-only window**: a borderless window with no panels, intended for the
  Sidecar display, with the main window keeping every panel. `DisplayInfo`
  identifies the Sidecar display where the platform allows, and the user can
  override.

## Annotation and reference layers

Three distinct mechanisms, deliberately not routed through one path:

- **Live overlay**: a transparent, always-on-top, click-through-except-for-stylus
  window covering a display. No screen capture, no permission prompt, lowest
  possible latency. Draw directly over whatever is running.
- **Pinned overlay**: the same window tracking a chosen window's frame, so the
  annotation follows it as it moves and the content beneath can keep updating.
- **Capture to document**: `ScreenCapture` grabs a still of a display, window, or
  region and opens it as a new document with the capture as a locked base layer.
  Needs the screen-recording permission once, requested with an explanation.

**Reference layers** hold their source rather than a flattened raster. A dropped
PDF page is re-rendered at the current zoom so it stays crisp; a dropped raster
image keeps its original pixels and is sampled, never destructively resized.
Annotations live on normal raster layers above. PDF export writes the original
page back with annotations either composited or retained as a separate layer.

## Performance budget

Measured by `make bench`, asserted as tests, recorded in `RUNBOOK.md`. The
reference document is 8192×8192 with 500 layers, 60% of tiles empty.

- Open that document in under 2 s; peak resident memory under the configured
  ceiling (default 4 GiB) at all times, including during a full-canvas composite.
- Switch active layer (worst-case cache rebuild) in under 150 ms.
- Composite and present a 2560×1600 viewport at 1:1 in under 8 ms.
- Stroke pipeline: sustain 10,000 dabs per second into dirty tiles.
- Zoomed-to-fit view of the full 8K canvas in under 16 ms via the pyramid.
- Residency never exceeds the ceiling under an adversarial access pattern of
  random layer switching and random viewport jumps for 10 minutes.

If a budget cannot be met, change the design (tile size, pyramid levels, cache
strategy), record it, and keep the assertion.

## Milestones

A milestone is done when its tests pass, `make check` is green, the commit is
made, and its boxes in `PLAN.md` are ticked. Order front-loads the parts the whole
project depends on and the parts that can invalidate the design.

| Milestone | Build | Done when |
| --- | --- | --- |
| M0 Scaffold | Workspace, crates, Makefile, pinned deps, `PLAN.md`, `ASSUMPTIONS.md`, headless wgpu init, mock platform layer, CI-equivalent `make check` | `make check` passes on an empty workspace; a wgpu device initializes headlessly; first commit |
| M1 Tile store and the budget gate | Tile formats, sparse layers, mmap store with index, compression, residency manager with eviction and prefetch, dirty tracking, synthetic document generator, the benchmark harness | Budget assertions for open time and residency ceiling pass on the 8K/500-layer synthetic document; adversarial access test holds the ceiling |
| M2 Compositor | All blend modes, backdrop/overlay caches, run compilation for backdrop-dependent modes, invalidation, viewport tiling, mip pyramid, CPU reference | Per-pixel parity with the reference across every blend mode and a randomized stack; layer-switch and viewport budgets pass |
| M3 Document and log | Package format, layers, groups, masks, operation log, checkpoints, append-only journal, crash-safe open, undo | Deterministic replay reproduces a document bit-identically; kill-mid-stroke soak leaves every document openable and correct |
| M4 Brush engine | Dab stamping, all parameters, response curves, wet mix, stroke buffer, seeded jitter, brush format, CPU reference, 12 shipped brushes | Parity with the reference rasterizer; identical inputs produce identical pixels across runs; dab throughput budget passes |
| M5 Input | Tablet source trait with mock and macOS implementations, filter chain, stabilization, prediction, pressure curves, auto-calibration, stylus-only routing, latency instrumentation | Recorded sample streams replay identically; calibration fits a known distribution; prediction reconciles without visible seams in the golden stroke tests |
| M6 App shell | Window management, canvas view, pan and zoom, tool and layer panels, brush editor, colour picker, settings, canvas-only window | App runs against a scratch document on the Linux harness; all panels operate; no simulation logic in `sideduck-ui` |
| M7 Selections and transform | Rectangle, ellipse, lasso, magic wand, feather, invert, grow/shrink, move, scale, rotate, flip, free distort, copy/cut/paste to layer | Selection masks match the reference; transform round-trips are lossless at integer steps and resample correctly otherwise |
| M8 Annotation and references | Overlay window trait with mock and macOS implementations, live overlay, pinned overlay, capture-to-document, PDF and image reference layers, export | Overlay modes work against the mock platform; a PDF reference layer re-renders crisply across zoom levels; PDF export preserves the source page |
| M9 Scale and soak | Full-scale validation, memory ceilings under worst case, compaction, long-session soak, performance report | `make soak` passes: 10-minute adversarial session, 500 forced kills, no corruption, no budget breach |
| M10 Ship | `make dist`, `RUNBOOK.md` with Sidecar setup and the macOS build steps, `TUNING.md`, final `ASSUMPTIONS.md` pass, fresh-clone test | Completion checklist fully true; final commit tagged `v0.1.0` |

## Testing requirements

- **Reference parity**: every blend mode, a randomized 50-layer stack, and every
  shipped brush, compared per-pixel against the CPU reference within a stated
  tolerance.
- **Determinism**: identical strokes produce identical pixels; log replay from a
  checkpoint is bit-identical; seeded jitter is stable across runs.
- **Memory**: residency ceiling held under adversarial access; no leak across a
  10-minute soak; tile eviction preserves correctness.
- **Crash safety**: forced termination at randomized points during strokes, layer
  operations, and compaction; every resulting document opens and matches the last
  committed state.
- **Property tests**: tile index round-trips; sparse layers never materialize empty
  tiles; selection operations are closed under invert and intersect; transform
  composition is associative.
- **Golden images** for strokes, stacks, and reference-layer rendering, with a
  documented update procedure.
- **Platform mock coverage**: every `sideduck-platform` trait has a mock used by
  tests, and the macOS implementation is a thin pass-through with no logic beyond
  translation.
- `make check` runs fmt, clippy with warnings denied, the workspace test suite with
  an 80% coverage floor for `sideduck-tile`, `sideduck-composite`, `sideduck-brush`, and
  `sideduck-doc`, a release build, and the budget benchmarks.

## Distribution

No developer account is required for the target workflow, and the spec must not
introduce one.

- `make dist` produces a `.app` bundle built on the user's Mac. Running an
  unsigned app requires the user to allow it in System Settings the first time.
- Signing and notarization, which would remove that step for other people, need a
  paid Apple Developer membership and are out of scope. Document this honestly in
  `RUNBOOK.md` rather than scripting half of it.
- Sidecar setup, display arrangement, the canvas-only window, and the
  screen-recording permission prompt are documented step by step with what the
  user should expect to see.

## Phase n+1

Specified here so the v1 hooks exist. Do not build these.

- **Guide and ruler system.** Guides as first-class document objects in their own
  coordinate space extending well beyond the canvas; multiple guides per document;
  per-layer assignment; types including one-, two- and three-point perspective,
  parallel, concentric circle, curve, and symmetry; snapping as an input-chain
  filter with adjustable magnetism and a modifier to suspend it mid-stroke; guides
  movable, rotatable and scalable after creation. **v1 hooks**: the document model
  reserves a guides collection, and the input filter chain is ordered and
  extensible.
- **Adjustment layers and filters**, as additional compositor passes. **v1 hook**:
  the layer enum and run compilation already distinguish pass types.
- **Canvas re-render at a new resolution**, from the operation log, producing a
  genuinely sharp larger version rather than an upscale.
- **Retroactive brush change**: select strokes by brush, layer, or time range and
  re-render with new parameters.
- **Branch and diff**: fork a document, compare two versions visually, merge a
  layer across. **v1 hook**: operation records are content-addressed.
- **Perspective from a photograph**: line detection and vanishing-point estimation
  to instantiate a matching guide, then a homography to project the photo onto the
  ground plane for traceable texture.
- Text, animation, timelapse export, liquify, PSD interchange.

## Completion checklist

The run ends only when every line is true. Check them yourself; do not report done
on the strength of the milestone commits alone.

- [ ] `SPEC.md`, `AGENTS.md`, `PLAN.md` (all boxes ticked), `ASSUMPTIONS.md`,
      `RUNBOOK.md`, `TUNING.md` exist and are current.
- [ ] `make check` passes on a fresh clone in a temp directory.
- [ ] `make bench` meets every number in the Performance budget, and the measured
      numbers are recorded in `RUNBOOK.md`.
- [ ] `make soak` passes: adversarial access without breaching the residency
      ceiling, and hundreds of forced kills with no corrupt documents.
- [ ] Per-pixel parity with the CPU reference holds for every blend mode and every
      shipped brush.
- [ ] Replaying a document's operation log from a checkpoint reproduces it
      bit-identically, asserted by a test.
- [ ] The 8192×8192, 500-layer synthetic document opens, paints, switches layers,
      and exports without exceeding the memory ceiling.
- [ ] Duck motifs appear only in the surfaces listed in Naming and motifs: no
      crate, type, function, module, log line, error, setting, menu item, or tool
      name references ducks or water, asserted by a grep-based test.
- [ ] No crate above `sideduck-platform` contains `#[cfg(target_os = ...)]`, and
      every platform trait has a mock implementation used by tests.
- [ ] The app builds and runs on the Linux harness against the mock platform, and
      `make dist` is documented for macOS with the unsigned-app caveat stated.
- [ ] `git status` is clean, `Cargo.lock` is committed, the final commit is tagged
      `v0.1.0`, and documents, exports, and build artifacts are gitignored.
- [ ] `RUNBOOK.md` covers: building on macOS, first run and the unsigned-app
      prompt, setting up Sidecar and the canvas-only window, granting screen
      recording for capture mode, pressure calibration, where the tuning knobs
      live, the memory ceiling setting, and the measured benchmark numbers.
