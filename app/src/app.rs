//! The Orbis window.
//!
//! ```text
//! ┌ Orbis ─ ● Turntable 192.168.1.50 ─ ● Camera X-T4 ──────────────────────┐
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

use crate::camera::{self, CamCmd, CamEvent, CamInfo};
use fuji::{Choices, Setting, Values};
use crate::plan::{self, format_duration, Preset, ScanPlan, ScanSettings};
use crate::wheel::{self, WheelState};
use crate::worker::{self, Cmd, Event, ScanOutput};

mod connections;
mod control;

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
    /// Camera settings chosen in the app, as (index into `Setting::ALL`,
    /// value). Re-applied on every connect: once the app lets go, the X-T4
    /// falls back to its physical dials.
    camera: Vec<(usize, i32)>,
    /// 3D model options.
    model: crate::model::ModelSettings,
    /// UI zoom (Ctrl +/- or A−/A+ in the top bar).
    zoom: f32,
    /// Old single Wi-Fi field; moved into `camera_conn.ip` on load.
    #[serde(skip_serializing)]
    camera_ip: String,
    /// How to reach the camera (Connections window).
    camera_conn: crate::connection::CameraConn,
    /// How to reach the turntable (Connections window).
    table_conn: crate::connection::TableConn,
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
            camera: Vec::new(),
            model: Default::default(),
            zoom: 1.0,
            camera_ip: String::new(),
            camera_conn: Default::default(),
            table_conn: Default::default(),
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

    fn remember_camera(&mut self, changes: &[(Setting, i32)]) {
        // The mode comes from the camera's dials; never stored or restored.
        for &(s, v) in changes.iter().filter(|(s, _)| *s != Setting::Mode) {
            let idx = Setting::ALL.iter().position(|&x| x == s).expect("in ALL");
            self.camera.retain(|(i, _)| *i != idx);
            self.camera.push((idx, v));
        }
    }

    /// Remembered settings to re-apply, given the camera's current mode
    /// (from its dials). Never the mode itself: on the X-T4, setting the
    /// exposure mode through the SDK makes the camera refuse every release
    /// until it's switched off and on.
    fn camera_settings(&self, mode: Option<i32>) -> Vec<(Setting, i32)> {
        let mut v: Vec<(usize, i32)> = self.camera.iter().copied().filter(|(i, _)| *i < Setting::ALL.len()).collect();
        v.sort_by_key(|(i, _)| *i);
        // Only what the mode lets you set: in A the camera picks the shutter,
        // in S the aperture, in P both (the camera refuses otherwise).
        use fuji::ffi::{XSDK_AE_APERTURE_PRIORITY as A, XSDK_AE_PROGRAM as P, XSDK_AE_SHUTTER_PRIORITY as S};
        v.into_iter()
            .map(|(i, val)| (Setting::ALL[i], val))
            .filter(|(s, _)| match (s, mode) {
                (Setting::Mode, _) => false,
                (Setting::Shutter, Some(A | P)) => false,
                (Setting::Aperture, Some(S | P)) => false,
                _ => true,
            })
            .collect()
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
    /// Table angle where the scan's 0° is.
    origin_deg: f64,
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
    cam_values: Option<Values>,
    /// Shared with the camera thread: how to reach the camera.
    cam_conn: crate::connection::SharedCameraConn,
    /// The Connections window.
    conn_ui: connections::ConnUi,
    /// Camera battery in percent.
    cam_battery: Option<u8>,
    /// Re-apply the saved camera settings when the first values arrive.
    restore_pending: bool,
    cam_choices: Choices,
    /// The turntable has no home sensor: its position is zeroed once per
    /// session on the first connect, so angles start at 0°.
    zeroed: bool,
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
    live_on: bool,
    live_tex: Option<TextureHandle>,
    focusing: bool,
    focus_result: Option<bool>,
    /// Focus frame to draw on the live view (photo coordinates 0..1).
    focus_rect: Option<[f32; 4]>,
    shut_down: bool,
    /// The chameleon emblem for the top bar.
    icon: Option<TextureHandle>,
    /// 3D reconstruction of finished scans.
    model: crate::model::ModelPanel,
    /// Local control API (None if its port was taken).
    api: Option<Receiver<crate::api::Call>>,
    log: VecDeque<String>,
}

impl OrbisApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let saved: Saved = cc.storage.and_then(|s| eframe::get_value(s, eframe::APP_KEY)).unwrap_or_default();
        let saved_root = saved.scans_root.clone();
        setup_fonts(&cc.egui_ctx, saved.zoom);
        let mut saved = saved;
        let mode_idx = Setting::ALL.iter().position(|&s| s == Setting::Mode);
        saved.camera.retain(|(i, _)| Some(*i) != mode_idx);
        if saved.camera_conn.ip.is_empty() && !saved.camera_ip.trim().is_empty() {
            saved.camera_conn.ip = std::mem::take(&mut saved.camera_ip).trim().to_owned();
        }
        let cam_conn: crate::connection::SharedCameraConn =
            std::sync::Arc::new(std::sync::Mutex::new(saved.camera_conn.clone()));
        let (cam_cmd, cam_events) = camera::spawn(cc.egui_ctx.clone(), cam_conn.clone());
        let (cmd, events) = worker::spawn(cc.egui_ctx.clone(), cam_cmd.clone(), saved.table_conn.clone());
        let _ = cmd.send(Cmd::SetGeometry(saved.geometry()));
        let (loaded_tx, loaded_rx) = mpsc::channel();
        let mut app = Self {
            saved,
            cmd,
            events,
            cam_cmd,
            cam_events,
            link: Link::Connecting,
            cam: CamState::Searching,
            cam_values: None,
            cam_conn,
            conn_ui: Default::default(),
            cam_battery: None,
            restore_pending: false,
            cam_choices: Choices::default(),
            zeroed: false,
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
            live_on: false,
            live_tex: None,
            focusing: false,
            focus_result: None,
            focus_rect: None,
            shut_down: false,
            icon: image::load_from_memory(include_bytes!("../../docs/img/icon-64.png")).ok().map(|img| {
                let rgba = img.to_rgba8();
                let size = [rgba.width() as usize, rgba.height() as usize];
                cc.egui_ctx.load_texture(
                    "orbis-icon",
                    ColorImage::from_rgba_unmultiplied(size, rgba.as_raw()),
                    TextureOptions::LINEAR,
                )
            }),
            model: crate::model::ModelPanel::new(&saved_root),
            api: None,
            log: VecDeque::new(),
        };
        match crate::api::spawn(cc.egui_ctx.clone()) {
            Ok(rx) => {
                app.api = Some(rx);
                app.push_log(format!("Control API on http://127.0.0.1:{} (header X-Orbis: 1)", crate::api::PORT));
            }
            Err(e) => app.push_log(format!("Control API off: {e}")),
        }
        app
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

    /// A real scan would fail partway: the X-T4 refuses shots on a weak battery.
    fn battery_too_low(&self) -> bool {
        self.cam_battery.is_some_and(|b| b <= crate::camera::BATTERY_LOW)
    }

    fn camera_ready(&self) -> bool {
        matches!(self.cam, CamState::Connected(_))
    }

    fn start_scan(&mut self, real: bool) {
        // Live view would pause and resume around every photo: switch it
        // off for the scan.
        if real && self.live_on {
            let _ = self.cam_cmd.send(CamCmd::LiveView(false));
        }
        let output = real.then(|| ScanOutput {
            dir: self.saved.new_scan_dir(),
            name: self.saved.scan_name.clone(),
            autofocus: self.saved.autofocus,
        });
        self.send(Cmd::StartScan { plan: self.saved.scan.plan(), output });
    }

    fn handle_events(&mut self, ctx: &egui::Context) {
        for line in self.model.poll() {
            self.push_log(line);
        }
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
                    if !self.zeroed {
                        self.zeroed = true;
                        self.send(Cmd::SetZero);
                    }
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
                        None if self.connected() => {
                            let real = self.camera_ready() && !self.battery_too_low();
                            if self.camera_ready() && !real {
                                self.push_log("Camera battery too low for a scan: dry run only".into());
                            }
                            self.start_scan(real)
                        }
                        None => {}
                    }
                }
                Event::ScanStarted { plan, dir, origin_deg } => {
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
                        origin_deg,
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
                        if let Some(dir) = s.dir.clone() {
                            self.model.scan_finished(dir);
                        }
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
                    self.cam_battery = None;
                    if !matches!(self.cam, CamState::Problem(_)) {
                        self.cam = CamState::Searching;
                    }
                }
                CamEvent::Connected(info) => {
                    self.cam = CamState::Connected(info);
                    // Restore once the first values (and so the mode) are in.
                    self.restore_pending = true;
                }
                CamEvent::Problem(why) => {
                    self.cam = CamState::Problem(why);
                    self.cam_values = None;
                }
                CamEvent::Battery(b) => self.cam_battery = b,
                CamEvent::Settings { values, choices } => {
                    if std::mem::take(&mut self.restore_pending) {
                        let restore = self.saved.camera_settings(values.get(Setting::Mode));
                        if !restore.is_empty() {
                            self.push_log(format!("Restoring {} camera settings", restore.len()));
                            let _ = self.cam_cmd.send(CamCmd::Set(restore));
                        }
                    }
                    self.cam_values = Some(values);
                    if let Some(c) = choices {
                        self.cam_choices = c;
                    }
                }
                CamEvent::Photo { path, preview, thumb } => {
                    if self.test_shot_busy {
                        let mb = std::fs::metadata(&path).map(|m| m.len() as f64 / 1e6).unwrap_or(0.0);
                        self.push_log(format!(
                            "Test shot saved: {} ({mb:.1} MB)",
                            path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default()
                        ));
                    }
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
                CamEvent::CaptureFailed => self.test_shot_busy = false,
                CamEvent::Live(on) => {
                    self.live_on = on;
                    if !on {
                        self.live_tex = None;
                    }
                }
                CamEvent::LiveFrame(img) => match &mut self.live_tex {
                    // Reuse the texture: a new upload each frame, no new handle.
                    Some(tex) => tex.set(img, TextureOptions::LINEAR),
                    None => self.live_tex = Some(ctx.load_texture("live", img, TextureOptions::LINEAR)),
                },
                CamEvent::Focused(ok) => {
                    self.focusing = false;
                    self.focus_result = Some(ok);
                }
                CamEvent::FocusPoint(r) => self.focus_rect = Some(r),
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
            if let Some(icon) = &self.icon {
                ui.add(egui::Image::new((icon.id(), egui::vec2(24.0, 24.0))));
            }
            ui.label(RichText::new("Orbis").size(20.0).strong());
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
            if ui.small_button("🔌").on_hover_text("Connections: USB / Wi-Fi for camera and turntable").clicked() {
                self.conn_ui.open = !self.conn_ui.open;
            }
            ui.separator();
            let (color, text, hover) = match &self.cam {
                CamState::Searching => (AMBER, "Camera: searching…".to_owned(), None),
                CamState::Connected(i) => (
                    GREEN,
                    format!(
                        "Camera: {}{} ({})",
                        i.model,
                        if i.lens.is_empty() { String::new() } else { format!(" · {}", i.lens) },
                        i.link
                    ),
                    Some(format!("Firmware {}", i.firmware)),
                ),
                CamState::Problem(why) => (RED, "Camera: not ready".to_owned(), Some(why.clone())),
            };
            ui.label(RichText::new("●").color(color));
            let resp = ui.label(text);
            if let Some(h) = hover {
                resp.on_hover_text(h);
            }
            if let (CamState::Connected(_), Some(b)) = (&self.cam, self.cam_battery) {
                let color = if b <= crate::camera::BATTERY_LOW { RED } else if b <= 40 { AMBER } else { ui.visuals().text_color() };
                ui.label(RichText::new(format!("🔋 {b} %")).color(color))
                    .on_hover_text("Camera battery. Below 20 % the X-T4 refuses shots and then switches off.");
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let z = ui.ctx().zoom_factor();
                if ui.button("A+").on_hover_text("Larger text (Ctrl +)").clicked() {
                    ui.ctx().set_zoom_factor((z + 0.1).min(2.5));
                }
                ui.label(RichText::new(format!("{:.0} %", z * 100.0)).weak());
                if ui.button("A−").on_hover_text("Smaller text (Ctrl −)").clicked() {
                    ui.ctx().set_zoom_factor((z - 0.1).max(0.6));
                }
            });
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
                let battery_low = self.battery_too_low();
                let ready = self.connected() && self.camera_ready() && !battery_low;
                let btn = egui::Button::new(RichText::new("▶  Start scan").size(16.0).strong())
                    .min_size(egui::vec2(ui.available_width(), 38.0));
                let hint = if battery_low {
                    "Camera battery too low for a scan: charge or swap it (the camera would refuse shots and switch off)"
                } else if ready {
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
            // Button first, then the path cut to the rest of the width: a long
            // scan name must not widen the panel (it then slides under the
            // preview and its left edge disappears).
            if ui.small_button("📂").on_hover_text("Open folder").clicked() {
                let _ = std::fs::create_dir_all(&folder);
                let _ = std::process::Command::new("explorer").arg(&folder).spawn();
            }
            let path = folder.display().to_string();
            ui.add(egui::Label::new(RichText::new(&path).small().weak()).truncate())
                .on_hover_text(format!("Where photos are saved:\n{path}"));
        });
    }

    fn camera_panel(&mut self, ui: &mut Ui) {
        ui.heading("Camera");
        ui.add_space(4.0);
        match (&self.cam, &self.cam_values) {
            (CamState::Connected(_), Some(values)) => {
                let editable = self.scan.is_none();
                let mut changes = Vec::new();
                egui::Grid::new("camera-settings").num_columns(2).spacing([10.0, 4.0]).show(ui, |ui| {
                    for s in Setting::ALL {
                        ui.label(s.label());
                        let current = values.get(s);
                        let text = current.map(|v| s.value_name(v)).unwrap_or_else(|| "—".into());
                        let choices = self.cam_choices.get(s);
                        if s == Setting::Mode {
                            // Read-only: the X-T4 takes the mode from its dials,
                            // and setting it via USB blocks the shutter.
                            ui.horizontal(|ui| {
                                ui.strong(text).on_hover_text(
                                    "Set on the camera: M = shutter dial on T (or a speed) and aperture ring on a value (neither on A).\n\
                                     Orbis doesn't change the mode: the X-T4 then refuses to shoot until it's switched off and on.",
                                );
                                // While Orbis is in control (PC priority) the X-T4
                                // ignores its dials; a reconnect reads them again.
                                if ui
                                    .add_enabled(self.scan.is_none(), egui::Button::new("↻ dials").small())
                                    .on_hover_text(
                                        "Changed a dial or the aperture ring? While Orbis controls the camera, the X-T4 \
                                         ignores them. This reconnects the camera so it reads them again.",
                                    )
                                    .clicked()
                                {
                                    let _ = self.cam_cmd.send(CamCmd::Reconnect);
                                }
                            });
                        } else if let Some(why) = locked_by_mode(s, values.get(Setting::Mode)) {
                            ui.strong(text).on_hover_text(why);
                        } else if choices.is_empty() || !editable {
                            ui.strong(text);
                        } else {
                            egui::ComboBox::from_id_salt(s.label())
                                .selected_text(text)
                                .width(220.0)
                                .height(420.0)
                                .show_ui(
                                ui,
                                |ui| {
                                    for &v in choices {
                                        if ui.selectable_label(current == Some(v), s.value_name(v)).clicked()
                                            && current != Some(v)
                                        {
                                            changes.push((s, v));
                                        }
                                    }
                                },
                            );
                        }
                        ui.end_row();
                    }
                });
                if !changes.is_empty() {
                    self.saved.remember_camera(&changes);
                    let _ = self.cam_cmd.send(CamCmd::Set(changes));
                }

                // Hints for consistent photogrammetry photos.
                let mut tips = Vec::new();
                if values.get(Setting::Mode) != Some(fuji::ffi::XSDK_AE_OFF) {
                    tips.push("Use M for identical exposure: shutter dial on a speed, aperture ring on a value (not A).");
                }
                if values.get(Setting::Iso).is_some_and(|v| v < 0) {
                    tips.push("Auto ISO changes brightness between shots: pick a fixed ISO.");
                }
                if values.get(Setting::WhiteBalance).is_some_and(|v| matches!(v, 0x0002 | 0x8020 | 0x8021)) {
                    tips.push("Auto white balance shifts colours: pick a fixed one.");
                }
                if values.get(Setting::FilmSimulation).is_some_and(|v| (6..=10).contains(&v) || (12..=15).contains(&v)) {
                    tips.push("Black & white film simulation: textures will have no colour.");
                }
                if values.get(Setting::Quality) == Some(fuji::ffi::SDK_IMAGEQUALITY_RAW) {
                    tips.push("RAW only gives no preview: choose RAW + JPEG.");
                }
                if values.get(Setting::Focus) == Some(fuji::ffi::SDK_FOCUS_AFC) {
                    tips.push("AF-C refocuses constantly: use AF-S, or focus once and switch to MF.");
                }
                for t in &tips {
                    ui.label(RichText::new(format!("💡 {t}")).small().color(AMBER));
                }
                if ui
                    .add_enabled(editable, egui::Button::new("✨ Photogrammetry settings"))
                    .on_hover_text(
                        "ISO 160, daylight white balance, Provia colours, RAW + JPEG Fine.\n\
                         Mode, aperture and shutter stay as they are: set M on the camera's dials for your light.",
                    )
                    .clicked()
                {
                    let preset = fuji::settings::photogrammetry_preset();
                    self.saved.remember_camera(&preset);
                    let _ = self.cam_cmd.send(CamCmd::Set(preset));
                }
            }
            (CamState::Connected(_), None) => {
                ui.label(RichText::new("reading settings…").weak());
            }
            _ => {
                ui.label(RichText::new("Not connected. See 🔌 Connections (USB or Wi-Fi).").weak());
            }
        }
        if ui.button("🔌 Connections…").on_hover_text("USB or Wi-Fi, camera search, turntable address/port").clicked() {
            self.conn_ui.open = true;
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
        // 0–360°: after a 2-ring scan the table is "at 720°", which is 0°.
        let pos = self.table_deg.map(|d| format!("{:.1}°", d.rem_euclid(360.0))).unwrap_or_else(|| "—".into());
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
        // Dots are drawn in scan angles (0° at the top), so the needle must
        // be relative to where the scan started, not the table's own zero.
        let origin = view.map(|s| s.origin_deg).unwrap_or(0.0);

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
                    table_degrees: self.table_deg.map(|d| d - origin),
                    center_title: title,
                    center_sub: sub,
                },
            );
            ui.add_space(12.0);
            ui.vertical(|ui| {
                self.preview_toolbar(ui);
                self.preview_area(ui, egui::vec2(side * 1.5, side - 30.0));
            });
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

    /// Live view / focus buttons above the preview.
    fn preview_toolbar(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            let ready = self.camera_ready();
            let label = if self.live_on { "■ Stop live view" } else { "◉ Live view" };
            if ui
                .add_enabled(ready && self.scan.is_none(), egui::Button::new(label).selected(self.live_on))
                .on_hover_text("See through the camera to frame the object. Click into the picture to focus there.")
                .clicked()
            {
                let _ = self.cam_cmd.send(CamCmd::LiveView(!self.live_on));
            }
            let focus = egui::Button::new(if self.focusing { "🎯 Focusing…" } else { "🎯 Focus" });
            if ui
                .add_enabled(ready && self.scan.is_none() && !self.focusing, focus)
                .on_hover_text("Autofocus once now, also in MF. Then shoot the scan in MF so focus stays put.")
                .clicked()
            {
                self.focusing = true;
                let _ = self.cam_cmd.send(CamCmd::FocusNow);
            }
            if let Some(ok) = self.focus_result {
                if ok {
                    ui.label(RichText::new("focus locked").color(GREEN).small());
                } else {
                    ui.label(RichText::new("couldn't focus").color(AMBER).small());
                }
            }
        });
    }

    fn preview_area(&mut self, ui: &mut Ui, size: egui::Vec2) {
        let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
        let v = ui.visuals();
        ui.painter().rect_filled(rect, 8.0, v.extreme_bg_color);

        // Live view takes over the preview while it runs.
        if let (true, Some(tex)) = (self.live_on, &self.live_tex) {
            let img = tex.size_vec2();
            let scale = (rect.width() / img.x).min(rect.height() / img.y);
            let r = egui::Rect::from_center_size(rect.center(), img * scale);
            let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
            ui.painter().image(tex.id(), r, uv, Color32::WHITE);

            // The camera sends 4:3 frames with the 3:2 photo letterboxed
            // inside; focus coordinates refer to the photo area.
            let photo = if r.width() / r.height() < 1.5 {
                egui::Rect::from_center_size(r.center(), egui::vec2(r.width(), r.width() / 1.5))
            } else {
                r
            };
            let hover = resp.hover_pos().filter(|p| photo.contains(*p));
            if hover.is_some() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
            }
            if resp.clicked() {
                if let Some(p) = resp.interact_pointer_pos().filter(|p| photo.contains(*p)) {
                    let x = (p.x - photo.left()) / photo.width();
                    let y = (p.y - photo.top()) / photo.height();
                    self.focusing = true;
                    self.focus_result = None;
                    let _ = self.cam_cmd.send(CamCmd::FocusAt { x, y });
                }
            }
            if let Some([fx, fy, fw, fh]) = self.focus_rect {
                let fr = egui::Rect::from_min_size(
                    egui::pos2(photo.left() + fx * photo.width(), photo.top() + fy * photo.height()),
                    egui::vec2(fw * photo.width(), fh * photo.height()),
                );
                let color = match (self.focusing, self.focus_result) {
                    (true, _) => Color32::WHITE,
                    (false, Some(true)) => GREEN,
                    (false, Some(false)) => RED,
                    (false, None) => Color32::WHITE,
                };
                ui.painter().rect_stroke(fr, 2.0, egui::Stroke::new(2.0, color), egui::StrokeKind::Middle);
            }
            let badge = egui::Rect::from_min_size(rect.left_top() + egui::vec2(10.0, 10.0), egui::vec2(58.0, 22.0));
            ui.painter().rect_filled(badge, 4.0, Color32::from_black_alpha(160));
            ui.painter().text(
                badge.center(),
                egui::Align2::CENTER_CENTER,
                "● LIVE",
                egui::FontId::proportional(13.0),
                RED,
            );
            // Rule-of-thirds grid helps centring the object on the plate.
            let stroke = egui::Stroke::new(1.0, Color32::from_white_alpha(50));
            for i in 1..3 {
                let x = r.left() + r.width() * i as f32 / 3.0;
                let y = r.top() + r.height() * i as f32 / 3.0;
                ui.painter().line_segment([egui::pos2(x, r.top()), egui::pos2(x, r.bottom())], stroke);
                ui.painter().line_segment([egui::pos2(r.left(), y), egui::pos2(r.right(), y)], stroke);
            }
            return;
        }

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
        // Closing: stop live view and hand the camera back before the
        // process ends, or the camera stays busy until it's power-cycled.
        if ctx.input(|i| i.viewport().close_requested()) && !self.shut_down {
            self.shut_down = true;
            self.model.shutdown();
            let (done_tx, done_rx) = mpsc::channel();
            if self.cam_cmd.send(CamCmd::Shutdown(done_tx)).is_ok() {
                let _ = done_rx.recv_timeout(Duration::from_secs(3));
            }
        }
        self.handle_events(&ctx);
        let calls: Vec<crate::api::Call> = self.api.as_ref().map(|rx| rx.try_iter().collect()).unwrap_or_default();
        for call in calls {
            self.handle_api(&ctx, call);
        }
        self.saved.zoom = ctx.zoom_factor();

        egui::Panel::top("top").show(ui, |ui| {
            ui.add_space(4.0);
            self.top_bar(ui);
            ui.add_space(2.0);
        });
        egui::Panel::bottom("log").resizable(true).default_size(150.0).show(ui, |ui| {
            egui::ScrollArea::vertical().stick_to_bottom(true).auto_shrink([false, false]).show(ui, |ui| {
                for line in &self.log {
                    ui.label(RichText::new(line).monospace());
                }
            });
        });
        egui::Panel::right("controls").resizable(false).exact_size(420.0).show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.add_space(6.0);
                self.scan_panel(ui);
                ui.add_space(10.0);
                ui.separator();
                let scanning = self.scan.is_some();
                self.model.ui(ui, &mut self.saved.model, &self.saved.scans_root, scanning);
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
        self.connections_window(&ctx);
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

/// Bigger text than egui's defaults (which are tuned for dense tools), and
/// the saved zoom on top.
fn setup_fonts(ctx: &egui::Context, zoom: f32) {
    use egui::{FontId, TextStyle};
    ctx.all_styles_mut(|s| {
        s.text_styles = [
            (TextStyle::Heading, FontId::proportional(22.0)),
            (TextStyle::Body, FontId::proportional(16.0)),
            (TextStyle::Button, FontId::proportional(16.0)),
            (TextStyle::Monospace, FontId::monospace(14.5)),
            (TextStyle::Small, FontId::proportional(13.5)),
        ]
        .into();
        s.spacing.interact_size.y = 26.0;
        s.spacing.item_spacing = egui::vec2(8.0, 6.0);
        s.spacing.button_padding = egui::vec2(8.0, 4.0);
    });
    ctx.set_zoom_factor(zoom.clamp(0.6, 2.5));
}

/// Settings the camera picks itself in the current mode (dials).
fn locked_by_mode(s: Setting, mode: Option<i32>) -> Option<&'static str> {
    use fuji::ffi::{XSDK_AE_APERTURE_PRIORITY as A, XSDK_AE_PROGRAM as P, XSDK_AE_SHUTTER_PRIORITY as S};
    match (s, mode) {
        (Setting::Aperture, Some(P | S)) => Some("The camera picks the aperture in this mode. Turn the aperture ring off A to set it."),
        (Setting::Shutter, Some(P | A)) => Some("The camera picks the shutter speed in this mode. Turn the shutter dial off A to set it."),
        _ => None,
    }
}
