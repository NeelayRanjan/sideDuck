# Tuning painting and input

Brush feel requires evaluation with the intended tablet and display connection.
Automated tests establish deterministic pixels, reference agreement, memory
bounds, and measured throughput; they cannot establish that a brush feels good.

## Configuration sources

- `crates/sideduck-brush/assets/brushes.json` is the single source for the twelve
  built-in brush definitions, their procedural shape/grain recipes, and engine
  constants. `assets/presets/*.quill/` contains the generated portable packages.
- `config/input.json` controls input routing, pressure curves, calibration,
  smoothing, prediction, and latency histogram buckets.
- `config/prototype.json` is retained for the original drawing prototype. Its
  settings do not define the new document editor's brush engine.

The application does not make network requests or sync these settings. Packaged
resource copies are included in the local `.app`; built-in defaults are also
compiled into the binary so a missing working directory cannot break startup.

## Brush parameters

A parameter has a base `value` and an optional `response` with a driver, piecewise
linear curve, minimum multiplier and maximum multiplier. Drivers are pressure,
tilt magnitude, azimuth, normalized velocity, and the seeded random stream.
The evaluated value is the base value multiplied by the curve-mapped response.

Size is the diameter in document pixels. Spacing is a fraction of diameter;
smaller spacing increases both overlap and work. Flow controls each dab's alpha.
Opacity controls the accumulated stroke against its original preimage, so it is
not repeatedly applied where dabs overlap. When opacity has dynamics, each pixel
uses the maximum opacity response of the dabs that touched it.

Scatter moves a dab within a radial neighborhood. Rotation is in radians, with
an independent response for following stroke direction. Shape is a grayscale
mask; grain is a repeating grayscale modulation with scale and motion. Hardness
changes the mask edge response. Wet mix samples the existing target behind the
dab at the configured pickup distance. Hue, saturation and value jitter draw
from separate channels of the deterministic random stream.

Each `.quill` directory contains `brush.json`, `shape.png`, and `grain.png`.
Textures are bounded grayscale PNGs. The format is open; it does not read or write
another application's proprietary brush files. Saving a package uses a new path
and publishes the directory atomically.

Start tuning with size, spacing, flow and pressure response. Test light and firm
strokes, slow curves, fast diagonals, short taps and repeated crossings. Compare
at the same zoom and display scale. A very large textured brush has a different
cost from the small technical-pen workload used by the throughput gate.

## Input

Filters execute in their configured order. Calibration, global pressure and
per-brush pressure curves are separate stages. Average smoothing retains a
bounded sample window; pull-string smoothing keeps the filtered point within a
configured document-space distance. Holding the suspension modifier bypasses
stabilization; future snapping extensions receive their own suspension flag.

Calibration fits an empirical pressure distribution after trimming configured
low and high quantiles. Gather several natural strokes covering the useful
pressure range. A narrow or insufficient sample set is rejected. Review the
before/after distribution instead of forcing every input device to the same
curve.

Prediction extrapolates a bounded distance and time. Predicted samples belong to
a temporary preview, are retired when confirmed input arrives, and never enter
the operation log. Increase the horizon only after checking overshoot at corners
and at the end of a stroke.

The latency histogram uses the application's presentation notification and the
sample's monotonic clock. It does not measure physical scanout or the complete
Sidecar transport delay. Do not compare clocks from unrelated time origins.

## Memory and reproducibility

The default process ceiling is 4 GiB. Tile, derived-image, mip, GPU, stroke,
selection and export allocations have separate admission checks. Increasing a
cache does not raise the overall ceiling. Dirty stroke data is committed only
when the operation succeeds; a failed stroke must restore its preimages.

A stroke is reproduced by its input sample stream, complete brush definition and
textures, seed, and engine version on the same platform. Avoid changing built-in
constants while comparing replay results. Golden fixture updates must be
intentional and accompanied by reference-parity and determinism checks.
