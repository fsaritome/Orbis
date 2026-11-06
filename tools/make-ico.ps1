# Packs docs\img\icon.png into a multi-size Windows icon (docs\img\orbis.ico)
# for the installer and shortcuts. Each size is a PNG entry (Vista+).
#   & tools\make-ico.ps1
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
$img = Join-Path $PSScriptRoot '..\docs\img' | Resolve-Path | ForEach-Object Path
$src = [System.Drawing.Bitmap]::new((Join-Path $img 'icon.png'))

$sizes = 16, 24, 32, 48, 64, 128, 256
$pngs = foreach ($s in $sizes) {
    $b = [System.Drawing.Bitmap]::new($s, $s, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($b)
    $g.InterpolationMode = 'HighQualityBicubic'; $g.SmoothingMode = 'AntiAlias'; $g.PixelOffsetMode = 'HighQuality'
    $g.DrawImage($src, 0, 0, $s, $s); $g.Dispose()
    $ms = [IO.MemoryStream]::new(); $b.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png); $b.Dispose()
    , $ms.ToArray()
}
$src.Dispose()

$out = [IO.MemoryStream]::new(); $w = [IO.BinaryWriter]::new($out)
$w.Write([uint16]0); $w.Write([uint16]1); $w.Write([uint16]$sizes.Count)   # ICONDIR
$offset = 6 + 16 * $sizes.Count
for ($i = 0; $i -lt $sizes.Count; $i++) {                                    # ICONDIRENTRY
    $s = $sizes[$i]; $d = $pngs[$i]
    $w.Write([byte]($s % 256)); $w.Write([byte]($s % 256))                   # 256 is stored as 0
    $w.Write([byte]0); $w.Write([byte]0); $w.Write([uint16]1); $w.Write([uint16]32)
    $w.Write([uint32]$d.Length); $w.Write([uint32]$offset); $offset += $d.Length
}
foreach ($d in $pngs) { $w.Write($d) }
$w.Flush()
[IO.File]::WriteAllBytes((Join-Path $img 'orbis.ico'), $out.ToArray())
"wrote $(Join-Path $img 'orbis.ico') ($([int]($out.Length / 1KB)) KB)"
