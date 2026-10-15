use crate::texture::random_unit;
use crate::{BrushDefinition, EngineConfig, Pixel, RasterSurface, Texture, TileCoordinate};
use serde::{Deserialize, Serialize};
use sideduck_input::{InputSample, SamplePhase, ToolKind};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BrushPreset {
    pub definition: BrushDefinition,
    pub shape: Texture,
    pub grain: Texture,
}
impl BrushPreset {
    pub fn validate(&self, config: &EngineConfig) -> Result<(), String> {
        config.validate()?;
        self.definition.validate()?;
        self.shape.validate()?;
        self.grain.validate()?;
        if self
            .shape
            .dimensions()
            .into_iter()
            .chain(self.grain.dimensions())
            .any(|side| side > config.maximum_texture_side)
        {
            return Err("Brush texture exceeds configured size ceiling".into());
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StrokeUpdate {
    pub dabs: u64,
    pub dirty_tiles: Vec<TileCoordinate>,
    pub resident_bytes: u64,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StrokeSummary {
    pub dabs: u64,
    pub dirty_tiles: Vec<TileCoordinate>,
    pub peak_resident_bytes: u64,
}
struct StrokeTile {
    original: Vec<Pixel>,
    accumulated: Vec<Pixel>,
    opacity: Vec<f32>,
}
struct Dab {
    position: [f32; 2],
    radius: f32,
    rotation: f32,
    color: [f32; 3],
    flow: f32,
    opacity: f32,
    hardness: f32,
    grain_scale: f32,
    grain_offset: f32,
}

/// Deterministic CPU implementation, suitable both for interactive sparse tiles and operation replay.
/// Opacity is applied once to the accumulated stroke against its saved preimage. With opacity dynamics,
/// each pixel uses the largest opacity response of the dabs touching it, never repeated source-over.
pub struct StrokeEngine {
    preset: BrushPreset,
    config: EngineConfig,
    color: [f32; 3],
    seed: u64,
    previous: Option<InputSample>,
    dimensions: Option<[u32; 2]>,
    tool: Option<ToolKind>,
    remaining: f32,
    distance: f32,
    dabs: u64,
    tiles: BTreeMap<TileCoordinate, StrokeTile>,
    dirty: BTreeSet<TileCoordinate>,
    peak: u64,
    closed: bool,
    reference: bool,
}
impl StrokeEngine {
    pub fn new(
        preset: BrushPreset,
        config: EngineConfig,
        color: [f32; 3],
        seed: u64,
    ) -> Result<Self, String> {
        preset.validate(&config)?;
        if color
            .iter()
            .any(|value| !value.is_finite() || !(0.0..=1.0).contains(value))
        {
            return Err("Brush color must be finite linear RGB".into());
        }
        Ok(Self {
            preset,
            config,
            color,
            seed,
            previous: None,
            dimensions: None,
            tool: None,
            remaining: 0.0,
            distance: 0.0,
            dabs: 0,
            tiles: BTreeMap::new(),
            dirty: BTreeSet::new(),
            peak: 0,
            closed: false,
            reference: false,
        })
    }
    /// Deliberately slow scalar oracle: every dab visits every canvas pixel and uses a separate
    /// weighted texture sampler. Use small canvases for reference parity tests.
    pub fn new_reference(
        preset: BrushPreset,
        config: EngineConfig,
        color: [f32; 3],
        seed: u64,
    ) -> Result<Self, String> {
        let mut engine = Self::new(preset, config, color, seed)?;
        engine.reference = true;
        Ok(engine)
    }
    pub fn resident_bytes(&self) -> u64 {
        self.tiles.len() as u64 * self.tile_bytes()
    }
    fn tile_bytes(&self) -> u64 {
        u64::from(self.config.tile_size).pow(2) * 36 + 2048
    }
    pub fn push_sample(
        &mut self,
        surface: &mut impl RasterSurface,
        mut sample: InputSample,
    ) -> Result<StrokeUpdate, String> {
        if self.closed {
            return Err("Stroke has already finished".into());
        }
        sample.validate()?;
        let dimensions = surface.dimensions();
        if dimensions.contains(&0) || self.dimensions.is_some_and(|old| old != dimensions) {
            return Err("Stroke target dimensions changed".into());
        }
        if self.tool.is_some_and(|tool| tool != sample.tool) {
            return Err("Input tool changed during stroke".into());
        }
        if self
            .previous
            .is_some_and(|old| old.timestamp_seconds > sample.timestamp_seconds)
        {
            return Err("Stroke timestamps must be monotonic".into());
        }
        if sample.phase == SamplePhase::Hover {
            return Ok(StrokeUpdate::default());
        }
        self.dimensions = Some(dimensions);
        self.tool = Some(sample.tool);
        sample.pressure = sample.pressure.clamp(0.0, 1.0);
        let before = self.dabs;
        let mut dirty = BTreeSet::new();
        if let Some(previous) = self.previous {
            let elapsed = (sample.timestamp_seconds - previous.timestamp_seconds) as f32;
            let raw_delta = [
                sample.position[0] - previous.position[0],
                sample.position[1] - previous.position[1],
            ];
            let speed = if elapsed > 0.0 {
                raw_delta[0].hypot(raw_delta[1]) / elapsed
            } else {
                0.0
            };
            let smoothing = self
                .preset
                .definition
                .parameters
                .smoothing
                .evaluate(self.drivers(sample, speed))
                .clamp(0.0, 1.0);
            if !sample.modifiers.suspend_stabilization {
                for (axis, delta) in raw_delta.iter().enumerate() {
                    sample.position[axis] = previous.position[axis] + delta * (1.0 - smoothing);
                }
            }
            let delta = [
                sample.position[0] - previous.position[0],
                sample.position[1] - previous.position[1],
            ];
            let length = delta[0].hypot(delta[1]);
            if !length.is_finite() {
                return Err("Stroke segment length exceeds numeric range".into());
            }
            let direction = delta[1].atan2(delta[0]);
            let mut position = self.remaining;
            let start_distance = self.distance;
            if length == 0.0
                && previous.pressure <= 0.0
                && sample.pressure > 0.0
                && sample.phase != SamplePhase::End
            {
                self.remaining = self.stamp(surface, sample, speed, direction, &mut dirty)?;
                position = self.remaining;
            }
            let mut count = 0;
            while position <= length && length > 0.0 {
                if count >= self.config.maximum_dabs_per_sample {
                    return Err("Stroke sample exceeds dab limit".into());
                }
                let t = position / length;
                let mut dab_sample = sample;
                for (axis, delta_value) in delta.iter().enumerate() {
                    dab_sample.position[axis] = previous.position[axis] + delta_value * t;
                    dab_sample.tilt[axis] =
                        previous.tilt[axis] + (sample.tilt[axis] - previous.tilt[axis]) * t;
                }
                dab_sample.pressure = previous.pressure + (sample.pressure - previous.pressure) * t;
                dab_sample.azimuth = previous.azimuth + (sample.azimuth - previous.azimuth) * t;
                self.distance = start_distance + position;
                let spacing = self.stamp(surface, dab_sample, speed, direction, &mut dirty)?;
                position += spacing;
                count += 1;
            }
            self.remaining = (position - length).max(0.0);
            self.distance = start_distance + length;
        } else if sample.pressure > 0.0 && sample.phase != SamplePhase::End {
            self.remaining = self.stamp(surface, sample, 0.0, 0.0, &mut dirty)?;
        }
        self.previous = Some(sample);
        self.dirty.extend(&dirty);
        Ok(StrokeUpdate {
            dabs: self.dabs - before,
            dirty_tiles: dirty.into_iter().collect(),
            resident_bytes: self.resident_bytes(),
        })
    }
    fn drivers(&self, sample: InputSample, speed: f32) -> [f32; 5] {
        [
            sample.pressure.clamp(0.0, 1.0),
            (sample.tilt[0].hypot(sample.tilt[1]) / self.config.tilt_normalizer).clamp(0.0, 1.0),
            sample.azimuth.rem_euclid(std::f32::consts::TAU) / std::f32::consts::TAU,
            (speed / self.config.velocity_normalizer).clamp(0.0, 1.0),
            random_unit(self.seed, self.dabs, 0),
        ]
    }
    fn stamp(
        &mut self,
        surface: &mut impl RasterSurface,
        sample: InputSample,
        speed: f32,
        direction: f32,
        dirty: &mut BTreeSet<TileCoordinate>,
    ) -> Result<f32, String> {
        let params = &self.preset.definition.parameters;
        let drivers = self.drivers(sample, speed);
        let diameter = params
            .size
            .evaluate(drivers)
            .clamp(self.config.minimum_spacing, 2048.0);
        let spacing =
            (params.spacing.evaluate(drivers) * diameter).max(self.config.minimum_spacing);
        if sample.pressure <= 0.0 {
            return Ok(spacing);
        }
        let radius = diameter * 0.5;
        let scatter = params.scatter.evaluate(drivers).clamp(0.0, 16.0)
            * radius
            * random_unit(self.seed, self.dabs, 1).sqrt();
        let theta = random_unit(self.seed, self.dabs, 2) * std::f32::consts::TAU;
        let position = [
            sample.position[0] + theta.cos() * scatter,
            sample.position[1] + theta.sin() * scatter,
        ];
        let rotation = params.rotation.evaluate(drivers)
            + direction
                * params
                    .rotation_follows_direction
                    .evaluate(drivers)
                    .clamp(0.0, 1.0);
        let mut color = jitter_color(
            self.color,
            [
                params.hue_jitter.evaluate(drivers),
                params.saturation_jitter.evaluate(drivers),
                params.value_jitter.evaluate(drivers),
            ],
            [
                random_unit(self.seed, self.dabs, 3),
                random_unit(self.seed, self.dabs, 4),
                random_unit(self.seed, self.dabs, 5),
            ],
        );
        let wet = params.wet_mix.evaluate(drivers).clamp(0.0, 1.0);
        if wet > 0.0 {
            let pickup = params.pickup_length.evaluate(drivers).max(0.0);
            let size = surface.dimensions();
            let x = (position[0] - direction.cos() * pickup)
                .floor()
                .clamp(0.0, (size[0] - 1) as f32) as u32;
            let y = (position[1] - direction.sin() * pickup)
                .floor()
                .clamp(0.0, (size[1] - 1) as f32) as u32;
            let pixel = surface.read_pixel(x, y)?;
            if pixel[3] > 0.0 {
                for channel in 0..3 {
                    color[channel] += (pixel[channel] / pixel[3] - color[channel]) * wet;
                }
            }
        }
        let dab = Dab {
            position,
            radius,
            rotation,
            color,
            flow: params.flow.evaluate(drivers).clamp(0.0, 1.0),
            opacity: params.opacity.evaluate(drivers).clamp(0.0, 1.0),
            hardness: params.hardness.evaluate(drivers).clamp(0.0, 1.0),
            grain_scale: params
                .grain_scale
                .evaluate(drivers)
                .max(self.config.minimum_spacing),
            grain_offset: params.grain_motion.evaluate(drivers) * self.distance,
        };
        self.paint(surface, &dab, dirty)?;
        self.dabs += 1;
        Ok(spacing)
    }
    fn paint(
        &mut self,
        surface: &mut impl RasterSurface,
        dab: &Dab,
        dirty: &mut BTreeSet<TileCoordinate>,
    ) -> Result<(), String> {
        if dab.opacity == 0.0 || dab.flow == 0.0 {
            return Ok(());
        }
        let [width, height] = surface.dimensions();
        let extent = dab.radius * std::f32::consts::SQRT_2;
        let bounds = if self.reference {
            [0, 0, width, height]
        } else {
            [
                (dab.position[0] - extent).floor().clamp(0.0, width as f32) as u32,
                (dab.position[1] - extent).floor().clamp(0.0, height as f32) as u32,
                (dab.position[0] + extent).ceil().clamp(0.0, width as f32) as u32,
                (dab.position[1] + extent).ceil().clamp(0.0, height as f32) as u32,
            ]
        };
        let (sin, cos) = dab.rotation.sin_cos();
        let eraser = self.preset.definition.eraser || self.tool == Some(ToolKind::Eraser);
        for y in bounds[1]..bounds[3] {
            for x in bounds[0]..bounds[2] {
                let dx = x as f32 + 0.5 - dab.position[0];
                let dy = y as f32 + 0.5 - dab.position[1];
                let u = (cos * dx + sin * dy) / dab.radius * 0.5 + 0.5;
                let v = (-sin * dx + cos * dy) / dab.radius * 0.5 + 0.5;
                let shape = if self.reference {
                    self.preset.shape.reference_sample(u, v, false)
                } else {
                    self.preset.shape.sample(u, v, false)
                };
                if shape <= 0.0 {
                    continue;
                }
                let gu = (x as f32 + 0.5 + dab.grain_offset) / dab.grain_scale;
                let gv = (y as f32 + 0.5 + dab.grain_offset) / dab.grain_scale;
                let grain = if self.reference {
                    self.preset.grain.reference_sample(gu, gv, true)
                } else {
                    self.preset.grain.sample(gu, gv, true)
                };
                let coverage = (shape
                    / (1.0 - dab.hardness + self.config.antialias_width / dab.radius))
                    .clamp(0.0, 1.0);
                let alpha = (coverage * grain * dab.flow).clamp(0.0, 1.0);
                if alpha == 0.0 {
                    continue;
                }
                let coord = [x / self.config.tile_size, y / self.config.tile_size];
                self.ensure_tile(surface, coord)?;
                let index = ((y % self.config.tile_size) * self.config.tile_size
                    + x % self.config.tile_size) as usize;
                let tile = self.tiles.get_mut(&coord).expect("stroke tile prepared");
                let accumulated = &mut tile.accumulated[index];
                for (channel, value) in accumulated[..3].iter_mut().enumerate() {
                    *value = dab.color[channel] * alpha + *value * (1.0 - alpha);
                }
                accumulated[3] = alpha + accumulated[3] * (1.0 - alpha);
                tile.opacity[index] = tile.opacity[index].max(dab.opacity);
                let opacity = tile.opacity[index];
                let original = tile.original[index];
                let mut output = [0.0; 4];
                for channel in 0..4 {
                    output[channel] = if eraser {
                        original[channel] * (1.0 - accumulated[3] * opacity)
                    } else {
                        accumulated[channel] * opacity
                            + original[channel] * (1.0 - accumulated[3] * opacity)
                    };
                }
                output[3] = output[3].clamp(0.0, 1.0);
                // Rounding must not produce a channel larger than the computed alpha.
                let alpha = output[3];
                for value in &mut output[..3] {
                    *value = value.min(alpha);
                }
                surface.write_pixel(x, y, output)?;
                dirty.insert(coord);
            }
        }
        Ok(())
    }
    fn ensure_tile(
        &mut self,
        surface: &impl RasterSurface,
        coord: TileCoordinate,
    ) -> Result<(), String> {
        if self.tiles.contains_key(&coord) {
            return Ok(());
        }
        if self.tile_bytes()
            > self
                .config
                .maximum_stroke_bytes
                .saturating_sub(self.resident_bytes())
        {
            return Err("Stroke buffer memory ceiling exceeded".into());
        }
        let side = self.config.tile_size;
        let dimensions = surface.dimensions();
        let mut original = vec![[0.0; 4]; (side * side) as usize];
        for y in 0..side {
            for x in 0..side {
                let px = coord[0] * side + x;
                let py = coord[1] * side + y;
                if px < dimensions[0] && py < dimensions[1] {
                    original[(y * side + x) as usize] = surface.read_pixel(px, py)?;
                }
            }
        }
        self.tiles.insert(
            coord,
            StrokeTile {
                original,
                accumulated: vec![[0.0; 4]; (side * side) as usize],
                opacity: vec![0.0; (side * side) as usize],
            },
        );
        self.peak = self.peak.max(self.resident_bytes());
        Ok(())
    }
    pub fn finish(&mut self) -> StrokeSummary {
        self.closed = true;
        self.dirty.extend(self.tiles.keys().copied());
        self.tiles.clear();
        StrokeSummary {
            dabs: self.dabs,
            dirty_tiles: self.dirty.iter().copied().collect(),
            peak_resident_bytes: self.peak,
        }
    }
    pub fn cancel(&mut self, surface: &mut impl RasterSurface) -> Result<(), String> {
        if self.closed {
            return Err("Stroke has already finished".into());
        }
        if self
            .dimensions
            .is_some_and(|dimensions| dimensions != surface.dimensions())
        {
            return Err("Stroke target dimensions changed".into());
        }
        let side = self.config.tile_size;
        let dimensions = surface.dimensions();
        for (coord, tile) in &self.tiles {
            for y in 0..side {
                for x in 0..side {
                    let px = coord[0] * side + x;
                    let py = coord[1] * side + y;
                    if px < dimensions[0] && py < dimensions[1] {
                        surface.write_pixel(px, py, tile.original[(y * side + x) as usize])?;
                    }
                }
            }
        }
        self.tiles.clear();
        self.closed = true;
        Ok(())
    }
}
fn jitter_color(rgb: [f32; 3], jitter: [f32; 3], random: [f32; 3]) -> [f32; 3] {
    if jitter == [0.0; 3] {
        return rgb;
    }
    let maximum = rgb.into_iter().fold(0.0, f32::max);
    let minimum = rgb.into_iter().fold(1.0, f32::min);
    let delta = maximum - minimum;
    let hue = if delta == 0.0 {
        0.0
    } else if maximum == rgb[0] {
        ((rgb[1] - rgb[2]) / delta).rem_euclid(6.0) / 6.0
    } else if maximum == rgb[1] {
        ((rgb[2] - rgb[0]) / delta + 2.0) / 6.0
    } else {
        ((rgb[0] - rgb[1]) / delta + 4.0) / 6.0
    };
    let saturation = if maximum == 0.0 { 0.0 } else { delta / maximum };
    let h = (hue + (random[0] * 2.0 - 1.0) * jitter[0]).rem_euclid(1.0) * 6.0;
    let s = (saturation + (random[1] * 2.0 - 1.0) * jitter[1]).clamp(0.0, 1.0);
    let v = (maximum + (random[2] * 2.0 - 1.0) * jitter[2]).clamp(0.0, 1.0);
    let c = v * s;
    let x = c * (1.0 - (h.rem_euclid(2.0) - 1.0).abs());
    let m = v - c;
    let components = match h as u32 {
        0 => [c, x, 0.0],
        1 => [x, c, 0.0],
        2 => [0.0, c, x],
        3 => [0.0, x, c],
        4 => [x, 0.0, c],
        _ => [c, 0.0, x],
    };
    components.map(|value| value + m)
}
