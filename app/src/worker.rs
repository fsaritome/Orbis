//! Background thread that owns the turntable connection.
//!
//! The UI never blocks on the network: it sends [`Cmd`]s and receives
//! [`Event`]s. The worker (re)connects on its own, polls status, forwards
//! BOOT-button presses and runs scans step by step so Pause/Stop stay
//! responsive.

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use rig::{Geometry, Rig};

use crate::plan::ScanPlan;

pub enum Cmd {
    Reconnect,
    SetGeometry(Geometry),
    /// Top speed and acceleration in steps/s and steps/s².
    SetMotion { max_speed: u32, accel: u32 },
    Jog(f64),
    GoTo(f64),
    SetZero,
    Enable(bool),
    StartScan(ScanPlan),
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
    ScanStarted(ScanPlan),
    /// A photo position was done (dry run: nothing shot).
    Shot { ring: u32, index: u32, angle: f64 },
    /// Ring `next` (0-based) is next; waiting for the user to move the camera.
    WaitingForRing(u32),
    ScanPaused(bool),
    ScanFinished,
    ScanStopped,
    Log(String),
}

/// How long a dry-run "shot" takes (stands in for the camera).
const DRY_SHOT: Duration = Duration::from_millis(300);
const STATUS_EVERY: Duration = Duration::from_millis(250);
const RETRY_EVERY: Duration = Duration::from_secs(3);

enum Phase {
    Move,
    Settle(Instant),
    Shoot(Instant),
    RingBreak,
}

struct ScanRun {
    plan: ScanPlan,
    /// Step position where the scan started (= 0° of the scan).
    origin: i32,
    ring: u32,
    shot: u32,
    phase: Phase,
    paused: bool,
}

pub fn spawn(ctx: egui::Context) -> (Sender<Cmd>, Receiver<Event>) {
    let (cmd_tx, cmd_rx) = mpsc::channel();
    let (ev_tx, ev_rx) = mpsc::channel();
    thread::Builder::new()
        .name("turntable".into())
        .spawn(move || Worker { rig: None, geo: Geometry::default(), scan: None, events: ev_tx, ctx }.run(cmd_rx))
        .expect("spawn turntable thread");
    (cmd_tx, ev_rx)
}

struct Worker {
    rig: Option<Rig>,
    geo: Geometry,
    scan: Option<ScanRun>,
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
            Cmd::StartScan(plan) if !scanning => {
                let Some(origin) = self.with_rig(|r| Ok(r.status()?.position)) else { return };
                self.scan = Some(ScanRun { plan, origin, ring: 0, shot: 0, phase: Phase::Settle(Instant::now()), paused: false });
                self.log(format!(
                    "Dry run started: {} rings × {} positions ({:.1}° steps)",
                    plan.rings,
                    plan.shots_per_ring,
                    plan.angle_step()
                ));
                self.emit(Event::ScanStarted(plan));
            }
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
                self.scan = None;
                self.log("Scan stopped");
                self.emit(Event::ScanStopped);
            }
            // Movement commands while scanning are ignored.
            _ => {}
        }
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
            Phase::Settle(_) => s.phase = Phase::Shoot(Instant::now() + DRY_SHOT),
            Phase::Shoot(_) => {
                let (ring, index, angle) = (s.ring, s.shot, s.plan.angle_of(s.shot));
                s.shot += 1;
                let mut next = None;
                if s.shot == s.plan.shots_per_ring {
                    s.shot = 0;
                    s.ring += 1;
                    next = Some(s.ring);
                    s.phase = Phase::RingBreak;
                } else {
                    s.phase = Phase::Move;
                }
                let rings = s.plan.rings;
                self.emit(Event::Shot { ring, index, angle });
                match next {
                    Some(r) if r == rings => {
                        self.scan = None;
                        self.log("Dry run finished");
                        self.emit(Event::ScanFinished);
                    }
                    Some(r) => self.emit(Event::WaitingForRing(r)),
                    None => {}
                }
            }
            Phase::RingBreak => {}
        }
    }
}
