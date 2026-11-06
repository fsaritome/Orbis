//! Orbis photogrammetry: turns a scan folder into a textured 3D model.
//!
//! Orbis doesn't reconstruct anything itself. It prepares the photos (smaller
//! working copies plus masks that cut away the static background), then
//! drives two proven open-source tools as separate processes:
//!
//! * **COLMAP** (BSD-3): features, matching, camera alignment, undistortion.
//! * **OpenMVS** (AGPL-3): dense point cloud, mesh, texture.
//!
//! Both run on the CPU, so no NVIDIA GPU is needed. See [`tools`] for where
//! they're found and how they're installed.

pub mod mask;
pub mod pipeline;
pub mod scan;
pub mod tools;

pub use pipeline::{run, Options, Progress, Quality};
