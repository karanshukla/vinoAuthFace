use image::{DynamicImage, ImageBuffer, Luma};
use tract_onnx::prelude::*;
use tract_ndarray::s;

use crate::capture::IrFrame;

#[cfg(feature = "npu")]
use openvino::{Core as OvCore, DeviceType, ElementType, InferRequest, PartialShape, Shape, Tensor as OvTensor};

/// Face bounding box, normalized to [0, 1] relative to the detector's input
/// frame (which itself is a resize of the full raw camera frame, so these
/// fractions apply directly to the original frame's width/height too).
#[derive(Debug, Clone, Copy)]
pub struct FaceBox {
    pub x1: f32,
    pub y1: f32,
    pub x2: f32,
    pub y2: f32,
}

/// A single SSD-style anchor box, normalized to the detector's 320x240 input.
#[derive(Clone, Copy)]
struct Prior {
    cx: f32,
    cy: f32,
    w: f32,
    h: f32,
}

/// `version-slim-320.onnx` (the "Ultra-Light-Fast-Generic-Face-Detector-1MB" slim
/// export) strips out anchor decoding from the ONNX graph — its `boxes` output is raw
/// regression deltas relative to a fixed 4420-anchor grid, not finished coordinates.
/// This regenerates that exact anchor grid (4 feature-map levels over a 320x240 input,
/// strides 8/16/32/64, matching min-box sizes per level) so `decode_box` below can turn
/// a delta back into a real box the same way the reference implementation's Python
/// postprocessing does.
fn priors() -> &'static [Prior] {
    use std::sync::OnceLock;
    static PRIORS: OnceLock<Vec<Prior>> = OnceLock::new();
    PRIORS.get_or_init(|| {
        const IMAGE_W: f32 = 320.0;
        const IMAGE_H: f32 = 240.0;
        const FEATURE_W: [usize; 4] = [40, 20, 10, 5];
        const FEATURE_H: [usize; 4] = [30, 15, 8, 4];
        const MIN_BOXES: [&[f32]; 4] = [
            &[10.0, 16.0, 24.0],
            &[32.0, 48.0],
            &[64.0, 96.0],
            &[128.0, 192.0, 256.0],
        ];

        let mut priors = Vec::with_capacity(4420);
        for level in 0..4 {
            for j in 0..FEATURE_H[level] {
                for i in 0..FEATURE_W[level] {
                    let cx = (i as f32 + 0.5) / FEATURE_W[level] as f32;
                    let cy = (j as f32 + 0.5) / FEATURE_H[level] as f32;
                    for &min_box in MIN_BOXES[level] {
                        priors.push(Prior { cx, cy, w: min_box / IMAGE_W, h: min_box / IMAGE_H });
                    }
                }
            }
        }
        priors
    })
}

const CENTER_VARIANCE: f32 = 0.1;
const SIZE_VARIANCE: f32 = 0.2;

/// Turns a raw `[dx, dy, dw, dh]` regression delta for one anchor into an actual
/// normalized `FaceBox`, per the standard SSD-prior decode formula.
fn decode_box(prior: &Prior, delta: &[f32]) -> FaceBox {
    let cx = delta[0] * CENTER_VARIANCE * prior.w + prior.cx;
    let cy = delta[1] * CENTER_VARIANCE * prior.h + prior.cy;
    let w = (delta[2] * SIZE_VARIANCE).exp() * prior.w;
    let h = (delta[3] * SIZE_VARIANCE).exp() * prior.h;
    FaceBox { x1: cx - w / 2.0, y1: cy - h / 2.0, x2: cx + w / 2.0, y2: cy + h / 2.0 }
}

pub enum FaceDetector {
    Tract {
        model: InferenceSimplePlan<InferenceModel>,
        threshold: f32,
    },
    #[cfg(feature = "npu")]
    OpenVino {
        request: InferRequest,
        input_name: String,
        scores_name: String,
        boxes_name: String,
        threshold: f32,
    },
}

impl FaceDetector {
    pub fn new(model_path: &str, threshold: f32, backend: &str, device: &str) -> anyhow::Result<Self> {
        match backend {
            "openvino" => Self::new_openvino(model_path, threshold, device),
            _ => Self::new_tract(model_path, threshold),
        }
    }

    fn new_tract(model_path: &str, threshold: f32) -> anyhow::Result<Self> {
        tracing::debug!(model_path, "FaceDetector: using tract (CPU) backend");
        let model = onnx()
            .model_for_path(model_path)?
            .into_runnable()?;
        Ok(Self::Tract { model, threshold })
    }

    #[cfg(feature = "npu")]
    fn new_openvino(model_path: &str, threshold: f32, device: &str) -> anyhow::Result<Self> {
        tracing::debug!(model_path, device, "FaceDetector: using openvino backend, compiling model");
        let t0 = std::time::Instant::now();
        let mut core = OvCore::new()?;
        let mut model = core.read_model_from_file(model_path, "")?;
        let input_name = model.get_input_by_index(0)?.get_name()?;
        let scores_name = model.get_output_by_index(0)?.get_name()?;
        let boxes_name = model.get_output_by_index(1)?.get_name()?;
        model.reshape_single_input(&PartialShape::new_static(4, &[1, 3, 240, 320])?)?;
        let mut compiled = core.compile_model(&model, DeviceType::from(device))?;
        let request = compiled.create_infer_request()?;
        tracing::debug!(device, compile_time_ms = t0.elapsed().as_millis(), "FaceDetector: openvino model compiled");
        Ok(Self::OpenVino { request, input_name, scores_name, boxes_name, threshold })
    }

    #[cfg(not(feature = "npu"))]
    fn new_openvino(_model_path: &str, _threshold: f32, _device: &str) -> anyhow::Result<Self> {
        anyhow::bail!(
            "backend = \"openvino\" requested but this build of face-auth was compiled without the `npu` feature"
        )
    }

    pub fn detect(&mut self, frame: &IrFrame) -> anyhow::Result<Option<FaceBox>> {
        let input = preprocess_for_detector(frame)?;

        match self {
            Self::Tract { model, threshold } => {
                let mut input = input.into_dyn();
                input.insert_axis_inplace(tract_ndarray::Axis(0));
                let input_tensor = Tensor::from(input).into_tvalue();
                let result = model.run(tvec!(input_tensor))?;

                let scores = result[0].to_array_view::<f32>()?;
                let face_scores = scores.slice(s![0, .., 1]);
                let (best_idx, max_face) = face_scores
                    .iter()
                    .cloned()
                    .enumerate()
                    .fold((0usize, 0.0f32), |acc, (i, v)| if v > acc.1 { (i, v) } else { acc });

                tracing::debug!(max_face, threshold = *threshold, "FaceDetector (tract): max face score");

                if max_face < *threshold {
                    return Ok(None);
                }

                let boxes_view = result[1].to_array_view::<f32>()?;
                let boxes_flat = boxes_view
                    .as_slice()
                    .ok_or_else(|| anyhow::anyhow!("non-contiguous detector boxes output"))?;
                let b = &boxes_flat[best_idx * 4..best_idx * 4 + 4];
                let face_box = decode_box(&priors()[best_idx], b);
                tracing::debug!(?face_box, "FaceDetector (tract): face box");
                Ok(Some(face_box))
            }
            #[cfg(feature = "npu")]
            Self::OpenVino { request, input_name, scores_name, boxes_name, threshold } => {
                let standard = input.as_standard_layout();
                let data = standard
                    .as_slice()
                    .ok_or_else(|| anyhow::anyhow!("non-contiguous detector input"))?;
                let mut tensor = OvTensor::new(ElementType::F32, &Shape::new(&[1, 3, 240, 320])?)?;
                tensor.get_data_mut::<f32>()?.copy_from_slice(data);
                request.set_tensor(input_name, &tensor)?;
                request.infer()?;
                let scores_output = request.get_tensor(scores_name)?;
                let scores = scores_output.get_data::<f32>()?;

                // scores is [1, 4420, 2] row-major; face-class probability is index 1 of each pair.
                let (best_idx, max_face) = scores
                    .chunks_exact(2)
                    .map(|pair| pair[1])
                    .enumerate()
                    .fold((0usize, 0.0f32), |acc, (i, v)| if v > acc.1 { (i, v) } else { acc });

                tracing::debug!(max_face, threshold = *threshold, "FaceDetector (openvino): max face score");

                if max_face < *threshold {
                    return Ok(None);
                }

                let boxes_output = request.get_tensor(boxes_name)?;
                let boxes = boxes_output.get_data::<f32>()?;
                let b = &boxes[best_idx * 4..best_idx * 4 + 4];
                let face_box = decode_box(&priors()[best_idx], b);
                tracing::debug!(?face_box, "FaceDetector (openvino): face box");
                Ok(Some(face_box))
            }
        }
    }
}

pub const CONTENT_VARIANCE_THRESHOLD: f32 = 100_000.0;

pub fn frame_mean(frame: &IrFrame) -> f32 {
    if frame.data.is_empty() {
        return 0.0;
    }
    let len = frame.data.len() as f64;
    let sum: f64 = frame.data.iter().map(|&v| v as f64).sum();
    (sum / len) as f32
}

pub fn frame_variance(frame: &IrFrame) -> f32 {
    if frame.data.is_empty() {
        return 0.0;
    }
    let len = frame.data.len() as f32;
    let mean = frame_mean(frame);
    frame.data.iter().map(|&v| { let d = (v as f32) - mean; d * d }).sum::<f32>() / len
}

pub fn raw_frame_has_content(frame: &IrFrame) -> bool {
    !frame.data.is_empty() && frame_variance(frame) > CONTENT_VARIANCE_THRESHOLD
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
            let r = (pixel.0[0] as f32 - 127.0) / 128.0;
            let g = (pixel.0[1] as f32 - 127.0) / 128.0;
            let b = (pixel.0[2] as f32 - 127.0) / 128.0;
            array[[0, y, x]] = r;
            array[[1, y, x]] = g;
            array[[2, y, x]] = b;
        }
    }

    Ok(array)
}

#[cfg(test)]
mod prior_tests {
    use super::*;

    #[test]
    fn zero_delta_reproduces_the_anchor_itself() {
        let p = priors()[0];
        let b = decode_box(&p, &[0.0, 0.0, 0.0, 0.0]);
        assert!((b.x1 - (p.cx - p.w / 2.0)).abs() < 1e-6);
        assert!((b.x2 - (p.cx + p.w / 2.0)).abs() < 1e-6);
    }

    #[test]
    fn prior_count_is_4420() {
        assert_eq!(priors().len(), 4420);
    }

    #[test]
    fn decoded_box_for_small_delta_stays_well_ordered() {
        let p = priors()[2000];
        let b = decode_box(&p, &[0.1, -0.1, 0.05, -0.05]);
        assert!(b.x1 < b.x2);
        assert!(b.y1 < b.y2);
    }
}
