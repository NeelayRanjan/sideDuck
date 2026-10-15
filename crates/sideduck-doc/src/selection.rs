//! Budgeted grayscale selections in document pixel coordinates.

type Result<T> = std::result::Result<T, String>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectionMask {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
    budget: usize,
}
impl SelectionMask {
    pub fn from_pixels(width: u32, height: u32, pixels: Vec<u8>, budget: usize) -> Result<Self> {
        let length = (width as usize)
            .checked_mul(height as usize)
            .filter(|&n| width > 0 && height > 0 && n <= budget && n <= u32::MAX as usize)
            .ok_or("selection dimensions exceed their memory budget")?;
        if pixels.len() != length || pixels.capacity() > budget {
            return Err("selection pixels do not match dimensions or memory budget".into());
        }
        Ok(Self {
            width,
            height,
            pixels,
            budget,
        })
    }
    pub fn new(width: u32, height: u32, budget: usize) -> Result<Self> {
        let length = (width as usize)
            .checked_mul(height as usize)
            .filter(|&n| width > 0 && height > 0 && n <= budget && n <= u32::MAX as usize)
            .ok_or("selection dimensions exceed their memory budget")?;
        Ok(Self {
            width,
            height,
            pixels: vec![0; length],
            budget,
        })
    }
    pub fn width(&self) -> u32 {
        self.width
    }
    pub fn height(&self) -> u32 {
        self.height
    }
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }
    pub fn allocated_bytes(&self) -> usize {
        self.pixels.capacity()
    }
    pub fn coverage(&self, x: u32, y: u32) -> u8 {
        if x < self.width && y < self.height {
            self.pixels[(y * self.width + x) as usize]
        } else {
            0
        }
    }
    pub fn clear(&mut self) {
        self.pixels.fill(0);
    }
    pub fn select_all(&mut self) {
        self.pixels.fill(255);
    }
    pub fn invert(&mut self) {
        self.pixels.iter_mut().for_each(|v| *v = 255 - *v);
    }
    pub fn intersect(&mut self, other: &Self) -> Result<()> {
        self.compatible(other)?;
        for (a, b) in self.pixels.iter_mut().zip(&other.pixels) {
            *a = (*a).min(*b);
        }
        Ok(())
    }
    pub fn union(&mut self, other: &Self) -> Result<()> {
        self.compatible(other)?;
        for (a, b) in self.pixels.iter_mut().zip(&other.pixels) {
            *a = (*a).max(*b);
        }
        Ok(())
    }
    fn compatible(&self, other: &Self) -> Result<()> {
        if self.width != other.width || self.height != other.height {
            Err("selection canvases differ".into())
        } else {
            Ok(())
        }
    }
    fn scratch(&self, bytes_per_pixel: usize) -> Result<()> {
        if self
            .pixels
            .len()
            .checked_mul(bytes_per_pixel)
            .is_none_or(|n| n > self.budget)
        {
            return Err("selection operation exceeds its memory budget".into());
        }
        Ok(())
    }
    pub fn rectangle(&mut self, a: [f64; 2], b: [f64; 2]) -> Result<()> {
        self.shape(a, b, false)
    }
    pub fn ellipse(&mut self, a: [f64; 2], b: [f64; 2]) -> Result<()> {
        self.shape(a, b, true)
    }
    fn shape(&mut self, a: [f64; 2], b: [f64; 2], ellipse: bool) -> Result<()> {
        if !a.into_iter().chain(b).all(f64::is_finite) {
            return Err("selection coordinates must be finite".into());
        }
        let lo = [a[0].min(b[0]), a[1].min(b[1])];
        let hi = [a[0].max(b[0]), a[1].max(b[1])];
        self.clear();
        if lo[0] == hi[0] || lo[1] == hi[1] {
            return Ok(());
        }
        let center = [(lo[0] + hi[0]) * 0.5, (lo[1] + hi[1]) * 0.5];
        let radius = [(hi[0] - lo[0]) * 0.5, (hi[1] - lo[1]) * 0.5];
        for y in 0..self.height {
            for x in 0..self.width {
                let px = x as f64 + 0.5;
                let py = y as f64 + 0.5;
                let inside = if ellipse {
                    ((px - center[0]) / radius[0]).powi(2) + ((py - center[1]) / radius[1]).powi(2)
                        <= 1.0
                } else {
                    px >= lo[0] && px < hi[0] && py >= lo[1] && py < hi[1]
                };
                self.pixels[(y * self.width + x) as usize] = if inside { 255 } else { 0 };
            }
        }
        Ok(())
    }
    /// Even-odd polygon fill, evaluated at pixel centers. Self intersections are allowed.
    pub fn lasso(&mut self, points: &[[f64; 2]]) -> Result<()> {
        if points.len() < 3
            || points.len() > 65_536
            || !points.iter().flatten().all(|v| v.is_finite())
        {
            return Err("lasso needs 3 through 65536 finite points".into());
        }
        for y in 0..self.height {
            let py = y as f64 + 0.5;
            for x in 0..self.width {
                let px = x as f64 + 0.5;
                let mut inside = false;
                let mut a = points[points.len() - 1];
                for &b in points {
                    if (a[1] > py) != (b[1] > py)
                        && px < (b[0] - a[0]) * (py - a[1]) / (b[1] - a[1]) + a[0]
                    {
                        inside = !inside;
                    }
                    a = b;
                }
                self.pixels[(y * self.width + x) as usize] = if inside { 255 } else { 0 };
            }
        }
        Ok(())
    }
    /// Four-connected color flood fill. Tolerance is the maximum per-channel difference.
    /// The source callback can read a bounded tile cache instead of a full canvas bitmap.
    pub fn magic_wand(
        &mut self,
        seed: [u32; 2],
        tolerance: u8,
        mut pixel: impl FnMut(u32, u32) -> [u8; 4],
    ) -> Result<()> {
        if seed[0] >= self.width || seed[1] >= self.height {
            return Err("selection seed is outside the canvas".into());
        }
        // Existing mask + visited bytes + a worst-case u32 queue, no reallocations.
        self.scratch(6)?;
        let mut visited = vec![false; self.pixels.len()];
        let mut queue = Vec::with_capacity(self.pixels.len());
        let target = pixel(seed[0], seed[1]);
        self.clear();
        let start = seed[1] * self.width + seed[0];
        queue.push(start);
        visited[start as usize] = true;
        let mut head = 0;
        while head < queue.len() {
            let index = queue[head];
            head += 1;
            let x = index % self.width;
            let y = index / self.width;
            if pixel(x, y)
                .into_iter()
                .zip(target)
                .any(|(a, b)| a.abs_diff(b) > tolerance)
            {
                continue;
            }
            self.pixels[index as usize] = 255;
            let neighbors = [
                x.checked_sub(1).map(|n| y * self.width + n),
                (x + 1 < self.width).then_some(index + 1),
                y.checked_sub(1).map(|n| n * self.width + x),
                (y + 1 < self.height).then_some(index + self.width),
            ];
            for next in neighbors.into_iter().flatten() {
                if !visited[next as usize] {
                    visited[next as usize] = true;
                    queue.push(next);
                }
            }
        }
        Ok(())
    }
    /// Separable box feather, transparent outside the canvas, rounded after each pass.
    pub fn feather(&mut self, radius: u32) -> Result<()> {
        if radius == 0 {
            return Ok(());
        }
        if radius > 4096 {
            return Err("feather radius exceeds 4096 pixels".into());
        }
        self.scratch(2)?;
        let mut scratch = vec![0; self.pixels.len()];
        box_pass(
            &self.pixels,
            &mut scratch,
            self.width as usize,
            self.height as usize,
            radius as usize,
            false,
        );
        box_pass(
            &scratch,
            &mut self.pixels,
            self.width as usize,
            self.height as usize,
            radius as usize,
            true,
        );
        Ok(())
    }
    /// Square structuring element. A matching shrink is its grayscale dual.
    pub fn grow(&mut self, radius: u32) -> Result<()> {
        self.morphology(radius, true)
    }
    pub fn shrink(&mut self, radius: u32) -> Result<()> {
        self.morphology(radius, false)
    }
    fn morphology(&mut self, radius: u32, grow: bool) -> Result<()> {
        if radius == 0 {
            return Ok(());
        }
        if radius > 4096 {
            return Err("selection radius exceeds 4096 pixels".into());
        }
        // Two masks plus at most one line's deque of (coordinate,value).
        let extra = self.width.max(self.height) as usize * std::mem::size_of::<(usize, u8)>();
        if self
            .pixels
            .len()
            .checked_mul(2)
            .and_then(|n| n.checked_add(extra))
            .is_none_or(|n| n > self.budget)
        {
            return Err("selection operation exceeds its memory budget".into());
        }
        let mut scratch = vec![0; self.pixels.len()];
        morph_pass(
            &self.pixels,
            &mut scratch,
            self.width as usize,
            self.height as usize,
            radius as usize,
            false,
            grow,
        );
        morph_pass(
            &scratch,
            &mut self.pixels,
            self.width as usize,
            self.height as usize,
            radius as usize,
            true,
            grow,
        );
        Ok(())
    }
}
fn box_pass(
    source: &[u8],
    destination: &mut [u8],
    width: usize,
    height: usize,
    radius: usize,
    vertical: bool,
) {
    let (lines, length) = if vertical {
        (width, height)
    } else {
        (height, width)
    };
    let denominator = 2 * radius + 1;
    for line in 0..lines {
        let index = |p| {
            if vertical {
                p * width + line
            } else {
                line * width + p
            }
        };
        let mut sum: usize = (0..=radius.min(length - 1))
            .map(|p| source[index(p)] as usize)
            .sum();
        for position in 0..length {
            destination[index(position)] = ((sum + denominator / 2) / denominator) as u8;
            if position >= radius {
                sum -= source[index(position - radius)] as usize;
            }
            if position + radius + 1 < length {
                sum += source[index(position + radius + 1)] as usize;
            }
        }
    }
}
fn morph_pass(
    source: &[u8],
    destination: &mut [u8],
    width: usize,
    height: usize,
    radius: usize,
    vertical: bool,
    grow: bool,
) {
    let (lines, length) = if vertical {
        (width, height)
    } else {
        (height, width)
    };
    for line in 0..lines {
        let index = |p| {
            if vertical {
                p * width + line
            } else {
                line * width + p
            }
        };
        let mut deque: Vec<(usize, u8)> = Vec::with_capacity(length);
        let mut head = 0;
        let mut next = 0;
        for position in 0..length {
            while next <= (position + radius).min(length - 1) {
                let value = source[index(next)];
                while deque.len() > head
                    && deque
                        .last()
                        .is_some_and(|&(_, v)| if grow { v <= value } else { v >= value })
                {
                    deque.pop();
                }
                deque.push((next, value));
                next += 1;
            }
            while deque
                .get(head)
                .is_some_and(|&(p, _)| p.saturating_add(radius) < position)
            {
                head += 1;
            }
            destination[index(position)] =
                if !grow && (position < radius || position + radius >= length) {
                    0
                } else {
                    deque.get(head).map_or(0, |&(_, v)| v)
                };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn mask() -> SelectionMask {
        SelectionMask::new(17, 13, 32768).unwrap()
    }
    #[test]
    fn rectangle_lasso_and_ellipse_match_scalar_pixel_center_reference() {
        let mut a = mask();
        let mut b = mask();
        a.rectangle([2.0, 3.0], [12.0, 10.0]).unwrap();
        b.lasso(&[[2.0, 3.0], [12.0, 3.0], [12.0, 10.0], [2.0, 10.0]])
            .unwrap();
        assert_eq!(a, b);
        assert_eq!(a.pixels.iter().filter(|&&v| v == 255).count(), 70);
        a.ellipse([2.0, 3.0], [12.0, 10.0]).unwrap();
        for y in 0..13 {
            for x in 0..17 {
                let expected = ((x as f64 + 0.5 - 7.0) / 5.0).powi(2)
                    + ((y as f64 + 0.5 - 6.5) / 3.5).powi(2)
                    <= 1.0;
                assert_eq!(a.coverage(x, y) > 0, expected);
            }
        }
    }
    #[test]
    fn selection_invert_is_involution_and_intersection_is_closed_commutative_idempotent() {
        let mut a = mask();
        a.ellipse([1.0, 1.0], [16.0, 12.0]).unwrap();
        a.feather(2).unwrap();
        let original = a.clone();
        a.invert();
        a.invert();
        assert_eq!(a, original);
        let mut b = mask();
        b.rectangle([4.0, 0.0], [10.0, 13.0]).unwrap();
        let mut ab = a.clone();
        ab.intersect(&b).unwrap();
        b.intersect(&a).unwrap();
        assert_eq!(ab, b);
        a.intersect(&original).unwrap();
        assert_eq!(a, original);
    }
    #[test]
    fn feather_grow_and_shrink_match_naive_reference_including_canvas_edges() {
        let mut original = mask();
        original.rectangle([4.0, 3.0], [11.0, 9.0]).unwrap();
        for radius in [1, 3, 20] {
            for grow in [false, true] {
                let mut actual = original.clone();
                actual.morphology(radius, grow).unwrap();
                for y in 0..13 {
                    for x in 0..17 {
                        let mut value = if grow { 0 } else { 255 };
                        for dy in -(radius as i32)..=radius as i32 {
                            for dx in -(radius as i32)..=radius as i32 {
                                let sample = if x + dx < 0 || y + dy < 0 {
                                    0
                                } else {
                                    original.coverage((x + dx) as u32, (y + dy) as u32)
                                };
                                value = if grow {
                                    value.max(sample)
                                } else {
                                    value.min(sample)
                                };
                            }
                        }
                        assert_eq!(actual.coverage(x as u32, y as u32), value);
                    }
                }
            }
        }
        let mut feather = original.clone();
        feather.feather(1).unwrap();
        assert_eq!(feather.coverage(5, 4), 255);
        assert_eq!(feather.coverage(3, 2), 28);
    }
    #[test]
    fn magic_wand_respects_connectivity_tolerance_and_memory_ceiling() {
        let mut selection = mask();
        selection
            .magic_wand([1, 1], 5, |x, _| if x == 8 { [100; 4] } else { [4; 4] })
            .unwrap();
        assert_eq!(selection.coverage(7, 12), 255);
        assert_eq!(selection.coverage(9, 12), 0);
        let mut tiny = SelectionMask::new(8, 8, 64).unwrap();
        assert!(tiny.feather(1).is_err());
        assert!(tiny.grow(1).is_err());
        assert!(tiny.magic_wand([0, 0], 0, |_, _| [0; 4]).is_err());
        assert!(tiny.rectangle([f64::NAN, 0.0], [1.0, 1.0]).is_err());
        assert!(SelectionMask::new(u32::MAX, u32::MAX, usize::MAX).is_err());
    }
}
