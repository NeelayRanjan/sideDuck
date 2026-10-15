# Prompts

Two prompts. Paste the first one to start the build. Paste the second only if the
agent comes back in a fresh session after a usage reset.

## Start

```
Build sideDuck, the project specified in this repo. Read AGENTS.md and SPEC.md in
full before doing anything else, then build it end to end without asking me a
single question.

AGENTS.md is binding. The rules that matter most: everything is Rust and
everything must build and run on this machine, with macOS-only code confined to
sideduck-platform behind traits that have mock implementations; the memory and
latency numbers in the Performance budget are test assertions, not reports;
correctness is measured per-pixel against the CPU reference implementations; and
brush rendering must be deterministic, because the operation log depends on it.

M1 is a gate. Build the tile store and the benchmark harness before anything that
sits on top of them, and do not move to M2 until the budget assertions pass on the
8192x8192, 500-layer synthetic document. If they can't pass, change the design and
record what you changed.

Record every decision in ASSUMPTIONS.md. One milestone at a time with a commit at
the end of each. Tests with every feature.

Start at M0 by writing PLAN.md with a checkbox for every task in every milestone,
then work straight through M0 to M10. Keep PLAN.md ticked as you go so a fresh
session can resume from it. Do not stop, do not report back for approval, and do
not declare the build finished until every line of the Completion checklist in
SPEC.md is true.
```

## Resume

```
You are resuming an unattended build of sideDuck. Do not ask me anything.

1. Read AGENTS.md, SPEC.md, PLAN.md, and ASSUMPTIONS.md in full, then run
   `git log --oneline` and `git status`.
2. If the working tree is dirty, finish or revert the in-progress change so
   `make check` passes, then commit.
3. Find the first unticked task in PLAN.md and continue from there, following
   every rule in AGENTS.md.
4. Keep going, milestone by milestone, until every item in the Completion
   checklist in SPEC.md is true. Then tag v0.1.0.
```
