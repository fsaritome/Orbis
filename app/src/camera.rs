//! Background thread that owns the Fujifilm camera.
//!
//! The SDK is not thread-safe, so the `Sdk` and `Camera` live here and
//! nowhere else. Other threads send [`CamCmd`]s; captures answer on their
//! own reply channel, everything else is reported as [`CamEvent`]s.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use egui::ColorImage;
use fuji::{Choices, Setting, Values};

pub enum CamCmd {
    /// Take one picture and save it as `<stem>.<ext>` (one file per format).
    Capture { stem: PathBuf, autofocus: bool, reply: Sender<Result<Capture, String>> },
    /// Change settings, in this order (e.g. mode before shutter speed).
    Set(Vec<(Setting, i32)>),
}

/// What a capture produced.
#[derive(Debug, Clone)]
pub struct Capture {
    pub files: Vec<PathBuf>,
    pub focus_ok: bool,
}

#[derive(Debug, Clone)]
pub struct CamInfo {
    pub model: String,
    pub lens: String,
    pub firmware: String,
}

pub enum CamEvent {
    Searching,
    Connected(CamInfo),
    /// Not usable right now; the text says why and what to do.
    Problem(String),
    /// Current settings, and (when refreshed) what each one accepts.
    Settings { values: Values, choices: Option<Choices> },
    /// A photo was saved. Images are already scaled for display.
    Photo { path: PathBuf, preview: Option<ColorImage>, thumb: Option<ColorImage> },
    CaptureFailed,
    Log(String),
}

const RETRY: Duration = Duration::from_secs(3);
const REFRESH: Duration = Duration::from_secs(2);
const IMAGE_TIMEOUT: Duration = Duration::from_secs(25);
pub const PREVIEW_PX: u32 = 1280;
pub const THUMB_PX: u32 = 200;

pub fn spawn(ctx: egui::Context) -> (Sender<CamCmd>, Receiver<CamEvent>) {
    let (cmd_tx, cmd_rx) = mpsc::channel();
    let (ev_tx, ev_rx) = mpsc::channel();
    thread::Builder::new()
        .name("camera".into())
        .spawn(move || run(cmd_rx, Emitter { tx: ev_tx, ctx }))
        .expect("spawn camera thread");
    (cmd_tx, ev_rx)
}

struct Emitter {
    tx: Sender<CamEvent>,
    ctx: egui::Context,
}

impl Emitter {
    fn send(&self, ev: CamEvent) {
        let _ = self.tx.send(ev);
        self.ctx.request_repaint();
    }
}

/// Fail any capture requests that arrive while there is no camera.
fn refuse_pending(cmds: &Receiver<CamCmd>, out: &Emitter, why: &str, wait: Duration) -> bool {
    let until = Instant::now() + wait;
    while Instant::now() < until {
        match cmds.recv_timeout(Duration::from_millis(100)) {
            Ok(CamCmd::Capture { reply, .. }) => {
                let _ = reply.send(Err(format!("camera not ready: {why}")));
                out.send(CamEvent::CaptureFailed);
            }
            Ok(CamCmd::Set(_)) => out.send(CamEvent::Log(format!("Camera: can't change settings: {why}"))),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return false,
        }
    }
    true
}

/// Shorten SDK errors into something the status line can show.
fn friendly(e: &anyhow::Error) -> String {
    let s = format!("{e:#}");
    if s.contains("not in tether mode") {
        "Camera found, but not in tether mode: set CONNECTION MODE → USB TETHER SHOOTING AUTO, then replug USB"
            .into()
    } else if s.contains("no Fujifilm camera found") {
        "No camera on USB (switched on and connected?)".into()
    } else if s.contains("SDK not found") {
        "Fujifilm SDK missing: copy the DLLs into sdk/bin (see README)".into()
    } else {
        s
    }
}

fn run(cmds: Receiver<CamCmd>, out: Emitter) {
    let mut last_problem = String::new();
    loop {
        out.send(CamEvent::Searching);
        // A fresh SDK per attempt: after a failed open the SDK wants
        // XSDK_Exit + XSDK_Init again (as in Fujifilm's samples).
        let sdk = match fuji::Sdk::load() {
            Ok(s) => s,
            Err(e) => {
                let msg = friendly(&e);
                out.send(CamEvent::Problem(msg.clone()));
                if !refuse_pending(&cmds, &out, &msg, Duration::from_secs(10)) {
                    return;
                }
                continue;
            }
        };
        let cam = match sdk.open_first() {
            Ok(c) => c,
            Err(e) => {
                let msg = friendly(&e);
                if msg != last_problem {
                    out.send(CamEvent::Log(format!("Camera: {msg}")));
                    last_problem = msg.clone();
                }
                out.send(CamEvent::Problem(msg.clone()));
                // `sdk` is dropped (XSDK_Exit) at the end of this iteration.
                if !refuse_pending(&cmds, &out, &msg, RETRY) {
                    return;
                }
                continue;
            }
        };
        last_problem.clear();

        let info = cam.device_info().map(|d| (d.product, d.firmware)).unwrap_or_default();
        let lens = cam.lens().unwrap_or_default();
        out.send(CamEvent::Log(format!("Camera connected: {} with {}", info.0, if lens.is_empty() { "no lens info" } else { &lens })));
        out.send(CamEvent::Connected(CamInfo { model: info.0, lens, firmware: info.1 }));
        send_settings(&cam, &out, true);

        // Connected: serve commands, refresh settings, watch for unplugging.
        let mut next_refresh = Instant::now() + REFRESH;
        let mut refreshes = 0u32;
        loop {
            match cmds.recv_timeout(Duration::from_millis(200)) {
                Ok(CamCmd::Capture { stem, autofocus, reply }) => {
                    let result = capture(&cam, &stem, autofocus, &out).map_err(|e| format!("{e:#}"));
                    if let Err(e) = &result {
                        out.send(CamEvent::Log(format!("Camera: shot failed: {e}")));
                        out.send(CamEvent::CaptureFailed);
                    }
                    let _ = reply.send(result);
                }
                Ok(CamCmd::Set(changes)) => {
                    for (s, v) in changes {
                        match cam.set(s, v) {
                            Ok(()) => out.send(CamEvent::Log(format!("Camera: {} → {}", s.label(), s.value_name(v)))),
                            Err(e) => out.send(CamEvent::Log(format!("Camera: {e:#}"))),
                        }
                    }
                    // Choices depend on the mode (e.g. shutter speeds in M).
                    send_settings(&cam, &out, true);
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
            if Instant::now() >= next_refresh {
                next_refresh = Instant::now() + REFRESH;
                if !cam.alive() {
                    out.send(CamEvent::Log("Camera disconnected".into()));
                    break;
                }
                // Values every 2 s; full choice lists every 20 s (lens swaps).
                refreshes += 1;
                send_settings(&cam, &out, refreshes.is_multiple_of(10));
            }
        }
    }
}

fn send_settings(cam: &fuji::Camera<'_>, out: &Emitter, with_choices: bool) {
    out.send(CamEvent::Settings { values: cam.values(), choices: with_choices.then(|| cam.choices()) });
}

fn capture(cam: &fuji::Camera<'_>, stem: &Path, autofocus: bool, out: &Emitter) -> anyhow::Result<Capture> {
    if let Some(dir) = stem.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // Leftovers from an earlier shot would be saved under the wrong name.
    while cam.next_image()?.is_some() {}

    let shot = cam.shoot(autofocus)?;
    let images = cam.wait_images(IMAGE_TIMEOUT)?;
    let mut files = Vec::new();
    let mut jpeg: Option<(PathBuf, Vec<u8>)> = None;
    for img in images {
        let path = stem.with_extension(img.extension());
        std::fs::write(&path, &img.data)?;
        if img.extension() == "JPG" && jpeg.is_none() {
            jpeg = Some((path.clone(), img.data));
        }
        files.push(path);
    }

    // Preview from the JPEG. RAW-only shots get no preview (RAF decoding
    // isn't worth it here); the camera should record RAW + JPEG.
    let (path, preview, thumb) = match jpeg {
        Some((path, data)) => {
            let (p, t) = decode_previews(&data);
            (path, p, t)
        }
        None => (files.first().cloned().unwrap_or_default(), None, None),
    };
    out.send(CamEvent::Photo { path, preview, thumb });
    Ok(Capture { files, focus_ok: shot.focus_ok })
}

fn to_color_image(img: &image::DynamicImage) -> ColorImage {
    let rgba = img.to_rgba8();
    ColorImage::from_rgba_unmultiplied([rgba.width() as usize, rgba.height() as usize], rgba.as_raw())
}

/// Decode a camera JPEG into a display preview and a thumbnail.
pub fn decode_previews(jpeg: &[u8]) -> (Option<ColorImage>, Option<ColorImage>) {
    let Ok(img) = image::load_from_memory_with_format(jpeg, image::ImageFormat::Jpeg) else {
        return (None, None);
    };
    let preview = img.thumbnail(PREVIEW_PX, PREVIEW_PX);
    let thumb = preview.thumbnail(THUMB_PX, THUMB_PX);
    (Some(to_color_image(&preview)), Some(to_color_image(&thumb)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn previews_are_scaled_down_keeping_aspect() {
        // A 6240×4160 JPEG like the X-T4 makes (3:2), solid grey.
        let img = image::RgbImage::from_pixel(6240, 4160, image::Rgb([128, 128, 128]));
        let mut jpeg = Vec::new();
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut std::io::Cursor::new(&mut jpeg), image::ImageFormat::Jpeg)
            .unwrap();
        let (preview, thumb) = decode_previews(&jpeg);
        let (p, t) = (preview.unwrap(), thumb.unwrap());
        assert_eq!(p.size, [PREVIEW_PX as usize, 853]);
        assert_eq!(t.size[0], THUMB_PX as usize);
        assert!((t.size[1] as i32 - 133).abs() <= 1);
    }

    /// `ORBIS_TEST_JPEG=path cargo test -p orbis real_jpeg -- --ignored`
    #[test]
    #[ignore]
    fn real_jpeg_decodes() {
        let path = std::env::var("ORBIS_TEST_JPEG").expect("set ORBIS_TEST_JPEG");
        let data = std::fs::read(&path).unwrap();
        let err = image::load_from_memory_with_format(&data, image::ImageFormat::Jpeg).err();
        let (p, t) = decode_previews(&data);
        assert!(p.is_some() && t.is_some(), "decode failed: {err:?}");
    }

    #[test]
    fn garbage_gives_no_preview() {
        assert!(decode_previews(b"not a jpeg").0.is_none());
    }
}
