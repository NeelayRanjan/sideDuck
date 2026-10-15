# Input pipeline

`InputSample` is a serializable sample in document coordinates with normalized
pressure, two-axis tilt, azimuth, device/tool identity, phase, buttons, modifiers,
and a timestamp from one monotonic clock. Native event translation remains in
`sideduck-platform`; the application maps coordinates before filtering. Existing
`Point`, `CanvasMapping`, and `ContactTracker` APIs remain available.

All shipped input tuning defaults live in [`config/input.json`](../../config/input.json).
`InputConfig::from_json` validates the bounded chain, calibration settings,
response curves, and latency buckets before state is allocated. Average smoothing
uses a sample window; replace it with `{"kind":"pull_string","distance":5.0}`
to use a document-pixel leash. Those are different smoothing algorithms and can
also be combined deliberately in the ordered list. The example distance is not a
second default. Pressure curves interpolate normalized points with strictly
increasing X; Y may decrease for inverse brush responses.

Construct `FilterChain` from that configuration. Calibration, global pressure,
brush pressure, smoothing, and registered extensions run in the specified order.
Prediction must be the last stage. `InputFilter` is the extension point for future
guide/snapping filters; it receives modifiers and lifecycle resets. Extensions
may change position, pressure, tilt, and azimuth but cannot rewrite event identity
or timestamps. `suspend_stabilization` resets smoothing history at the current
sample, while `suspend_snapping` is passed to extensions. A source/tool change,
lift, malformed sample, or backwards timestamp resets contact so separate strokes
cannot be joined accidentally. Mouse input routes to navigation by default.

`process` returns `ProcessedSample`, including both a confirmed sample and an
optional provisional prediction. Write only confirmed samples to the operation
log and committed stroke buffer. Before handling each result, erase the prior
temporary surface named by `retired_prediction`, then draw the confirmed segment
and optional replacement prediction on separate surfaces. Clear the temporary
surface on `reset()` and curve changes using their returned ID. Prediction never
changes the confirmed stream. This separation is required for reconciliation;
painting prediction directly into the document violates the contract.

Feed real, unfiltered contact samples to `PressureCalibrator`, then call `fit`.
The bounded empirical quantile fit maps the observed pressure distribution toward
a uniform response, trims the configured outliers, and returns before/after
histograms for the editor. Constant-pressure traces are rejected. Install the
result with `set_calibration`; `set_global_pressure` and `set_brush_pressure`
likewise validate and reset the current contact. Retain the raw calibration
result with application settings. Persisted document strokes contain the
confirmed, already filtered samples. JSON float round-trip parsing is explicitly
enabled so timestamps and curve values retain their bits.

`LatencyTracker::record_sample` receives confirmed sequence IDs and their source
timestamps. Call `present_through` when a frame including those IDs is presented,
using the same clock. The bounded pending queue drops its oldest entries on
overflow and reports that count. `report_json` produces an optional local
histogram; it sends nothing over the network. Percentiles are bucket upper-bound
estimates, with the observed maximum for the overflow bucket. Application
presentation notification is not physical scanout or sample-to-photon timing.

Run `./scripts/cargo.sh test -p sideduck-input`. The saved fixture in
`tests/data/recorded-stroke.json` is a synthetic portable event recording, not a
claim of physical Sidecar capture. Tests replay it identically, compare prediction
reconciliation to an independent integer golden raster, fit a known nonlinear
pressure distribution, and exercise smoothing, invalid events, extension order,
and memory bounds. To update a golden, change the fixture and its explicit
expected geometry together and explain the behavior change; never regenerate an
expected image from the implementation under test. Native Sidecar latency and
Pencil feel still require the device acceptance procedure in the runbook.
