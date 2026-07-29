use std::path::Path;
use crate::error::FaceAuthError;
use tract_onnx::prelude::*;

#[cfg(feature = "npu")]
use openvino::{Core as OvCore, DeviceType, ElementType, InferRequest, PartialShape, Shape, Tensor as OvTensor};

pub enum FaceEncoder {
    Tract(InferenceSimplePlan<InferenceModel>),
    #[cfg(feature = "npu")]
    OpenVino {
        request: InferRequest,
        input_name: String,
        output_name: String,
    },
}

impl FaceEncoder {
    pub fn new(model_path: &str, backend: &str, device: &str) -> anyhow::Result<Self> {
        match backend {
            "openvino" => Self::new_openvino(model_path, device),
            _ => Self::new_tract(model_path),
        }
    }

    fn new_tract(model_path: &str) -> anyhow::Result<Self> {
        tracing::debug!(model_path, "FaceEncoder: using tract (CPU) backend");
        let model = onnx()
            .model_for_path(model_path)?
            .with_input_fact(0, InferenceFact::dt_shape(f32::datum_type(), tvec!(1, 3, 112, 112)))?
            .into_runnable()?;
        Ok(Self::Tract(model))
    }

    #[cfg(feature = "npu")]
    fn new_openvino(model_path: &str, device: &str) -> anyhow::Result<Self> {
        tracing::debug!(model_path, device, "FaceEncoder: using openvino backend, compiling model");
        let t0 = std::time::Instant::now();
        let mut core = OvCore::new()?;
        let mut model = core.read_model_from_file(model_path, "")?;
        let input_name = model.get_input_by_index(0)?.get_name()?;
        let output_name = model.get_output_by_index(0)?.get_name()?;
        model.reshape_single_input(&PartialShape::new_static(4, &[1, 3, 112, 112])?)?;
        let mut compiled = core.compile_model(&model, DeviceType::from(device))?;
        let request = compiled.create_infer_request()?;
        tracing::debug!(device, compile_time_ms = t0.elapsed().as_millis(), "FaceEncoder: openvino model compiled");
        Ok(Self::OpenVino { request, input_name, output_name })
    }

    #[cfg(not(feature = "npu"))]
    fn new_openvino(_model_path: &str, _device: &str) -> anyhow::Result<Self> {
        anyhow::bail!(
            "backend = \"openvino\" requested but this build of face-auth was compiled without the `npu` feature"
        )
    }

    pub fn encode(&mut self, input: tract_ndarray::ArrayView3<f32>) -> anyhow::Result<Vec<f32>> {
        let embedding = match self {
            Self::Tract(model) => {
                let mut input = input.to_owned().into_dyn();
                input.insert_axis_inplace(tract_ndarray::Axis(0));
                let input_value = Tensor::from(input).into_tvalue();
                let result = model.run(tvec!(input_value))?;
                let output = result[0].to_array_view::<f32>()?;
                output.iter().copied().collect::<Vec<f32>>()
            }
            #[cfg(feature = "npu")]
            Self::OpenVino { request, input_name, output_name } => {
                let owned = input.to_owned();
                let standard = owned.as_standard_layout();
                let data = standard
                    .as_slice()
                    .ok_or_else(|| anyhow::anyhow!("non-contiguous encoder input"))?;
                let mut tensor = OvTensor::new(ElementType::F32, &Shape::new(&[1, 3, 112, 112])?)?;
                tensor.get_data_mut::<f32>()?.copy_from_slice(data);
                request.set_tensor(input_name, &tensor)?;
                request.infer()?;
                let output = request.get_tensor(output_name)?;
                output.get_data::<f32>()?.to_vec()
            }
        };

        let norm: f32 = embedding.iter().map(|x| x * x).sum::<f32>().sqrt();
        Ok(embedding.iter().map(|x| x / norm).collect())
    }
}

pub fn load_model(model_path: &str) -> anyhow::Result<FaceEncoder> {
    if !Path::new(model_path).exists() {
        return Err(FaceAuthError::Inference(format!("Model not found: {}", model_path)).into());
    }
    FaceEncoder::new(model_path, "tract", "")
}
