//! Editor presentation and interaction commands. Document and painting logic live in the app.
use eframe::egui::{self, Color32, Pos2, Rect, Sense, Vec2};
use sideduck_brush::{BrushDefinition, Driver, Parameter, Response};
use sideduck_input::ResponseCurve;
use sideduck_platform::{
    CaptureTarget, DesktopRect, DisplayDescriptor, OverlayTarget, WindowDescriptor,
    CAPTURE_PERMISSION_EXPLANATION,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    Brush,
    Eraser,
    Rectangle,
    Ellipse,
    Lasso,
    Wand,
    Transform,
}
impl Tool {
    pub const ALL: [Self; 7] = [
        Self::Brush,
        Self::Eraser,
        Self::Rectangle,
        Self::Ellipse,
        Self::Lasso,
        Self::Wand,
        Self::Transform,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::Brush => "Brush",
            Self::Eraser => "Eraser",
            Self::Rectangle => "Rectangle",
            Self::Ellipse => "Ellipse",
            Self::Lasso => "Lasso",
            Self::Wand => "Magic wand",
            Self::Transform => "Transform",
        }
    }
}
#[derive(Clone, Debug)]
pub struct LayerRow {
    pub id: u32,
    pub name: String,
    pub visible: bool,
    pub locked: bool,
    pub opacity: f32,
    pub mode: String,
    pub group: bool,
    pub raster: bool,
    pub parent: Option<u32>,
    pub mask: Option<u32>,
    pub clip_base: Option<u32>,
}
#[derive(Clone, Debug, Default)]
pub struct AnnotationSnapshot {
    pub displays: Vec<DisplayDescriptor>,
    pub windows: Vec<WindowDescriptor>,
    pub canvas_display: Option<u32>,
    pub overlay_active: bool,
    pub overlay_drawing: bool,
    pub capture_busy: bool,
}
#[derive(Clone, Debug, Default)]
pub struct PressureEditorSnapshot {
    pub global: ResponseCurve,
    pub per_brush: Option<ResponseCurve>,
    pub calibration_before: Vec<u64>,
    pub calibration_after: Vec<u64>,
}
#[derive(Clone, Debug)]
pub struct EditorSnapshot {
    pub title: String,
    pub dimensions: [u32; 2],
    pub layers: Vec<LayerRow>,
    pub active: Option<u32>,
    pub brushes: Vec<String>,
    pub brush: usize,
    pub brush_definition: Option<BrushDefinition>,
    pub pressure: PressureEditorSnapshot,
    pub annotation: AnnotationSnapshot,
    pub can_undo: bool,
    pub can_redo: bool,
    pub status: String,
    pub memory_bytes: u64,
    pub latency: String,
}
#[derive(Clone, Debug)]
pub enum EditorCommand {
    New {
        path: String,
        dimensions: [u32; 2],
    },
    Open(String),
    Save,
    Undo,
    Redo,
    Export {
        path: String,
        format: String,
    },
    ImportReference(String),
    AddLayer {
        group: bool,
    },
    SelectLayer(u32),
    DeleteLayer(u32),
    LayerVisible(u32, bool),
    LayerLocked(u32, bool),
    LayerOpacity(u32, f32),
    LayerName(u32, String),
    LayerMode(u32, String),
    LayerParent(u32, Option<u32>),
    LayerMask(u32, Option<u32>),
    LayerClipBase(u32, Option<u32>),
    Reorder(u32, i32),
    MergeDown(u32),
    SetTool(Tool),
    SetBrush(usize),
    UpdateBrush(Box<BrushDefinition>),
    ImportBrush(String),
    ExportBrush(String),
    SetBrushTexture {
        shape: bool,
        path: String,
    },
    SetGlobalPressureCurve(ResponseCurve),
    SetBrushPressureCurve(Option<ResponseCurve>),
    SetRadius(f32),
    SetOpacity(f32),
    SetColor([u8; 3]),
    SelectAll,
    ClearSelection,
    InvertSelection,
    Feather(u32),
    Grow(i32),
    Copy,
    Cut,
    Paste,
    Transform {
        move_by: [f64; 2],
        scale: [f64; 2],
        degrees: f64,
    },
    SetMousePainting(bool),
    SetStabilization(String),
    SetMemoryBudget(u64),
    Calibrate,
    RefreshDesktop,
    SelectCanvasDisplay(u32),
    StartOverlay {
        target: OverlayTarget,
    },
    SetOverlayDrawing(bool),
    Capture {
        target: CaptureTarget,
        path: String,
        request_permission: bool,
    },
    StopOverlay,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointerPhase {
    Begin,
    Move,
    End,
}
#[derive(Clone, Copy, Debug)]
pub struct CanvasEvent {
    pub position: [f32; 2],
    pub phase: PointerPhase,
    pub suspend: bool,
}
#[derive(Clone, Copy, Debug)]
pub struct CanvasGeometry {
    pub origin: [f32; 2],
    pub scale: f32,
    pub document: [u32; 2],
}
impl CanvasGeometry {
    pub fn document_position(self, screen: [f32; 2]) -> Option<[f32; 2]> {
        let p = [
            (screen[0] - self.origin[0]) / self.scale,
            (screen[1] - self.origin[1]) / self.scale,
        ];
        (p[0] >= 0.0
            && p[1] >= 0.0
            && p[0] < self.document[0] as f32
            && p[1] < self.document[1] as f32)
            .then_some(p)
    }
}
pub struct EditorUi {
    pub tool: Tool,
    pub radius: f32,
    pub opacity: f32,
    pub color: [u8; 3],
    pub mouse_painting: bool,
    pub canvas_window: bool,
    pub fullscreen: bool,
    pub zoom: f32,
    pub pan: Vec2,
    pub geometry: Option<CanvasGeometry>,
    path: String,
    export_path: String,
    new_size: [u32; 2],
    feather: u32,
    move_by: [f64; 2],
    scale: [f64; 2],
    degrees: f64,
    show_files: bool,
    show_settings: bool,
    show_transform: bool,
    show_about: bool,
    show_brush: bool,
    brush_draft: Option<BrushDefinition>,
    brush_draft_index: Option<usize>,
    brush_error: String,
    brush_path: String,
    shape_path: String,
    grain_path: String,
    stabilization: String,
    memory_mib: u64,
    pointer_down: bool,
    last_pointer: Option<Pos2>,
    selected_window: Option<u32>,
    capture_path: String,
    capture_kind: u8,
    capture_region: [f64; 4],
    capture_confirmation: Option<(CaptureTarget, String)>,
}
impl Default for EditorUi {
    fn default() -> Self {
        Self {
            tool: Tool::Brush,
            radius: 12.0,
            opacity: 1.0,
            color: [32, 65, 56],
            mouse_painting: false,
            canvas_window: true,
            fullscreen: false,
            zoom: 1.0,
            pan: Vec2::ZERO,
            geometry: None,
            path: "drawing.pond".into(),
            export_path: "drawing.png".into(),
            new_size: [2048, 1536],
            feather: 8,
            move_by: [0.0; 2],
            scale: [1.0; 2],
            degrees: 0.0,
            show_files: false,
            show_settings: false,
            show_transform: false,
            show_about: false,
            show_brush: false,
            brush_draft: None,
            brush_draft_index: None,
            brush_error: String::new(),
            brush_path: "brush.quill".into(),
            shape_path: "shape.png".into(),
            grain_path: "grain.png".into(),
            stabilization: "Average".into(),
            memory_mib: 4096,
            pointer_down: false,
            last_pointer: None,
            selected_window: None,
            capture_path: "capture.pond".into(),
            capture_kind: 0,
            capture_region: [0.0, 0.0, 640.0, 480.0],
            capture_confirmation: None,
        }
    }
}
impl EditorUi {
    pub fn controls(&mut self, ctx: &egui::Context, model: &EditorSnapshot) -> Vec<EditorCommand> {
        let mut commands = Vec::new();
        egui::TopBottomPanel::top("editor_menu").show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                if ui.button("Document").clicked() {
                    self.show_files = true;
                }
                if ui
                    .add_enabled(model.can_undo, egui::Button::new("Undo"))
                    .clicked()
                {
                    commands.push(EditorCommand::Undo);
                }
                if ui
                    .add_enabled(model.can_redo, egui::Button::new("Redo"))
                    .clicked()
                {
                    commands.push(EditorCommand::Redo);
                }
                if ui.button("Save").clicked() {
                    commands.push(EditorCommand::Save);
                }
                if ui.button("Settings").clicked() {
                    self.show_settings = true;
                }
                if ui.button("About").clicked() {
                    self.show_about = true;
                }
            });
        });
        egui::SidePanel::left("editor-controls")
            .default_width(420.0)
            .resizable(true)
            .show(ctx, |ui| {
                ui.heading(&model.title);
                ui.label(format!(
                    "{} × {} pixels",
                    model.dimensions[0], model.dimensions[1]
                ));
                ui.separator();
                ui.horizontal_wrapped(|ui| {
                    for tool in Tool::ALL {
                        if ui
                            .selectable_value(&mut self.tool, tool, tool.label())
                            .changed()
                        {
                            commands.push(EditorCommand::SetTool(tool));
                        }
                    }
                });
                ui.horizontal(|ui| {
                    if ui.color_edit_button_srgb(&mut self.color).changed() {
                        commands.push(EditorCommand::SetColor(self.color));
                    }
                    if ui
                        .add(
                            egui::Slider::new(&mut self.radius, 0.5..=256.0)
                                .text("Radius")
                                .logarithmic(true),
                        )
                        .changed()
                    {
                        commands.push(EditorCommand::SetRadius(self.radius));
                    }
                });
                if ui
                    .add(egui::Slider::new(&mut self.opacity, 0.0..=1.0).text("Opacity"))
                    .changed()
                {
                    commands.push(EditorCommand::SetOpacity(self.opacity));
                }
                let mut selected = model.brush;
                egui::ComboBox::from_label("Brush")
                    .selected_text(
                        model
                            .brushes
                            .get(selected)
                            .map_or("Empty brush library", String::as_str),
                    )
                    .show_ui(ui, |ui| {
                        for (index, name) in model.brushes.iter().enumerate() {
                            ui.selectable_value(&mut selected, index, name);
                        }
                    });
                if selected != model.brush {
                    commands.push(EditorCommand::SetBrush(selected));
                }
                if ui.button("Edit brush").clicked() {
                    self.show_brush = true;
                    self.brush_draft = model.brush_definition.clone();
                    self.brush_draft_index = Some(model.brush);
                    self.brush_error.clear();
                }
                ui.collapsing("Selection", |ui| {
                    ui.horizontal_wrapped(|ui| {
                        for (label, command) in [
                            ("Select all", EditorCommand::SelectAll),
                            ("Deselect", EditorCommand::ClearSelection),
                            ("Invert", EditorCommand::InvertSelection),
                            ("Copy", EditorCommand::Copy),
                            ("Cut", EditorCommand::Cut),
                            ("Paste", EditorCommand::Paste),
                        ] {
                            if ui.button(label).clicked() {
                                commands.push(command);
                            }
                        }
                    });
                    ui.add(egui::Slider::new(&mut self.feather, 1..=128).text("Pixels"));
                    ui.horizontal(|ui| {
                        if ui.button("Feather").clicked() {
                            commands.push(EditorCommand::Feather(self.feather));
                        }
                        if ui.button("Grow").clicked() {
                            commands.push(EditorCommand::Grow(self.feather as i32));
                        }
                        if ui.button("Shrink").clicked() {
                            commands.push(EditorCommand::Grow(-(self.feather as i32)));
                        }
                        if ui.button("Transform").clicked() {
                            self.show_transform = true;
                        }
                    });
                });
                ui.separator();
                ui.horizontal(|ui| {
                    ui.heading("Layers");
                    if ui.button("+").on_hover_text("Add paint layer").clicked() {
                        commands.push(EditorCommand::AddLayer { group: false });
                    }
                    if ui.button("Group").clicked() {
                        commands.push(EditorCommand::AddLayer { group: true });
                    }
                });
                egui::ScrollArea::vertical()
                    .max_height(330.0)
                    .show(ui, |ui| {
                        for row in model.layers.iter().rev() {
                            ui.horizontal(|ui| {
                                let mut visible = row.visible;
                                if ui.checkbox(&mut visible, "").changed() {
                                    commands.push(EditorCommand::LayerVisible(row.id, visible));
                                }
                                let label = if row.group {
                                    format!("▸ {}", row.name)
                                } else {
                                    row.name.clone()
                                };
                                if ui
                                    .selectable_label(model.active == Some(row.id), label)
                                    .clicked()
                                {
                                    commands.push(EditorCommand::SelectLayer(row.id));
                                }
                                let mut locked = row.locked;
                                if ui.checkbox(&mut locked, "Lock").changed() {
                                    commands.push(EditorCommand::LayerLocked(row.id, locked));
                                }
                            });
                            if model.active == Some(row.id) {
                                let mut opacity = row.opacity;
                                if ui
                                    .add(
                                        egui::Slider::new(&mut opacity, 0.0..=1.0)
                                            .text("Layer opacity"),
                                    )
                                    .changed()
                                {
                                    commands.push(EditorCommand::LayerOpacity(row.id, opacity));
                                }
                                let mut name = row.name.clone();
                                if ui.text_edit_singleline(&mut name).lost_focus()
                                    && name != row.name
                                {
                                    commands.push(EditorCommand::LayerName(row.id, name));
                                }
                                let mut mode = row.mode.clone();
                                egui::ComboBox::from_id_salt(("mode", row.id))
                                    .selected_text(&mode)
                                    .show_ui(ui, |ui| {
                                        for value in [
                                            "Normal",
                                            "Multiply",
                                            "Screen",
                                            "Overlay",
                                            "Darken",
                                            "Lighten",
                                            "Color dodge",
                                            "Color burn",
                                            "Hard light",
                                            "Soft light",
                                            "Difference",
                                            "Exclusion",
                                            "Hue",
                                            "Saturation",
                                            "Color",
                                            "Luminosity",
                                            "Add",
                                            "Subtract",
                                        ] {
                                            ui.selectable_value(&mut mode, value.into(), value);
                                        }
                                    });
                                if mode != row.mode {
                                    commands.push(EditorCommand::LayerMode(row.id, mode));
                                }
                                let label_for = |id: Option<u32>| {
                                    model
                                        .layers
                                        .iter()
                                        .find(|l| Some(l.id) == id)
                                        .map(|l| l.name.as_str())
                                        .unwrap_or("None")
                                };
                                let mut parent = row.parent;
                                egui::ComboBox::from_id_salt(("parent", row.id))
                                    .selected_text(format!("Group: {}", label_for(parent)))
                                    .show_ui(ui, |ui| {
                                        ui.selectable_value(&mut parent, None, "None");
                                        for candidate in model
                                            .layers
                                            .iter()
                                            .filter(|l| l.group && l.id != row.id)
                                        {
                                            ui.selectable_value(
                                                &mut parent,
                                                Some(candidate.id),
                                                &candidate.name,
                                            );
                                        }
                                    });
                                if parent != row.parent {
                                    commands.push(EditorCommand::LayerParent(row.id, parent));
                                }
                                let mut mask = row.mask;
                                egui::ComboBox::from_id_salt(("mask", row.id))
                                    .selected_text(format!("Mask: {}", label_for(mask)))
                                    .show_ui(ui, |ui| {
                                        ui.selectable_value(&mut mask, None, "None");
                                        for candidate in model
                                            .layers
                                            .iter()
                                            .filter(|l| l.raster && l.id != row.id)
                                        {
                                            ui.selectable_value(
                                                &mut mask,
                                                Some(candidate.id),
                                                &candidate.name,
                                            );
                                        }
                                    });
                                if mask != row.mask {
                                    commands.push(EditorCommand::LayerMask(row.id, mask));
                                }
                                let index = model
                                    .layers
                                    .iter()
                                    .position(|l| l.id == row.id)
                                    .unwrap_or(0);
                                let nearest = model.layers[..index]
                                    .iter()
                                    .rev()
                                    .find(|l| l.parent == row.parent && l.clip_base.is_none());
                                let mut clip = row.clip_base;
                                egui::ComboBox::from_id_salt(("clip", row.id))
                                    .selected_text(format!("Clip base: {}", label_for(clip)))
                                    .show_ui(ui, |ui| {
                                        ui.selectable_value(&mut clip, None, "None");
                                        if let Some(base) = nearest {
                                            ui.selectable_value(
                                                &mut clip,
                                                Some(base.id),
                                                &base.name,
                                            );
                                        }
                                    });
                                if clip != row.clip_base {
                                    commands.push(EditorCommand::LayerClipBase(row.id, clip));
                                }
                                ui.horizontal(|ui| {
                                    if ui.button("↑").clicked() {
                                        commands.push(EditorCommand::Reorder(row.id, 1));
                                    }
                                    if ui.button("↓").clicked() {
                                        commands.push(EditorCommand::Reorder(row.id, -1));
                                    }
                                    if ui.button("Merge down").clicked() {
                                        commands.push(EditorCommand::MergeDown(row.id));
                                    }
                                    if ui.button("Delete").clicked() {
                                        commands.push(EditorCommand::DeleteLayer(row.id));
                                    }
                                });
                            }
                        }
                        if model.layers.is_empty() {
                            ui.label("A quiet canvas, ready for a first layer");
                        }
                    });
                ui.separator();
                ui.horizontal(|ui| {
                    ui.checkbox(&mut self.canvas_window, "Canvas window");
                    ui.checkbox(&mut self.fullscreen, "Full screen");
                    if ui.button("Fit").clicked() {
                        self.zoom = 1.0;
                        self.pan = Vec2::ZERO;
                    }
                });
                ui.label("Scroll to zoom · drag with middle button or Space to pan");
                ui.label(&model.status);
                ui.small(format!(
                    "Resident {:.1} MiB · {}",
                    model.memory_bytes as f64 / 1048576.0,
                    model.latency
                ));
            });
        if self.show_files {
            egui::Window::new("Document")
                .open(&mut self.show_files)
                .show(ctx, |ui| {
                    ui.label("Document package (.pond)");
                    ui.text_edit_singleline(&mut self.path);
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::DragValue::new(&mut self.new_size[0])
                                .range(1..=16384)
                                .prefix("Width "),
                        );
                        ui.add(
                            egui::DragValue::new(&mut self.new_size[1])
                                .range(1..=16384)
                                .prefix("Height "),
                        );
                    });
                    ui.horizontal(|ui| {
                        if ui.button("New").clicked() {
                            commands.push(EditorCommand::New {
                                path: self.path.clone(),
                                dimensions: self.new_size,
                            });
                        }
                        if ui.button("Open").clicked() {
                            commands.push(EditorCommand::Open(self.path.clone()));
                        }
                        if ui.button("Import reference").clicked() {
                            commands.push(EditorCommand::ImportReference(self.path.clone()));
                        }
                    });
                    ui.separator();
                    ui.label("Export path");
                    ui.text_edit_singleline(&mut self.export_path);
                    ui.horizontal(|ui| {
                        for format in ["PNG", "TIFF", "JPEG", "PDF"] {
                            if ui.button(format).clicked() {
                                commands.push(EditorCommand::Export {
                                    path: self.export_path.clone(),
                                    format: format.into(),
                                });
                            }
                        }
                    });
                });
        }
        if self.show_settings {
            egui::Window::new("Settings")
                .open(&mut self.show_settings)
                .vscroll(true)
                .show(ctx, |ui| {
                    if ui
                        .checkbox(&mut self.mouse_painting, "Paint with mouse")
                        .changed()
                    {
                        commands.push(EditorCommand::SetMousePainting(self.mouse_painting));
                    }
                    let previous = self.stabilization.clone();
                    egui::ComboBox::from_label("Stabilization")
                        .selected_text(&self.stabilization)
                        .show_ui(ui, |ui| {
                            for value in ["None", "Average", "Pull string"] {
                                ui.selectable_value(&mut self.stabilization, value.into(), value);
                            }
                        });
                    if previous != self.stabilization {
                        commands.push(EditorCommand::SetStabilization(self.stabilization.clone()));
                    }
                    if ui
                        .add(
                            egui::Slider::new(&mut self.memory_mib, 512..=16384)
                                .text("Memory ceiling (MiB)"),
                        )
                        .changed()
                    {
                        commands.push(EditorCommand::SetMemoryBudget(self.memory_mib * 1048576));
                    }
                    if ui
                        .button("Calibrate pressure from recent strokes")
                        .clicked()
                    {
                        commands.push(EditorCommand::Calibrate);
                    }
                    ui.collapsing("Pressure response", |ui| {
                        ui.label("Global pressure curve");
                        let mut global = model.pressure.global.clone();
                        if curve_editor(ui, "global_pressure", &mut global) {
                            commands.push(EditorCommand::SetGlobalPressureCurve(global));
                        }
                        let mut enabled = model.pressure.per_brush.is_some();
                        if ui
                            .checkbox(&mut enabled, "Use a pressure curve for this brush")
                            .changed()
                        {
                            commands.push(EditorCommand::SetBrushPressureCurve(
                                enabled.then(ResponseCurve::default),
                            ));
                        }
                        if enabled {
                            let mut curve = model.pressure.per_brush.clone().unwrap_or_default();
                            if curve_editor(ui, "brush_pressure", &mut curve) {
                                commands.push(EditorCommand::SetBrushPressureCurve(Some(curve)));
                            }
                        }
                        pressure_histograms(
                            ui,
                            &model.pressure.calibration_before,
                            &model.pressure.calibration_after,
                        );
                    });
                    ui.separator();
                    ui.label("Annotation");
                    if ui.button("Refresh displays and windows").clicked(){commands.push(EditorCommand::RefreshDesktop);}
                    let mut display=model.annotation.canvas_display;
                    let display_name=model.annotation.displays.iter().find(|d|Some(d.id)==display).map(|d|d.name.as_str()).unwrap_or("Choose a display");
                    egui::ComboBox::from_id_salt("canvas_display").selected_text(display_name).show_ui(ui,|ui|{
                        for item in &model.annotation.displays {ui.selectable_value(&mut display,Some(item.id),format!("{} · {} × {}",item.name,item.pixel_size[0],item.pixel_size[1]));}
                    });
                    if display!=model.annotation.canvas_display {if let Some(id)=display{commands.push(EditorCommand::SelectCanvasDisplay(id));}}
                    ui.small("Choose the iPad display manually when its identity is unavailable.");
                    if ui.add_enabled(display.is_some(),egui::Button::new("Live display overlay")).clicked(){commands.push(EditorCommand::StartOverlay{target:OverlayTarget::Display(display.unwrap())});}
                    let window_name=model.annotation.windows.iter().find(|w|Some(w.id)==self.selected_window).map(|w|format!("{} · {}",w.owner,w.title)).unwrap_or_else(||"Choose a window".into());
                    egui::ComboBox::from_id_salt("overlay_window").selected_text(window_name).show_ui(ui,|ui|{
                        for window in &model.annotation.windows {ui.selectable_value(&mut self.selected_window,Some(window.id),format!("{} · {} [{}]",window.owner,window.title,window.id));}
                    });
                    let valid_window=self.selected_window.filter(|id|model.annotation.windows.iter().any(|w|w.id==*id && w.on_screen));
                    if ui.add_enabled(valid_window.is_some(),egui::Button::new("Pinned window overlay")).clicked(){commands.push(EditorCommand::StartOverlay{target:OverlayTarget::Window(valid_window.unwrap())});}
                    let mut drawing=model.annotation.overlay_drawing;
                    if ui.add_enabled(model.annotation.overlay_active,egui::Checkbox::new(&mut drawing,"Draw on overlay")).changed(){commands.push(EditorCommand::SetOverlayDrawing(drawing));}
                    ui.small("Draw accepts pointer events; turn it off to click through to other apps. Painting still follows stylus-only settings.");
                    if ui.add_enabled(model.annotation.overlay_active,egui::Button::new("Close overlay")).clicked() {
                        commands.push(EditorCommand::StopOverlay);
                    }
                    ui.separator();ui.label("Capture into a new document");ui.text_edit_singleline(&mut self.capture_path);
                    egui::ComboBox::from_id_salt("capture_target").selected_text(["Selected display","Selected window","Display region"][self.capture_kind as usize]).show_ui(ui,|ui|{
                        ui.selectable_value(&mut self.capture_kind,0,"Selected display");ui.selectable_value(&mut self.capture_kind,1,"Selected window");ui.selectable_value(&mut self.capture_kind,2,"Display region");
                    });
                    if self.capture_kind==2 {ui.horizontal_wrapped(|ui|{for (index,label) in ["X ","Y ","Width ","Height "].into_iter().enumerate(){ui.add(egui::DragValue::new(&mut self.capture_region[index]).prefix(label));}});ui.small("Region coordinates are logical points from the selected display’s top-left corner.");}
                    let target=match self.capture_kind {
                        1=>valid_window.map(CaptureTarget::Window),
                        2=>model.annotation.displays.iter().find(|d|Some(d.id)==display).map(|d|CaptureTarget::Region(DesktopRect{x:d.frame.x+self.capture_region[0],y:d.frame.y+self.capture_region[1],width:self.capture_region[2],height:self.capture_region[3]})),
                        _=>display.map(CaptureTarget::Display),
                    };
                    if ui.add_enabled(!model.annotation.capture_busy && target.is_some(),egui::Button::new("Capture to document…")).clicked(){self.capture_confirmation=target.map(|target|(target,self.capture_path.clone()));}
                    if model.annotation.capture_busy{ui.label("Capture in progress…");}
                });
        }
        if let Some((target, path)) = self.capture_confirmation.clone() {
            egui::Window::new("Capture permission")
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.label(CAPTURE_PERMISSION_EXPLANATION);
                    ui.label(format!("Save capture as {path}"));
                    ui.horizontal(|ui| {
                        if ui.button("Continue and capture").clicked() {
                            commands.push(EditorCommand::Capture {
                                target,
                                path,
                                request_permission: true,
                            });
                            self.capture_confirmation = None;
                        }
                        if ui.button("Cancel").clicked() {
                            self.capture_confirmation = None;
                        }
                    });
                });
        }
        if self.show_brush {
            if self.brush_draft_index != Some(model.brush) {
                self.brush_draft = model.brush_definition.clone();
                self.brush_draft_index = Some(model.brush);
                self.brush_error.clear();
            }
            egui::Window::new("Brush editor")
                .open(&mut self.show_brush)
                .default_width(460.0)
                .vscroll(true)
                .show(ctx, |ui| {
                    let mut reload = false;
                    if let Some(definition) = &mut self.brush_draft {
                        ui.horizontal(|ui| {
                            ui.label("Name");
                            ui.text_edit_singleline(&mut definition.name);
                        });
                        ui.checkbox(&mut definition.eraser, "Erase pixels");
                        let parameters = &mut definition.parameters;
                        for (label, parameter, range) in [
                            ("Diameter (pixels)", &mut parameters.size, 0.1..=2048.0),
                            ("Spacing (diameters)", &mut parameters.spacing, 0.001..=4.0),
                            ("Scatter (radii)", &mut parameters.scatter, 0.0..=16.0),
                            (
                                "Rotation (radians)",
                                &mut parameters.rotation,
                                -std::f32::consts::TAU..=std::f32::consts::TAU,
                            ),
                            (
                                "Follow direction",
                                &mut parameters.rotation_follows_direction,
                                0.0..=1.0,
                            ),
                            ("Flow", &mut parameters.flow, 0.0..=1.0),
                            ("Stroke opacity", &mut parameters.opacity, 0.0..=1.0),
                            ("Hardness", &mut parameters.hardness, 0.0..=1.0),
                            (
                                "Grain scale (pixels)",
                                &mut parameters.grain_scale,
                                0.001..=4096.0,
                            ),
                            ("Grain motion", &mut parameters.grain_motion, -8.0..=8.0),
                            ("Wet mix", &mut parameters.wet_mix, 0.0..=1.0),
                            (
                                "Pickup length (pixels)",
                                &mut parameters.pickup_length,
                                0.0..=4096.0,
                            ),
                            ("Hue jitter", &mut parameters.hue_jitter, 0.0..=1.0),
                            (
                                "Saturation jitter",
                                &mut parameters.saturation_jitter,
                                0.0..=1.0,
                            ),
                            ("Value jitter", &mut parameters.value_jitter, 0.0..=1.0),
                            ("Smoothing", &mut parameters.smoothing, 0.0..=1.0),
                        ] {
                            parameter_editor(ui, label, parameter, range);
                        }
                        ui.horizontal(|ui| {
                            if ui.button("Apply brush changes").clicked() {
                                match definition.validate() {
                                    Ok(()) => {
                                        commands.push(EditorCommand::UpdateBrush(Box::new(
                                            definition.clone(),
                                        )));
                                        self.brush_error.clear();
                                    }
                                    Err(error) => self.brush_error = error,
                                }
                            }
                            if ui.button("Reload current brush").clicked() {
                                reload = true;
                                self.brush_error.clear();
                            }
                        });
                    } else {
                        ui.label("Import a brush package to begin editing");
                    }
                    if reload {
                        self.brush_draft = model.brush_definition.clone();
                    }
                    if !self.brush_error.is_empty() {
                        ui.colored_label(Color32::LIGHT_RED, &self.brush_error);
                    }
                    ui.separator();
                    ui.label("Brush package (.quill)");
                    ui.text_edit_singleline(&mut self.brush_path);
                    ui.horizontal(|ui| {
                        if ui.button("Import package").clicked() {
                            commands.push(EditorCommand::ImportBrush(self.brush_path.clone()));
                        }
                        if ui
                            .add_enabled(
                                model.brush_definition.is_some(),
                                egui::Button::new("Save package"),
                            )
                            .clicked()
                        {
                            commands.push(EditorCommand::ExportBrush(self.brush_path.clone()));
                        }
                    });
                    ui.label("Shape PNG");
                    ui.horizontal(|ui| {
                        ui.text_edit_singleline(&mut self.shape_path);
                        if ui.button("Load shape").clicked() {
                            commands.push(EditorCommand::SetBrushTexture {
                                shape: true,
                                path: self.shape_path.clone(),
                            });
                        }
                    });
                    ui.label("Grain PNG");
                    ui.horizontal(|ui| {
                        ui.text_edit_singleline(&mut self.grain_path);
                        if ui.button("Load grain").clicked() {
                            commands.push(EditorCommand::SetBrushTexture {
                                shape: false,
                                path: self.grain_path.clone(),
                            });
                        }
                    });
                });
        }
        if self.show_transform {
            egui::Window::new("Transform selection")
                .open(&mut self.show_transform)
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Move");
                        for value in &mut self.move_by {
                            ui.add(egui::DragValue::new(value));
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.label("Scale");
                        for value in &mut self.scale {
                            ui.add(egui::DragValue::new(value).speed(0.01));
                        }
                    });
                    ui.add(egui::DragValue::new(&mut self.degrees).suffix("°"));
                    if ui.button("Apply").clicked() {
                        commands.push(EditorCommand::Transform {
                            move_by: self.move_by,
                            scale: self.scale,
                            degrees: self.degrees,
                        });
                    }
                });
        }
        if self.show_about {
            egui::Window::new("About sideDuck")
                .open(&mut self.show_about)
                .show(ctx, |ui| {
                    ui.heading("sideDuck");
                    ui.label(env!("CARGO_PKG_VERSION"));
                    ui.label("Local painting and annotation");
                });
        }
        let shortcuts = ctx.input(|i| {
            (
                i.modifiers.command,
                i.modifiers.shift,
                i.key_pressed(egui::Key::Z),
                i.key_pressed(egui::Key::S),
            )
        });
        if shortcuts.0 && shortcuts.2 {
            commands.push(if shortcuts.1 {
                EditorCommand::Redo
            } else {
                EditorCommand::Undo
            });
        }
        if shortcuts.0 && shortcuts.3 {
            commands.push(EditorCommand::Save);
        }
        commands
    }
    pub fn canvas(
        &mut self,
        ctx: &egui::Context,
        dimensions: [u32; 2],
        texture: Option<egui::TextureId>,
        transparent: bool,
    ) -> Vec<CanvasEvent> {
        let mut events = Vec::new();
        if !transparent {
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(self.fullscreen));
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(if transparent {
                Color32::TRANSPARENT
            } else {
                Color32::from_gray(38)
            }))
            .show(ctx, |ui| {
                let available = ui.available_rect_before_wrap();
                let response = ui.allocate_rect(available, Sense::click_and_drag());
                let fit = (available.width() / dimensions[0] as f32)
                    .min(available.height() / dimensions[1] as f32)
                    .max(0.0001);
                let pointer = ctx.input(|i| i.pointer.hover_pos());
                if response.hovered() {
                    let scroll = ctx.input(|i| i.smooth_scroll_delta.y);
                    if scroll != 0.0 {
                        let before = self.zoom;
                        self.zoom = (self.zoom * (scroll * 0.002).exp()).clamp(0.02, 64.0);
                        if let Some(p) = pointer {
                            self.pan = (self.pan - (p - available.center())) * (self.zoom / before)
                                + (p - available.center());
                        }
                    }
                }
                let panning = ctx.input(|i| {
                    i.pointer.middle_down()
                        || (i.key_down(egui::Key::Space) && i.pointer.primary_down())
                });
                if panning && response.hovered() {
                    self.pan += ctx.input(|i| i.pointer.delta());
                }
                let scale = fit * self.zoom;
                let size = Vec2::new(dimensions[0] as f32 * scale, dimensions[1] as f32 * scale);
                let rect = Rect::from_center_size(available.center() + self.pan, size);
                self.geometry = Some(CanvasGeometry {
                    origin: [rect.min.x, rect.min.y],
                    scale,
                    document: dimensions,
                });
                if !transparent {
                    ui.painter().rect_filled(rect, 0.0, Color32::WHITE);
                }
                if let Some(texture) = texture {
                    ui.painter().image(
                        texture,
                        rect,
                        Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                        Color32::WHITE,
                    );
                }
                let pressed = ctx.input(|i| i.pointer.primary_down()) && !panning;
                let position = pointer.and_then(|p| self.geometry?.document_position([p.x, p.y]));
                let suspend = ctx.input(|i| i.modifiers.alt);
                if let Some(position) = position.filter(|_| pressed && response.hovered()) {
                    let phase = if self.pointer_down {
                        PointerPhase::Move
                    } else {
                        PointerPhase::Begin
                    };
                    if !self.pointer_down || pointer != self.last_pointer {
                        events.push(CanvasEvent {
                            position,
                            phase,
                            suspend,
                        });
                    }
                    self.pointer_down = true;
                    self.last_pointer = pointer;
                } else if self.pointer_down {
                    if let Some(p) = self
                        .last_pointer
                        .and_then(|p| self.geometry?.document_position([p.x, p.y]))
                    {
                        events.push(CanvasEvent {
                            position: p,
                            phase: PointerPhase::End,
                            suspend,
                        });
                    }
                    self.pointer_down = false;
                    self.last_pointer = None;
                }
            });
        events
    }
}

fn parameter_editor(
    ui: &mut egui::Ui,
    label: &str,
    parameter: &mut Parameter,
    range: std::ops::RangeInclusive<f32>,
) {
    ui.push_id(label, |ui| {
        ui.horizontal(|ui| {
            ui.label(label);
            ui.add(
                egui::DragValue::new(&mut parameter.value)
                    .range(range)
                    .speed(0.01),
            );
        });
        ui.collapsing("Response", |ui| {
            let mut selected = parameter.response.as_ref().map(|response| response.driver);
            egui::ComboBox::from_id_salt("driver")
                .selected_text(selected.map_or("Constant", driver_label))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut selected, None, "Constant");
                    for driver in [
                        Driver::Pressure,
                        Driver::Tilt,
                        Driver::Azimuth,
                        Driver::Velocity,
                        Driver::Random,
                    ] {
                        ui.selectable_value(&mut selected, Some(driver), driver_label(driver));
                    }
                });
            if selected != parameter.response.as_ref().map(|response| response.driver) {
                parameter.response = selected.map(|driver| Response {
                    driver,
                    curve: ResponseCurve::default(),
                    minimum: 0.0,
                    maximum: 1.0,
                });
            }
            if let Some(response) = &mut parameter.response {
                ui.horizontal(|ui| {
                    ui.label("Minimum factor");
                    ui.add(
                        egui::DragValue::new(&mut response.minimum)
                            .range(0.0..=response.maximum)
                            .speed(0.01),
                    );
                    ui.label("Maximum factor");
                    ui.add(
                        egui::DragValue::new(&mut response.maximum)
                            .range(response.minimum..=16.0)
                            .speed(0.01),
                    );
                });
                curve_editor(ui, "parameter_curve", &mut response.curve);
            }
        });
    });
}
fn driver_label(driver: Driver) -> &'static str {
    match driver {
        Driver::Pressure => "Pressure",
        Driver::Tilt => "Tilt",
        Driver::Azimuth => "Azimuth",
        Driver::Velocity => "Velocity",
        Driver::Random => "Random",
    }
}
fn move_curve_point(curve: &mut ResponseCurve, index: usize, point: [f32; 2]) {
    if index >= curve.points.len()
        || curve.points.len() < 2
        || point.iter().any(|value| !value.is_finite())
    {
        return;
    }
    let x = if index == 0 {
        0.0
    } else if index + 1 == curve.points.len() {
        1.0
    } else {
        point[0].clamp(
            curve.points[index - 1][0].next_up(),
            curve.points[index + 1][0].next_down(),
        )
    };
    curve.points[index] = [x, point[1].clamp(0.0, 1.0)];
}
fn curve_editor(ui: &mut egui::Ui, id: &str, curve: &mut ResponseCurve) -> bool {
    let previous = curve.clone();
    ui.push_id(id, |ui| {
        if curve.validate().is_err() {
            *curve = ResponseCurve::default();
        }
        let (rect, response) = ui.allocate_exact_size(
            Vec2::new(ui.available_width().clamp(140.0, 360.0), 130.0),
            Sense::click_and_drag(),
        );
        let plot = rect.shrink(8.0);
        let to_screen = |point: [f32; 2]| {
            Pos2::new(
                plot.left() + point[0] * plot.width(),
                plot.bottom() - point[1] * plot.height(),
            )
        };
        ui.painter().rect_filled(rect, 4.0, Color32::from_gray(24));
        for fraction in [0.0, 0.25, 0.5, 0.75, 1.0] {
            ui.painter().line_segment(
                [to_screen([fraction, 0.0]), to_screen([fraction, 1.0])],
                egui::Stroke::new(1.0, Color32::from_gray(50)),
            );
            ui.painter().line_segment(
                [to_screen([0.0, fraction]), to_screen([1.0, fraction])],
                egui::Stroke::new(1.0, Color32::from_gray(50)),
            );
        }
        let selected_id = response.id.with("selected_point");
        if response.drag_started() {
            if let Some(pointer) = response.interact_pointer_pos() {
                let nearest = curve
                    .points
                    .iter()
                    .enumerate()
                    .min_by(|(_, a), (_, b)| {
                        to_screen(**a)
                            .distance_sq(pointer)
                            .total_cmp(&to_screen(**b).distance_sq(pointer))
                    })
                    .map(|(index, _)| index);
                if let Some(index) = nearest {
                    ui.data_mut(|data| data.insert_temp(selected_id, index));
                }
            }
        }
        if response.dragged() {
            if let (Some(index), Some(pointer)) = (
                ui.data(|data| data.get_temp::<usize>(selected_id)),
                response.interact_pointer_pos(),
            ) {
                move_curve_point(
                    curve,
                    index,
                    [
                        (pointer.x - plot.left()) / plot.width(),
                        (plot.bottom() - pointer.y) / plot.height(),
                    ],
                );
            }
        }
        for pair in curve.points.windows(2) {
            ui.painter().line_segment(
                [to_screen(pair[0]), to_screen(pair[1])],
                egui::Stroke::new(2.0, Color32::LIGHT_BLUE),
            );
        }
        for point in &curve.points {
            ui.painter()
                .circle_filled(to_screen(*point), 4.0, Color32::WHITE);
        }
        ui.small("Drag points · left to right: input · bottom to top: response");
        ui.horizontal(|ui| {
            if ui
                .add_enabled(curve.points.len() < 32, egui::Button::new("Add point"))
                .clicked()
            {
                if let Some((index, pair)) = curve
                    .points
                    .windows(2)
                    .enumerate()
                    .max_by(|(_, a), (_, b)| (a[1][0] - a[0][0]).total_cmp(&(b[1][0] - b[0][0])))
                {
                    let midpoint = [
                        (pair[0][0] + pair[1][0]) * 0.5,
                        (pair[0][1] + pair[1][1]) * 0.5,
                    ];
                    curve.points.insert(index + 1, midpoint);
                }
            }
            if ui.button("Reset linear").clicked() {
                *curve = ResponseCurve::default();
            }
        });
        let mut remove = None;
        egui::Grid::new("curve_points")
            .num_columns(4)
            .show(ui, |ui| {
                ui.label("Point");
                ui.label("Input");
                ui.label("Response");
                ui.end_row();
                for index in 0..curve.points.len() {
                    ui.label((index + 1).to_string());
                    let mut point = curve.points[index];
                    let endpoint = index == 0 || index + 1 == curve.points.len();
                    let x = ui
                        .add_enabled(
                            !endpoint,
                            egui::DragValue::new(&mut point[0])
                                .range(0.0..=1.0)
                                .speed(0.005),
                        )
                        .changed();
                    let y = ui
                        .add(
                            egui::DragValue::new(&mut point[1])
                                .range(0.0..=1.0)
                                .speed(0.005),
                        )
                        .changed();
                    if x || y {
                        move_curve_point(curve, index, point);
                    }
                    if ui
                        .add_enabled(!endpoint, egui::Button::new("Remove"))
                        .clicked()
                    {
                        remove = Some(index);
                    }
                    ui.end_row();
                }
            });
        if let Some(index) = remove {
            curve.points.remove(index);
        }
    });
    *curve != previous
}
fn pressure_histograms(ui: &mut egui::Ui, before: &[u64], after: &[u64]) {
    if before.is_empty() || after.is_empty() {
        ui.small(
            "Draw sample strokes with varied pressure, then calibrate to see the distribution.",
        );
        return;
    }
    ui.label("Calibration: observed pressure / calibrated pressure");
    let bins = before.len().min(after.len());
    let maximum = before
        .iter()
        .chain(after)
        .copied()
        .max()
        .unwrap_or(1)
        .max(1) as f32;
    let (rect, _) = ui.allocate_exact_size(
        Vec2::new(ui.available_width().clamp(140.0, 360.0), 100.0),
        Sense::hover(),
    );
    let bar_width = rect.width() / bins as f32;
    for index in 0..bins {
        for (offset, count, color) in [
            (0.0, before[index], Color32::from_rgb(120, 150, 180)),
            (0.5, after[index], Color32::from_rgb(80, 210, 170)),
        ] {
            let x = rect.left() + (index as f32 + offset) * bar_width;
            let bar = Rect::from_min_max(
                Pos2::new(x, rect.bottom() - count as f32 / maximum * rect.height()),
                Pos2::new(x + bar_width * 0.45, rect.bottom()),
            );
            ui.painter().rect_filled(bar, 0.0, color);
        }
    }
    ui.horizontal(|ui| {
        ui.colored_label(Color32::from_rgb(120, 150, 180), "Before");
        ui.colored_label(Color32::from_rgb(80, 210, 170), "After");
        ui.label(format!("{} samples", before.iter().sum::<u64>()));
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn canvas_mapping_handles_zoom_pan_and_outside_coordinates() {
        let geometry = CanvasGeometry {
            origin: [-40.0, 20.0],
            scale: 2.0,
            document: [100, 50],
        };
        assert_eq!(geometry.document_position([0.0, 40.0]), Some([20.0, 10.0]));
        assert_eq!(geometry.document_position([160.0, 40.0]), None);
    }
    #[test]
    fn controls_render_all_panels_without_document_simulation() {
        let context = egui::Context::default();
        let mut editor = EditorUi::default();
        let model = EditorSnapshot {
            title: "Scratch".into(),
            dimensions: [100, 80],
            layers: vec![],
            active: None,
            brushes: vec![],
            brush: 0,
            brush_definition: None,
            pressure: PressureEditorSnapshot::default(),
            can_undo: false,
            can_redo: false,
            status: String::new(),
            memory_bytes: 0,
            latency: String::new(),
            annotation: AnnotationSnapshot::default(),
        };
        let result = context.run(Default::default(), |ctx| {
            assert!(editor.controls(ctx, &model).is_empty());
        });
        assert!(!result.shapes.is_empty());
    }
    #[test]
    fn curve_editor_preserves_normalized_sorted_endpoints_and_dense_knots() {
        let mut curve = ResponseCurve {
            points: vec![[0.0, 0.0], [0.3, 0.4], [0.30000004, 0.5], [1.0, 1.0]],
        };
        move_curve_point(&mut curve, 1, [0.9, 2.0]);
        curve.validate().unwrap();
        assert!(curve.points[1][0] < curve.points[2][0]);
        assert_eq!(curve.points[1][1], 1.0);
        move_curve_point(&mut curve, 0, [0.8, -1.0]);
        assert_eq!(curve.points[0], [0.0, 0.0]);
        move_curve_point(&mut curve, 3, [-4.0, 0.5]);
        assert_eq!(curve.points[3], [1.0, 0.5]);
        let old = curve.clone();
        move_curve_point(&mut curve, 99, [0.5, 0.5]);
        move_curve_point(&mut curve, 1, [f32::NAN, 0.5]);
        assert_eq!(curve, old);
    }
    #[test]
    fn full_brush_editor_and_calibration_panels_render_without_mutating_document() {
        let (_, presets) = sideduck_brush::builtin_library().unwrap();
        let context = egui::Context::default();
        let mut editor = EditorUi {
            show_brush: true,
            show_settings: true,
            ..Default::default()
        };
        let model = EditorSnapshot {
            title: "Brush editing".into(),
            dimensions: [256, 256],
            layers: vec![],
            active: None,
            brushes: presets
                .iter()
                .map(|preset| preset.definition.name.clone())
                .collect(),
            brush: 0,
            brush_definition: Some(presets[0].definition.clone()),
            pressure: PressureEditorSnapshot {
                global: ResponseCurve::default(),
                per_brush: Some(ResponseCurve::default()),
                calibration_before: vec![0, 2, 6, 2],
                calibration_after: vec![2, 3, 3, 2],
            },
            can_undo: false,
            can_redo: false,
            status: String::new(),
            memory_bytes: 0,
            latency: String::new(),
            annotation: AnnotationSnapshot::default(),
        };
        let output = context.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1600.0, 1200.0))),
                ..Default::default()
            },
            |ctx| {
                assert!(editor.controls(ctx, &model).is_empty());
            },
        );
        assert!(!output.shapes.is_empty());
        assert_eq!(editor.brush_draft, model.brush_definition);
    }
}
