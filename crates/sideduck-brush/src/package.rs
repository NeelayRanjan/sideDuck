use crate::{BrushDefinition, BrushPreset, EngineConfig, Texture, TextureRecipe};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{BufReader, Write},
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
};

/// The single editable source of brush constants and texture recipes.
pub const BUILTIN_CONFIG_JSON: &str = include_str!("../assets/brushes.json");
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PresetRecipe {
    pub definition: BrushDefinition,
    pub texture: TextureRecipe,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BrushLibraryConfig {
    pub engine: EngineConfig,
    pub presets: Vec<PresetRecipe>,
}
impl BrushLibraryConfig {
    pub fn parse(json: &str) -> Result<Self, String> {
        let config: Self = serde_json::from_str(json).map_err(|error| error.to_string())?;
        config.engine.validate()?;
        for recipe in &config.presets {
            recipe.definition.validate()?;
            if recipe.texture.side > config.engine.maximum_texture_side {
                return Err("Preset texture exceeds configured limit".into());
            }
        }
        Ok(config)
    }
    pub fn build(&self) -> Result<Vec<BrushPreset>, String> {
        self.presets
            .iter()
            .map(|recipe| {
                let (shape, grain) = recipe.texture.generate()?;
                let preset = BrushPreset {
                    definition: recipe.definition.clone(),
                    shape,
                    grain,
                };
                preset.validate(&self.engine)?;
                Ok(preset)
            })
            .collect()
    }
}
pub fn builtin_library() -> Result<(EngineConfig, Vec<BrushPreset>), String> {
    let config = BrushLibraryConfig::parse(BUILTIN_CONFIG_JSON)?;
    let presets = config.build()?;
    Ok((config.engine, presets))
}

/// Open format: a directory containing brush.json, shape.png and grain.png. Textures are
/// eight-bit grayscale masks. Imports are bounded before decompression or JSON allocation.
pub fn load_brush_package(
    path: impl AsRef<Path>,
    config: &EngineConfig,
) -> Result<BrushPreset, String> {
    config.validate()?;
    let path = path.as_ref();
    let json = path.join("brush.json");
    if fs::metadata(&json).map_err(|e| e.to_string())?.len() > 1_048_576 {
        return Err("Brush metadata exceeds size limit".into());
    }
    let definition: BrushDefinition =
        serde_json::from_slice(&fs::read(json).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    definition.validate()?;
    let preset = BrushPreset {
        definition,
        shape: read_texture(&path.join("shape.png"), config.maximum_texture_side)?,
        grain: read_texture(&path.join("grain.png"), config.maximum_texture_side)?,
    };
    preset.validate(config)?;
    Ok(preset)
}
/// Creates a new package with an atomic directory rename. Existing packages are never replaced;
/// callers can save revisions to a fresh path. Files are flushed before the rename.
pub fn save_brush_package(
    path: impl AsRef<Path>,
    preset: &BrushPreset,
    config: &EngineConfig,
) -> Result<(), String> {
    static SERIAL: AtomicU64 = AtomicU64::new(0);
    preset.validate(config)?;
    let path = path.as_ref();
    if path.exists() {
        return Err("Brush package already exists".into());
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let temp = parent.join(format!(
        ".brush-{}-{}.tmp",
        std::process::id(),
        SERIAL.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&temp).map_err(|e| e.to_string())?;
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(temp.join("brush.json"))
            .map_err(|e| e.to_string())?;
        file.write_all(&serde_json::to_vec_pretty(&preset.definition).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        write_texture(&temp.join("shape.png"), &preset.shape)?;
        write_texture(&temp.join("grain.png"), &preset.grain)?;
        File::open(&temp)
            .and_then(|file| file.sync_all())
            .map_err(|e| e.to_string())?;
        fs::rename(&temp, path).map_err(|e| e.to_string())?;
        File::open(parent)
            .and_then(|file| file.sync_all())
            .map_err(|e| e.to_string())?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&temp);
    }
    result
}
fn read_texture(path: &Path, maximum_side: u32) -> Result<Texture, String> {
    let file = File::open(path).map_err(|e| e.to_string())?;
    if file.metadata().map_err(|e| e.to_string())?.len()
        > u64::from(maximum_side).pow(2) * 2 + 1_048_576
    {
        return Err("Brush PNG exceeds encoded size limit".into());
    }
    let mut decoder = png::Decoder::new(BufReader::new(file));
    decoder.set_limits(png::Limits {
        bytes: (u64::from(maximum_side).pow(2) * 2 + 1_048_576) as usize,
    });
    let mut reader = decoder.read_info().map_err(|e| e.to_string())?;
    let info = reader.info();
    if info.width == 0
        || info.height == 0
        || info.width > maximum_side
        || info.height > maximum_side
        || info.color_type != png::ColorType::Grayscale
        || info.bit_depth != png::BitDepth::Eight
    {
        return Err("Brush PNG must be bounded eight-bit grayscale".into());
    }
    let mut pixels = vec![0; reader.output_buffer_size()];
    let output = reader.next_frame(&mut pixels).map_err(|e| e.to_string())?;
    pixels.truncate(output.buffer_size());
    Texture::new(output.width, output.height, pixels)
}
fn write_texture(path: &Path, texture: &Texture) -> Result<(), String> {
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    let mut encoder = png::Encoder::new(&file, texture.width, texture.height);
    encoder.set_color(png::ColorType::Grayscale);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
    writer
        .write_image_data(&texture.pixels)
        .map_err(|e| e.to_string())?;
    writer.finish().map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())
}
