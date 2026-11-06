//! Reads a scan folder: which JPEGs belong to which ring.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

/// One photo of the scan.
#[derive(Debug, Clone)]
pub struct Photo {
    pub path: PathBuf,
    /// Camera height. Masks are computed per ring, because the camera (and
    /// so the background) only stays put within one ring.
    pub ring: u32,
}

fn is_jpeg(p: &Path) -> bool {
    p.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("jpg") || e.eq_ignore_ascii_case("jpeg"))
}

/// The JPEGs of a scan, in shooting order. Uses `scan.json` when it's there
/// (rings), otherwise every JPEG in the folder as one ring.
pub fn photos(dir: &Path) -> Result<Vec<Photo>> {
    let mut out = Vec::new();
    if let Ok(text) = std::fs::read_to_string(dir.join("scan.json")) {
        let json: serde_json::Value = serde_json::from_str(&text).context("scan.json")?;
        for shot in json["shots"].as_array().into_iter().flatten() {
            let ring = shot["ring"].as_u64().unwrap_or(0) as u32;
            for f in shot["files"].as_array().into_iter().flatten().filter_map(|f| f.as_str()) {
                let path = dir.join(f);
                if is_jpeg(&path) && path.is_file() {
                    out.push(Photo { path, ring });
                }
            }
        }
    }
    if out.is_empty() {
        let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
            .with_context(|| format!("can't read {}", dir.display()))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| is_jpeg(p))
            .collect();
        files.sort();
        out = files.into_iter().map(|path| Photo { path, ring: 0 }).collect();
    }
    if out.is_empty() {
        bail!("no JPEG photos in {}: shoot JPEG or RAW + JPEG", dir.display());
    }
    Ok(out)
}
