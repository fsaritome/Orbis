//! Orbis turntable firmware (ESP32-WROOM).
//!
//! * Core 1 turns the stepper motor and does nothing else, so the step
//!   timing stays clean while Wi-Fi is busy (`motor.rs`).
//! * Core 0 runs Wi-Fi, a TCP server on port 7878, a UDP discovery
//!   responder, USB serial and the BOOT button (async, embassy).
//!
//! The PC drives everything; see `rig-protocol` for the messages.
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

extern crate alloc;

mod config;
mod dispatch;
mod motor;
mod net;
mod serial;

use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::gpio::{Input, InputConfig, Level, Output, OutputConfig, Pull};
use esp_hal::system::Stack;
use esp_hal::timer::timg::TimerGroup;
use esp_hal::uart::{Config as UartConfig, Uart};
use esp_storage::FlashStorage;
use rig_protocol::Response;
use static_cell::StaticCell;

esp_bootloader_esp_idf::esp_app_desc!();

/// Debounced BOOT button → `ButtonPressed` event to the PC.
#[embassy_executor::task]
async fn button_task(mut button: Input<'static>) {
    loop {
        button.wait_for_falling_edge().await;
        Timer::after(Duration::from_millis(30)).await;
        if button.is_low() {
            dispatch::broadcast(Response::ButtonPressed);
            button.wait_for_high().await;
            Timer::after(Duration::from_millis(30)).await;
        }
    }
}

#[allow(
    clippy::large_stack_frames,
    reason = "it's not unusual to allocate larger buffers etc. in main"
)]
#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    let p = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));
    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 98768);

    let timg0 = TimerGroup::new(p.TIMG0);
    esp_rtos::start(timg0.timer0, p.FROM_CPU_INTR0);

    // All flash access happens here, before Wi-Fi and the second core run
    // (see config.rs for why).
    let mut flash = FlashStorage::new(p.FLASH);
    config::apply_pending(&mut flash);
    let creds = config::load(&mut flash);
    drop(flash);

    // Motor → core 1.
    let out = OutputConfig::default();
    let motor = motor::Motor::new(
        Output::new(p.GPIO18, Level::Low, out),
        Output::new(p.GPIO19, Level::Low, out),
        Output::new(p.GPIO23, motor::ENABLE_LEVEL, out),
    );
    static APP_CORE_STACK: StaticCell<Stack<8192>> = StaticCell::new();
    esp_rtos::start_second_core(p.CPU_CTRL, p.FROM_CPU_INTR1, APP_CORE_STACK.init(Stack::new()), move || {
        motor.run()
    });

    // USB serial.
    let (rx, tx) = Uart::new(p.UART0, UartConfig::default())
        .unwrap()
        .with_rx(p.GPIO3)
        .with_tx(p.GPIO1)
        .into_async()
        .split();
    spawner.spawn(serial::serial_rx_task(rx).unwrap());
    spawner.spawn(serial::serial_tx_task(tx).unwrap());

    spawner.spawn(dispatch::move_done_task().unwrap());
    spawner.spawn(config::config_task().unwrap());
    spawner.spawn(button_task(Input::new(p.GPIO0, InputConfig::default().with_pull(Pull::Up))).unwrap());

    // Wi-Fi, if configured (`rig wifi` over USB sets it up).
    if let Some(creds) = creds {
        dispatch::SSID.lock(|s| *s.borrow_mut() = creds.ssid.clone());
        let mut mac = [0u8; 6];
        mac.copy_from_slice(esp_hal::efuse::base_mac_address().as_bytes());
        net::start(&spawner, p.WIFI, &creds, mac);
    }

    loop {
        Timer::after(Duration::from_secs(3600)).await;
    }
}
