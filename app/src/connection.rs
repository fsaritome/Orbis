//! How the app reaches the camera and the turntable (the Connections
//! window). Saved with the app settings and shared with the worker threads.

use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum CameraMode {
    /// USB first, then the Wi-Fi address (if one is set).
    #[default]
    Auto,
    Usb,
    /// Wi-Fi only: the address, or a network search when it's empty.
    Wifi,
}

impl CameraMode {
    pub const ALL: [CameraMode; 3] = [CameraMode::Auto, CameraMode::Usb, CameraMode::Wifi];

    pub fn label(self) -> &'static str {
        match self {
            CameraMode::Auto => "Automatic",
            CameraMode::Usb => "USB only",
            CameraMode::Wifi => "Wi-Fi only",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            CameraMode::Auto => "USB first, then the Wi-Fi address below (if set)",
            CameraMode::Usb => "Camera: CONNECTION MODE → USB TETHER SHOOTING AUTO",
            CameraMode::Wifi => "Camera: PC CONNECTION MODE → WIRELESS TETHER SHOOTING FIXED",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "auto" | "automatic" => Some(CameraMode::Auto),
            "usb" => Some(CameraMode::Usb),
            "wifi" | "wi-fi" => Some(CameraMode::Wifi),
            _ => None,
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            CameraMode::Auto => "auto",
            CameraMode::Usb => "usb",
            CameraMode::Wifi => "wifi",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug, Default)]
#[serde(default)]
pub struct CameraConn {
    pub mode: CameraMode,
    /// Camera IP for Wi-Fi tethering (empty: search the network).
    pub ip: String,
}

impl CameraConn {
    /// Links to try, in order.
    pub fn links(&self) -> Vec<fuji::Link> {
        let ip = self.ip.trim();
        let wifi = if ip.is_empty() { fuji::Link::WifiSearch } else { fuji::Link::Wifi(ip.to_owned()) };
        match self.mode {
            CameraMode::Auto if ip.is_empty() => vec![fuji::Link::Usb],
            CameraMode::Auto => vec![fuji::Link::Usb, wifi],
            CameraMode::Usb => vec![fuji::Link::Usb],
            CameraMode::Wifi => vec![wifi],
        }
    }
}

/// Shared between the GUI and the camera thread.
pub type SharedCameraConn = Arc<Mutex<CameraConn>>;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum TableMode {
    /// Network search, then the ESP32's USB port.
    #[default]
    Auto,
    /// Wi-Fi: the address, or a network search when it's empty.
    Wifi,
    /// USB serial: the port, or the ESP32 port found automatically.
    Usb,
}

impl TableMode {
    pub const ALL: [TableMode; 3] = [TableMode::Auto, TableMode::Wifi, TableMode::Usb];

    pub fn label(self) -> &'static str {
        match self {
            TableMode::Auto => "Automatic",
            TableMode::Wifi => "Wi-Fi",
            TableMode::Usb => "USB cable",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            TableMode::Auto => "Search the network, then USB",
            TableMode::Wifi => "A fixed address, or search the network when empty",
            TableMode::Usb => "The ESP32's serial port (setup, or no Wi-Fi)",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "auto" | "automatic" => Some(TableMode::Auto),
            "wifi" | "wi-fi" => Some(TableMode::Wifi),
            "usb" | "serial" => Some(TableMode::Usb),
            _ => None,
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            TableMode::Auto => "auto",
            TableMode::Wifi => "wifi",
            TableMode::Usb => "usb",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug, Default)]
#[serde(default)]
pub struct TableConn {
    pub mode: TableMode,
    /// IP (optionally :port) for Wi-Fi; empty = search.
    pub host: String,
    /// Serial port like COM5; empty = find the ESP32.
    pub port: String,
}

impl TableConn {
    pub fn open(&self) -> anyhow::Result<rig::Rig> {
        use anyhow::Context;
        let host = self.host.trim();
        let port = self.port.trim();
        match self.mode {
            TableMode::Auto => rig::Rig::open_auto(),
            TableMode::Wifi if host.is_empty() => {
                let found = rig::discover(std::time::Duration::from_millis(1500)).unwrap_or_default();
                let first = found.first().context("no turntable answered on the network")?;
                rig::Rig::connect(first.addr)
            }
            TableMode::Wifi => rig::Rig::connect_host(host),
            TableMode::Usb if port.is_empty() => rig::Rig::open_serial(&rig::find_serial_port()?),
            TableMode::Usb => rig::Rig::open_serial(port),
        }
    }
}
