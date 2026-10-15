# Build plan

Current task: stabilize and push the implemented editor increment. The user explicitly reduced scope on 2026-10-14 because of credit limits: fix glaring issues, run focused checks, commit and push now. Unfinished specification items below remain open; this is not the completed v0.1.0 release.

Delivery: the user authorized committing and pushing the completed, validated build to GitHub.

## Deferred validation and integration
- [ ] Bring the full 500-layer worst-case switch under 150 ms; latest parallel/reuse run is 526.950 ms worst, with process peak 675.1 MiB. Retain the hard assertion.
- [ ] Resolve/report the original native composite-and-present wall-time targets separately from GPU processing. Full-corpus run also missed processing (13.562 ms maximum); earlier isolated timing is not a full gate pass.
- [ ] Complete fit-view performance, full workload soak, crash kills, Linux harness and fresh-clone release checks.
- [ ] Broaden merge beyond normal unmasked raster siblings. Independent stroke re-rasterization from recorded inputs is tested and passing.
- [ ] Native selective click-through for physical mouse while accepting Pencil remains a platform limitation; explicit draw/click-through overlay modes are implemented separately.

## M0 Scaffold
- [x] Rust workspace and pinned dependencies; all specified crate boundaries.
- [x] Build commands, headless GPU initialization, platform mock, and initial checks.
- [x] Record assumptions and commit passing scaffold.

## M1 Tile store and performance gate
- [x] Tile geometry, RGBA8/RGBA16F, sparse index, append-only mmap store, compression.
- [x] Budgeted cache, eviction, pinning, prefetch, dirty tracking and persistence tests.
- [x] Streaming 8K/500-layer synthetic document and fresh-process open-time check.
- [x] Assert process memory and cache budgets during ten-minute adversarial access.
- [x] Document measured results, limitations, and commit passing increment.

## Early macOS/Sidecar prototype (requested slice of M5/M6)
- [x] Native tablet point/proximity input with pressure/tilt and portable replay mock.
- [x] Deterministic pressure-driven drawing and input-routing tests.
- [x] Separate canvas/control windows, explicit mouse fallback, telemetry, clear/export.
- [x] Native build, desktop smoke check, and physical Sidecar acceptance instructions.

## M2 Compositor
- [x] Blend modes and scalar CPU reference.
- [x] Backdrop/overlay runs, invalidation, viewport tiles and mip pyramid.
- [ ] Per-pixel randomized parity and layer-switch/viewport performance gates.

## M3 Document and log
- [x] Package format, layers, groups, masks and structural operations.
- [x] Content-addressed operation log, checkpoints, journal and disk undo.
- [x] Replay tests and recovery at five forced-termination commit boundaries (500-kill soak remains M9).

## M4 Brush engine
- [x] Full dab parameters, texture/grain, wet mix and response curves.
- [x] Stroke-level compositing, seeded jitter and reference rasterizer.
- [x] Open brush format, twelve brushes and throughput/parity gates.

## M5 Complete input system
- [x] Stabilization, prediction and extensible filter chain.
- [x] Pressure curve editor and automatic calibration; global/per-brush settings persist.
- [ ] Actual presentation latency histograms; physical Sidecar verification.
- [ ] Recorded stream/golden reconciliation tests and real Sidecar verification.

## M6 Complete app shell
- [ ] Document lifecycle, pan/zoom, layer/tool panels and settings.
- [ ] Brush editor, colour management and canvas-only display routing.
- [ ] Linux mock harness and working panel tests.

## M7 Selections and transforms
- [x] Selection algorithms, masks, feather, invert and grow/shrink (UI wiring remains part of M6).
- [x] Transform core and copy/cut/paste with reference/property tests; editing commands are wired.
- [ ] Complete free-distort handles and broader merge behavior.

## M8 Annotation and references
- [x] Native and mock live/pinned overlay services and capture permission UI; native user permission flow remains unverified.
- [x] PDF/image source layers, cached zoom rendering and tested single-base-PDF export preservation. Mixed reference stacks export flattened PDFs.

## M9 Scale and soak
- [x] Bounded asynchronous tile prefetch and verified standalone compact-copy with crash tests.
- [ ] App prefetch wiring, in-place document compaction, full-render memory budgets and long-session validation.
- [ ] Five hundred forced kills and final performance report.

## M10 Ship
- [ ] Complete distribution, icon, runbook, tuning guide and assumption audit.
- [ ] Fresh-clone check, full specification checklist, clean tree and v0.1.0 tag.
