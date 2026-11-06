# Orbis

*Put it on the plate, press start, get a 3D-ready photo set.*

Orbis is an automated photogrammetry rig: a wireless ESP32 turntable plus a
tethered Fujifilm X camera, orchestrated by a Rust app on the PC. The output
is a folder of evenly spaced photos, ready for RealityCapture, Metashape or
Meshroom.

Rust rewrite of two earlier projects:

- **CPP-Fuji-Network-ReleaseButton**: MFC app driving the camera via the
  Fujifilm SDK, with a TCP server on port 27015 (`s1`/`s2`/`status`).
- **ARD_Photogrammetry_Table**: Arduino/PlatformIO ESP32 firmware that
  stepped the turntable with AccelStepper and fired the camera through that
  TCP server over Wi-Fi.

The new design inverts control: the **PC runs the scan**. The camera hangs
on USB, because that's the only reliable way to get full-size photos and
settings out of it. The turntable is a small motion controller on **Wi-Fi**,
so it only needs power.

```
                ┌──────────────── PC (Rust) ────────────────┐
 Camera ◄──USB──┤ fuji (SDK)        scanner + GUI           │
                │ rig ──── TCP 7878 / UDP 7879 discovery ───┼──Wi-Fi──► ESP32 ─► ST-4045 ─► stepper
                └───────────────────────────────────────────┘        (USB only for setup)
```

## Layout

| Path | What | Status |
|---|---|---|
| `rig-protocol/` | Messages shared by PC and firmware (`no_std`, postcard + COBS) | ✅ |
| `firmware/` | ESP32 firmware: motor on core 1, Wi-Fi/TCP/discovery/serial on core 0 | ✅ builds, untested on hardware |
| `rig/` | PC library + `rig` CLI to drive the turntable | ✅ |
| `fuji-sdk-sys/`, `fuji/` | Bindings + safe wrapper for Fujifilm `XAPI.dll` | next |
| `scanner/` | Scan orchestrator + egui GUI with preview | planned |
| `sdk/` | Your copy of the Fujifilm SDK (git-ignored) | local only |

## Hardware

- ESP32-WROOM dev board (ESP32-D0WD-V3, CP210x USB bridge)
- **ST-4045-A1** microstep driver, 12–40 V supply on DC+/DC−
- NEMA 17 stepper (A+ green, A− black, B+ red, B− blue)
- Fujifilm X-T4 on USB
- Power for the ESP32: USB charger, or a 5 V buck converter from the
  driver's supply into the board's 5V/VIN pin

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

# Firmware (separate toolchain, run inside firmware/)
. $env:USERPROFILE\export-esp.ps1
cd firmware
cargo build --release
espflash flash --chip esp32 --port COM5 target\xtensa-esp32-none-elf\release\orbis-firmware
```

**Flashing tip:** if `espflash` stops at "Connecting...", hold **BOOT**, tap
**EN**, then release BOOT. The board's auto-reset circuit is unreliable; a
10 µF capacitor between EN and GND fixes it for good.

## First-time setup

Once, over USB, tell the turntable your Wi-Fi (2.4 GHz). It's stored in the
ESP32's flash and survives reflashing the firmware.

```powershell
cargo run -p rig -- --usb wifi "MyNetwork" --password "secret"
cargo run -p rig -- discover        # after ~10 s: shows its IP
```

From then on it only needs power. `rig` finds it on the network by itself.

## Using the turntable

```powershell
cargo run -p rig -- status
cargo run -p rig -- rotate 90          # relative, degrees
cargo run -p rig -- goto 0             # absolute
cargo run -p rig -- spin --shots 36    # dry-run scan, no camera
cargo run -p rig -- speed 1600 3200    # steps/s, steps/s²
cargo run -p rig -- --gear-ratio 3 rotate 360   # belt/gear drive
cargo run -p rig -- --usb netinfo      # Wi-Fi state, over USB
cargo run -p rig -- --host 192.168.178.42 status
```

## Fujifilm SDK

The Fujifilm X SDK (`XAPI.dll`, `FTLPTP*.dll`, `FF00xxAPI.dll`, headers) is
proprietary and **must not be committed**. Get it from
<https://fujifilm-x.com/global/special/developer/> and copy into `sdk/`:

```
sdk/bin/       REDISTRIBUTABLES/Windows/64bit/*.dll
sdk/include/   HEADERS/XAPI.H, XAPIOpt.H, X-T4.h
sdk/doc/       MANUAL/SDK_ProgrammingReference.pdf
```

## Roadmap

1. ✅ Protocol, turntable CLI
2. ✅ Wi-Fi firmware (discovery, TCP, serial setup, dedicated motor core)
3. ⬜ Flash and test on the real hardware
4. ⬜ `fuji-sdk-sys` / `fuji`: detect camera, lock settings, shoot, download, live view
5. ⬜ `scanner`: rings × shots, settle time, retakes, `scan.json`, resume
6. ⬜ GUI (egui): detail presets, angle wheel, photo preview + thumbnails,
   turntable jog, past scans
7. ⬜ Optional: sharpness check, launch Meshroom/RealityCapture after a scan
