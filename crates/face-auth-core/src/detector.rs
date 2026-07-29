use image::{DynamicImage, ImageBuffer, Luma};
use tract_onnx::prelude::*;
use tract_ndarray::s;

use crate::capture::IrFrame;

#[cfg(feature = "npu")]
use openvino::{Core as OvCore, DeviceType, ElementType, InferRequest, PartialShape, Shape, Tensor as OvTensor};

pub enum FaceDetector {
    Tract {
        model: InferenceSimplePlan<InferenceModel>,
        threshold: f32,
    },
    #[cfg(feature = "npu")]
    OpenVino {
        request: InferRequest,
        input_name: String,
        output_name: String,
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
        let output_name = model.get_output_by_index(0)?.get_name()?;
        model.reshape_single_input(&PartialShape::new_static(4, &[1, 3, 240, 320])?)?;
        let mut compiled = core.compile_model(&model, DeviceType::from(device))?;
        let request = compiled.create_infer_request()?;
        tracing::debug!(device, compile_time_ms = t0.elapsed().as_millis(), "FaceDetector: openvino model compiled");
        Ok(Self::OpenVino { request, input_name, output_name, threshold })
    }

    #[cfg(not(feature = "npu"))]
    fn new_openvino(_model_path: &str, _threshold: f32, _device: &str) -> anyhow::Result<Self> {
        anyhow::bail!(
            "backend = \"openvino\" requested but this build of face-auth was compiled without the `npu` feature"
        )
    }

    pub fn detect(&mut self, frame: &IrFrame) -> anyhow::Result<bool> {
        let input = preprocess_for_detector(frame)?;

        match self {
            Self::Tract { model, threshold } => {
                let mut input = input.into_dyn();
                input.insert_axis_inplace(tract_ndarray::Axis(0));
                let input_tensor = Tensor::from(input).into_tvalue();
                let result = model.run(tvec!(input_tensor))?;

                let scores = result[0].to_array_view::<f32>()?;
                let face_scores = scores.slice(s![0, .., 1]);
                let max_face = face_scores.iter().cloned().fold(0.0f32, f32::max);

                tracing::debug!(max_face, threshold = *threshold, "FaceDetector (tract): max face score");
                Ok(max_face >= *threshold)
            }
            #[cfg(feature = "npu")]
            Self::OpenVino { request, input_name, output_name, threshold } => {
                let standard = input.as_standard_layout();
                let data = standard
                    .as_slice()
                    .ok_or_else(|| anyhow::anyhow!("non-contiguous detector input"))?;
                let mut tensor = OvTensor::new(ElementType::F32, &Shape::new(&[1, 3, 240, 320])?)?;
                tensor.get_data_mut::<f32>()?.copy_from_slice(data);
                request.set_tensor(input_name, &tensor)?;
                request.infer()?;
                let output = request.get_tensor(output_name)?;
                let scores = output.get_data::<f32>()?;

                // scores is [1, 4420, 2] row-major; face-class probability is index 1 of each pair.
                let max_face = scores
                    .chunks_exact(2)
                    .map(|pair| pair[1])
                    .fold(0.0f32, f32::max);

                tracing::debug!(max_face, threshold = *threshold, "FaceDetector (openvino): max face score");

                Ok(max_face >= *threshold)
            }
        }
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
