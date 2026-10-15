use std::collections::{BTreeMap, BTreeSet};

/// Linear-sRGB, premultiplied RGBA components.
pub type Pixel = [f32; 4];
pub type TileCoordinate = [u32; 2];

/// The document adapter can load/pin tiles here; the engine never owns a canvas-sized buffer.
pub trait RasterSurface {
    fn dimensions(&self) -> [u32; 2];
    fn read_pixel(&self, x: u32, y: u32) -> Result<Pixel, String>;
    fn write_pixel(&mut self, x: u32, y: u32, pixel: Pixel) -> Result<(), String>;
}

#[derive(Clone, Debug)]
pub struct DenseRaster {
    width: u32,
    height: u32,
    pixels: Vec<Pixel>,
}
impl DenseRaster {
    pub fn new(width: u32, height: u32, byte_budget: u64) -> Result<Self, String> {
        let count = u64::from(width) * u64::from(height);
        if width == 0 || height == 0 || count > byte_budget / 16 || count > usize::MAX as u64 / 16 {
            return Err("Raster dimensions exceed its memory budget".into());
        }
        Ok(Self {
            width,
            height,
            pixels: vec![[0.0; 4]; count as usize],
        })
    }
    pub fn pixels(&self) -> &[Pixel] {
        &self.pixels
    }
    pub fn clear(&mut self, pixel: Pixel) -> Result<(), String> {
        validate_pixel(pixel)?;
        self.pixels.fill(pixel);
        Ok(())
    }
    pub fn resident_bytes(&self) -> u64 {
        self.pixels.len() as u64 * 16
    }
    fn index(&self, x: u32, y: u32) -> Result<usize, String> {
        if x >= self.width || y >= self.height {
            return Err("Pixel is outside raster".into());
        }
        Ok(y as usize * self.width as usize + x as usize)
    }
}
impl RasterSurface for DenseRaster {
    fn dimensions(&self) -> [u32; 2] {
        [self.width, self.height]
    }
    fn read_pixel(&self, x: u32, y: u32) -> Result<Pixel, String> {
        Ok(self.pixels[self.index(x, y)?])
    }
    fn write_pixel(&mut self, x: u32, y: u32, pixel: Pixel) -> Result<(), String> {
        validate_pixel(pixel)?;
        let index = self.index(x, y)?;
        self.pixels[index] = pixel;
        Ok(())
    }
}

/// Sparse in-memory target with an enforced allocation ceiling. A production document can
/// implement RasterSurface over its disk-backed tile store instead.
#[derive(Clone, Debug)]
pub struct TileRaster {
    dimensions: [u32; 2],
    tile_size: u32,
    budget: u64,
    tiles: BTreeMap<TileCoordinate, Vec<Pixel>>,
    dirty: BTreeSet<TileCoordinate>,
}
impl TileRaster {
    pub fn new(width: u32, height: u32, tile_size: u32, byte_budget: u64) -> Result<Self, String> {
        if width == 0
            || height == 0
            || !(8..=256).contains(&tile_size)
            || !tile_size.is_power_of_two()
        {
            return Err("Invalid sparse raster geometry".into());
        }
        Ok(Self {
            dimensions: [width, height],
            tile_size,
            budget: byte_budget,
            tiles: BTreeMap::new(),
            dirty: BTreeSet::new(),
        })
    }
    fn tile_bytes(&self) -> u64 {
        u64::from(self.tile_size).pow(2) * 16 + 1024
    }
    pub fn resident_bytes(&self) -> u64 {
        self.tiles.len() as u64 * self.tile_bytes()
    }
    pub fn tile_count(&self) -> usize {
        self.tiles.len()
    }
    pub fn tile_size(&self) -> u32 {
        self.tile_size
    }
    pub fn tiles(&self) -> impl Iterator<Item = (TileCoordinate, &[Pixel])> {
        self.tiles
            .iter()
            .map(|(coord, pixels)| (*coord, pixels.as_slice()))
    }
    pub fn take_dirty_tiles(&mut self) -> Vec<TileCoordinate> {
        std::mem::take(&mut self.dirty).into_iter().collect()
    }
    pub fn trim_transparent(&mut self) {
        self.tiles
            .retain(|_, pixels| pixels.iter().any(|pixel| pixel[3] != 0.0));
    }
    fn address(&self, x: u32, y: u32) -> Result<(TileCoordinate, usize), String> {
        if x >= self.dimensions[0] || y >= self.dimensions[1] {
            return Err("Pixel is outside raster".into());
        }
        Ok((
            [x / self.tile_size, y / self.tile_size],
            ((y % self.tile_size) * self.tile_size + x % self.tile_size) as usize,
        ))
    }
}
impl RasterSurface for TileRaster {
    fn dimensions(&self) -> [u32; 2] {
        self.dimensions
    }
    fn read_pixel(&self, x: u32, y: u32) -> Result<Pixel, String> {
        let (coord, index) = self.address(x, y)?;
        Ok(self.tiles.get(&coord).map_or([0.0; 4], |tile| tile[index]))
    }
    fn write_pixel(&mut self, x: u32, y: u32, pixel: Pixel) -> Result<(), String> {
        validate_pixel(pixel)?;
        let (coord, index) = self.address(x, y)?;
        if !self.tiles.contains_key(&coord) {
            if pixel == [0.0; 4] {
                return Ok(());
            }
            if self.tile_bytes() > self.budget.saturating_sub(self.resident_bytes()) {
                return Err("Sparse raster memory ceiling exceeded".into());
            }
            self.tiles.insert(
                coord,
                vec![[0.0; 4]; (self.tile_size * self.tile_size) as usize],
            );
        }
        self.tiles.get_mut(&coord).expect("tile inserted")[index] = pixel;
        self.dirty.insert(coord);
        Ok(())
    }
}
pub(crate) fn validate_pixel(pixel: Pixel) -> Result<(), String> {
    if pixel
        .iter()
        .any(|v| !v.is_finite() || !(0.0..=1.0).contains(v))
        || pixel[..3].iter().any(|v| *v > pixel[3])
    {
        Err("Raster pixel must be finite linear premultiplied RGBA".into())
    } else {
        Ok(())
    }
}
