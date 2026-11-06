//! Turntable controller firmware.
//!
//! The ESP32 is a "dumb" motion controller: it turns the stepper motor on
//! request from the PC and reports back once the motor has stopped. All
//! scan logic (angles, camera, files) lives in the PC app.
//!
//! Link: UART0 = the board's USB serial bridge (COM port on the PC), framed
//! with `rig-protocol` (postcard + COBS).
//!
//! Wiring (ST-4045-A1 driver, 3.3 V logic, "common cathode"):
//!   GPIO18 → PUL+   PUL− → GND
//!   GPIO19 → DIR+   DIR− → GND
//!   GPIO23 → ENA+   ENA− → GND   (optional; unconnected = always enabled)
//!   GPIO0  = BOOT button (start/stop scan, reported to the PC)

#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]
#![deny(clippy::large_stack_frames)]

use esp_hal::clock::CpuClock;
use esp_hal::delay::Delay;
use esp_hal::gpio::{Input, InputConfig, Level, Output, OutputConfig, Pull};
use esp_hal::main;
use esp_hal::time::{Duration, Instant};
use esp_hal::uart::{Config as UartConfig, Uart};
use esp_hal::Blocking;
use rig_protocol::{
    decode, encode, Command, ErrorCode, FrameAccumulator, Reply, Request, Response, EVENT_ID,
    MAX_FRAME, PROTOCOL_VERSION,
};

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}

esp_bootloader_esp_idf::esp_app_desc!();

/// Step pulse high time. The driver's optocouplers need a few µs.
const PULSE_US: u32 = 5;
/// Delay between changing DIR and the first step pulse.
const DIR_SETUP_US: u32 = 10;
/// Lowest speed used at the start/end of a ramp (steps/s).
const MIN_SPEED: f32 = 50.0;
/// Level on the ENA pin that **enables** the motor. With ENA+ on the GPIO and
/// ENA− on GND, driving it high usually *releases* the motor, so enabled = low.
const ENABLE_LEVEL: Level = Level::Low;
/// Button debounce time.
const DEBOUNCE: Duration = Duration::from_millis(50);

/// Defaults: 1/16 microstepping → 3200 steps/rev; 1600 steps/s = half a
/// turn per second at the motor.
const DEFAULT_MAX_SPEED: u32 = 1600;
const DEFAULT_ACCEL: u32 = 3200;
const MAX_SPEED_LIMIT: u32 = 20_000;

struct Rig {
    uart: Uart<'static, Blocking>,
    rx: FrameAccumulator<MAX_FRAME>,
    step: Output<'static>,
    dir: Output<'static>,
    ena: Output<'static>,
    button: Input<'static>,
    button_was_down: bool,
    button_changed_at: Instant,
    delay: Delay,
    position: i32,
    enabled: bool,
    max_speed: u32,
    accel: u32,
}

/// What a request received while a move is running asks the move to do.
enum DuringMove {
    Continue,
    Stop,
}

impl Rig {
    fn send(&mut self, id: u16, resp: Response) {
        let mut buf = [0u8; MAX_FRAME];
        if let Ok(frame) = encode(&Reply { id, resp }, &mut buf) {
            let mut rest: &[u8] = frame;
            while !rest.is_empty() {
                match self.uart.write(rest) {
                    Ok(n) => rest = &rest[n..],
                    Err(_) => break,
                }
            }
        }
    }

    fn status(&self, moving: bool) -> Response {
        Response::Status {
            position: self.position,
            moving,
            enabled: self.enabled,
            max_speed: self.max_speed,
            accel: self.accel,
        }
    }

    fn set_enabled(&mut self, on: bool) {
        self.enabled = on;
        let level = if on { ENABLE_LEVEL } else { !ENABLE_LEVEL };
        self.ena.set_level(level);
    }

    /// Read whatever bytes are waiting and return the next complete request.
    /// Undecodable frames are answered with `BadFrame`.
    fn poll_request(&mut self) -> Option<Request> {
        let mut byte = [0u8; 1];
        while self.uart.read_ready() {
            match self.uart.read_buffered(&mut byte) {
                Ok(1) => {}
                _ => {
                    // Line error (noise, wrong baud): drop the partial frame.
                    let _ = self.uart.check_for_rx_errors();
                    return None;
                }
            }
            if let Some(frame) = self.rx.push(byte[0]) {
                match decode::<Request>(frame) {
                    Ok(req) => return Some(req),
                    Err(_) => {
                        self.send(EVENT_ID, Response::Error(ErrorCode::BadFrame));
                        return None;
                    }
                }
            }
        }
        None
    }

    /// Debounced falling edge on the BOOT button → event to the PC.
    fn poll_button(&mut self) {
        let down = self.button.is_low();
        if down != self.button_was_down && self.button_changed_at.elapsed() >= DEBOUNCE {
            self.button_was_down = down;
            self.button_changed_at = Instant::now();
            if down {
                self.send(EVENT_ID, Response::ButtonPressed);
            }
        }
    }

    /// Handle a request while idle.
    fn handle(&mut self, req: Request) {
        let id = req.id;
        match req.cmd {
            Command::Ping => self.send(id, Response::Pong { version: PROTOCOL_VERSION }),
            Command::Status => {
                let s = self.status(false);
                self.send(id, s);
            }
            Command::SetZero => {
                self.position = 0;
                self.send(id, Response::Ok);
            }
            Command::Stop => self.send(id, Response::Ok), // nothing moving
            Command::SetMotion { max_speed, accel } => {
                if max_speed == 0 || accel == 0 || max_speed > MAX_SPEED_LIMIT {
                    self.send(id, Response::Error(ErrorCode::InvalidArgument));
                } else {
                    self.max_speed = max_speed;
                    self.accel = accel;
                    self.send(id, Response::Ok);
                }
            }
            Command::Enable(on) => {
                self.set_enabled(on);
                self.send(id, Response::Ok);
            }
            Command::MoveTo { position } => self.start_move(id, position),
            Command::MoveBy { steps } => match self.position.checked_add(steps) {
                Some(target) => self.start_move(id, target),
                None => self.send(id, Response::Error(ErrorCode::InvalidArgument)),
            },
        }
    }

    /// Handle a request that arrives while the motor is turning.
    fn handle_during_move(&mut self, req: Request) -> DuringMove {
        match req.cmd {
            Command::Status => {
                let s = self.status(true);
                self.send(req.id, s);
                DuringMove::Continue
            }
            Command::Ping => {
                self.send(req.id, Response::Pong { version: PROTOCOL_VERSION });
                DuringMove::Continue
            }
            Command::Stop => {
                self.send(req.id, Response::Ok);
                DuringMove::Stop
            }
            _ => {
                self.send(req.id, Response::Error(ErrorCode::Busy));
                DuringMove::Continue
            }
        }
    }

    /// Blocking trapezoidal move to `target`. Keeps serving the serial line
    /// between steps so `Status` and `Stop` work mid-move. Replies to `id`
    /// only after the motor has come to rest.
    fn start_move(&mut self, id: u16, target: i32) {
        if !self.enabled {
            self.send(id, Response::Error(ErrorCode::Disabled));
            return;
        }
        let distance = target as i64 - self.position as i64;
        if distance == 0 {
            self.send(id, Response::Moved { position: self.position });
            return;
        }
        let forward = distance > 0;
        let mut total = distance.unsigned_abs() as u32;

        self.dir.set_level(if forward { Level::High } else { Level::Low });
        self.delay.delay_micros(DIR_SETUP_US);

        let accel = self.accel as f32;
        let vmax = self.max_speed as f32;
        let mut stopped_early = false;
        let mut next = Instant::now();
        let mut i: u32 = 0;

        while i < total {
            // Speed for this step: limited by distance from start (accel),
            // distance to end (decel) and the top speed.
            let from_start = (i + 1) as f32;
            let to_end = (total - i) as f32;
            let v = libm::sqrtf(2.0 * accel * from_start.min(to_end))
                .min(vmax)
                .max(MIN_SPEED);
            let period = Duration::from_micros((1_000_000.0 / v) as u64);

            // Wait for this step's time slot, serving the serial line.
            while Instant::now() < next {
                if let Some(req) = self.poll_request() {
                    if let DuringMove::Stop = self.handle_during_move(req) {
                        if !stopped_early {
                            stopped_early = true;
                            // Shorten the move so we ramp down from the
                            // current speed instead of stopping dead.
                            let ramp_down = (v * v / (2.0 * accel)) as u32 + 1;
                            total = total.min(i + ramp_down);
                        }
                    }
                }
            }

            self.step.set_high();
            self.delay.delay_micros(PULSE_US);
            self.step.set_low();
            self.position += if forward { 1 } else { -1 };
            i += 1;
            next += period;
        }

        if stopped_early && self.position != target {
            self.send(id, Response::Stopped { position: self.position });
        } else {
            self.send(id, Response::Moved { position: self.position });
        }
    }
}

#[allow(
    clippy::large_stack_frames,
    reason = "it's not unusual to allocate larger buffers etc. in main"
)]
#[main]
fn main() -> ! {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let p = esp_hal::init(config);

    // UART0 on the default pins is the USB serial bridge. Default config is
    // 115200 8N1, matching rig_protocol::BAUD_RATE.
    let uart = Uart::new(p.UART0, UartConfig::default())
        .unwrap()
        .with_rx(p.GPIO3)
        .with_tx(p.GPIO1);

    let out = OutputConfig::default();
    let mut rig = Rig {
        uart,
        rx: FrameAccumulator::new(),
        step: Output::new(p.GPIO18, Level::Low, out),
        dir: Output::new(p.GPIO19, Level::Low, out),
        ena: Output::new(p.GPIO23, ENABLE_LEVEL, out),
        button: Input::new(p.GPIO0, InputConfig::default().with_pull(Pull::Up)),
        button_was_down: false,
        button_changed_at: Instant::now(),
        delay: Delay::new(),
        position: 0,
        enabled: true,
        max_speed: DEFAULT_MAX_SPEED,
        accel: DEFAULT_ACCEL,
    };

    loop {
        if let Some(req) = rig.poll_request() {
            rig.handle(req);
        }
        rig.poll_button();
    }
}
