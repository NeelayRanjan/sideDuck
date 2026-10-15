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
- M0 checks cover the scaffold; M1 adds its budget gate to make check. Unimplemented compositor/brush latency targets and coverage floors remain pending and will not be reported as passed.

