//! The Orbis window.
//!
//! ```text
//! ┌ Orbis ─ ● Turntable 192.168.1.50 ─ ● Camera X-T4 ───────────────────┐
//! │                                        │ SCAN  name, presets, rings,     │
//! │   angle wheel        photo preview     │       estimate, Start / Dry run │
//! │                                        │ CAMERA  exposure, AF, test shot │
//! │   ▢▢▢▢▢▢▢ thumbnails                   │ TURNTABLE  position, jog, speed │
//! ├────────────────────────────────────────┴─────────────────────────────────┤
//! │ log                                                                      │
//! └──────────────────────────────────────────────────────────────────────────┘
//! ```

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use egui::{Color32, ColorImage, RichText, TextureHandle, TextureOptions, Ui};
use rig::Geometry;
use serde::{Deserialize, Serialize};

use crate::camera::{self, CamCmd, CamEvent, CamInfo, ExposureView};
use crate::plan::{self, format_duration, Preset, ScanPlan, ScanSettings};
use crate::wheel::{self, WheelState};
use crate::worker::{self, Cmd, Event, ScanOutput};

const GREEN: Color32 = Color32::from_rgb(0x3D, 0xC2, 0x6B);
const AMBER: Color32 = Color32::from_rgb(0xF2, 0xB1, 0x34);
const RED: Color32 = Color32::from_rgb(0xE5, 0x48, 0x4D);

/// Saved between sessions.
#[derive(Serialize, Deserialize)]
#[serde(default)]
struct Saved {
    scan: ScanSettings,
    motor_steps: u32,
    microsteps: u32,
    gear_ratio: f64,
    /// Turntable top speed in degrees per second.
    speed_deg_s: f64,
    /// Name of the next scan (used in the folder name).
    scan_name: String,
    /// Scans go into `<root>/<date>_<time>_<name>/`.
    scans_root: PathBuf,
    /// Focus before every photo (off = use the focus as set, e.g. MF).
    autofocus: bool,
}

fn default_scans_root() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .map(|h| PathBuf::from(h).join("Pictures").join("Orbis"))
        .unwrap_or_else(|| PathBuf::from("scans"))
}

impl Default for Saved {
    fn default() -> Self {
        Self {
            scan: ScanSettings::default(),
            motor_steps: 200,
            microsteps: 16,
            gear_ratio: 1.0,
            speed_deg_s: 90.0,
            scan_name: "object".into(),
            scans_root: default_scans_root(),
            autofocus: true,
        }
    }
}

impl Saved {
    fn geometry(&self) -> Geometry {
        Geometry { motor_steps: self.motor_steps, microsteps: self.microsteps, gear_ratio: self.gear_ratio }
    }

    /// (steps/s, steps/s²): reach top speed in half a second.
    fn motion(&self) -> (u32, u32) {
        let speed = (self.speed_deg_s / 360.0 * self.geometry().steps_per_table_rev()).round().max(1.0) as u32;
        (speed, speed * 2)
    }

    /// Folder for a scan starting now.
    fn new_scan_dir(&self) -> PathBuf {
        let name: String = self
            .scan_name
            .trim()
            .chars()
            .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
            .collect();
        let name = if name.is_empty() { "scan".into() } else { name };
        self.scans_root.join(format!("{}_{name}", chrono::Local::now().format("%Y-%m-%d_%H%M%S")))
    }
}

enum Link {
    Connecting,
    Connected(String),
    Down(String),
}

enum CamState {
    Searching,
    Connected(CamInfo),
    Problem(String),
}

struct ScanView {
    plan: ScanPlan,
    dir: Option<PathBuf>,
    done: Vec<u32>,
    current: Option<(u32, u32)>,
    paused: bool,
    waiting_ring: Option<u32>,
    started: Instant,
}

struct Thumb {
    tex: TextureHandle,
    path: PathBuf,
}

pub struct OrbisApp {
    saved: Saved,
    cmd: Sender<Cmd>,
    events: Receiver<Event>,
    cam_cmd: Sender<CamCmd>,
    cam_events: Receiver<CamEvent>,
    link: Link,
    cam: CamState,
    exposure: Option<ExposureView>,
    table_deg: Option<f64>,
    moving: bool,
    enabled: bool,
    scan: Option<ScanView>,
    /// Shown after a scan ends, until the next one starts.
    last_scan: Option<ScanView>,
    /// Big preview and which file it shows.
    preview: Option<(TextureHandle, PathBuf)>,
    thumbs: Vec<Thumb>,
    /// Previews decoded on demand when a thumbnail is clicked.
    loaded_tx: Sender<(PathBuf, Option<ColorImage>)>,
    loaded_rx: Receiver<(PathBuf, Option<ColorImage>)>,
    test_shot_busy: bool,
    log: VecDeque<String>,
}

impl OrbisApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let saved: Saved = cc.storage.and_then(|s| eframe::get_value(s, eframe::APP_KEY)).unwrap_or_default();
        let (cam_cmd, cam_events) = camera::spawn(cc.egui_ctx.clone());
        let (cmd, events) = worker::spawn(cc.egui_ctx.clone(), cam_cmd.clone());
        let _ = cmd.send(Cmd::SetGeometry(saved.geometry()));
        let (loaded_tx, loaded_rx) = mpsc::channel();
        Self {
            saved,
            cmd,
            events,
            cam_cmd,
            cam_events,
            link: Link::Connecting,
            cam: CamState::Searching,
            exposure: None,
            table_deg: None,
            moving: false,
            enabled: true,
            scan: None,
            last_scan: None,
            preview: None,
            thumbs: Vec::new(),
            loaded_tx,
            loaded_rx,
            test_shot_busy: false,
            log: VecDeque::new(),
        }
    }

    fn send(&self, c: Cmd) {
        let _ = self.cmd.send(c);
    }

    fn push_log(&mut self, msg: String) {
        if self.log.len() >= 300 {
            self.log.pop_front();
        }
        self.log.push_back(format!("{}  {msg}", chrono::Local::now().format("%H:%M:%S")));
    }

    fn connected(&self) -> bool {
        matches!(self.link, Link::Connected(_))
    }

    fn camera_ready(&self) -> bool {
        matches!(self.cam, CamState::Connected(_))
    }

    fn start_scan(&mut self, real: bool) {
        let output = real.then(|| ScanOutput {
            dir: self.saved.new_scan_dir(),
            name: self.saved.scan_name.clone(),
            autofocus: self.saved.autofocus,
        });
        self.send(Cmd::StartScan { plan: self.saved.scan.plan(), output });
    }

    fn handle_events(&mut self, ctx: &egui::Context) {
        while let Ok(ev) = self.events.try_recv() {
            match ev {
                Event::Connecting => {
                    if !matches!(self.link, Link::Down(_)) {
                        self.link = Link::Connecting;
                    }
                }
                Event::Connected(how) => {
                    self.link = Link::Connected(how);
                    let (max_speed, accel) = self.saved.motion();
                    self.send(Cmd::SetGeometry(self.saved.geometry()));
                    self.send(Cmd::SetMotion { max_speed, accel });
                }
                Event::Disconnected(why) => {
                    self.link = Link::Down(why);
                    self.table_deg = None;
                }
                Event::Status { degrees, moving, enabled } => {
                    self.table_deg = Some(degrees);
                    self.moving = moving;
                    self.enabled = enabled;
                }
                Event::ButtonPressed => {
                    self.push_log("BOOT button pressed".into());
                    match &self.scan {
                        Some(s) if s.waiting_ring.is_some() => self.continue_ring(),
                        Some(_) => self.send(Cmd::Stop),
                        None if self.connected() => self.start_scan(self.camera_ready()),
                        None => {}
                    }
                }
                Event::ScanStarted { plan, dir } => {
                    self.last_scan = None;
                    if dir.is_some() {
                        self.thumbs.clear();
                        self.preview = None;
                    }
                    self.scan = Some(ScanView {
                        plan,
                        dir,
                        done: vec![0; plan.rings as usize],
                        current: Some((0, 0)),
                        paused: false,
                        waiting_ring: None,
                        started: Instant::now(),
                    });
                }
                Event::Shot { ring, index, angle, files } => {
                    let what = if files.is_empty() {
                        String::new()
                    } else {
                        format!(
                            "  →  {}",
                            files
                                .iter()
                                .filter_map(|f| f.file_name())
                                .map(|n| n.to_string_lossy())
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    };
                    self.push_log(format!("ring {} · position {:>3} · {angle:6.1}°{what}", ring + 1, index + 1));
                    if let Some(s) = &mut self.scan {
                        s.done[ring as usize] = index + 1;
                        s.current = if index + 1 < s.plan.shots_per_ring {
                            Some((ring, index + 1))
                        } else if ring + 1 < s.plan.rings {
                            Some((ring + 1, 0))
                        } else {
                            None
                        };
                    }
                }
                Event::WaitingForRing(r) => {
                    if let Some(s) = &mut self.scan {
                        s.waiting_ring = Some(r);
                    }
                }
                Event::ScanPaused(p) => {
                    if let Some(s) = &mut self.scan {
                        s.paused = p;
                    }
                }
                Event::ScanFinished | Event::ScanStopped => {
                    if let Some(mut s) = self.scan.take() {
                        s.current = None;
                        s.waiting_ring = None;
                        self.last_scan = Some(s);
                    }
                }
                Event::Log(msg) => self.push_log(msg),
            }
        }

        while let Ok(ev) = self.cam_events.try_recv() {
            match ev {
                CamEvent::Searching => {
                    if !matches!(self.cam, CamState::Problem(_)) {
                        self.cam = CamState::Searching;
                    }
                }
                CamEvent::Connected(info) => self.cam = CamState::Connected(info),
                CamEvent::Problem(why) => {
                    self.cam = CamState::Problem(why);
                    self.exposure = None;
                }
                CamEvent::Exposure(e) => self.exposure = Some(e),
                CamEvent::Photo { path, preview, thumb } => {
                    self.test_shot_busy = false;
                    if let Some(img) = preview {
                        let tex = ctx.load_texture("preview", img, TextureOptions::LINEAR);
                        self.preview = Some((tex, path.clone()));
                    }
                    if let Some(img) = thumb {
                        let tex = ctx.load_texture(format!("thumb-{}", path.display()), img, TextureOptions::LINEAR);
                        self.thumbs.push(Thumb { tex, path });
                    }
                }
                CamEvent::Log(msg) => self.push_log(msg),
            }
        }

        // A clicked thumbnail finished decoding.
        while let Ok((path, img)) = self.loaded_rx.try_recv() {
            if let Some(img) = img {
                let tex = ctx.load_texture("preview", img, TextureOptions::LINEAR);
                self.preview = Some((tex, path));
            }
        }
    }

    fn continue_ring(&mut self) {
        if let Some(s) = &mut self.scan {
            s.waiting_ring = None;
        }
        self.send(Cmd::ContinueRing);
    }

    fn test_shot(&mut self) {
        let stem = self
            .saved
            .scans_root
            .join("test-shots")
            .join(format!("test_{}", chrono::Local::now().format("%Y-%m-%d_%H%M%S")));
        // The photo arrives as a CamEvent::Photo; the reply isn't needed.
        let (reply, _rx) = mpsc::channel();
        self.test_shot_busy = true;
        self.push_log("Test shot…".into());
        let _ = self.cam_cmd.send(CamCmd::Capture { stem, autofocus: self.saved.autofocus, reply });
    }

    /// Decode a saved photo for the big preview, off the UI thread.
    fn load_preview(&self, path: &Path, ctx: &egui::Context) {
        let tx = self.loaded_tx.clone();
        let path = path.to_owned();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let img = std::fs::read(&path).ok().and_then(|data| camera::decode_previews(&data).0);
            let _ = tx.send((path, img));
            ctx.request_repaint();
        });
    }

    // ---------------------------------------------------------------- UI parts

    fn top_bar(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("◎ Orbis").size(20.0).strong());
            ui.add_space(16.0);
            ui.separator();
            let (color, text) = match &self.link {
                Link::Connecting => (AMBER, "Turntable: searching…".to_owned()),
                Link::Connected(how) => (GREEN, format!("Turntable: {how}")),
                Link::Down(why) if why.contains("one client at a time") => {
                    (AMBER, "Turntable busy: in use by another window".to_owned())
                }
                Link::Down(_) => (RED, "Turntable: not found, retrying".to_owned()),
            };
            ui.label(RichText::new("●").color(color));
            let resp = ui.label(text);
            if let Link::Down(why) = &self.link {
                resp.on_hover_text(why);
            }
            if ui.small_button("⟳").on_hover_text("Reconnect").clicked() {
                self.link = Link::Connecting;
                self.send(Cmd::Reconnect);
            }
            ui.separator();
            let (color, text, hover) = match &self.cam {
                CamState::Searching => (AMBER, "Camera: searching…".to_owned(), None),
                CamState::Connected(i) => (
                    GREEN,
                    format!("Camera: {}{}", i.model, if i.lens.is_empty() { String::new() } else { format!(" · {}", i.lens) }),
                    Some(format!("Firmware {}", i.firmware)),
                ),
                CamState::Problem(why) => (RED, "Camera: not ready".to_owned(), Some(why.clone())),
            };
            ui.label(RichText::new("●").color(color));
            let resp = ui.label(text);
            if let Some(h) = hover {
                resp.on_hover_text(h);
            }
        });
        if let CamState::Problem(why) = &self.cam {
            ui.label(RichText::new(format!("⚠ {why}")).color(AMBER).small());
        }
    }

    fn scan_panel(&mut self, ui: &mut Ui) {
        ui.heading("Scan");
        ui.add_space(4.0);
        let scanning = self.scan.is_some();

        ui.add_enabled_ui(!scanning, |ui| {
            ui.horizontal(|ui| {
                ui.label("Name");
                ui.add(egui::TextEdit::singleline(&mut self.saved.scan_name).desired_width(200.0));
            });
            ui.label("Detail");
            ui.horizontal(|ui| {
                for p in Preset::ALL {
                    ui.selectable_value(&mut self.saved.scan.preset, p, p.label());
                }
            });
            ui.label(RichText::new(self.saved.scan.preset.hint()).weak().small());
            if self.saved.scan.preset == Preset::Custom {
                let shots = &mut self.saved.scan.custom_shots;
                ui.add(
                    egui::Slider::new(shots, 8..=180)
                        .text("photos per ring")
                        .custom_formatter(|v, _| format!("{v:.0} ({:.1}°)", 360.0 / v)),
                );
            }
            ui.add_space(6.0);
            ui.add(egui::Slider::new(&mut self.saved.scan.rings, 1..=6).text("rings (camera heights)"));
            ui.add(
                egui::Slider::new(&mut self.saved.scan.settle_ms, 0..=2000)
                    .step_by(50.0)
                    .text("settle time")
                    .suffix(" ms"),
            )
            .on_hover_text("Pause after each turn so the object stops wobbling before the photo.");
        });

        // Summary.
        let p = self.saved.scan.plan();
        let (max_speed, accel) = self.saved.motion();
        let geo = self.saved.geometry();
        ui.add_space(8.0);
        egui::Grid::new("summary").num_columns(2).spacing([12.0, 4.0]).show(ui, |ui| {
            ui.label("Angle step");
            ui.strong(format!("{:.1}°", p.angle_step()));
            ui.end_row();
            ui.label("Photos");
            ui.strong(format!("{} × {} = {}", p.rings, p.shots_per_ring, p.total_shots()));
            ui.end_row();
            ui.label("Time (estimate)");
            ui.strong(format_duration(p.estimate(&geo, max_speed, accel, plan::CAMERA_SHOT_TIME)));
            ui.end_row();
            ui.label("Disk (JPEG)");
            ui.strong(format!("≈ {:.1} GB", p.disk_mb() / 1024.0));
            ui.end_row();
        });

        ui.add_space(10.0);
        match &self.scan {
            None => {
                let ready = self.connected() && self.camera_ready();
                let btn = egui::Button::new(RichText::new("▶  Start scan").size(16.0).strong())
                    .min_size(egui::vec2(ui.available_width(), 38.0));
                let hint = if ready {
                    "Turn, settle, shoot and save every position"
                } else {
                    "Needs both the turntable and the camera connected"
                };
                if ui.add_enabled(ready, btn).on_hover_text(hint).on_disabled_hover_text(hint).clicked() {
                    self.start_scan(true);
                }
                if ui
                    .add_enabled(self.connected(), egui::Button::new("Dry run (turntable only)"))
                    .on_hover_text("Turns through the whole scan without taking photos")
                    .clicked()
                {
                    self.start_scan(false);
                }
            }
            Some(s) => {
                let paused = s.paused;
                let done: u32 = s.done.iter().sum();
                let total = s.plan.total_shots();
                let kind = if s.dir.is_some() { "Scanning" } else { "Dry run" };
                ui.label(RichText::new(if paused { format!("{kind} · paused") } else { kind.to_owned() }).strong());
                ui.add(egui::ProgressBar::new(done as f32 / total as f32).text(format!("{done} / {total}")));
                ui.horizontal(|ui| {
                    let label = if paused { "▶ Resume" } else { "⏸ Pause" };
                    if ui.button(label).clicked() {
                        self.send(Cmd::Pause(!paused));
                    }
                    if ui.button("■ Stop").clicked() {
                        self.send(Cmd::Stop);
                    }
                    ui.label(RichText::new(format_duration(s.started.elapsed())).weak());
                });
            }
        }
        ui.label(RichText::new("The BOOT button on the turntable also starts / stops.").weak().small());

        // Output folder.
        ui.add_space(6.0);
        let folder = self
            .scan
            .as_ref()
            .and_then(|s| s.dir.clone())
            .or_else(|| self.last_scan.as_ref().and_then(|s| s.dir.clone()))
            .unwrap_or_else(|| self.saved.scans_root.clone());
        ui.horizontal(|ui| {
            ui.label(RichText::new(folder.display().to_string()).small().weak()).on_hover_text("Where photos are saved");
            if ui.small_button("📂").on_hover_text("Open folder").clicked() {
                let _ = std::fs::create_dir_all(&folder);
                let _ = std::process::Command::new("explorer").arg(&folder).spawn();
            }
        });
    }

    fn camera_panel(&mut self, ui: &mut Ui) {
        ui.heading("Camera");
        ui.add_space(4.0);
        match (&self.cam, &self.exposure) {
            (CamState::Connected(_), Some(e)) => {
                egui::Grid::new("exposure").num_columns(2).spacing([12.0, 3.0]).show(ui, |ui| {
                    for (k, v) in [
                        ("Mode", &e.mode),
                        ("Aperture", &e.aperture),
                        ("Shutter", &e.shutter),
                        ("ISO", &e.iso),
                        ("Records", &e.records),
                    ] {
                        ui.label(k);
                        ui.strong(v);
                        ui.end_row();
                    }
                });
                let mut tips = Vec::new();
                if !e.mode.starts_with('M') {
                    tips.push("Use M mode so every photo has the same exposure.");
                }
                if e.iso.starts_with("auto") {
                    tips.push("Set a fixed ISO; auto ISO changes brightness between shots.");
                }
                if e.records == "RAW only" {
                    tips.push("RAW only: no preview. Choose RAW + JPEG.");
                }
                for t in tips {
                    ui.label(RichText::new(format!("💡 {t}")).small().color(AMBER));
                }
            }
            (CamState::Connected(_), None) => {
                ui.label(RichText::new("reading settings…").weak());
            }
            _ => {
                ui.label(RichText::new("Not connected. Camera on USB in USB TETHER SHOOTING AUTO mode.").weak());
            }
        }
        ui.checkbox(&mut self.saved.autofocus, "Autofocus before every photo")
            .on_hover_text("Off: the camera keeps its current focus (best with manual focus set once).");
        let can_test = self.camera_ready() && self.scan.is_none() && !self.test_shot_busy;
        if ui.add_enabled(can_test, egui::Button::new("📷 Test shot")).clicked() {
            self.test_shot();
        }
    }

    fn turntable_panel(&mut self, ui: &mut Ui) {
        ui.heading("Turntable");
        ui.add_space(4.0);
        let pos = self.table_deg.map(|d| format!("{d:.1}°")).unwrap_or_else(|| "—".into());
        ui.horizontal(|ui| {
            ui.label(RichText::new(pos).size(28.0).strong());
            if self.moving {
                ui.label(RichText::new("moving").color(AMBER));
            }
        });

        let can_move = self.connected() && self.scan.is_none();
        ui.add_enabled_ui(can_move, |ui| {
            ui.horizontal(|ui| {
                for d in [-90.0, -15.0, -5.0, -1.0] {
                    if ui.button(format!("{d:+}°")).clicked() {
                        self.send(Cmd::Jog(d));
                    }
                }
            });
            ui.horizontal(|ui| {
                for d in [1.0, 5.0, 15.0, 90.0] {
                    if ui.button(format!("{d:+}°")).clicked() {
                        self.send(Cmd::Jog(d));
                    }
                }
            });
            ui.horizontal(|ui| {
                if ui.button("Go to 0°").clicked() {
                    self.send(Cmd::GoTo(0.0));
                }
                if ui.button("Set as 0°").on_hover_text("Make the current position the scan's 0°").clicked() {
                    self.send(Cmd::SetZero);
                }
            });
            let mut enabled = self.enabled;
            if ui
                .checkbox(&mut enabled, "Motor holds position")
                .on_hover_text("Off = released, the plate turns freely by hand")
                .changed()
            {
                self.send(Cmd::Enable(enabled));
            }
            let speed = ui.add(egui::Slider::new(&mut self.saved.speed_deg_s, 10.0..=180.0).text("speed").suffix(" °/s"));
            if speed.drag_stopped() || (speed.changed() && !speed.dragged()) {
                let (max_speed, accel) = self.saved.motion();
                self.send(Cmd::SetMotion { max_speed, accel });
            }
        });

        egui::CollapsingHeader::new("Mechanics").default_open(false).show(ui, |ui| {
            let before = self.saved.geometry();
            ui.add_enabled_ui(self.scan.is_none(), |ui| {
                ui.add(egui::DragValue::new(&mut self.saved.motor_steps).range(1..=1000).prefix("motor steps/rev: "));
                egui::ComboBox::from_label("microstepping (DIP SW4–6)")
                    .selected_text(format!("1/{}", self.saved.microsteps))
                    .show_ui(ui, |ui| {
                        for m in [1, 2, 4, 8, 16] {
                            ui.selectable_value(&mut self.saved.microsteps, m, format!("1/{m}"));
                        }
                    });
                ui.add(
                    egui::DragValue::new(&mut self.saved.gear_ratio)
                        .range(0.1..=100.0)
                        .speed(0.01)
                        .prefix("gear ratio: "),
                );
            });
            let after = self.saved.geometry();
            if (before.motor_steps, before.microsteps, before.gear_ratio)
                != (after.motor_steps, after.microsteps, after.gear_ratio)
            {
                self.send(Cmd::SetGeometry(after));
                let (max_speed, accel) = self.saved.motion();
                self.send(Cmd::SetMotion { max_speed, accel });
            }
            ui.label(RichText::new(format!("{} steps per turn", after.steps_per_table_rev())).weak().small());
        });
    }

    fn center(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        // Wheel (side × side) + gap + preview (1.5·side wide) must fit the
        // width; the wheel plus the thumbnail strip must fit the height.
        let avail = ui.available_size();
        let side = ((avail.x - 24.0) / 2.5).min(avail.y - 130.0).max(160.0);
        let view = self.scan.as_ref().or(self.last_scan.as_ref());
        let plan = view.map(|s| s.plan).unwrap_or_else(|| self.saved.scan.plan());
        let done: Vec<u32> = view.map(|s| s.done.clone()).unwrap_or_else(|| vec![0; plan.rings as usize]);
        let current = self.scan.as_ref().and_then(|s| s.current);

        let (title, sub) = match (&self.scan, &self.last_scan) {
            (Some(s), _) => {
                let (ring, idx) = s.current.unwrap_or((plan.rings - 1, plan.shots_per_ring - 1));
                (format!("{} / {}", idx + 1, plan.shots_per_ring), format!("ring {} of {}", ring + 1, plan.rings))
            }
            (None, Some(s)) => {
                let total: u32 = s.done.iter().sum();
                (format!("{total} / {}", s.plan.total_shots()), "last scan".into())
            }
            (None, None) => (
                self.table_deg.map(|d| format!("{:.1}°", d.rem_euclid(360.0))).unwrap_or_else(|| "—".into()),
                format!("{} positions", plan.total_shots()),
            ),
        };

        ui.horizontal(|ui| {
            wheel::show(
                ui,
                side,
                &WheelState {
                    shots_per_ring: plan.shots_per_ring,
                    rings: plan.rings,
                    done: &done,
                    current,
                    table_degrees: self.table_deg,
                    center_title: title,
                    center_sub: sub,
                },
            );
            ui.add_space(12.0);
            self.preview_area(ui, egui::vec2(side * 1.5, side));
        });

        ui.add_space(10.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Photos").strong());
            if !self.thumbs.is_empty() {
                ui.label(RichText::new(format!("{} · click to view, double-click to open", self.thumbs.len())).weak().small());
            }
        });
        self.thumb_strip(ui, ctx);
    }

    fn preview_area(&mut self, ui: &mut Ui, size: egui::Vec2) {
        let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
        let v = ui.visuals();
        ui.painter().rect_filled(rect, 8.0, v.extreme_bg_color);
        match &self.preview {
            Some((tex, path)) => {
                let img = tex.size_vec2();
                let scale = (rect.width() / img.x).min(rect.height() / img.y);
                let r = egui::Rect::from_center_size(rect.center(), img * scale);
                ui.painter().image(tex.id(), r, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                ui.painter().text(
                    rect.left_bottom() + egui::vec2(8.0, -8.0),
                    egui::Align2::LEFT_BOTTOM,
                    name,
                    egui::FontId::proportional(13.0),
                    Color32::from_white_alpha(220),
                );
                if resp.double_clicked() {
                    let _ = std::process::Command::new("explorer").arg(path).spawn();
                }
            }
            None => {
                let msg = if self.camera_ready() {
                    "Take a test shot to see the camera's view"
                } else {
                    "Photo preview appears here once the camera is connected"
                };
                ui.painter().text(
                    rect.center() - egui::vec2(0.0, 12.0),
                    egui::Align2::CENTER_CENTER,
                    "📷",
                    egui::FontId::proportional(40.0),
                    v.weak_text_color(),
                );
                ui.painter().text(
                    rect.center() + egui::vec2(0.0, 28.0),
                    egui::Align2::CENTER_CENTER,
                    msg,
                    egui::FontId::proportional(14.0),
                    v.weak_text_color(),
                );
            }
        }
    }

    fn thumb_strip(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        let h = 84.0;
        let mut clicked = None;
        let mut opened = None;
        egui::ScrollArea::horizontal().id_salt("thumbs").stick_to_right(true).show(ui, |ui| {
            ui.horizontal(|ui| {
                if self.thumbs.is_empty() {
                    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), h), egui::Sense::hover());
                    let v = ui.visuals();
                    for i in 0..12 {
                        let x = rect.left() + i as f32 * 92.0;
                        if x + 86.0 > rect.right() {
                            break;
                        }
                        let r = egui::Rect::from_min_size(egui::pos2(x, rect.top()), egui::vec2(86.0, h - 8.0));
                        ui.painter().rect_filled(r, 4.0, v.faint_bg_color);
                    }
                    return;
                }
                let selected = self.preview.as_ref().map(|(_, p)| p.clone());
                for (i, t) in self.thumbs.iter().enumerate() {
                    let size = t.tex.size_vec2();
                    let w = size.x / size.y * (h - 8.0);
                    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, h - 8.0), egui::Sense::click());
                    ui.painter().image(
                        t.tex.id(),
                        rect,
                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                        Color32::WHITE,
                    );
                    if selected.as_ref() == Some(&t.path) {
                        ui.painter().rect_stroke(rect, 2.0, egui::Stroke::new(2.0, ui.visuals().selection.bg_fill), egui::StrokeKind::Outside);
                    }
                    let name = t.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    let resp = resp.on_hover_text(name);
                    if resp.double_clicked() {
                        opened = Some(i);
                    } else if resp.clicked() {
                        clicked = Some(i);
                    }
                }
            });
        });
        if let Some(i) = clicked {
            let path = self.thumbs[i].path.clone();
            self.load_preview(&path, ctx);
        }
        if let Some(i) = opened {
            let _ = std::process::Command::new("explorer").arg(&self.thumbs[i].path).spawn();
        }
    }

    fn ring_dialog(&mut self, ctx: &egui::Context) {
        let Some(next) = self.scan.as_ref().and_then(|s| s.waiting_ring) else { return };
        let rings = self.scan.as_ref().map(|s| s.plan.rings).unwrap_or(0);
        let mut cont = false;
        let mut stop = false;
        egui::Modal::new(egui::Id::unique("ring-break")).show(ctx, |ui| {
            ui.set_width(340.0);
            ui.heading(format!("Ring {next} of {rings} done"));
            ui.add_space(6.0);
            ui.label(format!(
                "Move the camera to height {} of {rings}, then continue.\nThe BOOT button on the turntable works too.",
                next + 1
            ));
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                cont = ui.button(RichText::new("Continue").strong()).clicked();
                stop = ui.button("Stop scan").clicked();
            });
        });
        if cont {
            self.continue_ring();
        }
        if stop {
            self.send(Cmd::Stop);
        }
    }
}

impl eframe::App for OrbisApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.handle_events(&ctx);

        egui::Panel::top("top").show(ui, |ui| {
            ui.add_space(4.0);
            self.top_bar(ui);
            ui.add_space(2.0);
        });
        egui::Panel::bottom("log").resizable(true).default_size(110.0).show(ui, |ui| {
            egui::ScrollArea::vertical().stick_to_bottom(true).auto_shrink([false, false]).show(ui, |ui| {
                for line in &self.log {
                    ui.label(RichText::new(line).monospace().small());
                }
            });
        });
        egui::Panel::right("controls").resizable(false).exact_size(350.0).show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.add_space(6.0);
                self.scan_panel(ui);
                ui.add_space(10.0);
                ui.separator();
                self.camera_panel(ui);
                ui.add_space(10.0);
                ui.separator();
                self.turntable_panel(ui);
            });
        });
        egui::CentralPanel::default().show(ui, |ui| self.center(ui, &ctx));

        self.ring_dialog(&ctx);
        // Keep the elapsed timer and status fresh.
        ctx.request_repaint_after(Duration::from_millis(250));
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, eframe::APP_KEY, &self.saved);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_folder_names_are_safe() {
        let s = Saved { scan_name: "Vase: blue/white?".into(), scans_root: PathBuf::from("root"), ..Default::default() };
        let name = s.new_scan_dir().file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.ends_with("_Vase__blue_white_"), "{name}");
        let s = Saved { scan_name: "   ".into(), ..s };
        assert!(s.new_scan_dir().to_string_lossy().ends_with("_scan"));
    }
}
