//! Prototype controls and canvas presentation. Rasterization stays in sideduck-brush.
pub mod editor;

use eframe::egui::{self, Color32, RichText, Vec2};
use serde::Deserialize;
use sideduck_brush::{BrushSettings, Canvas};
use sideduck_input::{CanvasMapping, ContactTracker, Point};
use sideduck_platform::{ContactPhase, TabletSample, TabletSource};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const INK: Color32 = Color32::from_rgb(30, 57, 50);
const MUTED: Color32 = Color32::from_rgb(103, 113, 105);
const PAPER: Color32 = Color32::from_rgb(246, 246, 240);
const CANVAS_TITLE: &str = "sideDuck Canvas";

#[derive(Clone, Deserialize)]
pub struct PrototypeConfig {
    pub canvas_width: u32,
    pub canvas_height: u32,
    pub input_queue_capacity: usize,
    pub mouse_pressure: f32,
    pub tablet_mouse_suppression_ms: u64,
    pub canvas_repaint_ms: u64,
    pub controls_repaint_ms: u64,
    pub brush: BrushSettings,
}
impl PrototypeConfig {
    pub fn load(path: Option<&Path>) -> Result<Self, Box<dyn std::error::Error>> {
        let text = match path {
            Some(p) => std::fs::read_to_string(p)?,
            None => include_str!("../../../config/prototype.json").to_owned(),
        };
        let config: Self = serde_json::from_str(&text)?;
        config.brush.validate()?;
        if !(64..=4096).contains(&config.canvas_width)
            || !(64..=4096).contains(&config.canvas_height)
            || !(1..=65536).contains(&config.input_queue_capacity)
            || !(0.01..=1.0).contains(&config.mouse_pressure)
            || !(1..=1000).contains(&config.tablet_mouse_suppression_ms)
            || !(1..=1000).contains(&config.canvas_repaint_ms)
            || !(1..=1000).contains(&config.controls_repaint_ms)
        {
            return Err("Invalid prototype configuration".into());
        }
        Ok(config)
    }
}

pub struct PrototypeApp {
    source: Box<dyn TabletSource>,
    config: PrototypeConfig,
    canvas: Canvas,
    texture: Option<egui::TextureHandle>,
    texture_revision: Option<u64>,
    tracker: ContactTracker,
    pending: Vec<TabletSample>,
    latest: Option<TabletSample>,
    samples: u64,
    dropped: u64,
    mouse_mode: bool,
    mouse_down: bool,
    eraser: bool,
    color: [u8; 3],
    canvas_open: bool,
    fullscreen: bool,
    show_clear_confirmation: bool,
    status: String,
    export_directory: PathBuf,
    started: Instant,
    smoke_duration: Option<Duration>,
    last_tablet: Option<Instant>,
    last_mapping: Option<CanvasMapping>,
}
impl PrototypeApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        config: PrototypeConfig,
        export_directory: PathBuf,
        smoke_duration: Option<Duration>,
        demo: bool,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let source = sideduck_platform::create_tablet_source(config.input_queue_capacity)?;
        let mut visuals = egui::Visuals::light();
        visuals.panel_fill = PAPER;
        visuals.window_fill = PAPER;
        visuals.override_text_color = Some(INK);
        visuals.selection.bg_fill = Color32::from_rgb(204, 224, 207);
        visuals.selection.stroke.color = INK;
        cc.egui_ctx.set_visuals(visuals);
        cc.egui_ctx.style_mut(|s| {
            s.spacing.item_spacing = Vec2::new(10.0, 10.0);
            s.spacing.button_padding = Vec2::new(14.0, 9.0);
        });
        let mut canvas = Canvas::new(config.canvas_width, config.canvas_height)?;
        if demo {
            draw_demo(&mut canvas, config.brush);
        }
        Ok(Self {
            source,
            config,
            canvas,
            texture: None,
            texture_revision: None,
            tracker: ContactTracker::default(),
            pending: Vec::new(),
            latest: None,
            samples: 0,
            dropped: 0,
            mouse_mode: false,
            mouse_down: false,
            eraser: false,
            color: [35, 73, 60],
            canvas_open: true,
            fullscreen: false,
            show_clear_confirmation: false,
            status: "Scratch canvas · export a PNG to keep your drawing".into(),
            export_directory,
            started: Instant::now(),
            smoke_duration,
            last_tablet: None,
            last_mapping: None,
        })
    }
    fn upload(&mut self, ctx: &egui::Context) {
        if self.texture_revision == Some(self.canvas.revision()) {
            return;
        }
        let image = egui::ColorImage::from_rgba_unmultiplied(
            [self.canvas.width() as usize, self.canvas.height() as usize],
            self.canvas.pixels(),
        );
        match &mut self.texture {
            Some(texture) => texture.set(image, egui::TextureOptions::LINEAR),
            None => {
                self.texture =
                    Some(ctx.load_texture("painting", image, egui::TextureOptions::LINEAR))
            }
        }
        self.texture_revision = Some(self.canvas.revision());
    }
    fn paint(&mut self, point: Option<Point>, eraser: bool) {
        if let Some((a, b)) = self.tracker.segment(point, eraser) {
            self.canvas
                .stroke(a, b, self.config.brush, self.color, eraser);
        }
    }
    fn export(&mut self) {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let path = self.export_directory.join(format!("drawing-{stamp}.png"));
        match export_png(&self.canvas, &path) {
            Ok(()) => self.status = format!("Saved {}", path.display()),
            Err(e) => self.status = format!("Export failed: {e}"),
        }
    }
    fn canvas_ui(&mut self, ctx: &egui::Context) {
        if ctx.input(|i| i.viewport().close_requested()) {
            self.canvas_open = false;
            self.mouse_down = false;
            self.pending.clear();
            self.tracker.reset();
            return;
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.fullscreen = false;
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(self.fullscreen));
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(Color32::from_rgb(221, 225, 217)))
            .show(ctx, |ui| {
                let available = ui.available_rect_before_wrap();
                let scale = (available.width() / self.canvas.width() as f32)
                    .min(available.height() / self.canvas.height() as f32)
                    .max(0.001);
                let size = Vec2::new(
                    self.canvas.width() as f32 * scale,
                    self.canvas.height() as f32 * scale,
                );
                let rect = egui::Rect::from_center_size(available.center(), size);
                let mapping = CanvasMapping {
                    origin: [rect.min.x, rect.min.y],
                    display_size: [size.x, size.y],
                    pixel_size: [self.canvas.width(), self.canvas.height()],
                };
                if self.last_mapping != Some(mapping) {
                    self.tracker.reset();
                    self.last_mapping = Some(mapping);
                }
                let window_id = sideduck_platform::window_id_for_title(CANVAS_TITLE);
                let samples = std::mem::take(&mut self.pending);
                for sample in samples {
                    if Some(sample.window_id) != window_id {
                        continue;
                    }
                    let window_size = ctx.screen_rect().size() * ctx.zoom_factor();
                    if (sample.window_size[0] as f32 - window_size.x).abs() > 1.0
                        || (sample.window_size[1] as f32 - window_size.y).abs() > 1.0
                    {
                        self.tracker.reset();
                        continue;
                    }
                    self.last_tablet = Some(Instant::now());
                    self.mouse_down = false;
                    match sample.phase {
                        ContactPhase::Up | ContactPhase::Hover => self.tracker.reset(),
                        ContactPhase::Down | ContactPhase::Move => {
                            if sample.phase == ContactPhase::Down {
                                self.tracker.reset();
                            }
                            let p = sample.position_top_left();
                            self.paint(
                                mapping.map(
                                    [
                                        p[0] as f32 / ctx.zoom_factor(),
                                        p[1] as f32 / ctx.zoom_factor(),
                                    ],
                                    sample.pressure,
                                ),
                                sample.eraser || self.eraser,
                            );
                        }
                    }
                }
                // AppKit tablet input also creates pointer events. Suppress that duplicate path.
                let tablet_recent = self.last_tablet.is_some_and(|t| {
                    t.elapsed() < Duration::from_millis(self.config.tablet_mouse_suppression_ms)
                });
                if self.mouse_mode && !tablet_recent {
                    for event in ctx.input(|i| i.events.clone()) {
                        match event {
                            egui::Event::PointerButton {
                                pos,
                                button: egui::PointerButton::Primary,
                                pressed,
                                ..
                            } => {
                                self.mouse_down = pressed;
                                self.tracker.reset();
                                if pressed {
                                    self.paint(
                                        mapping.map([pos.x, pos.y], self.config.mouse_pressure),
                                        self.eraser,
                                    );
                                }
                            }
                            egui::Event::PointerMoved(pos) if self.mouse_down => self.paint(
                                mapping.map([pos.x, pos.y], self.config.mouse_pressure),
                                self.eraser,
                            ),
                            egui::Event::PointerGone => {
                                self.mouse_down = false;
                                self.tracker.reset();
                            }
                            _ => {}
                        }
                    }
                }
                if !ctx.input(|i| i.focused) {
                    self.tracker.reset();
                    self.mouse_down = false;
                }
                self.upload(ctx);
                if let Some(texture) = &self.texture {
                    ui.painter().image(
                        texture.id(),
                        rect,
                        egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
                        Color32::WHITE,
                    );
                }
                ui.interact(rect, ui.id().with("drawing-surface"), egui::Sense::hover())
                    .on_hover_cursor(egui::CursorIcon::Crosshair);
            });
        ctx.request_repaint_after(Duration::from_millis(self.config.canvas_repaint_ms));
    }
    fn controls(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(PAPER).inner_margin(24.0))
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("sideDuck").size(32.0).strong());
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(RichText::new("DRAWING LAB  /  01").size(10.0).color(MUTED));
                        });
                    });
                    ui.label(RichText::new("Your canvas, on another screen.").size(16.0).color(MUTED));
                    ui.add_space(12.0);
                    egui::Frame::NONE.fill(Color32::WHITE).corner_radius(10.0).inner_margin(14.0).show(ui, |ui| {
                        ui.label(RichText::new("CANVAS PREVIEW").size(10.0).color(MUTED));
                        if let Some(texture) = &self.texture {
                            let width = ui.available_width();
                            let size = Vec2::new(width, width * self.canvas.height() as f32 / self.canvas.width() as f32);
                            ui.add(egui::Image::new((texture.id(), size)));
                        }
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(format!("{} × {} px", self.canvas.width(), self.canvas.height())).small().color(MUTED));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                ui.label(RichText::new("Scratch document").small().color(MUTED));
                            });
                        });
                    });
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Draw").strong());
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.color_edit_button_srgb(&mut self.color);
                        });
                    });
                    ui.add(egui::Slider::new(&mut self.config.brush.radius, 1.0..=64.0).text("Radius"));
                    ui.add(egui::Slider::new(&mut self.config.brush.pressure_gamma, 0.2..=3.0).text("Pressure curve"));
                    ui.horizontal_wrapped(|ui| {
                        if ui.selectable_label(!self.eraser, "Pen").clicked() {
                            self.eraser = false;
                            self.tracker.reset();
                        }
                        if ui.selectable_label(self.eraser, "Eraser").clicked() {
                            self.eraser = true;
                            self.tracker.reset();
                        }
                        if ui.checkbox(&mut self.mouse_mode, "Allow mouse drawing").changed() {
                            self.tracker.reset();
                            self.mouse_down = false;
                        }
                    });
                    ui.separator();
                    ui.label(RichText::new("INPUT SIGNAL").size(10.0).color(MUTED));
                    if let Some(sample) = &self.latest {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new("● Tablet events received").color(Color32::from_rgb(40,117,73)));
                            ui.label(format!("{:?}", sample.phase));
                        });
                        ui.add(egui::ProgressBar::new(sample.pressure).text(format!("Pressure  {:.3}", sample.pressure)));
                        ui.label(format!("Tilt {:+.3}, {:+.3}   ·   Azimuth {:.2} rad", sample.tilt[0], sample.tilt[1], sample.azimuth));
                        ui.label(RichText::new(format!("{} samples · {} dropped · device {}", self.samples, self.dropped, sample.device_id)).small().color(MUTED));
                    } else {
                        ui.label("Waiting for a stylus…");
                        ui.label(RichText::new("Connect Sidecar and draw in the canvas window. Mouse mode uses a fixed pressure.").small().color(MUTED));
                    }
                    ui.add_space(6.0);
                    ui.horizontal_wrapped(|ui| {
                        if ui.button(if self.canvas_open { "Focus canvas" } else { "Open canvas" }).clicked() {
                            self.canvas_open = true;
                            ctx.send_viewport_cmd_to(egui::ViewportId::from_hash_of("canvas"), egui::ViewportCommand::Focus);
                        }
                        ui.checkbox(&mut self.fullscreen, "Canvas full screen");
                    });
                    ui.horizontal(|ui| {
                        if ui.button("Export PNG").clicked() { self.export(); }
                        if ui.button("Clear canvas").clicked() { self.show_clear_confirmation = true; }
                    });
                    if self.show_clear_confirmation {
                        ui.horizontal_wrapped(|ui| {
                            ui.label("Discard this drawing?");
                            if ui.button("Clear").clicked() {
                                self.canvas.clear();
                                self.tracker.reset();
                                self.show_clear_confirmation = false;
                            }
                            if ui.button("Keep").clicked() { self.show_clear_confirmation = false; }
                        });
                    }
                    ui.add_space(4.0);
                    ui.label(RichText::new(&self.status).small().color(MUTED));
                });
            });
    }
}
impl eframe::App for PrototypeApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let new_samples = self.source.drain_samples();
        for sample in &new_samples {
            self.samples += 1;
            self.latest = Some(*sample);
        }
        if self.source.dropped_samples() != self.dropped {
            self.tracker.reset();
            self.dropped = self.source.dropped_samples();
        }
        self.pending = new_samples;
        self.upload(ctx);
        self.controls(ctx);
        if self.canvas_open {
            ctx.show_viewport_immediate(
                egui::ViewportId::from_hash_of("canvas"),
                egui::ViewportBuilder::default()
                    .with_title(CANVAS_TITLE)
                    .with_inner_size([1024.0, 768.0])
                    .with_min_inner_size([320.0, 240.0]),
                |canvas_ctx, _| self.canvas_ui(canvas_ctx),
            );
        } else {
            self.pending.clear();
        }
        if self
            .smoke_duration
            .is_some_and(|duration| self.started.elapsed() >= duration)
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        ctx.request_repaint_after(Duration::from_millis(self.config.controls_repaint_ms));
    }
}

pub fn export_png(canvas: &Canvas, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    let mut encoder = png::Encoder::new(
        std::io::BufWriter::new(file),
        canvas.width(),
        canvas.height(),
    );
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header()?;
    writer.write_image_data(canvas.pixels())?;
    writer.finish()?;
    Ok(())
}

fn draw_demo(canvas: &mut Canvas, brush: BrushSettings) {
    for row in 0..4 {
        let mut last = None;
        for step in 0..240 {
            let t = step as f32 / 239.0;
            let p = Point {
                x: canvas.width() as f32 * (0.12 + 0.76 * t),
                y: canvas.height() as f32
                    * (0.24 + row as f32 * 0.16 + 0.055 * (t * std::f32::consts::TAU * 1.5).sin()),
                pressure: 0.08 + 0.92 * (t * std::f32::consts::PI).sin(),
            };
            canvas.stroke(
                last.unwrap_or(p),
                p,
                BrushSettings {
                    radius: brush.radius * (1.0 + row as f32 * 0.4),
                    ..brush
                },
                [[35, 73, 60], [91, 112, 82], [178, 124, 73], [48, 80, 98]][row],
                false,
            );
            last = Some(p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shipped_prototype_config_is_valid() {
        assert_eq!(PrototypeConfig::load(None).unwrap().canvas_width, 2048);
    }
    #[test]
    fn png_export_round_trips_pixels_and_never_overwrites() {
        let canvas = Canvas::new(16, 16).unwrap();
        let path = std::env::temp_dir().join(format!(
            "prototype-export-{}-{}.png",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        export_png(&canvas, &path).unwrap();
        assert!(export_png(&canvas, &path).is_err());
        let mut decoder = png::Decoder::new(std::fs::File::open(&path).unwrap())
            .read_info()
            .unwrap();
        let mut bytes = vec![0; decoder.output_buffer_size()];
        let frame = decoder.next_frame(&mut bytes).unwrap();
        assert_eq!(&bytes[..frame.buffer_size()], canvas.pixels());
        std::fs::remove_file(path).unwrap();
    }
}
