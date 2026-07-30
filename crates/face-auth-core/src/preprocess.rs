use image::{DynamicImage, ImageBuffer, Luma};
use tract_onnx::prelude::tract_ndarray::Array3;

use crate::detector::FaceBox;

/// Crops a raw IR frame down to the detected face, expanded by `margin` (a
/// fraction of the box's own width/height on each side) and padded to a
/// square so the encoder's `resize_exact` doesn't distort the aspect ratio.
/// Clamped to the source frame's bounds.
pub fn crop_to_face(frame: &super::capture::IrFrame, face_box: &FaceBox, margin: f32) -> super::capture::IrFrame {
    let width = frame.width as f32;
    let height = frame.height as f32;

    let x1 = (face_box.x1 * width).clamp(0.0, width);
    let y1 = (face_box.y1 * height).clamp(0.0, height);
    let x2 = (face_box.x2 * width).clamp(0.0, width);
    let y2 = (face_box.y2 * height).clamp(0.0, height);

    let box_w = (x2 - x1).max(1.0);
    let box_h = (y2 - y1).max(1.0);

    let mx = box_w * margin;
    let my = box_h * margin;
    let ex1 = x1 - mx;
    let ey1 = y1 - my;
    let ex2 = x2 + mx;
    let ey2 = y2 + my;

    let cx = (ex1 + ex2) / 2.0;
    let cy = (ey1 + ey2) / 2.0;
    let side = (ex2 - ex1).max(ey2 - ey1).min(width.min(height));

    let mut sx1 = cx - side / 2.0;
    let mut sy1 = cy - side / 2.0;
    let mut sx2 = cx + side / 2.0;
    let mut sy2 = cy + side / 2.0;

    // Clamp to frame bounds by shifting the window rather than shrinking it,
    // so a face near an edge still gets a full-size (just re-centered) crop.
    if sx1 < 0.0 { sx2 -= sx1; sx1 = 0.0; }
    if sy1 < 0.0 { sy2 -= sy1; sy1 = 0.0; }
    if sx2 > width { sx1 -= sx2 - width; sx2 = width; }
    if sy2 > height { sy1 -= sy2 - height; sy2 = height; }
    sx1 = sx1.clamp(0.0, width);
    sy1 = sy1.clamp(0.0, height);
    sx2 = sx2.clamp(0.0, width);
    sy2 = sy2.clamp(0.0, height);

    let ix1 = sx1.round() as u32;
    let iy1 = sy1.round() as u32;
    let ix2 = (sx2.round() as u32).max(ix1 + 1).min(frame.width);
    let iy2 = (sy2.round() as u32).max(iy1 + 1).min(frame.height);

    let crop_w = ix2 - ix1;
    let crop_h = iy2 - iy1;

    let mut data = Vec::with_capacity((crop_w * crop_h) as usize);
    for y in iy1..iy2 {
        let row_start = (y * frame.width + ix1) as usize;
        let row_end = row_start + crop_w as usize;
        data.extend_from_slice(&frame.data[row_start..row_end]);
    }

    super::capture::IrFrame { data, width: crop_w, height: crop_h }
}

pub fn preprocess_ir_frame(frame: &super::capture::IrFrame) -> anyhow::Result<Array3<f32>> {
    let width = frame.width as u32;
    let height = frame.height as u32;
    
    let img_buffer: ImageBuffer<Luma<u16>, Vec<u16>> = ImageBuffer::from_fn(width, height, |x, y| {
        let idx = (y * width + x) as usize;
        let val = frame.data.get(idx).copied().unwrap_or(0);
        Luma([val])
    });
    
    let dynamic_img = DynamicImage::ImageLuma16(img_buffer);
    let resized = dynamic_img.resize_exact(112, 112, image::imageops::FilterType::Lanczos3);
    let gray_img = resized.to_luma16();
    
    let mut array = Array3::<f32>::zeros((3, 112, 112));
    
    for y in 0..112usize {
        for x in 0..112usize {
            let pixel = gray_img.get_pixel(x as u32, y as u32).0[0] as f32 / 65535.0;
            let normalized = (pixel - 0.5) / 0.5;
            for c in 0..3usize {
                array[[c, y, x]] = normalized;
            }
        }
    }
    
    Ok(array)
}

/// Fraction of pixels that changed by more than a noise-floor amount between two equalized
/// IR frames of the same dimensions. Used as a cheap liveness signal: a rigidly-held static
/// photo produces near-zero motion, while a real face has natural micro-motion (blinks,
/// breathing, postural sway) even when trying to hold still. Whole-frame averaging would dilute
/// small, localized motion (e.g. an eye blink) against a mostly-static background, so this
/// counts changed pixels instead of averaging raw differences.
pub fn frame_motion_fraction(a: &super::capture::IrFrame, b: &super::capture::IrFrame) -> f32 {
    if a.width != b.width || a.height != b.height || a.data.len() != b.data.len() || a.data.is_empty() {
        return 0.0;
    }

    const PER_PIXEL_NOISE_FLOOR: i32 = 1500; // ~2.3% of the full 16-bit equalized range

    let changed = a.data.iter().zip(b.data.iter())
        .filter(|(&x, &y)| (x as i32 - y as i32).abs() > PER_PIXEL_NOISE_FLOOR)
        .count();

    changed as f32 / a.data.len() as f32
}

const CLAHE_TILES_X: u32 = 8;
const CLAHE_TILES_Y: u32 = 8;
/// Bins are clipped at this multiple of a tile's average bin height before the clipped excess
/// is redistributed evenly across the tile's histogram. Global equalization over-amplifies flat
/// regions (most of an IR frame outside the face) into visible noise; a moderate clip keeps the
/// local contrast boost from doing the same to the sensor's own read noise.
const CLAHE_CLIP_FACTOR: f32 = 4.0;

/// Contrast-Limited Adaptive Histogram Equalization. Unlike a single frame-wide histogram, this
/// computes a separate tone mapping per tile (so per-region IR falloff — e.g. dimmer toward the
/// edges of the face — gets corrected locally) and bilinearly interpolates between neighboring
/// tiles' mappings so tile boundaries don't show up as visible seams.
///
/// Bins on the true 8-bit sample value (`val / 257`), not the full `u16` range: `capture_frame`
/// upscales each raw byte by `* 257` to fill `u16`'s range, so the source data only ever has 256
/// distinct levels — binning across 65536 mostly-empty buckets would waste work without adding
/// resolution.
pub fn clahe(frame: &mut super::capture::IrFrame) {
    let width = frame.width;
    let height = frame.height;
    if width == 0 || height == 0 || frame.data.is_empty() {
        return;
    }

    let tiles_x = CLAHE_TILES_X.min(width).max(1);
    let tiles_y = CLAHE_TILES_Y.min(height).max(1);

    let col_bounds = tile_bounds(width, tiles_x);
    let row_bounds = tile_bounds(height, tiles_y);

    let mappings: Vec<Vec<[u8; 256]>> = row_bounds
        .iter()
        .map(|&(y0, y1)| {
            col_bounds
                .iter()
                .map(|&(x0, x1)| tile_mapping(frame, x0, x1, y0, y1))
                .collect()
        })
        .collect();

    let col_centers: Vec<f32> = col_bounds.iter().map(|&(s, e)| (s + e) as f32 / 2.0).collect();
    let row_centers: Vec<f32> = row_bounds.iter().map(|&(s, e)| (s + e) as f32 / 2.0).collect();

    let mut out = vec![0u16; frame.data.len()];
    for y in 0..height {
        let (ty0, ty1, wy) = neighbor_weights(y as f32, &row_centers);
        for x in 0..width {
            let (tx0, tx1, wx) = neighbor_weights(x as f32, &col_centers);
            let idx = (y * width + x) as usize;
            let val = (frame.data[idx] / 257) as usize;

            let v00 = mappings[ty0][tx0][val] as f32;
            let v01 = mappings[ty0][tx1][val] as f32;
            let v10 = mappings[ty1][tx0][val] as f32;
            let v11 = mappings[ty1][tx1][val] as f32;

            let top = v00 * (1.0 - wx) + v01 * wx;
            let bottom = v10 * (1.0 - wx) + v11 * wx;
            let mapped = top * (1.0 - wy) + bottom * wy;

            out[idx] = (mapped.round() as u16) * 257;
        }
    }

    frame.data = out;
}

/// Partitions `dim` into `tiles` contiguous, roughly-equal ranges (`[start, end)`), with any
/// remainder absorbed into whichever tiles land on it rather than piled onto the last one.
fn tile_bounds(dim: u32, tiles: u32) -> Vec<(u32, u32)> {
    (0..tiles).map(|i| (i * dim / tiles, (i + 1) * dim / tiles)).collect()
}

/// Clip-limited histogram equalization mapping (256 -> 256) for one tile.
fn tile_mapping(frame: &super::capture::IrFrame, x0: u32, x1: u32, y0: u32, y1: u32) -> [u8; 256] {
    let mut hist = [0u32; 256];
    let mut count = 0u32;
    for y in y0..y1 {
        let row_start = (y * frame.width) as usize;
        for x in x0..x1 {
            let val = (frame.data[row_start + x as usize] / 257) as usize;
            hist[val] += 1;
            count += 1;
        }
    }
    if count == 0 {
        let mut identity = [0u8; 256];
        for (i, m) in identity.iter_mut().enumerate() {
            *m = i as u8;
        }
        return identity;
    }

    let avg = count as f32 / 256.0;
    let clip = (CLAHE_CLIP_FACTOR * avg).max(1.0) as u32;

    let mut excess = 0u32;
    for bin in hist.iter_mut() {
        if *bin > clip {
            excess += *bin - clip;
            *bin = clip;
        }
    }
    let redistribute = excess / 256;
    let remainder = excess % 256;
    for (i, bin) in hist.iter_mut().enumerate() {
        *bin += redistribute + if (i as u32) < remainder { 1 } else { 0 };
    }

    let mut mapping = [0u8; 256];
    let mut cumulative = 0u32;
    let total = count as f32;
    for (i, &bin) in hist.iter().enumerate() {
        cumulative += bin;
        mapping[i] = ((cumulative as f32 / total) * 255.0).round() as u8;
    }
    mapping
}

/// For a pixel coordinate along one axis, the two neighboring tile-center indices to
/// interpolate between and the interpolation weight toward the second one. Coordinates at or
/// beyond the outermost tile centers clamp to that tile with zero weight (no extrapolation).
fn neighbor_weights(pos: f32, centers: &[f32]) -> (usize, usize, f32) {
    let n = centers.len();
    if n <= 1 || pos <= centers[0] {
        return (0, 0, 0.0);
    }
    if pos >= centers[n - 1] {
        return (n - 1, n - 1, 0.0);
    }
    for i in 0..n - 1 {
        if pos >= centers[i] && pos <= centers[i + 1] {
            let span = centers[i + 1] - centers[i];
            let w = if span > 0.0 { (pos - centers[i]) / span } else { 0.0 };
            return (i, i + 1, w);
        }
    }
    (n - 1, n - 1, 0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::IrFrame;

    fn frame(data: Vec<u16>) -> IrFrame {
        IrFrame { data, width: 2, height: 2 }
    }

    #[test]
    fn identical_frames_have_zero_motion() {
        let a = frame(vec![1000, 2000, 3000, 4000]);
        let b = a.clone();
        assert_eq!(frame_motion_fraction(&a, &b), 0.0);
    }

    #[test]
    fn large_change_in_one_pixel_is_detected() {
        let a = frame(vec![1000, 2000, 3000, 4000]);
        let b = frame(vec![1000, 2000, 3000, 40000]);
        assert_eq!(frame_motion_fraction(&a, &b), 0.25);
    }

    #[test]
    fn small_noise_is_ignored() {
        let a = frame(vec![1000, 2000, 3000, 4000]);
        let b = frame(vec![1050, 2050, 2950, 3950]);
        assert_eq!(frame_motion_fraction(&a, &b), 0.0);
    }

    #[test]
    fn mismatched_dimensions_return_zero() {
        let a = frame(vec![1000, 2000, 3000, 4000]);
        let b = IrFrame { data: vec![1000, 2000], width: 2, height: 1 };
        assert_eq!(frame_motion_fraction(&a, &b), 0.0);
    }
}