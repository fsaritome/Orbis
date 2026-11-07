# Orbis control API

While the Orbis app runs, it serves a small HTTP/JSON API so other programs
can drive the rig: start scans, change camera settings, turn the table,
build 3D models, or just watch progress. Everything the buttons do, the API
can do too, and both act on the same single connection to the camera and
turntable.

- **Address:** `http://127.0.0.1:7880`, loopback only. Other machines can't
  reach it; put your own proxy in front if you need remote access.
- **Header:** every request must send `X-Orbis: 1`. Browsers can't add a
  custom header to a cross-site request, so a web page you visit can't drive
  the rig. The `Host` header must be a loopback address too.
- **Format:** request bodies are JSON (`Content-Type` is not checked);
  responses are JSON. `GET /` lists every endpoint.
- **Errors:** `400` bad request, `403` missing header or foreign host, `404`
  unknown endpoint, `409` not possible right now (scan running, camera not
  connected, …), `504` the camera didn't answer in time. Error bodies look
  like `{"error": "camera not connected"}`.
- **One controller:** while the app runs it owns the camera and the
  turntable. Use the API instead of opening them yourself.

## Quick start

```bash
H='X-Orbis: 1'; U=http://127.0.0.1:7880
curl -s -H "$H" $U/status
curl -s -H "$H" -d '{"preset":"standard","rings":1,"settle_ms":1500}' $U/scan/settings
curl -s -H "$H" -d '{"dry_run":false}' $U/scan/start
curl -s -H "$H" "$U/log?n=20"
```

PowerShell:

```powershell
$h = @{ 'X-Orbis' = '1' }
Invoke-RestMethod http://127.0.0.1:7880/status -Headers $h
Invoke-RestMethod http://127.0.0.1:7880/turntable/rotate -Method Post -Headers $h -Body '{"degrees":90}'
```

Python:

```python
import requests
api = requests.Session()
api.headers["X-Orbis"] = "1"
U = "http://127.0.0.1:7880"
print(api.get(f"{U}/status").json()["scan"])
api.post(f"{U}/camera", json={"Aperture": "f/11", "Shutter": "1/4"})
```

## Status and log

### `GET /status`

Everything at a glance. Abridged:

```json
{
  "turntable": { "state": "connected", "via": "192.168.1.60:7878 (Wi-Fi)",
                 "degrees": 0.0, "moving": false, "motor_holds": true, "speed_deg_s": 180.0 },
  "camera": { "state": "connected", "model": "X-T4", "lens": "XF16-80mmF4 R OIS WR",
              "link": "USB", "battery_percent": 63, "live_view": false, "test_shot_busy": false,
              "settings": { "Mode": "M (manual)", "Aperture": "f/11", "Shutter": "1/4", "ISO": "ISO 160", "...": "..." } },
  "scan_settings": { "preset": "Standard", "rings": 1, "shots_per_ring": 36, "angle_step": 10.0,
                     "settle_ms": 1500, "autofocus": false, "name": "vase", "scans_root": "C:\\Users\\me\\Pictures\\Orbis" },
  "scan": { "dir": "...", "done": 12, "total": 36, "rings": 1, "shots_per_ring": 36,
            "per_ring": [12], "paused": false, "waiting_for_ring": null, "elapsed_s": 40, "dry_run": false },
  "last_scan": null,
  "preview": "C:\\...\\ring1_012.JPG",
  "transfer": { "progress": 0.62, "label": "⇣ 21 MB/s" },
  "model": { "running": null, "last_result": null, "quality": "Standard", "masks": true, "...": "..." },
  "zoom": 1.0
}
```

- `scan` is `null` when no scan runs; `last_scan` keeps the finished one.
- `transfer.progress` is 0..1 while a photo travels from the camera to the
  PC (`null` when idle). The SDK reads each image in one call, so values
  between image boundaries are estimated from the measured speed.
  `transfer.label` holds the live speed, then the last result
  (`"13 MB · 0.7 s · 19 MB/s"`).

### `GET /log?n=50`

The last `n` log lines, oldest first: `{"lines": ["12:18:07  ring 1 · position 1 · 0.0° → ring1_001.JPG", ...]}`.

## Scans

| Endpoint | Body | Notes |
|---|---|---|
| `POST /scan/settings` | `{preset, custom_shots, rings, settle_ms, name, autofocus}`, all optional | `preset`: `quick` (24/ring), `standard` (36), `fine` (72), `custom` (uses `custom_shots`, 4–360). `rings` 1–6, `settle_ms` 0–5000. Returns the new `scan_settings`. 409 during a scan. |
| `POST /scan/start` | `{dry_run: false}` | `dry_run: true` turns the table without the camera. 409 if the turntable or camera isn't ready, or the camera battery is ≤ 20 %. |
| `POST /scan/pause`, `/scan/resume` | | Resume also retries a position whose photo failed. |
| `POST /scan/stop` | | |
| `POST /scan/continue` | | After a ring, the scan waits for you to move the camera to the next height; this starts the next ring. |

Photos land in `<scans_root>/<date>_<name>/ring<r>_<nnn>.JPG` (plus `.RAF`
if RAW is recorded), with a `scan.json` describing every shot (ring,
index, angle, files, time).

To follow a scan, poll `GET /status` (`scan.done`, `scan.paused`,
`scan.waiting_for_ring`) every second or so. A failed photo pauses the
scan and logs the reason.

## Camera

| Endpoint | Body | Notes |
|---|---|---|
| `GET /camera` | | Every setting with `name`, raw `value` and the `choices` the camera accepts right now. |
| `POST /camera` | `{"Setting": value, ...}` | Values by name (`"f/8"`, `"F8"`, `"1/125"`, `"ISO 160"`, `"MF"`; case, spaces, dots and slashes are ignored) or raw number. Also `{"autofocus": true}` for "autofocus before every photo". |
| `POST /camera/preset` | | Photogrammetry settings: ISO 160, daylight WB, Provia, JPEG Fine (L), stabilization off. |
| `POST /camera/test-shot` | | Result in `/log` and `/status` → `preview`. |
| `POST /camera/live` | `{on: true}` | Live view on/off (not during scans). |
| `POST /camera/focus` | `{x: 0.5, y: 0.5}` or `{}` | Autofocus at a point (0..1, top-left origin) or where the focus point is. |
| `GET /camera/diag` | | Buffer, card status, priority, battery raw values, last SDK error. |
| `POST /camera/search` | `{range: "192.168.1"}` optional | Cameras on USB and Wi-Fi. |

Settings: `Mode`, `Aperture`, `Shutter`, `ISO`, `White balance`, `Focus`,
`Image quality`, `Film simulation`, `Image size`, `Stabilization`.

- `Mode` is read-only (400): it comes from the camera's dials. On an X-T4
  set ISO **C**, shutter **T**, exposure compensation **C** and the
  aperture ring to a value for M, then everything else is settable.
- In P/A/S the camera picks aperture and/or shutter; setting them answers
  409 with the reason.

## Turntable

| Endpoint | Body | Notes |
|---|---|---|
| `POST /turntable/rotate` | `{degrees: 90}` | Relative, either direction. |
| `POST /turntable/goto` | `{degrees: 0}` | Absolute angle. |
| `POST /turntable/zero` | | The current position becomes 0°. |
| `POST /turntable/enable` | `{on: true}` | Motor holding torque on/off. |
| `POST /turntable/speed` | `{deg_s: 90}` | 10–180 °/s. |
| `POST /turntable/reconnect` | | |
| `GET /turntable/search` | | Turntables on the network and serial ports. |

Moves answer at once; poll `/status` → `turntable.moving`. All of them
except `reconnect` answer 409 during a scan.

## Connections

| Endpoint | Body |
|---|---|
| `GET /connections` | |
| `POST /connections` | `{camera: {mode: "auto"\|"usb"\|"wifi", ip}, turntable: {mode: "auto"\|"wifi"\|"usb", host, port}}`, any part optional |
| `POST /camera/wifi` | `{ip: "192.168.1.60"}`: Wi-Fi address with automatic mode |

## 3D model

| Endpoint | Body | Notes |
|---|---|---|
| `GET /model` | | Running build (stage, elapsed, last log line) and last result. |
| `POST /model/build` | `{scan: "<folder>", quality: "draft"\|"standard"\|"high", masks: true}`, all optional | Default: the scan selected in the app. |
| `POST /model/cancel` | | |

The model is written to `<scan>/model/`.

## UI

| Endpoint | Body | Notes |
|---|---|---|
| `POST /ui/zoom` | `{factor: 1.25}` | 0.6–2.5. |
| `POST /ui/connections` | `{open: true}` | Show or hide the Connections window. |
| `POST /screenshot` | `{path: "C:/tmp/orbis.png"}` | PNG of the window; default `%TEMP%\orbis-screenshot.png`. Use forward slashes or escaped backslashes in JSON. |

## Testing

`scripts/test-api.ps1` exercises the whole API against the running app
(settings, turntable moves, dry runs, live view, a test shot). Pass
`-NoMotion` and/or `-NoShutter` to leave the hardware alone.
