//! Atomic raster exports and PDF annotations that retain original page content.
use image::{DynamicImage, ImageBuffer, ImageFormat};
use lopdf::{dictionary, Dictionary, Document, Object, ObjectId, Stream};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

type Result<T> = std::result::Result<T, String>;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportFormat {
    Png,
    Tiff,
    Jpeg,
    Pdf,
}
impl ExportFormat {
    pub fn parse(value: &str) -> Result<Self> {
        match value.to_ascii_lowercase().as_str() {
            "png" => Ok(Self::Png),
            "tiff" | "tif" => Ok(Self::Tiff),
            "jpeg" | "jpg" => Ok(Self::Jpeg),
            "pdf" => Ok(Self::Pdf),
            _ => Err("unsupported export format".into()),
        }
    }
}
/// Zero-based source page. Keep the original bytes as a document source object.
pub struct PdfSource<'a> {
    pub bytes: &'a [u8],
    pub page: u32,
}
/// Input bytes are unassociated sRGB RGBA. Raster formats retain alpha except JPEG,
/// whose transparent pixels are composited onto white in linear light.
pub fn export(
    path: &Path,
    format: ExportFormat,
    dimensions: [u32; 2],
    rgba: Vec<u8>,
    budget: usize,
    source: Option<PdfSource<'_>>,
    separate_annotations: bool,
) -> Result<()> {
    let count = (dimensions[0] as usize)
        .checked_mul(dimensions[1] as usize)
        .filter(|_| !dimensions.contains(&0))
        .ok_or("invalid export dimensions")?;
    let source_bytes = source.as_ref().map_or(0, |s| s.bytes.len());
    if count.checked_mul(4) != Some(rgba.len()) {
        return Err("export pixel count does not match dimensions".into());
    }
    // Conservative live-payload allowance for input, channel extraction, encoder
    // scratch and compressed streams; reject before making additional allocations.
    if count
        .checked_mul(24)
        .and_then(|n| source_bytes.checked_mul(4).and_then(|s| n.checked_add(s)))
        .is_none_or(|n| n > budget)
    {
        return Err("export exceeds its memory budget".into());
    }
    atomic_output(path, |file| match format {
        ExportFormat::Pdf => write_pdf(file, dimensions, &rgba, source, separate_annotations),
        ExportFormat::Png | ExportFormat::Tiff => {
            let buffer = ImageBuffer::from_raw(dimensions[0], dimensions[1], rgba)
                .ok_or("invalid export buffer")?;
            DynamicImage::ImageRgba8(buffer)
                .write_to(
                    file,
                    if format == ExportFormat::Png {
                        ImageFormat::Png
                    } else {
                        ImageFormat::Tiff
                    },
                )
                .map_err(|e| e.to_string())
        }
        ExportFormat::Jpeg => {
            let mut rgb = Vec::with_capacity(count * 3);
            for pixel in rgba.chunks_exact(4) {
                let alpha = pixel[3] as f32 / 255.0;
                for &channel in &pixel[..3] {
                    let linear = sideduck_composite::color::srgb_to_linear(channel as f32 / 255.0);
                    let encoded =
                        sideduck_composite::color::linear_to_srgb(linear * alpha + 1.0 - alpha);
                    rgb.push((encoded * 255.0).round().clamp(0.0, 255.0) as u8);
                }
            }
            image::codecs::jpeg::JpegEncoder::new_with_quality(file, 95)
                .encode(
                    &rgb,
                    dimensions[0],
                    dimensions[1],
                    image::ExtendedColorType::Rgb8,
                )
                .map_err(|e| e.to_string())
        }
    })
}
fn atomic_output(path: &Path, write: impl FnOnce(&mut File) -> Result<()>) -> Result<()> {
    static SERIAL: AtomicU64 = AtomicU64::new(0);
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    if parent.ancestors().any(|ancestor| {
        ancestor
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("pond"))
    }) {
        return Err("Export outside the document package to protect its contents".into());
    }
    if path.exists() {
        return Err("Export destination already exists; choose a new file name".into());
    }
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let name = path
        .file_name()
        .ok_or("export needs a file name")?
        .to_string_lossy();
    let temp = parent.join(format!(
        ".{name}.export-{}-{}",
        std::process::id(),
        SERIAL.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .read(true)
            .create_new(true)
            .open(&temp)
            .map_err(|e| e.to_string())?;
        write(&mut file)?;
        file.flush()
            .and_then(|_| file.sync_all())
            .map_err(|e| e.to_string())?;
        // Atomically publish without replacing a destination created after the check.
        fs::hard_link(&temp, path).map_err(|e| e.to_string())?;
        fs::remove_file(&temp).map_err(|e| e.to_string())?;
        File::open(parent)
            .and_then(|f| f.sync_all())
            .map_err(|e| e.to_string())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}
fn inherited(document: &Document, mut page: ObjectId, key: &[u8]) -> Result<Option<Object>> {
    let mut visited = std::collections::BTreeSet::new();
    loop {
        if !visited.insert(page) || visited.len() > 256 {
            return Err("PDF page tree is cyclic or too deep".into());
        }
        let dict = document.get_dictionary(page).map_err(|e| e.to_string())?;
        if let Ok(value) = dict.get(key) {
            return Ok(Some(
                document
                    .dereference(value)
                    .map_err(|e| e.to_string())?
                    .1
                    .clone(),
            ));
        }
        match dict.get(b"Parent").and_then(Object::as_reference) {
            Ok(parent) => page = parent,
            Err(_) => return Ok(None),
        }
    }
}
fn number(object: &Object) -> Result<f32> {
    match object {
        Object::Integer(value) => Ok(*value as f32),
        Object::Real(value) => Ok(*value),
        _ => Err("PDF page box must contain numbers".into()),
    }
}
fn dictionary_value(
    document: &Document,
    dictionary: &Dictionary,
    key: &[u8],
) -> Result<Dictionary> {
    match dictionary.get(key) {
        Ok(value) => document
            .dereference(value)
            .map_err(|e| e.to_string())?
            .1
            .as_dict()
            .cloned()
            .map_err(|e| e.to_string()),
        Err(_) => Ok(Dictionary::new()),
    }
}
fn write_pdf(
    file: &mut File,
    dimensions: [u32; 2],
    rgba: &[u8],
    source: Option<PdfSource<'_>>,
    separate: bool,
) -> Result<()> {
    let (mut document, page) = if let Some(source) = source {
        if source.bytes.len() > 256 * 1024 * 1024 {
            return Err("PDF source exceeds 256 MiB".into());
        }
        let mut document = Document::load_mem(source.bytes).map_err(|e| e.to_string())?;
        if document.is_encrypted() {
            return Err("encrypted PDF sources must be unlocked before import".into());
        }
        let pages = document.get_pages();
        let number = source
            .page
            .checked_add(1)
            .ok_or("PDF page index overflow")?;
        let page = *pages.get(&number).ok_or("PDF page does not exist")?;
        let others: Vec<_> = pages.keys().copied().filter(|n| *n != number).collect();
        document.delete_pages(&others);
        (document, page)
    } else {
        let mut document = Document::with_version("1.7");
        let pages = document.new_object_id();
        let page=document.add_object(dictionary!{"Type"=>"Page","Parent"=>pages,
            "MediaBox"=>vec![0.into(),0.into(),dimensions[0].into(),dimensions[1].into()],"Resources"=>dictionary!{}});
        document.objects.insert(
            pages,
            Object::Dictionary(dictionary! {"Type"=>"Pages","Kids"=>vec![page.into()],"Count"=>1}),
        );
        let catalog = document.add_object(dictionary! {"Type"=>"Catalog","Pages"=>pages});
        document.trailer.set("Root", catalog);
        (document, page)
    };
    let bounds = inherited(&document, page, b"CropBox")?
        .or(inherited(&document, page, b"MediaBox")?)
        .ok_or("PDF page has no bounds")?;
    let values = bounds.as_array().map_err(|e| e.to_string())?;
    if values.len() != 4 {
        return Err("PDF page bounds need four coordinates".into());
    }
    let bounds: Vec<_> = values.iter().map(number).collect::<Result<_>>()?;
    let [x, y, w, h] = [
        bounds[0],
        bounds[1],
        bounds[2] - bounds[0],
        bounds[3] - bounds[1],
    ];
    if ![x, y, w, h].into_iter().all(f32::is_finite) || w <= 0.0 || h <= 0.0 {
        return Err("invalid PDF page bounds".into());
    }
    let rotation = inherited(&document, page, b"Rotate")?
        .map_or(Ok(0), |v| v.as_i64().map_err(|e| e.to_string()))?
        .rem_euclid(360);
    if ![0, 90, 180, 270].contains(&rotation) {
        return Err("unsupported PDF page rotation".into());
    }
    let mut resources = inherited(&document, page, b"Resources")?
        .map_or(Ok(Dictionary::new()), |v| {
            v.as_dict().cloned().map_err(|e| e.to_string())
        })?;
    let mut objects = dictionary_value(&document, &resources, b"XObject")?;
    let mut serial = 0;
    let image_name = loop {
        let name = format!("AnnotationImage{serial}");
        if !objects.has(name.as_bytes()) {
            break name;
        }
        serial += 1;
    };
    let mut rgb = Vec::with_capacity(rgba.len() / 4 * 3);
    let mut alpha = Vec::with_capacity(rgba.len() / 4);
    for pixel in rgba.chunks_exact(4) {
        rgb.extend_from_slice(&pixel[..3]);
        alpha.push(pixel[3]);
    }
    let mut mask = Stream::new(
        dictionary! {"Type"=>"XObject","Subtype"=>"Image","Width"=>dimensions[0],"Height"=>dimensions[1],"ColorSpace"=>"DeviceGray","BitsPerComponent"=>8},
        alpha,
    );
    mask.compress().map_err(|e| e.to_string())?;
    let mask_id = document.add_object(mask);
    let mut image = Stream::new(
        dictionary! {"Type"=>"XObject","Subtype"=>"Image","Width"=>dimensions[0],"Height"=>dimensions[1],"ColorSpace"=>"DeviceRGB","BitsPerComponent"=>8,"SMask"=>mask_id},
        rgb,
    );
    image.compress().map_err(|e| e.to_string())?;
    let image_id = document.add_object(image);
    objects.set(image_name.clone(), image_id);
    resources.set("XObject", objects);
    let mut content = String::from("q\n");
    if separate {
        let group = document
            .add_object(dictionary! {"Type"=>"OCG","Name"=>Object::string_literal("Annotations")});
        let mut properties = dictionary_value(&document, &resources, b"Properties")?;
        let mut serial = 0;
        let name = loop {
            let name = format!("AnnotationLayer{serial}");
            if !properties.has(name.as_bytes()) {
                break name;
            }
            serial += 1;
        };
        properties.set(name.clone(), group);
        resources.set("Properties", properties);
        let catalog = document.catalog().map_err(|e| e.to_string())?.clone();
        let mut oc = dictionary_value(&document, &catalog, b"OCProperties")?;
        let mut groups = oc
            .get(b"OCGs")
            .and_then(Object::as_array)
            .cloned()
            .unwrap_or_default();
        groups.push(group.into());
        oc.set("OCGs", groups);
        let mut defaults = dictionary_value(&document, &oc, b"D")?;
        for key in ["Order", "ON"] {
            let mut values = defaults
                .get(key.as_bytes())
                .and_then(Object::as_array)
                .cloned()
                .unwrap_or_default();
            values.push(group.into());
            defaults.set(key, values);
        }
        oc.set("D", defaults);
        document
            .catalog_mut()
            .map_err(|e| e.to_string())?
            .set("OCProperties", oc);
        content.push_str(&format!("/OC /{name} BDC\n"));
    }
    let matrix = match rotation {
        0 => [w, 0.0, 0.0, h, x, y],
        90 => [0.0, h, -w, 0.0, x + w, y],
        180 => [-w, 0.0, 0.0, -h, x + w, y + h],
        270 => [0.0, -h, w, 0.0, x, y + h],
        _ => unreachable!(),
    };
    content.push_str(&format!(
        "{} {} {} {} {} {} cm\n/{image_name} Do\n",
        matrix[0], matrix[1], matrix[2], matrix[3], matrix[4], matrix[5]
    ));
    if separate {
        content.push_str("EMC\n");
    }
    content.push_str("Q\n");
    // Isolate any transform or clip left by the original page before adding
    // annotations. Original content streams remain byte-for-byte untouched.
    let original_contents = document.get_page_contents(page);
    if !original_contents.is_empty() {
        let begin = document.add_object(Stream::new(Dictionary::new(), b"q\n".to_vec()));
        let end = document.add_object(Stream::new(Dictionary::new(), b"Q\n".to_vec()));
        let mut contents = vec![Object::Reference(begin)];
        contents.extend(original_contents.into_iter().map(Object::Reference));
        contents.push(Object::Reference(end));
        document
            .get_object_mut(page)
            .and_then(Object::as_dict_mut)
            .map_err(|e| e.to_string())?
            .set("Contents", contents);
    }
    document
        .get_object_mut(page)
        .and_then(Object::as_dict_mut)
        .map_err(|e| e.to_string())?
        .set("Resources", resources);
    document
        .add_page_contents(page, content.into_bytes())
        .map_err(|e| e.to_string())?;
    document.prune_objects();
    document.save_to(file).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn png_and_tiff_roundtrip_alpha_and_jpeg_composites_transparency() {
        let temporary = tempfile::tempdir().unwrap();
        let directory = temporary.path();
        let pixels = [255, 0, 0, 128, 0, 0, 0, 0, 0, 255, 0, 255, 20, 30, 40, 255].to_vec();
        for (format, name) in [(ExportFormat::Png, "a.png"), (ExportFormat::Tiff, "a.tiff")] {
            let path = directory.join(name);
            export(&path, format, [2, 2], pixels.clone(), 4096, None, false).unwrap();
            assert_eq!(image::open(path).unwrap().to_rgba8().as_raw(), &pixels);
        }
        let path = directory.join("a.jpg");
        export(
            &path,
            ExportFormat::Jpeg,
            [2, 2],
            pixels.clone(),
            4096,
            None,
            false,
        )
        .unwrap();
        let decoded = image::open(path).unwrap();
        assert_eq!([decoded.width(), decoded.height()], [2, 2]);
        assert!(export(
            &directory.join("bad.png"),
            ExportFormat::Png,
            [2, 2],
            pixels,
            95,
            None,
            false
        )
        .is_err());
        assert!(!directory.join("bad.png").exists());
    }
    #[test]
    fn pdf_preserves_original_page_stream_and_exposes_annotations_as_optional_layer() {
        let temporary = tempfile::tempdir().unwrap();
        let directory = temporary.path();
        let first = directory.join("source.pdf");
        export(
            &first,
            ExportFormat::Pdf,
            [2, 2],
            [0, 0, 255, 255].repeat(4),
            4096,
            None,
            false,
        )
        .unwrap();
        let bytes = fs::read(&first).unwrap();
        let original = Document::load_mem(&bytes).unwrap();
        let original_page = original.get_pages()[&1];
        let original_content = original.get_page_content(original_page).unwrap();
        let output = directory.join("annotated.pdf");
        export(
            &output,
            ExportFormat::Pdf,
            [2, 2],
            [255, 0, 0, 100].repeat(4),
            65536,
            Some(PdfSource {
                bytes: &bytes,
                page: 0,
            }),
            true,
        )
        .unwrap();
        let result = Document::load(output).unwrap();
        let page = result.get_pages()[&1];
        let content = result.get_page_content(page).unwrap();
        assert!(content
            .windows(original_content.len())
            .any(|part| part == original_content));
        assert!(result.catalog().unwrap().has(b"OCProperties"));
        assert_eq!(result.get_page_images(page).unwrap().len(), 2);
        assert_eq!(fs::read(first).unwrap(), bytes);
    }
    #[test]
    fn export_never_overwrites_existing_files_or_writes_inside_document_packages() {
        let temp = tempfile::tempdir().unwrap();
        let existing = temp.path().join("important.png");
        fs::write(&existing, b"original bytes").unwrap();
        assert!(export(
            &existing,
            ExportFormat::Png,
            [1, 1],
            vec![0; 4],
            1024,
            None,
            false
        )
        .is_err());
        assert_eq!(fs::read(existing).unwrap(), b"original bytes");
        let internal = temp.path().join("drawing.pond").join("tiles.store");
        assert!(export(
            &internal,
            ExportFormat::Png,
            [1, 1],
            vec![0; 4],
            1024,
            None,
            false
        )
        .is_err());
        assert!(!internal.exists());
    }
}
