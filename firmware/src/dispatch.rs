//! Request handling shared by the TCP and serial transports (core 0).

use core::cell::{Cell, RefCell};
use core::sync::atomic::{AtomicBool, Ordering::Relaxed};

use embassy_sync::blocking_mutex::{raw::CriticalSectionRawMutex as Cs, Mutex};
use embassy_sync::channel::Channel;
use rig_protocol::{Command, ErrorCode, Password, Reply, Request, Response, Ssid, EVENT_ID, PROTOCOL_VERSION};

use crate::motor::{self, MotorCmd};
use crate::net;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Tcp,
    Serial,
}

/// Outgoing replies per transport. The transport tasks drain these.
pub static TCP_OUT: Channel<Cs, Reply, 8> = Channel::new();
pub static SERIAL_OUT: Channel<Cs, Reply, 8> = Channel::new();

/// Wi-Fi credentials to store, handled by `config::config_task`.
pub static CONFIG_REQ: Channel<Cs, (Origin, u16, Ssid, Password), 1> = Channel::new();

/// True from accepting a move until its reply is sent.
static MOVING: AtomicBool = AtomicBool::new(false);
/// Who to answer when the running move finishes.
static PENDING_MOVE: Mutex<Cs, Cell<Option<(Origin, u16)>>> = Mutex::new(Cell::new(None));
/// Configured network name, for `NetInfo`.
pub static SSID: Mutex<Cs, RefCell<Ssid>> = Mutex::new(RefCell::new(Ssid::new()));

pub fn reply(origin: Origin, id: u16, resp: Response) {
    let out = match origin {
        Origin::Tcp => &TCP_OUT,
        Origin::Serial => &SERIAL_OUT,
    };
    // If nobody is reading (no TCP client), drop rather than block.
    let _ = out.try_send(Reply { id, resp });
}

/// Send an event to every transport.
pub fn broadcast(resp: Response) {
    reply(Origin::Tcp, EVENT_ID, resp.clone());
    reply(Origin::Serial, EVENT_ID, resp);
}

fn status() -> Response {
    Response::Status {
        position: motor::POSITION.load(Relaxed),
        moving: MOVING.load(Relaxed),
        enabled: motor::ENABLED.load(Relaxed),
        max_speed: motor::MAX_SPEED.load(Relaxed),
        accel: motor::ACCEL.load(Relaxed),
    }
}

pub fn dispatch(origin: Origin, req: Request) {
    let id = req.id;
    let busy = MOVING.load(Relaxed);
    let resp = match req.cmd {
        // Allowed at any time.
        Command::Ping => Response::Pong { version: PROTOCOL_VERSION },
        Command::Status => status(),
        Command::NetInfo => {
            let ip = net::ip();
            Response::NetInfo {
                ssid: SSID.lock(|s| s.borrow().clone()),
                connected: ip != [0; 4],
                ip,
            }
        }
        Command::Stop => {
            if busy {
                motor::STOP.store(true, Relaxed);
            }
            Response::Ok
        }
        // Everything else needs the motor idle.
        _ if busy => Response::Error(ErrorCode::Busy),
        Command::SetZero => {
            let _ = motor::CMD.try_send(MotorCmd::SetZero);
            Response::Ok
        }
        Command::Enable(on) => {
            let _ = motor::CMD.try_send(MotorCmd::Enable(on));
            Response::Ok
        }
        Command::SetMotion { max_speed, accel } => {
            if max_speed == 0 || accel == 0 || max_speed > motor::MAX_SPEED_LIMIT {
                Response::Error(ErrorCode::InvalidArgument)
            } else {
                motor::MAX_SPEED.store(max_speed, Relaxed);
                motor::ACCEL.store(accel, Relaxed);
                Response::Ok
            }
        }
        Command::MoveTo { position } => return start_move(origin, id, Some(position)),
        Command::MoveBy { steps } => {
            let target = motor::POSITION.load(Relaxed).checked_add(steps);
            return start_move(origin, id, target);
        }
        Command::SetWifi { ssid, password } => {
            // Answered by the config task once the flash write is done.
            if CONFIG_REQ.try_send((origin, id, ssid, password)).is_err() {
                Response::Error(ErrorCode::Busy)
            } else {
                return;
            }
        }
    };
    reply(origin, id, resp);
}

fn start_move(origin: Origin, id: u16, target: Option<i32>) {
    let Some(target) = target else {
        return reply(origin, id, Response::Error(ErrorCode::InvalidArgument));
    };
    if !motor::ENABLED.load(Relaxed) {
        return reply(origin, id, Response::Error(ErrorCode::Disabled));
    }
    MOVING.store(true, Relaxed);
    motor::STOP.store(false, Relaxed);
    PENDING_MOVE.lock(|p| p.set(Some((origin, id))));
    if motor::CMD.try_send(MotorCmd::MoveTo(target)).is_err() {
        MOVING.store(false, Relaxed);
        PENDING_MOVE.lock(|p| p.set(None));
        reply(origin, id, Response::Error(ErrorCode::Busy));
    }
}

/// Answers each finished move to whoever started it.
#[embassy_executor::task]
pub async fn move_done_task() {
    loop {
        let done = motor::DONE.receive().await;
        let pending = PENDING_MOVE.lock(|p| p.take());
        MOVING.store(false, Relaxed);
        if let Some((origin, id)) = pending {
            let resp = if done.stopped {
                Response::Stopped { position: done.position }
            } else {
                Response::Moved { position: done.position }
            };
            reply(origin, id, resp);
        }
    }
}
