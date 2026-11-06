//! PC-side driver for the ESP32 turntable controller.
//!
//! ```no_run
//! let mut rig = rig::Rig::open_auto()?;
//! let geo = rig::Geometry::default();
//! rig.move_to(geo.degrees_to_steps(90.0))?; // returns once the table has stopped
//! # anyhow::Ok(())
//! ```

use std::collections::VecDeque;
use std::io::{ErrorKind, Read, Write};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
pub use rig_protocol::{Command, ErrorCode, Response};
use rig_protocol::{decode, encode, FrameAccumulator, Reply, Request, BAUD_RATE, EVENT_ID, MAX_FRAME, PROTOCOL_VERSION};
use serialport::{SerialPort, SerialPortType};

/// USB vendor IDs of common ESP32 board serial bridges.
const KNOWN_VIDS: &[(u16, &str)] = &[
    (0x10C4, "Silicon Labs CP210x"),
    (0x1A86, "WCH CH340/CH9102"),
    (0x0403, "FTDI"),
    (0x303A, "Espressif USB"),
];

/// Converts between turntable degrees and motor steps.
#[derive(Debug, Clone, Copy)]
pub struct Geometry {
    /// Full steps per motor revolution (1.8° motor = 200).
    pub motor_steps: u32,
    /// Driver microstepping (DIP switches; 1/16 = 16).
    pub microsteps: u32,
    /// Motor turns per turntable turn (1.0 = direct drive).
    pub gear_ratio: f64,
}

impl Default for Geometry {
    fn default() -> Self {
        Self { motor_steps: 200, microsteps: 16, gear_ratio: 1.0 }
    }
}

impl Geometry {
    pub fn steps_per_table_rev(&self) -> f64 {
        self.motor_steps as f64 * self.microsteps as f64 * self.gear_ratio
    }
    pub fn degrees_to_steps(&self, deg: f64) -> i32 {
        (deg / 360.0 * self.steps_per_table_rev()).round() as i32
    }
    pub fn steps_to_degrees(&self, steps: i32) -> f64 {
        steps as f64 * 360.0 / self.steps_per_table_rev()
    }
}

/// Snapshot of the controller state.
#[derive(Debug, Clone, Copy)]
pub struct Status {
    pub position: i32,
    pub moving: bool,
    pub enabled: bool,
    pub max_speed: u32,
    pub accel: u32,
}

/// Find the serial port of an ESP32 board by its USB bridge chip.
pub fn find_port() -> Result<String> {
    let ports = serialport::available_ports().context("listing serial ports")?;
    let found: Vec<_> = ports
        .into_iter()
        .filter(|p| matches!(&p.port_type, SerialPortType::UsbPort(u) if KNOWN_VIDS.iter().any(|(v, _)| *v == u.vid)))
        .collect();
    match found.as_slice() {
        [one] => Ok(one.port_name.clone()),
        [] => bail!("no ESP32 serial port found (looked for CP210x, CH340, FTDI, Espressif USB); pass --port"),
        many => bail!(
            "several possible ESP32 ports: {}; pass --port",
            many.iter().map(|p| p.port_name.as_str()).collect::<Vec<_>>().join(", ")
        ),
    }
}

/// List serial ports with a hint for likely ESP32 boards.
pub fn list_ports() -> Result<Vec<(String, String)>> {
    let ports = serialport::available_ports().context("listing serial ports")?;
    Ok(ports
        .into_iter()
        .map(|p| {
            let desc = match &p.port_type {
                SerialPortType::UsbPort(u) => {
                    let chip = KNOWN_VIDS.iter().find(|(v, _)| *v == u.vid).map(|(_, n)| *n);
                    format!(
                        "USB {:04x}:{:04x} {}{}",
                        u.vid,
                        u.pid,
                        u.product.clone().unwrap_or_default(),
                        chip.map(|c| format!("  ← likely ESP32 ({c})")).unwrap_or_default()
                    )
                }
                SerialPortType::BluetoothPort => "Bluetooth".into(),
                SerialPortType::PciPort => "PCI".into(),
                SerialPortType::Unknown => "unknown".into(),
            };
            (p.port_name, desc)
        })
        .collect())
}

pub struct Rig {
    port: Box<dyn SerialPort>,
    rx: FrameAccumulator<256>,
    next_id: u16,
    button_presses: VecDeque<Instant>,
}

impl Rig {
    /// Open the first ESP32-looking serial port.
    pub fn open_auto() -> Result<Self> {
        Self::open(&find_port()?)
    }

    /// Open `path` (e.g. `COM5`) and check the firmware answers.
    pub fn open(path: &str) -> Result<Self> {
        let mut port = serialport::new(path, BAUD_RATE)
            .timeout(Duration::from_millis(20))
            .open()
            .with_context(|| format!("opening {path}"))?;
        // ESP32 boards wire DTR/RTS to EN/BOOT for auto-reset. Keep both
        // released so opening the port doesn't reset the board.
        let _ = port.write_data_terminal_ready(false);
        let _ = port.write_request_to_send(false);

        let mut rig = Self { port, rx: FrameAccumulator::new(), next_id: 1, button_presses: VecDeque::new() };

        // If the board did reset, it needs ~1 s to boot. Retry the ping.
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            match rig.request(Command::Ping, Duration::from_millis(300)) {
                Ok(Response::Pong { version }) if version == PROTOCOL_VERSION => return Ok(rig),
                Ok(Response::Pong { version }) => bail!(
                    "firmware speaks protocol v{version}, this program v{PROTOCOL_VERSION}; reflash the firmware"
                ),
                Ok(other) => bail!("unexpected answer to ping: {other:?}"),
                Err(_) if Instant::now() < deadline => continue,
                Err(e) => {
                    return Err(e).with_context(|| {
                        format!("no answer from turntable firmware on {path} (is it flashed? see firmware/README)")
                    })
                }
            }
        }
    }

    /// Send a command and wait for its reply.
    pub fn request(&mut self, cmd: Command, timeout: Duration) -> Result<Response> {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1); // 0 is reserved for events

        let mut buf = [0u8; MAX_FRAME];
        let frame = encode(&Request { id, cmd }, &mut buf).map_err(|e| anyhow!("encode: {e}"))?;
        self.port.write_all(frame).context("serial write")?;
        self.port.flush().ok();

        let deadline = Instant::now() + timeout;
        loop {
            if let Some(reply) = self.read_reply(deadline)? {
                if reply.id == id {
                    return match reply.resp {
                        Response::Error(code) => Err(anyhow!("turntable refused {cmd:?}: {code:?}")),
                        r => Ok(r),
                    };
                }
                // Replies to older, timed-out requests are dropped.
            } else {
                bail!("timed out waiting for reply to {cmd:?}");
            }
        }
    }

    /// Read one reply addressed to us, queueing events on the way.
    fn read_reply(&mut self, deadline: Instant) -> Result<Option<Reply>> {
        let mut byte = [0u8; 1];
        while Instant::now() < deadline {
            match self.port.read(&mut byte) {
                Ok(1) => {}
                Ok(_) => continue,
                Err(e) if e.kind() == ErrorKind::TimedOut => continue,
                Err(e) => return Err(e).context("serial read"),
            }
            let Some(frame) = self.rx.push(byte[0]) else { continue };
            // Boot messages etc. fail to decode; skip them.
            let Ok(reply) = decode::<Reply>(frame) else { continue };
            if reply.id == EVENT_ID {
                if let Response::ButtonPressed = reply.resp {
                    self.button_presses.push_back(Instant::now());
                }
                continue;
            }
            return Ok(Some(reply));
        }
        Ok(None)
    }

    /// Wait up to `timeout` for a button press. Presses that arrived during
    /// other requests are returned first.
    pub fn wait_button(&mut self, timeout: Duration) -> Result<bool> {
        if self.button_presses.pop_front().is_some() {
            return Ok(true);
        }
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            // Replies here belong to no pending request; ignore them.
            let _ = self.read_reply(deadline)?;
            if self.button_presses.pop_front().is_some() {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// True if the button was pressed since the last check (non-blocking).
    pub fn take_button_press(&mut self) -> Result<bool> {
        self.wait_button(Duration::ZERO)
    }

    pub fn status(&mut self) -> Result<Status> {
        match self.request(Command::Status, Duration::from_secs(1))? {
            Response::Status { position, moving, enabled, max_speed, accel } => {
                Ok(Status { position, moving, enabled, max_speed, accel })
            }
            r => bail!("unexpected answer to status: {r:?}"),
        }
    }

    /// Move to an absolute step position; returns once the motor has stopped.
    pub fn move_to(&mut self, position: i32) -> Result<i32> {
        let current = self.status()?.position;
        let timeout = self.move_timeout(position.saturating_sub(current))?;
        self.expect_moved(Command::MoveTo { position }, timeout)
    }

    /// Move relative; returns the new position once the motor has stopped.
    pub fn move_by(&mut self, steps: i32) -> Result<i32> {
        let timeout = self.move_timeout(steps)?;
        self.expect_moved(Command::MoveBy { steps }, timeout)
    }

    fn expect_moved(&mut self, cmd: Command, timeout: Duration) -> Result<i32> {
        match self.request(cmd, timeout)? {
            Response::Moved { position } => Ok(position),
            Response::Stopped { position } => bail!("move was stopped at step {position}"),
            r => bail!("unexpected answer to {cmd:?}: {r:?}"),
        }
    }

    /// Generous upper bound for a move of `steps` at the current speed.
    fn move_timeout(&mut self, steps: i32) -> Result<Duration> {
        let s = self.status()?;
        let secs = steps.unsigned_abs() as f64 / s.max_speed.max(1) as f64
            + 2.0 * s.max_speed as f64 / s.accel.max(1) as f64;
        Ok(Duration::from_secs_f64(secs * 2.0 + 2.0))
    }

    pub fn stop(&mut self) -> Result<()> {
        self.request(Command::Stop, Duration::from_secs(1)).map(drop)
    }

    pub fn set_zero(&mut self) -> Result<()> {
        self.request(Command::SetZero, Duration::from_secs(1)).map(drop)
    }

    pub fn enable(&mut self, on: bool) -> Result<()> {
        self.request(Command::Enable(on), Duration::from_secs(1)).map(drop)
    }

    /// Top speed (steps/s) and acceleration (steps/s²).
    pub fn set_motion(&mut self, max_speed: u32, accel: u32) -> Result<()> {
        self.request(Command::SetMotion { max_speed, accel }, Duration::from_secs(1)).map(drop)
    }
}
