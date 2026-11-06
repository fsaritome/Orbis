//! `cam`: test the Fujifilm camera connection from the command line.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(about = "Talk to a Fujifilm X camera over USB (tethered)")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Camera model, firmware, lens, exposure settings.
    Info,
    /// Take a picture and save it.
    Shoot {
        /// Folder to save into.
        #[arg(long, default_value = "scans/test")]
        out: PathBuf,
        /// Skip autofocus (use the current focus; for manual focus).
        #[arg(long)]
        no_af: bool,
        /// Number of pictures.
        #[arg(long, default_value_t = 1)]
        count: u32,
    },
    /// Every setting with its current value and the values it accepts.
    Settings,
    /// Show the focus point and the range the camera accepts; optionally
    /// move it (`--h --v --size`).
    FocusArea {
        #[arg(long, allow_hyphen_values = true)]
        h: Option<i32>,
        #[arg(long, allow_hyphen_values = true)]
        v: Option<i32>,
        #[arg(long)]
        size: Option<i32>,
    },
    /// Run live view for a few seconds and save the last frame.
    Live {
        #[arg(long, default_value_t = 3)]
        seconds: u64,
        #[arg(long, default_value = "scans/test/live.jpg")]
        out: PathBuf,
    },
    /// Read (or with --set write) the custom auto power-off value.
    PowerOff {
        #[arg(long)]
        set: Option<i32>,
    },
    /// Buffer, card recording, priority and last error.
    Diag,
    /// Save any images still waiting in the camera's transfer buffer.
    Drain {
        #[arg(long, default_value = "scans/test")]
        out: PathBuf,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let sdk = fuji::Sdk::load()?;
    eprintln!("Fujifilm SDK {} from {}", sdk.version(), sdk.dir().display());
    let cam = sdk.open_first()?;
    let info = cam.device_info()?;
    eprintln!("connected: {} {} (firmware {})", info.vendor, info.product, info.firmware);

    match cli.cmd {
        Cmd::Info => {
            println!("model     : {}", info.product);
            println!("firmware  : {}", info.firmware);
            println!("serial    : {}", info.serial);
            println!("link      : {}", info.framework);
            println!("lens      : {}", cam.lens().unwrap_or_else(|e| format!("({e})")));
            match cam.exposure() {
                Ok(e) => {
                    println!("mode      : {}", e.ae_mode_name());
                    println!("aperture  : {}  (raw {})", e.aperture_text(), e.aperture);
                    println!("shutter   : {}  (raw {})", e.shutter_text(), e.shutter);
                    println!("ISO       : {}  (raw {})", e.iso_text(), e.iso);
                }
                Err(e) => println!("exposure  : ({e})"),
            }
            let rec = cam.media_record().map(|v| match v {
                1 => "RAW + JPEG".to_owned(),
                2 => "RAW".to_owned(),
                3 => "JPEG".to_owned(),
                4 => "off (PC only)".to_owned(),
                v => format!("{v}"),
            });
            println!("records   : {}", rec.unwrap_or_else(|e| format!("({e})")));
            println!("buffered  : {}", cam.buffered().map(|n| n.to_string()).unwrap_or_else(|e| format!("({e})")));
            println!("PC control: {}", cam.priority_is_pc().map(|b| b.to_string()).unwrap_or_else(|e| format!("({e})")));
        }
        Cmd::Shoot { out, no_af, count } => {
            std::fs::create_dir_all(&out).with_context(|| format!("creating {}", out.display()))?;
            for n in 1..=count {
                let t = Instant::now();
                let shot = cam.shoot(!no_af)?;
                let fired = t.elapsed();
                let images = cam.wait_images(Duration::from_secs(20))?;
                for img in images {
                    // The SDK's internal name already carries an extension
                    // ("DSCF0001.raf"); keep the stem only.
                    let stem = img.name.rsplit_once('.').map_or(img.name.as_str(), |(s, _)| s).replace(['/', '\\'], "_");
                    let path = out.join(format!("shot_{n:03}_{stem}.{}", img.extension()));
                    std::fs::write(&path, &img.data)?;
                    println!(
                        "{n}/{count}: {} ({:.1} MB){}",
                        path.display(),
                        img.data.len() as f64 / 1e6,
                        if shot.focus_ok { "" } else { "  ⚠ autofocus failed" }
                    );
                }
                println!("    release {:.2} s, total {:.2} s", fired.as_secs_f64(), t.elapsed().as_secs_f64());
            }
        }
        Cmd::Settings => {
            let values = cam.values();
            let choices = cam.choices();
            for s in fuji::Setting::ALL {
                let cur = values.get(s).map(|v| s.value_name(v)).unwrap_or_else(|| "?".into());
                let list = choices.get(s);
                println!("{:16} {cur}", s.label());
                if !list.is_empty() {
                    let names: Vec<String> = list.iter().map(|&v| s.value_name(v)).collect();
                    let mut unique = names.clone();
                    unique.sort();
                    unique.dedup();
                    let dupes = if unique.len() < names.len() { "  ⚠ duplicate names" } else { "" };
                    println!("{:16} {} choices: {}{dupes}", "", names.len(), names.join(", "));
                }
            }
        }
        Cmd::FocusArea { h, v, size } => {
            println!("current : {:?}", cam.focus_area());
            println!("range   : {:?}", cam.focus_area_range());
            if h.is_some() || v.is_some() || size.is_some() {
                let cur = cam.focus_area()?;
                let new = fuji::ffi::FocusArea { h: h.unwrap_or(cur.h), v: v.unwrap_or(cur.v), size: size.unwrap_or(cur.size) };
                cam.set_focus_area(new)?;
                println!("set to  : {:?}", cam.focus_area());
            }
        }
        Cmd::Live { seconds, out } => {
            cam.start_live_view()?;
            let t = Instant::now();
            let (mut frames, mut last) = (0u32, None);
            while t.elapsed() < Duration::from_secs(seconds) {
                if let Some(f) = cam.next_live_frame()? {
                    frames += 1;
                    last = Some(f);
                } else {
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
            cam.stop_live_view()?;
            let fps = frames as f64 / t.elapsed().as_secs_f64();
            match last {
                Some(f) => {
                    if let Some(dir) = out.parent() {
                        std::fs::create_dir_all(dir)?;
                    }
                    std::fs::write(&out, &f)?;
                    println!("{frames} frames in {seconds} s ({fps:.1} fps), {} KB each; last saved to {}", f.len() / 1024, out.display());
                }
                None => println!("no live view frames arrived"),
            }
        }
        Cmd::PowerOff { set } => {
            println!("custom auto power-off now: {}", cam.custom_power_off()?);
            if let Some(v) = set {
                cam.set_custom_power_off(v)?;
                println!("set to {v}; reads back: {}", cam.custom_power_off()?);
            }
        }
        Cmd::Diag => {
            for (k, v) in cam.diagnostics() {
                println!("{k:32} {v}");
            }
        }
        Cmd::Drain { out } => {
            std::fs::create_dir_all(&out)?;
            let mut n = 0;
            while let Some(img) = cam.next_image()? {
                n += 1;
                let path = out.join(format!("drained_{n:03}.{}", img.extension()));
                std::fs::write(&path, &img.data)?;
                println!("{}", path.display());
            }
            println!("{n} image(s) saved");
        }
    }
    Ok(())
}
