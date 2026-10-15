# Operating rules

These rules govern the whole build of `sideDuck`. `SPEC.md` is the specification;
this file is how you work. Read both in full before your first action, and re-read
both after any restart or loss of context.

1. Never ask the user a question. When the spec is silent or ambiguous, decide,
   implement, and record the decision with a one-line rationale in `ASSUMPTIONS.md`.
   When something in the spec is impossible as written, implement the closest
   feasible version and record it under a "Deviations" heading in the same file.
   Never stop early for any reason other than the Completion checklist being
   fully satisfied.
2. **Everything is Rust and everything is buildable here.** You are building on
   Linux; the product ships on macOS. Platform-specific code lives behind traits
   in `sideduck-platform`, with a real macOS implementation (`objc2`, compiled only
   under `#[cfg(target_os = "macos")]`) and a mock implementation used by the test
   suite and the Linux build. No Swift, no Xcode project, no Objective-C source
   files. Any macOS-only code you cannot run is thin, marked unverified in its
   module header and in `RUNBOOK.md`, and contains no logic that could have lived
   in a portable crate.
3. **Memory budgets are correctness properties.** Every number in the Performance
   budget section is a test with a hard assertion, not a report. A build that
   exceeds its residency ceiling fails even if it renders correctly.
4. **Correctness is measured against a CPU reference.** The reference compositor
   and the reference brush rasterizer are simple, obviously correct, and slow.
   Every GPU path is diffed against them per-pixel. When they disagree, the
   reference is right.
5. **Brush rendering is deterministic.** Identical input samples, brush
   parameters, and seed produce bit-identical output on a given platform. The
   operation log depends on this, so a non-deterministic brush path is a bug even
   if it looks fine.
6. This is an original application. Do not copy Procreate's or Clip Studio's
   interface, icons, naming, brush formats, or document formats, do not put either
   product's name in any user-facing string or identifier, and do not read or
   write their proprietary files.
   Duck motifs are confined to the surfaces the Naming and motifs section of
   `SPEC.md` lists, and never appear in identifiers, log messages, or UI labels
   that describe what a control does.
7. Work milestone by milestone, in order. Do not start milestone N+1 until
   `make check` passes at milestone N. Commit at the end of each milestone with
   the message `M<n>: <summary>`. Commit more often if useful.
8. Keep `PLAN.md` current: one checkbox per task under each milestone, ticked as
   you go, with a "Current task" line at the top. On any restart or loss of
   context, read `AGENTS.md`, `SPEC.md`, `PLAN.md`, `ASSUMPTIONS.md`, and
   `git log --oneline` before doing anything else.
9. Every feature ships with tests. Where the spec gives an acceptance criterion,
   there is a test whose name contains that criterion.
10. You cannot judge whether the brushes feel good, and must not claim to. Your
    criteria are the mechanical ones: parity with the reference, determinism,
    latency and memory budgets, crash safety. Every brush and input constant lives
    in one documented config file so the user can tune feel afterward.
11. No runtime network, no telemetry, no accounts, no cloud. Documents are local
    files. Build-time crate installs are fine.
12. Prefer boring, widely used crates, pinned versions, and as few dependencies as
    the job allows. Commit `Cargo.lock`.
13. Quality bar at every milestone: `cargo fmt --check` clean, `cargo clippy
    --all-targets -- -D warnings` clean, `cargo test --workspace` green, release
    build succeeds, benchmarks within budget.
14. Documentation is for running and extending the thing. `README.md` stays a
    short pointer to `RUNBOOK.md` once that exists.
15. If a milestone is taking far longer than its neighbors, cut scope within it
    (fewer blend modes, fewer shipped brushes, plainer panels), note the cut in
    `ASSUMPTIONS.md`, and move on. Never cut reference parity, the memory ceiling
    tests, determinism, or crash safety to save time.
