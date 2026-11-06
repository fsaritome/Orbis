# Downloads the reconstruction tools Orbis drives (CPU builds, no CUDA needed)
# into %LOCALAPPDATA%\Orbis\tools, from their official GitHub releases:
#   COLMAP  (BSD-3)  camera alignment / sparse model
#   OpenMVS (AGPL-3) dense cloud, mesh, texture; source: https://github.com/cdcseacave/openMVS
#   & .\scripts\get-recon-tools.ps1
$ErrorActionPreference = 'Stop'
$tools = @(
    @{ Name = 'colmap';  Url = 'https://github.com/colmap/colmap/releases/download/4.2.1/colmap-x64-windows-nocuda.zip' }
    @{ Name = 'openmvs'; Url = 'https://github.com/cdcseacave/openMVS/releases/download/v2.4.0/OpenMVS_Windows_x64.zip' }
)
$dir = Join-Path $env:LOCALAPPDATA 'Orbis\tools'
New-Item -ItemType Directory -Force $dir | Out-Null
foreach ($t in $tools) {
    $zip = Join-Path $dir "$($t.Name).zip"; $dest = Join-Path $dir $t.Name
    if (Test-Path $dest) { Write-Host "==> $($t.Name): already there ($dest)"; continue }
    Write-Host "==> $($t.Name): $($t.Url)" -ForegroundColor Cyan
    curl.exe -L --fail --progress-bar -o $zip $t.Url
    if ($LASTEXITCODE) { throw "download failed: $($t.Url)" }
    Expand-Archive -Force $zip $dest; Remove-Item $zip
}
Get-ChildItem $dir -Recurse -Include colmap.exe, InterfaceCOLMAP.exe, DensifyPointCloud.exe, ReconstructMesh.exe, RefineMesh.exe, TextureMesh.exe |
    Select-Object -ExpandProperty FullName
Write-Host '==> Done' -ForegroundColor Green
