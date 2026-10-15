//! Inverse-mapped transforms with bounded tile output and premultiplied sampling.
pub type Pixel = [f32; 4];
type Result<T> = std::result::Result<T, String>;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transform {
    matrix: [[f64; 3]; 3],
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sampling {
    Nearest,
    Bilinear,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Region {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}
impl Transform {
    pub const IDENTITY: Self = Self {
        matrix: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
    };
    pub fn from_matrix(matrix: [[f64; 3]; 3]) -> Result<Self> {
        if !matrix.iter().flatten().all(|v| v.is_finite()) {
            return Err("transform must be finite".into());
        }
        let transform = Self { matrix };
        transform.inverse()?;
        Ok(transform)
    }
    pub fn matrix(self) -> [[f64; 3]; 3] {
        self.matrix
    }
    pub fn translation(x: f64, y: f64) -> Result<Self> {
        Self::from_matrix([[1.0, 0.0, x], [0.0, 1.0, y], [0.0, 0.0, 1.0]])
    }
    pub fn scale(x: f64, y: f64) -> Result<Self> {
        Self::from_matrix([[x, 0.0, 0.0], [0.0, y, 0.0], [0.0, 0.0, 1.0]])
    }
    pub fn rotation(radians: f64) -> Result<Self> {
        let (sin, cos) = radians.sin_cos();
        Self::from_matrix([[cos, -sin, 0.0], [sin, cos, 0.0], [0.0, 0.0, 1.0]])
    }
    /// Apply `self`, then `next`.
    pub fn then(self, next: Self) -> Result<Self> {
        let mut matrix = [[0.0; 3]; 3];
        for (row, values) in matrix.iter_mut().enumerate() {
            for (column, value) in values.iter_mut().enumerate() {
                *value = (0..3)
                    .map(|k| next.matrix[row][k] * self.matrix[k][column])
                    .sum();
            }
        }
        Self::from_matrix(matrix)
    }
    pub fn around(self, point: [f64; 2]) -> Result<Self> {
        Self::translation(-point[0], -point[1])?
            .then(self)?
            .then(Self::translation(point[0], point[1])?)
    }
    pub fn map(self, point: [f64; 2]) -> Option<[f64; 2]> {
        let m = self.matrix;
        let w = m[2][0] * point[0] + m[2][1] * point[1] + m[2][2];
        if !w.is_finite() || w.abs() < 1e-12 {
            return None;
        }
        let result = [
            (m[0][0] * point[0] + m[0][1] * point[1] + m[0][2]) / w,
            (m[1][0] * point[0] + m[1][1] * point[1] + m[1][2]) / w,
        ];
        result.iter().all(|v| v.is_finite()).then_some(result)
    }
    pub fn inverse(self) -> Result<Self> {
        let m = self.matrix;
        let cofactor = [
            [
                m[1][1] * m[2][2] - m[1][2] * m[2][1],
                m[1][2] * m[2][0] - m[1][0] * m[2][2],
                m[1][0] * m[2][1] - m[1][1] * m[2][0],
            ],
            [
                m[0][2] * m[2][1] - m[0][1] * m[2][2],
                m[0][0] * m[2][2] - m[0][2] * m[2][0],
                m[0][1] * m[2][0] - m[0][0] * m[2][1],
            ],
            [
                m[0][1] * m[1][2] - m[0][2] * m[1][1],
                m[0][2] * m[1][0] - m[0][0] * m[1][2],
                m[0][0] * m[1][1] - m[0][1] * m[1][0],
            ],
        ];
        let determinant: f64 = (0..3).map(|i| m[0][i] * cofactor[0][i]).sum();
        if !determinant.is_finite() || determinant.abs() < 1e-12 {
            return Err("transform is singular or too ill-conditioned".into());
        }
        let matrix = std::array::from_fn(|r| std::array::from_fn(|c| cofactor[c][r] / determinant));
        if !matrix.iter().flatten().all(|v| v.is_finite()) {
            return Err("transform inverse overflow".into());
        }
        Ok(Self { matrix })
    }
    /// Homography mapping four source corners to four destination corners.
    /// Degenerate quadrilaterals are rejected by pivoting and inverse validation.
    pub fn free_distort(source: [[f64; 2]; 4], destination: [[f64; 2]; 4]) -> Result<Self> {
        if !source
            .iter()
            .chain(&destination)
            .flatten()
            .all(|v| v.is_finite())
        {
            return Err("transform corners must be finite".into());
        }
        let mut rows = [[0.0; 9]; 8];
        for (i, ([x, y], [u, v])) in source.into_iter().zip(destination).enumerate() {
            rows[2 * i] = [x, y, 1.0, 0.0, 0.0, 0.0, -u * x, -u * y, u];
            rows[2 * i + 1] = [0.0, 0.0, 0.0, x, y, 1.0, -v * x, -v * y, v];
        }
        for column in 0..8 {
            let pivot = (column..8)
                .max_by(|&a, &b| rows[a][column].abs().total_cmp(&rows[b][column].abs()))
                .unwrap();
            rows.swap(column, pivot);
            let value = rows[column][column];
            if !value.is_finite() || value.abs() < 1e-12 {
                return Err("transform corners are degenerate".into());
            }
            rows[column].iter_mut().for_each(|v| *v /= value);
            let normalized = rows[column];
            for (r, row) in rows.iter_mut().enumerate() {
                if r != column {
                    let factor = row[column];
                    for c in column..9 {
                        row[c] -= factor * normalized[c];
                    }
                }
            }
        }
        let h: [f64; 8] = std::array::from_fn(|i| rows[i][8]);
        Self::from_matrix([[h[0], h[1], h[2]], [h[3], h[4], h[5]], [h[6], h[7], 1.0]])
    }
}
/// A sampler returns transparent for coordinates outside the source canvas.
/// It can read sparse tiles without materializing a whole image.
pub fn render_region(
    transform: Transform,
    region: Region,
    sampling: Sampling,
    budget: usize,
    mut source: impl FnMut(i64, i64) -> Pixel,
) -> Result<Vec<Pixel>> {
    let count = (region.width as usize)
        .checked_mul(region.height as usize)
        .filter(|n| {
            n.checked_mul(std::mem::size_of::<Pixel>())
                .is_some_and(|bytes| bytes <= budget)
        })
        .ok_or("transform tile exceeds its memory budget")?;
    if region.width == 0
        || region.height == 0
        || region.x.checked_add(region.width).is_none()
        || region.y.checked_add(region.height).is_none()
    {
        return Err("invalid transform output region".into());
    }
    let inverse = transform.inverse()?;
    let mut output = Vec::with_capacity(count);
    for y in region.y..region.y + region.height {
        for x in region.x..region.x + region.width {
            let Some([sx, sy]) = inverse.map([x as f64 + 0.5, y as f64 + 0.5]) else {
                output.push([0.0; 4]);
                continue;
            };
            // Prevent saturating casts from repeatedly sampling an extreme edge.
            if sx.abs() > i32::MAX as f64 || sy.abs() > i32::MAX as f64 {
                output.push([0.0; 4]);
                continue;
            }
            output.push(match sampling {
                Sampling::Nearest => source(sx.floor() as i64, sy.floor() as i64),
                Sampling::Bilinear => {
                    let px = sx - 0.5;
                    let py = sy - 0.5;
                    let ix = px.floor() as i64;
                    let iy = py.floor() as i64;
                    let tx = (px - px.floor()) as f32;
                    let ty = (py - py.floor()) as f32;
                    let samples = [
                        source(ix, iy),
                        source(ix + 1, iy),
                        source(ix, iy + 1),
                        source(ix + 1, iy + 1),
                    ];
                    std::array::from_fn(|c| {
                        (samples[0][c] * (1.0 - tx) + samples[1][c] * tx) * (1.0 - ty)
                            + (samples[2][c] * (1.0 - tx) + samples[3][c] * tx) * ty
                    })
                }
            });
        }
    }
    Ok(output)
}
/// Streams one tile at a time; callers must budget any retained callback data.
pub fn render_tiles(
    transform: Transform,
    canvas: [u32; 2],
    edge: u32,
    sampling: Sampling,
    budget: usize,
    mut source: impl FnMut(i64, i64) -> Pixel,
    mut consume: impl FnMut(Region, &[Pixel]) -> Result<()>,
) -> Result<()> {
    if canvas.contains(&0) || edge == 0 {
        return Err("transform canvas and tile edge must be nonzero".into());
    }
    for y in (0..canvas[1]).step_by(edge as usize) {
        for x in (0..canvas[0]).step_by(edge as usize) {
            let region = Region {
                x,
                y,
                width: edge.min(canvas[0] - x),
                height: edge.min(canvas[1] - y),
            };
            let pixels = render_region(transform, region, sampling, budget, &mut source)?;
            consume(region, &pixels)?;
        }
    }
    Ok(())
}
pub fn selected(pixel: Pixel, coverage: u8) -> Pixel {
    pixel.map(|v| v * (coverage as f32 / 255.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn source(x: i64, y: i64) -> Pixel {
        if (0..8).contains(&x) && (0..6).contains(&y) {
            [x as f32 / 8.0, y as f32 / 6.0, 0.25, 1.0]
        } else {
            [0.0; 4]
        }
    }
    #[test]
    fn integer_move_and_flip_round_trips_are_lossless() {
        let region = Region {
            x: 0,
            y: 0,
            width: 8,
            height: 6,
        };
        for transform in [
            Transform::translation(3.0, -2.0).unwrap(),
            Transform::scale(-1.0, 1.0)
                .unwrap()
                .then(Transform::translation(8.0, 0.0).unwrap())
                .unwrap(),
        ] {
            let roundtrip = transform.then(transform.inverse().unwrap()).unwrap();
            let pixels = render_region(roundtrip, region, Sampling::Nearest, 768, source).unwrap();
            for y in 0..6 {
                for x in 0..8 {
                    assert_eq!(pixels[y * 8 + x], source(x as i64, y as i64));
                }
            }
        }
        let flip = Transform::scale(-1.0, 1.0)
            .unwrap()
            .then(Transform::translation(8.0, 0.0).unwrap())
            .unwrap();
        let pixels = render_region(flip, region, Sampling::Nearest, 768, source).unwrap();
        assert_eq!(pixels[0], source(7, 0));
        assert_eq!(pixels[7], source(0, 0));
    }
    #[test]
    fn fractional_bilinear_transform_matches_analytic_reference() {
        let result = render_region(
            Transform::translation(0.5, 0.25).unwrap(),
            Region {
                x: 3,
                y: 3,
                width: 1,
                height: 1,
            },
            Sampling::Bilinear,
            16,
            source,
        )
        .unwrap();
        assert!((result[0][0] - 2.5 / 8.0).abs() < 1e-6);
        assert!((result[0][1] - 2.75 / 6.0).abs() < 1e-6);
        assert_eq!(result[0][3], 1.0);
    }
    #[test]
    fn free_distort_maps_all_four_corners_and_composition_is_associative() {
        let src = [[0.0, 0.0], [8.0, 0.0], [8.0, 6.0], [0.0, 6.0]];
        let dst = [[2.0, 1.0], [9.0, 2.0], [7.0, 9.0], [1.0, 5.0]];
        let a = Transform::free_distort(src, dst).unwrap();
        for (p, q) in src.into_iter().zip(dst) {
            let actual = a.map(p).unwrap();
            for c in 0..2 {
                assert!((actual[c] - q[c]).abs() < 1e-10);
            }
        }
        for n in 1..100 {
            let b = Transform::rotation(n as f64 * 0.07).unwrap();
            let c = Transform::translation(n as f64, -2.0).unwrap();
            let left = a.then(b).unwrap().then(c).unwrap().map([3.0, 4.0]).unwrap();
            let right = a.then(b.then(c).unwrap()).unwrap().map([3.0, 4.0]).unwrap();
            assert!((left[0] - right[0]).abs() < 1e-9 && (left[1] - right[1]).abs() < 1e-9);
        }
    }
    #[test]
    fn transform_streams_under_tile_ceiling_and_rejects_degenerate_inputs() {
        let mut count = 0;
        render_tiles(
            Transform::IDENTITY,
            [8, 6],
            2,
            Sampling::Nearest,
            64,
            source,
            |region, pixels| {
                assert!(pixels.len() * 16 <= 64);
                count += region.width * region.height;
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(count, 48);
        assert!(Transform::scale(0.0, 1.0).is_err());
        assert!(Transform::free_distort([[0.0; 2]; 4], [[1.0; 2]; 4]).is_err());
        assert!(render_region(
            Transform::IDENTITY,
            Region {
                x: 0,
                y: 0,
                width: 8,
                height: 6
            },
            Sampling::Nearest,
            767,
            source
        )
        .is_err());
    }
}
