//! PC-side driver for the Orbis turntable.
//!
//! The turntable is reached over Wi-Fi (TCP, found by UDP broadcast) or, for
//! first-time setup, over USB serial. Both speak the same framed protocol.
//!
//! ```no_run
//! let mut rig = rig::Rig::open_auto()?;
//! let geo = rig::Geometry::default();
//! rig.move_to(geo.degrees_to_steps(90.0))?; // returns once the table has stopped
//! # anyhow::Ok(())
//! ```

use std::collections::VecDeque;
use std::fmt;
use std::io::{ErrorKind, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpStream, UdpSocket};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
pub use rig_protocol::{Command, ErrorCode, Response};
use rig_protocol::{
    decode, encode, Announce, FrameAccumulator, Password, Reply, Request, Ssid, BAUD_RATE, DISCOVERY_MAGIC,
    DISCOVERY_PORT, EVENT_ID, MAX_FRAME, PROTOCOL_VERSION, TCP_PORT,
};
use serialport::SerialPortType;

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

/// Network state reported by the turntable.
#[derive(Debug, Clone)]
pub struct NetInfo {
    pub ssid: String,
    pub connected: bool,
    pub ip: Ipv4Addr,
}

/// A turntable that answered a discovery broadcast.
#[derive(Debug, Clone)]
pub struct Found {
    pub addr: SocketAddr,
    pub mac: [u8; 6],
    pub version: u16,
}

impl fmt::Display for Found {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let m = self.mac;
        write!(
            f,
            "{}  (MAC {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}, protocol v{})",
            self.addr, m[0], m[1], m[2], m[3], m[4], m[5], self.version
        )
    }
}

/// Broadcast `ORBIS?` on every IPv4 interface and collect answers.
pub fn discover(timeout: Duration) -> Result<Vec<Found>> {
    let sock = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).context("binding discovery socket")?;
    sock.set_broadcast(true)?;
    sock.set_read_timeout(Some(Duration::from_millis(50)))?;

    // Windows sends 255.255.255.255 out of one interface only, so also hit
    // each interface's directed broadcast address.
    let mut targets = vec![Ipv4Addr::BROADCAST];
    if let Ok(ifaces) = if_addrs::get_if_addrs() {
        for iface in ifaces {
            if let if_addrs::IfAddr::V4(v4) = iface.addr {
                if let Some(b) = v4.broadcast {
                    if !v4.ip.is_loopback() && !targets.contains(&b) {
                        targets.push(b);
                    }
                }
            }
        }
    }

    let mut found: Vec<Found> = Vec::new();
    let deadline = Instant::now() + timeout;
    let mut next_send = Instant::now();
    while Instant::now() < deadline {
        // Resend a few times; UDP over Wi-Fi drops packets.
        if Instant::now() >= next_send {
            for t in &targets {
                let _ = sock.send_to(DISCOVERY_MAGIC, (*t, DISCOVERY_PORT));
            }
            next_send = Instant::now() + Duration::from_millis(400);
        }
        let mut buf = [0u8; 64];
        match sock.recv_from(&mut buf) {
            Ok((n, from)) => {
                if let Ok(a) = postcard::from_bytes::<Announce>(&buf[..n]) {
                    let addr = SocketAddr::new(from.ip(), a.tcp_port);
                    if !found.iter().any(|f| f.addr == addr) {
                        found.push(Found { addr, mac: a.mac, version: a.version });
                    }
                }
            }
            // ConnectionReset: Windows reports an ICMP "port unreachable"
            // from some other host as an error on the next receive.
            Err(e) if matches!(e.kind(), ErrorKind::TimedOut | ErrorKind::WouldBlock | ErrorKind::ConnectionReset) => {}
            Err(e) => return Err(e).context("discovery receive"),
        }
    }
    Ok(found)
}

/// Find the serial port of an ESP32 board by its USB bridge chip.
pub fn find_serial_port() -> Result<String> {
    let ports = serialport::available_ports().context("listing serial ports")?;
    let found: Vec<_> = ports
        .into_iter()
        .filter(|p| matches!(&p.port_type, SerialPortType::UsbPort(u) if KNOWN_VIDS.iter().any(|(v, _)| *v == u.vid)))
        .collect();
    match found.as_slice() {
        [one] => Ok(one.port_name.clone()),
        [] => bail!("no ESP32 serial port found (looked for CP210x, CH340, FTDI, Espressif USB)"),
        many => bail!(
            "several possible ESP32 ports: {}; pass --port",
            many.iter().map(|p| p.port_name.as_str()).collect::<Vec<_>>().join(", ")
        ),
    }
}

/// List serial ports with a hint for likely ESP32 boards.
pub fn list_serial_ports() -> Result<Vec<(String, String)>> {
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

/// Byte stream to the turntable: TCP or serial.
trait Link: Read + Write + Send {}
impl<T: Read + Write + Send> Link for T {}

pub struct Rig {
    link: Box<dyn Link>,
    description: String,
    rx: FrameAccumulator<512>,
    next_id: u16,
    button_presses: VecDeque<Instant>,
}

impl Rig {
    /// Find the turntable: Wi-Fi first (discovery), then USB serial.
    pub fn open_auto() -> Result<Self> {
        let found = discover(Duration::from_millis(1500)).unwrap_or_default();
        match found.as_slice() {
            [one] => {
                return Self::connect(one.addr).with_context(|| {
                    format!(
                        "turntable at {} answered but refused the connection; it accepts one client at a time, \
                         so another Orbis window or `rig` command is probably using it",
                        one.addr.ip()
                    )
                })
            }
            [] => {}
            many => bail!(
                "several turntables answered: {}; pass --host",
                many.iter().map(|f| f.addr.ip().to_string()).collect::<Vec<_>>().join(", ")
            ),
        }
        let port = find_serial_port().context("no turntable found on the network or USB")?;
        Self::open_serial(&port)
    }

    /// Connect over Wi-Fi. `addr` without a port uses the default.
    pub fn connect_host(host: &str) -> Result<Self> {
        let addr: SocketAddr = match host.parse() {
            Ok(a) => a,
            Err(_) => SocketAddr::V4(SocketAddrV4::new(
                host.parse().with_context(|| format!("not an IP address: {host}"))?,
                TCP_PORT,
            )),
        };
        Self::connect(addr)
    }

    pub fn connect(addr: SocketAddr) -> Result<Self> {
        let stream = TcpStream::connect_timeout(&addr, Duration::from_secs(3))
            .with_context(|| format!("connecting to turntable at {addr}"))?;
        stream.set_nodelay(true)?;
        stream.set_read_timeout(Some(Duration::from_millis(20)))?;
        Self::handshake(Box::new(stream), format!("{addr} (Wi-Fi)"), Duration::from_secs(2))
    }

    /// Open a USB serial port (e.g. `COM5`).
    pub fn open_serial(path: &str) -> Result<Self> {
        let mut port = serialport::new(path, BAUD_RATE)
            .timeout(Duration::from_millis(20))
            .open()
            .with_context(|| format!("opening {path}"))?;
        // ESP32 boards wire DTR/RTS to EN/BOOT for auto-reset. Keep both
        // released so opening the port doesn't reset the board.
        let _ = port.write_data_terminal_ready(false);
        let _ = port.write_request_to_send(false);
        // If the board did reset anyway, give it time to boot.
        Self::handshake(Box::new(port), format!("{path} (USB)"), Duration::from_secs(4))
    }

    fn handshake(link: Box<dyn Link>, description: String, patience: Duration) -> Result<Self> {
        let mut rig =
            Self { link, description, rx: FrameAccumulator::new(), next_id: 1, button_presses: VecDeque::new() };
        let deadline = Instant::now() + patience;
        loop {
            match rig.request(Command::Ping, Duration::from_millis(400)) {
                Ok(Response::Pong { version }) if version == PROTOCOL_VERSION => return Ok(rig),
                Ok(Response::Pong { version }) => bail!(
                    "turntable firmware speaks protocol v{version}, this program v{PROTOCOL_VERSION}; reflash the firmware"
                ),
                Ok(other) => bail!("unexpected answer to ping: {other:?}"),
                Err(_) if Instant::now() < deadline => continue,
                Err(e) => {
                    return Err(e).with_context(|| {
                        format!("no answer from Orbis firmware on {} (is it flashed?)", rig.description)
                    })
                }
            }
        }
    }

    /// How we're connected, e.g. `192.168.178.42:7878 (Wi-Fi)`.
    pub fn description(&self) -> &str {
        &self.description
    }

    /// Send a command and wait for its reply.
    pub fn request(&mut self, cmd: Command, timeout: Duration) -> Result<Response> {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1); // 0 is reserved for events

        let mut buf = [0u8; MAX_FRAME];
        let frame = encode(&Request { id, cmd: cmd.clone() }, &mut buf).map_err(|e| anyhow!("encode: {e}"))?;
        self.link.write_all(frame).context("sending to turntable")?;
        self.link.flush().ok();

        let deadline = Instant::now() + timeout;
        loop {
            match self.read_reply(deadline)? {
                Some(reply) if reply.id == id => {
                    return match reply.resp {
                        Response::Error(code) => Err(anyhow!("turntable refused {}: {code:?}", describe(&cmd))),
                        r => Ok(r),
                    };
                }
                Some(_) => {} // reply to an older, timed-out request
                None => bail!("timed out waiting for reply to {}", describe(&cmd)),
            }
        }
    }

    /// Read one reply addressed to us, queueing events on the way.
    fn read_reply(&mut self, deadline: Instant) -> Result<Option<Reply>> {
        let mut chunk = [0u8; 256];
        loop {
            let n = match self.link.read(&mut chunk) {
                Ok(0) => bail!("turntable closed the connection"),
                Ok(n) => n,
                Err(e) if matches!(e.kind(), ErrorKind::TimedOut | ErrorKind::WouldBlock) => {
                    if Instant::now() >= deadline {
                        return Ok(None);
                    }
                    continue;
                }
                Err(e) => return Err(e).context("reading from turntable"),
            };
            // Decode every complete frame in this chunk. Keep the first
            // reply; later ones can only be events (one request in flight).
            let mut result = None;
            for &b in &chunk[..n] {
                let Some(frame) = self.rx.push(b) else { continue };
                // Boot messages etc. fail to decode; skip them.
                let Ok(reply) = decode::<Reply>(frame) else { continue };
                if reply.id == EVENT_ID {
                    if reply.resp == Response::ButtonPressed {
                        self.button_presses.push_back(Instant::now());
                    }
                } else if result.is_none() {
                    result = Some(reply);
                }
            }
            if result.is_some() {
                return Ok(result);
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
        }
    }

    /// Wait up to `timeout` for a button press. Presses that arrived during
    /// other requests are returned first.
    pub fn wait_button(&mut self, timeout: Duration) -> Result<bool> {
        if self.button_presses.pop_front().is_some() {
            return Ok(true);
        }
        let deadline = Instant::now() + timeout;
        loop {
            // Replies here belong to no pending request; ignore them.
            let _ = self.read_reply(deadline)?;
            if self.button_presses.pop_front().is_some() {
                return Ok(true);
            }
            if Instant::now() >= deadline {
                return Ok(false);
            }
        }
    }

    /// True if the button was pressed since the last check (non-blocking).
    pub fn take_button_press(&mut self) -> Result<bool> {
        self.wait_button(Duration::ZERO)
    }

    pub fn status(&mut self) -> Result<Status> {
        match self.request(Command::Status, Duration::from_secs(2))? {
            Response::Status { position, moving, enabled, max_speed, accel } => {
                Ok(Status { position, moving, enabled, max_speed, accel })
            }
            r => bail!("unexpected answer to status: {r:?}"),
        }
    }

    /// Move to an absolute step position; returns once the motor has stopped.
    pub fn move_to(&mut self, position: i32) -> Result<i32> {
        let s = self.status()?;
        let timeout = move_timeout(&s, position.saturating_sub(s.position));
        self.expect_moved(Command::MoveTo { position }, timeout)
    }

    /// Move relative; returns the new position once the motor has stopped.
    pub fn move_by(&mut self, steps: i32) -> Result<i32> {
        let s = self.status()?;
        let timeout = move_timeout(&s, steps);
        self.expect_moved(Command::MoveBy { steps }, timeout)
    }

    fn expect_moved(&mut self, cmd: Command, timeout: Duration) -> Result<i32> {
        match self.request(cmd.clone(), timeout)? {
            Response::Moved { position } => Ok(position),
            Response::Stopped { position } => bail!("move was stopped at step {position}"),
            r => bail!("unexpected answer to {}: {r:?}", describe(&cmd)),
        }
    }

    pub fn stop(&mut self) -> Result<()> {
        self.request(Command::Stop, Duration::from_secs(2)).map(drop)
    }

    pub fn set_zero(&mut self) -> Result<()> {
        self.request(Command::SetZero, Duration::from_secs(2)).map(drop)
    }

    pub fn enable(&mut self, on: bool) -> Result<()> {
        self.request(Command::Enable(on), Duration::from_secs(2)).map(drop)
    }

    /// Top speed (steps/s) and acceleration (steps/s²).
    pub fn set_motion(&mut self, max_speed: u32, accel: u32) -> Result<()> {
        self.request(Command::SetMotion { max_speed, accel }, Duration::from_secs(2)).map(drop)
    }

    pub fn net_info(&mut self) -> Result<NetInfo> {
        match self.request(Command::NetInfo, Duration::from_secs(2))? {
            Response::NetInfo { ssid, connected, ip } => {
                Ok(NetInfo { ssid: ssid.as_str().to_owned(), connected, ip: Ipv4Addr::from(ip) })
            }
            r => bail!("unexpected answer to netinfo: {r:?}"),
        }
    }

    /// Store Wi-Fi credentials; the turntable restarts and joins that network.
    pub fn set_wifi(&mut self, ssid: &str, password: &str) -> Result<()> {
        let ssid = Ssid::try_from(ssid).map_err(|_| anyhow!("SSID longer than 32 bytes"))?;
        let password = Password::try_from(password).map_err(|_| anyhow!("password longer than 64 bytes"))?;
        if !password.is_empty() && password.len() < 8 {
            bail!("WPA passwords are at least 8 characters");
        }
        self.request(Command::SetWifi { ssid, password }, Duration::from_secs(5)).map(drop)
    }
}

/// A command for messages and logs. Never includes secrets.
fn describe(cmd: &Command) -> String {
    match cmd {
        Command::SetWifi { ssid, .. } => format!("SetWifi {{ ssid: {:?}, password: <hidden> }}", ssid.as_str()),
        other => format!("{other:?}"),
    }
}

/// Generous upper bound for a move of `steps` with the given profile.
fn move_timeout(s: &Status, steps: i32) -> Duration {
    let secs = steps.unsigned_abs() as f64 / s.max_speed.max(1) as f64
        + 2.0 * s.max_speed as f64 / s.accel.max(1) as f64;
    Duration::from_secs_f64(secs * 2.0 + 3.0)
}
