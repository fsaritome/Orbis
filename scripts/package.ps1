# Builds the Orbis installer: target\installer\Orbis-Setup-<version>.exe
#   & .\scripts\package.ps1            # release build + installer
#   & .\scripts\package.ps1 -NoBuild   # reuse target\release
# Needs Inno Setup 6 (winget install JRSoftware.InnoSetup) and the Fujifilm
# SDK DLLs in sdk\bin. Close Orbis first, or the build can't replace orbis.exe.
param([switch]$NoBuild)
$ErrorActionPreference = 'Stop'
$root = Resolve-Path (Join-Path $PSScriptRoot '..')
Set-Location $root

if (-not (Test-Path 'sdk\bin\XAPI.dll')) { throw 'sdk\bin\XAPI.dll missing: copy the Fujifilm SDK first (see README)' }
$version = (Select-String -Path Cargo.toml -Pattern '^version\s*=\s*"(.+)"').Matches[0].Groups[1].Value

if (-not $NoBuild) {
    if (Get-Process orbis -ErrorAction SilentlyContinue) { throw 'Orbis is running: close it first' }
    Write-Host "==> cargo build --release (orbis, rig, cam) v$version" -ForegroundColor Cyan
    cargo build --release -p orbis -p rig -p fuji --bins
    if ($LASTEXITCODE) { throw 'cargo build failed' }
}
foreach ($exe in 'orbis', 'rig', 'cam') {
    if (-not (Test-Path "target\release\$exe.exe")) { throw "target\release\$exe.exe missing" }
}
if (-not (Test-Path 'docs\img\orbis.ico')) { & "$root\tools\make-ico.ps1" }

# License page: Orbis MIT license + the Fujifilm SDK notice users must accept.
New-Item -ItemType Directory -Force target\installer | Out-Null
$license = (Get-Content -Raw LICENSE) + "`r`n" + (Get-Content -Raw installer\fujifilm-sdk-notice.txt)
[IO.File]::WriteAllText("$root\target\installer\license.txt", $license, [Text.UTF8Encoding]::new($true))

$iscc = @(
    (Get-Command iscc.exe -ErrorAction SilentlyContinue).Source
    "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe"
    "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe"
    "$env:ProgramFiles\Inno Setup 6\ISCC.exe"
) | Where-Object { $_ -and (Test-Path $_) } | Select-Object -First 1
if (-not $iscc) { throw 'Inno Setup 6 not found: winget install JRSoftware.InnoSetup' }

Write-Host "==> $iscc" -ForegroundColor Cyan
& $iscc /Qp "/DAppVersion=$version" installer\orbis.iss
if ($LASTEXITCODE) { throw 'iscc failed' }
$out = Get-Item "target\installer\Orbis-Setup-$version.exe"
Write-Host ("==> Done: {0} ({1:N1} MB)" -f $out.FullName, ($out.Length / 1MB)) -ForegroundColor Green
