# Builds the derived artwork from the cut-out PNGs in docs\img
# (made by tools\make-art.ps1 from docs\art-source):
#   icon*.png        padded to exact squares
#   logo.svg         emblem + "Orbis" as real text (generated images can't do text)
#   social.png       1280x640 GitHub social preview
#   check-dark.png   everything on GitHub's dark background, to spot fringes (not committed)
#   & tools\compose-art.ps1
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
$img = Join-Path $PSScriptRoot '..\docs\img' | Resolve-Path | ForEach-Object Path

$teal = [System.Drawing.ColorTranslator]::FromHtml('#0f5e5c')
$mint = [System.Drawing.ColorTranslator]::FromHtml('#2ec4a6')
$orange = [System.Drawing.ColorTranslator]::FromHtml('#f28c28')
$cream = [System.Drawing.ColorTranslator]::FromHtml('#fff3e0')

function New-Canvas([int]$w, [int]$h) {
    $b = [System.Drawing.Bitmap]::new($w, $h, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($b)
    $g.InterpolationMode = 'HighQualityBicubic'; $g.SmoothingMode = 'AntiAlias'
    $g.PixelOffsetMode = 'HighQuality'; $g.TextRenderingHint = 'AntiAliasGridFit'
    return $b, $g
}

function Draw-Fit($g, $bmp, [float]$x, [float]$y, [float]$w, [float]$h) {
    $s = [Math]::Min($w / $bmp.Width, $h / $bmp.Height)
    $dw = $bmp.Width * $s; $dh = $bmp.Height * $s
    $g.DrawImage($bmp, [System.Drawing.RectangleF]::new($x + ($w - $dw) / 2, $y + ($h - $dh) / 2, $dw, $dh))
}

# The emblem's orbit ring has a gap (for the dot), so the cut-out is
# transparent inside and the dark teal vanishes on dark pages. Put a light
# disc inside the ring: a badge that works on light and dark backgrounds.
function Draw-Emblem($g, $bmp, [float]$x, [float]$y, [float]$size) {
    $inset = $size * 0.06
    $g.FillEllipse([System.Drawing.SolidBrush]::new([System.Drawing.Color]::White), $x + $inset, $y + $inset * 1.3,
        $size - 2 * $inset, $size - 2 * $inset)
    Draw-Fit $g $bmp $x $y $size $size
}

# 1. Square icons (the cut-out is cropped to content, so pad it).
$icon = [System.Drawing.Bitmap]::new((Join-Path $img 'icon.png'))
foreach ($size in 1024, 256, 64, 32) {
    $b, $g = New-Canvas $size $size
    Draw-Fit $g $icon 0 0 $size $size
    $g.Dispose()
    $name = if ($size -eq 1024) { 'icon-square.png' } else { "icon-$size.png" }
    $b.Save((Join-Path $img $name), [System.Drawing.Imaging.ImageFormat]::Png); $b.Dispose()
}
$icon.Dispose()
Move-Item -Force (Join-Path $img 'icon-square.png') (Join-Path $img 'icon.png')

# 2. logo.svg: emblem (embedded PNG) + wordmark as SVG text.
$emb = [System.Drawing.Bitmap]::new((Join-Path $img 'emblem.png'))
$b, $g = New-Canvas 320 320; Draw-Emblem $g $emb 0 0 320; $g.Dispose()
$b.Save((Join-Path $img 'emblem-badge.png'), [System.Drawing.Imaging.ImageFormat]::Png)
$ms = [IO.MemoryStream]::new(); $b.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png); $b.Dispose()
$b64 = [Convert]::ToBase64String($ms.ToArray())
$svg = @"
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 640 170" width="640" height="170" role="img" aria-labelledby="t">
  <title id="t">Orbis</title>
  <image href="data:image/png;base64,$b64" x="0" y="5" width="160" height="160"/>
  <text x="185" y="112" font-family="Segoe UI, Helvetica Neue, Helvetica, Arial, sans-serif" font-size="96"
        font-weight="700" letter-spacing="2" fill="#1d8f84">Orbis</text>
  <text x="190" y="150" font-family="Segoe UI, Helvetica Neue, Helvetica, Arial, sans-serif" font-size="22"
        fill="#8b949e">every side, every angle</text>
</svg>
"@
$svg | Set-Content -Encoding utf8 (Join-Path $img 'logo.svg')

# 3. Social preview 1280x640: cream card, mascot left, name + tagline right.
$mascot = [System.Drawing.Bitmap]::new((Join-Path $img 'mascot.png'))
$b, $g = New-Canvas 1280 640
$g.Clear($cream)
$g.FillRectangle([System.Drawing.SolidBrush]::new($teal), 0, 600, 1280, 40)
$g.FillRectangle([System.Drawing.SolidBrush]::new($orange), 0, 596, 1280, 4)
Draw-Fit $g $mascot 20 40 680 540
Draw-Emblem $g $emb 730 110 100
$font = 'Segoe UI'
$g.DrawString('Orbis', [System.Drawing.Font]::new($font, 92, [System.Drawing.FontStyle]::Bold, 'Pixel'),
    [System.Drawing.SolidBrush]::new($teal), 838, 92)
# Wrapped inside a box so nothing runs off the right edge.
$g.DrawString('Automated photogrammetry: an ESP32 turntable and a Fujifilm X camera, driven by a Rust app.',
    [System.Drawing.Font]::new($font, 32, [System.Drawing.FontStyle]::Regular, 'Pixel'),
    [System.Drawing.SolidBrush]::new([System.Drawing.ColorTranslator]::FromHtml('#33413f')),
    [System.Drawing.RectangleF]::new(734, 250, 500, 170))
$g.DrawString('every side, every angle',
    [System.Drawing.Font]::new($font, 30, [System.Drawing.FontStyle]::Italic, 'Pixel'),
    [System.Drawing.SolidBrush]::new($orange), 734, 440)
$g.Dispose()
$b.Save((Join-Path $img 'social.png'), [System.Drawing.Imaging.ImageFormat]::Png); $b.Dispose()

# 4. Dark-mode check sheet (GitHub dark #0d1117), not part of the repo.
$b, $g = New-Canvas 1400 520
$g.Clear([System.Drawing.ColorTranslator]::FromHtml('#0d1117'))
Draw-Fit $g $mascot 10 10 700 500
Draw-Emblem $g $emb 740 60 260
$ic = [System.Drawing.Bitmap]::new((Join-Path $img 'icon-64.png')); Draw-Fit $g $ic 1040 60 64 64; $ic.Dispose()
$ic = [System.Drawing.Bitmap]::new((Join-Path $img 'icon-32.png')); Draw-Fit $g $ic 1130 76 32 32; $ic.Dispose()
$g.Dispose()
$b.Save((Join-Path $env:TEMP 'orbis-check-dark.png'), [System.Drawing.Imaging.ImageFormat]::Png); $b.Dispose()
$mascot.Dispose(); $emb.Dispose()

Get-ChildItem $img | Select-Object Name, @{ n = 'KB'; e = { [int]($_.Length / 1KB) } } | Format-Table -AutoSize
"dark check: $env:TEMP\orbis-check-dark.png"
