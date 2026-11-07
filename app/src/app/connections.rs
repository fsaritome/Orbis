//! The Connections window: how to reach the camera (USB / Wi-Fi, with a
//! network search) and the turntable (network search, fixed address, or a
//! USB serial port).

use std::sync::mpsc;

use egui::{RichText, Ui};

use super::{CamState, Link, OrbisApp, AMBER, GREEN, RED};
use crate::camera::CamCmd;
use crate::connection::{CameraMode, TableMode};
use crate::worker::Cmd;

/// Results of the searches started from the window.
#[derive(Default)]
pub struct ConnUi {
    pub open: bool,
    cam_search: Option<mpsc::Receiver<Result<Vec<fuji::FoundCamera>, String>>>,
    cam_found: Option<Result<Vec<fuji::FoundCamera>, String>>,
    table_search: Option<mpsc::Receiver<Vec<String>>>,
    table_found: Option<Vec<String>>,
    ports: Option<Vec<(String, String)>>,
}

impl OrbisApp {
    /// Camera settings changed: share them with the camera thread and
    /// reconnect.
    pub(super) fn apply_camera_conn(&mut self) {
        if let Ok(mut c) = self.cam_conn.lock() {
            *c = self.saved.camera_conn.clone();
        }
        let _ = self.cam_cmd.send(CamCmd::Reconnect);
    }

    pub(super) fn apply_table_conn(&mut self) {
        self.link = Link::Connecting;
        self.send(Cmd::SetConnection(self.saved.table_conn.clone()));
    }

    /// Starts a camera search (USB + Wi-Fi) in the camera thread.
    pub(super) fn start_camera_search(&mut self) {
        let (tx, rx) = mpsc::channel();
        let range = Some(self.saved.camera_conn.ip.trim().to_owned()).filter(|s| !s.is_empty() && s.matches('.').count() < 3);
        let _ = self.cam_cmd.send(CamCmd::Search { range, reply: tx });
        self.conn_ui.cam_search = Some(rx);
        self.conn_ui.cam_found = None;
    }

    pub(super) fn start_table_search(&mut self, ctx: &egui::Context) {
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let found = rig::discover(std::time::Duration::from_millis(2000))
                .unwrap_or_default()
                .into_iter()
                .map(|f| f.addr.ip().to_string())
                .collect();
            let _ = tx.send(found);
            ctx.request_repaint();
        });
        self.conn_ui.table_search = Some(rx);
        self.conn_ui.table_found = None;
    }

    pub(super) fn connections_window(&mut self, ctx: &egui::Context) {
        if let Some(rx) = &self.conn_ui.cam_search {
            if let Ok(r) = rx.try_recv() {
                self.conn_ui.cam_found = Some(r);
                self.conn_ui.cam_search = None;
            }
        }
        if let Some(rx) = &self.conn_ui.table_search {
            if let Ok(r) = rx.try_recv() {
                self.conn_ui.table_found = Some(r);
                self.conn_ui.table_search = None;
            }
        }
        if !self.conn_ui.open {
            return;
        }
        let mut open = true;
        egui::Window::new("🔌 Connections")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(460.0)
            .show(ctx, |ui| {
                self.camera_section(ui);
                ui.add_space(8.0);
                ui.separator();
                self.table_section(ui, ctx);
            });
        self.conn_ui.open = open;
    }

    fn camera_section(&mut self, ui: &mut Ui) {
        ui.heading("Camera");
        let (color, state) = match &self.cam {
            CamState::Connected(i) => (GREEN, format!("connected: {} over {}", i.model, i.link)),
            CamState::Searching => (AMBER, "searching…".to_owned()),
            CamState::Problem(why) => (RED, why.clone()),
        };
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("●").color(color));
            ui.label(RichText::new(state).small());
        });
        ui.add_space(4.0);

        let mut changed = false;
        for m in CameraMode::ALL {
            changed |= ui.radio_value(&mut self.saved.camera_conn.mode, m, m.label()).on_hover_text(m.hint()).changed();
        }
        ui.label(RichText::new(self.saved.camera_conn.mode.hint()).weak().small());
        ui.add_enabled_ui(self.saved.camera_conn.mode != CameraMode::Usb, |ui| {
            ui.horizontal(|ui| {
                ui.label("Wi-Fi address");
                let r = ui.add(
                    egui::TextEdit::singleline(&mut self.saved.camera_conn.ip)
                        .hint_text("search the network")
                        .desired_width(160.0),
                );
                changed |= r.lost_focus() && r.changed() || r.lost_focus();
                let searching = self.conn_ui.cam_search.is_some();
                if ui
                    .add_enabled(!searching, egui::Button::new(if searching { "Searching…" } else { "🔍 Search" }))
                    .on_hover_text("Lists cameras on USB and on the local network (camera in WIRELESS TETHER SHOOTING FIXED). Takes up to ~30 s.\nTip: enter e.g. 192.168.1 to scan that whole range instead.")
                    .clicked()
                {
                    self.start_camera_search();
                }
            });
        });
        match &self.conn_ui.cam_found {
            None => {}
            Some(Err(e)) => {
                ui.label(RichText::new(format!("Search failed: {e}")).color(AMBER).small());
            }
            Some(Ok(v)) if v.is_empty() => {
                ui.label(
                    RichText::new("No camera found. Is it switched on and in a tether mode (USB TETHER SHOOTING AUTO or WIRELESS TETHER SHOOTING FIXED, lamp blinking orange)?")
                        .color(AMBER)
                        .small(),
                );
            }
            Some(Ok(v)) => {
                let mut pick = None;
                for c in v.clone() {
                    ui.horizontal(|ui| {
                        let wifi = !c.ip.is_empty();
                        ui.label(format!("📷 {}  ·  {}", c.product, if wifi { format!("Wi-Fi {}", c.ip) } else { "USB".into() }));
                        if ui.small_button("Use").clicked() {
                            pick = Some(c);
                        }
                    });
                }
                if let Some(c) = pick {
                    if c.ip.is_empty() {
                        self.saved.camera_conn.mode = CameraMode::Usb;
                    } else {
                        self.saved.camera_conn.ip = c.ip;
                        if self.saved.camera_conn.mode == CameraMode::Usb {
                            self.saved.camera_conn.mode = CameraMode::Auto;
                        }
                    }
                    changed = true;
                }
            }
        }
        ui.horizontal(|ui| {
            if ui.button("⟳ Reconnect camera").clicked() {
                changed = true;
            }
        });
        if changed {
            self.apply_camera_conn();
        }
    }

    fn table_section(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        ui.heading("Turntable");
        let (color, state) = match &self.link {
            Link::Connected(how) => (GREEN, format!("connected via {how}")),
            Link::Connecting => (AMBER, "connecting…".to_owned()),
            Link::Down(why) => (RED, why.clone()),
        };
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("●").color(color));
            ui.label(RichText::new(state).small());
        });
        ui.add_space(4.0);
        let scanning = self.scan.is_some();
        let mut changed = false;
        ui.add_enabled_ui(!scanning, |ui| {
            for m in TableMode::ALL {
                changed |= ui.radio_value(&mut self.saved.table_conn.mode, m, m.label()).on_hover_text(m.hint()).changed();
            }
            ui.label(RichText::new(self.saved.table_conn.mode.hint()).weak().small());
            match self.saved.table_conn.mode {
                TableMode::Auto => {}
                TableMode::Wifi => {
                    ui.horizontal(|ui| {
                        ui.label("Address");
                        let r = ui.add(
                            egui::TextEdit::singleline(&mut self.saved.table_conn.host)
                                .hint_text("search the network")
                                .desired_width(160.0),
                        );
                        changed |= r.lost_focus();
                        let searching = self.conn_ui.table_search.is_some();
                        if ui.add_enabled(!searching, egui::Button::new(if searching { "Searching…" } else { "🔍 Search" })).clicked() {
                            self.start_table_search(ctx);
                        }
                    });
                    if let Some(found) = self.conn_ui.table_found.clone() {
                        if found.is_empty() {
                            ui.label(RichText::new("No turntable answered. Powered, and on the same Wi-Fi?").color(AMBER).small());
                        }
                        for ip in found {
                            ui.horizontal(|ui| {
                                ui.label(format!("🔄 Orbis turntable  ·  {ip}"));
                                if ui.small_button("Use").clicked() {
                                    self.saved.table_conn.host = ip.clone();
                                    changed = true;
                                }
                            });
                        }
                    }
                }
                TableMode::Usb => {
                    if self.conn_ui.ports.is_none() {
                        self.conn_ui.ports = Some(rig::list_serial_ports().unwrap_or_default());
                    }
                    let ports = self.conn_ui.ports.clone().unwrap_or_default();
                    ui.horizontal(|ui| {
                        ui.label("Port");
                        let current = if self.saved.table_conn.port.is_empty() { "find the ESP32".to_owned() } else { self.saved.table_conn.port.clone() };
                        egui::ComboBox::from_id_salt("table-port").selected_text(current).width(260.0).show_ui(ui, |ui| {
                            changed |= ui.selectable_value(&mut self.saved.table_conn.port, String::new(), "find the ESP32 automatically").changed();
                            for (name, hint) in &ports {
                                changed |= ui.selectable_value(&mut self.saved.table_conn.port, name.clone(), format!("{name}  {hint}")).changed();
                            }
                        });
                        if ui.small_button("⟳").on_hover_text("List ports again").clicked() {
                            self.conn_ui.ports = None;
                        }
                    });
                }
            }
            if ui.button("⟳ Reconnect turntable").clicked() {
                changed = true;
            }
        });
        if scanning {
            ui.label(RichText::new("Not during a scan.").weak().small());
        }
        if changed {
            self.apply_table_conn();
        }
    }
}
