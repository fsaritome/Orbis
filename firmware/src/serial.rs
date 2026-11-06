//! Protocol over UART0 (the board's USB serial bridge): first-time Wi-Fi
//! setup and a wired fallback.

use esp_hal::uart::{UartRx, UartTx};
use esp_hal::Async;
use rig_protocol::{decode, encode, ErrorCode, FrameAccumulator, Request, Response, EVENT_ID, MAX_FRAME};

use crate::dispatch::{self, Origin, SERIAL_OUT};

#[embassy_executor::task]
pub async fn serial_rx_task(mut rx: UartRx<'static, Async>) {
    let mut acc = FrameAccumulator::<MAX_FRAME>::new();
    let mut buf = [0u8; 64];
    loop {
        match rx.read_async(&mut buf).await {
            Ok(n) => {
                for &b in &buf[..n] {
                    if let Some(frame) = acc.push(b) {
                        match decode::<Request>(frame) {
                            Ok(req) => dispatch::dispatch(Origin::Serial, req),
                            Err(_) => dispatch::reply(Origin::Serial, EVENT_ID, Response::Error(ErrorCode::BadFrame)),
                        }
                    }
                }
            }
            // Framing/parity error: drop the partial frame.
            Err(_) => acc.reset(),
        }
    }
}

#[embassy_executor::task]
pub async fn serial_tx_task(mut tx: UartTx<'static, Async>) {
    loop {
        let reply = SERIAL_OUT.receive().await;
        let mut out = [0u8; MAX_FRAME];
        let Ok(frame) = encode(&reply, &mut out) else { continue };
        let mut rest: &[u8] = frame;
        while !rest.is_empty() {
            match tx.write_async(rest).await {
                Ok(n) => rest = &rest[n..],
                Err(_) => break,
            }
        }
    }
}
