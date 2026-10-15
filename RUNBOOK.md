# sideDuck prototype runbook

This increment delivers the M0 workspace, an M1 storage budget gate, and an early
macOS/Sidecar drawing prototype. It is not the full application in SPEC.md.
The fixed-size scratch canvas uses a scalar CPU rasterizer and a GPU display
texture; it is not yet wired to the sparse multi-layer document store. Quit
without exporting and that drawing is discarded. There is no undo yet.

## Build and run

Install Rust 1.90.0 (rust-toolchain.toml), Clippy, rustfmt, and the platform's native
compiler. On macOS install Apple's Command Line Tools with `xcode-select --install`
if necessary. The `scripts/cargo.sh` wrapper uses a workspace-local toolchain in
`.tools/` when available, otherwise the normal `cargo` on PATH. Build-time network
access is needed to download crates; the application has no runtime network.

```sh
make setup
make check
make run
```

`make check` runs formatting, Clippy with denied warnings, tests, release builds,
and the short storage gate. A real headless GPU is required by the GPU probe;
GPU-denying sandboxes will correctly fail it. The prototype uses wgpu/Metal on
macOS. The Linux mock path is present but this increment is validated on macOS,
not certified on Linux. Full v1 coverage and compositor/brush performance gates
remain open in PLAN.md.

`make dist` creates `dist/sideDuck.app`. Open it from Finder, or run
`open dist/sideDuck.app`. This is a local developer build, without Developer ID
signing or notarization. If macOS blocks this local build, inspect it and use
Privacy & Security → Open Anyway as appropriate; no developer account is needed
for building locally. Do not assume this is a distributable release.

Useful commands:

```sh
./scripts/cargo.sh run --release --locked -p sideduck-app -- --gpu-probe
./scripts/cargo.sh run --release --locked -p sideduck-app -- --demo
./scripts/cargo.sh run --release --locked -p sideduck-app -- --smoke-test
./scripts/cargo.sh run --release --locked -p sideduck-app -- --config config/prototype.json
```

`--demo` paints deterministic example curves; they are not evidence of real tablet
input. `--smoke-test` opens both native windows and exits after four seconds.
Default PNG exports go to `~/Pictures/sideDuck Exports`; override with `--exports
DIRECTORY`. Export creates a new file and never overwrites an existing one.

## Sidecar acceptance on real hardware (pending)

1. Connect the iPad using the Mac's Screen Mirroring menu and use it as an extended
   display. Arrange the displays in System Settings → Displays.
2. Launch the prototype. Keep **Drawing Lab** on the Mac. Move **sideDuck Canvas**
   to the iPad, then optionally enable **Canvas full screen**. Escape exits full
   screen. Window placement is manual; automatic Sidecar identification is pending.
3. Leave **Allow mouse drawing** off. Draw using Apple Pencil. The input panel must
   change from waiting to **Tablet events received**, and the sample count must
   rise. A mouse or trackpad must not paint in this mode.
4. Draw light and firm strokes. Native pressure must vary and the radius must
   follow it. Tilt X/Y and derived azimuth should vary when the stylus tilts.
   Eraser/barrel fields depend on device capabilities and are not promised for
   Apple Pencil. The explicit Eraser control is always available.
5. Lift, start a new stroke, move across the canvas edge, switch windows and return:
   separate strokes must not become joined. Try different display scale settings
   and full-screen transitions; the mark must remain under the Pencil.
6. Export PNG and inspect it. Close/reopen the canvas using **Open canvas**; the
   scratch drawing should remain while the control window stays open.
7. Record Mac/iPad models, macOS/iPadOS versions, wired/wireless connection, pressure
   range, tilt behavior, lost samples and visible offset. This checklist has NOT
   been passed by the automated tests or desktop smoke test.

The prototype monitors application-local AppKit tablet events, including mouse
messages with tablet subtypes. It does not use system-wide event taps, screen
capture, or require Input Monitoring/Screen Recording permissions. Live overlays,
pinned overlays and screenshot capture are not implemented. When capture lands,
its screen-recording permission flow must be tested separately.

The pressure curve slider changes an exponent; automatic pressure calibration,
stabilization and prediction are later milestones. Raw telemetry shows the
uncurved input. No end-to-display latency claim is made.

## Storage budget gate

```sh
make bench  # full corpus, fresh-process open and short adversarial session
make soak   # full corpus, ten-minute adversarial session
```

The corpus is 8192×8192, 500 layers, 256×256 RGBA8 tiles. Exactly 204,800 of 512,000
slots are occupied: 50 GiB of logical pixel data. Every occupied tile is generated
independently with seeded colored marks on transparent pixels and an identity
stamp. This is highly compressible painting-like content, not worst-case random
noise. The generator streams through a 64 MiB decoded tile cache. The fixture is
reused under `target/storage-gate`; generated files are ignored by git.

The gate asserts index-only open under 2 seconds in a **fresh process**, peak
process residency under 4 GiB, and decoded cache residency within its independent
budget. It samples current RSS and checks the kernel's peak RSS, so transient
process memory is covered. OS disk cache is uncontrolled and usually warm;
these results are not cold-disk latency. The process memory gate includes index,
scratch, mmap pages and allocator overhead, not just decoded tile accounting.

Random access alternates layers and viewport regions, verifies pixels, edits
known tiles, forces eviction, commits, reopens and verifies edits. Any failed
assertion exits nonzero. `--cache-mib`, `--ceiling-mib`, `--seed`, `--root` and
`--results` are supported; use `--help`. Concurrent runs require separate `--root`
directories because each run modifies the same bounded set of probe tiles.

Measured results are recorded below after validation. JSON artifacts include seed,
architecture, corpus size, cache budget, actual RSS, open time and access counts.
The short gate writes `target/gate-results.json`; the long gate writes
`target/soak-results.json`.

This is the **tile-store gate only**. Full-canvas composite memory, layer-cache
rebuild latency, viewport presentation, zoom pyramid, 10,000 dabs/s, compaction,
500 forced kills and M3 operation-log recovery are pending. The short gate is not
a replacement for the ten-minute run.

## Persistence contract and limits

See `crates/sideduck-tile/README.md` for the API. Dirty tile data is appended before
eviction. Explicit `commit()` synchronizes data, atomically publishes a
checksummed index, and synchronizes its directory. Reopening sees the last
published index; uncommitted blobs are ignored. Store files are exclusively
locked. CRC failures and malformed indices return errors. Tests cover interrupted
publication/tail data, but do not constitute the future process-kill soak.

Prefetch is synchronous for this first increment. Compaction and asynchronous
prefetch are pending. `.pond` documents and `.quill` brushes are specified formats,
not implemented save formats in this prototype. Tile stores are internal test
directories, not complete document packages.

## Measurements

Validated on macOS 15.3.1, arm64, Apple M4 Pro (Metal), 2026-10-14.

- `make check GATE_ROOT=target/storage-gate-check`: passed, including 48 tests,
  Clippy with denied warnings, formatting, a release build and the short gate.
- Freshly generated 50 GiB logical corpus: 354.81 MiB initial disk size,
  generation 14.82 s; fresh-process index open 0.0129 s; process peak 88.7 MiB,
  below 4 GiB, with a 64 MiB decoded tile cache.
- Native `--smoke-test`: successful startup and clean automatic exit;
  headless adapter reported `Apple M4 Pro (Metal)`.
- Automated desktop clicking/visual inspection was unavailable because Computer
  Use permissions were not granted. Physical Sidecar/Pencil acceptance is pending.
- Final ten-minute gate and clean-checkout results will be recorded here after completion.
