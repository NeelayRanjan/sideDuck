# Storage budget gate

This executable enforces the M1 storage budgets. It exits unsuccessfully on a
timing, residency, corpus-size, or pixel-persistence failure. Other milestones'
compositing and brush budgets are explicitly marked deferred in its JSON report.

```sh
./scripts/cargo.sh run --release -p sideduck-bench -- --seconds 2
./scripts/cargo.sh run --release -p sideduck-bench -- --seconds 600 --results target/soak-results.json
```

Both commands use the full 8192 × 8192 canvas with 500 layers and 256 × 256 RGBA8
tiles. Exactly 204,800 of 512,000 possible tile slots exist: 60% are empty and
the occupied tiles represent 50 GiB of uncompressed pixels. Every occupied tile
has independently seeded geometry and colors, with transparent areas and eighteen
overlapping colored marks. Small opaque stamps encode tile identity and the
revision used to verify dirty writes. The corpus is deliberately compressible;
it does not characterize incompressible images or worst-case disk throughput.

Generation streams one tile at a time through a bounded cache. A versioned,
seeded corpus under `target/storage-gate` is reused after successful generation.
Interrupted generation is rebuilt only when its ownership marker matches.
`--root PATH` selects another fixture root. Gates hold an OS file lock so that
concurrent invocations require separate roots.

The parent generates the corpus and then launches a new process. That process
must open the index in strictly less than two seconds without loading decoded
tiles. The OS filesystem cache is uncontrolled and normally warm after generation;
this is **not a cold-disk open measurement**. The timer includes opening and
validating the store, and excludes generation and application startup.

During random layer switches and viewport jumps, the harness reads occupied tiles,
checks their identity and revision, compares full pixels periodically, and performs
dirty edits on a bounded set of 64 probe tiles. It requires actual eviction.
All probe tiles and 64 additional random tiles are compared in full after commit
and reopen. Probe tiles are reset on the next run, including after interruption.

The process ceiling defaults to 4096 MiB, independently of the 64 MiB decoded-tile
cache. The harness checks current and lifetime-peak resident memory reported by
the operating system and checks both cache counters on every access. A ceiling
breach is an error. Generation also enforces these ceilings; its parent process
peak is recorded separately. When a fixture is reused, that field covers only
fixture preparation in this run, not its original generation. `--cache-mib` and
`--ceiling-mib` override the limits, and
`--seed` accepts an unsigned decimal integer for reproducible data and access.

`--seconds 600` runs the adversarial loop for at least ten minutes. Short runs are
useful checks but never claim that the ten-minute criterion was exercised.
`target/gate-results.json` records the latest successful run by default, including
seed, OS, architecture, logical CPU count, corpus sizes, timing, process memory,
evictions, reads, writes, and full pixel comparisons. `--results PATH` selects a
separate report. The gate uses a direct executable rather than statistical
microbenchmark averages because the requirements are strict end-to-end ceilings.
