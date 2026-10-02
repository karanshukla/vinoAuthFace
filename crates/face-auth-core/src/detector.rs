use std::sync::Arc;
use image::DynamicImage;
use tract_onnx::prelude::*;
use tract_onnx::tract_hir::infer::Factoid;

use crate::capture::IrFrame;

// Frame-quality gates in 8-bit-equivalent units, comparable to what
// `cargo run --example frame-stats` prints. The mean gate must carry the
// dark-frame rejection: dark strobe frames have enough variance to pass a
// variance-only gate. Pinned by the `*_minimum_mean` and `*_minimum_variance` tests.
//
// Measured on the reference ASUS IR sensor:
//   illuminated frames   mean 48-96,  variance  82-317
//   dark (strobe off)    mean 1.8-8,  variance 3.4-39
const MIN_FRAME_MEAN_8BIT: f64 = 12.0;
const MIN_FRAME_VARIANCE_8BIT: f64 = 20.0;

/// Samples are u16 but carry 8-bit data widened by 257 (see `capture_frame`).
const U16_PER_8BIT: f64 = 257.0;

/// Why a frame was rejected before inference.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FrameQuality {
    Ok,
    Empty,
    /// Almost certainly an unlit frame from a strobing IR illuminator.
    TooDark { mean_8bit: f64 },
    /// Uniform field — a covered lens, or a wall.
    TooFlat { variance_8bit: f64 },
}

impl std::fmt::Display for FrameQuality {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FrameQuality::Ok => write!(f, "ok"),
            FrameQuality::Empty => write!(f, "empty frame"),
            FrameQuality::TooDark { mean_8bit } => write!(
                f,
                "too dark (mean {mean_8bit:.1}/255, need {MIN_FRAME_MEAN_8BIT}) \
                 — unlit frame, or the IR illuminator is not firing"
            ),
            FrameQuality::TooFlat { variance_8bit } => write!(
                f,
                "too flat (variance {variance_8bit:.1}, need {MIN_FRAME_VARIANCE_8BIT}) \
                 — lens covered, or nothing in view"
            ),
        }
    }
}

const DETECTOR_WIDTH: usize = 320;
const DETECTOR_HEIGHT: usize = 240;

/// Face bounding box, normalised to [0, 1] relative to the detector's input.
/// That input is a resize of the whole frame, so the fractions apply directly
/// to the original frame's width and height too.
#[derive(Debug, Clone, Copy)]
pub struct FaceBox {
    pub x1: f32,
    pub y1: f32,
    pub x2: f32,
    pub y2: f32,
    /// Eyes, nose tip and mouth corners, normalised the same way, from a
    /// landmark detector (SCRFD). `None` from `version-slim-320`, which only
    /// gives a box; the face is then cropped rather than aligned.
    pub landmarks: Option<[(f32, f32); 5]>,
}

impl FaceBox {
    /// The larger of the box's width and height, as a fraction of the frame.
    /// Using the max keeps a turned head, with its narrower box, from being
    /// mistaken for a distant one.
    pub fn size_ratio(&self) -> f32 {
        (self.x2 - self.x1).max(self.y2 - self.y1)
    }

    pub fn is_smaller_than(&self, min_ratio: f32) -> bool {
        self.size_ratio() < min_ratio
    }
}

/// One SSD-style anchor, normalised to the detector's 320x240 input.
#[derive(Clone, Copy)]
struct Prior {
    cx: f32,
    cy: f32,
    w: f32,
    h: f32,
}

/// `version-slim-320.onnx` strips anchor decoding out of the graph: its
/// `boxes` output is raw regression deltas against a fixed 4420-anchor grid,
/// not finished coordinates. This regenerates that grid (4 feature-map levels,
/// strides 8/16/32/64, matching min-box sizes per level) the same way the
/// reference implementation's Python postprocessing does.
fn priors() -> &'static [Prior] {
    use std::sync::OnceLock;
    static PRIORS: OnceLock<Vec<Prior>> = OnceLock::new();
    PRIORS.get_or_init(|| {
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
                        priors.push(Prior {
                            cx,
                            cy,
                            w: min_box / DETECTOR_WIDTH as f32,
                            h: min_box / DETECTOR_HEIGHT as f32,
                        });
                    }
                }
            }
        }
        priors
    })
}

const CENTER_VARIANCE: f32 = 0.1;
const SIZE_VARIANCE: f32 = 0.2;

/// Standard SSD decode of one anchor's `[dx, dy, dw, dh]` delta.
fn decode_box(prior: &Prior, delta: &[f32]) -> FaceBox {
    let cx = delta[0] * CENTER_VARIANCE * prior.w + prior.cx;
    let cy = delta[1] * CENTER_VARIANCE * prior.h + prior.cy;
    let w = (delta[2] * SIZE_VARIANCE).exp() * prior.w;
    let h = (delta[3] * SIZE_VARIANCE).exp() * prior.h;
    FaceBox { x1: cx - w / 2.0, y1: cy - h / 2.0, x2: cx + w / 2.0, y2: cy + h / 2.0, landmarks: None }
}

/// Pick the highest-scoring anchor and decode its box, or `None` if nothing
/// clears `threshold`. `scores` is `[anchors * 2]` row-major with the face
/// probability in column 1; `boxes` is `[anchors * 4]`.
fn best_face(scores: &[f32], boxes: &[f32], threshold: f32) -> anyhow::Result<Option<FaceBox>> {
    let anchors = priors().len();
    anyhow::ensure!(
        scores.len() == anchors * 2 && boxes.len() == anchors * 4,
        "detector returned {} scores / {} box values, expected {} anchors",
        scores.len(),
        boxes.len(),
        anchors
    );

    let (best, max_face) = scores
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| pair[1])
        .enumerate()
        .filter(|(_, v)| v.is_finite())
        .fold((0usize, f32::NEG_INFINITY), |acc, (i, v)| if v > acc.1 { (i, v) } else { acc });

    tracing::debug!(max_face, threshold, "detector: best face score");
    if max_face < threshold {
        return Ok(None);
    }

    let face_box = decode_box(&priors()[best], &boxes[best * 4..best * 4 + 4]);
    tracing::debug!(?face_box, "detector: face box");
    Ok(Some(face_box))
}

/// Work around tract 0.21's `Resize` when it is given sizes and an *empty*
/// scales input (legal ONNX, and how SCRFD's feature pyramid upsamples to the
/// next level's size): the output shape comes out right, but `eval` walks the
/// empty scales and never resizes, so the ×2 upsample is a no-op and the
/// optimised plan reads past the smaller tensor (garbage boxes, varying from
/// run to run). The pyramid levels are exactly 2× apart for any input that is
/// a multiple of 32, so the scales are made explicit. Returns how many nodes
/// were patched.
fn fix_empty_resize_scales(model: &mut InferenceModel) -> anyhow::Result<usize> {
    const SCALES_SLOT: usize = 2;
    let targets: Vec<usize> = model
        .nodes()
        .iter()
        .filter(|n| n.op.name() == "Resize" && n.inputs.len() > SCALES_SLOT)
        .filter(|n| {
            model
                .outlet_fact(n.inputs[SCALES_SLOT])
                .ok()
                .and_then(|f| f.value.concretize())
                .is_some_and(|t| t.len() == 0)
        })
        .map(|n| n.id)
        .collect();
    if targets.is_empty() {
        return Ok(0);
    }
    let scales = model.add_const("vinoauthface.upsample_scales", tensor1(&[1f32, 1.0, 2.0, 2.0]))?;
    for &node in &targets {
        model.add_edge(scales, InletId::new(node, SCALES_SLOT))?;
    }
    Ok(targets.len())
}

#[cfg(feature = "npu")]
use openvino::{Core as OvCore, DeviceType, ElementType, InferRequest, PartialShape, Shape, Tensor as OvTensor};

/// Which detector a model file is, told apart by its outputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetectorKind {
    /// `version-slim-320.onnx`: scores and boxes, 320x240 input.
    Slim,
    /// SCRFD (`det_500m.onnx`): scores, boxes and five landmarks at three
    /// strides, square input. See `scrfd`.
    Scrfd,
}

impl DetectorKind {
    fn from_outputs(n: usize) -> anyhow::Result<Self> {
        match n {
            2 => Ok(Self::Slim),
            crate::scrfd::OUTPUTS => Ok(Self::Scrfd),
            _ => anyhow::bail!("unrecognised face detector: {n} outputs (expected 2 or {})", crate::scrfd::OUTPUTS),
        }
    }

    /// `[height, width]` of the model input.
    fn input_hw(self) -> [usize; 2] {
        match self {
            Self::Slim => [DETECTOR_HEIGHT, DETECTOR_WIDTH],
            Self::Scrfd => [crate::scrfd::INPUT_SIZE; 2],
        }
    }
}

#[cfg_attr(feature = "npu", allow(clippy::large_enum_variant))]
pub enum FaceDetector {
    Tract {
        model: Arc<TypedRunnableModel>,
        kind: DetectorKind,
        threshold: f32,
    },
    #[cfg(feature = "npu")]
    OpenVino {
        request: InferRequest,
        input_name: String,
        output_names: Vec<String>,
        kind: DetectorKind,
        threshold: f32,
    },
}

impl FaceDetector {
    /// `backend` is "tract" (pure-Rust CPU) or "openvino", which needs a build
    /// with the `npu` feature and runs on `device` ("NPU", "GPU" or "CPU").
    pub fn new(model_path: &str, threshold: f32, backend: &str, device: &str) -> anyhow::Result<Self> {
        if !std::path::Path::new(model_path).exists() {
            anyhow::bail!("face detector model not found at {model_path}");
        }
        match backend {
            "openvino" => Self::new_openvino(model_path, threshold, device),
            _ => Self::new_tract(model_path, threshold),
        }
    }

    fn new_tract(model_path: &str, threshold: f32) -> anyhow::Result<Self> {
        let mut model = onnx().model_for_path(model_path)?;
        let kind = DetectorKind::from_outputs(model.output_outlets()?.len())?;
        if kind == DetectorKind::Scrfd {
            let patched = fix_empty_resize_scales(&mut model)?;
            tracing::debug!(patched, "SCRFD: upsample scales made explicit for tract");
        }
        let [h, w] = kind.input_hw();
        // The input fact is required before optimisation: without a concrete
        // shape the graph stays symbolic and tract cannot lower it.
        let model = model
            .with_input_fact(0, InferenceFact::dt_shape(f32::datum_type(), tvec!(1, 3, h, w)))?
            .into_optimized()?
            .into_runnable()?;
        Ok(Self::Tract { model, kind, threshold })
    }

    #[cfg(feature = "npu")]
    fn new_openvino(model_path: &str, threshold: f32, device: &str) -> anyhow::Result<Self> {
        let t0 = std::time::Instant::now();
        let mut core = OvCore::new()?;
        let mut model = core.read_model_from_file(model_path, "")?;
        let input_name = model.get_input_by_index(0)?.get_name()?;
        let kind = DetectorKind::from_outputs(model.get_outputs_len()?)?;
        let output_names = (0..model.get_outputs_len()?)
            .map(|i| Ok(model.get_output_by_index(i)?.get_name()?))
            .collect::<anyhow::Result<Vec<String>>>()?;
        let [h, w] = kind.input_hw();
        model.reshape_single_input(&PartialShape::new_static(4, &[1, 3, h as i64, w as i64])?)?;
        let mut compiled = core.compile_model(&model, DeviceType::from(device))?;
        let request = compiled.create_infer_request()?;
        tracing::debug!(device, ?kind, compile_ms = t0.elapsed().as_millis() as u64, "detector compiled (openvino)");
        Ok(Self::OpenVino { request, input_name, output_names, kind, threshold })
    }

    #[cfg(not(feature = "npu"))]
    fn new_openvino(_model_path: &str, _threshold: f32, _device: &str) -> anyhow::Result<Self> {
        anyhow::bail!("backend = \"openvino\" but this build was compiled without the `npu` feature")
    }

    /// Which detector this is. SCRFD faces carry landmarks and are aligned
    /// before encoding, which changes what an embedding means.
    pub fn kind(&self) -> DetectorKind {
        match self {
            Self::Tract { kind, .. } => *kind,
            #[cfg(feature = "npu")]
            Self::OpenVino { kind, .. } => *kind,
        }
    }

    pub fn detect(&mut self, frame: &IrFrame) -> anyhow::Result<Option<FaceBox>> {
        let kind = self.kind();
        let (input, letterbox) = match kind {
            DetectorKind::Slim => (preprocess_for_detector(frame)?, None),
            DetectorKind::Scrfd => {
                let (input, lb) = crate::scrfd::preprocess(frame)?;
                (input, Some(lb))
            }
        };
        let (outputs, threshold) = self.run(input)?;
        match letterbox {
            None => {
                anyhow::ensure!(outputs.len() >= 2, "detector returned {} outputs, expected scores and boxes", outputs.len());
                best_face(&outputs[0], &outputs[1], threshold)
            }
            Some(lb) => crate::scrfd::best_face(&outputs, &lb, frame, threshold),
        }
    }

    /// Run the model; every output as a contiguous copy (the optimised plan
    /// may hand back strided views).
    fn run(&mut self, input: tract_ndarray::Array3<f32>) -> anyhow::Result<(Vec<Vec<f32>>, f32)> {
        match self {
            Self::Tract { model, threshold, .. } => {
                let mut input = input.into_dyn();
                input.insert_axis_inplace(tract_ndarray::Axis(0));
                let result = model.run(tvec!(Tensor::from(input).into_tvalue()))?;
                let outputs = result
                    .iter()
                    .map(|t| Ok(t.to_plain_array_view::<f32>()?.iter().copied().collect()))
                    .collect::<anyhow::Result<_>>()?;
                Ok((outputs, *threshold))
            }
            #[cfg(feature = "npu")]
            Self::OpenVino { request, input_name, output_names, kind, threshold } => {
                let standard = input.as_standard_layout();
                let data = standard
                    .as_slice()
                    .ok_or_else(|| anyhow::anyhow!("non-contiguous detector input"))?;
                let [h, w] = kind.input_hw();
                let shape = Shape::new(&[1, 3, h as i64, w as i64])?;
                let mut tensor = OvTensor::new(ElementType::F32, &shape)?;
                tensor.get_data_mut::<f32>()?.copy_from_slice(data);
                request.set_tensor(input_name, &tensor)?;
                request.infer()?;
                let outputs = output_names
                    .iter()
                    .map(|name| Ok(request.get_tensor(name)?.get_data::<f32>()?.to_vec()))
                    .collect::<anyhow::Result<_>>()?;
                Ok((outputs, *threshold))
            }
        }
    }
}

/// Cheap "is this frame worth running inference on" check.
///
/// Accumulates in f64: a 640x400 frame sums ~256k terms of up to 4.3e9, well
/// past what f32's ~7 significant digits can carry.
pub fn assess_frame(frame: &IrFrame) -> FrameQuality {
    if frame.data.is_empty() {
        return FrameQuality::Empty;
    }
    let len = frame.data.len() as f64;
    let mean = frame.data.iter().map(|&v| v as f64).sum::<f64>() / len;
    let variance: f64 = frame
        .data
        .iter()
        .map(|&v| {
            let d = v as f64 - mean;
            d * d
        })
        .sum::<f64>()
        / len;

    let mean_8bit = mean / U16_PER_8BIT;
    let variance_8bit = variance / (U16_PER_8BIT * U16_PER_8BIT);

    if mean_8bit < MIN_FRAME_MEAN_8BIT {
        return FrameQuality::TooDark { mean_8bit };
    }
    if variance_8bit < MIN_FRAME_VARIANCE_8BIT {
        return FrameQuality::TooFlat { variance_8bit };
    }
    FrameQuality::Ok
}

pub fn raw_frame_has_content(frame: &IrFrame) -> bool {
    assess_frame(frame) == FrameQuality::Ok
}

fn preprocess_for_detector(frame: &IrFrame) -> anyhow::Result<tract_ndarray::Array3<f32>> {
    let img_buffer = crate::preprocess::frame_to_luma16(frame)?;

    let dynamic_img = DynamicImage::ImageLuma16(img_buffer);
    let resized = dynamic_img.resize_exact(
        DETECTOR_WIDTH as u32,
        DETECTOR_HEIGHT as u32,
        image::imageops::FilterType::Lanczos3,
    );
    let rgb = resized.to_rgb8();

    let mut array =
        tract_ndarray::Array3::<f32>::zeros((3, DETECTOR_HEIGHT, DETECTOR_WIDTH));

    // (px - 127) / 128 is what the model was trained on (the reference
    // implementation's image_mean/image_std). Plain [0, 1] scaling shifts every
    // input and skews both the scores and the regressed boxes.
    for y in 0..DETECTOR_HEIGHT {
        for x in 0..DETECTOR_WIDTH {
            let pixel = rgb.get_pixel(x as u32, y as u32);
            for c in 0..3 {
                array[[c, y, x]] = (pixel.0[c] as f32 - 127.0) / 128.0;
            }
        }
    }

    Ok(array)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a frame from 8-bit values, widened the way `capture_frame` does.
    fn frame8(values: Vec<u8>, width: u32, height: u32) -> IrFrame {
        IrFrame {
            data: values.into_iter().map(|v| v as u16 * 257).collect(),
            width,
            height,
        }
    }

    #[test]
    fn empty_frame_is_rejected() {
        assert_eq!(assess_frame(&frame8(vec![], 0, 0)), FrameQuality::Empty);
    }

    #[test]
    fn covered_lens_is_too_flat() {
        // Uniform mid-grey: bright enough, but no structure at all.
        let q = assess_frame(&frame8(vec![128; 1024], 32, 32));
        assert!(matches!(q, FrameQuality::TooFlat { .. }), "got {q:?}");
    }

    #[test]
    fn unlit_strobe_frame_is_too_dark() {
        // Values 0..=16: mean 8.0, variance 24. Dark, yet enough variance to
        // pass a variance-only gate.
        let data: Vec<u8> = (0..1024).map(|i| (i % 17) as u8).collect();
        let q = assess_frame(&frame8(data, 32, 32));
        assert!(matches!(q, FrameQuality::TooDark { .. }), "got {q:?}");
    }

    /// Alternates `mean - spread` and `mean + spread`: exact mean, variance `spread^2`.
    fn two_level_frame(mean: u8, spread: u8) -> IrFrame {
        frame8((0..1024).map(|i| if i % 2 == 0 { mean - spread } else { mean + spread }).collect(), 32, 32)
    }

    #[test]
    fn accepts_frame_at_minimum_mean() {
        assert_eq!(assess_frame(&two_level_frame(12, 5)), FrameQuality::Ok);
    }

    #[test]
    fn rejects_frame_below_minimum_mean() {
        let q = assess_frame(&two_level_frame(11, 5));
        assert!(matches!(q, FrameQuality::TooDark { .. }), "got {q:?}");
    }

    #[test]
    fn accepts_frame_above_minimum_variance() {
        assert_eq!(assess_frame(&two_level_frame(100, 5)), FrameQuality::Ok);
    }

    #[test]
    fn rejects_frame_below_minimum_variance() {
        let q = assess_frame(&two_level_frame(100, 4));
        assert!(matches!(q, FrameQuality::TooFlat { .. }), "got {q:?}");
    }

    #[test]
    fn illuminated_frame_is_accepted() {
        // Mean ~64, plenty of structure — like a real lit frame.
        let data: Vec<u8> = (0..1024).map(|i| (i % 128) as u8).collect();
        assert_eq!(assess_frame(&frame8(data, 32, 32)), FrameQuality::Ok);
        assert!(raw_frame_has_content(&frame8(
            (0..1024).map(|i| (i % 128) as u8).collect(),
            32,
            32
        )));
    }

    #[test]
    fn face_size_gate_uses_the_larger_side() {
        let tall = FaceBox { x1: 0.4, y1: 0.2, x2: 0.5, y2: 0.7, landmarks: None };
        assert!((tall.size_ratio() - 0.5).abs() < 1e-6);
        assert!(!tall.is_smaller_than(0.5));
        assert!(tall.is_smaller_than(0.51));

        let wide = FaceBox { x1: 0.1, y1: 0.4, x2: 0.4, y2: 0.5, landmarks: None };
        assert!(wide.is_smaller_than(0.4));
        assert!(!wide.is_smaller_than(0.25));
    }

    #[test]
    fn zero_min_face_size_disables_the_gate() {
        let dot = FaceBox { x1: 0.5, y1: 0.5, x2: 0.5, y2: 0.5, landmarks: None };
        assert!(!dot.is_smaller_than(0.0));
    }

    #[test]
    fn prior_grid_matches_the_model() {
        assert_eq!(priors().len(), 4420);
    }

    #[test]
    fn zero_delta_reproduces_the_anchor_itself() {
        let p = priors()[0];
        let b = decode_box(&p, &[0.0, 0.0, 0.0, 0.0]);
        assert!((b.x1 - (p.cx - p.w / 2.0)).abs() < 1e-6);
        assert!((b.x2 - (p.cx + p.w / 2.0)).abs() < 1e-6);
    }

    #[test]
    fn decoded_box_for_small_delta_stays_well_ordered() {
        let b = decode_box(&priors()[2000], &[0.1, -0.1, 0.05, -0.05]);
        assert!(b.x1 < b.x2);
        assert!(b.y1 < b.y2);
    }

    #[test]
    fn best_face_picks_the_top_anchor_and_respects_threshold() {
        let n = priors().len();
        let mut scores = vec![0.0f32; n * 2];
        let boxes = vec![0.0f32; n * 4];
        scores[2 * 1234 + 1] = 0.9;
        scores[2 * 99 + 1] = f32::NAN;

        let b = best_face(&scores, &boxes, 0.5).unwrap().expect("face");
        let p = priors()[1234];
        assert!((b.x1 - (p.cx - p.w / 2.0)).abs() < 1e-6);

        assert!(best_face(&scores, &boxes, 0.95).unwrap().is_none());
    }

    #[test]
    fn best_face_rejects_mismatched_output_shapes() {
        assert!(best_face(&[0.0; 10], &[0.0; 20], 0.5).is_err());
    }

    #[test]
    fn brightness_alone_does_not_pass() {
        let q = assess_frame(&frame8(vec![200; 1024], 32, 32));
        assert!(matches!(q, FrameQuality::TooFlat { .. }), "got {q:?}");
    }
}
