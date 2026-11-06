//! The Orbis window.
//!
//! ```text
//! ┌ Orbis ─ ● Turntable 192.168.1.50 ─ ○ Camera ───────────────────────┐
//! │                                       │ SCAN  presets, rings, settle   │
//! │   angle wheel        photo preview    │       estimate, Start/Stop     │
//! │                                       │ TURNTABLE  position, jog, speed│
//! ├───────────────────────────────────────┴────────────────────────────────┤
//! │ log                                                                    │
//! └────────────────────────────────────────────────────────────────────────┘
//! ```

use std::collections::VecDeque;
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

use egui::{Color32, RichText, Ui};
use rig::Geometry;
use serde::{Deserialize, Serialize};

use crate::plan::{self, format_duration, Preset, ScanPlan, ScanSettings};
use crate::wheel::{self, WheelState};
use crate::worker::{self, Cmd, Event};

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
}

impl Default for Saved {
    fn default() -> Self {
        Self { scan: ScanSettings::default(), motor_steps: 200, microsteps: 16, gear_ratio: 1.0, speed_deg_s: 90.0 }
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
}

enum Link {
    Connecting,
    Connected(String),
    Down(String),
}

struct ScanView {
    plan: ScanPlan,
    done: Vec<u32>,
    current: Option<(u32, u32)>,
    paused: bool,
    waiting_ring: Option<u32>,
    started: Instant,
}

pub struct OrbisApp {
    saved: Saved,
    cmd: Sender<Cmd>,
    events: Receiver<Event>,
    link: Link,
    table_deg: Option<f64>,
    moving: bool,
    enabled: bool,
    scan: Option<ScanView>,
    /// Shown after a scan ends, until the next one starts.
    last_scan: Option<ScanView>,
    log: VecDeque<String>,
}

impl OrbisApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let saved: Saved = cc.storage.and_then(|s| eframe::get_value(s, eframe::APP_KEY)).unwrap_or_default();
        let (cmd, events) = worker::spawn(cc.egui_ctx.clone());
        let _ = cmd.send(Cmd::SetGeometry(saved.geometry()));
        Self {
            saved,
            cmd,
            events,
            link: Link::Connecting,
            table_deg: None,
            moving: false,
            enabled: true,
            scan: None,
            last_scan: None,
            log: VecDeque::new(),
        }
    }

    fn send(&self, c: Cmd) {
        let _ = self.cmd.send(c);
    }

    fn push_log(&mut self, msg: String) {
        if self.log.len() >= 200 {
            self.log.pop_front();
        }
        self.log.push_back(msg);
    }

    fn connected(&self) -> bool {
        matches!(self.link, Link::Connected(_))
    }

    fn start_scan(&mut self) {
        self.send(Cmd::StartScan(self.saved.scan.plan()));
    }

    fn handle_events(&mut self) {
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
                        None if self.connected() => self.start_scan(),
                        None => {}
                    }
                }
                Event::ScanStarted(plan) => {
                    self.last_scan = None;
                    self.scan = Some(ScanView {
                        plan,
                        done: vec![0; plan.rings as usize],
                        current: Some((0, 0)),
                        paused: false,
                        waiting_ring: None,
                        started: Instant::now(),
                    });
                }
                Event::Shot { ring, index, angle } => {
                    self.push_log(format!("ring {} · position {:>3} · {angle:6.1}°", ring + 1, index + 1));
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
    }

    fn continue_ring(&mut self) {
        if let Some(s) = &mut self.scan {
            s.waiting_ring = None;
        }
        self.send(Cmd::ContinueRing);
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
            ui.label(RichText::new("●").color(ui.visuals().weak_text_color()));
            ui.label(RichText::new("Camera: coming next").weak())
                .on_hover_text("Fujifilm X-T4 over USB is the next milestone. Scans run as dry runs until then.");
        });
    }

    fn scan_panel(&mut self, ui: &mut Ui) {
        ui.heading("Scan");
        ui.add_space(4.0);
        let scanning = self.scan.is_some();

        ui.add_enabled_ui(!scanning, |ui| {
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
            ui.label("Time (with camera)");
            ui.strong(format_duration(p.estimate(&geo, max_speed, accel, plan::CAMERA_SHOT_TIME)));
            ui.end_row();
            ui.label("Disk (JPEG)");
            ui.strong(format!("≈ {:.1} GB", p.disk_mb() / 1024.0));
            ui.end_row();
        });

        ui.add_space(10.0);
        match &self.scan {
            None => {
                let btn = egui::Button::new(RichText::new("▶  Start dry run").size(16.0))
                    .min_size(egui::vec2(ui.available_width(), 36.0));
                if ui
                    .add_enabled(self.connected(), btn)
                    .on_hover_text("Turns the table through the whole scan without taking photos")
                    .clicked()
                {
                    self.start_scan();
                }
            }
            Some(s) => {
                let paused = s.paused;
                let done: u32 = s.done.iter().sum();
                let total = s.plan.total_shots();
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

    fn center(&mut self, ui: &mut Ui) {
        // Wheel (side × side) + gap + preview (1.2·side wide) must fit the
        // width; the wheel plus the thumbnail strip must fit the height.
        let avail = ui.available_size();
        let side = ((avail.x - 24.0) / 2.2).min(avail.y - 110.0).max(160.0);
        let view = self.scan.as_ref().or(self.last_scan.as_ref());
        let plan = view.map(|s| s.plan).unwrap_or_else(|| self.saved.scan.plan());
        let empty = vec![0; plan.rings as usize];
        let done = view.map(|s| s.done.as_slice()).unwrap_or(&empty);

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
                    done,
                    current: self.scan.as_ref().and_then(|s| s.current),
                    table_degrees: self.table_deg,
                    center_title: title,
                    center_sub: sub,
                },
            );
            ui.add_space(12.0);
            // Photo preview placeholder.
            let (rect, _) = ui.allocate_exact_size(egui::vec2(side * 1.2, side * 0.8), egui::Sense::hover());
            let v = ui.visuals();
            ui.painter().rect_filled(rect, 8.0, v.extreme_bg_color);
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
                "Photo preview arrives with camera support",
                egui::FontId::proportional(14.0),
                v.weak_text_color(),
            );
        });

        // Thumbnail strip placeholder.
        ui.add_space(10.0);
        ui.label(RichText::new("Photos").strong());
        let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 72.0), egui::Sense::hover());
        let v = ui.visuals();
        let total = plan.total_shots().min(60);
        let w = 54.0;
        for i in 0..total {
            let x = rect.left() + i as f32 * (w + 6.0);
            if x + w > rect.right() {
                break;
            }
            let r = egui::Rect::from_min_size(egui::pos2(x, rect.top() + 8.0), egui::vec2(w, 54.0));
            ui.painter().rect_filled(r, 4.0, v.faint_bg_color);
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
        self.handle_events();
        let ctx = ui.ctx().clone();

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
        egui::Panel::right("controls").resizable(false).exact_size(340.0).show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.add_space(6.0);
                self.scan_panel(ui);
                ui.add_space(10.0);
                ui.separator();
                self.turntable_panel(ui);
            });
        });
        egui::CentralPanel::default().show(ui, |ui| self.center(ui));

        self.ring_dialog(&ctx);
        // Keep the elapsed timer and status fresh.
        ctx.request_repaint_after(Duration::from_millis(250));
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, eframe::APP_KEY, &self.saved);
    }
}
