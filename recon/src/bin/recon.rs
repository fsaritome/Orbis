//! `recon`: build a 3D model from an Orbis scan folder on the command line.
//!   recon <scan folder> [--quality draft|standard|high] [--no-masks]
//!   recon <scan folder> --masks-only     # just write masks to <scan>/model/masks-preview
//!   recon --install                      # download COLMAP + OpenMVS

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use anyhow::{bail, Context, Result};
use clap::Parser;
use recon::{mask, scan, tools, Options, Progress, Quality};

#[derive(Parser)]
#[command(about = "Build a textured 3D model from an Orbis scan (COLMAP + OpenMVS)")]
struct Args {
    /// Scan folder (with scan.json and the JPEGs).
    dir: Option<PathBuf>,
    #[arg(long, default_value = "standard", value_parser = ["draft", "standard", "high"])]
    quality: String,
    /// Don't cut away the static background.
    #[arg(long)]
    no_masks: bool,
    /// Only compute the masks and save them as a preview.
    #[arg(long)]
    masks_only: bool,
    /// Download COLMAP and OpenMVS.
    #[arg(long)]
    install: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();
    if args.install {
        let t = tools::install(|l| println!("{l}"))?;
        println!("COLMAP:  {}\nOpenMVS: {}", t.colmap.display(), t.openmvs.display());
        return Ok(());
    }
    let Some(dir) = args.dir else { bail!("give a scan folder (or --install)") };
    if args.masks_only {
        return masks_preview(&dir);
    }
    let tools = tools::locate().context("COLMAP / OpenMVS not installed: run `recon --install`")?;
    let quality = match args.quality.as_str() {
        "draft" => Quality::Draft,
        "high" => Quality::High,
        _ => Quality::Standard,
    };
    let opts = Options { quality, masks: !args.no_masks };
    let cancel = AtomicBool::new(false);
    let started = std::time::Instant::now();
    let obj = recon::run(&dir, &opts, &tools, &cancel, &mut |p| match p {
        Progress::Stage { index, total, name } => println!("\n==> [{}/{total}] {name}  ({:.0?})", index + 1, started.elapsed()),
        Progress::Line(l) => println!("    {l}"),
    })?;
    println!("\n==> Done in {:.0?}: {}", started.elapsed(), obj.display());
    Ok(())
}

/// Masks side by side with the photos, to check them by eye.
fn masks_preview(dir: &std::path::Path) -> Result<()> {
    let photos = scan::photos(dir)?;
    let out = dir.join("model").join("masks-preview");
    std::fs::create_dir_all(&out)?;
    // Marks the model folder as made by Orbis, so a later run may replace it.
    let _ = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("model").join(recon::pipeline::LOG));
    let small: Vec<_> = photos.iter().map(|p| Ok(mask::analysis_image(&image::open(&p.path)?.to_rgb8()))).collect::<Result<_>>()?;
    let mut rings: Vec<u32> = photos.iter().map(|p| p.ring).collect();
    rings.dedup();
    for ring in rings {
        let idx: Vec<usize> = (0..photos.len()).filter(|&i| photos[i].ring == ring).collect();
        let imgs: Vec<_> = idx.iter().map(|&i| small[i].clone()).collect();
        for (k, m) in mask::ring_masks(&imgs).iter().enumerate() {
            let mut img = imgs[k].clone();
            for (p, px) in img.pixels_mut().enumerate() {
                if !m.on[p] {
                    px.0 = [px.0[0] / 4, px.0[1] / 4, px.0[2] / 4 + 60];
                }
            }
            let name = photos[idx[k]].path.file_stem().unwrap().to_string_lossy().into_owned();
            img.save(out.join(format!("{name}.png")))?;
            println!("{name}: keeps {:.0} %{}", m.coverage() * 100.0, if m.fallback { " (no mask)" } else { "" });
        }
    }
    println!("Preview: {}", out.display());
    Ok(())
}
