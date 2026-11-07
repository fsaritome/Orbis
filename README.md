<p align="center"><img src="docs/img/logo.svg" alt="Orbis logo: a chameleon whose curled tail forms an orbit ring, next to the word Orbis" width="560"></p>

<p align="center"><em>Put it on the plate, press start, get a 3D-ready photo set.</em></p>

Orbis is an automated photogrammetry rig: a wireless ESP32 turntable plus a
tethered Fujifilm X camera, orchestrated by a Rust app on the PC. The output
is a folder of evenly spaced photos, ready for RealityCapture, Metashape or
Meshroom.

<p align="center"><img src="docs/img/mascot.png" alt="The Orbis mascot, a friendly chameleon sitting on a turntable, aims its camera-lens eye at a clay vase while snapshots of the vase circle the plate on a dotted orbit." width="520"></p>

Why a chameleon: its eyes swivel on their own and see all the way round, it
sits perfectly still while the world turns, and its tail curls into a
circle, which is what *orbis* means.

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
| `firmware/` | ESP32 firmware: motor on core 1, Wi-Fi/TCP/discovery/serial on core 0 | ✅ tested on hardware |
| `rig/` | PC library + `rig` CLI to drive the turntable | ✅ |
| `fuji/` | Bindings + safe wrapper for Fujifilm `XAPI.dll` (USB and Wi-Fi tethering), and the `cam` test CLI | ✅ tested on the X-T4 |
| `recon/` | Photogrammetry: background masks + COLMAP/OpenMVS pipeline, and the `recon` CLI | 🚧 masks tested on real photos; full model awaits a 36-photo scan |
| `app/` | `orbis` desktop app (egui): scans, camera control, live view, 3D model, local control API | ✅ |
| `installer/`, `scripts/` | Windows installer, flashing, packaging, API end-to-end test | ✅ |
| `sdk/` | Your copy of the Fujifilm SDK (git-ignored) | local only |
| `docs/img/`, `tools/*-art.ps1` | Logo, mascot, icons, social preview and the scripts that make them | ✅ |

## Hardware

- ESP32-WROOM dev board (ESP32-D0WD-V3, CP210x USB bridge)
- **ST-4045-A1** microstep driver, 12–40 V supply on DC+/DC−
- NEMA 17 stepper (A+ green, A− black, B+ red, B− blue)
- Fujifilm X-T4 on USB (or Wi-Fi). For long sessions: a USB-C PD supply
  or the CP-W235 DC coupler, since tethering doesn't charge the battery
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

# Firmware: build + flash (keeps the stored Wi-Fi settings)
.\scripts\flash.ps1 -Port COM5
# Blank or foreign board: also writes bootloader + partition table
.\scripts\flash.ps1 -Port COM5 -Full
```

The script waits at "Connecting..." until the board is in download mode:
**hold BOOT, tap EN, release BOOT**. Don't use `espflash flash` on this
board: it toggles the reset lines when opening the port, which knocks the
board back out of download mode. A 10 µF capacitor between EN and GND
fixes the board's unreliable auto-reset for good.

## First-time setup

Once, over USB, tell the turntable your Wi-Fi (2.4 GHz). It's stored in the
ESP32's flash and survives reflashing the firmware.

```powershell
cargo run -p rig -- --usb wifi "MyNetwork" --password "secret"
cargo run -p rig -- discover        # after ~10 s: shows its IP
```

From then on it only needs power. `rig` finds it on the network by itself.

## The Orbis app

```powershell
cargo run --release -p orbis
```

- **Scan:** name, detail preset, rings, settle time, time/disk estimate.
  *Start scan* turns, settles, shoots and saves every position;
  *Dry run* turns only.
- **Camera:** aperture, shutter, ISO, white balance, focus (MF/AF-S/AF-C),
  image quality and film simulation from the app; value lists come from the
  camera itself. The **mode** follows the camera's dials and is shown
  read-only (see Camera setup). **✨ Photogrammetry settings** sets ISO
  160, daylight WB, Provia, RAW + JPEG Fine in one click; tips
  flag anything that would make photos inconsistent. Your choices are
  re-applied on every connect (the X-T4 returns to its dials when the app
  lets go). Autofocus toggle and test shot. Connects over USB or Wi-Fi.
- **Battery:** the camera's charge in the top bar. At 20 % or less the
  X-T4 refuses shots and then switches off, so Orbis warns and won't start
  a real scan (dry runs still work).
- **Live view:** with click-to-focus; settings changed during live view
  restart it with the new exposure.
- **Preview:** the latest photo large, thumbnails below (click to view,
  double-click to open).
- **Angle wheel:** progress per ring and the live table angle.
- **Turntable:** jog, zero, hold, speed, mechanics.
- **3D model:** build a textured mesh from a finished scan (see below).
- **Text size:** A− / A+ in the top bar (or Ctrl + / Ctrl −), remembered.

The BOOT button on the turntable starts/stops a scan and confirms ring
changes. A failed photo pauses the scan; fix the cause and press Resume
to retry that position.

Photos go to `Pictures\Orbis\<date>_<time>_<name>\ring1_001.JPG …`
together with `scan.json` (angle, ring, files and time of every photo).

### Windows installer

```powershell
winget install JRSoftware.InnoSetup   # once
.\scripts\package.ps1                 # -> target\installer\Orbis-Setup-<version>.exe
```

Builds `orbis`, `rig` and `cam` in release mode and packs them with the
Fujifilm DLLs from your `sdk\bin` (so the SDK has to be there first). The
setup installs per user, without an admin prompt, into
`%LOCALAPPDATA%\Programs\Orbis`, with a Start-menu shortcut, an optional
desktop shortcut, optionally `rig`/`cam` on PATH, and an uninstaller. The
icon (`docs\img\orbis.ico`) comes from `tools\make-ico.ps1`.

Fujifilm's [SDK agreement](https://www.fujifilm-x.com/global/camera-control-sdk/agreement/)
allows shipping the `REDISTRIBUTABLE` DLLs inside an app like this, provided
users accept the same obligations and are told Fujifilm offers no support or
warranty. The setup's license page therefore shows the MIT license plus
[installer/fujifilm-sdk-notice.txt](installer/fujifilm-sdk-notice.txt), which
the user must accept. Refresh `sdk\bin` when Fujifilm releases bug fixes. The
DLLs still never go into git: they're not under the MIT license.

### Control API

While it runs, the app also listens on `http://127.0.0.1:7880` (loopback
only) for scripts: status, log, scan settings and start/stop, camera
settings, turntable moves, 3D model builds, UI zoom and window
screenshots. Each request needs the header `X-Orbis: 1`. Browsers can't
send that cross-site, so web pages can't drive the rig. `GET /` lists every
endpoint.

```powershell
curl.exe -s -H "X-Orbis: 1" http://127.0.0.1:7880/status
curl.exe -s -H "X-Orbis: 1" -d '{\"degrees\":90}' http://127.0.0.1:7880/turntable/rotate
```

`scripts\test-api.ps1` runs an end-to-end test of the running app through
the API: every setting, turntable moves, dry runs, live view, a test shot.

### 3D model

The **🧊 Build 3D model** section turns a scan into a textured mesh,
`<scan>\model\model.obj`, right in the app. Pick the scan, a quality
(Draft / Standard / High) and go. It runs in the background at low priority
and can be cancelled.

Orbis doesn't reconstruct anything itself. It prepares the photos and
drives two open-source tools, both on the CPU, so no NVIDIA GPU is needed:

| Step | Tool |
|---|---|
| Working copies + **background masks** | Orbis (`recon` crate) |
| Features, matching, camera alignment, undistortion | [COLMAP](https://github.com/colmap/colmap) (BSD-3) |
| Dense point cloud, mesh, texture | [OpenMVS](https://github.com/cdcseacave/openMVS) (AGPL-3) |

**Why masks:** on a turntable the camera and background stand still while
the object turns. To the reconstruction that's two worlds moving against
each other. Within a ring, the per-pixel median of all photos is the static
scene, so whatever differs from it is the object. Orbis masks out the rest
(wall, stand, cables), with no ML model needed. Check the masks with
`cargo run -p recon -- <scan folder> --masks-only`.

**For good results:** 36+ photos per ring (Standard preset), 2–3 rings,
f/8–f/11 so the whole object is sharp, even light, and an object with
texture (plain white or shiny surfaces don't work; matte spray or a
pattern helps).

The tools aren't bundled. On first use Orbis downloads the official CPU
builds (≈140 MB) into `%LOCALAPPDATA%\Orbis\tools`, or run
`.\scripts\get-recon-tools.ps1`. They run as separate programs under their
own licenses. Command line: `cargo run --release -p recon -- <scan folder>
--quality draft|standard|high`.

### Camera setup (X-T4)

- MENU → wrench → CONNECTION SETTING → PC CONNECTION MODE →
  **USB TETHER SHOOTING FIXED** (stays in tether mode through reconnects;
  AUTO works too), then replug USB. Use a data USB-C cable: some only
  charge. *🔌 Connections* in the app picks USB or Wi-Fi.
- **Or over Wi-Fi:** CONNECTION SETTING → WIRELESS SETTINGS (access point,
  IP address), then PC CONNECTION MODE → **WIRELESS TETHER SHOOTING FIXED**,
  and enter the camera's IP under *Wi-Fi address* in the camera panel.
  Orbis tries USB first, then Wi-Fi. Photos arrive slower (≈45 MB RAW +
  JPEG each) and Wi-Fi drains the battery faster: JPEG only and a
  DC coupler (CP-W235) help. `cam --ip <address> info` tests it.
- **Dials, set once, then leave them:**

  | Control | Position |
  |---|---|
  | ISO dial | **C** |
  | Shutter speed dial | **T** (not B: bulb doesn't work tethered) |
  | Exposure compensation dial | **C** |
  | Aperture ring | **any value** (not A) |
  | Focus mode lever (front) | **M** |
  | Drive dial | **S** (single frame) |

  That's mode M, and aperture, shutter and ISO are all set from the app
  (tested: f/4 to f/22, 1/125 to 2 s, 36-photo scans). Orbis never sets
  the mode itself. While the app controls the camera, the X-T4 ignores
  its dials: after changing one, press **↻ dials** next to the mode. In
  P/A/S the camera picks aperture and/or shutter, so the app shows those
  read-only.
- Everything else is set from the app. Best results: press
  ✨ Photogrammetry settings, pick aperture/shutter for your light, focus
  once (test shot with autofocus), then set Focus = MF and switch off
  "Autofocus before every photo".
- Test without the app (close it first, only one program can control the
  camera): `cargo run -p fuji --bin cam -- info | settings | shoot | diag`.
- **Power:** tethering runs the camera on its battery, and the X-T4 only
  charges over USB while switched off. SHOOT_ERROR on every shot, followed by
  the camera switching off, means the battery is empty. Set AUTO POWER OFF in
  the camera menu if it dozes off between scans.

Only one program can talk to the turntable at a time: close the app before
using `rig`, and vice versa.

## Using the turntable from the command line

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
3. ✅ Flashed and tested on the real hardware (Wi-Fi, discovery, motor)
4. ✅ `fuji`: detect, open, shoot, transfer, read/set all settings
   (tested on the X-T4)
5. ✅ Scans with photos, `scan.json`, pause + retry on failure (tested)
6. ✅ GUI: detail presets, angle wheel, photo preview + thumbnails,
   camera panel, turntable jog
7. ✅ Camera settings from the app, live view with click-to-focus, battery
   display, Wi-Fi tethering
8. ✅ Local control API and end-to-end test script; Windows installer
9. 🚧 3D model in the app (masks + COLMAP + OpenMVS): pipeline runs, first
   full model from a 36-photo scan still to come
10. ⬜ Retake a single photo, past-scans browser, sharpness check, return
    to 0° after a scan
