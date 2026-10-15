# Decisions

- The user has now requested completion of the entire v1 specification. The earlier prototype and M1 gate are the starting point; all remaining milestones stay open until their criteria are actually demonstrated.
- The host is macOS arm64, contrary to the original Linux-host assumption; build and test the real native path here and retain portable mocks.
- Use Rust 1.90.0 with a workspace-local toolchain when no system toolchain exists; this leaves global developer settings unchanged.
- Dependencies are pinned and Cargo.lock is committed; dependency versions are selected for compatibility, not claimed to be the newest.
- Keep the specified sideduck-* crate names; the motif prohibition concerns invented duck-themed technical names, not the explicitly mandated crate prefix.
- Process resident memory and decoded tile-cache residency are measured separately; the benchmark must assert both and state dataset entropy and cache conditions.
- Add and subtract use bounded straight-color addition/subtraction followed by premultiplied source-over; nonseparable modes use W3C luminosity weights in the linear working space.
- RGBA8 and RGBA16F working tiles contain linear-sRGB premultiplied components; image import/export performs the transfer function explicitly.
- Input prediction is provisional and never enters committed stroke samples; calibration fits trimmed empirical pressure quantiles. Latency tracks the application's presentation notification, not physical scanout.
- Driven stroke opacity uses the maximum touched-dab opacity at each pixel, applied once against the preimage; the brush engine's throughput fixture states its technical-pen size instead of generalizing to every brush diameter.

## Deviations

- On 2026-10-14 the user reduced delivery scope to fixing glaring issues and pushing the current increment to conserve credits; unfinished gates remain explicit, and no final release tag will be created.
- Pass-through groups blend children against the parent backdrop and interpolate group opacity/masks; groups inside atomic clipping runs or with a non-normal group blend mode are isolated.


- The user's instruction to keep moving overrides strict milestone sequencing: independent document, brush, and input work proceeds in parallel while failed compositor performance gates stay open and remain required before release.
- The user explicitly authorized deferring prolonged blockers until the end; unfinished gates and integration gaps stay listed in PLAN.md while independent features continue.
- Native presentation reports both GPU/host processing and complete elapsed wall time. The 8/16 ms processing assertions exclude display scheduling and timestamp readback; the original end-to-end target is reported independently and is not yet met on this host.
- Switching the active layer generally changes both stack partitions; exact dependency keys and reusable range caches preserve correctness instead of claiming one arbitrary partition remains valid.

- The user explicitly requested an early macOS/Sidecar prototype alongside M1, so a narrow slice of M5/M6 is built before the full compositor and document model.
- Physical Apple Pencil/Sidecar behavior cannot be certified by mocks or a desktop launch; the runbook carries an explicit device acceptance checklist.
- M0 checks cover the scaffold; M1 adds its budget gate to make check. Unimplemented compositor/brush latency targets and future document/compositor coverage floors remain pending and will not be reported as passed.


- The legacy --prototype drawing lab remains a scratch canvas; the default editor now paints into journaled sparse documents. Its CPU preview is not the production GPU performance path.
- Pressure/tilt come from AppKit tablet events; azimuth is derived from the tilt vector, not a separate native Pencil angle.
- Native event coordinates are converted from content-view points into egui coordinates, including UI zoom; contacts end when geometry or input windows change.
- Tile prefetch now supports bounded async workers. Verified compact-copy creates an independent store; replacing a document store remains pending because retained checkpoint offsets must stay valid.
- The storage fixture is deliberately compressible but contains independently generated distinct tiles; results identify this and do not stand for high-entropy photos.
- Open-time measurement is a new process with the filesystem cache uncontrolled; it is not called a cold-disk result.
- make soak currently exercises the ten-minute M1 residency workload; the 500-kill M9 crash soak is not implemented.
- Coverage scripts enforce separate 80% production-source line floors for tile, compositor, brush, input, and document crates; test bodies and native glue/UI are excluded. Current full-run status is recorded in RUNBOOK.md.
- No project license has been chosen on the owner's behalf; package license fields remain absent until the owner selects one.
