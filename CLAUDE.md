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

## Hardware facts

- Driver ST-4045-A1: DIP SW4–6 = ON ON OFF for 1/16 microstepping (all OFF
  or all ON = Standby, motor won't turn). 12–40 V on DC+/DC−.
- Pins: STEP GPIO18, DIR GPIO19, ENA GPIO23, BOOT button GPIO0.
- 200 steps/rev × 16 microsteps = 3200 steps per turn (direct drive).
