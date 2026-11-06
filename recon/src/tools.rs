//! Finds (and installs) COLMAP and OpenMVS.
//!
//! Looked up in `ORBIS_TOOLS_DIR`, then `%LOCALAPPDATA%\Orbis\tools`. They
//! aren't bundled with Orbis: [`install`] downloads the official CPU builds
//! from GitHub on first use (or run `scripts\get-recon-tools.ps1`).

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

pub const COLMAP_URL: &str = "https://github.com/colmap/colmap/releases/download/4.2.1/colmap-x64-windows-nocuda.zip";
pub const OPENMVS_URL: &str = "https://github.com/cdcseacave/openMVS/releases/download/v2.4.0/OpenMVS_Windows_x64.zip";

#[derive(Debug, Clone)]
pub struct Tools {
    pub colmap: PathBuf,
    /// Folder with InterfaceCOLMAP.exe, DensifyPointCloud.exe, ...
    pub openmvs: PathBuf,
}

impl Tools {
    pub fn mvs(&self, exe: &str) -> PathBuf {
        self.openmvs.join(format!("{exe}.exe"))
    }
}

/// Where the tools live (whether or not they're installed yet).
pub fn dir() -> PathBuf {
    if let Ok(d) = std::env::var("ORBIS_TOOLS_DIR") {
        return PathBuf::from(d);
    }
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join("Orbis").join("tools")
}

/// Depth-limited search for a file name below `root`.
fn find(root: &Path, name: &str, depth: u32) -> Option<PathBuf> {
    let entries = std::fs::read_dir(root).ok()?;
    let mut dirs = Vec::new();
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            dirs.push(p);
        } else if p.file_name().is_some_and(|n| n.eq_ignore_ascii_case(name)) {
            return Some(p);
        }
    }
    if depth == 0 {
        return None;
    }
    dirs.sort();
    dirs.iter().find_map(|d| find(d, name, depth - 1))
}

/// The installed tools, or None when something is missing.
pub fn locate() -> Option<Tools> {
    let root = dir();
    let colmap = find(&root, "colmap.exe", 4)?;
    let openmvs = find(&root, "DensifyPointCloud.exe", 5)?.parent()?.to_path_buf();
    for exe in ["InterfaceCOLMAP", "ReconstructMesh", "TextureMesh"] {
        if !openmvs.join(format!("{exe}.exe")).is_file() {
            return None;
        }
    }
    Some(Tools { colmap, openmvs })
}

/// Downloads and unpacks both tools (about 140 MB) with the curl.exe and
/// tar.exe that ship with Windows. `say` gets one line per step.
pub fn install(mut say: impl FnMut(String)) -> Result<Tools> {
    let root = dir();
    std::fs::create_dir_all(&root).with_context(|| format!("can't create {}", root.display()))?;
    for (name, url) in [("colmap", COLMAP_URL), ("openmvs", OPENMVS_URL)] {
        let dest = root.join(name);
        if dest.is_dir() && std::fs::read_dir(&dest).map(|mut d| d.next().is_some()).unwrap_or(false) {
            continue;
        }
        let zip = root.join(format!("{name}.zip"));
        say(format!("Downloading {name}: {url}"));
        let ok = Command::new("curl.exe")
            .args(["-L", "--fail", "--silent", "--show-error", "-o"])
            .arg(&zip)
            .arg(url)
            .status()
            .context("curl.exe not found")?
            .success();
        if !ok {
            bail!("download failed: {url}");
        }
        say(format!("Unpacking {name}"));
        std::fs::create_dir_all(&dest)?;
        let ok = Command::new("tar.exe").arg("-xf").arg(&zip).arg("-C").arg(&dest).status().context("tar.exe not found")?.success();
        let _ = std::fs::remove_file(&zip);
        if !ok {
            let _ = std::fs::remove_dir_all(&dest);
            bail!("unpacking {name} failed");
        }
    }
    locate().context("tools downloaded, but colmap.exe / OpenMVS executables not found in them")
}
