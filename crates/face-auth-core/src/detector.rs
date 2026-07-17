use image::{DynamicImage, ImageBuffer, Luma};
use tract_onnx::prelude::*;
use tract_ndarray::s;

use crate::capture::IrFrame;

pub struct FaceDetector {
    model: InferenceSimplePlan<InferenceModel>,
    threshold: f32,
}

impl FaceDetector {
    pub fn new(model_path: &str, threshold: f32) -> anyhow::Result<Self> {
        let model = onnx()
            .model_for_path(model_path)?
            .into_runnable()?;
        Ok(Self { model, threshold })
    }

    pub fn detect(&mut self, frame: &IrFrame) -> anyhow::Result<bool> {
        let input = preprocess_for_detector(frame)?;
        let mut input = input.into_dyn();
        input.insert_axis_inplace(tract_ndarray::Axis(0));
        let input_tensor = Tensor::from(input).into_tvalue();
        let result = self.model.run(tvec!(input_tensor))?;

        let scores = result[0].to_array_view::<f32>()?;
        let face_scores = scores.slice(s![0, .., 1]);
        let max_face = face_scores.iter().cloned().fold(0.0f32, f32::max);

        Ok(max_face >= self.threshold)
    }
}

pub fn raw_frame_has_content(frame: &IrFrame) -> bool {
    if frame.data.is_empty() {
        return false;
    }
    let len = frame.data.len() as f32;
    let sum: f64 = frame.data.iter().map(|&v| v as f64).sum();
    let mean = (sum / len as f64) as f32;
    let variance: f32 = frame.data.iter().map(|&v| { let d = (v as f32) - mean; d * d }).sum::<f32>() / len;
    variance > 100_000.0
}

fn preprocess_for_detector(frame: &IrFrame) -> anyhow::Result<tract_ndarray::Array3<f32>> {
    let width = frame.width as u32;
    let height = frame.height as u32;

    let img_buffer: ImageBuffer<Luma<u16>, Vec<u16>> = ImageBuffer::from_fn(width, height, |x, y| {
        let idx = (y * width + x) as usize;
        let val = frame.data.get(idx).copied().unwrap_or(0);
        Luma([val])
    });

    let dynamic_img = DynamicImage::ImageLuma16(img_buffer);
    let resized = dynamic_img.resize_exact(320, 240, image::imageops::FilterType::Lanczos3);
    let rgb = resized.to_rgb8();

    let mut array = tract_ndarray::Array3::<f32>::zeros((3, 240, 320));

    for y in 0..240usize {
        for x in 0..320usize {
            let pixel = rgb.get_pixel(x as u32, y as u32);
            let r = pixel.0[0] as f32 / 255.0;
            let g = pixel.0[1] as f32 / 255.0;
            let b = pixel.0[2] as f32 / 255.0;
            array[[0, y, x]] = r;
            array[[1, y, x]] = g;
            array[[2, y, x]] = b;
        }
    }

    Ok(array)
}


