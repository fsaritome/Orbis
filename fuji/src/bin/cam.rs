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
                    let path = out.join(format!("shot_{:03}_{}.{}", n, img.name.replace(['/', '\\'], "_"), img.extension()));
                    std::fs::write(&path, &img.data)?;
                    println!(
                        "{n}/{count}: {} ({:.1} MB, {}×{}){}",
                        path.display(),
                        img.data.len() as f64 / 1e6,
                        img.width,
                        img.height,
                        if shot.focus_ok { "" } else { "  ⚠ autofocus failed" }
                    );
                }
                println!("    release {:.2} s, total {:.2} s", fired.as_secs_f64(), t.elapsed().as_secs_f64());
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
