# End-to-end test of the running Orbis app through its control API.
#   & .\scripts\test-api.ps1                # everything except a real scan
#   & .\scripts\test-api.ps1 -NoMotion      # don't move the turntable
#   & .\scripts\test-api.ps1 -NoShutter     # don't fire the camera
# Needs Orbis running with turntable and camera connected. Puts scan and
# camera settings back the way they were.
param([switch]$NoMotion, [switch]$NoShutter)
$ErrorActionPreference = 'Stop'
$U = 'http://127.0.0.1:7880'
$H = @{ 'X-Orbis' = '1' }
$script:pass = 0; $script:fail = 0; $script:fails = @()

function Api([string]$Method, [string]$Path, $Body = $null, [hashtable]$Headers = $H) {
    $p = @{ Method = $Method; Uri = "$U$Path"; Headers = $Headers; SkipHttpErrorCheck = $true; TimeoutSec = 30 }
    if ($null -ne $Body) { $p.Body = ($Body | ConvertTo-Json -Depth 5 -Compress); $p.ContentType = 'application/json' }
    $r = Invoke-WebRequest @p
    [pscustomobject]@{ Code = [int]$r.StatusCode; Json = ($r.Content | ConvertFrom-Json -Depth 20) }
}
function Check([string]$Name, $Ok, [string]$Detail = '') {
    $Ok = [bool]$Ok
    if ($Ok) { $script:pass++; Write-Host "  PASS  $Name" -ForegroundColor Green }
    else { $script:fail++; $script:fails += "$Name $Detail"; Write-Host "  FAIL  $Name  $Detail" -ForegroundColor Red }
}
function Status { (Api GET /status).Json }
function WaitFor([string]$Name, [scriptblock]$Cond, [int]$Seconds = 30) {
    $until = (Get-Date).AddSeconds($Seconds)
    while ((Get-Date) -lt $until) { if (& $Cond) { Check $Name $true; return $true }; Start-Sleep -Milliseconds 400 }
    Check $Name $false "(timeout ${Seconds}s)"; return $false
}
function Section([string]$t) { Write-Host "`n== $t" -ForegroundColor Cyan }

# ------------------------------------------------------------------ basics
Section 'Basics and security'
$s = Status
Check 'GET /status' ($null -ne $s.turntable)
Check 'turntable connected' ($s.turntable.state -eq 'connected') $s.turntable.state
Check 'camera connected' ($s.camera.state -eq 'connected') $s.camera.state
Check 'GET / lists endpoints' ((Api GET /).Json.endpoints.Count -gt 10)
Check 'GET /log' ($null -ne (Api GET '/log?n=5').Json.lines)
Check 'GET /model' ($null -ne (Api GET /model).Json.quality)
Check 'no X-Orbis header -> 403' ((Api GET /status $null @{}).Code -eq 403)
Check 'foreign Host -> 403' ((Api GET /status $null @{ 'X-Orbis' = '1'; 'Host' = 'evil.example' }).Code -eq 403)
Check 'unknown endpoint -> 404' ((Api POST /nope @{}).Code -eq 404)
Check 'bad JSON -> 400' ((Invoke-WebRequest -Method POST -Uri "$U/camera" -Headers $H -Body '{nope' -SkipHttpErrorCheck).StatusCode -eq 400)

# ------------------------------------------------------------------ scan settings
Section 'Scan settings'
$orig = $s.scan_settings
foreach ($p in 'quick', 'standard', 'fine') {
    $r = Api POST /scan/settings @{ preset = $p }
    Check "preset $p" ($r.Code -eq 200 -and $r.Json.preset -eq (Get-Culture).TextInfo.ToTitleCase($p)) "$($r.Json.preset) $($r.Json.shots_per_ring)"
}
$r = Api POST /scan/settings @{ preset = 'custom'; custom_shots = 24; rings = 2; settle_ms = 400; name = 'api test'; autofocus = $false }
Check 'custom 24 x 2, settle, name' ($r.Json.shots_per_ring -eq 24 -and $r.Json.rings -eq 2 -and $r.Json.settle_ms -eq 400 -and $r.Json.name -eq 'api test')
Check 'angle step 15' ($r.Json.angle_step -eq 15)
Check 'bad preset -> 400' ((Api POST /scan/settings @{ preset = 'ultra' }).Code -eq 400)
Check 'pause without scan -> 409' ((Api POST /scan/pause @{}).Code -eq 409)
Check 'continue without scan -> 409' ((Api POST /scan/continue @{}).Code -eq 409)

# ------------------------------------------------------------------ camera
Section 'Camera settings (each changed and put back)'
$cam = (Api GET /camera).Json.settings
$before = @{}; foreach ($k in $cam.PSObject.Properties.Name) { $before[$k] = $cam.$k.value }
function CamSet([string]$k, $v) { Api POST /camera @{ $k = $v } }
function CamName([string]$k) { (Api GET /camera).Json.settings.$k.name }
$mode = $cam.Mode.name
foreach ($k in 'Aperture', 'ISO', 'White balance', 'Focus', 'Image quality', 'Film simulation') {
    $c = $cam.$k
    if ($k -eq 'Aperture' -and $mode -match '^(P|S)') {
        Check "Aperture locked in mode $mode -> 409" ((CamSet $k 'f/8').Code -eq 409)
        continue
    }
    # "Super Fine" is offered but refused (COMBINATION), and the X-T4 dropped off USB after it once.
    $other = $c.choices | Where-Object { $_ -ne $c.name -and $_ -notmatch "Super Fine" } | Select-Object -First 1
    if (-not $other) { Check "$k has choices" $false; continue }
    $r = CamSet $k $other
    Check "$k -> $other (accepted)" ($r.Code -eq 200) ($r.Json.error)
    WaitFor "$k reads back $other" { (CamName $k) -eq $other } 8 | Out-Null
    $null = CamSet $k $before[$k]
    WaitFor "$k back to $($c.name)" { (CamName $k) -eq $c.name } 8 | Out-Null
}
# The mode comes from the dials; Orbis refuses to set it (it would block the
# shutter on the X-T4). Shutter is only settable when the dials say M or S.
Check 'Mode is read-only -> 400' ((CamSet 'Mode' 'M').Code -eq 400)
$sh = (Api GET /camera).Json.settings.Shutter
if ($sh.choices -contains '1/60') {
    $old = $sh.name
    $null = CamSet 'Shutter' '1/60'
    WaitFor 'Shutter -> 1/60' { (CamName 'Shutter') -eq '1/60' } 8 | Out-Null
    $null = CamSet 'Shutter' $old
} else {
    Write-Host '  skip  shutter (camera dials not on M/S)' -ForegroundColor DarkGray
}
if ($mode -notmatch '^(P|S)') { Check 'value by name (f8 without slash)' ((CamSet 'Aperture' 'f8').Code -eq 200) }
Check 'unknown setting -> 400' ((CamSet 'Zoom' 3).Code -eq 400)
Check 'unknown value -> 400' ((CamSet 'ISO' 'banana').Code -eq 400)
$r = Api POST /camera/preset @{}
WaitFor 'photogrammetry preset (ISO 160, Provia)' { $c = (Api GET /camera).Json.settings; $c.ISO.name -eq 'ISO 160' -and $c.'Film simulation'.name -like 'Provia*' } 10 | Out-Null
# Put everything back (mode first).
$restore = [ordered]@{}
foreach ($k in $before.Keys) { if ($k -notin 'Mode', 'Shutter' -and $null -ne $before[$k]) { $restore[$k] = $before[$k] } }
$null = Api POST /camera $restore
Start-Sleep 2

# ------------------------------------------------------------------ UI
Section 'UI'
$z = (Status).zoom
$null = Api POST /ui/zoom @{ factor = 1.3 }
Check 'zoom 1.3' ([math]::Abs((Status).zoom - 1.3) -lt 0.01)
$null = Api POST /ui/zoom @{ factor = $z }
$png = Join-Path $env:TEMP 'orbis-api-test.png'
$r = Api POST /screenshot @{ path = $png.Replace('\', '/') }
Check 'screenshot' ($r.Code -eq 200 -and (Test-Path $png) -and $r.Json.width -gt 500) "$($r.Json.error)"

# ------------------------------------------------------------------ turntable
if (-not $NoMotion) {
    Section 'Turntable'
    $null = Api POST /turntable/goto @{ degrees = 0 }
    WaitFor 'at 0°' { $t = (Status).turntable; -not $t.moving -and [math]::Abs($t.degrees) -lt 0.2 } 20 | Out-Null
    $speed0 = (Status).turntable.speed_deg_s
    $null = Api POST /turntable/speed @{ deg_s = 120 }
    $null = Api POST /turntable/rotate @{ degrees = 45 }
    WaitFor 'rotate +45 -> 45°' { $t = (Status).turntable; -not $t.moving -and [math]::Abs($t.degrees - 45) -lt 0.2 } 20 | Out-Null
    $null = Api POST /turntable/rotate @{ degrees = -90 }
    WaitFor 'rotate -90 -> 315°' { $t = (Status).turntable; -not $t.moving -and [math]::Abs($t.degrees - 315) -lt 0.2 } 20 | Out-Null
    $null = Api POST /turntable/goto @{ degrees = 180 }
    WaitFor 'goto 180' { $t = (Status).turntable; -not $t.moving -and [math]::Abs($t.degrees - 180) -lt 0.2 } 20 | Out-Null
    $null = Api POST /turntable/zero @{}
    WaitFor 'zero here -> 0°' { [math]::Abs((Status).turntable.degrees) -lt 0.2 } 10 | Out-Null
    $null = Api POST /turntable/rotate @{ degrees = 180 }   # back to the original 0
    WaitFor 'back to start' { $t = (Status).turntable; -not $t.moving -and [math]::Abs($t.degrees - 180) -lt 0.2 } 20 | Out-Null
    $null = Api POST /turntable/zero @{}
    $null = Api POST /turntable/enable @{ on = $false }
    WaitFor 'motor released' { -not (Status).turntable.motor_holds } 10 | Out-Null
    $null = Api POST /turntable/enable @{ on = $true }
    WaitFor 'motor holds' { (Status).turntable.motor_holds } 10 | Out-Null
    $null = Api POST /turntable/speed @{ deg_s = $speed0 }
    Check 'rotate without degrees -> 400' ((Api POST /turntable/rotate @{}).Code -eq 400)

    Section 'Dry runs'
    $null = Api POST /scan/settings @{ preset = 'custom'; custom_shots = 8; rings = 1; settle_ms = 0 }
    Check 'start dry run' ((Api POST /scan/start @{ dry_run = $true }).Code -eq 200)
    WaitFor 'dry run running' { $null -ne (Status).scan } 5 | Out-Null
    Check 'second start -> 409' ((Api POST /scan/start @{ dry_run = $true }).Code -eq 409)
    Check 'camera change during scan -> 409' ((CamSet 'ISO' 'ISO 200').Code -eq 409)
    Check 'move during scan -> 409' ((Api POST /turntable/rotate @{ degrees = 5 }).Code -eq 409)
    $null = Api POST /scan/pause @{}
    WaitFor 'paused' { (Status).scan.paused } 10 | Out-Null
    $d1 = (Status).scan.done; Start-Sleep 2
    Check 'no progress while paused' ((Status).scan.done -eq $d1)
    $null = Api POST /scan/resume @{}
    WaitFor 'dry run 8/8 finished' { $s = Status; $null -eq $s.scan -and $s.last_scan.done -eq 8 } 60 | Out-Null

    $null = Api POST /scan/settings @{ rings = 2 }
    $null = Api POST /scan/start @{ dry_run = $true }
    WaitFor 'waits for ring 2' { (Status).scan.waiting_for_ring -eq 2 } 60 | Out-Null
    Check 'continue ring' ((Api POST /scan/continue @{}).Code -eq 200)
    WaitFor 'ring 2 runs' { (Status).scan.per_ring[1] -gt 0 } 30 | Out-Null
    $null = Api POST /scan/stop @{}
    WaitFor 'stopped' { $null -eq (Status).scan } 20 | Out-Null
    $null = Api POST /turntable/goto @{ degrees = 0 }
    WaitFor 'home again' { $t = (Status).turntable; -not $t.moving -and [math]::Abs($t.degrees) -lt 0.2 } 30 | Out-Null
}

# ------------------------------------------------------------------ live view, focus, shutter
Section 'Live view and focus'
$null = Api POST /camera/live @{ on = $true }
WaitFor 'live view on' { (Status).camera.live_view } 10 | Out-Null
$null = Api POST /camera/focus @{ x = 0.5; y = 0.5 }
Start-Sleep 3
$null = Api POST /camera/focus @{}
Start-Sleep 3
$log = (Api GET '/log?n=12').Json.lines -join "`n"
Check 'focus answered' ($log -match 'focus locked|couldn.t lock')
$null = CamSet 'ISO' 'ISO 200'
Start-Sleep 2
Check 'setting change during live view keeps it on' ((Status).camera.live_view)
$null = CamSet 'ISO' 'ISO 160'
$null = Api POST /camera/live @{ on = $false }
WaitFor 'live view off' { -not (Status).camera.live_view } 10 | Out-Null

if (-not $NoShutter) {
    Section 'Test shot'
    $prev = (Status).preview
    Check 'test shot accepted' ((Api POST /camera/test-shot @{}).Code -eq 200)
    WaitFor 'photo arrived' { $p = (Status).preview; $p -and $p -ne $prev } 45 | Out-Null
}

# ------------------------------------------------------------------ put back
$null = Api POST /scan/settings @{ preset = $orig.preset; rings = $orig.rings; settle_ms = $orig.settle_ms; name = $orig.name; autofocus = $orig.autofocus }
if ($orig.preset -eq 'Custom') { $null = Api POST /scan/settings @{ custom_shots = $orig.shots_per_ring } }

Write-Host "`n$script:pass passed, $script:fail failed" -ForegroundColor ($script:fail ? 'Red' : 'Green')
$script:fails | ForEach-Object { Write-Host "  - $_" -ForegroundColor Red }
