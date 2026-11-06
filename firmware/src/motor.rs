//! Stepper control. Runs alone on the second CPU core (APP_CPU) so Wi-Fi
//! interrupts on core 0 can't disturb the step timing.
//!
//! Core 0 talks to this loop through lock-free state:
//! * [`CMD`]: moves and settings, consumed here.
//! * [`DONE`]: one message per finished move, produced here.
//! * [`STOP`]: set by core 0 to cut a move short (with a ramp down).
//! * [`POSITION`], [`MAX_SPEED`], [`ACCEL`], [`ENABLED`]: readable anywhere.

use core::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering::Relaxed};

use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex as Cs, channel::Channel};
use esp_hal::delay::Delay;
use esp_hal::gpio::{Level, Output};
use esp_hal::time::{Duration, Instant};

/// Step pulse high time. The driver's optocouplers need a few µs.
const PULSE_US: u32 = 5;
/// Delay between changing DIR and the first step pulse.
const DIR_SETUP_US: u32 = 10;
/// Lowest speed used at the start/end of a ramp (steps/s).
const MIN_SPEED: f32 = 50.0;
/// Level on the ENA pin that **enables** the motor. With ENA+ on the GPIO and
/// ENA− on GND, driving it high usually *releases* the motor, so enabled = low.
pub const ENABLE_LEVEL: Level = Level::Low;
/// Upper bound accepted for `SetMotion`.
pub const MAX_SPEED_LIMIT: u32 = 20_000;

pub enum MotorCmd {
    MoveTo(i32),
    SetZero,
    Enable(bool),
}

pub struct MoveDone {
    pub position: i32,
    pub stopped: bool,
}

pub static CMD: Channel<Cs, MotorCmd, 4> = Channel::new();
pub static DONE: Channel<Cs, MoveDone, 2> = Channel::new();
pub static STOP: AtomicBool = AtomicBool::new(false);
pub static POSITION: AtomicI32 = AtomicI32::new(0);
pub static ENABLED: AtomicBool = AtomicBool::new(true);
/// Defaults for 1/16 microstepping (3200 steps/rev): half a turn per second.
pub static MAX_SPEED: AtomicU32 = AtomicU32::new(1600);
pub static ACCEL: AtomicU32 = AtomicU32::new(3200);

pub struct Motor {
    step: Output<'static>,
    dir: Output<'static>,
    ena: Output<'static>,
    delay: Delay,
}

impl Motor {
    pub fn new(step: Output<'static>, dir: Output<'static>, ena: Output<'static>) -> Self {
        Self { step, dir, ena, delay: Delay::new() }
    }

    /// Main loop of core 1. Never returns.
    pub fn run(mut self) -> ! {
        loop {
            if let Ok(cmd) = CMD.try_receive() {
                match cmd {
                    MotorCmd::MoveTo(target) => {
                        let done = self.move_to(target);
                        // Capacity 2 and core 0 drains it; can't realistically fail.
                        let _ = DONE.try_send(done);
                    }
                    MotorCmd::SetZero => POSITION.store(0, Relaxed),
                    MotorCmd::Enable(on) => {
                        ENABLED.store(on, Relaxed);
                        self.ena.set_level(if on { ENABLE_LEVEL } else { !ENABLE_LEVEL });
                    }
                }
            }
        }
    }

    /// Blocking trapezoidal move. Speed at each step is limited by the
    /// distance from the start (accelerate), the distance to the end
    /// (decelerate) and the top speed.
    fn move_to(&mut self, target: i32) -> MoveDone {
        let mut position = POSITION.load(Relaxed);
        let distance = target as i64 - position as i64;
        if distance == 0 {
            return MoveDone { position, stopped: false };
        }
        let forward = distance > 0;
        let mut total = distance.unsigned_abs() as u32;

        self.dir.set_level(if forward { Level::High } else { Level::Low });
        self.delay.delay_micros(DIR_SETUP_US);

        let accel = ACCEL.load(Relaxed) as f32;
        let vmax = MAX_SPEED.load(Relaxed) as f32;
        let mut stopping = false;
        let mut next = Instant::now();
        let mut i: u32 = 0;

        while i < total {
            let from_start = (i + 1) as f32;
            let to_end = (total - i) as f32;
            let v = libm::sqrtf(2.0 * accel * from_start.min(to_end))
                .min(vmax)
                .max(MIN_SPEED);

            if !stopping && STOP.load(Relaxed) {
                stopping = true;
                // Ramp down from the current speed instead of stopping dead.
                let ramp_down = (v * v / (2.0 * accel)) as u32 + 1;
                total = total.min(i + ramp_down);
            }

            while Instant::now() < next {}

            self.step.set_high();
            self.delay.delay_micros(PULSE_US);
            self.step.set_low();
            position += if forward { 1 } else { -1 };
            POSITION.store(position, Relaxed);
            i += 1;
            next += Duration::from_micros((1_000_000.0 / v) as u64);
        }

        MoveDone { position, stopped: stopping && position != target }
    }
}
