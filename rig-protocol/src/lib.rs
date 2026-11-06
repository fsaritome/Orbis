//! Wire protocol between the PC and the ESP32 turntable controller.
//!
//! Every message is serialized with `postcard` and framed with COBS, so a
//! frame ends with a single `0x00` byte and never contains `0x00` otherwise.
//! That lets either side resynchronise after garbage on the line (e.g. the
//! ESP32 ROM bootloader banner printed at 115200 baud after a reset).
//!
//! The firmware only knows motor steps. Converting degrees to steps
//! (motor steps × microstepping × gear ratio) is the PC's job.

#![cfg_attr(not(feature = "std"), no_std)]

use serde::{Deserialize, Serialize};

/// Bumped whenever a message changes incompatibly.
pub const PROTOCOL_VERSION: u16 = 1;

/// Serial baud rate used by the firmware (UART0, the USB bridge).
pub const BAUD_RATE: u32 = 115_200;

/// Largest encoded frame, including the trailing `0x00`.
pub const MAX_FRAME: usize = 64;

/// PC → ESP32.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Request {
    /// Echoed back in the matching [`Reply`].
    pub id: u16,
    pub cmd: Command,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    /// Answered with [`Response::Pong`].
    Ping,
    /// Answered with [`Response::Status`]; also allowed while moving.
    Status,
    /// Declare the current position to be step 0.
    SetZero,
    /// Move to an absolute position. Answered with [`Response::Moved`]
    /// once the motor has **stopped**, or [`Response::Stopped`] if a
    /// [`Command::Stop`] cut it short.
    MoveTo { position: i32 },
    /// Move relative to the current position. Same answers as `MoveTo`.
    MoveBy { steps: i32 },
    /// Decelerate and stop the current move. Answered with `Ok`.
    Stop,
    /// Motion profile: top speed in steps/s and acceleration in steps/s².
    SetMotion { max_speed: u32, accel: u32 },
    /// Energise (true) or release (false) the motor via the ENA input.
    Enable(bool),
}

/// ESP32 → PC.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reply {
    /// The `id` of the request this answers.
    pub id: u16,
    pub resp: Response,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Response {
    Ok,
    Pong { version: u16 },
    Status {
        position: i32,
        moving: bool,
        enabled: bool,
        max_speed: u32,
        accel: u32,
    },
    /// The move finished at `position`.
    Moved { position: i32 },
    /// The move was stopped early at `position`.
    Stopped { position: i32 },
    Error(ErrorCode),
    /// Unsolicited (sent with `id` = [`EVENT_ID`]): the physical button
    /// on the turntable was pressed. The PC uses it to start/stop a scan.
    ButtonPressed,
}

/// `Reply::id` used for unsolicited events. The PC never sends id 0.
pub const EVENT_ID: u16 = 0;

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    /// A move is in progress; only `Status` and `Stop` are accepted.
    Busy,
    /// The motor is disabled.
    Disabled,
    /// A parameter is out of range.
    InvalidArgument,
    /// The frame could not be decoded.
    BadFrame,
}

/// Encode `msg` as a COBS frame (ending in `0x00`) into `buf`.
pub fn encode<'a, T: Serialize>(msg: &T, buf: &'a mut [u8]) -> postcard::Result<&'a mut [u8]> {
    postcard::to_slice_cobs(msg, buf)
}

/// Decode one COBS frame (as returned by [`FrameAccumulator::push`]).
pub fn decode<'a, T: Deserialize<'a>>(frame: &'a mut [u8]) -> postcard::Result<T> {
    postcard::from_bytes_cobs(frame)
}

/// Collects bytes until a `0x00` delimiter, without allocating.
pub struct FrameAccumulator<const N: usize> {
    buf: [u8; N],
    len: usize,
    overflow: bool,
}

impl<const N: usize> Default for FrameAccumulator<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> FrameAccumulator<N> {
    pub const fn new() -> Self {
        Self { buf: [0; N], len: 0, overflow: false }
    }

    /// Feed one byte. Returns a complete frame (including the trailing
    /// `0x00`) when the delimiter arrives. Over-long frames are dropped.
    pub fn push(&mut self, byte: u8) -> Option<&mut [u8]> {
        if byte == 0 {
            let len = self.len;
            let overflow = self.overflow;
            self.len = 0;
            self.overflow = false;
            if overflow || len == 0 {
                return None;
            }
            self.buf[len] = 0;
            return Some(&mut self.buf[..=len]);
        }
        // Keep one byte free for the delimiter.
        if self.len + 1 >= N {
            self.overflow = true;
        } else {
            self.buf[self.len] = byte;
            self.len += 1;
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip<T>(msg: T)
    where
        T: Serialize + for<'de> Deserialize<'de> + PartialEq + core::fmt::Debug,
    {
        let mut out = [0u8; MAX_FRAME];
        let encoded = encode(&msg, &mut out).unwrap();
        assert_eq!(*encoded.last().unwrap(), 0);
        assert!(!encoded[..encoded.len() - 1].contains(&0));

        // Garbage before the frame (bootloader banner) must not break it.
        let mut acc = FrameAccumulator::<MAX_FRAME>::new();
        for b in b"ets Jul 29 2019\r\n" {
            assert!(acc.push(*b).is_none());
        }
        acc.push(0); // garbage frame ends; decoding it would fail, which is fine
        let mut got = None;
        for b in encoded.iter() {
            if let Some(frame) = acc.push(*b) {
                got = Some(decode::<T>(frame).unwrap());
            }
        }
        assert_eq!(got.unwrap(), msg);
    }

    #[test]
    fn requests_roundtrip() {
        roundtrip(Request { id: 1, cmd: Command::Ping });
        roundtrip(Request { id: 65535, cmd: Command::MoveTo { position: -3200 } });
        roundtrip(Request { id: 7, cmd: Command::SetMotion { max_speed: 4000, accel: 8000 } });
        roundtrip(Request { id: 8, cmd: Command::Enable(false) });
    }

    #[test]
    fn replies_roundtrip() {
        roundtrip(Reply { id: 2, resp: Response::Moved { position: i32::MIN } });
        roundtrip(Reply {
            id: 3,
            resp: Response::Status { position: 12, moving: true, enabled: true, max_speed: u32::MAX, accel: 1 },
        });
        roundtrip(Reply { id: 4, resp: Response::Error(ErrorCode::Busy) });
    }

    #[test]
    fn overlong_frame_is_dropped() {
        let mut acc = FrameAccumulator::<8>::new();
        for _ in 0..20 {
            assert!(acc.push(1).is_none());
        }
        assert!(acc.push(0).is_none());
        // Next frame works again.
        let mut out = [0u8; 8];
        let enc = encode(&Request { id: 1, cmd: Command::Ping }, &mut out).unwrap().to_vec();
        let frame = enc.iter().find_map(|b| acc.push(*b).map(|f| f.to_vec()));
        assert!(frame.is_some());
    }
}
