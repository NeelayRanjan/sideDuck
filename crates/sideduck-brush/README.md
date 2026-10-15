# Brush engine

`builtin_library()` returns the engine limits and twelve presets. `StrokeEngine`
accepts confirmed `InputSample`s in document pixels and writes linear-sRGB,
premultiplied RGBA through `RasterSurface`. The app supplies its document adapter;
`TileRaster` is a bounded sparse implementation and `DenseRaster` is useful in tests.
Create an engine per contact, call `push_sample`, then `finish`. On a budget or
surface error, call `cancel` to restore the preimages before discarding the engine.
Do not commit predicted input to the document log.

All shipped brush parameters, curves, texture recipes, and engine limits live in
[`assets/brushes.json`](assets/brushes.json). Size is a diameter in pixels; spacing
is a fraction of diameter; rotation is radians; grain scale and pickup length are
pixels. Each scalar parameter accepts one pressure, tilt magnitude, azimuth,
velocity or seeded random response curve. Curves scale a base value between the
specified minimum and maximum factors. Normalized quantities are clamped when a
dab is evaluated. Jitter uses independent SplitMix64 channels indexed by stroke
seed and dab index. No time or operating-system random source enters rendering.

Dabs accumulate into sparse stroke tiles. Opacity is applied once against the
original target. An opacity response curve controls a per-pixel maximum envelope
of touched dabs, so overlapping dabs do not repeatedly apply opacity. Erasing uses
the same coverage to attenuate the original premultiplied pixel. Wet mix samples
the current target at a point behind the dab, controlled by pickup length; it is
basic color pickup, without a fluid simulation. Brush smoothing is a configurable
position filter; the input modifier bypasses it.

The reference rasterizer visits every canvas pixel and uses an independent weighted
bilinear sampler. Tests compare every preset per pixel within `2e-5`, test analytic
stroke-opacity results, and require bit-identical pixels for identical inputs on
the same platform. This implementation is CPU-based; there is no unverified GPU
brush renderer. Mechanical tests do not certify brush feel.

## Open brush package

A `.quill` package is a directory containing `brush.json` (version 1 metadata and
parameters), `shape.png`, and `grain.png` (eight-bit grayscale masks). White means
full coverage; shape sampling clamps to zero outside the dab and grain repeats.
Imports validate dimensions and decoded/encoded size limits. A save flushes all
files and atomically renames a temporary directory; it refuses an existing path.
Operation records can serialize `BrushPreset` to retain both texture arrays and
all parameters independently of later library edits.

Twelve ready-made packages are in `assets/presets`. Regenerate packages into a new
empty directory after editing the config:

```sh
scripts/cargo.sh run --release -p sideduck-brush --example export_presets -- /tmp/brush-presets
```

## Verification

```sh
scripts/cargo.sh test -p sideduck-brush
scripts/cargo.sh run --release -p sideduck-brush --example brush_gate
```

The throughput gate uses the shipped 4px Technical pen and 10,000 input samples
on a 256×64 sparse target. It asserts at least 10,000 dabs/s and a combined 80 MiB
stroke/target ceiling. This is a specified baseline, not a claim about every brush
diameter. The observed Apple M4 Pro release result was 12,499 dabs in 16.19 ms
(772,018 dabs/s), with 278,528 target bytes and 622,592 peak stroke bytes.
Those counters include pixel buffers and a conservative per-tile metadata allowance;
the application gate measures process residency independently.

The checked-in `assets/strokes.png` is a numerical golden atlas: six columns,
two rows in preset order, with linear premultiplied bytes. It is not a normal
color-managed image export. The test allows one byte of cross-platform rounding.
Update only after reviewing reference parity and intentional config changes:

```sh
scripts/cargo.sh run --release -p sideduck-brush --example render_golden -- crates/sideduck-brush/assets/strokes.png
scripts/cargo.sh test -p sideduck-brush
```
