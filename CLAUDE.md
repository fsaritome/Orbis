# Orbis: notes for Claude

Photogrammetry rig: ESP32 turntable (Wi-Fi) + Fujifilm X-T4 (USB) driven by a
Rust app on the PC. See README.md for architecture and wiring.

## Flashing the ESP32: always this way

The board's auto-reset is unreliable, and `espflash flash` toggles DTR/RTS
when it opens the port, which knocks the board out of download mode. Never
use `espflash flash`, and never run esptool hidden in the background.

1. Run the flash script **in a visible terminal tab** (`run_in_terminal`),
   so the user can watch the progress bar:
   `& .\scripts\flash.ps1 -Port COM5` (add `-NoBuild` if already built).
2. Tell the user: **hold BOOT, tap EN, release BOOT**. The script waits
   forever ("Connecting...") until the board answers. There's no timing to
   hit.
3. Read the tab with `read_terminal` to confirm "Done", or report the error.

`-Full` writes bootloader + partition table + app and **erases the stored
Wi-Fi settings**; use it only for a blank or foreign board. The default
writes only the app at 0x10000 and keeps them.

## Show long-running work

The user wants to see what is happening. Anything that takes more than a few
seconds or involves the hardware (flashing, serial monitors, scans, motor
tests) runs in a visible terminal tab, not as hidden background output.

## Running the app

- Start it in a visible terminal tab: `cargo run --release -p orbis`.
  Close the running window first, or the build can't replace `orbis.exe`.
- To click in the app yourself: close it, `cargo build --release -p
  orbis`, then start it with computer-use `open_application("Orbis")`
  **once**. `open_application` always starts a *new* instance, so never
  call it again while one runs (check `Get-Process orbis`); if another
  window covers Orbis, click inside Orbis or drag its title bar instead.
- The camera, like the turntable, has one controller at a time: close
  the app before `cam ...` commands.
- The turntable accepts **one TCP client at a time**. Never run `rig` while
  the app is connected, and never start a second app window: computer-use
  `open_application("Orbis")` launches a new instance. To look at an
  already running window, just take a screenshot.

## Controlling the running app: use the API

The app serves a local control API on `http://127.0.0.1:7880` (see
`app/src/api.rs`; `GET /` lists the endpoints). Prefer it over computer-use
clicks: it's exact, and it acts like the buttons. Every request needs the
header `X-Orbis: 1`.

```bash
H='X-Orbis: 1'; U=http://127.0.0.1:7880
curl -s -H "$H" $U/status                       # turntable, camera, scan, model
curl -s -H "$H" "$U/log?n=30"
curl -s -H "$H" -d '{"preset":"standard","rings":2}' $U/scan/settings
curl -s -H "$H" -d '{"Aperture":"f/8","ISO":160}' $U/camera
curl -s -H "$H" -d '{"path":"C:/.../shot.png"}' $U/screenshot   # then Read the PNG
```

- Use forward slashes in JSON paths (or escape backslashes).
- It is the same single controller as the GUI: while the app runs, never
  run `rig` or `cam`, so the API is the way to drive the hardware.
- Moving the table or starting a scan changes the real rig: only when the
  user asked for it. Read-only calls (`/status`, `/log`, `/camera`,
  `/model`, `/screenshot`) are always fine.

## 3D reconstruction (`recon` crate)

- COLMAP + OpenMVS CPU builds live in `%LOCALAPPDATA%\Orbis\tools`
  (`scripts\get-recon-tools.ps1` or `recon --install`). This PC has an AMD
  Radeon 780M, no NVIDIA: never use CUDA builds or CUDA-only steps
  (COLMAP `patch_match_stereo`, Meshroom).
- Output goes to `<scan>\model\`. The pipeline only wipes that folder if
  it contains `orbis-recon.log`.
- Reconstructions take minutes: run `recon` in a visible terminal tab.

## Installer

`& .\scripts\package.ps1` (visible terminal tab, Orbis closed) builds
`target\installer\Orbis-Setup-<version>.exe` with Inno Setup 6
(`installer\orbis.iss`). It bundles the proprietary SDK DLLs from `sdk\bin`; the license page makes
users accept `installer\fujifilm-sdk-notice.txt` (required by Fujifilm's SDK
agreement 2(b), 3.2, 5.3). Keep that page. Never upload the setup (GitHub
Release etc.) without the user's OK.

## Environment

- Windows 11, PowerShell. Repo at `C:\GIT\Orbis`; GitHub `fsaritome/Orbis`
  (private).
- ESP32 on **COM5** (CP210x, ESP32-D0WD-V3). Its IP comes from DHCP;
  find it with `rig discover`.
- Firmware toolchain: `. $env:USERPROFILE\export-esp.ps1` first; the
  firmware is its own workspace in `firmware/` (toolchain `esp`, target
  `xtensa-esp32-none-elf`). Build it from inside `firmware/`. PC crates
  build from the repo root with stable Rust.
- Fujifilm SDK 1.33 lives in `sdk/` (git-ignored, from
  `C:\GIT\FUJI_SDK\SDK13300`). Never commit SDK files: they're proprietary.

## Camera troubleshooting (learned the hard way)

- **SHOOT_ERROR (then stuck until power-cycled) whenever the lens is
  stopped down = full press too soon after the half-press.** Wide open
  (f/4) S1 → S2 back to back works; at f/4.5, f/8 the X-T4 aborts. The
  camera's own button works at f/8. Fix: `S1_TO_S2` = 500 ms pause, and
  BUSY retries on every release (like Fujifilm's ReleaseButton sample,
  where S2 comes when the user presses through). Found 2026-11-07 by
  bisecting with `cam shoot --no-af` and `cam set` / `--probe m8`. This was
  probably also behind the older "SetAEMode blocks the shutter" finding
  (setting M changed the aperture); Orbis still never sets the mode.
- **Release always S1ON → S2_S1OFF**, also in MF: the SDK manual
  (XSDK_Release, "IMPORTANT NOTICE") requires the half-press first.
- **A stuck camera answers BUSY to `SetPriorityMode` for a while after
  power-on**: wait a few seconds and connect again.
- **Dials for M on the X-T4: ISO C, shutter T, exposure comp C, aperture
  ring off A.** Not B (bulb). While Orbis holds PC priority the camera
  ignores dial changes: the "↻ dials" button (camera reconnect) reads them.
- **BUSY right after connecting** is normal: `Camera::set` retries ~3 s.
- **SHOOT_ERROR on every release + the camera switching itself off = low
  battery.** Check `battery_percent` in `/status` (or `GET /camera/diag`,
  `cam diag`) before debugging anything else.
- **Never write the camera's power settings** (`SetCustomAutoPowerOff`
  etc.). Setting 255 ("never" per the manual) made the X-T4 switch off on
  its own; it was reset to its original 0. To stop it sleeping, the user
  sets AUTO POWER OFF in the camera menu.
- "RAW + JPEG Super Fine" is offered by the camera but refused
  (COMBINATION); don't pick it in tests.
- The user runs the camera on **USB (USB TETHER SHOOTING FIXED)**. Wi-Fi works
  (22 s per photo vs 2.5 s) but isn't used.
- The camera can also be on **Wi-Fi** (PC CONNECTION MODE → WIRELESS
  TETHER SHOOTING FIXED; address in the app's camera panel or `POST
  /camera/wifi`; `cam --ip <addr> ...`). Orbis tries USB first. Over Wi-Fi
  a dying camera blocks SDK calls for minutes before the disconnect shows.
- After changing the app or API, run `& .\scripts\test-api.ps1` in a
  visible tab (`-NoMotion` / `-NoShutter` to leave the hardware alone).

## Git

- Build, restart and commit locally on your own; **push only when the
  user says so**.
- Commits as `fsaritome <ferit@powdersplash.com>`. No real LAN addresses,
  MACs or passwords in code, docs or commit messages: use examples like
  192.168.1.60.

## Hardware facts

- Driver ST-4045-A1: DIP SW4–6 = ON ON OFF for 1/16 microstepping (all OFF
  or all ON = Standby, motor won't turn). 12–40 V on DC+/DC−.
- Pins: STEP GPIO18, DIR GPIO19, ENA GPIO23, BOOT button GPIO0.
- 200 steps/rev × 16 microsteps = 3200 steps per turn (direct drive).
