# Logo/mascot/icon drafts for Orbis with OpenAI's image API.
#   & tools\gen-art.ps1 [-Out docs\art-source] [-Only mascot] [-Quality medium]
#
# The API key comes from the DPAPI-encrypted file written by StreamCopy's
# save-openai-key.ps1 (%APPDATA%\scopy\openai-key.xml). It is decrypted in
# memory only, never printed or written anywhere. Only the prompts below are
# sent. Approach and lessons from the StreamCopy project: generated on plain
# white (cut out later with tools\make-art.ps1), fixed hex palette, no text
# in the images (garbled), must not resemble existing brands.
param(
    [string]$Out = (Join-Path $PSScriptRoot '..\docs\art-source'),
    [string]$Only,
    [string]$Quality = 'medium',
    [string]$Model = 'gpt-image-2'
)
$ErrorActionPreference = 'Stop'
$keyFile = "$env:APPDATA\scopy\openai-key.xml"
if (-not (Test-Path $keyFile)) { throw "no key file ($keyFile)" }
$key = [Net.NetworkCredential]::new('', (Import-Clixml $keyFile)).Password
New-Item -ItemType Directory -Force $Out | Out-Null

# Mascot: a chameleon. Its turret eyes swivel independently and see all the
# way round (Orbis photographs every side), it sits still while the world
# turns, and its curled tail is a circle ("orbis"). Palette kept away from
# StreamCopy (sailfish, blues) and the owl used in the Tessera context.
$palette = 'deep teal #0f5e5c, mint green #2ec4a6, warm orange #f28c28 (accent only), cream #fff3e0'
$common = "Plain pure white background (#ffffff), nothing else behind it. No text, no letters, no numbers. " +
          "Do not copy or resemble any existing company's logo or mascot. No owl, no fish."

$jobs = @(
  @{ name = 'logo'; n = 3; size = '1024x1024'; prompt = @"
Design an original, modern logo emblem for a photogrammetry tool called Orbis that photographs objects from every side
on a turntable. Motif: a stylised chameleon in side view whose long tail curls into one large, clean, almost perfect
circle that wraps around the whole emblem like an orbit ring. Its big turret eye is drawn as a camera lens (concentric
rings with a small bright highlight) and looks sideways along the circle. One small dot sits on the tail circle like a
camera travelling around the object. Style: flat vector logo, bold simple shapes, clean negative space, like a premium
tech brand mark. No outlines, no gradients, no realistic shading, no scales texture. Colours: $palette - body in deep
teal with a mint green belly stripe, the lens pupil and the orbit dot in warm orange. Must stay recognisable as a tiny
32 px icon. $common
"@ },
  @{ name = 'mascot'; n = 3; size = '1536x1024'; prompt = @"
An original mascot character for an open-source photogrammetry project called Orbis: a friendly, curious little
chameleon sitting calmly on the edge of a round turntable plate while the plate slowly turns. Its two turret eyes point
in different directions - one eye is a shiny camera lens looking at a small clay vase standing in the middle of the
plate, the other eye looks cheerfully at the viewer. Its tail curls into a neat spiral. A dotted circular orbit with a
small curved arrow goes around the plate to show rotation, and a few tiny photo-frame icons float along the orbit like
snapshots taken from every angle. Semi-flat vector illustration, clean shapes, soft shading only, limited palette:
$palette, a light grey #dfe4ea turntable, white highlights. Patient, clever, approachable. $common
"@ },
  @{ name = 'icon'; n = 3; size = '1024x1024'; prompt = @"
Ultra-simple square app icon: the silhouette of a chameleon's head and front body in side view, with its tail curling
all the way round into a single bold circle that frames the head. The eye is one camera-lens ring with an orange
centre. Deep teal #0f5e5c shapes, eye centre warm orange #f28c28, centred with generous padding. Flat vector, no
outlines, no gradients, no shading, no texture. Must be readable at 16x16 pixels. $common
"@ }
)

foreach ($j in $jobs) {
    if ($Only -and $j.name -ne $Only) { continue }
    Write-Host "`n==> $($j.name): $($j.n) image(s), $($j.size), quality $Quality" -ForegroundColor Cyan
    $body = @{ model = $Model; prompt = $j.prompt; n = $j.n; size = $j.size; quality = $Quality } | ConvertTo-Json
    $t = Get-Date
    $r = Invoke-RestMethod -Method Post -Uri 'https://api.openai.com/v1/images/generations' `
        -Headers @{ Authorization = "Bearer $key" } -ContentType 'application/json' -Body $body -TimeoutSec 300
    $i = 0
    foreach ($d in $r.data) {
        $i++
        $file = Join-Path $Out ('{0}-{1}.png' -f $j.name, $i)
        [IO.File]::WriteAllBytes($file, [Convert]::FromBase64String($d.b64_json))
        '    {0}  ({1:N0} KB)' -f $file, ((Get-Item $file).Length / 1KB)
    }
    '    {0} image(s) in {1:N0} s' -f $i, ((Get-Date) - $t).TotalSeconds
}
$key = $null
