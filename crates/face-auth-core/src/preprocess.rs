use image::{DynamicImage, ImageBuffer, Luma};
use tract_onnx::prelude::tract_ndarray::Array3;

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