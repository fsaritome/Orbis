//! Wi-Fi station, TCP server and discovery responder (core 0).

use core::sync::atomic::{AtomicU32, Ordering::Relaxed};

use embassy_executor::Spawner;
use embassy_futures::select::select;
use embassy_net::tcp::TcpSocket;
use embassy_net::udp::{PacketMetadata, UdpSocket};
use embassy_net::{Runner, Stack, StackResources};
use embassy_time::{Duration, Timer};
use embedded_io_async::Write;
use esp_hal::peripherals::WIFI;
use esp_hal::rng::Rng;
use esp_radio::wifi::{
    sta::StationConfig, AuthenticationMethodConfig, Config, ControllerConfig, Interface, WifiController,
};
use rig_protocol::{
    decode, encode, Announce, ErrorCode, FrameAccumulator, Request, Response, DISCOVERY_MAGIC, DISCOVERY_PORT,
    EVENT_ID, MAX_FRAME, PROTOCOL_VERSION, TCP_PORT,
};
use static_cell::StaticCell;

use crate::config::Creds;
use crate::dispatch::{self, Origin, TCP_OUT};

/// Current IPv4 address as big-endian u32, 0 when not connected.
static IP: AtomicU32 = AtomicU32::new(0);

pub fn ip() -> [u8; 4] {
    IP.load(Relaxed).to_be_bytes()
}

/// Bring up Wi-Fi with `creds` and spawn the network tasks.
pub fn start(spawner: &Spawner, wifi: WIFI<'static>, creds: &Creds, mac: [u8; 6]) {
    let auth = if creds.password.is_empty() {
        AuthenticationMethodConfig::Open
    } else {
        AuthenticationMethodConfig::Wpa2Personal(creds.password.as_str().try_into().unwrap())
    };
    let station = Config::Station(
        StationConfig::default().with_ssid(creds.ssid.as_str().try_into().unwrap()).with_authentication(auth),
    );
    let controller = WifiController::new(wifi, ControllerConfig::default().with_initial_config(station))
        .expect("Wi-Fi init");

    let rng = Rng::new();
    let seed = (rng.random() as u64) << 32 | rng.random() as u64;
    static RESOURCES: StaticCell<StackResources<4>> = StaticCell::new();
    let (stack, runner) = embassy_net::new(
        Interface::station(),
        embassy_net::Config::dhcpv4(Default::default()),
        RESOURCES.init(StackResources::new()),
        seed,
    );

    spawner.spawn(connection_task(controller).unwrap());
    spawner.spawn(net_task(runner).unwrap());
    spawner.spawn(ip_watch_task(stack).unwrap());
    spawner.spawn(tcp_server_task(stack).unwrap());
    spawner.spawn(discovery_task(stack, mac).unwrap());
}

/// Keeps the station connected, retrying every 5 s.
#[embassy_executor::task]
async fn connection_task(mut controller: WifiController<'static>) {
    loop {
        if controller.connect_async().await.is_ok() {
            let _ = controller.wait_for_disconnect_async().await;
        }
        Timer::after(Duration::from_secs(5)).await;
    }
}

#[embassy_executor::task]
async fn net_task(mut runner: Runner<'static, Interface>) {
    runner.run().await
}

#[embassy_executor::task]
async fn ip_watch_task(stack: Stack<'static>) {
    loop {
        let ip = stack.config_v4().map(|c| u32::from_be_bytes(c.address.address().octets())).unwrap_or(0);
        IP.store(ip, Relaxed);
        Timer::after(Duration::from_secs(1)).await;
    }
}

/// One PC at a time on TCP port 7878.
#[embassy_executor::task]
async fn tcp_server_task(stack: Stack<'static>) {
    let mut rx_buf = [0u8; 512];
    let mut tx_buf = [0u8; 512];
    loop {
        let mut sock = TcpSocket::new(stack, &mut rx_buf, &mut tx_buf);
        // Notice a PC that vanished (sleep, cable pulled) within ~15 s.
        sock.set_keep_alive(Some(Duration::from_secs(5)));
        sock.set_timeout(Some(Duration::from_secs(15)));
        if sock.accept(TCP_PORT).await.is_err() {
            continue;
        }
        // Replies queued for a previous client are stale.
        while TCP_OUT.try_receive().is_ok() {}

        {
            let (mut reader, mut writer) = sock.split();
            let read_loop = async {
                let mut acc = FrameAccumulator::<MAX_FRAME>::new();
                let mut buf = [0u8; 64];
                loop {
                    let n = match reader.read(&mut buf).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => n,
                    };
                    for &b in &buf[..n] {
                        if let Some(frame) = acc.push(b) {
                            match decode::<Request>(frame) {
                                Ok(req) => dispatch::dispatch(Origin::Tcp, req),
                                Err(_) => dispatch::reply(Origin::Tcp, EVENT_ID, Response::Error(ErrorCode::BadFrame)),
                            }
                        }
                    }
                }
            };
            let write_loop = async {
                loop {
                    let reply = TCP_OUT.receive().await;
                    let mut out = [0u8; MAX_FRAME];
                    if let Ok(frame) = encode(&reply, &mut out) {
                        if writer.write_all(frame).await.is_err() {
                            return;
                        }
                    }
                }
            };
            select(read_loop, write_loop).await;
        }
        sock.close();
        Timer::after(Duration::from_millis(100)).await;
        sock.abort();
    }
}

/// Answers `ORBIS?` broadcasts so the PC can find the turntable.
#[embassy_executor::task]
async fn discovery_task(stack: Stack<'static>, mac: [u8; 6]) {
    let mut rx_meta = [PacketMetadata::EMPTY; 4];
    let mut rx_buf = [0u8; 128];
    let mut tx_meta = [PacketMetadata::EMPTY; 4];
    let mut tx_buf = [0u8; 128];
    let mut sock = UdpSocket::new(stack, &mut rx_meta, &mut rx_buf, &mut tx_meta, &mut tx_buf);
    if sock.bind(DISCOVERY_PORT).is_err() {
        return;
    }
    let announce = Announce { version: PROTOCOL_VERSION, tcp_port: TCP_PORT, mac };
    let mut out = [0u8; 32];
    let Ok(answer) = postcard::to_slice(&announce, &mut out) else { return };
    let answer: &[u8] = answer;
    let mut buf = [0u8; 16];
    loop {
        if let Ok((n, meta)) = sock.recv_from(&mut buf).await {
            if &buf[..n] == DISCOVERY_MAGIC {
                let _ = sock.send_to(answer, meta.endpoint).await;
            }
        }
    }
}
