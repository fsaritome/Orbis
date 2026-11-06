# Build and flash the Orbis firmware to the ESP32.
#
#   .\scripts\flash.ps1                 # build + flash the app (keeps Wi-Fi settings)
#   .\scripts\flash.ps1 -Full           # blank/foreign board: bootloader + partition table + app
#   .\scripts\flash.ps1 -Port COM5 -NoBuild
#
# The board's auto-reset is unreliable, and espflash toggles DTR/RTS when it
# opens the port, which knocks the board back out of download mode. So this
# uses esptool with --before no-reset and waits (forever) for you to put the
# board in download mode: hold BOOT, tap EN, release BOOT.
#
# Flash layout (espflash default partition table, 4 MB):
#   0x1000 bootloader | 0x8000 partition table | 0x9000 nvs (Wi-Fi settings)
#   0xF000 phy_init   | 0x10000 factory app
# A normal flash writes only 0x10000+, so the stored Wi-Fi survives.

param(
    [string]$Port,
    [switch]$NoBuild,
    [switch]$Full
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$fw = Join-Path $root 'firmware'
$elf = Join-Path $fw 'target\xtensa-esp32-none-elf\release\orbis-firmware'
$merged = Join-Path $fw 'target\orbis-merged.bin'
$app = Join-Path $fw 'target\orbis-app.bin'
$appOffset = '0x10000'

function Step($text) { Write-Host "`n==> $text" -ForegroundColor Cyan }

$exportEsp = Join-Path $env:USERPROFILE 'export-esp.ps1'
if (Test-Path $exportEsp) { . $exportEsp }

if (-not $NoBuild) {
    Step 'Building firmware'
    Push-Location $fw
    try { cargo build --release; if ($LASTEXITCODE) { throw 'build failed' } }
    finally { Pop-Location }
}

if ($Full) {
    Step 'Creating full image (bootloader + partition table + app) - erases Wi-Fi settings'
    espflash save-image --chip esp32 --merge --skip-padding $elf $merged
    if ($LASTEXITCODE) { throw 'save-image failed' }
    $writeArgs = @('0x0', $merged)
} else {
    Step 'Creating app image (keeps bootloader, partition table and Wi-Fi settings)'
    espflash save-image --chip esp32 $elf $app
    if ($LASTEXITCODE) { throw 'save-image failed' }
    $writeArgs = @($appOffset, $app)
}

if (-not $Port) {
    $Port = (Get-PnpDevice -PresentOnly -Class Ports -ErrorAction SilentlyContinue |
        Where-Object { $_.InstanceId -match 'VID_(10C4|1A86|0403|303A)' } |
        Select-Object -First 1).FriendlyName -replace '.*\((COM\d+)\).*', '$1'
    if (-not $Port) { throw 'No ESP32 serial port found; pass -Port COMx' }
}

Step "Flashing via $Port"
Write-Host ''
Write-Host '   Put the board in download mode now:' -ForegroundColor Yellow
Write-Host '     1. hold BOOT   2. tap EN   3. release BOOT' -ForegroundColor Yellow
Write-Host '   (waiting until it answers...)' -ForegroundColor Yellow
Write-Host ''
python -m esptool --port $Port --before no-reset --after hard-reset --connect-attempts 0 write-flash @writeArgs
if ($LASTEXITCODE) { throw 'flashing failed' }

Step 'Done. Press EN once if the board does not restart by itself.'
