# Build plan

Current task: final ten-minute M1 residency validation, coverage check and clean-checkout verification.

## M0 Scaffold
- [x] Rust workspace and pinned dependencies; all specified crate boundaries.
- [x] Build commands, headless GPU initialization, platform mock, and initial checks.
- [x] Record assumptions and commit passing scaffold.

## M1 Tile store and performance gate
- [x] Tile geometry, RGBA8/RGBA16F, sparse index, append-only mmap store, compression.
- [x] Budgeted cache, eviction, pinning, prefetch, dirty tracking and persistence tests.
- [x] Streaming 8K/500-layer synthetic document and fresh-process open-time check.
- [ ] Assert process memory and cache budgets during ten-minute adversarial access.
- [ ] Document measured results, limitations, and commit passing increment.

## Early macOS/Sidecar prototype (requested slice of M5/M6)
- [x] Native tablet point/proximity input with pressure/tilt and portable replay mock.
- [x] Deterministic pressure-driven drawing and input-routing tests.
- [x] Separate canvas/control windows, explicit mouse fallback, telemetry, clear/export.
- [x] Native build, desktop smoke check, and physical Sidecar acceptance instructions.

## M2 Compositor
- [ ] Blend modes and scalar CPU reference.
- [ ] Backdrop/overlay runs, invalidation, viewport tiles and mip pyramid.
- [ ] Per-pixel randomized parity and layer-switch/viewport performance gates.

## M3 Document and log
- [ ] Package format, layers, groups, masks and structural operations.
- [ ] Deterministic operation log, checkpoints, journal and disk undo.
- [ ] Replay tests and forced-termination crash recovery.

## M4 Brush engine
- [ ] Full dab parameters, texture/grain, wet mix and response curves.
- [ ] Stroke-level compositing, seeded jitter and reference rasterizer.
- [ ] Open brush format, twelve brushes and throughput/parity gates.

## M5 Complete input system
- [ ] Stabilization, prediction and extensible filter chain.
- [ ] Pressure curve editor, automatic calibration and latency histograms.
- [ ] Recorded stream/golden reconciliation tests and real Sidecar verification.

## M6 Complete app shell
- [ ] Document lifecycle, pan/zoom, layer/tool panels and settings.
- [ ] Brush editor, colour management and canvas-only display routing.
- [ ] Linux mock harness and working panel tests.

## M7 Selections and transforms
- [ ] Selection tools, masks, feather, invert and grow/shrink.
- [ ] Transform tools and copy/cut/paste with reference/property tests.

## M8 Annotation and references
- [ ] Native and mock live/pinned overlays plus capture permission flow.
- [ ] PDF/image source layers, zoom rendering and export preservation.

## M9 Scale and soak
- [ ] Full-render memory budgets, compaction and long-session validation.
- [ ] Five hundred forced kills and final performance report.

## M10 Ship
- [ ] Complete distribution, icon, runbook, tuning guide and assumption audit.
- [ ] Fresh-clone check, full specification checklist, clean tree and v0.1.0 tag.
