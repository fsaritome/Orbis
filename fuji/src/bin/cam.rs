//! `cam`: test the Fujifilm camera connection from the command line.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(about = "Talk to a Fujifilm X camera, tethered over USB or Wi-Fi")]
struct Cli {
    /// Camera over Wi-Fi at this IPv4 address (WIRELESS TETHER SHOOTING
    /// FIXED) instead of USB. "search" scans the local network.
    #[arg(long, global = true)]
    ip: Option<String>,
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
        /// Before each shot, query like the app does: values, choices
        /// (Cap*), battery (all|values|choices|battery).
        #[arg(long)]
        probe: Option<String>,
    },
    /// Every setting with its current value and the values it accepts.
    Settings,
    /// Set one setting to a raw value (see `settings`), e.g. `set Aperture 800`.
    Set {
        setting: String,
        value: i32,
    },
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
    /// List cameras: USB, the local network, or (with --range) an address
    /// range like 192.168.1 (runs without opening a camera).
    Search {
        #[arg(long)]
        range: Option<String>,
    },
    /// Read (or with --set write) the custom auto power-off value.
    PowerOff {
        #[arg(long)]
        set: Option<i32>,
    },
    /// Buffer, card recording, priority and last error.
    Diag,
    /// Read (or with --set 1-4 write) where the camera records:
    /// 1 RAW+JPEG to card, 2 RAW, 3 JPEG, 4 off (PC only).
    MediaRecord {
        #[arg(long)]
        set: Option<i32>,
    },
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
    if let Cmd::Search { range } = &cli.cmd {
        let mut links = vec![fuji::Link::Usb, fuji::Link::WifiSearch];
        if let Some(r) = range {
            links.push(fuji::Link::Wifi(r.clone()));
        }
        for link in links {
            let t = Instant::now();
            match sdk.list(&link) {
                Ok(v) => {
                    println!("{link:12} {} found in {:.1} s", v.len(), t.elapsed().as_secs_f32());
                    for c in v {
                        println!("    {}  serial {}  ip {}  via {}", c.product, c.serial, c.ip, c.framework);
                    }
                }
                Err(e) => println!("{link:12} error: {e:#}"),
            }
        }
        return Ok(());
    }
    let link = match cli.ip.as_deref() {
        None => fuji::Link::Usb,
        Some("search") => fuji::Link::WifiSearch,
        Some(ip) => fuji::Link::Wifi(ip.to_owned()),
    };
    let t = Instant::now();
    let cam = sdk.open(&link)?;
    eprintln!("opened over {link} in {:.1} s", t.elapsed().as_secs_f32());
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
        Cmd::Shoot { out, no_af, count, probe } => {
            std::fs::create_dir_all(&out).with_context(|| format!("creating {}", out.display()))?;
            for n in 1..=count {
                if let Some(p) = probe.as_deref() {
                    if matches!(p, "all" | "values") {
                        let _ = cam.values();
                    }
                    if matches!(p, "all" | "choices") {
                        let _ = cam.choices();
                    }
                    if matches!(p, "all" | "battery") {
                        let _ = cam.battery();
                    }
                    if let Some(i) = p.strip_prefix("preset").and_then(|n| n.parse::<usize>().ok()) {
                        if let Some(&(st, v)) = fuji::settings::photogrammetry_preset().get(i) {
                            println!("    set {} = {}", st.label(), st.value_name(v));
                            if let Err(e) = cam.set(st, v) {
                                println!("    set {}: {e:#}", st.label());
                            }
                        }
                    }
                    if p == "preset" {
                        for (st, v) in fuji::settings::photogrammetry_preset() {
                            if let Err(e) = cam.set(st, v) {
                                println!("    set {}: {e:#}", st.label());
                            }
                        }
                    }
                    if p == "m8" {
                        // M and f/8 from the PC, in this session (dials T, ring A).
                        for (st, v) in [(fuji::Setting::Mode, 1), (fuji::Setting::Aperture, 800)] {
                            match cam.set(st, v) {
                                Ok(()) => println!("    set {} = {}", st.label(), st.value_name(v)),
                                Err(e) => println!("    set {}: {e:#}", st.label()),
                            }
                        }
                        for st in [fuji::Setting::Mode, fuji::Setting::Aperture, fuji::Setting::Shutter] {
                            println!("    {} is {}", st.label(), cam.get(st).map(|v| st.value_name(v)).unwrap_or_default());
                        }
                    }
                    if p == "mf" {
                        if let Err(e) = cam.set(fuji::Setting::Focus, fuji::ffi::SDK_FOCUS_MANUAL) {
                            println!("    set focus: {e:#}");
                        }
                    }
                    if matches!(p, "all" | "drain") {
                        while let Ok(Some(_)) = cam.next_image() {}
                    }
                    println!("    probed: {p}");
                }
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
        Cmd::Set { setting, value } => {
            let s = fuji::Setting::ALL
                .into_iter()
                .find(|s| s.label().eq_ignore_ascii_case(&setting))
                .with_context(|| format!("unknown setting {setting}"))?;
            cam.set(s, value)?;
            println!("{} = {}", s.label(), cam.get(s).map(|v| s.value_name(v)).unwrap_or_else(|e| format!("{e:#}")));
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
        Cmd::Search { .. } => unreachable!(),
        Cmd::PowerOff { set } => {
            println!("custom auto power-off now: {}", cam.custom_power_off()?);
            if let Some(v) = set {
                cam.set_custom_power_off(v)?;
                println!("set to {v}; reads back: {}", cam.custom_power_off()?);
            }
        }
        Cmd::MediaRecord { set } => {
            println!("media record now: {}", cam.media_record()?);
            if let Some(v) = set {
                cam.set_media_record(v)?;
                println!("set to {v}; reads back: {}", cam.media_record()?);
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
