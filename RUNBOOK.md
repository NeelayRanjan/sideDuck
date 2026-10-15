# sideDuck development runbook

This is a working development increment, not the completed release in SPEC.md.
The default editor paints into sparse `.pond` document packages, records strokes
and layer operations, and supports disk-backed undo/redo. The user requested an
early push to conserve credits; unfinished requirements remain in PLAN.md.

## Build and run

Install Rust 1.90.0, Clippy, rustfmt, LLVM tools, Python 3 and the native compiler.
On macOS, install Apple's Command Line Tools with `xcode-select --install` if
needed. `scripts/cargo.sh` uses `.tools/` when present, otherwise Cargo on PATH.
Dependencies download at build time; there is no runtime network or telemetry.

```sh
make setup
./scripts/cargo.sh test --workspace --locked
./scripts/cargo.sh run --release --locked -p sideduck-app -- --document target/my-drawing.pond
make dist
```

`make dist` builds `dist/sideDuck.app` with the original SVG icon rendered to
macOS icon sizes. Open it from Finder. This local build is unsigned and is not
notarized. If macOS blocks it, use System Settings → Privacy & Security → Open
Anyway after inspecting the build. A paid developer account is not required for
local development; signing/notarization for distribution are outside scope.

`--demo` paints a deterministic example into the chosen document. `--smoke-test`
opens native windows and exits after four seconds. Use a disposable document for
these checks. `--gpu-probe` reports the GPU adapter. `--prototype` opens the older
Drawing Lab, whose scratch canvas is only saved by PNG export; its `--config`
and `--exports` arguments apply to that lab.

## Editing and persistence

Use **Document** to create/open packages, import an image or PDF reference, and
export PNG/TIFF/JPEG/PDF. Paths are typed into the panel. Completed strokes and
structural edits commit automatically; **Save** writes a checkpoint. Undo/redo
restore recorded state and pixels. Input samples, full brush settings and seeds
are stored for deterministic replay. Prediction samples are never committed.

The canvas supports scroll zoom and middle-drag/Space-drag pan. The layer panel
provides visibility, locking, opacity, blend modes, ordering and groups. Selection
tools include rectangle, ellipse, lasso and wand; feather, invert, grow/shrink,
copy/cut/paste and transforms are connected. Merge currently supports normal,
unmasked, unclipped raster siblings only and rejects unsupported combinations.

Twelve editable `.quill` brush packages ship with the app. **Edit brush** exposes
shape/grain, dynamics and response curves. **Settings** includes stabilization,
global and per-brush pressure curves, and calibration from recent strokes.
Settings and brush edits persist separately from document stroke records.
See TUNING.md for parameters, formats and the calibration procedure.

Reference layers retain original sources. For one eligible base PDF reference, PDF export preserves original page
content and adds annotations in an optional layer. Mixed or transformed reference
stacks export a flattened PDF so other references are not omitted. PNG/TIFF retain transparency;
JPEG composites onto white in linear light. Existing export paths are rejected; choose a new file name. Export also refuses
to write inside a document package. New files publish atomically without overwriting. Keep the `.pond` package for editable history.

## Sidecar and annotation

1. Connect the iPad through the Mac's Screen Mirroring menu as an extended display.
   Arrange screens in System Settings → Displays.
2. Keep the control window on the Mac. Open **Canvas window** and move it to the
   iPad, using the display selector when available. Enable **Full screen** if useful.
3. Leave **Paint with mouse** off for stylus-only painting. Test light/firm pressure,
   tilt, canvas edges, pen lifts, window changes and display scale changes.
4. Record hardware/OS versions and any offset or missing input. Physical Apple
   Pencil/Sidecar acceptance has NOT been demonstrated by automated smoke tests.

Native APIs provide live display overlays, pinned window overlays, display/window
listing and capture. Settings has explicit overlay Draw/Click-through modes:
macOS window-wide mouse ignoring cannot simultaneously pass a physical mouse
through while intercepting Pencil. Pinned overlays follow a selected window.
Drawing overlays do not capture the desktop or request Screen Recording access.

**Capture to document** explains Screen Recording permission and requires an
explicit capture confirmation before requesting it. If denied, grant access in
System Settings → Privacy & Security → Screen Recording and retry. Captures become
a locked base layer with a paint layer above. Actual permission dialogs and user
desktop capture were not exercised during automated validation.

## Checks and known limits

```sh
make check       # fmt, Clippy, tests, per-crate coverage, release, all budget gates
make bench       # brush, storage, native presentation and fit-view gates
make soak        # ten-minute storage access gate; NOT yet a 500-kill document soak
make coverage
```

GPU tests require an accessible Metal/Vulkan adapter. Linux mock CI is configured
in `.github/workflows/ci.yml` but has not yet been run for this increment.
`make check` is NOT currently claimed passing: full 500-layer compositor timing
gates remain unmet. Assertions are retained rather than relaxed.

Other open limits: the editor uses a bounded CPU preview (currently capped at
1024 pixels per side), the production GPU renderer is not connected to editor
painting, full-scale paint/export integration is unproven, exact presentation
latency is unavailable, and free-distort UI needs completion. Reference rendering
is cached within 128 MiB; very large full-resolution reference exports can hit
memory bounds. Display P3 math is tested, but native P3 output is not certified.

Default document tile cache is 64 MiB; editor paint, preview, selection and export
have explicit internal bounds. The storage harness accepts `--cache-mib` and
`--ceiling-mib` (default process ceiling 4096 MiB). A unified runtime memory-setting
panel remains pending. Bounds reject operations instead of silently dropping data.

Async prefetch is bounded to 32 keys/8 workers with cancellation and stale-version
checks, but app scheduling remains pending. Compact-copy verifies live blobs into
a new standalone store. It does not replace a `.pond` store: checkpoint offsets
and journal hashes must remain valid. In-place document compaction is unfinished.

## Measurements and evidence

Host: macOS 15.3.1, arm64 Apple M4 Pro/Metal. Hardware results are specific to this
host. The 8K/500-layer storage corpus has 204,800 occupied tiles, 60% empty slots,
and 50 GiB logical RGBA8 data. Seeded painting-like marks are deliberately highly
compressible; this is not random-noise or photographic worst-case data. Disk cache
is warm/uncontrolled, even for fresh-process open measurements.

- Previously completed M1 ten-minute run: 600.000398 s, 5,379,191 reads, 188,490
  edits, 1,319,424 viewport jumps and 5,226,692 evictions. All 64 edited probe tiles
  survived commit/reopen. Index open 18.31 ms; peak process residency 85.84 MiB
  under 4 GiB, with a 64 MiB decoded cache. Evidence: validation/ storage JSONs.
- CPU brush gate: approximately 772,018 dabs/s for the documented 4 px technical
  pen workload; this is not a claim for every brush diameter. All twelve brushes
  have reference parity and deterministic replay tests. Evidence:
  `validation/brush-macos-arm64.json`.
- Latest full 500-layer switch: 526.950 ms worst, above 150 ms; process peak
  675.1 MiB. Full native processing timing reached 13.562 ms, above 8 ms.
  End-to-end presentation is recorded separately and also remains open. Earlier
  isolated measurements in `validation/native-presentation-macos-arm64.json` do
  not establish a passing full-corpus gate.
- Document tests cover torn tails, exact replay, undo/redo and forced termination
  at five commit boundaries. Tile compaction tests cover five publication failure
  boundaries and abrupt child exits. The required 500-kill soak remains pending.
- The document-backed native editor smoke test passed with saved painted content.
  Computer Use permission was unavailable, so visual desktop inspection and real
  Sidecar behavior remain unverified.

Final stabilization checks: all 204 workspace tests passed with native Metal
access; workspace formatting and all-target Clippy with warnings denied passed.
The full coverage/performance/fresh-clone suite was not repeated for this early
push; the failed performance gates and incomplete release checklist remain open.
