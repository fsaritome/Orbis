//! Background masks for turntable scans, without any ML model.
//!
//! Within one ring the camera stands still and only the object turns, so the
//! per-pixel median of all photos of the ring is the scene *without* the
//! object's changing surface: wall, table, stand, cables. Whatever differs
//! from that median in a photo is the turning object. Anything textured
//! enough to be reconstructed at all shows up as change; plain areas inside
//! the object are filled in as holes.
//!
//! The masks matter because a static background contradicts the turning
//! object: photogrammetry would see two worlds moving against each other.

use image::{imageops, GrayImage, Luma, RgbImage};

/// A binary mask at analysis resolution (`true` = object).
#[derive(Debug, Clone)]
pub struct Mask {
    pub w: u32,
    pub h: u32,
    pub on: Vec<bool>,
    /// The mask couldn't be computed and keeps everything.
    pub fallback: bool,
}

/// Long side of the images the masks are computed on.
pub const ANALYSIS_PX: u32 = 640;
/// Fewer photos than this per ring can't give a median background.
pub const MIN_PHOTOS: usize = 5;

impl Mask {
    pub fn full(w: u32, h: u32) -> Self {
        Mask { w, h, on: vec![true; (w * h) as usize], fallback: true }
    }

    pub fn coverage(&self) -> f32 {
        self.on.iter().filter(|&&b| b).count() as f32 / self.on.len().max(1) as f32
    }

    /// The mask scaled to `w`×`h`: 255 = object, 0 = ignore.
    pub fn to_image(&self, w: u32, h: u32) -> GrayImage {
        let small = GrayImage::from_fn(self.w, self.h, |x, y| Luma([if self.on[(y * self.w + x) as usize] { 255 } else { 0 }]));
        if (w, h) == (self.w, self.h) {
            small
        } else {
            // Bilinear then threshold: smooth edges instead of blocks.
            let mut big = imageops::resize(&small, w, h, imageops::FilterType::Triangle);
            for p in big.pixels_mut() {
                p.0[0] = if p.0[0] >= 128 { 255 } else { 0 };
            }
            big
        }
    }
}

/// Shrinks a photo to analysis size.
pub fn analysis_image(img: &RgbImage) -> RgbImage {
    let (w, h) = img.dimensions();
    let s = ANALYSIS_PX as f32 / w.max(h) as f32;
    if s >= 1.0 {
        return img.clone();
    }
    imageops::thumbnail(img, ((w as f32 * s).round() as u32).max(1), ((h as f32 * s).round() as u32).max(1))
}

/// Masks for the photos of one ring (all the same size, analysis resolution).
pub fn ring_masks(images: &[RgbImage]) -> Vec<Mask> {
    let Some(first) = images.first() else { return Vec::new() };
    let (w, h) = first.dimensions();
    if images.len() < MIN_PHOTOS || images.iter().any(|i| i.dimensions() != (w, h)) {
        return images.iter().map(|_| Mask::full(w, h)).collect();
    }
    let n = images.len();
    let px = (w * h) as usize;

    // Median background, per pixel and channel.
    let mut median = vec![0u8; px * 3];
    let mut vals = vec![0u8; n];
    for (i, m) in median.iter_mut().enumerate() {
        for (k, img) in images.iter().enumerate() {
            vals[k] = img.as_raw()[i];
        }
        *m = *vals.select_nth_unstable(n / 2).1;
    }

    // Difference of each photo against it (largest channel difference).
    let diffs: Vec<Vec<u8>> = images
        .iter()
        .map(|img| {
            let raw = img.as_raw();
            (0..px)
                .map(|p| (0..3).map(|c| raw[p * 3 + c].abs_diff(median[p * 3 + c])).max().unwrap_or(0))
                .collect()
        })
        .collect();

    // Threshold from the noise: most pixels are background, so the median
    // difference over everything is the sensor/compression noise level.
    let mut sample: Vec<u8> = diffs.iter().flat_map(|d| d.iter().step_by(7).copied()).collect();
    let mid = sample.len() / 2;
    let noise = *sample.select_nth_unstable(mid).1 as u32;
    let threshold = (noise * 4 + 8).clamp(14, 60) as u8;

    diffs.iter().map(|d| clean(w, h, d.iter().map(|&v| v > threshold).collect())).collect()
}

/// Closing (joins the change edges), keeping the big blobs (drops noise
/// speckles), filling holes, margin. No opening: an erosion would wipe out
/// the thin edges a small turning step leaves.
fn clean(w: u32, h: u32, mut on: Vec<bool>) -> Mask {
    let (wu, hu) = (w as usize, h as usize);
    let r_close = (w.max(h) / 90).max(2) as usize;
    let r_margin = (w.max(h) / 70).max(2) as usize;

    on = morph(&on, wu, hu, r_close, true); // close: dilate ...
    on = morph(&on, wu, hu, r_close, false); // ... erode
    on = keep_big_blobs(&on, wu, hu);
    on = fill_holes(&on, wu, hu);
    on = morph(&on, wu, hu, r_margin, true);

    let mask = Mask { w, h, on, fallback: false };
    let c = mask.coverage();
    if !(0.003..=0.92).contains(&c) {
        return Mask::full(w, h);
    }
    mask
}

/// Square dilation (`grow`) or erosion with radius `r`, separable.
fn morph(on: &[bool], w: usize, h: usize, r: usize, grow: bool) -> Vec<bool> {
    let pass = |src: &[bool], len: usize, stride: usize, lines: usize, step: usize| -> Vec<bool> {
        let mut out = vec![false; src.len()];
        let mut prefix = vec![0u32; len + 1];
        for line in 0..lines {
            let base = line * step;
            for i in 0..len {
                prefix[i + 1] = prefix[i] + src[base + i * stride] as u32;
            }
            for i in 0..len {
                let (a, b) = (i.saturating_sub(r), (i + r + 1).min(len));
                let count = prefix[b] - prefix[a];
                out[base + i * stride] = if grow { count > 0 } else { count == (b - a) as u32 };
            }
        }
        out
    };
    let rows = pass(on, w, 1, h, w);
    pass(&rows, h, w, w, 1)
}

/// 4-connected components; keeps those at least 15 % of the largest.
fn keep_big_blobs(on: &[bool], w: usize, h: usize) -> Vec<bool> {
    let mut label = vec![0u32; on.len()];
    let mut sizes = vec![0usize]; // index = label
    let mut stack = Vec::new();
    for start in 0..on.len() {
        if !on[start] || label[start] != 0 {
            continue;
        }
        let id = sizes.len() as u32;
        let mut size = 0;
        label[start] = id;
        stack.push(start);
        while let Some(p) = stack.pop() {
            size += 1;
            let (x, y) = (p % w, p / w);
            let mut visit = |q: usize| {
                if on[q] && label[q] == 0 {
                    label[q] = id;
                    stack.push(q);
                }
            };
            if x > 0 { visit(p - 1) }
            if x + 1 < w { visit(p + 1) }
            if y > 0 { visit(p - w) }
            if y + 1 < h { visit(p + w) }
        }
        sizes.push(size);
    }
    let largest = sizes.iter().copied().max().unwrap_or(0);
    let min = (largest * 15 / 100).max(on.len() / 1000).max(1);
    label.iter().map(|&l| l != 0 && sizes[l as usize] >= min).collect()
}

/// Background regions not connected to the image border become object.
fn fill_holes(on: &[bool], w: usize, h: usize) -> Vec<bool> {
    let mut outside = vec![false; on.len()];
    let mut stack: Vec<usize> = (0..w).flat_map(|x| [x, (h - 1) * w + x]).chain((0..h).flat_map(|y| [y * w, y * w + w - 1])).collect();
    while let Some(p) = stack.pop() {
        if on[p] || outside[p] {
            continue;
        }
        outside[p] = true;
        let (x, y) = (p % w, p / w);
        if x > 0 { stack.push(p - 1) }
        if x + 1 < w { stack.push(p + 1) }
        if y > 0 { stack.push(p - w) }
        if y + 1 < h { stack.push(p + w) }
    }
    outside.iter().map(|&o| !o).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A static textured background with a turning textured box in the middle.
    fn ring(n: usize) -> Vec<RgbImage> {
        (0..n)
            .map(|k| {
                RgbImage::from_fn(200, 120, |x, y| {
                    let in_obj = (80..120).contains(&x) && (30..100).contains(&y);
                    if in_obj {
                        // Texture that shifts with the "rotation".
                        let v = (((x as usize + k * 7) / 4 + (y as usize) / 4) % 2) as u8 * 200 + 30;
                        image::Rgb([v, v / 2, 255 - v])
                    } else {
                        let v = ((x / 10 + y / 10) % 2) as u8 * 60 + 80;
                        image::Rgb([v, v, v])
                    }
                })
            })
            .collect()
    }

    #[test]
    fn finds_the_turning_object_and_drops_the_background() {
        let masks = ring(12);
        let masks = ring_masks(&masks);
        assert_eq!(masks.len(), 12);
        for m in &masks {
            assert!(!m.fallback);
            let at = |x: u32, y: u32| m.on[(y * m.w + x) as usize];
            assert!(at(100, 65), "object centre");
            assert!(!at(10, 10) && !at(190, 110), "background corners");
            let c = m.coverage();
            assert!((0.08..0.3).contains(&c), "coverage {c}");
        }
    }

    #[test]
    fn too_few_photos_keep_everything() {
        let masks = ring_masks(&ring(3));
        assert!(masks.iter().all(|m| m.fallback && m.coverage() == 1.0));
    }

    #[test]
    fn holes_are_filled() {
        let (w, h) = (20, 20);
        let on: Vec<bool> = (0..w * h).map(|p| { let (x, y) = (p % w, p / w); (5..15).contains(&x) && (5..15).contains(&y) && !((8..12).contains(&x) && (8..12).contains(&y)) }).collect();
        let filled = fill_holes(&on, w, h);
        assert!(filled[10 * w + 10]);
        assert!(!filled[0]);
    }
}
