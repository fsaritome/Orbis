//! What a scan looks like: how many photos, at which angles, how long.

use std::time::Duration;

use rig::Geometry;
use serde::{Deserialize, Serialize};

/// Detail level → angle between shots.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Preset {
    Quick,
    Standard,
    Fine,
    Custom,
}

impl Preset {
    pub const ALL: [Preset; 4] = [Preset::Quick, Preset::Standard, Preset::Fine, Preset::Custom];

    pub fn label(self) -> &'static str {
        match self {
            Preset::Quick => "Quick",
            Preset::Standard => "Standard",
            Preset::Fine => "Fine",
            Preset::Custom => "Custom",
        }
    }

    /// Shots per ring for the fixed presets.
    pub fn shots(self) -> Option<u32> {
        match self {
            Preset::Quick => Some(24),
            Preset::Standard => Some(36),
            Preset::Fine => Some(72),
            Preset::Custom => None,
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            Preset::Quick => "15° steps: fast preview scans, simple shapes",
            Preset::Standard => "10° steps: good default for most objects",
            Preset::Fine => "5° steps: small objects, fine surface detail",
            Preset::Custom => "pick your own angle",
        }
    }
}

/// Scan settings as edited in the UI and saved between sessions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanSettings {
    pub preset: Preset,
    /// Used when `preset == Custom`.
    pub custom_shots: u32,
    /// Camera heights. The app pauses between rings so you can move the camera.
    pub rings: u32,
    /// Wait after each move before shooting, so the object stops wobbling.
    pub settle_ms: u64,
}

impl Default for ScanSettings {
    fn default() -> Self {
        Self { preset: Preset::Standard, custom_shots: 48, rings: 3, settle_ms: 500 }
    }
}

impl ScanSettings {
    pub fn shots_per_ring(&self) -> u32 {
        self.preset.shots().unwrap_or(self.custom_shots).clamp(4, 360)
    }

    pub fn plan(&self) -> ScanPlan {
        ScanPlan {
            shots_per_ring: self.shots_per_ring(),
            rings: self.rings.clamp(1, 10),
            settle: Duration::from_millis(self.settle_ms),
        }
    }
}

/// A fixed scan, handed to the turntable worker.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScanPlan {
    pub shots_per_ring: u32,
    pub rings: u32,
    pub settle: Duration,
}

/// Rough time for one shot with a real camera (S1 + S2 + transfer).
pub const CAMERA_SHOT_TIME: Duration = Duration::from_millis(2500);
/// X-T4 fine JPEG, 26 MP.
pub const JPEG_MB: f64 = 12.0;

impl ScanPlan {
    pub fn angle_step(&self) -> f64 {
        360.0 / self.shots_per_ring as f64
    }

    pub fn total_shots(&self) -> u32 {
        self.shots_per_ring * self.rings
    }

    /// Angle of shot `i` within a ring.
    pub fn angle_of(&self, i: u32) -> f64 {
        self.angle_step() * i as f64
    }

    /// Estimated scan time, excluding the pauses where you move the camera.
    pub fn estimate(&self, geo: &Geometry, max_speed: u32, accel: u32, shot_time: Duration) -> Duration {
        let steps = geo.degrees_to_steps(self.angle_step()).unsigned_abs();
        let per_move = move_time(steps, max_speed, accel);
        // Network round trips per shot (status + move + reply), measured ≈ 0.3 s.
        let overhead = Duration::from_millis(300);
        (per_move + self.settle + shot_time + overhead) * self.total_shots()
    }

    pub fn disk_mb(&self) -> f64 {
        self.total_shots() as f64 * JPEG_MB
    }
}

/// Duration of a trapezoidal move (same profile as the firmware).
pub fn move_time(steps: u32, max_speed: u32, accel: u32) -> Duration {
    let (s, v, a) = (steps as f64, max_speed.max(1) as f64, accel.max(1) as f64);
    let ramp_steps = v * v / a; // accelerate + decelerate
    let secs = if s >= ramp_steps { s / v + v / a } else { 2.0 * (s / a).sqrt() };
    Duration::from_secs_f64(secs)
}

pub fn format_duration(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 3600 {
        format!("{} h {:02} min", s / 3600, (s % 3600) / 60)
    } else if s >= 60 {
        format!("{} min {:02} s", s / 60, s % 60)
    } else {
        format!("{s} s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_map_to_angles() {
        for (preset, step) in [(Preset::Quick, 15.0), (Preset::Standard, 10.0), (Preset::Fine, 5.0)] {
            let s = ScanSettings { preset, ..Default::default() };
            assert_eq!(s.plan().angle_step(), step);
        }
    }

    #[test]
    fn custom_shots_are_clamped() {
        let s = ScanSettings { preset: Preset::Custom, custom_shots: 1, ..Default::default() };
        assert_eq!(s.shots_per_ring(), 4);
        let s = ScanSettings { preset: Preset::Custom, custom_shots: 10_000, ..Default::default() };
        assert_eq!(s.shots_per_ring(), 360);
    }

    #[test]
    fn totals_and_disk() {
        let p = ScanSettings::default().plan(); // 36 × 3
        assert_eq!(p.total_shots(), 108);
        assert_eq!(p.disk_mb(), 108.0 * JPEG_MB);
    }

    #[test]
    fn move_time_matches_profile() {
        // Long move reaches top speed: 3200 steps at 1600/s, accel 3200/s².
        let t = move_time(3200, 1600, 3200).as_secs_f64();
        assert!((t - 2.5).abs() < 1e-9, "{t}");
        // Short move never reaches top speed: 2·sqrt(100/3200).
        let t = move_time(100, 1600, 3200).as_secs_f64();
        assert!((t - 2.0 * (100.0f64 / 3200.0).sqrt()).abs() < 1e-9, "{t}");
    }

    #[test]
    fn durations_read_well() {
        assert_eq!(format_duration(Duration::from_secs(42)), "42 s");
        assert_eq!(format_duration(Duration::from_secs(125)), "2 min 05 s");
        assert_eq!(format_duration(Duration::from_secs(3720)), "1 h 02 min");
    }
}
