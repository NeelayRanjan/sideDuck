# sideDuck

A macOS painting application in development, using an iPad over Sidecar as its
drawing surface and sparse tiles to support large layer counts.

The first increment contains a native drawing prototype and a tested disk-backed
tile store with an 8K/500-layer memory gate. It is not yet the full editor.

- [RUNBOOK.md](RUNBOOK.md): build, launch, benchmark and real Sidecar verification.
- [TUNING.md](TUNING.md): prototype brush/input settings.
- [PLAN.md](PLAN.md): implementation status and remaining milestones.
- [SPEC.md](SPEC.md): full product specification.
- [ASSUMPTIONS.md](ASSUMPTIONS.md): decisions and explicit limitations.

```sh
make run    # Drawing Lab + separate canvas window
make check  # lint, tests, release build and short tile-store gate
make soak   # ten-minute tile-store residency gate
make dist   # local macOS prototype bundle
```
