//! The "3D model" panel: turns a scan folder into a textured mesh with the
//! `recon` crate (COLMAP + OpenMVS), in a background thread.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{Color32, RichText, Ui};
use recon::{Options, Progress, Quality};
use serde::{Deserialize, Serialize};

const GREEN: Color32 = Color32::from_rgb(0x3D, 0xC2, 0x6B);
const AMBER: Color32 = Color32::from_rgb(0xF2, 0xB1, 0x34);

/// Saved between sessions (part of the app's settings).
#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct ModelSettings {
    /// Index into `Quality::ALL`.
    quality: usize,
    masks: bool,
}

impl Default for ModelSettings {
    fn default() -> Self {
        Self { quality: 1, masks: true }
    }
}

impl ModelSettings {
    fn quality(&self) -> Quality {
        Quality::ALL.get(self.quality).copied().unwrap_or_default()
    }
}

enum Msg {
    Stage { index: usize, total: usize, name: &'static str },
    Line(String),
    Done(Result<PathBuf, String>),
}

struct Job {
    dir: PathBuf,
    rx: Receiver<Msg>,
    cancel: Arc<AtomicBool>,
    started: Instant,
    stage: Option<(usize, usize, &'static str)>,
    last_line: String,
}

pub struct ModelPanel {
    /// Scan folder to reconstruct.
    target: Option<PathBuf>,
    job: Option<Job>,
    /// Last result: model path or error, with the scan it belongs to.
    result: Option<(PathBuf, Result<PathBuf, String>, Duration)>,
    tools_installed: bool,
}

/// A scan folder for the picker.
struct ScanEntry {
    dir: PathBuf,
    photos: usize,
    has_model: bool,
}

fn jpeg_count(dir: &Path) -> usize {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| e.path().extension().and_then(|x| x.to_str()).is_some_and(|x| x.eq_ignore_ascii_case("jpg")))
                .count()
        })
        .unwrap_or(0)
}

fn model_file(scan: &Path) -> PathBuf {
    scan.join(recon::pipeline::MODEL_DIR).join(recon::pipeline::MODEL_FILE)
}

/// Scan folders under `root`, newest first.
fn list_scans(root: &Path) -> Vec<ScanEntry> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(root)
        .map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.is_dir()).collect())
        .unwrap_or_default();
    dirs.sort();
    dirs.reverse();
    dirs.into_iter()
        .map(|dir| ScanEntry { photos: jpeg_count(&dir), has_model: model_file(&dir).is_file(), dir })
        .filter(|s| s.photos > 0)
        .collect()
}

fn folder_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.display().to_string())
}

fn open(path: &Path) {
    let _ = std::process::Command::new("explorer").arg(path).spawn();
}

impl ModelPanel {
    pub fn new(scans_root: &Path) -> Self {
        Self {
            target: list_scans(scans_root).into_iter().next().map(|s| s.dir),
            job: None,
            result: None,
            tools_installed: recon::tools::locate().is_some(),
        }
    }

    /// A scan just finished: offer it.
    pub fn scan_finished(&mut self, dir: PathBuf) {
        if self.job.is_none() {
            self.target = Some(dir);
        }
    }

    /// App closing: cancel a running build and wait (briefly) until the
    /// tool process is killed, so no COLMAP/OpenMVS keeps running orphaned.
    pub fn shutdown(&mut self) {
        if let Some(job) = self.job.take() {
            job.cancel.store(true, Ordering::Relaxed);
            let until = Instant::now() + Duration::from_secs(3);
            while let Some(left) = until.checked_duration_since(Instant::now()) {
                match job.rx.recv_timeout(left) {
                    Ok(Msg::Done(_)) | Err(_) => break,
                    Ok(_) => {}
                }
            }
        }
    }

    fn start(&mut self, ctx: &egui::Context, dir: PathBuf, opts: Options) {
        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let stop = cancel.clone();
        let ctx = ctx.clone();
        let scan = dir.clone();
        std::thread::Builder::new()
            .name("recon".into())
            .spawn(move || {
                let send = |m: Msg| {
                    let _ = tx.send(m);
                    ctx.request_repaint();
                };
                let result = (|| {
                    let tools = match recon::tools::locate() {
                        Some(t) => t,
                        None => {
                            send(Msg::Stage { index: 0, total: 1, name: "Downloading COLMAP + OpenMVS (≈140 MB)" });
                            recon::tools::install(|l| send(Msg::Line(l)))?
                        }
                    };
                    recon::run(&scan, &opts, &tools, &stop, &mut |p| match p {
                        Progress::Stage { index, total, name } => send(Msg::Stage { index, total, name }),
                        Progress::Line(l) => send(Msg::Line(l)),
                    })
                })();
                send(Msg::Done(result.map_err(|e| format!("{e:#}"))));
            })
            .expect("spawn recon thread");
        self.result = None;
        self.job = Some(Job { dir, rx, cancel, started: Instant::now(), stage: None, last_line: String::new() });
    }

    /// Drains progress; returns lines for the app log (stages and outcome).
    pub fn poll(&mut self) -> Vec<String> {
        let mut log = Vec::new();
        let Some(job) = &mut self.job else { return log };
        let mut finished = None;
        while let Ok(m) = job.rx.try_recv() {
            match m {
                Msg::Stage { index, total, name } => {
                    log.push(format!("3D model: {name}"));
                    job.stage = Some((index, total, name));
                }
                Msg::Line(l) => {
                    if l.starts_with("Aligned") || l.starts_with("Ring") || l.starts_with('⚠') {
                        log.push(format!("3D model: {l}"));
                    }
                    job.last_line = l;
                }
                Msg::Done(r) => finished = Some(r),
            }
        }
        if let Some(r) = finished {
            let job = self.job.take().expect("job");
            let took = job.started.elapsed();
            log.push(match &r {
                Ok(p) => format!("3D model done in {}: {}", crate::plan::format_duration(took), p.display()),
                Err(e) => format!("3D model failed: {}", e.lines().next().unwrap_or("")),
            });
            self.tools_installed = recon::tools::locate().is_some();
            self.result = Some((job.dir, r, took));
        }
        log
    }

    pub fn ui(&mut self, ui: &mut Ui, settings: &mut ModelSettings, scans_root: &Path, scanning: bool) {
        ui.heading("3D model");
        ui.add_space(4.0);

        if let Some(job) = &self.job {
            ui.label(RichText::new(format!("Building from {}", folder_name(&job.dir))).strong());
            let (frac, text) = match job.stage {
                Some((i, n, name)) => ((i as f32 + 0.5) / n as f32, format!("{}/{n}  {name}", i + 1)),
                None => (0.0, "Starting…".into()),
            };
            ui.add(egui::ProgressBar::new(frac).text(text).animate(true));
            let mut line = job.last_line.clone();
            if line.chars().count() > 70 {
                line = format!("{}…", line.chars().take(70).collect::<String>());
            }
            ui.label(RichText::new(line).small().weak().monospace());
            ui.horizontal(|ui| {
                if ui.button("■ Cancel").clicked() {
                    job.cancel.store(true, Ordering::Relaxed);
                }
                ui.label(RichText::new(crate::plan::format_duration(job.started.elapsed())).weak());
            });
            ui.label(RichText::new("Runs in the background at low priority; full log in model\\orbis-recon.log.").weak().small());
            return;
        }

        // Which scan.
        let selected = self.target.as_deref().map(folder_name).unwrap_or_else(|| "pick a scan…".into());
        egui::ComboBox::from_id_salt("recon-scan")
            .selected_text(selected)
            .width(ui.available_width() - 8.0)
            .height(360.0)
            .show_ui(ui, |ui| {
                let scans = list_scans(scans_root);
                if scans.is_empty() {
                    ui.label("No scans yet");
                }
                for s in scans {
                    let text = format!("{}  ·  {} photos{}", folder_name(&s.dir), s.photos, if s.has_model { "  · 🧊" } else { "" });
                    let on = self.target.as_ref() == Some(&s.dir);
                    if ui.selectable_label(on, text).clicked() {
                        self.target = Some(s.dir);
                    }
                }
            });

        ui.horizontal(|ui| {
            ui.label("Quality");
            for (i, q) in Quality::ALL.iter().enumerate() {
                ui.selectable_value(&mut settings.quality, i, q.label()).on_hover_text(q.hint());
            }
        });
        ui.checkbox(&mut settings.masks, "Cut away the background")
            .on_hover_text("Masks out everything that doesn't turn with the object (wall, stand, cables). Leave on for turntable scans.");

        let photos = self.target.as_deref().map(jpeg_count).unwrap_or(0);
        let hint = if scanning {
            "Wait for the scan to finish"
        } else if self.target.is_none() {
            "Pick a scan first"
        } else if photos < 8 {
            "This scan has too few photos (needs 8+, better 36+ per ring)"
        } else {
            "Align the photos and build a textured mesh (COLMAP + OpenMVS, on the CPU)"
        };
        let ok = !scanning && self.target.is_some() && photos >= 8;
        let btn = egui::Button::new(RichText::new("🧊  Build 3D model").strong()).min_size(egui::vec2(ui.available_width(), 30.0));
        if ui.add_enabled(ok, btn).on_hover_text(hint).on_disabled_hover_text(hint).clicked() {
            let dir = self.target.clone().expect("target");
            self.start(ui.ctx(), dir, Options { quality: settings.quality(), masks: settings.masks });
        }
        if !self.tools_installed {
            ui.label(RichText::new("First run downloads COLMAP + OpenMVS (≈140 MB) from GitHub.").weak().small());
        }
        if photos > 0 && photos < 24 {
            ui.label(RichText::new(format!("⚠ {photos} photos: aim for 36+ per ring (10° steps)")).color(AMBER).small());
        }

        // Result of the last run, or an existing model of the selected scan.
        let existing = self.target.as_deref().map(model_file).filter(|p| p.is_file());
        match &self.result {
            Some((dir, Err(e), _)) if Some(dir) == self.target.as_ref() => {
                ui.label(RichText::new(format!("⚠ {e}")).color(AMBER).small());
                let log = dir.join(recon::pipeline::MODEL_DIR).join(recon::pipeline::LOG);
                if log.is_file() && ui.small_button("Show log").clicked() {
                    open(&log);
                }
            }
            _ => {
                if let Some(obj) = existing {
                    let took = match &self.result {
                        Some((d, Ok(_), t)) if Some(d) == self.target.as_ref() => format!(" in {}", crate::plan::format_duration(*t)),
                        _ => String::new(),
                    };
                    ui.label(RichText::new(format!("✔ model.obj ready{took}")).color(GREEN));
                    ui.horizontal(|ui| {
                        if ui.button("Open model").on_hover_text("Opens in your default 3D app (e.g. 3D Viewer, Blender)").clicked() {
                            open(&obj);
                        }
                        if ui.button("📂 Folder").clicked() {
                            if let Some(d) = obj.parent() {
                                open(d);
                            }
                        }
                    });
                }
            }
        }
    }
}
