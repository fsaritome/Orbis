//! The reconstruction pipeline: photos → masks → COLMAP → OpenMVS → model.obj.
//!
//! Everything lands in `<scan>/model/`; `orbis-recon.log` there has the
//! full output of every tool.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Mutex};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use image::RgbImage;

use crate::mask;
use crate::scan::{self, Photo};
use crate::tools::Tools;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Quality {
    /// Quick look: a few minutes.
    Draft,
    #[default]
    Standard,
    /// Full detail: slow on a CPU.
    High,
}

impl Quality {
    pub const ALL: [Quality; 3] = [Quality::Draft, Quality::Standard, Quality::High];

    pub fn label(self) -> &'static str {
        match self {
            Quality::Draft => "Draft",
            Quality::Standard => "Standard",
            Quality::High => "High",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            Quality::Draft => "Small working copies: minutes, coarse model",
            Quality::Standard => "Good balance of detail and time",
            Quality::High => "Full detail: can take an hour on the CPU",
        }
    }

    /// Long side of the working copies of the photos.
    fn image_px(self) -> u32 {
        match self {
            Quality::Draft => 2000,
            Quality::Standard => 3200,
            Quality::High => 4800,
        }
    }

    /// OpenMVS --resolution-level (how often to halve before densifying).
    fn dense_level(self) -> u32 {
        match self {
            Quality::Draft => 2,
            Quality::Standard => 1,
            Quality::High => 0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Options {
    pub quality: Quality,
    /// Cut away the static background (recommended for turntable scans).
    pub masks: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options { quality: Quality::Standard, masks: true }
    }
}

#[derive(Debug, Clone)]
pub enum Progress {
    /// A new stage started (index counts from 0).
    Stage { index: usize, total: usize, name: &'static str },
    /// A line of output (ours or a tool's).
    Line(String),
}

pub const STAGES: [&str; 8] = [
    "Preparing photos and masks",
    "Finding features",
    "Matching photos",
    "Aligning cameras",
    "Undistorting",
    "Dense point cloud",
    "Mesh",
    "Texture",
];

/// Folder inside the scan that holds the reconstruction.
pub const MODEL_DIR: &str = "model";
/// Log file; its presence also marks [`MODEL_DIR`] as ours (safe to wipe).
pub const LOG: &str = "orbis-recon.log";
/// The finished model inside [`MODEL_DIR`].
pub const MODEL_FILE: &str = "model.obj";

struct Run<'a> {
    dir: PathBuf,
    log: std::fs::File,
    cancel: &'a AtomicBool,
    on: &'a mut dyn FnMut(Progress),
}

impl Run<'_> {
    fn say(&mut self, line: String) {
        let _ = writeln!(self.log, "{line}");
        (self.on)(Progress::Line(line));
    }

    fn stage(&mut self, index: usize) -> Result<()> {
        if self.cancel.load(Ordering::Relaxed) {
            bail!("cancelled");
        }
        let _ = writeln!(self.log, "\n=== {} ===", STAGES[index]);
        (self.on)(Progress::Stage { index, total: STAGES.len(), name: STAGES[index] });
        Ok(())
    }

    /// Runs a tool in the model folder, streaming its output; kills it on cancel.
    fn exec(&mut self, exe: &Path, args: &[&str]) -> Result<()> {
        let name = exe.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let _ = writeln!(self.log, "> {} {}", exe.display(), args.join(" "));
        let mut cmd = Command::new(exe);
        cmd.args(args).current_dir(&self.dir).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x0000_4000;
            cmd.creation_flags(CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS);
        }
        let mut child = cmd.spawn().with_context(|| format!("can't start {}", exe.display()))?;
        let (tx, rx) = mpsc::channel::<String>();
        let readers: Vec<_> = [child.stdout.take().map(|s| Box::new(s) as Box<dyn std::io::Read + Send>), child.stderr.take().map(|s| Box::new(s) as _)]
            .into_iter()
            .flatten()
            .map(|pipe| {
                let tx = tx.clone();
                std::thread::spawn(move || {
                    for line in BufReader::new(pipe).lines().map_while(|l| l.ok()) {
                        for part in line.split('\r').filter(|p| !p.trim().is_empty()) {
                            let _ = tx.send(part.trim_end().to_owned());
                        }
                    }
                })
            })
            .collect();
        drop(tx);
        let mut tail: Vec<String> = Vec::new();
        let status = loop {
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(line) => {
                    tail.push(line.clone());
                    if tail.len() > 12 {
                        tail.remove(0);
                    }
                    self.say(line);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break child.wait()?,
            }
            if self.cancel.load(Ordering::Relaxed) {
                let _ = child.kill();
                let _ = child.wait();
                bail!("cancelled");
            }
            if let Some(status) = child.try_wait()? {
                for line in rx.iter() {
                    self.say(line);
                }
                break status;
            }
        };
        for r in readers {
            let _ = r.join();
        }
        if !status.success() {
            bail!("{name} failed ({status}). Last output:\n{}", tail.join("\n"));
        }
        Ok(())
    }
}

/// Runs the whole pipeline on a scan folder. Returns the path of model.obj.
pub fn run(scan_dir: &Path, opts: &Options, tools: &Tools, cancel: &AtomicBool, on: &mut dyn FnMut(Progress)) -> Result<PathBuf> {
    let photos = scan::photos(scan_dir)?;
    if photos.len() < 8 {
        bail!("only {} photos: a 3D model needs at least 8, better 24+ (a scan with more photos per ring)", photos.len());
    }
    let dir = scan_dir.join(MODEL_DIR);
    if dir.exists() {
        // Only ever wipe a folder we made ourselves.
        if !dir.join(LOG).is_file() {
            bail!("{} exists and wasn't made by Orbis: rename or delete it first", dir.display());
        }
        std::fs::remove_dir_all(&dir).with_context(|| format!("can't clear {} (open in another program?)", dir.display()))?;
    }
    std::fs::create_dir_all(&dir)?;
    let log = std::fs::File::create(dir.join(LOG))?;
    let mut r = Run { dir: dir.clone(), log, cancel, on };
    r.say(format!("{} photos, quality {}, masks {}", photos.len(), opts.quality.label(), if opts.masks { "on" } else { "off" }));

    // 1. Working copies + masks.
    r.stage(0)?;
    let images = dir.join("images");
    let masks = dir.join("masks");
    std::fs::create_dir_all(&images)?;
    let small = prepare(&photos, &images, opts.quality.image_px(), cancel, &mut r)?;
    let rings: Vec<u32> = photos.iter().map(|p| p.ring).collect();
    if opts.masks {
        std::fs::create_dir_all(&masks)?;
        let names: Vec<String> = photos.iter().map(|p| file_name(&p.path)).collect();
        write_masks(&small, &rings, &mut r, |i, m| {
            let (w, h) = image::image_dimensions(images.join(&names[i]))?;
            // COLMAP convention: masks/<image name>.png, black = ignore.
            m.to_image(w, h).save(masks.join(format!("{}.png", names[i])))?;
            Ok(())
        })?;
    }
    drop(small);

    // 2-5. COLMAP.
    let colmap = &tools.colmap;
    r.stage(1)?;
    let mut fe = vec![
        "feature_extractor", "--database_path", "database.db", "--image_path", "images",
        "--ImageReader.single_camera", "1", "--FeatureExtraction.use_gpu", "0",
    ];
    if opts.masks {
        fe.extend(["--ImageReader.mask_path", "masks"]);
    }
    r.exec(colmap, &fe)?;
    r.stage(2)?;
    r.exec(colmap, &["exhaustive_matcher", "--database_path", "database.db", "--FeatureMatching.use_gpu", "0", "--FeatureMatching.guided_matching", "1"])?;
    r.stage(3)?;
    std::fs::create_dir_all(dir.join("sparse"))?;
    const NO_ALIGN: &str = "COLMAP couldn't align the photos. Usual fixes: more photos per ring (36+, steps of 10° or less), more light and depth of field (f/8 to f/11), a textured object, and check the masks (recon --masks-only)";
    let mapped = r.exec(colmap, &["mapper", "--database_path", "database.db", "--image_path", "images", "--output_path", "sparse"]);
    let best = best_model(&dir.join("sparse"));
    if mapped.is_err() && best.is_none() {
        bail!(NO_ALIGN);
    }
    mapped?;
    let (model, aligned) = best.context(NO_ALIGN)?;
    r.say(format!("Aligned {aligned} of {} photos", photos.len()));
    if aligned < 3 {
        bail!("only {aligned} photos could be aligned");
    }
    if aligned * 2 < photos.len() {
        r.say("⚠ Less than half the photos aligned: the model will have gaps".into());
    }
    r.stage(4)?;
    let model_rel = format!("sparse/{model}");
    let px = opts.quality.image_px().to_string();
    r.exec(colmap, &["image_undistorter", "--image_path", "images", "--input_path", &model_rel, "--output_path", "dense", "--output_type", "COLMAP", "--max_image_size", &px])?;

    // Masks again, for the undistorted images OpenMVS works on.
    let dense_masks = dir.join("dense").join("masks");
    if opts.masks {
        let ring_of: std::collections::HashMap<String, u32> = photos.iter().map(|p| (file_name(&p.path), p.ring)).collect();
        let mut files: Vec<PathBuf> = std::fs::read_dir(dir.join("dense").join("images"))?.filter_map(|e| e.ok().map(|e| e.path())).collect();
        files.sort();
        let und: Vec<Photo> = files.iter().map(|p| Photo { path: p.clone(), ring: ring_of.get(&file_name(p)).copied().unwrap_or(0) }).collect();
        let small = analysis_only(&und, cancel)?;
        std::fs::create_dir_all(&dense_masks)?;
        let rings: Vec<u32> = und.iter().map(|p| p.ring).collect();
        write_masks(&small, &rings, &mut r, |i, m| {
            let (w, h) = image::image_dimensions(&und[i].path)?;
            let stem = und[i].path.file_stem().unwrap_or_default().to_string_lossy().into_owned();
            // OpenMVS convention: <stem>.mask.png.
            m.to_image(w, h).save(dense_masks.join(format!("{stem}.mask.png")))?;
            Ok(())
        })?;
    }

    // 6-8. OpenMVS.
    r.stage(5)?;
    r.exec(&tools.mvs("InterfaceCOLMAP"), &["-i", "dense", "-o", "scene.mvs", "--image-folder", "dense/images"])?;
    let level = opts.quality.dense_level().to_string();
    let mut dense = vec!["-i", "scene.mvs", "-o", "scene_dense.mvs", "--resolution-level", &level];
    if opts.masks {
        dense.extend(["--mask-path", "dense/masks", "--ignore-mask-label", "0"]);
    }
    r.exec(&tools.mvs("DensifyPointCloud"), &dense)?;
    r.stage(6)?;
    r.exec(&tools.mvs("ReconstructMesh"), &["-i", "scene_dense.mvs", "-p", "scene_dense.ply", "-o", "scene_mesh.ply"])?;
    r.stage(7)?;
    r.exec(&tools.mvs("TextureMesh"), &["-i", "scene_dense.mvs", "-m", "scene_mesh.ply", "-o", "model.mvs", "--export-type", "obj"])?;

    let obj = dir.join(MODEL_FILE);
    if !obj.is_file() {
        bail!("TextureMesh finished but {} is missing", obj.display());
    }
    r.say(format!("Done: {}", obj.display()));
    Ok(obj)
}

fn file_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

fn threads() -> usize {
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(12)
}

/// Runs `f(i)` for every index on all cores; results in order.
fn parallel<T: Send>(n: usize, cancel: &AtomicBool, f: impl Fn(usize) -> Result<T> + Sync) -> Result<Vec<T>> {
    let next = AtomicUsize::new(0);
    let slots: Mutex<Vec<Option<Result<T>>>> = Mutex::new((0..n).map(|_| None).collect());
    std::thread::scope(|s| {
        for _ in 0..threads().min(n.max(1)) {
            s.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                if i >= n || cancel.load(Ordering::Relaxed) {
                    break;
                }
                let out = f(i);
                slots.lock().unwrap()[i] = Some(out);
            });
        }
    });
    if cancel.load(Ordering::Relaxed) {
        bail!("cancelled");
    }
    slots.into_inner().unwrap().into_iter().map(|s| s.unwrap_or_else(|| bail!("missing result"))).collect()
}

/// Writes the working copies; returns the analysis-size images for masking.
fn prepare(photos: &[Photo], out: &Path, max_px: u32, cancel: &AtomicBool, r: &mut Run) -> Result<Vec<RgbImage>> {
    let done = AtomicUsize::new(0);
    let total = photos.len();
    let lines = Mutex::new(Vec::new());
    let small = parallel(total, cancel, |i| {
        let p = &photos[i].path;
        let img = image::open(p).with_context(|| format!("can't read {}", p.display()))?.to_rgb8();
        let (w, h) = img.dimensions();
        let s = max_px as f32 / w.max(h) as f32;
        let work = if s < 1.0 {
            image::imageops::resize(&img, (w as f32 * s).round() as u32, (h as f32 * s).round() as u32, image::imageops::FilterType::Triangle)
        } else {
            img
        };
        let dest = out.join(file_name(&photos[i].path));
        let mut f = std::io::BufWriter::new(std::fs::File::create(&dest)?);
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut f, 95).encode_image(&work)?;
        let n = done.fetch_add(1, Ordering::Relaxed) + 1;
        lines.lock().unwrap().push(format!("Prepared {n}/{total}: {}", file_name(&photos[i].path)));
        Ok(mask::analysis_image(&work))
    })?;
    for l in lines.into_inner().unwrap() {
        r.say(l);
    }
    Ok(small)
}

fn analysis_only(photos: &[Photo], cancel: &AtomicBool) -> Result<Vec<RgbImage>> {
    parallel(photos.len(), cancel, |i| {
        let img = image::open(&photos[i].path).with_context(|| format!("can't read {}", photos[i].path.display()))?.to_rgb8();
        Ok(mask::analysis_image(&img))
    })
}

/// Masks per ring; `save(i, mask)` writes the mask of photo `i`.
fn write_masks(small: &[RgbImage], rings: &[u32], r: &mut Run, save: impl Fn(usize, &mask::Mask) -> Result<()>) -> Result<()> {
    let mut ring_ids: Vec<u32> = rings.to_vec();
    ring_ids.sort();
    ring_ids.dedup();
    for ring in ring_ids {
        let idx: Vec<usize> = (0..small.len()).filter(|&i| rings[i] == ring).collect();
        let imgs: Vec<RgbImage> = idx.iter().map(|&i| small[i].clone()).collect();
        let masks = mask::ring_masks(&imgs);
        let fallback = masks.iter().filter(|m| m.fallback).count();
        let cover = masks.iter().filter(|m| !m.fallback).map(|m| m.coverage()).sum::<f32>() / (masks.len() - fallback).max(1) as f32;
        r.say(if fallback == masks.len() {
            format!("Ring {}: no masks ({} photos; needs {}+ with the object turning)", ring + 1, masks.len(), mask::MIN_PHOTOS)
        } else {
            format!("Ring {}: masks keep {:.0} % of the frame on average{}", ring + 1, cover * 100.0, if fallback > 0 { format!(", {fallback} unmasked") } else { String::new() })
        });
        for (k, m) in masks.iter().enumerate() {
            save(idx[k], m)?;
        }
    }
    Ok(())
}

/// The sparse model with the most aligned images: (folder name, count).
fn best_model(sparse: &Path) -> Option<(String, usize)> {
    std::fs::read_dir(sparse)
        .ok()?
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let bytes = std::fs::read(e.path().join("images.bin")).ok()?;
            let n = u64::from_le_bytes(bytes.get(..8)?.try_into().ok()?) as usize;
            Some((e.file_name().to_string_lossy().into_owned(), n))
        })
        .max_by_key(|(_, n)| *n)
}
