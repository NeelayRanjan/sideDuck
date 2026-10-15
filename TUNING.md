# Prototype tuning

All shipped prototype brush and input defaults live in `config/prototype.json`.
They are embedded at compile time; use `--config config/prototype.json` to load
edits without recompiling. Invalid dimensions, pressures, queue capacity or brush
parameters are rejected at startup.

| Setting | Meaning |
| --- | --- |
| canvas_width / canvas_height | Scratch raster dimensions, each 64–4096 pixels. |
| input_queue_capacity | Maximum retained native samples; overflow ends a stroke rather than joining across missing data. |
| tablet_mouse_suppression_ms | Ignore duplicate mouse input briefly after native tablet events (default 200 ms). |
| canvas_repaint_ms / controls_repaint_ms | Idle redraw request intervals; these are not latency measurements. |
| mouse_pressure | Fixed pressure for explicitly enabled mouse mode; does not simulate native tablet telemetry. |
| brush.radius | Maximum brush radius in document pixels. |
| brush.minimum_radius_fraction | Radius fraction at the lightest positive pressure. |
| brush.spacing_fraction | Dab spacing relative to pressure-adjusted radius. |
| brush.pressure_gamma | Exponent on normalized pressure. Larger values require more pressure for a wide mark. |

Radius and pressure exponent can be changed in the controls. These changes are
session-only. The prototype is an opaque round pen on white, with a white eraser.
It does not yet implement stroke opacity, textured brushes, linear-light working
color, P3 output, wet mixing, grain or the v1 brush format. Rendering is a scalar
CPU reference path; no GPU brush parity claim applies. Deterministic replay tests
use identical sample streams on the same platform. The engineer has not judged
brush feel: tuning requires drawing on real hardware.
