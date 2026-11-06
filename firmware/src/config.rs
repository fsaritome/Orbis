//! Wi-Fi credentials in flash.
//!
//! Stored in the last 4 KiB sector of the `nvs` partition (the firmware
//! doesn't use ESP-IDF's NVS). Record layout:
//! `"ORB1" | ssid_len u8 | ssid | pass_len u8 | pass | checksum u32 LE`.
//!
//! **Flash is only ever written at boot**, before Wi-Fi and the motor core
//! start. Writing flash disables the instruction cache, and with Wi-Fi
//! running that hangs the chip. So `SetWifi` parks the new record in RTC
//! RAM (which survives a software reset), restarts, and [`apply_pending`]
//! writes it on the way up.

use embassy_time::Timer;
use esp_bootloader_esp_idf::partitions::{self, DataPartitionSubType, PartitionType};
use esp_storage::FlashStorage;
use rig_protocol::{Password, Response, Ssid};

use crate::dispatch::{self, CONFIG_REQ};

const MAGIC: &[u8; 4] = b"ORB1";
const SECTOR: u32 = 4096;
const RECORD_MAX: usize = 4 + 1 + rig_protocol::SSID_MAX + 1 + rig_protocol::PASSWORD_MAX + 4;

/// A record waiting to be written at the next boot. Survives software
/// resets; after power-on it holds garbage, which the checksum rejects.
#[esp_hal::ram(unstable(rtc_fast, persistent))]
static mut PENDING: [u8; RECORD_MAX] = [0; RECORD_MAX];

pub struct Creds {
    pub ssid: Ssid,
    pub password: Password,
}

/// FNV-1a, enough to reject an erased, garbage or half-written record.
fn checksum(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0x811c_9dc5u32, |h, b| (h ^ *b as u32).wrapping_mul(0x0100_0193))
}

/// Serialize; returns the buffer and the used length.
fn encode(ssid: &str, password: &str) -> ([u8; RECORD_MAX], usize) {
    let mut buf = [0xFFu8; RECORD_MAX];
    buf[..4].copy_from_slice(MAGIC);
    buf[4] = ssid.len() as u8;
    let pass_at = 5 + ssid.len();
    buf[5..pass_at].copy_from_slice(ssid.as_bytes());
    buf[pass_at] = password.len() as u8;
    let end = pass_at + 1 + password.len();
    buf[pass_at + 1..end].copy_from_slice(password.as_bytes());
    let sum = checksum(&buf[..end]);
    buf[end..end + 4].copy_from_slice(&sum.to_le_bytes());
    (buf, end + 4)
}

/// Parse a record. An empty SSID is valid (means "forget the network").
fn parse(buf: &[u8; RECORD_MAX]) -> Option<Creds> {
    if &buf[..4] != MAGIC {
        return None;
    }
    let ssid_len = buf[4] as usize;
    if ssid_len > rig_protocol::SSID_MAX {
        return None;
    }
    let pass_at = 5 + ssid_len;
    let pass_len = buf[pass_at] as usize;
    let end = pass_at + 1 + pass_len;
    if pass_len > rig_protocol::PASSWORD_MAX || end + 4 > RECORD_MAX {
        return None;
    }
    let stored = u32::from_le_bytes(buf[end..end + 4].try_into().ok()?);
    if stored != checksum(&buf[..end]) {
        return None;
    }
    let ssid = core::str::from_utf8(&buf[5..pass_at]).ok()?;
    let pass = core::str::from_utf8(&buf[pass_at + 1..end]).ok()?;
    Some(Creds { ssid: Ssid::try_from(ssid).ok()?, password: Password::try_from(pass).ok()? })
}

/// Run `f` on the sector reserved for our record.
fn with_sector<R>(
    flash: &mut FlashStorage<'static>,
    f: impl FnOnce(&mut partitions::FlashRegion<'_, 'static>, u32) -> Result<R, partitions::Error>,
) -> Result<R, partitions::Error> {
    let mut table = [0u8; partitions::PARTITION_TABLE_MAX_LEN];
    let pt = partitions::read_partition_table(flash, &mut table)?;
    let nvs = pt
        .find_partition(PartitionType::Data(DataPartitionSubType::Nvs))?
        .ok_or(partitions::Error::Invalid)?;
    let mut region = nvs.as_flash_region(flash);
    let offset = region.capacity() as u32 - SECTOR;
    f(&mut region, offset)
}

/// Stored network, if any. Call at boot only.
pub fn load(flash: &mut FlashStorage<'static>) -> Option<Creds> {
    let mut buf = [0u8; RECORD_MAX];
    with_sector(flash, |r, off| r.read(off, &mut buf)).ok()?;
    parse(&buf).filter(|c| !c.ssid.is_empty())
}

/// Write a record parked by `SetWifi` before the last restart. Call at boot,
/// before Wi-Fi and the second core start.
pub fn apply_pending(flash: &mut FlashStorage<'static>) {
    // SAFETY: single-threaded at this point in boot.
    let pending = unsafe { *(&raw const PENDING) };
    let Some(creds) = parse(&pending) else { return };
    let (buf, len) = encode(&creds.ssid, &creds.password);
    let _ = with_sector(flash, |r, off| {
        r.erase(off, off + SECTOR)?;
        r.write(off, &buf[..len])
    });
    // Consume it so the next restart doesn't write again.
    unsafe { (*(&raw mut PENDING))[0] = 0 };
}

/// Handles `SetWifi`: park the record, answer, restart.
#[embassy_executor::task]
pub async fn config_task() {
    loop {
        let (origin, id, ssid, password) = CONFIG_REQ.receive().await;
        let (buf, _) = encode(&ssid, &password);
        // SAFETY: only this task touches PENDING after boot.
        unsafe { *(&raw mut PENDING) = buf };
        dispatch::reply(origin, id, Response::Ok);
        // Give the transport time to send the reply, then restart.
        Timer::after_millis(500).await;
        esp_hal::system::software_reset();
    }
}
