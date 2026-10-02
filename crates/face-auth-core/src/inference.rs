use std::sync::Arc;
use tract_onnx::prelude::*;

#[cfg(feature = "npu")]
use openvino::{Core as OvCore, DeviceType, ElementType, InferRequest, PartialShape, Shape, Tensor as OvTensor};

#[cfg(feature = "npu")]
const ENCODER_SIZE: i64 = 112;

// One encoder per process, so the size gap between variants costs nothing.
#[cfg_attr(feature = "npu", allow(clippy::large_enum_variant))]
pub enum FaceEncoder {
    Tract(Arc<TypedRunnableModel>),
    #[cfg(feature = "npu")]
    OpenVino {
        request: InferRequest,
        input_name: String,
        output_name: String,
    },
}

impl FaceEncoder {
    /// `backend` is "tract" (pure-Rust CPU) or "openvino", which needs a build
    /// with the `npu` feature and runs on `device` ("NPU", "GPU" or "CPU").
    pub fn new(model_path: &str, backend: &str, device: &str) -> anyhow::Result<Self> {
        // Check first: tract's own error for a missing file is opaque, and
        // this is the most common misconfiguration.
        if !std::path::Path::new(model_path).exists() {
            anyhow::bail!("face encoder model not found at {model_path}");
        }
        match backend {
            "openvino" => Self::new_openvino(model_path, device),
            _ => Self::new_tract(model_path),
        }
    }

    fn new_tract(model_path: &str) -> anyhow::Result<Self> {
        // into_optimized() runs tract's fusion and lowering passes. Without it
        // the graph executes op-by-op as written: this model took ~400ms per
        // frame unoptimized, which dominated unlock latency.
        let model = onnx()
            .model_for_path(model_path)?
            .with_input_fact(
                0,
                InferenceFact::dt_shape(f32::datum_type(), tvec!(1, 3, 112, 112)),
            )?
            .into_optimized()?
            .into_runnable()?;
        Ok(Self::Tract(model))
    }

    #[cfg(feature = "npu")]
    fn new_openvino(model_path: &str, device: &str) -> anyhow::Result<Self> {
        let t0 = std::time::Instant::now();
        let mut core = OvCore::new()?;
        let mut model = core.read_model_from_file(model_path, "")?;
        let input_name = model.get_input_by_index(0)?.get_name()?;
        let output_name = model.get_output_by_index(0)?.get_name()?;
        model.reshape_single_input(&PartialShape::new_static(4, &[1, 3, ENCODER_SIZE, ENCODER_SIZE])?)?;
        let mut compiled = core.compile_model(&model, DeviceType::from(device))?;
        let request = compiled.create_infer_request()?;
        tracing::debug!(device, compile_ms = t0.elapsed().as_millis() as u64, "encoder compiled (openvino)");
        Ok(Self::OpenVino { request, input_name, output_name })
    }

    #[cfg(not(feature = "npu"))]
    fn new_openvino(_model_path: &str, _device: &str) -> anyhow::Result<Self> {
        anyhow::bail!("backend = \"openvino\" but this build was compiled without the `npu` feature")
    }

    /// Run the encoder and return an L2-normalised embedding.
    pub fn encode(&mut self, input: tract_ndarray::ArrayView3<f32>) -> anyhow::Result<Vec<f32>> {
        let embedding: Vec<f32> = match self {
            Self::Tract(model) => {
                let mut input = input.to_owned().into_dyn();
                input.insert_axis_inplace(tract_ndarray::Axis(0));
                let result = model.run(tvec!(Tensor::from(input).into_tvalue()))?;
                result[0].to_plain_array_view::<f32>()?.iter().copied().collect()
            }
            #[cfg(feature = "npu")]
            Self::OpenVino { request, input_name, output_name } => {
                let owned = input.as_standard_layout().to_owned();
                let data = owned
                    .as_slice()
                    .ok_or_else(|| anyhow::anyhow!("non-contiguous encoder input"))?;
                let shape = Shape::new(&[1, 3, ENCODER_SIZE, ENCODER_SIZE])?;
                let mut tensor = OvTensor::new(ElementType::F32, &shape)?;
                tensor.get_data_mut::<f32>()?.copy_from_slice(data);
                request.set_tensor(input_name, &tensor)?;
                request.infer()?;
                request.get_tensor(output_name)?.get_data::<f32>()?.to_vec()
            }
        };

        anyhow::ensure!(!embedding.is_empty(), "encoder returned an empty embedding");

        // A zero or non-finite norm would divide into NaN/inf and produce an
        // embedding that fails every comparison for reasons nothing reports.
        let norm: f32 = embedding.iter().map(|x| x * x).sum::<f32>().sqrt();
        anyhow::ensure!(
            norm.is_finite() && norm > 0.0,
            "encoder produced a degenerate embedding (norm {norm})"
        );

        Ok(embedding.into_iter().map(|x| x / norm).collect())
    }
}
