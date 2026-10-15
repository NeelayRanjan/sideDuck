use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Texture {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) pixels: Vec<u8>,
}
impl Texture {
    pub fn new(width: u32, height: u32, pixels: Vec<u8>) -> Result<Self, String> {
        if width == 0
            || height == 0
            || width > 4096
            || height > 4096
            || pixels.len() as u64 != u64::from(width) * u64::from(height)
        {
            return Err("Invalid brush texture dimensions or pixel count".into());
        }
        Ok(Self {
            width,
            height,
            pixels,
        })
    }
    pub fn validate(&self) -> Result<(), String> {
        if self.width == 0
            || self.height == 0
            || self.width > 4096
            || self.height > 4096
            || self.pixels.len() as u64 != u64::from(self.width) * u64::from(self.height)
        {
            return Err("Invalid brush texture dimensions or pixel count".into());
        }
        Ok(())
    }
    pub fn dimensions(&self) -> [u32; 2] {
        [self.width, self.height]
    }
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }
    pub(crate) fn sample(&self, u: f32, v: f32, repeat: bool) -> f32 {
        if !repeat && (!(0.0..=1.0).contains(&u) || !(0.0..=1.0).contains(&v)) {
            return 0.0;
        }
        let x = if repeat { u.rem_euclid(1.0) } else { u } * self.width as f32 - 0.5;
        let y = if repeat { v.rem_euclid(1.0) } else { v } * self.height as f32 - 0.5;
        let left = x.floor() as i32;
        let top = y.floor() as i32;
        let fx = x - x.floor();
        let fy = y - y.floor();
        let a = self.texel(left, top, repeat);
        let b = self.texel(left + 1, top, repeat);
        let c = self.texel(left, top + 1, repeat);
        let d = self.texel(left + 1, top + 1, repeat);
        let upper = a + (b - a) * fx;
        let lower = c + (d - c) * fx;
        upper + (lower - upper) * fy
    }
    pub(crate) fn reference_sample(&self, u: f32, v: f32, repeat: bool) -> f32 {
        if !repeat && (!(0.0..=1.0).contains(&u) || !(0.0..=1.0).contains(&v)) {
            return 0.0;
        }
        let x = if repeat { u.rem_euclid(1.0) } else { u } * self.width as f32 - 0.5;
        let y = if repeat { v.rem_euclid(1.0) } else { v } * self.height as f32 - 0.5;
        let mut value = 0.0;
        for dy in 0..2 {
            for dx in 0..2 {
                let weight_x = if dx == 0 {
                    1.0 - (x - x.floor())
                } else {
                    x - x.floor()
                };
                let weight_y = if dy == 0 {
                    1.0 - (y - y.floor())
                } else {
                    y - y.floor()
                };
                value += self.texel(x.floor() as i32 + dx, y.floor() as i32 + dy, repeat)
                    * weight_x
                    * weight_y;
            }
        }
        value
    }
    fn texel(&self, x: i32, y: i32, repeat: bool) -> f32 {
        let x = if repeat {
            x.rem_euclid(self.width as i32)
        } else {
            x.clamp(0, self.width as i32 - 1)
        };
        let y = if repeat {
            y.rem_euclid(self.height as i32)
        } else {
            y.clamp(0, self.height as i32 - 1)
        };
        f32::from(self.pixels[y as usize * self.width as usize + x as usize]) / 255.0
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TextureRecipe {
    pub side: u32,
    pub shape: Shape,
    pub aspect: f32,
    pub bristle_frequency: f32,
    pub grain_contrast: f32,
    pub grain_seed: u64,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Shape {
    Round,
    Square,
    Bristle,
}
impl TextureRecipe {
    pub fn generate(&self) -> Result<(Texture, Texture), String> {
        if self.side == 0
            || self.side > 4096
            || !self.aspect.is_finite()
            || self.aspect <= 0.0
            || !self.bristle_frequency.is_finite()
            || self.bristle_frequency < 0.0
            || !(0.0..=1.0).contains(&self.grain_contrast)
        {
            return Err("Invalid brush texture recipe".into());
        }
        let mut shape = Vec::with_capacity((self.side * self.side) as usize);
        let mut grain = Vec::with_capacity(shape.capacity());
        for y in 0..self.side {
            for x in 0..self.side {
                let u = ((x as f32 + 0.5) / self.side as f32 * 2.0 - 1.0) / self.aspect;
                let v = (y as f32 + 0.5) / self.side as f32 * 2.0 - 1.0;
                let alpha = match self.shape {
                    Shape::Round => 1.0 - u.hypot(v),
                    Shape::Square => 1.0 - u.abs().max(v.abs()),
                    Shape::Bristle => {
                        (1.0 - u.abs().max(v.abs())) * (v * self.bristle_frequency).cos().abs()
                    }
                }
                .clamp(0.0, 1.0);
                shape.push((alpha * 255.0).round() as u8);
                let random = random_unit(self.grain_seed, u64::from(y * self.side + x), 0);
                grain.push(((1.0 - self.grain_contrast * random) * 255.0).round() as u8);
            }
        }
        Ok((
            Texture::new(self.side, self.side, shape)?,
            Texture::new(self.side, self.side, grain)?,
        ))
    }
}

/// Stateless SplitMix64: adding a response channel cannot shift another channel's stream.
pub(crate) fn random_unit(seed: u64, dab: u64, channel: u64) -> f32 {
    let mut value = seed
        ^ dab.wrapping_mul(0x9e37_79b9_7f4a_7c15)
        ^ channel.wrapping_mul(0xd1b5_4a32_d192_ed03);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    ((value ^ (value >> 31)) >> 40) as f32 / 16_777_216.0
}
