//! Live view of a photo's way from the camera to the PC: a ring around the
//! wheel's center fills up, with the measured speed underneath.
//!
//! The SDK reads each image (RAW, then JPEG) in one blocking call, so the
//! exact progress is only known at image boundaries. In between, the ring
//! moves at the speed measured on earlier images and snaps to the real
//! value when an image arrives.

use std::time::{Duration, Instant};

use crate::camera::Transfer;

/// How long the full ring and the summary stay after a transfer.
const LINGER: Duration = Duration::from_millis(1500);

pub struct TransferMeter {
    /// Bytes per second, smoothed over recent images.
    rate: f64,
    /// Size of the last complete photo (all its images), the target for
    /// the next one until its real size is known.
    expected: f64,
    active: Option<Active>,
    last: Option<(Summary, Instant)>,
}

struct Active {
    /// First image read started (None = exposed, nothing read yet).
    started: Option<Instant>,
    done_bytes: usize,
    /// Time spent reading images (the transfer itself, without saving).
    read_secs: f64,
    /// Image being read: its size and when it started.
    current: Option<(usize, Instant)>,
}

#[derive(Clone, Copy)]
pub struct Summary {
    pub bytes: usize,
    pub secs: f64,
}

impl Default for TransferMeter {
    fn default() -> Self {
        // USB on the X-T4 measured ~20 MB/s; RAW + JPEG ~38 MB, JPEG Fine
        // ~13 MB. The target adapts after the first photo.
        Self { rate: 20e6, expected: 38e6, active: None, last: None }
    }
}

impl TransferMeter {
    pub fn on(&mut self, t: Transfer) {
        let now = Instant::now();
        match t {
            Transfer::Exposed => {
                self.active = Some(Active { started: None, done_bytes: 0, read_secs: 0.0, current: None });
            }
            Transfer::Image { bytes } => {
                let a = self.active.get_or_insert(Active { started: None, done_bytes: 0, read_secs: 0.0, current: None });
                a.started.get_or_insert(now);
                a.current = Some((bytes, now));
                // A bigger photo than the last one: grow the target.
                self.expected = self.expected.max((a.done_bytes + bytes) as f64);
            }
            Transfer::ImageDone { bytes, secs } => {
                if let Some(a) = &mut self.active {
                    a.done_bytes += bytes;
                    a.read_secs += secs as f64;
                    a.current = None;
                }
                // Tiny or instant reads say nothing about the link speed.
                if bytes > 1_000_000 && secs > 0.05 {
                    let r = bytes as f64 / secs as f64;
                    self.rate = 0.5 * self.rate + 0.5 * r;
                }
            }
            Transfer::Done { ok } => {
                if let Some(a) = self.active.take() {
                    if ok && a.done_bytes > 0 {
                        self.expected = a.done_bytes as f64;
                        self.last = Some((Summary { bytes: a.done_bytes, secs: a.read_secs }, now));
                    }
                }
            }
        }
    }

    /// 0..1 while a transfer runs (and full for a moment after it).
    pub fn progress(&self) -> Option<f32> {
        if let Some(a) = &self.active {
            let current = a.current.map_or(0.0, |(bytes, t)| {
                // Estimated, but never shown as finished before it is.
                (self.rate * t.elapsed().as_secs_f64()).min(bytes as f64 * 0.97)
            });
            let p = (a.done_bytes as f64 + current) / self.expected.max(1.0);
            return Some(p.clamp(0.0, 0.99) as f32);
        }
        match self.last {
            Some((_, t)) if t.elapsed() < LINGER => Some(1.0),
            _ => None,
        }
    }

    /// Short text for under the ring: live speed, then the result.
    pub fn label(&self) -> Option<String> {
        if let Some(a) = &self.active {
            return Some(match a.started {
                None => "exposed".into(),
                Some(_) => format!("⇣ {:.0} MB/s", self.rate / 1e6),
            });
        }
        let (s, _) = self.last?;
        let rate = if s.secs > 0.0 { s.bytes as f64 / s.secs / 1e6 } else { 0.0 };
        Some(format!("{:.0} MB · {:.1} s · {:.0} MB/s", s.bytes as f64 / 1e6, s.secs, rate))
    }

    /// Something is moving: keep repainting for the animation.
    pub fn animating(&self) -> bool {
        self.active.is_some() || self.last.is_some_and(|(_, t)| t.elapsed() < LINGER)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fills_and_reports() {
        let mut m = TransferMeter::default();
        assert!(m.progress().is_none());
        m.on(Transfer::Exposed);
        assert_eq!(m.progress(), Some(0.0));
        m.on(Transfer::Image { bytes: 25_000_000 });
        m.on(Transfer::ImageDone { bytes: 25_000_000, secs: 1.0 });
        let p = m.progress().unwrap();
        assert!(p > 0.6 && p < 0.7, "{p}");
        m.on(Transfer::Image { bytes: 13_000_000 });
        m.on(Transfer::ImageDone { bytes: 13_000_000, secs: 0.5 });
        m.on(Transfer::Done { ok: true });
        assert_eq!(m.progress(), Some(1.0));
        assert_eq!(m.label().unwrap(), "38 MB · 1.5 s · 25 MB/s");
        // The next photo aims at the real size of this one.
        assert_eq!(m.expected, 38e6);
    }

    #[test]
    fn failed_capture_clears() {
        let mut m = TransferMeter::default();
        m.on(Transfer::Exposed);
        m.on(Transfer::Done { ok: false });
        assert!(m.progress().is_none());
        assert!(m.label().is_none());
    }
}
