//! Background thread that owns the turntable connection and runs scans.
//!
//! The UI never blocks on the network: it sends [`Cmd`]s and receives
//! [`Event`]s. The worker (re)connects on its own, polls status, forwards
//! BOOT-button presses and runs scans step by step so Pause/Stop stay
//! responsive. For real scans it asks the camera thread for each photo.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use rig::{Geometry, Rig};
use serde::Serialize;

use crate::camera::CamCmd;
use crate::plan::ScanPlan;

/// Where and how a real (non-dry) scan saves its photos.
#[derive(Debug, Clone)]
pub struct ScanOutput {
    pub dir: PathBuf,
    pub name: String,
    pub autofocus: bool,
}

pub enum Cmd {
    Reconnect,
    SetGeometry(Geometry),
    /// Top speed and acceleration in steps/s and steps/s².
    SetMotion { max_speed: u32, accel: u32 },
    Jog(f64),
    GoTo(f64),
    SetZero,
    Enable(bool),
    /// `output: None` = dry run (turn only).
    StartScan { plan: ScanPlan, output: Option<ScanOutput> },
    Pause(bool),
    ContinueRing,
    Stop,
}

#[derive(Debug, Clone)]
pub enum Event {
    Connecting,
    Connected(String),
    Disconnected(String),
    Status { degrees: f64, moving: bool, enabled: bool },
    ButtonPressed,
    ScanStarted { plan: ScanPlan, dir: Option<PathBuf> },
    /// A photo position is done (dry run: no files).
    Shot { ring: u32, index: u32, angle: f64, files: Vec<PathBuf> },
    /// Ring `next` (0-based) is next; waiting for the user to move the camera.
    WaitingForRing(u32),
    ScanPaused(bool),
    ScanFinished,
    ScanStopped,
    Log(String),
}

/// How long a dry-run "shot" takes (stands in for the camera).
const DRY_SHOT: Duration = Duration::from_millis(300);
/// Upper bound for one capture incl. autofocus and transfer.
const CAPTURE_TIMEOUT: Duration = Duration::from_secs(45);
const STATUS_EVERY: Duration = Duration::from_millis(250);
const RETRY_EVERY: Duration = Duration::from_secs(3);

enum Phase {
    Move,
    Settle(Instant),
    Shoot(Instant),
    RingBreak,
}

/// `scan.json` next to the photos: everything reconstruction tools or a
/// later retake need to know.
#[derive(Serialize)]
struct ScanRecord {
    app: &'static str,
    version: &'static str,
    name: String,
    started: String,
    finished: Option<String>,
    rings: u32,
    shots_per_ring: u32,
    angle_step_deg: f64,
    settle_ms: u64,
    steps_per_turn: f64,
    autofocus: bool,
    shots: Vec<ShotRecord>,
}

#[derive(Serialize)]
struct ShotRecord {
    ring: u32,
    index: u32,
    angle_deg: f64,
    files: Vec<String>,
    time: String,
    focus_ok: bool,
}

struct ScanRun {
    plan: ScanPlan,
    output: Option<ScanOutput>,
    record: Option<ScanRecord>,
    /// Step position where the scan started (= 0° of the scan).
    origin: i32,
    ring: u32,
    shot: u32,
    phase: Phase,
    paused: bool,
}

impl ScanRun {
    fn save_record(&self) -> anyhow::Result<()> {
        if let (Some(out), Some(rec)) = (&self.output, &self.record) {
            std::fs::create_dir_all(&out.dir)?;
            std::fs::write(out.dir.join("scan.json"), serde_json::to_vec_pretty(rec)?)?;
        }
        Ok(())
    }
}

pub fn spawn(ctx: egui::Context, camera: Sender<CamCmd>) -> (Sender<Cmd>, Receiver<Event>) {
    let (cmd_tx, cmd_rx) = mpsc::channel();
    let (ev_tx, ev_rx) = mpsc::channel();
    thread::Builder::new()
        .name("turntable".into())
        .spawn(move || {
            Worker { rig: None, geo: Geometry::default(), scan: None, camera, events: ev_tx, ctx }.run(cmd_rx)
        })
        .expect("spawn turntable thread");
    (cmd_tx, ev_rx)
}

struct Worker {
    rig: Option<Rig>,
    geo: Geometry,
    scan: Option<ScanRun>,
    camera: Sender<CamCmd>,
    events: Sender<Event>,
    ctx: egui::Context,
}

impl Worker {
    fn emit(&self, ev: Event) {
        let _ = self.events.send(ev);
        self.ctx.request_repaint();
    }

    fn log(&self, msg: impl Into<String>) {
        self.emit(Event::Log(msg.into()));
    }

    fn run(mut self, cmds: Receiver<Cmd>) {
        let mut next_status = Instant::now();
        loop {
            if self.rig.is_none() && !self.connect(&cmds) {
                return; // UI closed
            }

            // Wait for a command, but not longer than the next thing to do.
            let wait = if self.scan_has_work() { Duration::ZERO } else { Duration::from_millis(50) };
            match cmds.recv_timeout(wait) {
                Ok(cmd) => self.handle(cmd),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
            while let Ok(cmd) = cmds.try_recv() {
                self.handle(cmd);
            }

            self.step_scan();

            if self.rig.is_some() && Instant::now() >= next_status {
                next_status = Instant::now() + STATUS_EVERY;
                self.poll();
            }
        }
    }

    /// Try to connect until it works. Returns false if the UI went away.
    fn connect(&mut self, cmds: &Receiver<Cmd>) -> bool {
        loop {
            self.emit(Event::Connecting);
            match Rig::open_auto() {
                Ok(rig) => {
                    let how = rig.description().to_owned();
                    self.rig = Some(rig);
                    self.log(format!("Turntable connected via {how}"));
                    self.emit(Event::Connected(how));
                    return true;
                }
                Err(e) => self.emit(Event::Disconnected(format!("{e:#}"))),
            }
            // Wait before retrying; settings sent meanwhile still apply.
            let until = Instant::now() + RETRY_EVERY;
            while Instant::now() < until {
                match cmds.recv_timeout(Duration::from_millis(100)) {
                    Ok(Cmd::Reconnect) => break,
                    Ok(Cmd::SetGeometry(g)) => self.geo = g,
                    Ok(_) => {} // needs a connection; drop
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => return false,
                }
            }
        }
    }

    /// Run `f` on the connection; on error, drop the connection and any scan.
    fn with_rig<T>(&mut self, f: impl FnOnce(&mut Rig) -> anyhow::Result<T>) -> Option<T> {
        let rig = self.rig.as_mut()?;
        match f(rig) {
            Ok(v) => Some(v),
            Err(e) => {
                self.rig = None;
                if self.scan.take().is_some() {
                    self.emit(Event::ScanStopped);
                }
                self.log(format!("Turntable error: {e:#}"));
                self.emit(Event::Disconnected(format!("{e:#}")));
                None
            }
        }
    }

    fn poll(&mut self) {
        let geo = self.geo;
        let Some((status, pressed)) = self.with_rig(|r| Ok((r.status()?, r.take_button_press()?))) else {
            return;
        };
        self.emit(Event::Status {
            degrees: geo.steps_to_degrees(status.position),
            moving: status.moving,
            enabled: status.enabled,
        });
        if pressed {
            self.emit(Event::ButtonPressed);
        }
    }

    fn handle(&mut self, cmd: Cmd) {
        let scanning = self.scan.is_some();
        let geo = self.geo;
        match cmd {
            Cmd::Reconnect => {
                self.rig = None;
                if self.scan.take().is_some() {
                    self.emit(Event::ScanStopped);
                }
            }
            Cmd::SetGeometry(g) => self.geo = g,
            Cmd::SetMotion { max_speed, accel } => {
                self.with_rig(|r| r.set_motion(max_speed, accel));
            }
            Cmd::Jog(deg) if !scanning => {
                self.with_rig(|r| r.move_by(geo.degrees_to_steps(deg)));
            }
            Cmd::GoTo(deg) if !scanning => {
                self.with_rig(|r| r.move_to(geo.degrees_to_steps(deg)));
            }
            Cmd::SetZero if !scanning => {
                if self.with_rig(|r| r.set_zero()).is_some() {
                    self.log("Current position is now 0°");
                }
            }
            Cmd::Enable(on) if !scanning => {
                self.with_rig(|r| r.enable(on));
            }
            Cmd::StartScan { plan, output } if !scanning => self.start_scan(plan, output),
            Cmd::Pause(p) => {
                if let Some(s) = &mut self.scan {
                    s.paused = p;
                    self.emit(Event::ScanPaused(p));
                }
            }
            Cmd::ContinueRing => {
                if let Some(s) = &mut self.scan {
                    if let Phase::RingBreak = s.phase {
                        s.phase = Phase::Move;
                    }
                }
            }
            Cmd::Stop if scanning => {
                if let Some(mut s) = self.scan.take() {
                    if let Some(rec) = &mut s.record {
                        rec.finished = Some(format!("stopped {}", now()));
                    }
                    let _ = s.save_record();
                }
                self.log("Scan stopped");
                self.emit(Event::ScanStopped);
            }
            // Movement commands while scanning are ignored.
            _ => {}
        }
    }

    fn start_scan(&mut self, plan: ScanPlan, output: Option<ScanOutput>) {
        let Some(origin) = self.with_rig(|r| Ok(r.status()?.position)) else { return };
        let record = output.as_ref().map(|o| ScanRecord {
            app: "Orbis",
            version: env!("CARGO_PKG_VERSION"),
            name: o.name.clone(),
            started: now(),
            finished: None,
            rings: plan.rings,
            shots_per_ring: plan.shots_per_ring,
            angle_step_deg: plan.angle_step(),
            settle_ms: plan.settle.as_millis() as u64,
            steps_per_turn: self.geo.steps_per_table_rev(),
            autofocus: o.autofocus,
            shots: Vec::new(),
        });
        let dir = output.as_ref().map(|o| o.dir.clone());
        let run = ScanRun {
            plan,
            output,
            record,
            origin,
            ring: 0,
            shot: 0,
            phase: Phase::Settle(Instant::now()),
            paused: false,
        };
        if let Err(e) = run.save_record() {
            self.log(format!("Can't write to the scan folder: {e:#}"));
            return;
        }
        self.scan = Some(run);
        match &dir {
            Some(d) => self.log(format!(
                "Scan started: {} rings × {} photos ({:.1}° steps) → {}",
                plan.rings,
                plan.shots_per_ring,
                plan.angle_step(),
                d.display()
            )),
            None => self.log(format!(
                "Dry run started: {} rings × {} positions ({:.1}° steps)",
                plan.rings,
                plan.shots_per_ring,
                plan.angle_step()
            )),
        }
        self.emit(Event::ScanStarted { plan, dir });
    }

    fn scan_has_work(&self) -> bool {
        match &self.scan {
            Some(s) if !s.paused => match s.phase {
                Phase::Move => true,
                Phase::Settle(t) | Phase::Shoot(t) => Instant::now() >= t,
                Phase::RingBreak => false,
            },
            _ => false,
        }
    }

    /// Advance the scan by at most one phase.
    fn step_scan(&mut self) {
        if !self.scan_has_work() {
            return;
        }
        let geo = self.geo;
        let s = self.scan.as_mut().expect("checked");
        match s.phase {
            Phase::Move => {
                let deg = 360.0 * s.ring as f64 + s.plan.angle_of(s.shot);
                let target = s.origin + geo.degrees_to_steps(deg);
                let settle = s.plan.settle;
                if self.with_rig(|r| r.move_to(target)).is_none() {
                    return;
                }
                if let Some(s) = &mut self.scan {
                    s.phase = Phase::Settle(Instant::now() + settle);
                }
                self.poll();
            }
            Phase::Settle(_) => {
                // Dry runs fake the shot with a short pause; real ones shoot now.
                let delay = if s.output.is_some() { Duration::ZERO } else { DRY_SHOT };
                s.phase = Phase::Shoot(Instant::now() + delay);
            }
            Phase::Shoot(_) => self.shoot(),
            Phase::RingBreak => {}
        }
    }

    fn shoot(&mut self) {
        let s = self.scan.as_mut().expect("scanning");
        let (ring, index, angle) = (s.ring, s.shot, s.plan.angle_of(s.shot));

        // Real scan: ask the camera thread for the photo and wait for it.
        let mut files = Vec::new();
        if let Some(out) = &s.output {
            let stem = out.dir.join(format!("ring{}_{:03}", ring + 1, index + 1));
            let (tx, rx) = mpsc::channel();
            let sent = self.camera.send(CamCmd::Capture { stem, autofocus: out.autofocus, reply: tx });
            let result = match sent {
                Ok(()) => rx.recv_timeout(CAPTURE_TIMEOUT).unwrap_or_else(|_| Err("camera did not answer".into())),
                Err(_) => Err("camera thread is gone".into()),
            };
            match result {
                Ok(capture) => {
                    files = capture.files.clone();
                    if let Some(rec) = &mut s.record {
                        rec.shots.push(ShotRecord {
                            ring,
                            index,
                            angle_deg: angle,
                            files: capture
                                .files
                                .iter()
                                .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
                                .collect(),
                            time: now(),
                            focus_ok: capture.focus_ok,
                        });
                    }
                    if !capture.focus_ok {
                        let msg = format!("ring {} photo {}: autofocus failed, check sharpness", ring + 1, index + 1);
                        self.log(msg);
                    }
                }
                Err(e) => {
                    // Keep this position and wait for the user.
                    s.paused = true;
                    self.log(format!(
                        "Photo ring {} / {} failed: {e}. Fix it, then press Resume to retry this position.",
                        ring + 1,
                        index + 1
                    ));
                    self.emit(Event::ScanPaused(true));
                    return;
                }
            }
        }

        let s = self.scan.as_mut().expect("scanning");
        s.shot += 1;
        let mut next_ring = None;
        if s.shot == s.plan.shots_per_ring {
            s.shot = 0;
            s.ring += 1;
            next_ring = Some(s.ring);
            s.phase = Phase::RingBreak;
        } else {
            s.phase = Phase::Move;
        }
        let rings = s.plan.rings;
        let finished = next_ring == Some(rings);
        if finished {
            if let Some(rec) = &mut s.record {
                rec.finished = Some(now());
            }
        }
        if let Err(e) = s.save_record() {
            self.log(format!("Couldn't update scan.json: {e:#}"));
        }
        self.emit(Event::Shot { ring, index, angle, files });

        match next_ring {
            Some(_) if finished => {
                let real = self.scan.take().is_some_and(|s| s.output.is_some());
                self.log(if real { "Scan finished" } else { "Dry run finished" });
                self.emit(Event::ScanFinished);
            }
            Some(r) => self.emit(Event::WaitingForRing(r)),
            None => {}
        }
    }
}

fn now() -> String {
    chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false)
}
