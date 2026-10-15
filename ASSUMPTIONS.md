# Decisions

- The current request is the first working increment: macOS/Sidecar drawing prototype and M1 tile-store gate; the full v1 checklist remains a roadmap, not a claim of completion.
- The host is macOS arm64, contrary to the original Linux-host assumption; build and test the real native path here and retain portable mocks.
- Use Rust 1.90.0 with a workspace-local toolchain when no system toolchain exists; this leaves global developer settings unchanged.
- Dependencies are pinned and Cargo.lock is committed; dependency versions are selected for compatibility, not claimed to be the newest.
- Keep the specified sideduck-* crate names; the motif prohibition concerns invented duck-themed technical names, not the explicitly mandated crate prefix.
- Process resident memory and decoded tile-cache residency are measured separately; the benchmark must assert both and state dataset entropy and cache conditions.

## Deviations

- The user explicitly requested an early macOS/Sidecar prototype alongside M1, so a narrow slice of M5/M6 is built before the full compositor and document model.
- Physical Apple Pencil/Sidecar behavior cannot be certified by mocks or a desktop launch; the runbook carries an explicit device acceptance checklist.
- M0 checks cover the scaffold; M1 adds its budget gate to make check. Unimplemented compositor/brush latency targets and future document/compositor coverage floors remain pending and will not be reported as passed.


- Prototype painting uses a fixed-size white CPU raster while the tile gate exercises a separate store; connecting the two requires the compositor/document milestones and is not implied by the current UI.
- Pressure/tilt come from AppKit tablet events; azimuth is derived from the tilt vector, not a separate native Pencil angle.
- Native event coordinates are converted from content-view points into egui coordinates, including UI zoom; contacts end when geometry or input windows change.
- Tile prefetch is initially synchronous, with explicitly bounded requested keys; asynchronous prefetch and compaction remain pending.
- The storage fixture is deliberately compressible but contains independently generated distinct tiles; results identify this and do not stand for high-entropy photos.
- Open-time measurement is a new process with the filesystem cache uncontrolled; it is not called a cold-disk result.
- make soak currently exercises the ten-minute M1 residency workload; the 500-kill M9 crash soak is not implemented.
- Coverage enforces separate 80% production-source line floors for the implemented tile, prototype brush, and input crates; test bodies, native glue/UI, and milestone placeholders are excluded explicitly.
- No project license has been chosen on the owner's behalf; package license fields remain absent until the owner selects one.
