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

pub fn histogram_equalize(frame: &mut super::capture::IrFrame) {
    let mut hist = [0u32; 65536];
    
    for &val in &frame.data {
        hist[val as usize] += 1;
    }
    
    let total = frame.data.len() as f32;
    let mut cdf = [0f32; 65536];
    let mut sum = 0f32;
    
    for i in 0..65536 {
        sum += hist[i] as f32;
        cdf[i] = sum / total;
    }
    
    for val in &mut frame.data {
        *val = (cdf[*val as usize] * 65535.0) as u16;
    }
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