# Fuji Scanner

Automated photogrammetry rig: an ESP32-driven turntable plus a tethered
Fujifilm X camera, orchestrated by a Rust app on the PC. The output is a folder
of evenly spaced photos, ready for RealityCapture, Metashape or Meshroom.

Rust rewrite of two earlier projects:

- **CPP-Fuji-Network-ReleaseButton**: MFC app driving the camera via the
  Fujifilm SDK, with a TCP server on port 27015 (`s1`/`s2`/`status`).
- **ARD_Photogrammetry_Table**: Arduino/PlatformIO ESP32 firmware that
  stepped the turntable with AccelStepper and fired the camera through that
  TCP server over Wi-Fi.

The new design inverts control: the **PC runs the scan** and talks to the
camera over USB (SDK) and to the turntable over USB serial. The ESP32 is a
small motion controller. There's no Wi-Fi to configure, no TCP hop, and the
images are transferred straight to the PC.

```
          ┌──────────── PC (Rust) ─────────────┐
 Camera ◄─┤ fuji (SDK, planned)   scanner (planned) │
   USB    │ rig  ── postcard/COBS over serial ─┼─► ESP32 ─► ST-4045 driver ─► stepper
          └────────────────────────────────────┘    USB
```

## Layout

| Path | What | Status |
|---|---|---|
| `rig-protocol/` | Message types shared by PC and firmware (`no_std`, postcard + COBS) | ✅ |
| `firmware/` | ESP32 turntable firmware (esp-hal 1.2, bare metal) | ✅ builds |
| `rig/` | PC library + `rig` CLI to drive the turntable | ✅ builds |
| `fuji-sdk-sys/`, `fuji/` | Bindings + safe wrapper for Fujifilm `XAPI.dll` | planned |
| `scanner/` | Scan orchestrator: rotate → settle → shoot → download | planned |

## Hardware

- ESP32 dev board (ESP32-D0WD-V3, CP210x USB bridge)
- **ST-4045-A1** microstep driver, 12–40 V supply on DC+/DC−
- NEMA 17 stepper (A+ green, A− black, B+ red, B− blue)
- Fujifilm X-T4 on USB

### Driver DIP switches

| | SW1 | SW2 | SW3 | SW4 | SW5 | SW6 |
|---|---|---|---|---|---|---|
| **1.8 A, 1/16 microstep** | OFF | ON | OFF | ON | ON | OFF |

SW1–3 set the current: match the motor's rated current (table on the
driver). SW4–6 set microstepping. Both "all OFF" and "all ON" mean
**Standby**: the motor won't turn.

### Wiring (3.3 V logic)

| ESP32 | Driver |
|---|---|
| GPIO18 | PUL+ |
| GPIO19 | DIR+ |
| GPIO23 | ENA+ (optional) |
| GND | PUL−, DIR−, ENA− |
| GPIO0 (BOOT button) | start/stop scan |

The driver's inputs are labelled for 5 V, but driving the "+" pins from
3.3 V GPIOs with "−" to ground usually works. If the motor stutters or
misses steps, add a 74AHCT125 level shifter.

## Build

Prerequisites: Rust (stable) for the PC side; for the firmware,
`cargo install espup espflash` then `espup install`.

```powershell
# PC side
cargo test --workspace
cargo run -p rig -- ports

# Firmware (separate toolchain, run inside firmware/)
. $env:USERPROFILE\export-esp.ps1
cd firmware
cargo build --release
espflash flash --chip esp32 --port COM5 target\xtensa-esp32-none-elf\release\firmware
```

**Flashing tip:** if `espflash` stops at "Connecting...", hold **BOOT**, tap
**EN**, then release BOOT. The board's auto-reset circuit is unreliable; a
10 µF capacitor between EN and GND fixes it for good.

## Using the turntable

```powershell
cargo run -p rig -- status
cargo run -p rig -- rotate 90          # relative, degrees
cargo run -p rig -- goto 0             # absolute
cargo run -p rig -- spin --shots 36    # dry-run scan, no camera
cargo run -p rig -- speed 1600 3200    # steps/s, steps/s²
cargo run -p rig -- --gear-ratio 3 rotate 360   # belt/gear drive
```

## Fujifilm SDK

The Fujifilm X SDK (`XAPI.dll`, `XAPI.H`, `FTLPTP*.dll`) is proprietary and
**must not be committed**. Get it from
<https://fujifilm-x.com/global/special/developer/> and place the x64
files in `sdk/` (git-ignored).

## Roadmap

1. ✅ Protocol, firmware, turntable CLI
2. ⬜ Flash the firmware and test the motor on the real hardware
3. ⬜ `fuji-sdk-sys` / `fuji`: detect camera, shoot, download image (tethered)
4. ⬜ `scanner`: full scan (rings × shots, settle time, locked exposure,
   `scan.json` metadata, resume)
5. ⬜ GUI (egui) with live progress and thumbnails
6. ⬜ Optional: launch Meshroom/COLMAP when a scan finishes
