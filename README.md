# sideDuck

A macOS painting and annotation app with no practical layer limit, drawn on an
iPad over Sidecar. Documents are `.pond` packages and brushes are `.quill`
packages, which is as far as the duck goes outside the icon.

The usual iPad painting app ties its layer ceiling to canvas size, because every
layer is a fully resident bitmap and the device has a few gigabytes to spend: a
4096×4096 layer is 64 MiB at 8-bit and 128 MiB at 16-bit, so a big canvas runs out
of layers fast. `sideDuck` attacks that three ways. It runs on a Mac, which has far
more memory. It stores layers as sparse 256×256 tiles, so a layer with twenty
painted tiles costs 5 MiB instead of 64. And it composites everything below the
active layer into one cached texture and everything above into another, so the
work per frame barely depends on how many layers exist.

The iPad becomes the drawing surface over Sidecar — free, built into macOS, no
developer account, no sideloading, and it does deliver Apple Pencil pressure and
tilt to apps that implement tablet-event handling, which this one does. The Mac
screen holds every panel; the iPad holds nothing but canvas, edge to edge. A
tablet-only app can't do that.

Beyond painting: a transparent always-on-top overlay for drawing over anything on
screen, a variant pinned to a single window, screenshot capture into a document,
and dropped PDFs and images kept as live reference layers that re-render crisply
as you zoom instead of being flattened.

Documents are stored as an operation log with raster checkpoints rather than only
pixels. That costs some disk now and buys a lot later: re-rendering a finished
painting at a higher resolution for real, changing a brush after the fact across
every stroke that used it, and diffing two versions of a document.

This repo currently holds the specification only. The code is built by an agent
from the prompts below.

## Files

- `SPEC.md` — the specification: naming and motifs, tile store, compositor,
  document and operation log, brush engine, input and Sidecar handling,
  annotation modes, performance budget, milestones, tests, distribution, and
  phase n+1 with its v1 hooks.
- `AGENTS.md` — operating rules for the agent (read automatically by Codex and
  Claude Code).
- `PROMPT.md` — the prompt to paste to start the build, and the one to paste if a
  fresh session has to resume it.

## Building it

1. Clone this repo and open it in Codex (or Claude Code) with the most autonomous
   approval mode available and network access for crate installs.
2. Paste the Start prompt from `PROMPT.md`.
3. When the usage cap hits, press reset and let it continue. If the same session
   resumes, do nothing. If you land in a new session, paste the Resume prompt.

Everything is Rust, so the agent can build and test the whole thing on Linux
against mock platform implementations. macOS-only code (tablet events, screen
capture, overlay windows) is confined to one crate behind traits, and it is the
one part the agent cannot verify. M1 is a hard performance gate placed before
anything is built on top of it.

## After the build

1. Build on the Mac with `make dist`. The app is unsigned, so the first launch
   needs an approval in System Settings; signing it properly would require a paid
   developer membership and is deliberately out of scope.
2. Turn on Sidecar, move the canvas-only window to the iPad, and leave the panels
   on the Mac.
3. Run the pressure calibration — draw a few natural strokes and let it fit the
   curve. Sidecar's default pressure mapping is reportedly skewed, so this matters
   more here than it would natively.
4. Tune the brushes. `TUNING.md` says which constants do what. The agent can prove
   the engine is correct, fast, and deterministic; whether the brushes feel right
   is the one thing it cannot judge.

`RUNBOOK.md` covers all of this in detail once the build has produced it.
