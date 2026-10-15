//! Immutable source assets rendered at the requested resolution.
use image::ImageReader;
use sideduck_composite::color::{rgba8_srgb_to_linear_premultiplied, srgb_to_linear};
use sideduck_composite::Pixel;
use sideduck_platform::PdfRasterizer;
use std::{io::Cursor, path::Path};
type Result<T> = std::result::Result<T, String>;
const MAX_SOURCE: usize = 64 * 1024 * 1024;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReferenceKind {
    Image,
    Pdf,
}
#[derive(Clone, Debug)]
pub struct ReferenceSource {
    bytes: Vec<u8>,
    kind: ReferenceKind,
    dimensions: [f64; 2],
    page: u32,
}
pub struct ReferenceRaster {
    pub dimensions: [u32; 2],
    pub pixels: Vec<Pixel>,
}
impl ReferenceSource {
    pub fn open(path: &Path, page: u32, pdf: &dyn PdfRasterizer) -> Result<Self> {
        let metadata = std::fs::metadata(path).map_err(|e| e.to_string())?;
        if !metadata.is_file() || metadata.len() > MAX_SOURCE as u64 {
            return Err("reference source exceeds64MiB or is not a file".into());
        }
        Self::from_bytes(std::fs::read(path).map_err(|e| e.to_string())?, page, pdf)
    }
    pub fn from_bytes(bytes: Vec<u8>, page: u32, pdf: &dyn PdfRasterizer) -> Result<Self> {
        if bytes.len() > MAX_SOURCE {
            return Err("reference source exceeds64MiB".into());
        }
        let (kind, dimensions) = if bytes.starts_with(b"%PDF-") {
            (
                ReferenceKind::Pdf,
                pdf.page_size(&bytes, page).map_err(|e| e.to_string())?,
            )
        } else {
            if page != 0 {
                return Err("raster references contain one image".into());
            }
            let (w, h) = ImageReader::new(Cursor::new(&bytes))
                .with_guessed_format()
                .map_err(|e| e.to_string())?
                .into_dimensions()
                .map_err(|e| e.to_string())?;
            (ReferenceKind::Image, [w as f64, h as f64])
        };
        if dimensions.iter().any(|v| !v.is_finite() || *v <= 0.0) {
            return Err("reference dimensions are invalid".into());
        }
        Ok(Self {
            bytes,
            kind,
            dimensions,
            page,
        })
    }
    pub fn kind(&self) -> ReferenceKind {
        self.kind
    }
    pub fn dimensions(&self) -> [f64; 2] {
        self.dimensions
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub fn page(&self) -> u32 {
        self.page
    }
    pub fn media_type(&self) -> &'static str {
        match self.kind {
            ReferenceKind::Pdf => "application/pdf",
            ReferenceKind::Image => "image/raster",
        }
    }
    /// Original pixels/PDF operators remain unchanged. Output is linear premultiplied.
    pub fn render(
        &self,
        size: [u32; 2],
        budget: usize,
        pdf: &dyn PdfRasterizer,
    ) -> Result<ReferenceRaster> {
        let count = (size[0] as usize)
            .checked_mul(size[1] as usize)
            .filter(|_| !size.contains(&0))
            .ok_or("invalid reference render size")?;
        let output_bytes = count
            .checked_mul(16)
            .ok_or("reference output size overflow")?;
        let extra = match self.kind {
            ReferenceKind::Pdf => count.checked_mul(4),
            ReferenceKind::Image => (self.dimensions[0] as usize)
                .checked_mul(self.dimensions[1] as usize)
                .and_then(|n| n.checked_mul(16)),
        }
        .ok_or("reference decoder size overflow")?;
        if output_bytes
            .checked_add(extra)
            .and_then(|n| n.checked_add(self.bytes.len()))
            .is_none_or(|n| n > budget)
        {
            return Err("reference render exceeds its memory budget".into());
        }
        let pixels = match self.kind {
            ReferenceKind::Pdf => {
                let image = pdf
                    .render_page(&self.bytes, self.page, size, extra)
                    .map_err(|e| e.to_string())?;
                image.validate(extra).map_err(|e| e.to_string())?;
                if [image.width, image.height] != size {
                    return Err("PDF rasterizer returned a different size".into());
                }
                image
                    .rgba8
                    .chunks_exact(4)
                    .map(|p| {
                        let a = p[3] as f32 / 255.0;
                        if a == 0.0 {
                            [0.0; 4]
                        } else {
                            [
                                srgb_to_linear(p[0] as f32 / 255.0 / a) * a,
                                srgb_to_linear(p[1] as f32 / 255.0 / a) * a,
                                srgb_to_linear(p[2] as f32 / 255.0 / a) * a,
                                a,
                            ]
                        }
                    })
                    .collect()
            }
            ReferenceKind::Image => {
                let mut limits = image::Limits::default();
                limits.max_alloc = Some(extra as u64);
                limits.max_image_width = Some(self.dimensions[0] as u32);
                limits.max_image_height = Some(self.dimensions[1] as u32);
                let mut reader = ImageReader::new(Cursor::new(&self.bytes))
                    .with_guessed_format()
                    .map_err(|e| e.to_string())?;
                reader.limits(limits);
                let image = reader.decode().map_err(|e| e.to_string())?.into_rgba8();
                let mut pixels = Vec::with_capacity(count);
                for y in 0..size[1] {
                    for x in 0..size[0] {
                        let sx = ((x as f64 + 0.5) * image.width() as f64 / size[0] as f64 - 0.5)
                            .clamp(0.0, (image.width() - 1) as f64);
                        let sy = ((y as f64 + 0.5) * image.height() as f64 / size[1] as f64 - 0.5)
                            .clamp(0.0, (image.height() - 1) as f64);
                        let ix = sx.floor() as u32;
                        let iy = sy.floor() as u32;
                        let tx = (sx - sx.floor()) as f32;
                        let ty = (sy - sy.floor()) as f32;
                        let sample =
                            |px, py| rgba8_srgb_to_linear_premultiplied(image.get_pixel(px, py).0);
                        let a = sample(ix, iy);
                        let b = sample((ix + 1).min(image.width() - 1), iy);
                        let c = sample(ix, (iy + 1).min(image.height() - 1));
                        let d = sample(
                            (ix + 1).min(image.width() - 1),
                            (iy + 1).min(image.height() - 1),
                        );
                        pixels.push(std::array::from_fn(|channel| {
                            (a[channel] * (1.0 - tx) + b[channel] * tx) * (1.0 - ty)
                                + (c[channel] * (1.0 - tx) + d[channel] * tx) * ty
                        }));
                    }
                }
                pixels
            }
        };
        Ok(ReferenceRaster {
            dimensions: size,
            pixels,
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    struct Pdf;
    impl PdfRasterizer for Pdf {
        fn page_count(&self, _: &[u8]) -> sideduck_platform::PlatformResult<u32> {
            Ok(1)
        }
        fn page_size(&self, _: &[u8], _: u32) -> sideduck_platform::PlatformResult<[f64; 2]> {
            Ok([2.0, 2.0])
        }
        fn render_page(
            &self,
            _: &[u8],
            _: u32,
            size: [u32; 2],
            _: usize,
        ) -> sideduck_platform::PlatformResult<sideduck_platform::CapturedImage> {
            Ok(sideduck_platform::CapturedImage {
                width: size[0],
                height: size[1],
                rgba8: [128, 0, 0, 128].repeat((size[0] * size[1]) as usize),
            })
        }
    }
    #[test]
    fn original_raster_source_survives_zoom_and_sampling_is_linear() {
        let mut encoded = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(
            image::RgbaImage::from_raw(2, 1, vec![0, 0, 0, 255, 255, 255, 255, 255]).unwrap(),
        )
        .write_to(&mut encoded, image::ImageFormat::Png)
        .unwrap();
        let original = encoded.into_inner();
        let source = ReferenceSource::from_bytes(original.clone(), 0, &Pdf).unwrap();
        let render = source.render([1, 1], 4096, &Pdf).unwrap();
        assert_eq!(render.pixels, vec![[0.5, 0.5, 0.5, 1.0]]);
        source.render([40, 20], 65536, &Pdf).unwrap();
        assert_eq!(source.bytes(), original);
        assert!(source.render([8192, 8192], 4096, &Pdf).is_err());
    }
    #[test]
    fn pdf_reference_rerenders_at_current_zoom_and_converts_encoded_premultiplied_alpha() {
        let source = ReferenceSource::from_bytes(b"%PDF-fixture".to_vec(), 0, &Pdf).unwrap();
        for size in [[2, 2], [20, 20]] {
            let render = source.render(size, 65536, &Pdf).unwrap();
            assert_eq!(render.dimensions, size);
            for pixel in render.pixels {
                assert!((pixel[0] - 128.0 / 255.0).abs() < 1e-6);
                assert_eq!(pixel[0], pixel[3]);
            }
        }
    }
}
