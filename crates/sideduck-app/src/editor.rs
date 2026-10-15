//! Desktop orchestration. Panels emit commands; document and raster logic live in core crates.
use crate::{
    core::{EditorCore, Result, OUTPUT_BUDGET, SELECTION_BUDGET},
    preview::PreviewRenderer,
};
use eframe::egui;
use sideduck_doc::{selection::SelectionMask, Layer, LayerKind, Operation};
use sideduck_input::{
    FilterChain, FilterSpec, InputConfig, InputSample, Modifiers, PressureCalibrator, SamplePhase,
    ToolKind,
};
use sideduck_platform::{ContactPhase, TabletSource};
use sideduck_ui::editor::{
    CanvasEvent, EditorCommand, EditorSnapshot, EditorUi, LayerRow, PointerPhase,
    PressureEditorSnapshot, Tool,
};
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
const CANVAS_TITLE: &str = "sideDuck · Canvas";

pub struct EditorApp {
    pub core: EditorCore,
    annotation: crate::annotation::AnnotationController,
    canvas_frame: Option<sideduck_platform::DesktopRect>,
    ui: EditorUi,
    renderer: PreviewRenderer,
    texture: Option<egui::TextureHandle>,
    source: Box<dyn TabletSource>,
    filter: FilterChain,
    calibration: PressureCalibrator,
    pressure: PressureEditorSnapshot,
    status: String,
    started: Instant,
    smoke: Option<Duration>,
    dropped: u64,
    last_tablet: Option<Instant>,
    last_geometry: Option<([f32; 2], f32)>,
    selection_points: Vec<[f64; 2]>,
    predicted: Option<[f32; 2]>,
    last_confirmed: Option<[f32; 2]>,
    ceiling: u64,
    clipboard: Option<crate::editing::Clipboard>,
    preferences_path: Option<PathBuf>,
    brush_pressure: std::collections::BTreeMap<usize, Option<sideduck_input::ResponseCurve>>,
}
impl EditorApp {
    pub fn new(path: PathBuf, smoke: Option<Duration>, demo: bool) -> Result<Self> {
        let mut core = if path.exists() {
            EditorCore::open(&path)?
        } else {
            EditorCore::create(&path, [2048, 1536])?
        };
        if demo && core.document.borrow().tile_keys().next().is_none() {
            core.brush = 2;
            for i in 0..90 {
                core.paint(
                    InputSample {
                        position: [220. + i as f32 * 17., 700. + (i as f32 * 0.08).sin() * 230.],
                        pressure: 0.15 + 0.85 * (i as f32 / 89. * std::f32::consts::PI).sin(),
                        timestamp_seconds: i as f64 * 0.01,
                        phase: if i == 0 {
                            SamplePhase::Begin
                        } else if i == 89 {
                            SamplePhase::End
                        } else {
                            SamplePhase::Move
                        },
                        ..Default::default()
                    },
                    18.,
                    0.8,
                    [27, 89, 73],
                    false,
                )?;
            }
        }
        let mut app = Self::with_source(
            core,
            sideduck_platform::create_tablet_source(8192).map_err(|e| e.to_string())?,
            smoke,
        )?;
        let preferences = sideduck_platform::app_data_directory()
            .map_err(|e| e.to_string())?
            .join("editor.json");
        app.preferences_path = Some(preferences.clone());
        match crate::preferences::Preferences::load(&preferences) {
            Ok(Some(prefs)) => {
                app.core.brushes = prefs.brushes;
                app.core.engine_config = prefs.engine;
                app.core.brush = prefs.selected_brush;
                app.filter = FilterChain::new(prefs.input)?;
                app.filter.set_calibration(prefs.calibration)?;
                app.brush_pressure = prefs.brush_pressure;
                let curve = app.brush_pressure.get(&app.core.brush).cloned().flatten();
                app.filter
                    .set_brush_pressure(curve.clone().unwrap_or_default())?;
                app.pressure.global = app.filter.config().global_pressure.clone();
                app.pressure.per_brush = curve;
                app.ui.mouse_painting = !app.filter.config().stylus_only;
                app.ui.radius = app.core.brushes[app.core.brush]
                    .definition
                    .parameters
                    .size
                    .value
                    * 0.5;
                app.ui.opacity = app.core.brushes[app.core.brush]
                    .definition
                    .parameters
                    .opacity
                    .value;
            }
            Ok(None) => {}
            Err(error) => {
                app.preferences_path = None;
                app.status =
                    format!("Preferences were not loaded; existing file preserved: {error}");
            }
        }
        Ok(app)
    }
    fn with_source(
        core: EditorCore,
        source: Box<dyn TabletSource>,
        smoke: Option<Duration>,
    ) -> Result<Self> {
        let path = core.document.borrow().path().to_path_buf();
        let config = InputConfig::default();
        let calibration = PressureCalibrator::new(config.calibration.clone())?;
        let pressure = PressureEditorSnapshot {
            global: config.global_pressure.clone(),
            per_brush: Some(config.brush_pressure.clone()),
            ..Default::default()
        };
        let renderer = PreviewRenderer::new(&core)?;
        let mut ui = EditorUi::default();
        ui.radius = core.brushes[core.brush].definition.parameters.size.value * 0.5;
        ui.opacity = core.brushes[core.brush].definition.parameters.opacity.value;
        Ok(Self {
            core,
            annotation: crate::annotation::AnnotationController::new(),
            canvas_frame: None,
            ui,
            renderer,
            texture: None,
            source,
            filter: FilterChain::new(config)?,
            calibration,
            pressure,
            status: format!("Opened {} · changes save after each action", path.display()),
            started: Instant::now(),
            smoke,
            dropped: 0,
            last_tablet: None,
            last_geometry: None,
            selection_points: vec![],
            predicted: None,
            last_confirmed: None,
            ceiling: 4 * 1024 * 1024 * 1024,
            clipboard: None,
            preferences_path: None,
            brush_pressure: std::collections::BTreeMap::new(),
        })
    }
    fn snapshot(&self) -> EditorSnapshot {
        let doc = self.core.document.borrow();
        EditorSnapshot {
            title: doc
                .path()
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            dimensions: self.core.dimensions(),
            layers: doc
                .state()
                .layers
                .iter()
                .map(|l| LayerRow {
                    id: l.id,
                    name: l.name.clone(),
                    visible: l.visible,
                    locked: l.locked,
                    opacity: l.opacity,
                    mode: serde_json::to_value(l.blend_mode)
                        .ok()
                        .and_then(|v| v.as_str().map(str::to_owned))
                        .unwrap_or_else(|| "normal".into()),
                    group: matches!(l.kind, LayerKind::Group { .. }),
                    raster: matches!(l.kind, LayerKind::Raster),
                    parent: l.parent,
                    mask: l.mask,
                    clip_base: l.clip_base,
                })
                .collect(),
            active: doc.state().active_layer,
            brushes: self
                .core
                .brushes
                .iter()
                .map(|b| b.definition.name.clone())
                .collect(),
            brush: self.core.brush,
            brush_definition: self
                .core
                .brushes
                .get(self.core.brush)
                .map(|b| b.definition.clone()),
            pressure: self.pressure.clone(),
            annotation: self.annotation.snapshot(),
            can_undo: doc.can_undo(),
            can_redo: doc.can_redo(),
            status: self.status.clone(),
            memory_bytes: sideduck_platform::process_memory().map_or(0, |m| m.resident_bytes),
            latency: "CPU preview · presentation latency pending".into(),
        }
    }
    fn upload(&mut self, ctx: &egui::Context) -> Result<()> {
        if !self.core.preview_dirty {
            return Ok(());
        }
        let level = self.renderer.level_for_preview(1024);
        let (dimensions, pixels) = self.renderer.render(&self.core, level, false)?;
        let mut rgba: Vec<_> = pixels
            .into_iter()
            .flat_map(sideduck_composite::color::linear_premultiplied_to_rgba8_srgb)
            .collect();
        if let Some(mask) = &self.core.selection {
            let step = 1_u32 << level;
            for y in 0..dimensions[1] {
                for x in 0..dimensions[0] {
                    let sx = x * step;
                    let sy = y * step;
                    if mask.coverage(sx, sy) > 127
                        && (sx < step
                            || sy < step
                            || mask.coverage(sx.saturating_sub(step), sy) <= 127
                            || mask.coverage(sx, sy.saturating_sub(step)) <= 127
                            || mask.coverage(sx.saturating_add(step), sy) <= 127
                            || mask.coverage(sx, sy.saturating_add(step)) <= 127)
                    {
                        let index = ((y * dimensions[0] + x) * 4) as usize;
                        rgba[index..index + 4].copy_from_slice(&[20, 150, 215, 255]);
                    }
                }
            }
        }
        let image = egui::ColorImage::from_rgba_unmultiplied(
            [dimensions[0] as usize, dimensions[1] as usize],
            &rgba,
        );
        if let Some(texture) = &mut self.texture {
            texture.set(image, egui::TextureOptions::LINEAR);
        } else {
            self.texture = Some(ctx.load_texture("document", image, egui::TextureOptions::LINEAR));
        }
        self.core.preview_dirty = false;
        Ok(())
    }
    fn reset_input(&mut self) {
        self.core.cancel_stroke();
        self.filter.reset();
        self.predicted = None;
        self.last_confirmed = None;
    }
    fn sample(&mut self, sample: InputSample) -> Result<()> {
        self.calibration.observe(sample);
        let output = self.filter.process(sample);
        if output.stroke_reset {
            self.core.cancel_stroke();
        }
        self.predicted = output.provisional.map(|p| p.sample.position);
        if let Some(sample) = output.confirmed {
            self.last_confirmed = Some(sample.position);
            if matches!(self.ui.tool, Tool::Brush | Tool::Eraser) {
                self.core.paint(
                    sample,
                    self.ui.radius,
                    self.ui.opacity,
                    self.ui.color,
                    self.ui.tool == Tool::Eraser || sample.tool == ToolKind::Eraser,
                )?;
            } else {
                self.selection_event(CanvasEvent {
                    position: sample.position,
                    phase: match sample.phase {
                        SamplePhase::Begin => PointerPhase::Begin,
                        SamplePhase::End => PointerPhase::End,
                        _ => PointerPhase::Move,
                    },
                    suspend: sample.modifiers.suspend_stabilization,
                })?;
            }
        }
        Ok(())
    }
    fn selection_event(&mut self, event: CanvasEvent) -> Result<()> {
        let point = event.position.map(f64::from);
        if event.phase == PointerPhase::Begin {
            self.selection_points.clear();
        }
        if self.selection_points.len() < 65_536 {
            self.selection_points.push(point);
        }
        if event.phase != PointerPhase::End {
            return Ok(());
        }
        let [w, h] = self.core.dimensions();
        let mut mask = SelectionMask::new(w, h, SELECTION_BUDGET)?;
        let start = *self.selection_points.first().unwrap_or(&point);
        match self.ui.tool {
            Tool::Rectangle => mask.rectangle(start, point)?,
            Tool::Ellipse => mask.ellipse(start, point)?,
            Tool::Lasso => mask.lasso(&self.selection_points)?,
            Tool::Wand => {
                let layer = self.core.active_layer()?;
                let surface = crate::core::DocumentSurface::new(
                    self.core.document.clone(),
                    layer,
                    64 * 1024 * 1024,
                    None,
                );
                let mut error = None;
                mask.magic_wand([point[0] as u32, point[1] as u32], 24, |x, y| {
                    match sideduck_brush::RasterSurface::read_pixel(&surface, x, y) {
                        Ok(p) => sideduck_composite::color::linear_premultiplied_to_rgba8_srgb(p),
                        Err(e) => {
                            error = Some(e);
                            [0; 4]
                        }
                    }
                })?;
                if let Some(error) = error {
                    return Err(error);
                }
            }
            Tool::Transform => {
                return crate::editing::transform_selection(
                    &mut self.core,
                    sideduck_doc::transform::Transform::translation(
                        point[0] - start[0],
                        point[1] - start[1],
                    )?,
                )
            }
            _ => return Ok(()),
        }
        self.core.save_selection(Some(mask))
    }
    fn change_layer(&mut self, id: u32, change: impl FnOnce(&mut Layer)) -> Result<()> {
        let mut layer = self
            .core
            .document
            .borrow()
            .state()
            .layer(id)
            .cloned()
            .ok_or("Layer no longer exists")?;
        change(&mut layer);
        self.core.apply(Operation::SetLayer { layer }, vec![])
    }
    fn command(&mut self, command: EditorCommand) -> Result<()> {
        use EditorCommand::*;
        let save_preferences = matches!(
            &command,
            SetBrush(_)
                | UpdateBrush(_)
                | ImportBrush(_)
                | SetBrushTexture { .. }
                | SetGlobalPressureCurve(_)
                | SetBrushPressureCurve(_)
                | SetRadius(_)
                | SetOpacity(_)
                | SetMousePainting(_)
                | SetStabilization(_)
                | Calibrate
        );
        match command {
            New { path, dimensions } => {
                self.reset_input();
                self.annotation.stop();
                self.replace_core(EditorCore::create(path, dimensions)?)?;
            }
            Open(path) => {
                self.reset_input();
                self.annotation.stop();
                self.replace_core(EditorCore::open(path)?)?;
            }
            Save => {
                self.core
                    .document
                    .borrow_mut()
                    .checkpoint()
                    .map_err(|e| e.to_string())?;
                self.status = "Saved durable checkpoint".into();
            }
            Undo => self.core.undo(false)?,
            Redo => self.core.undo(true)?,
            AddLayer { group } => {
                let doc = self.core.document.borrow();
                let id = doc.next_layer_id().map_err(|e| e.to_string())?;
                let index = doc.state().layers.len();
                drop(doc);
                let mut layer = Layer::raster(
                    id,
                    format!("{} {}", if group { "Group" } else { "Paint" }, id + 1),
                );
                if group {
                    layer.kind = LayerKind::Group { isolated: true };
                }
                self.core
                    .apply(Operation::CreateLayer { layer, index }, vec![])?;
                self.core
                    .apply(Operation::SetActiveLayer { id: Some(id) }, vec![])?;
            }
            SelectLayer(id) => self
                .core
                .apply(Operation::SetActiveLayer { id: Some(id) }, vec![])?,
            DeleteLayer(id) => self.core.apply(Operation::RemoveLayer { id }, vec![])?,
            LayerParent(id, parent) => {
                let index = self
                    .core
                    .document
                    .borrow()
                    .state()
                    .layers
                    .iter()
                    .position(|l| l.id == id)
                    .ok_or("Layer disappeared")?;
                self.core
                    .apply(Operation::ReorderLayer { id, index, parent }, vec![])?;
            }
            LayerMask(id, mask) => self.change_layer(id, |l| l.mask = mask)?,
            LayerClipBase(id, base) => self.change_layer(id, |l| l.clip_base = base)?,
            LayerVisible(id, value) => self.change_layer(id, |l| l.visible = value)?,
            LayerLocked(id, value) => self.change_layer(id, |l| l.locked = value)?,
            LayerOpacity(id, value) => self.change_layer(id, |l| l.opacity = value)?,
            LayerName(id, value) => self.change_layer(id, |l| l.name = value)?,
            LayerMode(id, value) => {
                let mode = serde_json::from_value(serde_json::Value::String(value))
                    .map_err(|e| e.to_string())?;
                self.change_layer(id, |l| l.blend_mode = mode)?;
            }
            Reorder(id, delta) => {
                let doc = self.core.document.borrow();
                let index = doc
                    .state()
                    .layers
                    .iter()
                    .position(|l| l.id == id)
                    .ok_or("Layer disappeared")?;
                let parent = doc.state().layers[index].parent;
                let target = (index as i64 + i64::from(delta))
                    .clamp(0, doc.state().layers.len().saturating_sub(1) as i64)
                    as usize;
                drop(doc);
                self.core.apply(
                    Operation::ReorderLayer {
                        id,
                        index: target,
                        parent,
                    },
                    vec![],
                )?;
            }
            MergeDown(id) => crate::editing::merge_down(&mut self.core, id)?,
            SetTool(tool) => {
                self.reset_input();
                self.ui.tool = tool;
            }
            SetBrush(index) => {
                self.reset_input();
                if index >= self.core.brushes.len() {
                    return Err("Brush does not exist".into());
                }
                self.core.brush = index;
                self.ui.radius = self.core.brushes[index].definition.parameters.size.value * 0.5;
                self.ui.opacity = self.core.brushes[index].definition.parameters.opacity.value;
                let curve = self.brush_pressure.get(&index).cloned().flatten();
                self.filter
                    .set_brush_pressure(curve.clone().unwrap_or_default())?;
                self.pressure.per_brush = curve;
            }
            UpdateBrush(definition) => {
                definition.validate()?;
                self.ui.radius = definition.parameters.size.value * 0.5;
                self.ui.opacity = definition.parameters.opacity.value;
                self.core.brushes[self.core.brush].definition = *definition;
            }
            ImportBrush(path) => {
                let brush = sideduck_brush::load_brush_package(path, &self.core.engine_config)?;
                self.core.brushes.push(brush);
                self.core.brush = self.core.brushes.len() - 1;
            }
            ExportBrush(path) => sideduck_brush::save_brush_package(
                path,
                &self.core.brushes[self.core.brush],
                &self.core.engine_config,
            )?,
            SetBrushTexture { shape, path } => {
                let reader = image::ImageReader::open(&path).map_err(|e| e.to_string())?;
                let (w, h) = reader.into_dimensions().map_err(|e| e.to_string())?;
                if w.max(h) > self.core.engine_config.maximum_texture_side {
                    return Err("Brush texture exceeds configured size limit".into());
                }
                let texture = image::open(path).map_err(|e| e.to_string())?.to_luma8();
                let texture = sideduck_brush::Texture::new(w, h, texture.into_raw())?;
                if shape {
                    self.core.brushes[self.core.brush].shape = texture
                } else {
                    self.core.brushes[self.core.brush].grain = texture
                }
            }
            SetGlobalPressureCurve(curve) => {
                self.reset_input();
                self.filter.set_global_pressure(curve.clone())?;
                self.pressure.global = curve;
            }
            SetBrushPressureCurve(curve) => {
                self.reset_input();
                self.filter
                    .set_brush_pressure(curve.clone().unwrap_or_default())?;
                self.brush_pressure.insert(self.core.brush, curve.clone());
                self.pressure.per_brush = curve;
            }
            SetRadius(value) => {
                self.ui.radius = value;
                self.core.brushes[self.core.brush]
                    .definition
                    .parameters
                    .size
                    .value = value * 2.;
            }
            SetOpacity(value) => {
                self.ui.opacity = value;
                self.core.brushes[self.core.brush]
                    .definition
                    .parameters
                    .opacity
                    .value = value;
            }
            SetColor(value) => self.ui.color = value,
            SelectAll => {
                let [w, h] = self.core.dimensions();
                let mut mask = SelectionMask::new(w, h, SELECTION_BUDGET)?;
                mask.select_all();
                self.core.save_selection(Some(mask))?;
            }
            ClearSelection => self.core.save_selection(None)?,
            InvertSelection | Feather(_) | Grow(_) => {
                let mut mask = self
                    .core
                    .selection
                    .as_deref()
                    .cloned()
                    .ok_or("Create a selection first")?;
                match command {
                    InvertSelection => mask.invert(),
                    Feather(r) => mask.feather(r)?,
                    Grow(r) if r >= 0 => mask.grow(r as u32)?,
                    Grow(r) => mask.shrink(r.unsigned_abs())?,
                    _ => unreachable!(),
                };
                self.core.save_selection(Some(mask))?;
            }
            Copy => self.clipboard = Some(crate::editing::copy_selection(&mut self.core, false)?),
            Cut => self.clipboard = Some(crate::editing::copy_selection(&mut self.core, true)?),
            Paste => crate::editing::paste(
                &mut self.core,
                self.clipboard.as_ref().ok_or("Copy pixels first")?,
            )?,
            Transform {
                move_by,
                scale,
                degrees,
            } => {
                use sideduck_doc::transform::Transform as Matrix;
                let dimensions = self.core.dimensions();
                let center = [dimensions[0] as f64 / 2., dimensions[1] as f64 / 2.];
                let transform = Matrix::scale(scale[0], scale[1])?
                    .then(Matrix::rotation(degrees.to_radians())?)?
                    .around(center)?
                    .then(Matrix::translation(move_by[0], move_by[1])?)?;
                crate::editing::transform_selection(&mut self.core, transform)?;
            }
            SetMousePainting(value) => {
                self.reset_input();
                self.ui.mouse_painting = value;
                let mut config = self.filter.config().clone();
                config.stylus_only = !value;
                self.filter = FilterChain::new(config)?;
            }
            SetStabilization(mode) => {
                self.reset_input();
                let mut config = self.filter.config().clone();
                config.filters.retain(|f| {
                    !matches!(
                        f,
                        FilterSpec::Average { .. } | FilterSpec::PullString { .. }
                    )
                });
                let at = config.filters.len().saturating_sub(1);
                if mode == "Average" {
                    config.filters.insert(at, FilterSpec::Average { window: 4 });
                } else if mode == "Pull string" {
                    config
                        .filters
                        .insert(at, FilterSpec::PullString { distance: 12. });
                }
                self.filter = FilterChain::new(config)?;
            }
            SetMemoryBudget(bytes) => {
                if bytes < 512 * 1024 * 1024 {
                    return Err("Editor memory ceiling must be at least512MiB".into());
                }
                self.ceiling = bytes;
            }
            Calibrate => {
                let fit = self.calibration.fit()?;
                self.filter.set_calibration(fit.curve)?;
                self.pressure.calibration_before = fit.before_histogram;
                self.pressure.calibration_after = fit.after_histogram;
                self.status = format!("Calibration fitted {} samples", fit.sample_count);
            }
            ImportReference(path) => self.import_reference(Path::new(&path))?,
            Export { path, format } => self.export(
                Path::new(&path),
                crate::export::ExportFormat::parse(&format)?,
            )?,
            RefreshDesktop => self.annotation.refresh_desktop()?,
            SelectCanvasDisplay(id) => {
                self.annotation.select_canvas_display(id)?;
                self.ui.canvas_window = true;
            }
            StartOverlay { target } => {
                self.reset_input();
                let dimensions = self.annotation.begin_overlay(target)?;
                let path = crate::annotation::annotation_path(self.core.document.borrow().path());
                match EditorCore::create(&path, dimensions) {
                    Ok(core) => self.replace_core(core)?,
                    Err(error) => {
                        self.annotation.stop();
                        return Err(error);
                    }
                }
                self.ui.canvas_window = true;
                self.ui.zoom = 1.;
                self.ui.pan = egui::Vec2::ZERO;
                self.status = format!("Local annotation document: {}", path.display());
            }
            SetOverlayDrawing(value) => self.annotation.set_drawing(value)?,
            Capture {
                target,
                path,
                request_permission,
            } => self.annotation.start_capture(
                target,
                PathBuf::from(path),
                request_permission,
                256 * 1024 * 1024,
            )?,
            StopOverlay => {
                self.reset_input();
                self.annotation.stop();
            }
        }
        if save_preferences {
            self.save_preferences()?;
        }
        Ok(())
    }
    fn replace_core(&mut self, mut core: EditorCore) -> Result<()> {
        core.brushes = self.core.brushes.clone();
        core.engine_config = self.core.engine_config.clone();
        core.brush = self.core.brush;
        self.core = core;
        self.renderer = PreviewRenderer::new(&self.core)?;
        self.texture = None;
        self.last_geometry = None;
        self.ui.zoom = 1.;
        self.ui.pan = egui::Vec2::ZERO;
        Ok(())
    }
    fn save_preferences(&self) -> Result<()> {
        let Some(path) = &self.preferences_path else {
            return Ok(());
        };
        crate::preferences::Preferences {
            version: 1,
            input: self.filter.config().clone(),
            brushes: self.core.brushes.clone(),
            engine: self.core.engine_config.clone(),
            selected_brush: self.core.brush,
            brush_pressure: self.brush_pressure.clone(),
            calibration: self.filter.calibration_curve().clone(),
        }
        .save(path)
    }
    fn import_reference(&mut self, path: &Path) -> Result<()> {
        let pdf = sideduck_platform::create_pdf_rasterizer();
        let source = crate::reference::ReferenceSource::open(path, 0, pdf.as_ref())?;
        let source_dimensions = source.dimensions();
        self.status = format!(
            "Imported {:?} reference {} × {}",
            source.kind(),
            source_dimensions[0],
            source_dimensions[1]
        );
        let doc = self.core.document.borrow();
        let source_id = doc.put_source(source.bytes()).map_err(|e| e.to_string())?;
        let id = doc.next_layer_id().map_err(|e| e.to_string())?;
        drop(doc);
        let mut layer = Layer::raster(id, path.file_name().unwrap_or_default().to_string_lossy());
        layer.kind = LayerKind::Reference {
            source: source_id,
            media_type: source.media_type().into(),
            page: source.page(),
        };
        layer.locked = true;
        self.core
            .apply(Operation::CreateLayer { layer, index: 0 }, vec![])
    }
    fn export(&mut self, path: &Path, format: crate::export::ExportFormat) -> Result<()> {
        self.reset_input();
        let pdf_layer = if format == crate::export::ExportFormat::Pdf {
            original_pdf_base(&self.core.document.borrow().state().layers)
        } else {
            None
        };
        let source = pdf_layer
            .map(|(id, page)| {
                self.core
                    .document
                    .borrow()
                    .read_source(id)
                    .map(|bytes| (bytes, page))
                    .map_err(|e| e.to_string())
            })
            .transpose()?;
        let (dimensions, pixels) = self.renderer.render(&self.core, 0, source.is_some())?;
        let rgba = pixels
            .into_iter()
            .flat_map(sideduck_composite::color::linear_premultiplied_to_rgba8_srgb)
            .collect();
        crate::export::export(
            path,
            format,
            dimensions,
            rgba,
            3 * OUTPUT_BUDGET,
            source
                .as_ref()
                .map(|(bytes, page)| crate::export::PdfSource { bytes, page: *page }),
            true,
        )?;
        self.status = if format == crate::export::ExportFormat::Pdf && source.is_none() {
            format!(
                "Exported complete flattened composite to {}",
                path.display()
            )
        } else {
            format!("Exported {}", path.display())
        };
        Ok(())
    }
    fn canvas(&mut self, ctx: &egui::Context, native: Vec<sideduck_platform::TabletSample>) {
        if ctx.input(|i| i.viewport().close_requested()) {
            self.ui.canvas_window = false;
            self.annotation.stop();
            self.reset_input();
            return;
        }
        if let Some(frame) = self.canvas_frame.take() {
            ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(
                frame.x as f32,
                frame.y as f32,
            )));
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(
                frame.width as f32,
                frame.height as f32,
            )));
        }
        let events = self.ui.canvas(
            ctx,
            self.core.dimensions(),
            self.texture.as_ref().map(|t| t.id()),
            self.annotation.active(),
        );
        if let Some(geometry) = self.ui.geometry {
            let signature = (geometry.origin, geometry.scale);
            if self.last_geometry.is_some_and(|old| old != signature) {
                self.reset_input();
            }
            self.last_geometry = Some(signature);
            let title = if self.ui.canvas_window {
                CANVAS_TITLE
            } else {
                "sideDuck"
            };
            let window_id = sideduck_platform::window_id_for_title(title);
            for event in native {
                if Some(event.window_id) != window_id {
                    continue;
                }
                let expected = ctx.screen_rect().size() * ctx.zoom_factor();
                if (event.window_size[0] as f32 - expected.x).abs() > 1.
                    || (event.window_size[1] as f32 - expected.y).abs() > 1.
                {
                    self.reset_input();
                    continue;
                }
                self.last_tablet = Some(Instant::now());
                let position = event.position_top_left();
                let position = geometry.document_position([
                    position[0] as f32 / ctx.zoom_factor(),
                    position[1] as f32 / ctx.zoom_factor(),
                ]);
                let Some(position) = position.or_else(|| {
                    matches!(event.phase, ContactPhase::Up)
                        .then_some(self.last_confirmed)
                        .flatten()
                }) else {
                    continue;
                };
                let sample = InputSample {
                    position,
                    pressure: event.pressure,
                    tilt: event.tilt,
                    azimuth: event.azimuth,
                    timestamp_seconds: event.timestamp_seconds,
                    device_id: event.device_id,
                    phase: match event.phase {
                        ContactPhase::Down => SamplePhase::Begin,
                        ContactPhase::Move => SamplePhase::Move,
                        ContactPhase::Up => SamplePhase::End,
                        ContactPhase::Hover => SamplePhase::Hover,
                    },
                    tool: if event.eraser {
                        ToolKind::Eraser
                    } else {
                        ToolKind::Stylus
                    },
                    buttons: event.buttons,
                    modifiers: Modifiers {
                        suspend_stabilization: ctx.input(|i| i.modifiers.alt),
                        suspend_snapping: ctx.input(|i| i.modifiers.alt),
                    },
                };
                if let Err(error) = self.sample(sample) {
                    self.status = error;
                    self.reset_input();
                }
            }
            if self.ui.mouse_painting
                && self
                    .last_tablet
                    .is_none_or(|t| t.elapsed() > Duration::from_millis(250))
            {
                for event in events {
                    let sample = InputSample {
                        position: event.position,
                        pressure: 1.,
                        timestamp_seconds: self.started.elapsed().as_secs_f64(),
                        phase: match event.phase {
                            PointerPhase::Begin => SamplePhase::Begin,
                            PointerPhase::Move => SamplePhase::Move,
                            PointerPhase::End => SamplePhase::End,
                        },
                        tool: ToolKind::Mouse,
                        device_id: u64::MAX,
                        modifiers: Modifiers {
                            suspend_stabilization: event.suspend,
                            suspend_snapping: event.suspend,
                        },
                        ..Default::default()
                    };
                    if let Err(error) = self.sample(sample) {
                        self.status = error;
                        self.reset_input();
                    }
                }
            }
            if let Some((a, b)) = self.last_confirmed.zip(self.predicted) {
                let map = |p: [f32; 2]| {
                    egui::pos2(
                        geometry.origin[0] + p[0] * geometry.scale,
                        geometry.origin[1] + p[1] * geometry.scale,
                    )
                };
                ctx.layer_painter(egui::LayerId::new(
                    egui::Order::Foreground,
                    egui::Id::new("provisional-input"),
                ))
                .line_segment(
                    [map(a), map(b)],
                    egui::Stroke::new(1.5, egui::Color32::from_rgba_unmultiplied(80, 80, 80, 140)),
                );
            }
        }
    }
}
// Source-preserving PDF export is exact only for one unmodified base reference.
// More complex reference stacks use the complete flattened composite instead of
// accidentally dropping additional image/PDF layers or changing layer order.
fn original_pdf_base(layers: &[Layer]) -> Option<(sideduck_doc::ContentId, u32)> {
    let references: Vec<_> = layers
        .iter()
        .filter(|l| l.visible && matches!(l.kind, LayerKind::Reference { .. }))
        .collect();
    if references.len() != 1 {
        return None;
    }
    let layer = references[0];
    if layers.iter().find(|l| l.visible).map(|l| l.id) != Some(layer.id)
        || layer.parent.is_some()
        || layer.mask.is_some()
        || layer.clip_base.is_some()
        || layer.opacity != 1.
        || layer.blend_mode != sideduck_composite::BlendMode::Normal
    {
        return None;
    }
    if let LayerKind::Reference {
        source,
        ref media_type,
        page,
    } = layer.kind
    {
        (media_type == "application/pdf").then_some((source, page))
    } else {
        None
    }
}
impl eframe::App for EditorApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Some(result) = self.annotation.poll_capture() {
            match result.and_then(|(image, path)| {
                crate::annotation::capture_to_document(image, &path, 256 * 1024 * 1024)
            }) {
                Ok(core) => {
                    self.reset_input();
                    self.annotation.stop();
                    if let Err(error) = self.replace_core(core) {
                        self.status = error;
                    } else {
                        self.status = "Capture saved as a local document".into();
                    }
                }
                Err(error) => self.status = error,
            }
        }
        let window_id = sideduck_platform::window_id_for_title(CANVAS_TITLE)
            .and_then(|id| u32::try_from(id).ok());
        if let Err(error) = self.annotation.update(window_id) {
            self.status = error;
            self.annotation.stop();
        }
        if let Some(frame) = self.annotation.take_canvas_frame() {
            self.canvas_frame = Some(frame);
        }
        let mut native = self.source.drain_samples();
        if self.source.dropped_samples() != self.dropped {
            self.dropped = self.source.dropped_samples();
            self.reset_input();
            self.status = "Input queue overflow: unfinished stroke cancelled".into();
        }
        if let Err(error) = self.upload(ctx) {
            self.status = error;
            self.core.preview_dirty = false;
        }
        let commands = self.ui.controls(ctx, &self.snapshot());
        for command in commands {
            if let Err(error) = self.command(command) {
                self.status = error;
            }
        }
        if self.ui.canvas_window {
            ctx.show_viewport_immediate(
                egui::ViewportId::from_hash_of("editor-canvas"),
                egui::ViewportBuilder::default()
                    .with_title(CANVAS_TITLE)
                    .with_inner_size([1100., 800.])
                    .with_min_inner_size([320., 240.])
                    .with_decorations(false)
                    .with_transparent(true),
                |canvas_ctx, _| self.canvas(canvas_ctx, std::mem::take(&mut native)),
            );
        } else {
            self.canvas(ctx, native);
        }
        for file in ctx.input(|i| i.raw.dropped_files.clone()) {
            if let Some(path) = file.path {
                if let Err(error) = self.import_reference(&path) {
                    self.status = error;
                }
            }
        }
        if let Ok(memory) = sideduck_platform::process_memory() {
            assert!(
                memory.resident_bytes < self.ceiling,
                "process residency exceeded editor memory ceiling"
            );
        }
        if self
            .smoke
            .is_some_and(|time| self.started.elapsed() >= time)
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        ctx.request_repaint_after(Duration::from_millis(16));
    }
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.; 4]
    }
    fn on_exit(&mut self) {
        self.reset_input();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mixed_reference_pdf_export_never_discards_other_sources() {
        let mut pdf = Layer::raster(0, "PDF");
        pdf.kind = LayerKind::Reference {
            source: [1; 32],
            media_type: "application/pdf".into(),
            page: 0,
        };
        let paint = Layer::raster(1, "Paint");
        assert!(original_pdf_base(&[pdf.clone(), paint.clone()]).is_some());
        let mut image = Layer::raster(2, "Image");
        image.kind = LayerKind::Reference {
            source: [2; 32],
            media_type: "image/raster".into(),
            page: 0,
        };
        assert!(original_pdf_base(&[pdf.clone(), paint.clone(), image]).is_none());
        assert!(original_pdf_base(&[paint, pdf.clone()]).is_none());
        pdf.opacity = 0.5;
        assert!(original_pdf_base(&[pdf]).is_none());
    }
    #[test]
    fn panels_drive_real_document_brush_and_selection_actions_against_mock_input() {
        let temp = tempfile::tempdir().unwrap();
        let core = EditorCore::create(temp.path().join("editor.pond"), [64, 64]).unwrap();
        let mut app = EditorApp::with_source(
            core,
            Box::new(sideduck_platform::MockTabletSource::new(32)),
            None,
        )
        .unwrap();
        assert!(!app.ui.mouse_painting);
        app.command(EditorCommand::AddLayer { group: false })
            .unwrap();
        let id = app.core.active_layer().unwrap();
        assert_eq!(id, 1);
        app.command(EditorCommand::LayerName(id, "Ink".into()))
            .unwrap();
        app.command(EditorCommand::LayerOpacity(id, 0.6)).unwrap();
        app.command(EditorCommand::SetMousePainting(true)).unwrap();
        app.command(EditorCommand::SetBrush(2)).unwrap();
        app.command(EditorCommand::SetRadius(4.)).unwrap();
        for (i, phase) in [SamplePhase::Begin, SamplePhase::Move, SamplePhase::End]
            .into_iter()
            .enumerate()
        {
            app.sample(InputSample {
                position: [10. + i as f32 * 8., 20.],
                pressure: 1.,
                timestamp_seconds: i as f64 * 0.01,
                phase,
                tool: ToolKind::Mouse,
                ..Default::default()
            })
            .unwrap();
        }
        assert!(app
            .core
            .document
            .borrow()
            .tile_keys()
            .any(|key| key.layer == id));
        let sequence = app.core.document.borrow().sequence();
        let Operation::Stroke(stroke) = app.core.document.borrow().operation(sequence).unwrap()
        else {
            panic!("expected stroke")
        };
        let committed: Vec<InputSample> = serde_json::from_value(stroke.samples).unwrap();
        assert_eq!(
            committed.len(),
            3,
            "only three confirmed inputs enter the operation"
        );
        app.command(EditorCommand::Undo).unwrap();
        assert_eq!(app.core.document.borrow().tile_keys().count(), 0);
        app.command(EditorCommand::Redo).unwrap();
        app.command(EditorCommand::SelectAll).unwrap();
        assert_eq!(app.core.selection.as_ref().unwrap().coverage(30, 30), 255);
        app.command(EditorCommand::InvertSelection).unwrap();
        assert_eq!(app.core.selection.as_ref().unwrap().coverage(30, 30), 0);
        app.command(EditorCommand::ClearSelection).unwrap();
        let before = app.core.document.borrow().sequence();
        app.command(EditorCommand::LayerLocked(id, true)).unwrap();
        assert!(app
            .sample(InputSample {
                position: [3., 3.],
                pressure: 1.,
                timestamp_seconds: 1.,
                phase: SamplePhase::Begin,
                tool: ToolKind::Mouse,
                ..Default::default()
            })
            .is_err());
        assert_eq!(app.core.document.borrow().sequence(), before + 1);
        let ctx = egui::Context::default();
        let _ = ctx.run(Default::default(), |ctx| {
            app.ui.controls(ctx, &app.snapshot());
        });
    }
    #[test]
    fn cancelled_stroke_clears_preview_without_committing_pixels() {
        let temp = tempfile::tempdir().unwrap();
        let mut core = EditorCore::create(temp.path().join("editor.pond"), [64, 64]).unwrap();
        core.brush = 2;
        let mut renderer = PreviewRenderer::new(&core).unwrap();
        core.paint(
            InputSample {
                position: [20., 20.],
                pressure: 1.,
                phase: SamplePhase::Begin,
                ..Default::default()
            },
            5.,
            1.,
            [0, 0, 0],
            false,
        )
        .unwrap();
        let (_, pixels) = renderer.render(&core, 0, false).unwrap();
        assert!(pixels.iter().any(|p| p[3] > 0.));
        core.cancel_stroke();
        let (_, pixels) = renderer.render(&core, 0, false).unwrap();
        assert!(pixels.iter().all(|p| p[3] == 0.));
        assert_eq!(core.document.borrow().tile_keys().count(), 0);
    }
}
