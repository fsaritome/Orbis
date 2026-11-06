//! Local control API: a tiny HTTP/JSON server on 127.0.0.1:7880.
//!
//! Lets scripts (and Claude) drive the running app exactly like the buttons
//! do: requests are handed to the GUI thread, which answers them between
//! frames. See [`HELP`] for the endpoints.
//!
//! Safety: it listens on loopback only, and every request must carry an
//! `X-Orbis: 1` header. Browsers can't send custom headers cross-origin
//! without a CORS preflight, which is refused, so a web page can't steer
//! the rig through localhost. The Host header must be loopback too (no DNS
//! rebinding).
//!
//! ```text
//! curl.exe -s -H "X-Orbis: 1" http://127.0.0.1:7880/status
//! curl.exe -s -H "X-Orbis: 1" -d "{\"degrees\":90}" http://127.0.0.1:7880/turntable/rotate
//! ```

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use serde_json::{json, Value};

pub const PORT: u16 = 7880;

pub const HELP: &str = "\
GET  /status                       everything at a glance
GET  /log?n=50                     last log lines
POST /scan/settings                {preset: quick|standard|fine|custom, custom_shots, rings, settle_ms, name, autofocus}
POST /scan/start                   {dry_run: false}
POST /scan/pause | /scan/resume | /scan/stop | /scan/continue   (continue = next ring)
GET  /camera                       settings, values and choices
POST /camera                       {\"Aperture\": \"F8\", \"ISO\": 160, ...}  (name or raw value)
POST /camera/preset                photogrammetry settings
POST /camera/test-shot
POST /camera/live                  {on: true}
POST /camera/focus                 {x: 0.5, y: 0.5} or {} for focus now
POST /turntable/rotate             {degrees: 90}   (relative)
POST /turntable/goto               {degrees: 0}    (absolute, scan angle)
POST /turntable/zero | /turntable/reconnect
POST /turntable/enable             {on: true}
POST /turntable/speed              {deg_s: 90}
GET  /model                        3D model build status
POST /model/build                  {scan: \"<folder>\" (default: selected), quality: draft|standard|high, masks: true}
POST /model/cancel
POST /ui/zoom                      {factor: 1.25}
POST /screenshot                   {path: \"C:\\\\...\\\\shot.png\"} (default %TEMP%\\orbis-screenshot.png)
Every request needs the header  X-Orbis: 1";

/// One request, handed to the GUI thread.
pub struct Call {
    pub method: String,
    pub path: String,
    pub query: HashMap<String, String>,
    pub body: Value,
    pub reply: Sender<Reply>,
}

pub struct Reply {
    pub status: u16,
    pub body: Value,
}

impl Reply {
    pub fn ok(body: Value) -> Self {
        Reply { status: 200, body }
    }
    pub fn err(status: u16, msg: impl Into<String>) -> Self {
        Reply { status, body: json!({ "error": msg.into() }) }
    }
}

/// Starts the server; calls arrive on the receiver (the GUI is woken up).
pub fn spawn(ctx: egui::Context) -> Result<Receiver<Call>, String> {
    let listener = TcpListener::bind(("127.0.0.1", PORT)).map_err(|e| format!("API port {PORT} unavailable: {e}"))?;
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("api".into())
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                let tx = tx.clone();
                let ctx = ctx.clone();
                std::thread::spawn(move || {
                    let _ = handle(stream, &tx, &ctx);
                });
            }
        })
        .map_err(|e| e.to_string())?;
    Ok(rx)
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        409 => "Conflict",
        413 => "Payload Too Large",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "Error",
    }
}

fn respond(stream: &mut TcpStream, r: Reply) -> std::io::Result<()> {
    let body = serde_json::to_string_pretty(&r.body).unwrap_or_default();
    write!(
        stream,
        "HTTP/1.1 {} {}\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        r.status,
        reason(r.status),
        body.len()
    )?;
    stream.flush()
}

fn handle(mut stream: TcpStream, tx: &Sender<Call>, ctx: &egui::Context) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let mut parts = line.split_whitespace();
    let (Some(method), Some(target)) = (parts.next(), parts.next()) else {
        return respond(&mut stream, Reply::err(400, "bad request line"));
    };
    let method = method.to_ascii_uppercase();
    let target = target.to_owned();

    let mut headers = HashMap::new();
    let mut total = 0;
    loop {
        let mut h = String::new();
        let n = reader.read_line(&mut h)?;
        total += n;
        if n == 0 || h == "\r\n" || h == "\n" || total > 32 * 1024 {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_owned());
        }
    }

    // Loopback Host only, and the custom header (keeps browsers out).
    let host_ok = headers.get("host").is_some_and(|h| {
        let host = h.rsplit_once(':').map(|(a, _)| a).unwrap_or(h);
        matches!(host, "127.0.0.1" | "localhost" | "[::1]")
    });
    if method == "OPTIONS" || !host_ok || headers.get("x-orbis").map(String::as_str) != Some("1") {
        return respond(&mut stream, Reply::err(403, "local API: send the header `X-Orbis: 1` to 127.0.0.1"));
    }

    let len: usize = headers.get("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
    if len > 1024 * 1024 {
        return respond(&mut stream, Reply::err(413, "body too large"));
    }
    let mut buf = vec![0; len];
    reader.read_exact(&mut buf)?;
    let body = if buf.iter().all(|b| b.is_ascii_whitespace()) {
        Value::Null
    } else {
        match serde_json::from_slice(&buf) {
            Ok(v) => v,
            Err(e) => return respond(&mut stream, Reply::err(400, format!("body is not JSON: {e}"))),
        }
    };

    let (path, q) = target.split_once('?').unwrap_or((&target, ""));
    let query = q.split('&').filter_map(|kv| kv.split_once('=')).map(|(k, v)| (k.to_owned(), v.to_owned())).collect();
    let path = path.trim_end_matches('/').to_owned();
    let path = if path.is_empty() { "/".into() } else { path };

    let (reply_tx, reply_rx) = mpsc::channel();
    if tx.send(Call { method, path, query, body, reply: reply_tx }).is_err() {
        return respond(&mut stream, Reply::err(503, "app is closing"));
    }
    ctx.request_repaint();
    let reply = reply_rx.recv_timeout(Duration::from_secs(20)).unwrap_or_else(|_| Reply::err(504, "app didn't answer"));
    respond(&mut stream, reply)
}
