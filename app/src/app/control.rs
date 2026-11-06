//! Answers control API calls (see `crate::api`) on the GUI thread, using
//! the same actions as the buttons.

use std::path::PathBuf;

use fuji::Setting;
use serde_json::{json, Value};

use super::{CamState, Link, OrbisApp};
use crate::api::{Call, Reply, HELP};
use crate::camera::CamCmd;
use crate::plan::Preset;
use crate::worker::Cmd;

fn num(body: &Value, key: &str) -> Result<f64, Reply> {
    body.get(key).and_then(Value::as_f64).ok_or_else(|| Reply::err(400, format!("needs a number `{key}`")))
}

fn flag(body: &Value, key: &str) -> Result<bool, Reply> {
    body.get(key).and_then(Value::as_bool).ok_or_else(|| Reply::err(400, format!("needs `{key}`: true/false")))
}

impl OrbisApp {
    pub(super) fn handle_api(&mut self, ctx: &egui::Context, call: Call) {
        let Call { method, path, query, body, reply } = call;
        // Screenshots answer later, from the renderer's callback.
        if (method.as_str(), path.as_str()) == ("POST", "/screenshot") {
            let file = body
                .get("path")
                .and_then(Value::as_str)
                .map(PathBuf::from)
                .unwrap_or_else(|| std::env::temp_dir().join("orbis-screenshot.png"));
            ctx.request_screenshot(move |img| {
                let [w, h] = img.size;
                let rgba: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
                let r = image::RgbaImage::from_raw(w as u32, h as u32, rgba)
                    .ok_or_else(|| "bad image".to_string())
                    .and_then(|i| i.save(&file).map_err(|e| e.to_string()));
                let _ = reply.send(match r {
                    Ok(()) => Reply::ok(json!({ "path": file, "width": w, "height": h })),
                    Err(e) => Reply::err(400, format!("can't save {}: {e}", file.display())),
                });
            });
            ctx.request_repaint();
            return;
        }
        if (method.as_str(), path.as_str()) == ("GET", "/camera/diag") {
            let (tx, rx) = std::sync::mpsc::channel();
            let _ = self.cam_cmd.send(CamCmd::Diagnose(tx));
            std::thread::spawn(move || {
                let r = match rx.recv_timeout(std::time::Duration::from_secs(10)) {
                    Ok(d) => Reply::ok(Value::Object(d.into_iter().map(|(k, v)| (k, Value::String(v))).collect())),
                    Err(_) => Reply::err(504, "camera thread busy (stuck in a call?)"),
                };
                let _ = reply.send(r);
            });
            return;
        }
        let r = self.route(ctx, &method, &path, &query, &body).unwrap_or_else(|e| e);
        let _ = reply.send(r);
    }

    fn route(
        &mut self,
        ctx: &egui::Context,
        method: &str,
        path: &str,
        query: &std::collections::HashMap<String, String>,
        body: &Value,
    ) -> Result<Reply, Reply> {
        let scanning = self.scan.is_some();
        let idle = |what: &str| if scanning { Err(Reply::err(409, format!("can't {what} during a scan"))) } else { Ok(()) };
        let done = |what: &str| Ok(Reply::ok(json!({ "ok": true, "done": what })));
        match (method, path) {
            ("GET", "/") => Ok(Reply::ok(json!({ "endpoints": HELP.lines().collect::<Vec<_>>() }))),
            ("GET", "/status") => Ok(Reply::ok(self.status_json(ctx))),
            ("GET", "/log") => {
                let n: usize = query.get("n").and_then(|v| v.parse().ok()).unwrap_or(50);
                let lines: Vec<&String> = self.log.iter().rev().take(n).collect::<Vec<_>>().into_iter().rev().collect();
                Ok(Reply::ok(json!({ "lines": lines })))
            }

            // ---- scan
            ("POST", "/scan/settings") => {
                idle("change scan settings")?;
                let s = &mut self.saved.scan;
                if let Some(p) = body.get("preset").and_then(Value::as_str) {
                    s.preset = Preset::ALL
                        .into_iter()
                        .find(|x| x.label().eq_ignore_ascii_case(p))
                        .ok_or_else(|| Reply::err(400, "preset: quick, standard, fine or custom"))?;
                }
                if let Some(v) = body.get("custom_shots").and_then(Value::as_u64) {
                    s.custom_shots = (v as u32).clamp(4, 360);
                }
                if let Some(v) = body.get("rings").and_then(Value::as_u64) {
                    s.rings = (v as u32).clamp(1, 6);
                }
                if let Some(v) = body.get("settle_ms").and_then(Value::as_u64) {
                    s.settle_ms = v.min(5000);
                }
                if let Some(v) = body.get("name").and_then(Value::as_str) {
                    self.saved.scan_name = v.to_owned();
                }
                if let Some(v) = body.get("autofocus").and_then(Value::as_bool) {
                    self.saved.autofocus = v;
                }
                Ok(Reply::ok(self.status_json(ctx)["scan_settings"].clone()))
            }
            ("POST", "/scan/start") => {
                idle("start another scan")?;
                let dry = body.get("dry_run").and_then(Value::as_bool).unwrap_or(false);
                if !self.connected() {
                    return Err(Reply::err(409, "turntable not connected"));
                }
                if !dry && self.battery_too_low() {
                    return Err(Reply::err(409, format!("camera battery at {} %: charge or swap it before a scan", self.cam_battery.unwrap_or(0))));
                }
                if !dry && !self.camera_ready() {
                    return Err(Reply::err(409, "camera not ready (or pass {\"dry_run\": true})"));
                }
                self.start_scan(!dry);
                done(if dry { "dry run started" } else { "scan started" })
            }
            ("POST", "/scan/pause" | "/scan/resume") => {
                if !scanning {
                    return Err(Reply::err(409, "no scan running"));
                }
                self.send(Cmd::Pause(path == "/scan/pause"));
                done(path)
            }
            ("POST", "/scan/stop") => {
                if !scanning {
                    return Err(Reply::err(409, "no scan running"));
                }
                self.send(Cmd::Stop);
                done("stopping")
            }
            ("POST", "/scan/continue") => {
                if !self.scan.as_ref().is_some_and(|s| s.waiting_ring.is_some()) {
                    return Err(Reply::err(409, "not waiting for the next ring"));
                }
                self.continue_ring();
                done("next ring")
            }

            // ---- camera
            ("GET", "/camera") => Ok(Reply::ok(self.camera_json())),
            ("POST", "/camera") => {
                idle("change camera settings")?;
                let obj = body.as_object().ok_or_else(|| Reply::err(400, "send {\"Setting\": value, ...}"))?;
                let mut changes = Vec::new();
                for (k, v) in obj {
                    if k.eq_ignore_ascii_case("autofocus") {
                        self.saved.autofocus = v.as_bool().ok_or_else(|| Reply::err(400, "autofocus: true/false"))?;
                        continue;
                    }
                    let s = Setting::ALL
                        .into_iter()
                        .find(|s| s.label().eq_ignore_ascii_case(k))
                        .ok_or_else(|| Reply::err(400, format!("unknown setting {k}: {}", Setting::ALL.map(|s| s.label()).join(", "))))?;
                    changes.push((s, self.resolve_value(s, v)?));
                }
                if !changes.is_empty() {
                    if !self.camera_ready() {
                        return Err(Reply::err(409, "camera not connected"));
                    }
                    self.saved.remember_camera(&changes);
                    let _ = self.cam_cmd.send(CamCmd::Set(changes.clone()));
                }
                Ok(Reply::ok(json!({ "ok": true, "set": changes.iter().map(|(s, v)| json!({ "setting": s.label(), "value": v, "name": s.value_name(*v) })).collect::<Vec<_>>() })))
            }
            ("POST", "/camera/wifi") => {
                let ip = body.get("ip").and_then(Value::as_str).ok_or_else(|| Reply::err(400, "send {\"ip\": \"192.168.…\"} or {\"ip\": \"\"} for USB only"))?;
                self.saved.camera_ip = ip.trim().to_owned();
                if let Ok(mut w) = self.cam_wifi.lock() {
                    *w = self.saved.camera_ip.clone();
                }
                done("camera Wi-Fi address set (used when no USB camera is found)")
            }
            ("POST", "/camera/preset") => {
                idle("change camera settings")?;
                let preset = fuji::settings::photogrammetry_preset();
                self.saved.remember_camera(&preset);
                let _ = self.cam_cmd.send(CamCmd::Set(preset));
                done("photogrammetry settings")
            }
            ("POST", "/camera/test-shot") => {
                idle("take a test shot")?;
                if !self.camera_ready() || self.test_shot_busy {
                    return Err(Reply::err(409, "camera not ready or busy"));
                }
                self.test_shot();
                done("test shot (see /log and /status preview)")
            }
            ("POST", "/camera/live") => {
                let on = flag(body, "on")?;
                if on && scanning {
                    return Err(Reply::err(409, "no live view during a scan"));
                }
                let _ = self.cam_cmd.send(CamCmd::LiveView(on));
                done(if on { "live view on" } else { "live view off" })
            }
            ("POST", "/camera/focus") => {
                match (body.get("x").and_then(Value::as_f64), body.get("y").and_then(Value::as_f64)) {
                    (Some(x), Some(y)) => {
                        let _ = self.cam_cmd.send(CamCmd::FocusAt { x: x.clamp(0.0, 1.0) as f32, y: y.clamp(0.0, 1.0) as f32 });
                    }
                    _ => {
                        let _ = self.cam_cmd.send(CamCmd::FocusNow);
                    }
                }
                done("focusing")
            }

            // ---- turntable
            ("POST", p) if p.starts_with("/turntable/") => {
                if p == "/turntable/reconnect" {
                    self.link = Link::Connecting;
                    self.send(Cmd::Reconnect);
                    return done("reconnecting");
                }
                idle("move the turntable")?;
                if !self.connected() {
                    return Err(Reply::err(409, "turntable not connected"));
                }
                match p {
                    "/turntable/rotate" => self.send(Cmd::Jog(num(body, "degrees")?)),
                    "/turntable/goto" => self.send(Cmd::GoTo(num(body, "degrees")?)),
                    "/turntable/zero" => self.send(Cmd::SetZero),
                    "/turntable/enable" => self.send(Cmd::Enable(flag(body, "on")?)),
                    "/turntable/speed" => {
                        self.saved.speed_deg_s = num(body, "deg_s")?.clamp(10.0, 180.0);
                        let (max_speed, accel) = self.saved.motion();
                        self.send(Cmd::SetMotion { max_speed, accel });
                    }
                    _ => return Err(Reply::err(404, format!("no endpoint {p}"))),
                }
                done(p)
            }

            // ---- 3D model
            ("GET", "/model") => Ok(Reply::ok(self.model.status(&self.saved.model))),
            ("POST", "/model/build") => {
                idle("build a model")?;
                let scan = body.get("scan").and_then(Value::as_str).map(PathBuf::from);
                let quality = body.get("quality").and_then(Value::as_str);
                let masks = body.get("masks").and_then(Value::as_bool);
                let dir = self
                    .model
                    .api_build(ctx, &mut self.saved.model, scan, quality, masks)
                    .map_err(|e| Reply::err(409, e))?;
                Ok(Reply::ok(json!({ "ok": true, "building": dir })))
            }
            ("POST", "/model/cancel") => {
                if self.model.api_cancel() {
                    done("cancelling")
                } else {
                    Err(Reply::err(409, "no build running"))
                }
            }

            // ---- UI
            ("POST", "/ui/zoom") => {
                let f = num(body, "factor")?.clamp(0.6, 2.5) as f32;
                ctx.set_zoom_factor(f);
                self.saved.zoom = f;
                done("zoom")
            }

            _ => Err(Reply::err(404, format!("no endpoint {method} {path}; GET / lists them"))),
        }
    }

    /// A camera value from JSON: the raw number, or a (case-insensitive)
    /// name like "F8", "1/125", "ISO 160", "MF".
    fn resolve_value(&self, s: Setting, v: &Value) -> Result<i32, Reply> {
        let choices = self.cam_choices.get(s);
        if let Some(n) = v.as_i64() {
            return Ok(n as i32);
        }
        let want = v.as_str().ok_or_else(|| Reply::err(400, format!("{}: number or name", s.label())))?;
        let norm = |t: &str| t.to_ascii_lowercase().replace([' ', '.', '/'], "");
        let w = norm(want);
        let names: Vec<(i32, String)> = choices.iter().map(|&c| (c, s.value_name(c))).collect();
        names
            .iter()
            .find(|(_, n)| norm(n) == w)
            .or_else(|| names.iter().find(|(_, n)| norm(n).starts_with(&w)))
            .or_else(|| names.iter().find(|(_, n)| norm(n).contains(&w)))
            .map(|(c, _)| *c)
            .ok_or_else(|| {
                Reply::err(400, format!("{}: no choice matches {want:?}; choices: {}", s.label(), names.iter().map(|(_, n)| n.as_str()).collect::<Vec<_>>().join(", ")))
            })
    }

    fn camera_json(&self) -> Value {
        let settings: serde_json::Map<String, Value> = Setting::ALL
            .into_iter()
            .map(|s| {
                let v = self.cam_values.as_ref().and_then(|vals| vals.get(s));
                (
                    s.label().to_owned(),
                    json!({
                        "value": v,
                        "name": v.map(|v| s.value_name(v)),
                        "choices": self.cam_choices.get(s).iter().map(|&c| s.value_name(c)).collect::<Vec<_>>(),
                    }),
                )
            })
            .collect();
        json!({ "settings": settings, "autofocus": self.saved.autofocus })
    }

    fn status_json(&self, ctx: &egui::Context) -> Value {
        let turntable = match &self.link {
            Link::Connecting => json!({ "state": "connecting" }),
            Link::Connected(how) => json!({
                "state": "connected", "via": how,
                "degrees": self.table_deg.map(|d| (d.rem_euclid(360.0) * 10.0).round() / 10.0),
                "moving": self.moving, "motor_holds": self.enabled, "speed_deg_s": self.saved.speed_deg_s,
            }),
            Link::Down(why) => json!({ "state": "down", "why": why }),
        };
        let camera = match &self.cam {
            CamState::Searching => json!({ "state": "searching", "wifi_ip": self.saved.camera_ip }),
            CamState::Connected(i) => json!({
                "state": "connected", "model": i.model, "lens": i.lens, "link": i.link,
                "settings": self.cam_values.as_ref().map(|v| Setting::ALL.into_iter()
                    .map(|s| (s.label().to_owned(), json!(v.get(s).map(|x| s.value_name(x)))))
                    .collect::<serde_json::Map<_, _>>()),
                "battery_percent": self.cam_battery,
                "live_view": self.live_on,
                "test_shot_busy": self.test_shot_busy,
            }),
            CamState::Problem(why) => json!({ "state": "problem", "why": why }),
        };
        let view = |s: &super::ScanView| {
            json!({
                "dir": s.dir, "dry_run": s.dir.is_none(),
                "done": s.done.iter().sum::<u32>(), "total": s.plan.total_shots(),
                "per_ring": s.done, "rings": s.plan.rings, "shots_per_ring": s.plan.shots_per_ring,
                "paused": s.paused, "waiting_for_ring": s.waiting_ring.map(|r| r + 1),
                "elapsed_s": s.started.elapsed().as_secs(),
            })
        };
        let p = self.saved.scan.plan();
        json!({
            "turntable": turntable,
            "camera": camera,
            "scan": self.scan.as_ref().map(view),
            "last_scan": self.last_scan.as_ref().map(view),
            "scan_settings": {
                "preset": self.saved.scan.preset.label(),
                "shots_per_ring": p.shots_per_ring, "rings": p.rings, "angle_step": p.angle_step(),
                "settle_ms": self.saved.scan.settle_ms, "name": self.saved.scan_name,
                "autofocus": self.saved.autofocus, "scans_root": self.saved.scans_root,
            },
            "preview": self.preview.as_ref().map(|(_, p)| p),
            "model": self.model.status(&self.saved.model),
            "zoom": ctx.zoom_factor(),
        })
    }
}
