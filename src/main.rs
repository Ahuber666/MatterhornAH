//! Matterhorn AH — compact starter based on the TDD.
//! - egui/eframe UI with CPU/GPU fractal rendering
//! - Timeline UI with draggable keyframes and easing curves
//! - Orbit traps, extensible palettes, palette import/export (.ahpal)
//! - Save/load JSON or TOML (.mahproj) projects
//! - Export tiling for absurd resolutions + ffmpeg codecs (H264/ProRes/VP9/AV1)

use std::{
    cmp::Ordering,
    f32::consts::PI,
    fs,
    path::{Path, PathBuf},
    time::Instant,
};

use clap::{Parser, Subcommand};
use eframe::{egui, egui::Vec2, App};
use egui::{
    pos2, vec2,
    widgets::color_picker::{color_edit_button_srgba, Alpha},
    Color32, ColorImage, Id, Rect, Sense, Stroke, TextureHandle,
};
use image::{ImageBuffer, ImageError, Rgba};
use serde::{Deserialize, Serialize};

#[cfg(feature = "gpu")]
use gpu_renderer::GpuRenderer;

// ------------------------- Project Data -------------------------

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum RenderBackend {
    Cpu,
    #[cfg(feature = "gpu")]
    Gpu,
}

impl Default for RenderBackend {
    fn default() -> Self {
        RenderBackend::Cpu
    }
}

impl RenderBackend {
    fn label(&self) -> &'static str {
        match self {
            RenderBackend::Cpu => "CPU",
            #[cfg(feature = "gpu")]
            RenderBackend::Gpu => "GPU",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum FractalKind {
    Mandelbrot,
    Julia,
    BurningShip,
    Multibrot,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum OrbitTrapKind {
    Point,
    Circle,
    Cross,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OrbitTrap {
    enabled: bool,
    kind: OrbitTrapKind,
    radius: f32,
    softness: f32,
    color: [f32; 3],
    point: Complex,
}

impl Default for OrbitTrap {
    fn default() -> Self {
        Self {
            enabled: false,
            kind: OrbitTrapKind::Point,
            radius: 0.35,
            softness: 5.0,
            color: [1.0, 0.5, 0.3],
            point: Complex { re: 0.0, im: 0.0 },
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct PaletteStop {
    pos: f32,
    color: [f32; 3],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FractalParams {
    kind: FractalKind,
    max_iter: u32,
    escape_radius: f32,
    power: f32,
    c: Complex,         // used for Julia
    palette_phase: f32, // 0..1
    exposure: f32,
    gamma: f32,
    palette: Vec<PaletteStop>,
    orbit: OrbitTrap,
}

impl Default for FractalParams {
    fn default() -> Self {
        Self {
            kind: FractalKind::Mandelbrot,
            max_iter: 800,
            escape_radius: 4.0,
            power: 2.0,
            c: Complex {
                re: -0.8,
                im: 0.156,
            },
            palette_phase: 0.0,
            exposure: 1.0,
            gamma: 2.2,
            palette: default_palette(),
            orbit: OrbitTrap::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct Complex {
    re: f32,
    im: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Camera {
    center: Complex, // complex plane center
    scale: f32,      // pixels per unit (zoom)
    rotation: f32,   // radians
}

impl Default for Camera {
    fn default() -> Self {
        Self {
            center: Complex { re: -0.5, im: 0.0 },
            scale: 300.0,
            rotation: 0.0,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct RepeatingSpot {
    center: Complex,
    rotation: f32,
    start_scale: f32,
}

/// Location inside Seahorse Valley that exhibits near-perfect self similarity.
const SEAHORSE_REPEAT_SPOT: RepeatingSpot = RepeatingSpot {
    center: Complex {
        re: -0.743_643_9,
        im: 0.131_825_91,
    },
    rotation: 0.0,
    start_scale: 3_200.0,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Project {
    name: String,
    fractal: FractalParams,
    camera: Camera,
    anim: Animation,
    export: ExportSettings,
    render_backend: RenderBackend,
}

impl Default for Project {
    fn default() -> Self {
        Self {
            name: "Untitled".into(),
            fractal: Default::default(),
            camera: Default::default(),
            anim: Animation::default(),
            export: ExportSettings::default(),
            render_backend: RenderBackend::default(),
        }
    }
}

// ------------------------- Animation -------------------------

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Easing {
    Linear,
    EaseIn,
    EaseOut,
    EaseInOut,
    SmoothStep,
}

impl Default for Easing {
    fn default() -> Self {
        Easing::Linear
    }
}

impl Easing {
    fn label(&self) -> &'static str {
        match self {
            Easing::Linear => "Linear",
            Easing::EaseIn => "EaseIn",
            Easing::EaseOut => "EaseOut",
            Easing::EaseInOut => "EaseInOut",
            Easing::SmoothStep => "SmoothStep",
        }
    }

    fn apply(&self, t: f32) -> f32 {
        match self {
            Easing::Linear => t,
            Easing::EaseIn => t * t,
            Easing::EaseOut => 1.0 - (1.0 - t) * (1.0 - t),
            Easing::EaseInOut => {
                if t < 0.5 {
                    2.0 * t * t
                } else {
                    1.0 - (-2.0 * t + 2.0).powi(2) / 2.0
                }
            }
            Easing::SmoothStep => t * t * (3.0 - 2.0 * t),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Animation {
    fps: u32,
    duration: f32, // seconds
    playing: bool,
    #[serde(default)]
    looping: bool,
    t: f32, // current time
    kf_zoom: Keyframes<f32>,
    kf_palette: Keyframes<f32>,
    kf_center_x: Keyframes<f32>,
    kf_center_y: Keyframes<f32>,
    selection: Option<SelectedKey>,
    #[serde(default)]
    zoom_forever: Option<EndlessZoom>,
}

impl Default for Animation {
    fn default() -> Self {
        Self {
            fps: 30,
            duration: 5.0,
            playing: false,
            looping: false,
            t: 0.0,
            kf_zoom: Keyframes::default(),
            kf_palette: Keyframes::default(),
            kf_center_x: Keyframes::default(),
            kf_center_y: Keyframes::default(),
            selection: None,
            zoom_forever: None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct EndlessZoom {
    start_scale: f32,
    speed: f32,
    #[serde(default)]
    reverse: bool,
    #[serde(default)]
    lock_repeating_spot: bool,
}

impl Animation {
    fn advance(&mut self, dt: f32) {
        if !self.playing {
            return;
        }
        self.t += dt;
        if self.duration > 0.0 && self.t >= self.duration {
            if self.looping {
                self.t = self.t % self.duration;
            } else {
                self.t = self.duration;
                self.playing = false;
            }
        }
    }

    fn sample_zoom(&self, t: f32, default: f32) -> f32 {
        if let Some(zoom) = self.zoom_forever {
            return zoom.value_at(t);
        }
        self.kf_zoom.sample(t, default)
    }

    fn apply_endless_zoom_preset(&mut self, start_scale: f32) {
        self.zoom_forever = Some(EndlessZoom::with_defaults(start_scale));
        self.kf_zoom.keys.clear();
        self.playing = true;
        self.looping = true;
        self.t = 0.0;
    }

    fn resolve_times(&self, absolute_time: f32) -> (f32, f32) {
        if self.duration <= 0.0 {
            let zoom = if self.zoom_forever.is_some() {
                absolute_time
            } else {
                0.0
            };
            return (0.0, zoom);
        }
        let key_time = if self.looping {
            absolute_time % self.duration
        } else {
            absolute_time.min(self.duration)
        };
        let zoom_time = if self.zoom_forever.is_some() {
            absolute_time
        } else {
            key_time
        };
        (key_time, zoom_time)
    }

    fn current_times(&self) -> (f32, f32) {
        self.resolve_times(self.t)
    }

    fn timeline_time(&self) -> f32 {
        self.resolve_times(self.t).0
    }

    fn set_timeline_time(&mut self, timeline_time: f32) {
        if self.duration <= 0.0 {
            self.t = timeline_time.max(0.0);
        } else {
            self.t = timeline_time.clamp(0.0, self.duration);
        }
    }

    fn is_repeating_spot_locked(&self) -> bool {
        self.zoom_forever
            .map_or(false, |zoom| zoom.lock_repeating_spot)
    }
}

impl EndlessZoom {
    fn with_defaults(scale: f32) -> Self {
        Self {
            start_scale: scale.max(0.0001),
            speed: 0.9,
            reverse: false,
            lock_repeating_spot: false,
        }
    }

    fn value_at(self, t: f32) -> f32 {
        let clamped_speed = self.speed.clamp(0.5, 0.995);
        let factor = if self.reverse {
            1.0 / clamped_speed
        } else {
            clamped_speed
        };
        self.start_scale * factor.powf(t.max(0.0))
    }
}

fn enforce_repeating_spot(camera: &mut Camera) {
    camera.center = SEAHORSE_REPEAT_SPOT.center;
    camera.rotation = SEAHORSE_REPEAT_SPOT.rotation;
}

fn snap_camera_to_repeating_spot(camera: &mut Camera, zoom: &mut EndlessZoom) {
    enforce_repeating_spot(camera);
    camera.scale = SEAHORSE_REPEAT_SPOT.start_scale;
    zoom.start_scale = camera.scale.max(0.0001);
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum TrackKind {
    Zoom,
    Palette,
    CenterX,
    CenterY,
}

impl TrackKind {
    fn label(&self) -> &'static str {
        match self {
            TrackKind::Zoom => "Zoom",
            TrackKind::Palette => "Palette",
            TrackKind::CenterX => "Center X",
            TrackKind::CenterY => "Center Y",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SelectedKey {
    track: TrackKind,
    index: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Keyframe<T> {
    t: f32,
    v: T,
    easing: Easing,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct Keyframes<T> {
    keys: Vec<Keyframe<T>>,
}

impl<T: Copy + Interp> Keyframes<T> {
    fn sample(&self, t: f32, default: T) -> T {
        if self.keys.is_empty() {
            return default;
        }
        if self.keys.len() == 1 {
            return self.keys[0].v;
        }
        let mut prev = &self.keys[0];
        for k in &self.keys[1..] {
            if t <= k.t {
                let denom = (k.t - prev.t).max(1e-4);
                let mut u = ((t - prev.t) / denom).clamp(0.0, 1.0);
                u = prev.easing.apply(u);
                return T::lerp(prev.v, k.v, u);
            }
            prev = k;
        }
        prev.v
    }

    fn upsert(&mut self, t: f32, v: T) {
        if let Some(existing) = self.keys.iter_mut().find(|key| (key.t - t).abs() < 1e-4) {
            existing.v = v;
            return;
        }
        self.keys.push(Keyframe {
            t,
            v,
            easing: Easing::Linear,
        });
        self.keys.sort_by(|a, b| a.t.partial_cmp(&b.t).unwrap());
    }

    fn clamp_all(&mut self, duration: f32) {
        for k in &mut self.keys {
            k.t = k.t.clamp(0.0, duration);
        }
    }
}

trait Interp {
    fn lerp(a: Self, b: Self, u: f32) -> Self;
}
impl Interp for f32 {
    fn lerp(a: Self, b: Self, u: f32) -> Self {
        a + (b - a) * u
    }
}

// ------------------------- Export -------------------------

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum VideoCodec {
    H264,
    ProRes,
    Vp9,
    Av1,
}

impl Default for VideoCodec {
    fn default() -> Self {
        VideoCodec::H264
    }
}

impl VideoCodec {
    fn label(&self) -> &'static str {
        match self {
            VideoCodec::H264 => "H.264",
            VideoCodec::ProRes => "ProRes 422",
            VideoCodec::Vp9 => "VP9",
            VideoCodec::Av1 => "AV1",
        }
    }

    fn ffmpeg_args(&self, crf: u8) -> Vec<String> {
        match self {
            VideoCodec::H264 => vec![
                "-c:v".into(),
                "libx264".into(),
                "-pix_fmt".into(),
                "yuv420p".into(),
                "-crf".into(),
                crf.to_string(),
            ],
            VideoCodec::ProRes => vec![
                "-c:v".into(),
                "prores_ks".into(),
                "-profile:v".into(),
                "3".into(),
                "-pix_fmt".into(),
                "yuv422p10le".into(),
            ],
            VideoCodec::Vp9 => vec![
                "-c:v".into(),
                "libvpx-vp9".into(),
                "-b:v".into(),
                "0".into(),
                "-crf".into(),
                crf.to_string(),
            ],
            VideoCodec::Av1 => vec![
                "-c:v".into(),
                "libaom-av1".into(),
                "-b:v".into(),
                "0".into(),
                "-crf".into(),
                crf.to_string(),
            ],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ExportSettings {
    width: u32,
    height: u32,
    fps: u32,
    duration: f32,
    crf: u8,
    codec: VideoCodec,
    tile_size: u32,
    out_path: PathBuf,
}

impl Default for ExportSettings {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            fps: 30,
            duration: 5.0,
            crf: 20,
            codec: VideoCodec::default(),
            tile_size: 2048,
            out_path: PathBuf::from("output.mp4"),
        }
    }
}

// ------------------------- CLI -------------------------

#[derive(Parser)]
#[command(name = "Matterhorn AH")]
#[command(about = "Real-time fractal studio (starter)")]
struct Args {
    /// Optional project file to load (.json / .mahproj)
    #[arg(short, long)]
    project: Option<PathBuf>,

    /// Headless export (no UI)
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    Export {
        project: PathBuf,
        out: Option<PathBuf>,
    },
}

// ------------------------- App State -------------------------

struct MatterhornApp {
    proj: Project,
    tex: Option<TextureHandle>,
    last_update: Instant,
    #[cfg(feature = "gpu")]
    gpu: Option<GpuRenderer>,
}

impl App for MatterhornApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let dt = self.last_update.elapsed().as_secs_f32();
        self.last_update = Instant::now();
        self.proj.anim.advance(dt);
        if self.proj.anim.playing {
            ctx.request_repaint();
        }

        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.heading("Matterhorn AH");
                ui.separator();
                if ui
                    .button(if self.proj.anim.playing {
                        "Pause"
                    } else {
                        "Play"
                    })
                    .clicked()
                {
                    self.proj.anim.playing = !self.proj.anim.playing;
                }
                if ui.button("Restart").clicked() {
                    self.proj.anim.t = 0.0;
                }
                ui.label(format!("t = {:.2}s", self.proj.anim.t));
                ui.separator();
                if ui.button("Save JSON").clicked() {
                    save_project_dialog_json(&self.proj);
                }
                if ui.button("Save .mahproj").clicked() {
                    save_project_dialog_toml(&self.proj);
                }
                if ui.button("Load Project").clicked() {
                    if let Some(p) = open_project_dialog() {
                        self.proj = p;
                    }
                }
                if ui.button("Export Video").clicked() {
                    if let Err(e) = export_video_blocking(
                        &self.proj,
                        #[cfg(feature = "gpu")]
                        self.gpu.as_mut(),
                    ) {
                        eprintln!("Export error: {e}");
                    }
                }
                ui.separator();
                ui.label("Backend:");
                ui.selectable_value(
                    &mut self.proj.render_backend,
                    RenderBackend::Cpu,
                    RenderBackend::Cpu.label(),
                );
                #[cfg(feature = "gpu")]
                {
                    ui.selectable_value(
                        &mut self.proj.render_backend,
                        RenderBackend::Gpu,
                        RenderBackend::Gpu.label(),
                    );
                    if matches!(self.proj.render_backend, RenderBackend::Gpu) && self.gpu.is_none()
                    {
                        match GpuRenderer::new() {
                            Ok(renderer) => self.gpu = Some(renderer),
                            Err(err) => {
                                eprintln!("GPU init failed: {err}");
                                self.proj.render_backend = RenderBackend::Cpu;
                            }
                        }
                    }
                }
            });
        });

        egui::SidePanel::left("left")
            .default_width(320.0)
            .show(ctx, |ui| {
                ui.heading("Fractal");
                ui.separator();
                ui.vertical(|ui| {
                    ui.label("Kind");
                    for kind in [
                        FractalKind::Mandelbrot,
                        FractalKind::Julia,
                        FractalKind::BurningShip,
                        FractalKind::Multibrot,
                    ] {
                        ui.selectable_value(
                            &mut self.proj.fractal.kind,
                            kind,
                            format!("{:?}", kind),
                        );
                    }
                });
                ui.add(egui::Slider::new(&mut self.proj.fractal.power, 2.0..=12.0).text("Power"));
                ui.add(
                    egui::Slider::new(&mut self.proj.fractal.max_iter, 50..=20_000)
                        .text("Max Iter"),
                );
                ui.add(
                    egui::Slider::new(&mut self.proj.fractal.escape_radius, 2.0..=128.0)
                        .text("Escape R"),
                );
                if matches!(self.proj.fractal.kind, FractalKind::Julia) {
                    ui.horizontal(|ui| {
                        ui.label("Julia c Re");
                        ui.add(egui::DragValue::new(&mut self.proj.fractal.c.re).speed(0.01));
                    });
                    ui.horizontal(|ui| {
                        ui.label("Julia c Im");
                        ui.add(egui::DragValue::new(&mut self.proj.fractal.c.im).speed(0.01));
                    });
                }
                ui.separator();
                ui.heading("Camera");
                ui.add(
                    egui::Slider::new(&mut self.proj.camera.center.re, -2.5..=2.5).text("Center X"),
                );
                ui.add(
                    egui::Slider::new(&mut self.proj.camera.center.im, -2.0..=2.0).text("Center Y"),
                );
                ui.add(
                    egui::Slider::new(&mut self.proj.camera.scale, 50.0..=8000.0)
                        .text("Scale (zoom)"),
                );
                ui.add(
                    egui::Slider::new(&mut self.proj.camera.rotation, -PI..=PI).text("Rotation"),
                );
                ui.separator();
                ui.heading("Color & FX");
                ui.add(
                    egui::Slider::new(&mut self.proj.fractal.palette_phase, 0.0..=1.0)
                        .text("Palette phase"),
                );
                ui.add(
                    egui::Slider::new(&mut self.proj.fractal.exposure, 0.1..=6.0).text("Exposure"),
                );
                ui.add(egui::Slider::new(&mut self.proj.fractal.gamma, 0.5..=4.0).text("Gamma"));
                orbit_trap_ui(ui, &mut self.proj.fractal.orbit);
                palette_editor_ui(ui, &mut self.proj.fractal.palette);
                ui.separator();
                export_panel_ui(ui, &mut self.proj.export);
            });

        egui::CentralPanel::default().show(ctx, |ui| {
            let (timeline_t, zoom_t) = self.proj.anim.current_times();

            // Sample animated parameters
            let base_scale = self.proj.camera.scale;
            self.proj.camera.scale = self.proj.anim.sample_zoom(zoom_t, base_scale);
            self.proj.fractal.palette_phase = self
                .proj
                .anim
                .kf_palette
                .sample(timeline_t, self.proj.fractal.palette_phase);
            self.proj.camera.center.re = self
                .proj
                .anim
                .kf_center_x
                .sample(timeline_t, self.proj.camera.center.re);
            self.proj.camera.center.im = self
                .proj
                .anim
                .kf_center_y
                .sample(timeline_t, self.proj.camera.center.im);
            if self.proj.anim.is_repeating_spot_locked() {
                enforce_repeating_spot(&mut self.proj.camera);
            }

            let avail = ui.available_size();
            let size = (avail.x.max(128.0) as u32, avail.y.max(128.0) as u32);
            let pixels = render_image(
                size,
                &self.proj.fractal,
                &self.proj.camera,
                self.proj.render_backend,
                0,
                #[cfg(feature = "gpu")]
                self.gpu.as_mut(),
            );
            let color_image =
                ColorImage::from_rgba_unmultiplied([size.0 as usize, size.1 as usize], &pixels);
            let tex = self.tex.get_or_insert_with(|| {
                ui.ctx()
                    .load_texture("preview", color_image.clone(), egui::TextureOptions::LINEAR)
            });
            tex.set(color_image, egui::TextureOptions::LINEAR);
            ui.image((tex.id(), Vec2::new(size.0 as f32, size.1 as f32)));
        });

        egui::TopBottomPanel::bottom("timeline")
            .default_height(200.0)
            .show(ctx, |ui| {
                timeline_ui(
                    ui,
                    &mut self.proj.anim,
                    &mut self.proj.camera,
                    &self.proj.fractal,
                );
            });
    }
}

fn orbit_trap_ui(ui: &mut egui::Ui, orbit: &mut OrbitTrap) {
    ui.collapsing("Orbit Trap", |ui| {
        ui.checkbox(&mut orbit.enabled, "Enabled");
        ui.horizontal(|ui| {
            ui.label("Kind");
            ui.selectable_value(&mut orbit.kind, OrbitTrapKind::Point, "Point");
            ui.selectable_value(&mut orbit.kind, OrbitTrapKind::Circle, "Circle");
            ui.selectable_value(&mut orbit.kind, OrbitTrapKind::Cross, "Cross");
        });
        ui.add(egui::Slider::new(&mut orbit.radius, 0.05..=2.0).text("Radius"));
        ui.add(egui::Slider::new(&mut orbit.softness, 0.5..=20.0).text("Softness"));
        ui.horizontal(|ui| {
            ui.label("Point Re");
            ui.add(egui::DragValue::new(&mut orbit.point.re).speed(0.01));
        });
        ui.horizontal(|ui| {
            ui.label("Point Im");
            ui.add(egui::DragValue::new(&mut orbit.point.im).speed(0.01));
        });
        let mut color = Color32::from_rgb(
            (orbit.color[0] * 255.0) as u8,
            (orbit.color[1] * 255.0) as u8,
            (orbit.color[2] * 255.0) as u8,
        );
        if color_edit_button_srgba(ui, &mut color, Alpha::Opaque).changed() {
            orbit.color = [
                color.r() as f32 / 255.0,
                color.g() as f32 / 255.0,
                color.b() as f32 / 255.0,
            ];
        }
    });
}

fn palette_editor_ui(ui: &mut egui::Ui, palette: &mut Vec<PaletteStop>) {
    ui.collapsing("Palette", |ui| {
        if palette.is_empty() {
            *palette = default_palette();
        }
        ui.horizontal(|ui| {
            ui.menu_button("Flashy presets", |menu| {
                for preset in palette_presets() {
                    if menu.button(preset.name).clicked() {
                        apply_palette_preset(palette, preset);
                        menu.close_menu();
                    }
                }
            });
            if ui.button("Flip colors").clicked() {
                flip_palette(palette);
            }
            if ui.button("Cycle colors").clicked() {
                cycle_palette_colors(palette);
            }
        });
        ui.separator();
        let mut remove_idx: Option<usize> = None;
        for (idx, stop) in palette.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                ui.label(format!("Stop {}", idx + 1));
                ui.add(egui::Slider::new(&mut stop.pos, 0.0..=1.0).text("Pos"));
                let mut color = Color32::from_rgb(
                    (stop.color[0] * 255.0) as u8,
                    (stop.color[1] * 255.0) as u8,
                    (stop.color[2] * 255.0) as u8,
                );
                if color_edit_button_srgba(ui, &mut color, Alpha::Opaque).changed() {
                    stop.color = [
                        color.r() as f32 / 255.0,
                        color.g() as f32 / 255.0,
                        color.b() as f32 / 255.0,
                    ];
                }
                if ui.button("✕").clicked() {
                    remove_idx = Some(idx);
                }
            });
        }
        if let Some(idx) = remove_idx {
            if palette.len() > 2 {
                palette.remove(idx);
            }
        }
        if ui.button("Add stop").clicked() {
            palette.push(PaletteStop {
                pos: 0.5,
                color: [1.0, 1.0, 1.0],
            });
        }
        ui.horizontal(|ui| {
            if ui.button("Export .ahpal").clicked() {
                save_palette_dialog(palette);
            }
            if ui.button("Import .ahpal").clicked() {
                if let Some(new_pal) = load_palette_dialog() {
                    *palette = new_pal;
                }
            }
        });
    });
}

fn export_panel_ui(ui: &mut egui::Ui, export: &mut ExportSettings) {
    ui.collapsing("Export", |ui| {
        ui.add(
            egui::DragValue::new(&mut export.width)
                .speed(16)
                .suffix(" px"),
        );
        ui.add(
            egui::DragValue::new(&mut export.height)
                .speed(16)
                .suffix(" px"),
        );
        ui.add(egui::Slider::new(&mut export.duration, 1.0..=120.0).text("Duration (s)"));
        ui.add(egui::Slider::new(&mut export.fps, 12..=120).text("FPS"));
        ui.add(egui::Slider::new(&mut export.crf, 0..=40).text("Quality/CRF"));
        ui.add(
            egui::DragValue::new(&mut export.tile_size)
                .clamp_range(512..=8192)
                .suffix(" tile"),
        );
        ui.horizontal(|ui| {
            ui.label("Codec");
            for codec in [
                VideoCodec::H264,
                VideoCodec::ProRes,
                VideoCodec::Vp9,
                VideoCodec::Av1,
            ] {
                ui.selectable_value(&mut export.codec, codec, codec.label());
            }
        });
        if ui.button("Pick output").clicked() {
            if let Some(path) = rfd::FileDialog::new()
                .add_filter("Video", &["mp4", "mov", "webm", "mkv"])
                .save_file()
            {
                export.out_path = path;
            }
        }
        ui.label(format!("Output: {}", export.out_path.display()));
    });
}

fn timeline_ui(
    ui: &mut egui::Ui,
    anim: &mut Animation,
    camera: &mut Camera,
    fractal: &FractalParams,
) {
    let initial_cursor = anim.timeline_time();
    let mut timeline_cursor = initial_cursor;
    ui.horizontal(|ui| {
        ui.add(egui::Slider::new(&mut anim.duration, 0.5..=120.0).text("Duration"));
        anim.kf_zoom.clamp_all(anim.duration);
        anim.kf_palette.clamp_all(anim.duration);
        anim.kf_center_x.clamp_all(anim.duration);
        anim.kf_center_y.clamp_all(anim.duration);
        ui.add(egui::Slider::new(&mut anim.fps, 12..=240).text("Preview FPS"));
        ui.checkbox(&mut anim.looping, "Loop playback");
        if ui.button("Add key @t").clicked() {
            anim.kf_zoom.upsert(timeline_cursor, camera.scale);
            anim.kf_palette
                .upsert(timeline_cursor, fractal.palette_phase);
            anim.kf_center_x.upsert(timeline_cursor, camera.center.re);
            anim.kf_center_y.upsert(timeline_cursor, camera.center.im);
        }
    });

    ui.horizontal(|ui| {
        if ui.button("Preset: Endless Zoom").clicked() {
            anim.apply_endless_zoom_preset(camera.scale);
            timeline_cursor = 0.0;
        }
        if let Some(zoom) = anim.zoom_forever.as_mut() {
            ui.label("Speed");
            ui.add(egui::Slider::new(&mut zoom.speed, 0.5..=0.995).text("scale/sec"));
            ui.checkbox(&mut zoom.reverse, "Reverse direction");
            let lock_resp = ui
                .checkbox(&mut zoom.lock_repeating_spot, "Auto-place repeating spot")
                .on_hover_text("Snap to a self-similar Seahorse Valley minibrot so the zoom keeps repeating.");
            if lock_resp.changed() && zoom.lock_repeating_spot {
                snap_camera_to_repeating_spot(camera, zoom);
                timeline_cursor = 0.0;
            }
            if zoom.lock_repeating_spot
                && ui
                    .button("Re-center to repeating spot")
                    .on_hover_text("Move the camera back to the repeating minibrot and reset the zoom start scale.")
                    .clicked()
            {
                snap_camera_to_repeating_spot(camera, zoom);
                timeline_cursor = 0.0;
            }
            if ui.button("Re-base").on_hover_text("Use current zoom as the new starting scale").clicked() {
                zoom.start_scale = camera.scale.max(0.0001);
                timeline_cursor = 0.0;
            }
            if ui.button("Disable").clicked() {
                anim.zoom_forever = None;
            }
        }
    });
    if anim.zoom_forever.is_some() {
        ui.small("Endless zoom keeps shrinking scale beyond the timeline duration.");
    }

    track_timeline_row(
        ui,
        TrackKind::Zoom,
        "Zoom",
        camera.scale,
        anim.duration,
        &mut timeline_cursor,
        &mut anim.selection,
        &mut anim.kf_zoom,
    );
    track_timeline_row(
        ui,
        TrackKind::Palette,
        "Palette",
        fractal.palette_phase,
        anim.duration,
        &mut timeline_cursor,
        &mut anim.selection,
        &mut anim.kf_palette,
    );
    track_timeline_row(
        ui,
        TrackKind::CenterX,
        "Center X",
        camera.center.re,
        anim.duration,
        &mut timeline_cursor,
        &mut anim.selection,
        &mut anim.kf_center_x,
    );
    track_timeline_row(
        ui,
        TrackKind::CenterY,
        "Center Y",
        camera.center.im,
        anim.duration,
        &mut timeline_cursor,
        &mut anim.selection,
        &mut anim.kf_center_y,
    );
    if (timeline_cursor - initial_cursor).abs() > f32::EPSILON {
        anim.set_timeline_time(timeline_cursor);
    }

    if let Some(sel) = anim.selection.clone() {
        let keys = match sel.track {
            TrackKind::Zoom => &mut anim.kf_zoom,
            TrackKind::Palette => &mut anim.kf_palette,
            TrackKind::CenterX => &mut anim.kf_center_x,
            TrackKind::CenterY => &mut anim.kf_center_y,
        };
        if let Some(key) = keys.keys.get_mut(sel.index) {
            ui.separator();
            ui.label(format!("Editing key {:?} at {:.2}s", sel.track, key.t));
            egui::ComboBox::from_label("Easing")
                .selected_text(key.easing.label())
                .show_ui(ui, |ui| {
                    for easing in [
                        Easing::Linear,
                        Easing::EaseIn,
                        Easing::EaseOut,
                        Easing::EaseInOut,
                        Easing::SmoothStep,
                    ] {
                        ui.selectable_value(&mut key.easing, easing, easing.label());
                    }
                });
            if ui.button("Delete key").clicked() {
                keys.keys.remove(sel.index);
                anim.selection = None;
            }
        } else {
            anim.selection = None;
        }
    }
}

fn track_timeline_row(
    ui: &mut egui::Ui,
    track: TrackKind,
    label: &str,
    current_value: f32,
    duration: f32,
    time: &mut f32,
    selection: &mut Option<SelectedKey>,
    keys: &mut Keyframes<f32>,
) {
    let height = 36.0;
    ui.label(label);
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 4.0, Color32::from_gray(28));

    // Scrub timeline if dragged/clicked
    if (response.dragged() || response.clicked()) && duration > 0.0 {
        if let Some(pos) = response.interact_pointer_pos() {
            let rel = ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
            *time = rel * duration;
        }
    }

    // Draw scrubber
    let scrub_x = rect.left() + rect.width() * (*time / duration.max(0.001));
    painter.line_segment(
        [pos2(scrub_x, rect.top()), pos2(scrub_x, rect.bottom())],
        Stroke::new(1.5, Color32::LIGHT_BLUE),
    );

    let mut remove_idx = None;
    let current_selection = selection.clone();
    for (idx, key) in keys.keys.iter_mut().enumerate() {
        let x = rect.left() + rect.width() * (key.t / duration.max(0.001));
        let key_rect = Rect::from_center_size(pos2(x, rect.center().y), vec2(10.0, height - 8.0));
        let id = Id::new((track as u8, idx as u32));
        let resp = ui.interact(key_rect, id, Sense::click_and_drag());
        let color = if current_selection
            .as_ref()
            .map_or(false, |sel| sel.track == track && sel.index == idx)
        {
            Color32::from_rgb(255, 170, 70)
        } else {
            Color32::from_rgb(120, 200, 255)
        };
        painter.rect_filled(key_rect, 2.0, color);
        painter.text(
            key_rect.center_top() + vec2(0.0, -10.0),
            egui::Align2::CENTER_TOP,
            key.easing.label(),
            egui::FontId::proportional(10.0),
            Color32::GRAY,
        );

        if resp.dragged() {
            if let Some(pos) = resp.interact_pointer_pos() {
                let rel = ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
                key.t = rel * duration;
            }
        }
        if resp.clicked() {
            *selection = Some(SelectedKey { track, index: idx });
        }
        if resp.double_clicked() {
            key.easing = match key.easing {
                Easing::Linear => Easing::EaseIn,
                Easing::EaseIn => Easing::EaseOut,
                Easing::EaseOut => Easing::EaseInOut,
                Easing::EaseInOut => Easing::SmoothStep,
                Easing::SmoothStep => Easing::Linear,
            };
        }
        if resp.secondary_clicked() {
            remove_idx = Some(idx);
        }
    }
    if let Some(idx) = remove_idx {
        keys.keys.remove(idx);
        if selection
            .as_ref()
            .map_or(false, |sel| sel.track == track && sel.index == idx)
        {
            *selection = None;
        }
    }

    if response.double_clicked() {
        if let Some(pos) = response.interact_pointer_pos() {
            let rel = ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
            keys.upsert(rel * duration, current_value);
        }
    }
}

fn default_palette() -> Vec<PaletteStop> {
    vec![
        PaletteStop {
            pos: 0.0,
            color: [0.0, 0.03, 0.39],
        },
        PaletteStop {
            pos: 0.16,
            color: [0.13, 0.42, 0.8],
        },
        PaletteStop {
            pos: 0.42,
            color: [0.93, 1.0, 1.0],
        },
        PaletteStop {
            pos: 0.6425,
            color: [1.0, 0.67, 0.0],
        },
        PaletteStop {
            pos: 0.8575,
            color: [0.0, 0.01, 0.0],
        },
        PaletteStop {
            pos: 1.0,
            color: [0.0, 0.03, 0.39],
        },
    ]
}

#[derive(Clone, Copy)]
struct PaletteStopDef {
    pos: f32,
    color: [f32; 3],
}

struct PalettePreset {
    name: &'static str,
    stops: &'static [PaletteStopDef],
}

fn palette_presets() -> &'static [PalettePreset] {
    const NEON_PULSE: &[PaletteStopDef] = &[
        PaletteStopDef {
            pos: 0.0,
            color: [1.0, 0.0, 0.6],
        },
        PaletteStopDef {
            pos: 0.2,
            color: [0.0, 1.0, 0.9],
        },
        PaletteStopDef {
            pos: 0.4,
            color: [1.0, 1.0, 0.0],
        },
        PaletteStopDef {
            pos: 0.6,
            color: [0.0, 0.8, 0.2],
        },
        PaletteStopDef {
            pos: 0.8,
            color: [0.0, 0.2, 1.0],
        },
        PaletteStopDef {
            pos: 1.0,
            color: [1.0, 0.0, 0.0],
        },
    ];
    const CYBER_SUNSET: &[PaletteStopDef] = &[
        PaletteStopDef {
            pos: 0.0,
            color: [0.05, 0.0, 0.2],
        },
        PaletteStopDef {
            pos: 0.15,
            color: [0.4, 0.0, 0.5],
        },
        PaletteStopDef {
            pos: 0.35,
            color: [1.0, 0.0, 0.4],
        },
        PaletteStopDef {
            pos: 0.6,
            color: [1.0, 0.6, 0.0],
        },
        PaletteStopDef {
            pos: 0.85,
            color: [1.0, 0.95, 0.5],
        },
        PaletteStopDef {
            pos: 1.0,
            color: [0.05, 0.0, 0.2],
        },
    ];
    const LASER_GRID: &[PaletteStopDef] = &[
        PaletteStopDef {
            pos: 0.0,
            color: [0.0, 0.0, 0.0],
        },
        PaletteStopDef {
            pos: 0.2,
            color: [0.0, 1.0, 0.0],
        },
        PaletteStopDef {
            pos: 0.4,
            color: [1.0, 0.0, 0.0],
        },
        PaletteStopDef {
            pos: 0.6,
            color: [0.0, 0.0, 1.0],
        },
        PaletteStopDef {
            pos: 0.8,
            color: [1.0, 1.0, 1.0],
        },
        PaletteStopDef {
            pos: 1.0,
            color: [1.0, 0.0, 1.0],
        },
    ];
    const ULTRAVIOLET: &[PaletteStopDef] = &[
        PaletteStopDef {
            pos: 0.0,
            color: [0.2, 0.0, 0.4],
        },
        PaletteStopDef {
            pos: 0.25,
            color: [0.5, 0.0, 0.9],
        },
        PaletteStopDef {
            pos: 0.5,
            color: [0.1, 0.6, 1.0],
        },
        PaletteStopDef {
            pos: 0.75,
            color: [0.9, 0.6, 0.0],
        },
        PaletteStopDef {
            pos: 1.0,
            color: [0.1, 0.0, 0.2],
        },
    ];
    &[
        PalettePreset {
            name: "Neon Pulse",
            stops: NEON_PULSE,
        },
        PalettePreset {
            name: "Cyber Sunset",
            stops: CYBER_SUNSET,
        },
        PalettePreset {
            name: "Laser Grid",
            stops: LASER_GRID,
        },
        PalettePreset {
            name: "Ultraviolet",
            stops: ULTRAVIOLET,
        },
    ]
}

fn apply_palette_preset(palette: &mut Vec<PaletteStop>, preset: &PalettePreset) {
    palette.clear();
    palette.extend(preset.stops.iter().map(|stop| PaletteStop {
        pos: stop.pos,
        color: stop.color,
    }));
    palette.sort_by(|a, b| a.pos.partial_cmp(&b.pos).unwrap_or(Ordering::Equal));
}

fn flip_palette(palette: &mut Vec<PaletteStop>) {
    for stop in palette.iter_mut() {
        stop.pos = 1.0 - stop.pos;
    }
    palette.sort_by(|a, b| a.pos.partial_cmp(&b.pos).unwrap_or(Ordering::Equal));
}

fn cycle_palette_colors(palette: &mut Vec<PaletteStop>) {
    if palette.len() > 1 {
        let mut colors: Vec<[f32; 3]> = palette.iter().map(|stop| stop.color).collect();
        colors.rotate_right(1);
        for (stop, color) in palette.iter_mut().zip(colors.into_iter()) {
            stop.color = color;
        }
    }
}

fn normalized_stops(stops: &[PaletteStop]) -> Vec<PaletteStop> {
    if stops.is_empty() {
        return default_palette();
    }
    let mut sorted = stops.to_vec();
    sorted.sort_by(|a, b| a.pos.partial_cmp(&b.pos).unwrap_or(Ordering::Equal));
    let first_color = sorted.first().unwrap().color;
    if sorted.first().unwrap().pos > 0.0 {
        sorted.insert(
            0,
            PaletteStop {
                pos: 0.0,
                color: first_color,
            },
        );
    }
    let last_color = sorted.last().unwrap().color;
    if (sorted.last().unwrap().pos - 1.0).abs() > f32::EPSILON {
        sorted.push(PaletteStop {
            pos: 1.0,
            color: last_color,
        });
    }
    sorted
}

fn build_palette(params: &FractalParams, size: usize) -> Vec<[u8; 3]> {
    let stops = normalized_stops(&params.palette);
    let mut lut = Vec::with_capacity(size);
    for i in 0..size {
        let mut t = i as f32 / (size as f32 - 1.0);
        t = (t + params.palette_phase).fract();
        let mut prev = stops.first().unwrap();
        let mut color = prev.color;
        for stop in stops.iter().skip(1) {
            if t <= stop.pos {
                let span = (stop.pos - prev.pos).max(1e-4);
                let u = ((t - prev.pos) / span).clamp(0.0, 1.0);
                color = [
                    Interp::lerp(prev.color[0], stop.color[0], u),
                    Interp::lerp(prev.color[1], stop.color[1], u),
                    Interp::lerp(prev.color[2], stop.color[2], u),
                ];
                break;
            }
            prev = stop;
        }
        lut.push([
            (color[0].clamp(0.0, 1.0) * 255.0) as u8,
            (color[1].clamp(0.0, 1.0) * 255.0) as u8,
            (color[2].clamp(0.0, 1.0) * 255.0) as u8,
        ]);
    }
    lut
}

#[derive(Clone, Copy)]
struct TileInfo {
    full_w: u32,
    full_h: u32,
    offset_x: u32,
    offset_y: u32,
    tile_w: u32,
    tile_h: u32,
}

impl TileInfo {
    fn full(width: u32, height: u32) -> Self {
        Self {
            full_w: width,
            full_h: height,
            offset_x: 0,
            offset_y: 0,
            tile_w: width,
            tile_h: height,
        }
    }
}

fn tile_iterator(width: u32, height: u32, mut tile: u32) -> Vec<TileInfo> {
    if tile == 0 {
        if width <= 8192 && height <= 8192 {
            return vec![TileInfo::full(width, height)];
        }
        tile = 4096;
    }
    if width <= tile && height <= tile {
        return vec![TileInfo::full(width, height)];
    }
    let mut tiles = Vec::new();
    let stride = tile.max(256);
    let mut y = 0;
    while y < height {
        let mut x = 0;
        while x < width {
            let w = stride.min(width - x);
            let h = stride.min(height - y);
            tiles.push(TileInfo {
                full_w: width,
                full_h: height,
                offset_x: x,
                offset_y: y,
                tile_w: w,
                tile_h: h,
            });
            x += stride;
        }
        y += stride;
    }
    tiles
}

fn render_image(
    size: (u32, u32),
    params: &FractalParams,
    cam: &Camera,
    backend: RenderBackend,
    tile_override: u32,
    #[cfg(feature = "gpu")] gpu: Option<&mut GpuRenderer>,
) -> Vec<u8> {
    let palette = build_palette(params, 2048);
    let tiles = tile_iterator(size.0, size.1, tile_override);
    let mut frame = vec![0u8; (size.0 * size.1 * 4) as usize];
    #[cfg(feature = "gpu")]
    let mut gpu = gpu;

    for tile in tiles {
        let tile_pixels = render_tile(
            &tile,
            params,
            cam,
            backend,
            &palette,
            #[cfg(feature = "gpu")]
            gpu.as_deref_mut(),
        );
        blit_tile(&mut frame, size.0, &tile, &tile_pixels);
    }

    frame
}

fn blit_tile(target: &mut [u8], full_width: u32, tile: &TileInfo, tile_pixels: &[u8]) {
    for ty in 0..tile.tile_h {
        let dst_y = tile.offset_y + ty;
        let dst_offset = ((dst_y * full_width + tile.offset_x) * 4) as usize;
        let src_offset = ((ty * tile.tile_w) * 4) as usize;
        let len = (tile.tile_w * 4) as usize;
        target[dst_offset..dst_offset + len]
            .copy_from_slice(&tile_pixels[src_offset..src_offset + len]);
    }
}

fn render_tile(
    tile: &TileInfo,
    params: &FractalParams,
    cam: &Camera,
    backend: RenderBackend,
    palette: &[[u8; 3]],
    #[cfg(feature = "gpu")] gpu: Option<&mut GpuRenderer>,
) -> Vec<u8> {
    match backend {
        RenderBackend::Cpu => render_fractal_cpu(tile, params, cam, palette),
        #[cfg(feature = "gpu")]
        RenderBackend::Gpu => {
            if let Some(renderer) = gpu {
                match renderer.render(tile, params, cam, palette) {
                    Ok(data) => data,
                    Err(err) => {
                        eprintln!("GPU render failed, falling back to CPU: {err}");
                        render_fractal_cpu(tile, params, cam, palette)
                    }
                }
            } else {
                render_fractal_cpu(tile, params, cam, palette)
            }
        }
    }
}

fn render_fractal_cpu(
    tile: &TileInfo,
    p: &FractalParams,
    cam: &Camera,
    palette: &[[u8; 3]],
) -> Vec<u8> {
    let mut buf: ImageBuffer<Rgba<u8>, Vec<u8>> = ImageBuffer::new(tile.tile_w, tile.tile_h);
    let cosr = cam.rotation.cos();
    let sinr = cam.rotation.sin();
    let er2 = p.escape_radius * p.escape_radius;

    for (y, row) in buf.enumerate_rows_mut() {
        let global_y = tile.offset_y + y;
        let v = global_y as f32 - (tile.full_h as f32) / 2.0;
        for (x, _, px) in row {
            let global_x = tile.offset_x + x;
            let u = global_x as f32 - (tile.full_w as f32) / 2.0;
            let rx = (u * cosr - v * sinr) / cam.scale + cam.center.re;
            let ry = (u * sinr + v * cosr) / cam.scale + cam.center.im;

            let (mut zx, mut zy) = match p.kind {
                FractalKind::Julia => (rx, ry),
                _ => (0.0, 0.0),
            };
            let (cx, cy) = match p.kind {
                FractalKind::Julia => (p.c.re, p.c.im),
                _ => (rx, ry),
            };

            let mut i = 0u32;
            let mut smooth = 0.0f32;
            let mut trap_min = f32::MAX;
            while i < p.max_iter {
                let mut x2 = zx * zx;
                let mut y2 = zy * zy;
                if x2 + y2 > er2 {
                    break;
                }

                match p.kind {
                    FractalKind::Mandelbrot | FractalKind::Julia => {
                        let new_x = x2 - y2 + cx;
                        let new_y = 2.0 * zx * zy + cy;
                        zx = new_x;
                        zy = new_y;
                    }
                    FractalKind::BurningShip => {
                        let new_x = x2 - y2 + cx;
                        let new_y = 2.0 * zx.abs() * zy.abs() + cy;
                        zx = new_x.abs();
                        zy = new_y.abs();
                    }
                    FractalKind::Multibrot => {
                        let r = (x2 + y2).sqrt();
                        let theta = zy.atan2(zx);
                        let r_p = r.powf(p.power);
                        let th_p = theta * p.power;
                        zx = r_p * th_p.cos() + cx;
                        zy = r_p * th_p.sin() + cy;
                    }
                }

                x2 = zx * zx;
                y2 = zy * zy;
                if p.orbit.enabled {
                    let dist = match p.orbit.kind {
                        OrbitTrapKind::Point => {
                            (zx - p.orbit.point.re).hypot(zy - p.orbit.point.im)
                        }
                        OrbitTrapKind::Circle => ((x2 + y2).sqrt() - p.orbit.radius).abs(),
                        OrbitTrapKind::Cross => (zx - p.orbit.point.re)
                            .abs()
                            .min((zy - p.orbit.point.im).abs()),
                    };
                    trap_min = trap_min.min(dist);
                }

                i += 1;
            }

            if i < p.max_iter {
                let r = (zx * zx + zy * zy).sqrt().max(1e-20);
                let mu = (i as f32) + 1.0 - (r.ln() / 2.0f32.ln()).ln() / (2.0f32.ln());
                smooth = mu / p.max_iter as f32;
            }

            let col = sample_palette(palette, smooth.fract());
            let mut r = col[0] as f32 / 255.0;
            let mut g = col[1] as f32 / 255.0;
            let mut b = col[2] as f32 / 255.0;
            r = 1.0 - (-r * p.exposure).exp();
            g = 1.0 - (-g * p.exposure).exp();
            b = 1.0 - (-b * p.exposure).exp();
            r = r.powf(1.0 / p.gamma);
            g = g.powf(1.0 / p.gamma);
            b = b.powf(1.0 / p.gamma);

            if p.orbit.enabled {
                let trap = (-trap_min * p.orbit.softness).exp().clamp(0.0, 1.0);
                r = Interp::lerp(r, p.orbit.color[0], trap);
                g = Interp::lerp(g, p.orbit.color[1], trap);
                b = Interp::lerp(b, p.orbit.color[2], trap);
            }

            *px = Rgba([(r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8, 255]);
        }
    }
    buf.into_raw()
}

fn sample_palette(lut: &[[u8; 3]], t: f32) -> [u8; 3] {
    let idx = ((lut.len() - 1) as f32 * t.clamp(0.0, 1.0)) as usize;
    lut[idx]
}

// ------------------------- Export (blocking) -------------------------

#[derive(thiserror::Error, Debug)]
enum ExportError {
    #[error("IO: {0}")]
    Io(#[from] std::io::Error),
    #[error("Image: {0}")]
    Image(ImageError),
    #[cfg(feature = "gpu")]
    #[error("GPU: {0}")]
    Gpu(String),
    #[error("FFmpeg failed")]
    Ffmpeg,
}

impl From<ImageError> for ExportError {
    fn from(err: ImageError) -> Self {
        ExportError::Image(err)
    }
}

fn export_video_blocking(
    proj: &Project,
    #[cfg(feature = "gpu")] gpu: Option<&mut GpuRenderer>,
) -> Result<(), ExportError> {
    let tmp = tempfile::tempdir()?;
    let dir = tmp.path();
    let total = (proj.export.duration * proj.export.fps as f32).round() as u32;

    #[cfg(feature = "gpu")]
    let mut gpu = gpu;

    for frame in 0..total {
        let time = frame as f32 / proj.export.fps as f32;
        let mut p = proj.clone();
        let (key_t, zoom_t) = p.anim.resolve_times(time);
        p.camera.scale = p.anim.sample_zoom(zoom_t, p.camera.scale);
        p.fractal.palette_phase = p.anim.kf_palette.sample(key_t, p.fractal.palette_phase);
        p.camera.center.re = p.anim.kf_center_x.sample(key_t, p.camera.center.re);
        p.camera.center.im = p.anim.kf_center_y.sample(key_t, p.camera.center.im);

        let pixels = render_image(
            (proj.export.width, proj.export.height),
            &p.fractal,
            &p.camera,
            proj.render_backend,
            proj.export.tile_size,
            #[cfg(feature = "gpu")]
            gpu.as_deref_mut(),
        );
        let img =
            ImageBuffer::<Rgba<u8>, _>::from_raw(proj.export.width, proj.export.height, pixels)
                .unwrap();
        let path = dir.join(format!("frame_{:06}.png", frame));
        img.save(&path)?;
    }

    let mut args = vec![
        "-y".into(),
        "-framerate".into(),
        proj.export.fps.to_string(),
        "-i".into(),
        format!("{}/frame_%06d.png", dir.display()),
    ];
    args.extend(proj.export.codec.ffmpeg_args(proj.export.crf));
    args.push(proj.export.out_path.display().to_string());

    let status = std::process::Command::new("ffmpeg").args(args).status();
    if matches!(status, Ok(st) if st.success()) {
        Ok(())
    } else {
        Err(ExportError::Ffmpeg)
    }
}

// ------------------------- Entry -------------------------

fn main() -> eframe::Result<()> {
    let args = Args::parse();
    if let Some(Cmd::Export { project, out }) = args.cmd {
        let mut proj = if project.exists() {
            load_project(&project).unwrap_or_default()
        } else {
            Project::default()
        };
        if let Some(out) = out {
            proj.export.out_path = out;
        }
        #[cfg(feature = "gpu")]
        let mut gpu = if proj.render_backend == RenderBackend::Gpu {
            match GpuRenderer::new() {
                Ok(renderer) => Some(renderer),
                Err(err) => {
                    eprintln!("GPU init failed: {err}. Falling back to CPU.");
                    None
                }
            }
        } else {
            None
        };
        export_video_blocking(
            &proj,
            #[cfg(feature = "gpu")]
            gpu.as_mut(),
        )
        .expect("Export failed");
        return Ok(());
    }

    let mut proj = Project::default();
    if let Some(p) = args.project {
        if p.exists() {
            proj = load_project(&p).unwrap_or_default();
        }
    }

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 840.0])
            .with_min_inner_size([900.0, 600.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Matterhorn AH",
        options,
        Box::new(|_cc| {
            Box::new(MatterhornApp {
                proj,
                tex: None,
                last_update: Instant::now(),
                #[cfg(feature = "gpu")]
                gpu: None,
            })
        }),
    )
}

// ------------------------- Project IO -------------------------

fn save_project_dialog_json(p: &Project) {
    if let Some(path) = rfd::FileDialog::new()
        .add_filter("Project", &["json"])
        .set_file_name("project.json")
        .save_file()
    {
        if let Ok(data) = serde_json::to_string_pretty(p) {
            let _ = fs::write(&path, data);
        }
    }
}

fn save_project_dialog_toml(p: &Project) {
    if let Some(path) = rfd::FileDialog::new()
        .add_filter("Matterhorn", &["mahproj", "toml"])
        .set_file_name("project.mahproj")
        .save_file()
    {
        if let Ok(data) = toml::to_string_pretty(p) {
            let _ = fs::write(&path, data);
        }
    }
}

fn open_project_dialog() -> Option<Project> {
    let file = rfd::FileDialog::new()
        .add_filter("Project", &["json", "mahproj", "toml"])
        .pick_file()?;
    load_project(&file).ok()
}

fn load_project(path: &Path) -> Result<Project, String> {
    let data = fs::read_to_string(path).map_err(|e| e.to_string())?;
    match path.extension().and_then(|ext| ext.to_str()).unwrap_or("") {
        "json" => serde_json::from_str(&data).map_err(|e| e.to_string()),
        "mahproj" | "toml" => toml::from_str(&data).map_err(|e| e.to_string()),
        _ => serde_json::from_str(&data)
            .or_else(|_| toml::from_str(&data))
            .map_err(|e| e.to_string()),
    }
}

fn save_palette_dialog(stops: &[PaletteStop]) {
    if let Some(path) = rfd::FileDialog::new()
        .add_filter("Palette", &["ahpal"])
        .set_file_name("palette.ahpal")
        .save_file()
    {
        if let Ok(data) = serde_json::to_string_pretty(stops) {
            let _ = fs::write(path, data);
        }
    }
}

fn load_palette_dialog() -> Option<Vec<PaletteStop>> {
    let file = rfd::FileDialog::new()
        .add_filter("Palette", &["ahpal"])
        .pick_file()?;
    let data = fs::read_to_string(file).ok()?;
    serde_json::from_str(&data).ok()
}

// ------------------------- Optional: file dialog dep -------------------------
mod rfd_shim {
    pub use rfd::*;
}
use rfd_shim as rfd;

#[cfg(feature = "gpu")]
mod gpu_renderer {
    use super::{Camera, FractalKind, FractalParams, OrbitTrapKind, TileInfo};
    use bytemuck::{Pod, Zeroable};
    use std::borrow::Cow;
    use std::num::NonZeroU64;
    use wgpu::util::DeviceExt;

    const SHADER_SRC: &str = r#"
struct VertexOut {
    @builtin(position) pos: vec4<f32>;
    @location(0) uv: vec2<f32>;
};

@vertex
fn vs_main(@builtin(vertex_index) idx: u32) -> VertexOut {
    var positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -3.0),
        vec2<f32>(-1.0, 1.0),
        vec2<f32>(3.0, 1.0),
    );
    var output: VertexOut;
    let pos = positions[idx];
    output.pos = vec4<f32>(pos, 0.0, 1.0);
    output.uv = pos * 0.5 + vec2<f32>(0.5, 0.5);
    return output;
}

struct Params {
    full: vec2<f32>;
    offset: vec2<f32>;
    tile: vec2<f32>;
    center: vec2<f32>;
    julia_c: vec2<f32>;
    trap_point: vec2<f32>;
    orbit_color: vec3<f32>;
    orbit_enabled: f32;
    scale: f32;
    rotation: f32;
    max_iter: u32;
    fractal_kind: u32;
    escape_radius: f32;
    power: f32;
    orbit_kind: u32;
    orbit_radius: f32;
    orbit_softness: f32;
    exposure: f32;
    gamma: f32;
};

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var palette_tex: texture_2d<f32>;
@group(0) @binding(2) var palette_sampler: sampler;

fn palette_sample(t: f32) -> vec3<f32> {
    return textureSample(palette_tex, palette_sampler, vec2<f32>(fract(t), 0.5)).rgb;
}

@fragment
fn fs_main(input: VertexOut) -> @location(0) vec4<f32> {
    let pixel = params.offset + input.uv * params.tile;
    let screen = pixel - params.full * 0.5;
    let cos_r = cos(params.rotation);
    let sin_r = sin(params.rotation);
    let coord = vec2<f32>(
        (screen.x * cos_r - screen.y * sin_r) / params.scale + params.center.x,
        (screen.x * sin_r + screen.y * cos_r) / params.scale + params.center.y,
    );

    var z = vec2<f32>(0.0, 0.0);
    var c = coord;
    if (params.fractal_kind == 1u) {
        z = coord;
        c = params.julia_c;
    }

    let escape = params.escape_radius * params.escape_radius;
    var iter: u32 = 0u;
    var smooth: f32 = 0.0;
    var trap: f32 = 1e6;

    loop {
        if (iter >= params.max_iter) {
            break;
        }
        var zx = z.x;
        var zy = z.y;
        var x2 = zx * zx;
        var y2 = zy * zy;

        if (x2 + y2 > escape) {
            let radius = sqrt(x2 + y2);
            let log_r = log(max(radius, 1e-5));
            let mu = f32(iter) + 1.0 - log(log_r) / log(2.0);
            smooth = mu / f32(params.max_iter);
            break;
        }

        switch params.fractal_kind {
            case 0u, 1u: {
                z = vec2<f32>(x2 - y2 + c.x, 2.0 * zx * zy + c.y);
            }
            case 2u: {
                let new_x = x2 - y2 + c.x;
                let new_y = 2.0 * abs(zx) * abs(zy) + c.y;
                z = vec2<f32>(abs(new_x), abs(new_y));
            }
            default: {
                let r = sqrt(x2 + y2);
                let theta = atan2(zy, zx);
                let rp = pow(r, params.power);
                let th = theta * params.power;
                z = vec2<f32>(rp * cos(th) + c.x, rp * sin(th) + c.y);
            }
        }

        if (params.orbit_enabled > 0.5) {
            let dist = switch params.orbit_kind {
                case 0u => length(z - params.trap_point),
                case 1u => abs(length(z) - params.orbit_radius),
                default => min(abs(z.x - params.trap_point.x), abs(z.y - params.trap_point.y)),
            };
            trap = min(trap, dist);
        }

        iter = iter + 1u;
    }

    var color = palette_sample(smooth);
    color = 1.0 - exp(-color * params.exposure);
    color = pow(color, vec3<f32>(1.0 / params.gamma));

    if (params.orbit_enabled > 0.5) {
        let trap_mix = clamp(exp(-trap * params.orbit_softness), 0.0, 1.0);
        color = color + (params.orbit_color - color) * trap_mix;
    }

    return vec4<f32>(color, 1.0);
}
"#;

    pub struct GpuRenderer {
        device: wgpu::Device,
        queue: wgpu::Queue,
        pipeline: wgpu::RenderPipeline,
        bind_group_layout: wgpu::BindGroupLayout,
        sampler: wgpu::Sampler,
    }

    impl GpuRenderer {
        pub fn new() -> Result<Self, String> {
            let instance = wgpu::Instance::default();
            let adapter = pollster::block_on(
                instance.request_adapter(&wgpu::RequestAdapterOptions::default()),
            )
            .ok_or_else(|| "No GPU adapter available".to_string())?;
            let (device, queue) = pollster::block_on(
                adapter.request_device(&wgpu::DeviceDescriptor::default(), None),
            )
            .map_err(|e| format!("Failed to create device: {e}"))?;

            let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("fractal_shader"),
                source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(SHADER_SRC)),
            });

            let bind_group_layout =
                device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("fractal_bind"),
                    entries: &[
                        wgpu::BindGroupLayoutEntry {
                            binding: 0,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Buffer {
                                ty: wgpu::BufferBindingType::Uniform,
                                has_dynamic_offset: false,
                                min_binding_size: Some(
                                    NonZeroU64::new(std::mem::size_of::<GpuUniform>() as u64)
                                        .unwrap(),
                                ),
                            },
                            count: None,
                        },
                        wgpu::BindGroupLayoutEntry {
                            binding: 1,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Texture {
                                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                                view_dimension: wgpu::TextureViewDimension::D2,
                                multisampled: false,
                            },
                            count: None,
                        },
                        wgpu::BindGroupLayoutEntry {
                            binding: 2,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                            count: None,
                        },
                    ],
                });

            let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("fractal_layout"),
                bind_group_layouts: &[&bind_group_layout],
                push_constant_ranges: &[],
            });

            let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("fractal_pipeline"),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: "vs_main",
                    buffers: &[],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: "fs_main",
                    targets: &[Some(wgpu::ColorTargetState {
                        format: wgpu::TextureFormat::Rgba8Unorm,
                        blend: Some(wgpu::BlendState::REPLACE),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview: None,
            });

            let sampler = device.create_sampler(&wgpu::SamplerDescriptor::default());

            Ok(Self {
                device,
                queue,
                pipeline,
                bind_group_layout,
                sampler,
            })
        }

        pub fn render(
            &mut self,
            tile: &TileInfo,
            params: &FractalParams,
            cam: &Camera,
            palette: &[[u8; 3]],
        ) -> Result<Vec<u8>, String> {
            let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("fractal_target"),
                size: wgpu::Extent3d {
                    width: tile.tile_w,
                    height: tile.tile_h,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

            let palette_texture = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("palette"),
                size: wgpu::Extent3d {
                    width: palette.len() as u32,
                    height: 1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let palette_stride = align_to(
                (palette.len() as u32) * 4,
                wgpu::COPY_BYTES_PER_ROW_ALIGNMENT,
            ) as usize;
            let mut rgba = vec![0u8; palette_stride];
            for (idx, rgb) in palette.iter().enumerate() {
                let offset = idx * 4;
                rgba[offset..offset + 4].copy_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
            }
            self.queue.write_texture(
                palette_texture.as_image_copy(),
                &rgba,
                wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(palette_stride as u32),
                    rows_per_image: Some(1),
                },
                wgpu::Extent3d {
                    width: palette.len() as u32,
                    height: 1,
                    depth_or_array_layers: 1,
                },
            );
            let palette_view = palette_texture.create_view(&wgpu::TextureViewDescriptor::default());

            let uniforms = GpuUniform::new(tile, params, cam);
            let uniform_buffer =
                self.device
                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("fractal_uniform"),
                        contents: bytemuck::bytes_of(&uniforms),
                        usage: wgpu::BufferUsages::UNIFORM,
                    });

            let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("fractal_bind"),
                layout: &self.bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: uniform_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&palette_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                ],
            });

            let mut encoder = self
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("fractal_encoder"),
                });
            {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("fractal_pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                });
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, &bind_group, &[]);
                pass.draw(0..3, 0..1);
            }
            self.queue.submit(Some(encoder.finish()));

            let bytes_per_row = align_to(tile.tile_w * 4, wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
            let buffer_size = bytes_per_row as u64 * tile.tile_h as u64;
            let output_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("fractal_readback"),
                size: buffer_size,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });

            let mut encoder = self
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("copy_encoder"),
                });
            encoder.copy_texture_to_buffer(
                texture.as_image_copy(),
                wgpu::ImageCopyBuffer {
                    buffer: &output_buffer,
                    layout: wgpu::ImageDataLayout {
                        offset: 0,
                        bytes_per_row: Some(bytes_per_row),
                        rows_per_image: Some(tile.tile_h),
                    },
                },
                wgpu::Extent3d {
                    width: tile.tile_w,
                    height: tile.tile_h,
                    depth_or_array_layers: 1,
                },
            );
            self.queue.submit(Some(encoder.finish()));

            let slice = output_buffer.slice(..);
            let map_future = slice.map_async(wgpu::MapMode::Read);
            self.device.poll(wgpu::Maintain::Wait);
            pollster::block_on(map_future).map_err(|e| format!("Map error: {e}"))?;
            let data = slice.get_mapped_range();
            let mut pixels = vec![0u8; (tile.tile_w * tile.tile_h * 4) as usize];
            let row_bytes = (tile.tile_w * 4) as usize;
            let padded = bytes_per_row as usize;
            for (row_idx, chunk) in pixels.chunks_mut(row_bytes).enumerate() {
                let start = row_idx * padded;
                chunk.copy_from_slice(&data[start..start + row_bytes]);
            }
            drop(data);
            output_buffer.unmap();
            Ok(pixels)
        }
    }

    #[repr(C)]
    #[derive(Clone, Copy, Pod, Zeroable)]
    struct GpuUniform {
        full: [f32; 2],
        offset: [f32; 2],
        tile: [f32; 2],
        center: [f32; 2],
        julia_c: [f32; 2],
        trap_point: [f32; 2],
        orbit_color: [f32; 3],
        orbit_enabled: f32,
        scale: f32,
        rotation: f32,
        max_iter: u32,
        fractal_kind: u32,
        escape_radius: f32,
        power: f32,
        orbit_kind: u32,
        orbit_radius: f32,
        orbit_softness: f32,
        exposure: f32,
        gamma: f32,
    }

    impl GpuUniform {
        fn new(tile: &TileInfo, params: &FractalParams, cam: &Camera) -> Self {
            Self {
                full: [tile.full_w as f32, tile.full_h as f32],
                offset: [tile.offset_x as f32, tile.offset_y as f32],
                tile: [tile.tile_w as f32, tile.tile_h as f32],
                center: [cam.center.re, cam.center.im],
                julia_c: [params.c.re, params.c.im],
                trap_point: [params.orbit.point.re, params.orbit.point.im],
                orbit_color: params.orbit.color,
                orbit_enabled: if params.orbit.enabled { 1.0 } else { 0.0 },
                scale: cam.scale,
                rotation: cam.rotation,
                max_iter: params.max_iter,
                fractal_kind: match params.kind {
                    FractalKind::Mandelbrot => 0,
                    FractalKind::Julia => 1,
                    FractalKind::BurningShip => 2,
                    FractalKind::Multibrot => 3,
                },
                escape_radius: params.escape_radius,
                power: params.power,
                orbit_kind: match params.orbit.kind {
                    OrbitTrapKind::Point => 0,
                    OrbitTrapKind::Circle => 1,
                    OrbitTrapKind::Cross => 2,
                },
                orbit_radius: params.orbit.radius,
                orbit_softness: params.orbit.softness,
                exposure: params.exposure,
                gamma: params.gamma,
            }
        }
    }

    fn align_to(value: u32, alignment: u32) -> u32 {
        ((value + alignment - 1) / alignment) * alignment
    }
}
